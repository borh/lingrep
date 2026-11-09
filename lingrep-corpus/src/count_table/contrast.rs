//! Ranks a pattern's entries by how differently two document scopes use them.
//!
//! The score is the two-cell log-likelihood of UCREL's calculator
//! (<https://ucrel.lancs.ac.uk/llwizard.html>): for counts `a` and `b` in
//! scopes of `N1` and `N2` tokens, `E_i = N_i (a + b) / (N1 + N2)` and
//! `G² = 2 Σ O_i ln(O_i / E_i)`, where a cell with `O_i = 0` contributes 0.

use std::sync::Arc;

use lingrep_core::execution::CancellationFlag;
use lingrep_core::spill::{SortedDrain, SortedRunCounts, SpillPolicy, TempBudget};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    Admission, CANCELLATION_INTERVAL, CountTable, CountTableError, DocumentMask, cancelled_or,
    check_cancellation, corrupt,
};

/// Occurrences in one scope and the admitted documents holding at least one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScopeCount {
    pub count: u64,
    pub works: u64,
}

/// The scope whose rate is higher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    First,
    Second,
    Equal,
}

/// The comparison of one count across two scopes. Every value is `None`
/// when either scope admits no tokens, except the rate of a scope that does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contrast {
    pub rates_per_million: [Option<f64>; 2],
    /// G², never negative.
    pub log_likelihood: Option<f64>,
    /// The smaller of the two expected counts `E_i`.
    pub min_expected: Option<f64>,
    /// `log2((a / N1) / (b / N2))`; also `None` when either count is 0.
    pub log2_ratio: Option<f64>,
    pub direction: Option<Direction>,
}

impl Contrast {
    /// Compares `counts[i]` occurrences among `tokens[i]` tokens.
    #[must_use]
    pub fn of(counts: [u64; 2], tokens: [u64; 2]) -> Self {
        let rates_per_million = [0, 1].map(|scope| {
            (tokens[scope] > 0).then(|| counts[scope] as f64 * 1e6 / tokens[scope] as f64)
        });
        if tokens.contains(&0) {
            return Self {
                rates_per_million,
                log_likelihood: None,
                min_expected: None,
                log2_ratio: None,
                direction: None,
            };
        }
        let total = counts[0] as f64 + counts[1] as f64;
        let all = tokens[0] as f64 + tokens[1] as f64;
        let expected = tokens.map(|scope_tokens| scope_tokens as f64 * total / all);
        let sum = counts
            .iter()
            .zip(expected)
            .filter(|(count, _)| **count > 0)
            .map(|(count, expected)| *count as f64 * (*count as f64 / expected).ln())
            .sum::<f64>();
        let first = u128::from(counts[0]) * u128::from(tokens[1]);
        let second = u128::from(counts[1]) * u128::from(tokens[0]);
        let direction = match first.cmp(&second) {
            std::cmp::Ordering::Greater => Direction::First,
            std::cmp::Ordering::Less => Direction::Second,
            std::cmp::Ordering::Equal => Direction::Equal,
        };
        let log_likelihood = if direction == Direction::Equal || sum <= 0.0 {
            0.0
        } else {
            2.0 * sum
        };
        // log2(high / low) = log2(1 + (high - low) / low), taken from the exact
        // difference: the sign follows `direction` even when both products
        // round to the same f64, and swapping the scopes negates the value.
        let log2_ratio = (!counts.contains(&0)).then(|| {
            let (high, low) = (first.max(second), first.min(second));
            let magnitude = ((high - low) as f64 / low as f64).ln_1p() / std::f64::consts::LN_2;
            if first >= second {
                magnitude
            } else {
                -magnitude
            }
        });
        Self {
            rates_per_million,
            log_likelihood: Some(log_likelihood),
            min_expected: Some(expected[0].min(expected[1])),
            log2_ratio,
            direction: Some(direction),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeRelation {
    /// The scopes admit no document in common.
    Disjoint,
    /// The scopes admit some documents in common, but not the same documents.
    Overlapping,
    /// The scopes admit the same documents.
    Identical,
}

/// Two scopes over one table: what each admits and what both admit. Only
/// [`CountTable::scopes`] builds one, and only that table ranks with it.
///
/// ```compile_fail
/// fn relabel(scopes: &mut lingrep_corpus::Scopes) {
///     scopes.relation = lingrep_corpus::ScopeRelation::Identical;
/// }
/// ```
pub struct Scopes {
    /// Per table document, bit 0 when the first scope admits it and bit 1
    /// when the second does.
    bits: Vec<u8>,
    table: u64,
    admission: [Admission; 2],
    shared: Admission,
    relation: ScopeRelation,
}

impl Scopes {
    /// What the first and the second scope admit.
    #[must_use]
    pub fn admission(&self) -> [Admission; 2] {
        self.admission
    }

    /// What both scopes admit.
    #[must_use]
    pub fn shared(&self) -> Admission {
        self.shared
    }

    #[must_use]
    pub fn relation(&self) -> ScopeRelation {
        self.relation
    }

    fn tokens(&self) -> [u64; 2] {
        self.admission.map(|admission| admission.tokens_admitted)
    }
}

/// One entry's counts in both scopes, its count in the documents both admit,
/// and their comparison.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContrastRow {
    pub entry: u32,
    pub scopes: [ScopeCount; 2],
    pub shared: ScopeCount,
    pub contrast: Contrast,
}

/// A pattern's highest-scoring entries and how many entries were ranked.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedContrast {
    /// Entries with at least one occurrence in either scope.
    pub entries: u64,
    pub rows: Vec<ContrastRow>,
}

/// The ranking order: descending G², with `None` last; then descending
/// occurrences in both scopes together; then entry order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ContrastOrder {
    undefined: bool,
    inverted_score: u64,
    inverted_total: u64,
    entry: u32,
    scopes: [ScopeCount; 2],
    shared: ScopeCount,
}

impl ContrastOrder {
    fn of(row: &ContrastRow) -> Self {
        Self {
            undefined: row.contrast.log_likelihood.is_none(),
            // A non-negative double orders as its bits do.
            inverted_score: u64::MAX - row.contrast.log_likelihood.map_or(0, f64::to_bits),
            inverted_total: u64::MAX - row.scopes[0].count.saturating_add(row.scopes[1].count),
            entry: row.entry,
            scopes: row.scopes,
            shared: row.shared,
        }
    }

    fn row(self, tokens: [u64; 2]) -> ContrastRow {
        row(self.entry, self.scopes, self.shared, tokens)
    }
}

fn row(entry: u32, scopes: [ScopeCount; 2], shared: ScopeCount, tokens: [u64; 2]) -> ContrastRow {
    ContrastRow {
        entry,
        scopes,
        shared,
        contrast: Contrast::of(scopes.map(|scope| scope.count), tokens),
    }
}

impl CountTable {
    /// What each of two masks admits and what both admit.
    ///
    /// # Errors
    ///
    /// A mask built for another table.
    pub fn scopes(&self, masks: [&DocumentMask; 2]) -> Result<Scopes, CountTableError> {
        let first = self.admission(masks[0])?.0;
        let second = self.admission(masks[1])?.0;
        let bits = masks[0]
            .admitted
            .iter()
            .zip(&masks[1].admitted)
            .map(|(first, second)| u8::from(*first) | (u8::from(*second) << 1))
            .collect::<Vec<_>>();
        let mut shared = Admission {
            documents_admitted: 0,
            sentences_admitted: 0,
            tokens_admitted: 0,
        };
        for (document, bits) in bits.iter().enumerate() {
            if *bits == 3 {
                shared.documents_admitted = shared.documents_admitted.saturating_add(1);
                shared.sentences_admitted = shared
                    .sentences_admitted
                    .saturating_add(self.document_sentences[document]);
                shared.tokens_admitted = shared
                    .tokens_admitted
                    .saturating_add(self.document_tokens[document]);
            }
        }
        let relation = if masks[0] == masks[1] {
            ScopeRelation::Identical
        } else if shared.documents_admitted > 0 {
            ScopeRelation::Overlapping
        } else {
            ScopeRelation::Disjoint
        };
        Ok(Scopes {
            bits,
            table: self.instance,
            admission: [first, second],
            shared,
            relation,
        })
    }

    fn contrasted(
        &self,
        entry: u32,
        bits: &[u8],
    ) -> Result<([ScopeCount; 2], ScopeCount), CountTableError> {
        let (documents, counts) = self.entry_documents(entry)?;
        let mut scopes = [ScopeCount::default(); 2];
        let mut shared = ScopeCount::default();
        for (document, count) in documents.iter().zip(counts) {
            let admitted = *bits
                .get(*document as usize)
                .ok_or_else(|| corrupt("pair document out of range"))?;
            let count = u64::from(*count);
            for (scope, counted) in scopes.iter_mut().enumerate() {
                if admitted & (1 << scope) != 0 {
                    counted.count = counted.count.saturating_add(count);
                    counted.works = counted.works.saturating_add(1);
                }
            }
            if admitted == 3 {
                shared.count = shared.count.saturating_add(count);
                shared.works = shared.works.saturating_add(1);
            }
        }
        Ok((scopes, shared))
    }

    /// Visits, block by block, the ranking keys of every entry of `pattern`
    /// that occurs in either scope.
    fn contrast_blocks(
        &self,
        pattern: usize,
        scopes: &Scopes,
        cancellation: &CancellationFlag,
        mut visit: impl FnMut(Vec<ContrastOrder>) -> Result<(), CountTableError>,
    ) -> Result<u64, CountTableError> {
        if scopes.table != self.instance {
            return Err(corrupt("scopes were built for a different table"));
        }
        let tokens = scopes.tokens();
        let std::ops::Range { start, end } = self.pattern_range(pattern)?;
        let mut entries = 0u64;
        let mut at = start;
        while at < end {
            check_cancellation(cancellation)?;
            let stop = at.saturating_add(CANCELLATION_INTERVAL).min(end);
            let block = (at..stop)
                .into_par_iter()
                .with_min_len(4096)
                .filter_map(|entry| {
                    let entry = entry as u32;
                    match self.contrasted(entry, &scopes.bits) {
                        Ok((counts, _)) if counts.iter().all(|counted| counted.count == 0) => None,
                        Ok((counts, shared)) => {
                            Some(Ok(ContrastOrder::of(&row(entry, counts, shared, tokens))))
                        }
                        Err(error) => Some(Err(error)),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            entries = entries.saturating_add(block.len() as u64);
            visit(block)?;
            at = stop;
        }
        Ok(entries)
    }

    /// Every entry of `pattern` occurring in either scope, ranked by G², cut
    /// to the first `limit`.
    ///
    /// # Errors
    ///
    /// Scopes built for another table, a corrupt table, or
    /// [`CountTableError::Cancelled`] once `cancellation` is raised.
    pub fn contrast_top(
        &self,
        pattern: usize,
        scopes: &Scopes,
        limit: usize,
        cancellation: &CancellationFlag,
    ) -> Result<RankedContrast, CountTableError> {
        let mut best: Vec<ContrastOrder> = Vec::new();
        let entries = self.contrast_blocks(pattern, scopes, cancellation, |block| {
            best.extend(block);
            if best.len() > limit {
                best.select_nth_unstable(limit);
                best.truncate(limit);
            }
            Ok(())
        })?;
        best.sort_unstable();
        let tokens = scopes.tokens();
        Ok(RankedContrast {
            entries,
            rows: best.into_iter().map(|order| order.row(tokens)).collect(),
        })
    }

    /// Every entry of `pattern` occurring in either scope in
    /// [`Self::contrast_top`]'s order, ordered through sorted runs that
    /// reserve their files from `budget`.
    ///
    /// # Errors
    ///
    /// Scopes built for another table, a corrupt table, temporary storage
    /// beyond `budget`, or [`CountTableError::Cancelled`] once
    /// `cancellation` is raised.
    pub fn ranked_contrast_rows<'a>(
        &self,
        pattern: usize,
        scopes: &'a Scopes,
        policy: SpillPolicy,
        budget: Arc<TempBudget>,
        cancellation: &CancellationFlag,
    ) -> Result<ContrastRows<'a>, CountTableError> {
        let mut ordered = SortedRunCounts::with_policy(policy, budget)?;
        ordered.set_cancellation(cancellation.clone());
        let entries = self.contrast_blocks(pattern, scopes, cancellation, |block| {
            for order in block {
                ordered.add(order, 1)?;
            }
            Ok(())
        })?;
        let drain = ordered
            .into_sorted_drain()
            .map_err(|error| cancelled_or(cancellation, error.into()))?;
        Ok(ContrastRows {
            entries,
            scopes,
            drain,
            cancellation: cancellation.clone(),
            finished: false,
        })
    }
}

/// The rows of [`CountTable::ranked_contrast_rows`].
pub struct ContrastRows<'a> {
    pub entries: u64,
    scopes: &'a Scopes,
    drain: SortedDrain<ContrastOrder>,
    cancellation: CancellationFlag,
    finished: bool,
}

impl Iterator for ContrastRows<'_> {
    type Item = Result<ContrastRow, CountTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        if let Err(error) = check_cancellation(&self.cancellation) {
            self.finished = true;
            return Some(Err(error));
        }
        match self.drain.next_entry() {
            Ok(Some((order, _))) => Some(Ok(order.row(self.scopes.tokens()))),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(error) => {
                self.finished = true;
                Some(Err(error.into()))
            }
        }
    }
}
