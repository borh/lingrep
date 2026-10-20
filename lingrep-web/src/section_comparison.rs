//! Every pattern's section distribution in one table, on one scale.
//!
//! The per-pattern panel in `pattern_detail` answers "where does *this*
//! pattern occur", and answering "does it occur there more than the others do"
//! meant selecting patterns one at a time and holding bars in memory — bars
//! that are scaled to each pattern's own maximum, so even a careful reader was
//! comparing differently-scaled charts. See `vibrato-pipe-0f3`.
//!
//! Nothing new is measured. [`SectionTally`](crate::section_tally::SectionTally)
//! is already row-major `patterns × slots` over the whole catalog, so every
//! row of this table is in the snapshot the detail pane reads; only the
//! projection was per-pattern.

use std::collections::HashSet;

use crate::section_tally::{SectionAvailability, SectionSlot};
use crate::snapshot::PatternDescriptor;

/// Labelled sections carried as columns.
///
/// A catalog may record far more labels than a table can be read across. The
/// cut is by section size rather than by any pattern's counts, so which
/// columns appear does not depend on which patterns matched.
pub const COMPARISON_SECTION_LIMIT: usize = 12;

/// One section, and how much corpus sits under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonColumn {
    pub label: String,
    /// Sentences observed under this section, whatever matched. The
    /// denominator a rate needs: a section holding ten times the sentences
    /// tops every pattern's chart on raw counts alone.
    pub universe: u64,
}

/// One pattern's count in one section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonCell {
    pub count: u64,
    /// Percent of the **table's** maximum, not the row's. The shared scale is
    /// the point of the view; a per-row scale would reproduce exactly the
    /// comparison error this table exists to remove.
    pub intensity: u64,
    /// Tenths of a percent of the section's own universe, so a small section
    /// is not read as unimportant because its bars are short.
    pub rate_tenths: u64,
    /// `rate_tenths` as the template needs it. Formatted here because a
    /// template doing its own division is a second place for the unit to be
    /// got wrong.
    pub rate_label: String,
}

/// One pattern, across the columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonRow {
    pub pattern_key: usize,
    pub pattern_name: String,
    pub cells: Vec<ComparisonCell>,
    /// This pattern's total over the displayed columns, which is what makes a
    /// row whose mass sits in undisplayed sections visible as such.
    pub displayed_total: u64,
}

/// The catalog-wide table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionComparison {
    pub columns: Vec<ComparisonColumn>,
    pub rows: Vec<ComparisonRow>,
    /// Labelled sections the column limit left out.
    pub hidden_sections: usize,
    /// The largest cell in the table; `0` only when no pattern matched under
    /// any displayed section.
    pub max_count: u64,
}

impl SectionComparison {
    /// Builds the table, or `None` when there is nothing to compare.
    ///
    /// `None` rather than an empty table for three distinct cases that all
    /// mean the same thing to a reader: the projection was refused, the corpus
    /// records no labelled sections, or fewer than two patterns have any
    /// labelled count. The last is deliberate — a comparison of one row is not
    /// a comparison, and rendering the band for it invites the reader to
    /// believe the view is broken rather than empty.
    #[must_use]
    pub fn new(sections: &SectionAvailability, patterns: &[PatternDescriptor]) -> Option<Self> {
        Self::selected(sections, patterns, None)
    }

    #[must_use]
    pub fn selected(
        sections: &SectionAvailability,
        patterns: &[PatternDescriptor],
        selected_labels: Option<&[String]>,
    ) -> Option<Self> {
        let SectionAvailability::Available(tally) = sections else {
            return None;
        };
        let selected =
            selected_labels.map(|labels| labels.iter().map(String::as_str).collect::<HashSet<_>>());

        // Columns by section size, largest first, so the cut is a property of
        // the corpus rather than of whichever patterns happened to match.
        let mut labelled = tally
            .slots()
            .iter()
            .enumerate()
            .filter_map(|(slot, descriptor)| match descriptor {
                SectionSlot::Label(label) => Some((slot, label.clone(), tally.universe(slot))),
                SectionSlot::None => None,
            })
            .filter(|(_, _, universe)| *universe > 0)
            .filter(|(_, label, _)| {
                selected
                    .as_ref()
                    .is_none_or(|set| set.contains(label.as_str()))
            })
            .collect::<Vec<_>>();
        if labelled.is_empty() {
            return None;
        }
        labelled.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.1.cmp(&right.1)));
        let hidden_sections = labelled.len().saturating_sub(COMPARISON_SECTION_LIMIT);
        labelled.truncate(COMPARISON_SECTION_LIMIT);

        let mut max_count = 0u64;
        let mut rows = Vec::new();
        for pattern in patterns {
            let counts = labelled
                .iter()
                .map(|(slot, _, _)| tally.count(pattern.key, *slot))
                .collect::<Vec<_>>();
            let displayed_total = counts.iter().copied().fold(0u64, u64::saturating_add);
            // A pattern with no labelled occurrence contributes an empty row
            // and nothing else, so leave it out rather than pad the table.
            if displayed_total == 0 {
                continue;
            }
            max_count = max_count.max(counts.iter().copied().max().unwrap_or(0));
            rows.push((pattern, counts, displayed_total));
        }
        if rows.len() < 2 {
            return None;
        }

        let columns = labelled
            .iter()
            .map(|(_, label, universe)| ComparisonColumn {
                label: label.clone(),
                universe: *universe,
            })
            .collect::<Vec<_>>();
        let rows = rows
            .into_iter()
            .map(|(pattern, counts, displayed_total)| ComparisonRow {
                pattern_key: pattern.key.index(),
                pattern_name: pattern.name.to_string(),
                cells: counts
                    .iter()
                    .zip(columns.iter())
                    .map(|(count, column)| {
                        let rate_tenths = scale(*count, column.universe, 1_000);
                        ComparisonCell {
                            count: *count,
                            intensity: scale(*count, max_count, 100),
                            rate_tenths,
                            rate_label: format!("{}.{}%", rate_tenths / 10, rate_tenths % 10),
                        }
                    })
                    .collect(),
                displayed_total,
            })
            .collect();

        Some(Self {
            columns,
            rows,
            hidden_sections,
            max_count,
        })
    }
}

/// `value / total * factor`, rounded to nearest, saturating, and `0` when
/// there is no denominator. Kept in one place because the two callers differ
/// only in `factor` and an inconsistency between them would be invisible.
fn scale(value: u64, total: u64, factor: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    let numerator = u128::from(value) * u128::from(factor) + u128::from(total) / 2;
    u64::try_from(numerator / u128::from(total)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section_tally::{SpanEvent, SpanEvents, build};
    use crate::snapshot::PatternKey;

    fn descriptor(index: usize, name: &str) -> PatternDescriptor {
        PatternDescriptor {
            key: PatternKey::new(index).unwrap(),
            name: name.into(),
            exact_total: 0,
            metadata: Box::default(),
        }
    }

    /// `spans` are `(slot, sentence_count)` laid end to end; `matches` gives
    /// the patterns that matched in each sentence, by ordinal.
    fn tally(labels: &[&str], spans: &[(u32, u64)], matches: &[&[usize]]) -> SectionAvailability {
        let coverage = labels
            .iter()
            .map(|label| lingrep_corpus::SectionCoverage {
                label: (*label).to_string(),
                coverage: 1,
            })
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        let mut cursor = 0u64;
        for (slot, count) in spans {
            events.push(SpanEvent {
                slot: *slot,
                first: cursor,
                end: cursor + count,
            });
            cursor += count;
        }
        let mut builder = build(&coverage, 4, SpanEvents::from_events(events))
            .expect("the fixture vocabulary fits");
        for (ordinal, matched) in matches.iter().enumerate() {
            let keys = matched
                .iter()
                .map(|index| PatternKey::new(*index).unwrap())
                .collect::<Vec<_>>();
            builder.observe(ordinal as u64, &keys, &[]).unwrap();
        }
        SectionAvailability::Available(builder.finish().expect("the stream ends cleanly"))
    }

    #[test]
    fn intensity_is_scaled_to_the_table_not_to_each_row() {
        // methods holds sentences 0..2, results 2..4. `a` matches twice under
        // methods and once under results; `b` matches once under results.
        let sections = tally(
            &["methods", "results"],
            &[(0, 2), (1, 2)],
            &[&[0], &[0], &[0, 1], &[]],
        );
        let comparison =
            SectionComparison::new(&sections, &[descriptor(0, "a"), descriptor(1, "b")])
                .expect("two patterns with labelled counts");

        assert_eq!(comparison.max_count, 2);
        let a = &comparison.rows[0];
        let b = &comparison.rows[1];
        // `a`'s own maximum is 2 and `b`'s is 1. Under a per-row scale both
        // rows would show a full cell, which is the error this pins.
        assert_eq!(a.cells[0].intensity, 100, "2 of the table maximum 2");
        assert_eq!(b.cells[1].intensity, 50, "1 of the table maximum 2");
    }

    #[test]
    fn a_rate_reads_against_the_section_rather_than_the_table() {
        // methods is twice the size of results, so equal counts are not equal
        // densities and the rate is what says so.
        let sections = tally(
            &["methods", "results"],
            &[(0, 4), (1, 2)],
            &[&[0], &[0], &[], &[], &[0], &[1]],
        );
        let comparison =
            SectionComparison::new(&sections, &[descriptor(0, "a"), descriptor(1, "b")])
                .expect("two patterns with labelled counts");
        let a = &comparison.rows[0];
        assert_eq!(a.cells[0].count, 2, "two of four methods sentences");
        assert_eq!(a.cells[0].rate_tenths, 500, "50.0% of methods");
        assert_eq!(a.cells[1].count, 1, "one of two results sentences");
        assert_eq!(a.cells[1].rate_tenths, 500, "50.0% of results");
    }

    #[test]
    fn columns_are_cut_by_section_size_not_by_any_pattern() {
        let labels = (0..COMPARISON_SECTION_LIMIT + 2)
            .map(|index| format!("s{index:02}"))
            .collect::<Vec<_>>();
        let borrowed = labels.iter().map(String::as_str).collect::<Vec<_>>();
        // Section n holds n + 1 sentences, so the two smallest are cut.
        let spans = (0..borrowed.len())
            .map(|slot| (slot as u32, slot as u64 + 1))
            .collect::<Vec<_>>();
        let sentences = spans.iter().map(|(_, count)| count).sum::<u64>() as usize;
        let matches = vec![&[0usize, 1usize][..]; sentences];
        let sections = tally(&borrowed, &spans, &matches);
        let comparison =
            SectionComparison::new(&sections, &[descriptor(0, "a"), descriptor(1, "b")])
                .expect("two patterns with labelled counts");

        assert_eq!(comparison.columns.len(), COMPARISON_SECTION_LIMIT);
        assert_eq!(comparison.hidden_sections, 2);
        assert_eq!(
            comparison.columns[0].label, "s13",
            "largest section first, and the cut is by size"
        );
        assert!(
            !comparison
                .columns
                .iter()
                .any(|column| column.label == "s00"),
            "the two smallest sections are the ones dropped"
        );
    }

    #[test]
    fn one_comparable_pattern_is_not_a_comparison() {
        let sections = tally(&["methods"], &[(0, 2)], &[&[0], &[0]]);
        assert!(
            SectionComparison::new(&sections, &[descriptor(0, "a"), descriptor(1, "b")]).is_none(),
            "`b` has no labelled count, so there is one row and nothing to compare"
        );
    }

    #[test]
    fn a_refused_projection_has_nothing_to_compare() {
        let refused =
            SectionAvailability::VocabularyTooLarge(crate::section_tally::SectionRefusal {
                labels: 100,
                patterns: 100,
            });
        assert!(SectionComparison::new(&refused, &[descriptor(0, "a")]).is_none());
    }
}
