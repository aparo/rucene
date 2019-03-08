use core::analysis::TokenStream;
use core::index::byte_slice_reader::ByteSliceReader;
use core::index::postings_array::{ParallelPostingsArray, PostingsArray};
use core::index::term_vector::TermVectorsConsumerPerField;
use core::index::terms_hash::TermsHashBase;
use core::index::{FieldInfo, FieldInvertState, Fieldable, IndexOptions};
use core::util::bit_util::UnsignedShift;
use core::util::byte_block_pool::{self, ByteBlockPool};
use core::util::bytes_ref_hash::{BytesRefHash, BytesStartArray};
use core::util::int_block_pool::{self, IntBlockPool};
use core::util::{Count, Counter, DocId};

use std::cmp::{max, Ordering};

use error::Result;

const HASH_INIT_SIZE: usize = 4;

pub struct TermsHashPerFieldBase<T: PostingsArray> {
    // Copied from our perThread
    pub int_pool: *mut IntBlockPool,
    pub byte_pool: *mut ByteBlockPool,
    pub term_byte_pool: *mut ByteBlockPool,
    pub bytes_used: Counter,

    stream_count: usize,
    num_posting_int: usize,
    pub field_info: FieldInfo,
    pub bytes_hash: BytesRefHash,
    pub postings_array: T,
    // bytes_used: Counter,    // term_hash.bytes_used
    // sorted_term_ids: Vec<u32>,  bytes_hash.ids after sort
    int_upto_idx: usize,
    // cur int_uptos index for int_pool.buffer
    int_upto_start: usize,
    do_next_call: bool,
    inited: bool,
    // must init before use
}

impl<T: PostingsArray + 'static> TermsHashPerFieldBase<T> {
    /// streamCount: how many streams this field stores per term.
    /// E.g. doc(+freq) is 1 stream, prox+offset is a second.
    pub fn new(
        stream_count: usize,
        parent: &mut TermsHashBase,
        field_info: FieldInfo,
        postings_array: T,
    ) -> Self {
        TermsHashPerFieldBase {
            int_pool: &mut parent.int_pool,
            byte_pool: &mut parent.byte_pool,
            term_byte_pool: parent.term_byte_pool,
            bytes_used: unsafe { parent.bytes_used.shallow_copy() },
            stream_count,
            num_posting_int: 2 * stream_count,
            field_info,
            bytes_hash: BytesRefHash::default(),
            postings_array,
            int_upto_idx: 0,
            int_upto_start: 0,
            do_next_call: false,
            inited: false,
        }
    }

    pub fn init(&mut self) {
        let bytes_starts: Box<BytesStartArray> = {
            let mut counter = unsafe { self.bytes_used.shallow_copy() };
            Box::new(PostingsBytesStartArray::new(self, &mut counter))
        };
        self.bytes_hash =
            unsafe { BytesRefHash::new(&mut *self.term_byte_pool, HASH_INIT_SIZE, bytes_starts) };
        self.inited = true;
    }

    pub fn reset_ptr(&mut self, parent: &mut TermsHashBase) {
        self.int_pool = &mut parent.int_pool;
        self.byte_pool = &mut parent.byte_pool;
        self.term_byte_pool = parent.term_byte_pool;
        self.bytes_hash.pool = parent.term_byte_pool;
    }

    fn int_block_pool(&self) -> &IntBlockPool {
        unsafe { &*self.int_pool }
    }
    fn int_pool_mut(&mut self) -> &mut IntBlockPool {
        unsafe { &mut *self.int_pool }
    }
    pub fn byte_block_pool(&self) -> &ByteBlockPool {
        unsafe { &*self.byte_pool }
    }
    fn byte_pool_mut(&mut self) -> &mut ByteBlockPool {
        unsafe { &mut *self.byte_pool }
    }
    pub fn term_pool(&self) -> &ByteBlockPool {
        unsafe { &*self.term_byte_pool }
    }

    fn add(&mut self, term_id: i32) {
        debug_assert!(self.inited);
        if term_id >= 0 {
            // new posting
            let term_id = term_id as usize;
            // first time we are seeing this token since we last flushed the hash.
            // Init stream slices
            if self.num_posting_int + self.int_block_pool().int_upto
                > int_block_pool::INT_BLOCK_SIZE
            {
                self.int_pool_mut().next_buffer();
            }
            if byte_block_pool::BYTE_BLOCK_SIZE - self.byte_block_pool().byte_upto
                < self.num_posting_int * byte_block_pool::FIRST_LEVEL_SIZE
            {
                self.byte_pool_mut().next_buffer();
            }

            self.int_upto_idx = self.int_block_pool().buffer_upto as usize;
            self.int_upto_start = self.int_block_pool().int_upto;
            self.int_pool_mut().int_upto += self.stream_count;

            self.postings_array.parallel_array_mut().int_starts[term_id] =
                (self.int_upto_start as isize + self.int_block_pool().int_offset) as usize as u32;

            for i in 0..self.stream_count {
                let upto = self.byte_pool_mut()
                    .new_slice(byte_block_pool::FIRST_LEVEL_SIZE);
                unsafe {
                    (&mut *self.int_pool).buffers[self.int_upto_idx][self.int_upto_start + i] =
                        (upto as isize + self.byte_block_pool().byte_offset) as i32;
                }
            }
            self.postings_array.parallel_array_mut().byte_starts[term_id] =
                self.int_block_pool().buffers[self.int_upto_idx][self.int_upto_start] as u32;
        } else {
            let term_id = -term_id - 1;
            let int_start =
                self.postings_array.parallel_array().int_starts[term_id as usize] as usize;
            self.int_upto_idx = int_start >> int_block_pool::INT_BLOCK_SHIFT;
            self.int_upto_start = int_start & int_block_pool::INT_BLOCK_MASK;
        }
    }

    fn write_byte(&mut self, stream: usize, b: u8) {
        debug_assert!(self.inited);
        unsafe {
            let upto =
                (*self.int_pool).buffers[self.int_upto_idx][self.int_upto_start + stream] as usize;
            let mut byte_pool_idx = upto >> byte_block_pool::BYTE_BLOCK_SHIFT;
            let mut offset = upto & byte_block_pool::BYTE_BLOCK_MASK;
            if (*self.byte_pool).buffers[byte_pool_idx][offset] != 0 {
                // End of slice; allocate a new one
                offset = (*self.byte_pool).alloc_slice(byte_pool_idx, offset);
                byte_pool_idx = (*self.byte_pool).buffer_upto as usize;
                (*self.int_pool).buffers[self.int_upto_idx][self.int_upto_start + stream] =
                    (offset as isize + (*self.byte_pool).byte_offset) as i32;
            }
            (*self.byte_pool).buffers[byte_pool_idx][offset] = b;
            (*self.int_pool).buffers[self.int_upto_idx][self.int_upto_start + stream] += 1;
        }
    }

    pub fn write_bytes(&mut self, stream: usize, data: &[u8]) {
        debug_assert!(self.inited);
        for b in data {
            self.write_byte(stream, *b);
        }
    }

    pub fn write_vint(&mut self, stream: usize, i: i32) {
        debug_assert!(self.inited);
        debug_assert!(stream < self.stream_count);
        let mut v = i;
        loop {
            if v & !0x7f == 0 {
                break;
            }
            self.write_byte(stream, ((v & 0x7f) | 0x80) as u8);
            v = v.unsigned_shift(7);
        }
        self.write_byte(stream, (v & 0x7f) as u8);
    }

    /// Collapse the hash table and sort in-place; also sets
    /// this.sortedTermIDs to the results
    pub fn sort_postings(&mut self) {
        debug_assert!(self.inited);
        self.bytes_hash.sort();
    }
}

pub trait TermsHashPerField: Ord + PartialOrd + Eq + PartialEq {
    type P: PostingsArray + 'static;
    fn base(&self) -> &TermsHashPerFieldBase<Self::P>;
    fn base_mut(&mut self) -> &mut TermsHashPerFieldBase<Self::P>;

    // TODO init the raw pointer in base and anywhere
    fn init(&mut self);

    fn reset_ptr(&mut self, parent: &mut TermsHashBase);

    fn reset(&mut self) {
        self.base_mut().bytes_hash.clear(false);
    }

    fn init_reader(&self, reader: &mut ByteSliceReader, term_id: usize, stream: usize) {
        debug_assert!(stream < self.base().stream_count);
        let int_start = self.base().postings_array.parallel_array().int_starts[term_id];
        let ints_idx = int_start as usize >> int_block_pool::INT_BLOCK_SHIFT;
        let upto = int_start as usize & int_block_pool::INT_BLOCK_MASK;
        let start_index = self.base().postings_array.parallel_array().byte_starts[term_id] as usize
            + stream * byte_block_pool::FIRST_LEVEL_SIZE;
        let end_index = self.base().int_block_pool().buffers[ints_idx][upto + stream];
        reader.init(
            &self.base().byte_block_pool(),
            start_index,
            end_index as usize,
        );
    }

    // Secondary entry point (for 2nd & subsequent TermsHash),
    // because token text has already been "interned" into
    // textStart, so we hash by textStart.  term vectors use
    // this API.
    fn add_by_offset(
        &mut self,
        state: &mut FieldInvertState,
        token_stream: &TokenStream,
        doc_id: DocId,
        text_start: usize,
    ) -> Result<()> {
        let term_id = self.base_mut().bytes_hash.add_by_pool_offset(text_start);
        self.base_mut().add(term_id);
        if term_id >= 0 {
            self.new_term(term_id as usize, state, token_stream, doc_id)
        } else {
            self.add_term(-(term_id + 1) as usize, state, token_stream, doc_id)
        }
    }

    /// Called once per inverted token.  This is the primary
    /// entry point (for first TermsHash); postings use this
    /// API.
    fn add(
        &mut self,
        field_state: &mut FieldInvertState,
        token_stream: &TokenStream,
        doc_id: DocId,
    ) -> Result<()> {
        // We are first in the chain so we must "insert" the
        // term text into text_start address
        let bytes_ref = token_stream.term_bytes_attribute().get_bytes_ref();
        let term_id = self.base_mut().bytes_hash.add(&bytes_ref);
        if term_id >= 0 {
            self.base_mut().bytes_hash.byte_start(term_id as usize);
        }

        self.base_mut().add(term_id);

        let mut real_term_id = term_id;
        if term_id >= 0 {
            self.new_term(term_id as usize, field_state, token_stream, doc_id)?;
        } else {
            self.add_term(-(term_id + 1) as usize, field_state, token_stream, doc_id)?;
            real_term_id = -(term_id + 1);
        }
        if self.base().do_next_call {
            let text_start = self.base().postings_array.parallel_array().text_starts
                [real_term_id as usize] as usize;
            self.do_next(field_state, token_stream, doc_id, text_start)?;
        }
        Ok(())
    }

    fn do_next(
        &mut self,
        _field_state: &mut FieldInvertState,
        _token_stream: &TokenStream,
        _doc_id: DocId,
        _text_start: usize,
    ) -> Result<()> {
        Ok(())
    }

    fn start(
        &mut self,
        field_state: &FieldInvertState,
        field: &Fieldable,
        first: bool,
    ) -> Result<bool>;

    fn finish(&mut self, field_state: &FieldInvertState) -> Result<()>;

    // Called when a term is seen for the first time.
    fn new_term(
        &mut self,
        term_id: usize,
        field_state: &mut FieldInvertState,
        token_stream: &TokenStream,
        doc_id: DocId,
    ) -> Result<()>;

    // Called when a previously seen term is seen again.
    fn add_term(
        &mut self,
        term_id: usize,
        field_state: &mut FieldInvertState,
        token_stream: &TokenStream,
        doc_id: DocId,
    ) -> Result<()>;

    // Creates a new postings array of the specified size.
    fn create_postings_array(&self, size: usize) -> Self::P;
}

struct PostingsBytesStartArray<T: PostingsArray + 'static> {
    per_field: *mut TermsHashPerFieldBase<T>,
    bytes_used: Counter,
}

impl<T: PostingsArray + 'static> PostingsBytesStartArray<T> {
    fn new(per_field: *mut TermsHashPerFieldBase<T>, bytes_used: &mut Counter) -> Self {
        let bytes_used = unsafe { bytes_used.shallow_copy() };
        PostingsBytesStartArray {
            per_field,
            bytes_used,
        }
    }
}

impl<T: PostingsArray + 'static> BytesStartArray for PostingsBytesStartArray<T> {
    fn bytes_mut(&mut self) -> &mut [u32] {
        unsafe {
            &mut (&mut *self.per_field)
                .postings_array
                .parallel_array_mut()
                .text_starts
        }
    }

    fn bytes(&self) -> &[u32] {
        unsafe {
            &(&mut *self.per_field)
                .postings_array
                .parallel_array()
                .text_starts
        }
    }

    fn init(&mut self) {}

    fn grow(&mut self) {
        unsafe {
            let postings_array = &mut (*self.per_field).postings_array;
            let old_size = postings_array.parallel_array().size;
            postings_array.grow();
            self.bytes_used.add_get(
                postings_array.bytes_per_posting() as i64
                    * (postings_array.parallel_array().size - old_size) as i64,
            );
        }
    }

    fn clear(&mut self) {
        unsafe {
            let postings_array = &mut (*self.per_field).postings_array;
            let size = postings_array.parallel_array().size * postings_array.bytes_per_posting();
            self.bytes_used.add_get(-1 * size as i64);
            postings_array.clear();
        }
    }

    fn bytes_used_mut(&mut self) -> &mut Counter {
        &mut self.bytes_used
    }
}

// TODO: break into separate freq and prox writers as
// codecs; make separate container (tii/tis/skip/*) that can
// be configured as any number of files 1..N
pub struct FreqProxTermsWriterPerField {
    pub base: TermsHashPerFieldBase<FreqProxPostingsArray>,
    // freq_prox_postings_array: FreqProxPostingsArray,
    pub has_freq: bool,
    pub has_prox: bool,
    pub has_offsets: bool,
    // offset_attribute: OffsetAttribute,
    sum_total_term_freq: i64,
    sum_doc_freq: i64,

    // how many docs have this field:
    doc_count: u32,
    /// Set to true if any token had a payload in the current segment
    pub saw_payloads: bool,
    pub next_per_field: TermVectorsConsumerPerField,
}

impl FreqProxTermsWriterPerField {
    pub fn new(
        terms_hash: &mut TermsHashBase,
        field_info: FieldInfo,
        next_per_field: TermVectorsConsumerPerField,
    ) -> Self {
        let stream_count = if field_info.index_options >= IndexOptions::DocsAndFreqsAndPositions {
            2
        } else {
            1
        };
        let index_options = field_info.index_options;
        let base = TermsHashPerFieldBase::new(
            stream_count,
            terms_hash,
            field_info,
            FreqProxPostingsArray::new(2, index_options.has_freqs(),
                index_options.has_positions(), index_options.has_offsets()),
        );

        FreqProxTermsWriterPerField {
            base,
            has_freq: index_options >= IndexOptions::DocsAndFreqs,
            has_prox: index_options >= IndexOptions::DocsAndFreqsAndPositions,
            has_offsets: index_options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
            sum_total_term_freq: 0,
            sum_doc_freq: 0,
            doc_count: 0,
            saw_payloads: false,
            next_per_field,
        }
    }
    fn write_prox(
        &mut self,
        term_id: usize,
        prox_code: u32,
        field_state: &FieldInvertState,
        token_stream: &TokenStream,
    ) -> Result<()> {
        if let Some(payload_attr) = token_stream.payload_attribute() {
            let payload = payload_attr.get_payload();
            if payload.len() > 0 {
                self.base.write_vint(1, (prox_code << 1 | 1) as i32);
                self.base.write_vint(1, payload.len() as i32);
                self.base.write_bytes(1, payload);
                self.saw_payloads = true;
            } else {
                self.base.write_vint(1, (prox_code << 1) as i32);
            }
        } else {
            self.base.write_vint(1, (prox_code << 1) as i32);
        }

        self.base.postings_array.last_positions[term_id] = field_state.position as u32;
        Ok(())
    }

    fn write_offsets(&mut self, term_id: usize, offset_accum: usize, token_stream: &TokenStream) {
        let start_offset = (offset_accum + token_stream.offset_attribute().start_offset()) as u32;
        let end_offset = (offset_accum + token_stream.offset_attribute().end_offset()) as u32;
        debug_assert!(start_offset >= self.base.postings_array.last_offsets[term_id]);
        let value = (start_offset - self.base.postings_array.last_offsets[term_id]) as i32;
        self.base.write_vint(1, value);
        self.base.write_vint(1, (end_offset - start_offset) as i32);
        self.base.postings_array.last_offsets[term_id] = start_offset;
    }
}

impl TermsHashPerField for FreqProxTermsWriterPerField {
    type P = FreqProxPostingsArray;

    fn base(&self) -> &TermsHashPerFieldBase<FreqProxPostingsArray> {
        &self.base
    }

    fn base_mut(&mut self) -> &mut TermsHashPerFieldBase<FreqProxPostingsArray> {
        &mut self.base
    }

    fn init(&mut self) {
        self.base.init();
        self.next_per_field.init();
    }

    fn reset_ptr(&mut self, parent: &mut TermsHashBase) {
        self.base.reset_ptr(parent);
        self.next_per_field.reset_ptr(parent);
    }

    fn do_next(
        &mut self,
        field_state: &mut FieldInvertState,
        token_stream: &TokenStream,
        doc_id: DocId,
        text_start: usize,
    ) -> Result<()> {
        self.next_per_field
            .add_by_offset(field_state, token_stream, doc_id, text_start)
    }

    fn start(
        &mut self,
        field_state: &FieldInvertState,
        field: &Fieldable,
        first: bool,
    ) -> Result<bool> {
        self.base.do_next_call = self.next_per_field.start(field_state, field, first)?;
        Ok(true)
    }

    fn finish(&mut self, field_state: &FieldInvertState) -> Result<()> {
        self.next_per_field.finish(field_state)?;
        self.sum_doc_freq += field_state.unique_term_count as i64;
        self.sum_total_term_freq += field_state.length as i64;
        if field_state.length > 0 {
            self.doc_count += 1;
        }
        if self.saw_payloads {
            self.base.field_info.set_store_payloads();
        }
        Ok(())
    }

    fn new_term(
        &mut self,
        term_id: usize,
        field_state: &mut FieldInvertState,
        token_stream: &TokenStream,
        doc_id: DocId,
    ) -> Result<()> {
        // Firset time we're seeing this term since the last flush
        self.base.postings_array.last_doc_ids[term_id] = doc_id;

        if !self.has_freq {
            debug_assert!(self.base.postings_array.term_freqs.is_empty());
            self.base.postings_array.last_doc_codes[term_id] = doc_id as u32;
        } else {
            self.base.postings_array.last_doc_codes[term_id] = (doc_id << 1) as u32;
            self.base.postings_array.term_freqs[term_id] = 1;
            if self.has_prox {
                self.write_prox(
                    term_id,
                    field_state.position as u32,
                    field_state,
                    token_stream,
                )?;
                if self.has_offsets {
                    self.write_offsets(term_id, field_state.offset, token_stream);
                }
            } else {
                assert!(!self.has_offsets);
            }
        }
        field_state.max_term_frequency = max(1, field_state.max_term_frequency);
        field_state.unique_term_count += 1;
        Ok(())
    }

    fn add_term(
        &mut self,
        term_id: usize,
        field_state: &mut FieldInvertState,
        token_stream: &TokenStream,
        doc_id: DocId,
    ) -> Result<()> {
        debug_assert!(!self.has_freq || self.base.postings_array.term_freqs[term_id] > 0);

        if !self.has_freq {
            debug_assert!(self.base.postings_array.term_freqs.is_empty());
            if doc_id != self.base.postings_array.last_doc_ids[term_id] {
                // New document; now encode doc_code for previous doc:
                debug_assert!(doc_id > self.base.postings_array.last_doc_ids[term_id]);
                let v = self.base.postings_array.last_doc_codes[term_id] as i32;
                self.base.write_vint(0, v);
                self.base.postings_array.last_doc_codes[term_id] =
                    (doc_id - self.base.postings_array.last_doc_ids[term_id]) as u32;
                self.base.postings_array.last_doc_ids[term_id] = doc_id;
                field_state.unique_term_count += 1;
            }
        } else if doc_id != self.base.postings_array.last_doc_ids[term_id] {
            debug_assert!(doc_id > self.base.postings_array.last_doc_ids[term_id]);
            // Term not yet seen in the current doc but previously
            // seen in other doc(s) since the last flush

            // Now that we know doc freq for previous doc,
            // write it & lastDocCode
            if self.base.postings_array.term_freqs[term_id] == 1 {
                let v = (self.base.postings_array.last_doc_codes[term_id] | 1) as i32;
                self.base.write_vint(0, v);
            } else {
                let v = self.base.postings_array.last_doc_codes[term_id] as i32;
                self.base.write_vint(0, v);
                let v = self.base.postings_array.term_freqs[term_id] as i32;
                self.base.write_vint(0, v);
            }

            // Init freq for the current document
            self.base.postings_array.term_freqs[term_id] = 1;
            field_state.max_term_frequency = max(
                self.base.postings_array.term_freqs[term_id],
                field_state.max_term_frequency,
            );
            self.base.postings_array.last_doc_codes[term_id] =
                ((doc_id - self.base.postings_array.last_doc_ids[term_id]) << 1) as u32;
            self.base.postings_array.last_doc_ids[term_id] = doc_id;
            if self.has_prox {
                self.write_prox(
                    term_id,
                    field_state.position as u32,
                    field_state,
                    token_stream,
                )?;
                if self.has_offsets {
                    self.base.postings_array.last_offsets[term_id] = 0;
                    self.write_offsets(term_id, field_state.offset, token_stream);
                }
            } else {
                debug_assert!(!self.has_offsets);
            }
            field_state.unique_term_count += 1;
        } else {
            self.base.postings_array.term_freqs[term_id] += 1;
            field_state.max_term_frequency = max(
                field_state.max_term_frequency,
                self.base.postings_array.term_freqs[term_id],
            );
            if self.has_prox {
                let code =
                    field_state.position as u32 - self.base.postings_array.last_positions[term_id];
                self.write_prox(term_id, code, field_state, token_stream)?;
                if self.has_offsets {
                    self.write_offsets(term_id, field_state.offset, token_stream);
                }
            }
        }
        Ok(())
    }

    fn create_postings_array(&self, size: usize) -> FreqProxPostingsArray {
        let index_options = self.base.field_info.index_options;
        assert_ne!(index_options, IndexOptions::Null);
        let has_freq = index_options >= IndexOptions::DocsAndFreqs;
        let has_prox = index_options >= IndexOptions::DocsAndFreqsAndPositions;
        let has_offsets = index_options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
        FreqProxPostingsArray::new(size, has_freq, has_prox, has_offsets)
    }
}

impl Eq for FreqProxTermsWriterPerField {}

impl PartialEq for FreqProxTermsWriterPerField {
    fn eq(&self, other: &Self) -> bool {
        self.base.field_info.name.eq(&other.base.field_info.name)
    }
}

impl Ord for FreqProxTermsWriterPerField {
    fn cmp(&self, other: &Self) -> Ordering {
        self.base.field_info.name.cmp(&other.base.field_info.name)
    }
}

impl PartialOrd for FreqProxTermsWriterPerField {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct FreqProxPostingsArray {
    pub base: ParallelPostingsArray,
    pub term_freqs: Vec<u32>,
    // # times this term occurs in the current doc
    pub last_doc_ids: Vec<i32>,
    // Last doc_id where this term occurred
    last_doc_codes: Vec<u32>,
    // Code for prior doc
    last_positions: Vec<u32>,
    // Last position where this term occurred
    last_offsets: Vec<u32>,
    // Last endOffset where this term occurred
}

impl Default for FreqProxPostingsArray {
    fn default() -> Self {
        let default_size = 1024;
        FreqProxPostingsArray {
            base: ParallelPostingsArray::default(),
            term_freqs: vec![0u32; default_size],
            last_doc_ids: vec![0i32; default_size],
            last_doc_codes: vec![0u32; default_size],
            last_positions: vec![0u32; default_size],
            last_offsets: vec![0u32; default_size],
        }
    }
}

impl FreqProxPostingsArray {
    fn new(size: usize, write_freqs: bool, write_pos: bool, write_offsets: bool) -> Self {
        let base = ParallelPostingsArray::new(size);
        let term_freqs = if write_freqs {
            vec![0u32; size]
        } else {
            Vec::with_capacity(0)
        };
        let mut last_positions = Vec::with_capacity(0);
        let mut last_offsets = Vec::with_capacity(0);
        if write_pos {
            last_positions = vec![0u32; size];
            if write_offsets {
                last_offsets = vec![0u32; size];
            }
        } else {
            assert!(!write_offsets);
        }
        FreqProxPostingsArray {
            base,
            term_freqs,
            last_doc_ids: vec![0i32; size],
            last_doc_codes: vec![0u32; size],
            last_positions,
            last_offsets,
        }
    }
}

impl PostingsArray for FreqProxPostingsArray {
    fn parallel_array(&self) -> &ParallelPostingsArray {
        &self.base
    }

    fn parallel_array_mut(&mut self) -> &mut ParallelPostingsArray {
        &mut self.base
    }

    fn bytes_per_posting(&self) -> usize {
        let mut bytes = self.base.bytes_per_posting() + 2 * 4;
        if !self.last_positions.is_empty() {
            bytes += 4;
        }
        if !self.last_offsets.is_empty() {
            bytes += 4;
        }
        if !self.term_freqs.is_empty() {
            bytes += 4;
        }
        bytes
    }

    fn grow(&mut self) {
        self.base.grow();
        let new_size = self.base.size;
        if !self.last_positions.is_empty() {
            self.last_positions.resize(new_size, 0u32);
        }
        if !self.last_offsets.is_empty() {
            self.last_offsets.resize(new_size, 0u32);
        }
        if !self.term_freqs.is_empty() {
            self.term_freqs.resize(new_size, 0u32);
        }
        self.last_doc_ids.resize(new_size, 0i32);
        self.last_doc_codes.resize(new_size, 0u32);
    }

    fn clear(&mut self) {
        self.base.clear();
        if !self.last_positions.is_empty() {
            self.last_positions = Vec::with_capacity(0);
        }
        if !self.last_offsets.is_empty() {
            self.last_offsets = Vec::with_capacity(0);
        }
        if !self.term_freqs.is_empty() {
            self.term_freqs = Vec::with_capacity(0);
        }
        self.last_doc_ids = Vec::with_capacity(0);
        self.last_doc_codes = Vec::with_capacity(0);
    }
}
