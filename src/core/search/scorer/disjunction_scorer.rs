// Copyright 2019 Zhizhesihai (Beijing) Technology Limited.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

use core::search::scorer::Scorer;
use core::search::DocIterator;
use core::util::{DisiPriorityQueue, DocId};

use error::Result;
use std::f32;

/// A Scorer for OR like queries, counterpart of `ConjunctionScorer`.
pub struct DisjunctionSumScorer<T: Scorer> {
    sub_scorers: DisiPriorityQueue<T>,
    needs_scores: bool,
    cost: usize,
}

impl<T: Scorer> DisjunctionSumScorer<T> {
    pub fn new(children: Vec<T>, needs_scores: bool) -> DisjunctionSumScorer<T> {
        assert!(children.len() > 1);

        let cost = children.iter().map(|w| w.cost()).sum();

        DisjunctionSumScorer {
            sub_scorers: DisiPriorityQueue::new(children),
            needs_scores,
            cost,
        }
    }

    pub fn sub_scorers(&self) -> &DisiPriorityQueue<T> {
        &self.sub_scorers
    }

    pub fn sub_scorers_mut(&mut self) -> &mut DisiPriorityQueue<T> {
        &mut self.sub_scorers
    }

    pub fn get_cost(&self) -> usize {
        self.cost
    }
}

impl<T: Scorer> Scorer for DisjunctionSumScorer<T> {
    fn score(&mut self) -> Result<f32> {
        let mut score: f32 = 0.0f32;

        if !self.needs_scores {
            return Ok(score);
        }

        let mut disi = self.sub_scorers_mut().top_list();

        loop {
            let sub_score = disi.inner_mut().score()?;
            score += sub_score;

            if disi.next.is_null() {
                break;
            } else {
                unsafe { disi = &mut *disi.next };
            }
        }

        Ok(score)
    }
}

impl<T: Scorer> DocIterator for DisjunctionSumScorer<T> {
    fn doc_id(&self) -> DocId {
        self.sub_scorers().peek().doc()
    }

    fn next(&mut self) -> Result<DocId> {
        self.approximate_next()
    }

    fn advance(&mut self, target: DocId) -> Result<DocId> {
        self.approximate_advance(target)
    }

    fn cost(&self) -> usize {
        self.cost
    }

    fn matches(&mut self) -> Result<bool> {
        Ok(true)
    }

    fn match_cost(&self) -> f32 {
        0f32
    }

    fn approximate_next(&mut self) -> Result<DocId> {
        let sub_scorers = self.sub_scorers_mut();
        let doc = sub_scorers.peek().doc();

        loop {
            sub_scorers.peek_mut().approximate_next()?;
            if sub_scorers.peek().doc() != doc {
                break;
            }
        }

        Ok(sub_scorers.peek().doc())
    }

    fn approximate_advance(&mut self, target: DocId) -> Result<DocId> {
        let sub_scorers = self.sub_scorers_mut();
        loop {
            sub_scorers.peek_mut().approximate_advance(target)?;
            if sub_scorers.peek().doc() >= target {
                break;
            }
        }

        Ok(sub_scorers.peek().doc())
    }
}

/// The Scorer for DisjunctionMaxQuery.  The union of all documents generated by the the subquery
/// scorers is generated in document number order.  The score for each document is the maximum of
/// the scores computed by the subquery scorers that generate that document, plus
/// tieBreakerMultiplier times the sum of the scores for the other subqueries that generate the
/// document.
pub struct DisjunctionMaxScorer<T: Scorer> {
    sub_scorers: DisiPriorityQueue<T>,
    needs_scores: bool,
    cost: usize,
    tie_breaker_multiplier: f32,
}

impl<T: Scorer> DisjunctionMaxScorer<T> {
    pub fn new(
        children: Vec<T>,
        tie_breaker_multiplier: f32,
        needs_scores: bool,
    ) -> DisjunctionMaxScorer<T> {
        assert!(children.len() > 1);

        let cost = children.iter().map(|w| w.cost()).sum();

        DisjunctionMaxScorer {
            sub_scorers: DisiPriorityQueue::new(children),
            needs_scores,
            cost,
            tie_breaker_multiplier,
        }
    }

    pub fn sub_scorers(&self) -> &DisiPriorityQueue<T> {
        &self.sub_scorers
    }

    pub fn sub_scorers_mut(&mut self) -> &mut DisiPriorityQueue<T> {
        &mut self.sub_scorers
    }

    pub fn get_cost(&self) -> usize {
        self.cost
    }
}

impl<T: Scorer> Scorer for DisjunctionMaxScorer<T> {
    fn score(&mut self) -> Result<f32> {
        let mut score_sum = 0.0f32;

        if !self.needs_scores {
            return Ok(score_sum);
        }

        let mut score_max = f32::NEG_INFINITY;
        let mut disi = self.sub_scorers_mut().top_list();

        loop {
            let sub_score = disi.inner_mut().score()?;
            score_sum += sub_score;
            if sub_score > score_max {
                score_max = sub_score;
            }

            if disi.next.is_null() {
                break;
            } else {
                unsafe { disi = &mut *disi.next };
            }
        }

        Ok(score_max + (score_sum - score_max) * self.tie_breaker_multiplier)
    }
}

impl<T: Scorer> DocIterator for DisjunctionMaxScorer<T> {
    fn doc_id(&self) -> DocId {
        self.sub_scorers().peek().doc()
    }

    fn next(&mut self) -> Result<DocId> {
        self.approximate_next()
    }

    fn advance(&mut self, target: DocId) -> Result<DocId> {
        self.approximate_advance(target)
    }

    fn cost(&self) -> usize {
        self.cost
    }

    fn matches(&mut self) -> Result<bool> {
        Ok(true)
    }

    fn match_cost(&self) -> f32 {
        0f32
    }

    fn approximate_next(&mut self) -> Result<DocId> {
        let sub_scorers = self.sub_scorers_mut();
        let doc = sub_scorers.peek().doc();

        loop {
            sub_scorers.peek_mut().approximate_next()?;
            if sub_scorers.peek().doc() != doc {
                break;
            }
        }

        Ok(sub_scorers.peek().doc())
    }

    fn approximate_advance(&mut self, target: DocId) -> Result<DocId> {
        let sub_scorers = self.sub_scorers_mut();
        loop {
            sub_scorers.peek_mut().approximate_advance(target)?;
            if sub_scorers.peek().doc() >= target {
                break;
            }
        }

        Ok(sub_scorers.peek().doc())
    }
}
