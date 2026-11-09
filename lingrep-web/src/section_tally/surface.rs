//! Counts sentences per pattern, surface and section slot; patterns exceeding the vocabulary or byte limit are withheld in full.

use std::collections::HashMap;

use crate::snapshot::PatternKey;

/// Maximum distinct surfaces per pattern before its cross-tabulation is withheld.
pub const SURFACE_VOCABULARY_LIMIT: usize = 64;

/// Maximum retained bytes for surface cross-tabulations per workbench.
pub const SURFACE_CEILING_BYTES: usize = 262_144;

/// Why one pattern has no cross-tabulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceRefusal {
    /// The pattern carried more than [`SURFACE_VOCABULARY_LIMIT`] surfaces.
    TooManySurfaces { limit: usize },
    /// [`SURFACE_CEILING_BYTES`] was already spent when this pattern required a
    /// new surface. Order-dependent: the alternative requires predicting
    /// vocabulary in advance.
    BudgetExhausted,
}

/// One pattern's cross-tabulation, or why it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceSections<'a> {
    Counted {
        /// Surfaces in first-seen order.
        surfaces: &'a [Box<str>],
        /// Row-major `surfaces × slots`.
        counts: &'a [u64],
        /// Per surface, sentences in which the pattern matched as it *at all*.
        ///
        /// Not the row sum: a sentence inside two sections is counted in both
        /// slots, so summing `counts` across a row over-counts exactly the
        /// sentences the section vocabulary overlaps on. This is recorded once
        /// per sentence instead, which is why it is a column of its own.
        totals: &'a [u64],
        slots: usize,
    },
    Refused(&'a SurfaceRefusal),
}

impl SurfaceSections<'_> {
    /// Sentences in which `pattern` matched as `surface`, within `slot`.
    ///
    /// `None` when the pattern was withheld or the surface was never seen,
    /// which a caller must not read as zero: the first is "not counted" and
    /// the second cannot arise for a surface the run actually observed.
    #[must_use]
    pub fn count(&self, surface: &str, slot: usize) -> Option<u64> {
        let Self::Counted {
            surfaces,
            counts,
            slots,
            ..
        } = self
        else {
            return None;
        };
        let row = surfaces.iter().position(|held| &**held == surface)?;
        counts.get(row * slots + slot).copied()
    }

    /// Every counted surface with the sentences it matched in, in first-seen
    /// order. Empty for a withheld pattern, which a caller must tell apart from
    /// a pattern that matched nothing.
    pub fn variations(&self) -> impl Iterator<Item = (&str, u64)> {
        let (surfaces, totals) = match self {
            Self::Counted {
                surfaces, totals, ..
            } => (*surfaces, *totals),
            Self::Refused(_) => (&[][..], &[][..]),
        };
        surfaces
            .iter()
            .map(|surface| &**surface)
            .zip(totals.iter().copied())
    }
}

/// The retained cross-tabulation: one row per pattern, dense within a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceTally {
    slots: usize,
    rows: Box<[SurfaceRow]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SurfaceRow {
    Counted {
        surfaces: Box<[Box<str>]>,
        counts: Box<[u64]>,
        totals: Box<[u64]>,
    },
    Refused(SurfaceRefusal),
}

impl SurfaceTally {
    /// Returns a pattern's cross-tabulation; an out-of-catalog pattern returns a budget-withheld result.
    #[must_use]
    pub fn pattern(&self, pattern: PatternKey) -> SurfaceSections<'_> {
        static EXHAUSTED: SurfaceRefusal = SurfaceRefusal::BudgetExhausted;
        match self.rows.get(pattern.index()) {
            Some(SurfaceRow::Counted {
                surfaces,
                counts,
                totals,
            }) => SurfaceSections::Counted {
                surfaces,
                counts,
                totals,
                slots: self.slots,
            },
            Some(SurfaceRow::Refused(refusal)) => SurfaceSections::Refused(refusal),
            None => SurfaceSections::Refused(&EXHAUSTED),
        }
    }
}

#[derive(Debug)]
pub(crate) struct SurfaceTallyBuilder {
    slots: usize,
    rows: Vec<SurfaceRowBuilder>,
    charged: usize,
}

#[derive(Debug)]
enum SurfaceRowBuilder {
    Counted {
        /// Surface to its row index. Owned keys, so a lookup by `&str` costs
        /// nothing and only a first sighting allocates.
        index: HashMap<Box<str>, usize>,
        order: Vec<Box<str>>,
        counts: Vec<u64>,
        totals: Vec<u64>,
    },
    Refused(SurfaceRefusal),
}

/// What one more surface costs: its counts, its total, its two owned copies,
/// and the map and vector overhead the two containers carry per entry.
fn surface_cost(slots: usize, surface: &str) -> usize {
    slots * 8 + 8 + 2 * surface.len() + 64
}

impl SurfaceTallyBuilder {
    pub(crate) fn new(patterns: usize, slots: usize) -> Self {
        Self {
            slots,
            rows: (0..patterns).map(|_| empty_row()).collect(),
            charged: 0,
        }
    }

    pub(crate) fn ensure_patterns(&mut self, patterns: usize) {
        self.rows.resize_with(patterns, empty_row);
    }

    /// Records one sentence in which `pattern` matched as `surface`, within
    /// `slot`.
    ///
    /// The caller supplies distinct `(pattern, surface)` pairs per sentence;
    /// the unit is the sentence, matching the two-dimensional table this
    /// extends, so a sentence carrying one surface twice counts once.
    pub(crate) fn record(&mut self, pattern: PatternKey, surface: &str, slot: usize) {
        let Some(position) = self.position_of(pattern, surface) else {
            return;
        };
        let slots = self.slots;
        let Some(SurfaceRowBuilder::Counted { counts, .. }) = self.rows.get_mut(pattern.index())
        else {
            return;
        };
        if let Some(cell) = counts.get_mut(position * slots + slot) {
            *cell += 1;
        }
    }

    /// Records one sentence in which `pattern` matched as `surface`, whatever
    /// sections held it.
    ///
    /// Called once per sentence, ahead of the per-slot [`Self::record`] calls,
    /// because the whole-corpus figure the variation list ranks on cannot be
    /// recovered from the slot row: a sentence inside two sections appears in
    /// two slots and is still one sentence.
    pub(crate) fn record_sentence(&mut self, pattern: PatternKey, surface: &str) {
        let Some(position) = self.position_of(pattern, surface) else {
            return;
        };
        let Some(SurfaceRowBuilder::Counted { totals, .. }) = self.rows.get_mut(pattern.index())
        else {
            return;
        };
        if let Some(cell) = totals.get_mut(position) {
            *cell += 1;
        }
    }

    fn position_of(&mut self, pattern: PatternKey, surface: &str) -> Option<usize> {
        let Some(SurfaceRowBuilder::Counted { index, order, .. }) = self.rows.get(pattern.index())
        else {
            return None;
        };
        // Both values are copied out before any mutation: `refuse` below takes
        // `&mut self`, and the row is borrowed from `self`.
        let existing = index.get(surface).copied();
        let saturated = order.len() >= SURFACE_VOCABULARY_LIMIT;
        if let Some(position) = existing {
            return Some(position);
        }
        if saturated {
            self.refuse(
                pattern,
                SurfaceRefusal::TooManySurfaces {
                    limit: SURFACE_VOCABULARY_LIMIT,
                },
            );
            return None;
        }
        let slots = self.slots;
        let cost = surface_cost(slots, surface);
        if self.charged + cost > SURFACE_CEILING_BYTES {
            self.refuse(pattern, SurfaceRefusal::BudgetExhausted);
            return None;
        }
        self.charged += cost;
        let SurfaceRowBuilder::Counted {
            index,
            order,
            counts,
            totals,
        } = &mut self.rows[pattern.index()]
        else {
            return None;
        };
        let position = order.len();
        let owned: Box<str> = surface.into();
        index.insert(owned.clone(), position);
        order.push(owned);
        counts.resize((position + 1) * slots, 0);
        totals.resize(position + 1, 0);
        Some(position)
    }

    fn refuse(&mut self, pattern: PatternKey, refusal: SurfaceRefusal) {
        let slots = self.slots;
        let Some(row) = self.rows.get_mut(pattern.index()) else {
            return;
        };
        if let SurfaceRowBuilder::Counted { order, .. } = row {
            let released = order
                .iter()
                .map(|surface| surface_cost(slots, surface))
                .sum::<usize>();
            self.charged = self.charged.saturating_sub(released);
        }
        self.rows[pattern.index()] = SurfaceRowBuilder::Refused(refusal);
    }

    pub(crate) fn finish(self) -> SurfaceTally {
        SurfaceTally {
            slots: self.slots,
            rows: self
                .rows
                .into_iter()
                .map(|row| match row {
                    SurfaceRowBuilder::Counted {
                        order,
                        counts,
                        totals,
                        ..
                    } => SurfaceRow::Counted {
                        surfaces: order.into_boxed_slice(),
                        counts: counts.into_boxed_slice(),
                        totals: totals.into_boxed_slice(),
                    },
                    SurfaceRowBuilder::Refused(refusal) => SurfaceRow::Refused(refusal),
                })
                .collect(),
        }
    }
}

fn empty_row() -> SurfaceRowBuilder {
    SurfaceRowBuilder::Counted {
        index: HashMap::new(),
        order: Vec::new(),
        counts: Vec::new(),
        totals: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(index: usize) -> PatternKey {
        PatternKey::new(index).expect("a pattern key")
    }

    #[test]
    fn a_surface_is_counted_once_per_sentence_and_slot() {
        let mut builder = SurfaceTallyBuilder::new(2, 3);
        builder.record(key(0), "した", 0);
        builder.record(key(0), "した", 0);
        builder.record(key(0), "した", 2);
        builder.record(key(0), "する", 0);
        let tally = builder.finish();
        let row = tally.pattern(key(0));
        assert_eq!(row.count("した", 0), Some(2));
        assert_eq!(row.count("した", 1), Some(0));
        assert_eq!(row.count("した", 2), Some(1));
        assert_eq!(row.count("する", 0), Some(1));
    }

    #[test]
    fn a_pattern_with_no_matches_counts_nothing_and_is_not_refused() {
        let builder = SurfaceTallyBuilder::new(2, 3);
        let tally = builder.finish();
        assert!(matches!(
            tally.pattern(key(1)),
            SurfaceSections::Counted { surfaces, .. } if surfaces.is_empty()
        ));
    }

    /// Past the limit the row goes away entirely. A truncated vocabulary would
    /// answer some surfaces and silently zero the rest.
    #[test]
    fn a_pattern_past_the_surface_limit_is_refused_whole() {
        let mut builder = SurfaceTallyBuilder::new(1, 2);
        for index in 0..=SURFACE_VOCABULARY_LIMIT {
            builder.record(key(0), &format!("s{index}"), 0);
        }
        let tally = builder.finish();
        assert_eq!(
            tally.pattern(key(0)),
            SurfaceSections::Refused(&SurfaceRefusal::TooManySurfaces {
                limit: SURFACE_VOCABULARY_LIMIT
            })
        );
        assert_eq!(
            tally.pattern(key(0)).count("s0", 0),
            None,
            "a refused row answers nothing, not zero"
        );
    }

    #[test]
    fn refusing_a_pattern_returns_its_bytes_to_the_budget() {
        let mut builder = SurfaceTallyBuilder::new(2, 2);
        for index in 0..SURFACE_VOCABULARY_LIMIT {
            builder.record(key(0), &format!("s{index}"), 0);
        }
        let held = builder.charged;
        assert!(held > 0, "the first pattern charged the budget");
        builder.record(key(0), "one too many", 0);
        assert_eq!(builder.charged, 0, "the refusal released every byte");
        builder.record(key(1), "あ", 0);
        let tally = builder.finish();
        assert_eq!(
            tally.pattern(key(1)).count("あ", 0),
            Some(1),
            "the second pattern counts on the returned budget"
        );
    }

    #[test]
    fn the_budget_refuses_rather_than_growing_without_bound() {
        // Wide enough that a handful of surfaces exhausts the ceiling.
        let slots = SURFACE_CEILING_BYTES / 8 / 4;
        let mut builder = SurfaceTallyBuilder::new(1, slots);
        for index in 0..8 {
            builder.record(key(0), &format!("s{index}"), 0);
        }
        assert!(builder.charged <= SURFACE_CEILING_BYTES);
        let tally = builder.finish();
        assert_eq!(
            tally.pattern(key(0)),
            SurfaceSections::Refused(&SurfaceRefusal::BudgetExhausted)
        );
    }
}
