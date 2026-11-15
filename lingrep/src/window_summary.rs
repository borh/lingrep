//! Aggregates exact counts for token-window catalogs.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use compact_str::CompactString;
use lingrep_core::fsm::{ProjectedKeyField, TokenWindow};
use lingrep_core::spill::{TempBudget, TempReservation};
use lingrep_core::token_match::{AttributeKey, TokenSequence};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::summary::{AggregationGroup, SummaryPartition, SummaryRow, SummaryRows};

const MAX_WIDTH: usize = 8;
const ORDER_WORDS: usize = 3 + MAX_WIDTH;
const RUN_RECORDS: usize = 1 << 22;

const PAD: u32 = 0;
const ABSENT: u32 = 1;
const FIRST_SYMBOL: u32 = 2;
const VALUE_END: u8 = 0xFF;

struct Pattern {
    ordinal: u32,
    width: usize,
    names: Vec<CompactString>,
}

struct Runs<const N: usize> {
    buffer: Vec<[u32; N]>,
    files: Vec<File>,
    reservation: TempReservation,
}

impl<const N: usize> Runs<N> {
    fn new(budget: &Arc<TempBudget>) -> io::Result<Self> {
        Ok(Self {
            buffer: Vec::new(),
            files: Vec::new(),
            reservation: budget.reserve(0)?,
        })
    }

    fn push(&mut self, pool: &rayon::ThreadPool, record: [u32; N]) -> io::Result<()> {
        self.buffer.push(record);
        if self.buffer.len() == RUN_RECORDS {
            self.spill(pool)?;
        }
        Ok(())
    }

    fn spill(&mut self, pool: &rayon::ThreadPool) -> io::Result<()> {
        pool.install(|| self.buffer.par_sort_unstable());
        self.reservation.grow((self.buffer.len() * N * 4) as u64)?;
        let mut writer = BufWriter::with_capacity(1 << 20, tempfile::tempfile()?);
        for record in &self.buffer {
            for word in record {
                writer.write_all(&word.to_le_bytes())?;
            }
        }
        let mut file = writer
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        file.seek(SeekFrom::Start(0))?;
        self.files.push(file);
        self.buffer.clear();
        Ok(())
    }

    fn into_sorted(mut self, pool: &rayon::ThreadPool) -> io::Result<Sorted<N>> {
        pool.install(|| self.buffer.par_sort_unstable());
        let mut sources = vec![Source::Memory(self.buffer.into_iter())];
        sources.extend(
            self.files
                .into_iter()
                .map(|file| Source::File(BufReader::with_capacity(1 << 18, file))),
        );
        let mut heads = BinaryHeap::with_capacity(sources.len());
        for (index, source) in sources.iter_mut().enumerate() {
            if let Some(record) = source.next()? {
                heads.push(Reverse((record, index)));
            }
        }
        Ok(Sorted {
            sources,
            heads,
            _reservation: self.reservation,
        })
    }
}

enum Source<const N: usize> {
    Memory(std::vec::IntoIter<[u32; N]>),
    File(BufReader<File>),
}

impl<const N: usize> Source<N> {
    fn next(&mut self) -> io::Result<Option<[u32; N]>> {
        match self {
            Self::Memory(records) => Ok(records.next()),
            Self::File(reader) => {
                if reader.fill_buf()?.is_empty() {
                    return Ok(None);
                }
                let mut bytes = [0_u8; 4 * ORDER_WORDS];
                let bytes = &mut bytes[..4 * N];
                reader.read_exact(bytes)?;
                let mut record = [0_u32; N];
                for (word, bytes) in record.iter_mut().zip(bytes.as_chunks::<4>().0) {
                    *word = u32::from_le_bytes(*bytes);
                }
                Ok(Some(record))
            }
        }
    }
}

struct Sorted<const N: usize> {
    sources: Vec<Source<N>>,
    heads: BinaryHeap<Reverse<([u32; N], usize)>>,
    _reservation: TempReservation,
}

impl<const N: usize> Sorted<N> {
    fn next(&mut self) -> io::Result<Option<[u32; N]>> {
        let Some(Reverse((record, index))) = self.heads.pop() else {
            return Ok(None);
        };
        if let Some(next) = self.sources[index].next()? {
            self.heads.push(Reverse((next, index)));
        }
        Ok(Some(record))
    }
}

pub struct WindowTally {
    patterns: Vec<Pattern>,
    attributes: Vec<AttributeKey>,
    width: usize,
    symbols: FxHashMap<Box<[u8]>, u32>,
    key: Vec<u8>,
    sentence: Vec<u32>,
    windows: Runs<MAX_WIDTH>,
    budget: Arc<TempBudget>,
}

impl WindowTally {
    /// `None` unless every definition keys each of its positions, in order,
    /// by one attribute list shared by the whole catalog.
    pub fn new(
        windows: &[TokenWindow],
        pattern_ordinals: &BTreeMap<String, u64>,
        budget: Arc<TempBudget>,
    ) -> io::Result<Option<Self>> {
        let Some(first) = windows.first() else {
            return Ok(None);
        };
        let attributes = first
            .key
            .iter()
            .take_while(|field| field.position == 0)
            .map(|field| field.attribute.clone())
            .collect::<Vec<_>>();
        if attributes.is_empty() {
            return Ok(None);
        }
        let mut patterns = Vec::with_capacity(windows.len());
        for window in windows {
            let positional = window.key.len() == window.width * attributes.len()
                && window.key.iter().enumerate().all(|(index, field)| {
                    field.position == index / attributes.len()
                        && field.attribute == attributes[index % attributes.len()]
                });
            let ordinal = pattern_ordinals
                .get(window.name.as_str())
                .and_then(|ordinal| u32::try_from(*ordinal).ok());
            let (true, Some(ordinal), 1..=MAX_WIDTH) = (positional, ordinal, window.width) else {
                return Ok(None);
            };
            patterns.push(Pattern {
                ordinal,
                width: window.width,
                names: window.key.iter().map(|field| field.name.clone()).collect(),
            });
        }
        let width = patterns
            .iter()
            .map(|pattern| pattern.width)
            .max()
            .unwrap_or(0);
        Ok(Some(Self {
            patterns,
            attributes,
            width,
            symbols: FxHashMap::default(),
            key: Vec::new(),
            sentence: Vec::new(),
            windows: Runs::new(&budget)?,
            budget,
        }))
    }

    /// Adds one sentence and returns its window count over every definition.
    pub fn push<S: TokenSequence + ?Sized>(
        &mut self,
        pool: &rayon::ThreadPool,
        sentence: &S,
    ) -> io::Result<usize> {
        self.sentence.clear();
        for token in 0..sentence.len() {
            self.key.clear();
            let mut complete = true;
            for attribute in &self.attributes {
                match sentence.scalar_key(token, attribute) {
                    Some(value) => {
                        self.key.extend_from_slice(value.as_bytes());
                        self.key.push(VALUE_END);
                    }
                    None => complete = false,
                }
            }
            let symbol = if !complete {
                ABSENT
            } else if let Some(symbol) = self.symbols.get(self.key.as_slice()) {
                *symbol
            } else {
                let symbol = u32::try_from(self.symbols.len())
                    .ok()
                    .and_then(|count| count.checked_add(FIRST_SYMBOL))
                    .ok_or_else(|| io::Error::other("window symbol count exceeds u32"))?;
                self.symbols.insert(self.key.as_slice().into(), symbol);
                symbol
            };
            self.sentence.push(symbol);
        }
        for start in 0..self.sentence.len() {
            let mut record = [PAD; MAX_WIDTH];
            for (slot, symbol) in record
                .iter_mut()
                .zip(&self.sentence[start..])
                .take(self.width)
            {
                *slot = *symbol;
            }
            self.windows.push(pool, record)?;
        }
        Ok(self
            .patterns
            .iter()
            .map(|pattern| (sentence.len() + 1).saturating_sub(pattern.width))
            .sum())
    }

    pub fn finalize(self, pool: &rayon::ThreadPool) -> io::Result<FinalizedWindows> {
        let mut by_value = self.symbols.into_iter().collect::<Vec<_>>();
        pool.install(|| {
            by_value.par_sort_unstable_by(|(left, _), (right, _)| {
                left.split(|byte| *byte == VALUE_END)
                    .cmp(right.split(|byte| *byte == VALUE_END))
            });
        });
        let mut ranks = vec![0_u32; by_value.len() + FIRST_SYMBOL as usize];
        let mut values = Vec::with_capacity(by_value.len() * self.attributes.len());
        for (rank, (key, symbol)) in by_value.into_iter().enumerate() {
            ranks[symbol as usize] = rank as u32;
            for value in key
                .split(|byte| *byte == VALUE_END)
                .take(self.attributes.len())
            {
                values.push(
                    std::str::from_utf8(value)
                        .map_err(|_| io::Error::other("window symbol is not UTF-8"))?
                        .into(),
                );
            }
        }

        let mut ordered = Runs::<ORDER_WORDS>::new(&self.budget)?;
        let mut unkeyed = 0_u64;
        let mut windows = self.windows.into_sorted(pool)?;
        let mut current = [PAD; MAX_WIDTH];
        let mut counts = [0_u64; MAX_WIDTH + 1];
        let mut flush = |window: &[u32; MAX_WIDTH],
                         counts: &mut [u64; MAX_WIDTH + 1],
                         from: usize|
         -> io::Result<()> {
            for pattern in &self.patterns {
                let count = counts[pattern.width];
                if pattern.width <= from || count == 0 {
                    continue;
                }
                let tokens = &window[..pattern.width];
                if tokens.contains(&ABSENT) {
                    unkeyed += count;
                    continue;
                }
                let inverted = !count;
                let mut record = [0_u32; ORDER_WORDS];
                record[0] = pattern.ordinal;
                record[1] = (inverted >> 32) as u32;
                record[2] = inverted as u32;
                for (slot, symbol) in record[3..].iter_mut().zip(tokens) {
                    *slot = ranks[*symbol as usize];
                }
                ordered.push(pool, record)?;
            }
            counts[from + 1..].fill(0);
            Ok(())
        };
        while let Some(window) = windows.next()? {
            let shared = window
                .iter()
                .zip(&current)
                .take_while(|(left, right)| left == right)
                .count();
            if shared < MAX_WIDTH {
                flush(&current, &mut counts, shared)?;
                current = window;
            }
            for (width, symbol) in window.iter().enumerate() {
                if *symbol != PAD {
                    counts[width + 1] += 1;
                }
            }
        }
        flush(&current, &mut counts, 0)?;
        drop(windows);

        Ok(FinalizedWindows {
            patterns: self.patterns,
            attributes: self.attributes.len(),
            values,
            ordered: ordered.into_sorted(pool)?,
            unkeyed,
            budget: self.budget,
        })
    }
}

pub struct FinalizedWindows {
    patterns: Vec<Pattern>,
    attributes: usize,
    values: Vec<Box<str>>,
    ordered: Sorted<ORDER_WORDS>,
    unkeyed: u64,
    budget: Arc<TempBudget>,
}

impl FinalizedWindows {
    /// Windows left out of the rows because a token lacked a key attribute.
    pub fn records_unkeyed(&self) -> u64 {
        self.unkeyed
    }

    pub fn peak_temp_bytes(&self) -> u64 {
        self.budget.peak_used_bytes()
    }
}

impl SummaryRows for FinalizedWindows {
    fn visit(
        &mut self,
        top: Option<u64>,
        mut visitor: impl FnMut(SummaryRow) -> io::Result<()>,
    ) -> io::Result<u64> {
        let mut current = None;
        let mut rank = 0_u64;
        let mut emitted = 0_u64;
        while let Some(record) = self.ordered.next()? {
            if current.map(|pattern: &Pattern| pattern.ordinal) != Some(record[0]) {
                current = self
                    .patterns
                    .iter()
                    .find(|pattern| pattern.ordinal == record[0]);
                rank = 0;
            }
            let pattern = current.ok_or_else(|| io::Error::other("unknown window pattern"))?;
            rank += 1;
            if top.is_some_and(|limit| rank > limit) {
                continue;
            }
            let mut fields = Vec::with_capacity(pattern.names.len());
            for (symbol, names) in record[3..]
                .iter()
                .zip(pattern.names.chunks(self.attributes))
            {
                let first = *symbol as usize * self.attributes;
                for (name, value) in names.iter().zip(&self.values[first..]) {
                    fields.push(ProjectedKeyField {
                        name: name.clone(),
                        value: value.to_string(),
                    });
                }
            }
            visitor(SummaryRow {
                partition: SummaryPartition::pattern(u64::from(pattern.ordinal)),
                rank,
                group: AggregationGroup::Key(fields),
                count: !(u64::from(record[1]) << 32 | u64::from(record[2])),
            })?;
            emitted += 1;
        }
        Ok(emitted)
    }
}
