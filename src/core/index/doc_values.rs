use core::index::term::TermIterator;
use core::index::LeafReader;
use core::index::RandomAccessOrds;
use core::index::SingletonSortedNumericDocValues;
use core::index::SingletonSortedSetDocValues;
use core::index::SortedDocValuesTermIterator;
use core::index::NO_MORE_ORDS;
use core::index::{BinaryDocValues, BinaryDocValuesRef};
use core::index::{NumericDocValues, NumericDocValuesRef};
use core::index::{SortedDocValues, SortedDocValuesRef};
use core::index::{SortedNumericDocValues, SortedNumericDocValuesRef};
use core::index::{SortedSetDocValues, SortedSetDocValuesRef};
use core::util::DocId;
use error::Result;

use core::util::{Bits, BitsRef, ImmutableBits, MatchNoBits};

pub struct EmptyBinaryDocValues;

impl EmptyBinaryDocValues {
    fn new() -> Self {
        EmptyBinaryDocValues {}
    }
}

impl BinaryDocValues for EmptyBinaryDocValues {
    fn get(&self, _doc_id: DocId) -> Result<Vec<u8>> {
        Ok(Vec::with_capacity(0))
    }
}

#[derive(Clone)]
pub struct EmptySortedDocValues;

impl EmptySortedDocValues {
    fn new() -> Self {
        EmptySortedDocValues {}
    }
}

impl SortedDocValues for EmptySortedDocValues {
    fn get_ord(&self, _doc_id: DocId) -> Result<i32> {
        Ok(-1)
    }

    fn lookup_ord(&self, _ord: i32) -> Result<Vec<u8>> {
        Ok(Vec::with_capacity(0))
    }

    fn get_value_count(&self) -> usize {
        0
    }

    fn term_iterator<'a, 'b: 'a>(&'b self) -> Result<Box<TermIterator + 'a>> {
        let ti = SortedDocValuesTermIterator::new(self);
        Ok(Box::new(ti))
    }
}

impl BinaryDocValues for EmptySortedDocValues {
    fn get(&self, _doc_id: DocId) -> Result<Vec<u8>> {
        Ok(Vec::with_capacity(0))
    }
}

pub struct EmptyNumericDocValues;
impl NumericDocValues for EmptyNumericDocValues {
    fn get(&self, _doc_id: DocId) -> Result<i64> {
        Ok(0)
    }
}

pub struct DocValues;

impl DocValues {
    pub fn empty_binary() -> EmptyBinaryDocValues {
        EmptyBinaryDocValues::new()
    }
    pub fn empty_numeric() -> EmptyNumericDocValues {
        EmptyNumericDocValues {}
    }
    pub fn empty_sorted() -> EmptySortedDocValues {
        EmptySortedDocValues::new()
    }
    /// An empty SortedNumericDocValues which returns zero values for every document
    pub fn empty_sorted_numeric(max_doc: usize) -> Box<SortedNumericDocValues> {
        let dv = Box::new(DocValues::empty_numeric());
        let mybox = Box::new(MatchNoBits::new(max_doc));
        let docs_with_field = Bits::new(mybox);
        Box::new(SingletonSortedNumericDocValues::new(dv, docs_with_field))
    }

    pub fn empty_sorted_set() -> Box<RandomAccessOrds> {
        let dv = Box::new(DocValues::empty_sorted());
        Box::new(SingletonSortedSetDocValues::new(dv))
    }

    pub fn singleton_sorted_doc_values(dv: Box<SortedDocValues>) -> SingletonSortedSetDocValues {
        SingletonSortedSetDocValues::new(dv)
    }

    pub fn singleton_sorted_numeric_doc_values(
        numeric_doc_values_in: Box<NumericDocValues>,
        docs_with_field: Bits,
    ) -> SingletonSortedNumericDocValues {
        SingletonSortedNumericDocValues::new(numeric_doc_values_in, docs_with_field)
    }

    pub fn docs_with_value_sorted(dv: Box<SortedDocValues>, max_doc: i32) -> Bits {
        let boxed = SortedDocValuesBits { dv, max_doc };
        Bits::new(Box::new(boxed))
    }

    pub fn docs_with_value_sorted_set(dv: Box<SortedSetDocValues>, max_doc: i32) -> Bits {
        let boxed = SortedSetDocValuesBits { dv, max_doc };
        Bits::new(Box::new(boxed))
    }

    pub fn docs_with_value_sorted_numeric(dv: Box<SortedNumericDocValues>, max_doc: i32) -> Bits {
        let boxed = SortedNumericDocValuesBits { dv, max_doc };
        Bits::new(Box::new(boxed))
    }

    pub fn get_docs_with_field(reader: &LeafReader, field: &str) -> Result<BitsRef> {
        reader.get_docs_with_field(field)
    }

    pub fn get_numeric(reader: &LeafReader, field: &str) -> Result<NumericDocValuesRef> {
        reader.get_numeric_doc_values(field)
    }

    pub fn get_binary(reader: &LeafReader, field: &str) -> Result<BinaryDocValuesRef> {
        reader.get_binary_doc_values(field)
    }

    pub fn get_sorted(reader: &LeafReader, field: &str) -> Result<SortedDocValuesRef> {
        reader.get_sorted_doc_values(field)
    }

    pub fn get_sorted_numeric(
        reader: &LeafReader,
        field: &str,
    ) -> Result<SortedNumericDocValuesRef> {
        reader.get_sorted_numeric_doc_values(field)
    }

    pub fn get_sorted_set(reader: &LeafReader, field: &str) -> Result<SortedSetDocValuesRef> {
        reader.get_sorted_set_doc_values(field)
    }

    pub fn unwrap_singleton(dv: &SortedNumericDocValuesRef) -> Result<Option<NumericDocValuesRef>> {
        let val = dv.get_numeric_doc_values();
        Ok(val)
    }
}

struct SortedDocValuesBits {
    dv: Box<SortedDocValues>,
    max_doc: i32,
}

impl ImmutableBits for SortedDocValuesBits {
    fn get(&self, index: usize) -> Result<bool> {
        let ord = self.dv.get_ord(index as DocId)?;
        Ok(ord >= 0)
    }

    fn len(&self) -> usize {
        self.max_doc as usize
    }
}

struct SortedSetDocValuesBits {
    dv: Box<SortedSetDocValues>,
    max_doc: i32,
}

impl ImmutableBits for SortedSetDocValuesBits {
    fn get(&self, index: usize) -> Result<bool> {
        let mut ctx = self.dv.set_document(index as DocId)?;
        let ord = self.dv.next_ord(&mut ctx)?;
        Ok(ord != NO_MORE_ORDS)
    }
    fn len(&self) -> usize {
        self.max_doc as usize
    }
}

struct SortedNumericDocValuesBits {
    dv: Box<SortedNumericDocValues>,
    max_doc: i32,
}

impl ImmutableBits for SortedNumericDocValuesBits {
    fn get(&self, index: usize) -> Result<bool> {
        let ctx = self.dv.set_document(index as DocId)?;
        Ok(self.dv.count(&ctx) != 0)
    }
    fn len(&self) -> usize {
        self.max_doc as usize
    }
}
