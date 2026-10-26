//! The Chains view of a base pattern: every derived compound item it is a
//! component of, grouped by where it sits in the chain.

use crate::snapshot::{CompletedSnapshot, PatternKey, PatternOrigin};
use crate::templates::format_count;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRow {
    pub compound_key: usize,
    pub compound_name: String,
    /// The other member of a two-member chain, or the compound name for a
    /// longer one.
    pub label: String,
    pub length: usize,
    /// One-based positions of the pattern in the chain, comma-separated; a
    /// chain can contain the same pattern more than once.
    pub positions: String,
    /// The compound's own corpus count.
    pub count: u64,
    pub count_label: String,
}

/// Rows are ordered by count, then by compound key, which is derivation order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChainPanel {
    /// Two-member chains ending in the pattern: its left neighbours.
    pub before: Vec<ChainRow>,
    /// Two-member chains starting with the pattern: its right neighbours.
    pub after: Vec<ChainRow>,
    /// Chains of three or more members.
    pub longer: Vec<ChainRow>,
}

/// The share of a base pattern's occurrences that are a component of some
/// chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainShare {
    pub chained_total: u64,
    pub chained_label: String,
    /// Occurrences with no other published match ending at their start or
    /// starting at their end: `exact_total - chained_total`.
    pub standalone_total: u64,
    pub standalone_label: String,
    /// `chained_total / exact_total` in tenths of a percent.
    pub percent_tenths: u64,
    pub percent_label: String,
}

/// `None` when the run derived no compounds, when `key` is a compound, or
/// when the pattern has no occurrences.
pub fn chain_share(snapshot: &CompletedSnapshot, key: PatternKey) -> Option<ChainShare> {
    if !snapshot.derive_compounds {
        return None;
    }
    let pattern = snapshot.pattern(key)?;
    let PatternOrigin::Authored { chained_total } = pattern.origin else {
        return None;
    };
    if pattern.exact_total == 0 {
        return None;
    }
    let percent_tenths = chained_total * 1000 / pattern.exact_total;
    let standalone_total = pattern.exact_total.saturating_sub(chained_total);
    Some(ChainShare {
        chained_total,
        chained_label: format_count(chained_total),
        standalone_total,
        standalone_label: format_count(standalone_total),
        percent_tenths,
        percent_label: format!("{}.{}", percent_tenths / 10, percent_tenths % 10),
    })
}

/// `None` when the run derived no compounds, when `key` is itself a compound,
/// or when no compound contains it.
pub fn chain_panel(snapshot: &CompletedSnapshot, key: PatternKey) -> Option<ChainPanel> {
    if !snapshot.derive_compounds || snapshot.pattern(key)?.components().is_some() {
        return None;
    }
    let mut panel = ChainPanel::default();
    for compound in &snapshot.patterns {
        let Some(components) = compound.components() else {
            continue;
        };
        let positions = components
            .iter()
            .enumerate()
            .filter(|(_, component)| **component == key)
            .map(|(index, _)| index + 1)
            .collect::<Vec<_>>();
        if positions.is_empty() {
            continue;
        }
        let row = |label: String| ChainRow {
            compound_key: compound.key.index(),
            compound_name: compound.name.clone(),
            label,
            length: components.len(),
            positions: positions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            count: compound.exact_total,
            count_label: format_count(compound.exact_total),
        };
        match components {
            [left, right] => {
                if *right == key
                    && let Some(neighbour) = snapshot.pattern(*left)
                {
                    panel.before.push(row(neighbour.name.clone()));
                }
                if *left == key
                    && let Some(neighbour) = snapshot.pattern(*right)
                {
                    panel.after.push(row(neighbour.name.clone()));
                }
            }
            _ => panel.longer.push(row(compound.name.clone())),
        }
    }
    for rows in [&mut panel.before, &mut panel.after, &mut panel.longer] {
        rows.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.compound_key.cmp(&b.compound_key))
        });
    }
    (!panel.before.is_empty() || !panel.after.is_empty() || !panel.longer.is_empty())
        .then_some(panel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus_runtime::CorpusIdentity;
    use crate::revision::RevisionId;
    use crate::section_tally::{SectionAvailability, SectionRefusal};
    use crate::snapshot::{CompoundCategory, PatternDescriptor};

    fn key(index: usize) -> PatternKey {
        PatternKey::new(index).unwrap()
    }

    fn authored(index: usize, name: &str, total: u64) -> PatternDescriptor {
        PatternDescriptor {
            key: key(index),
            name: name.into(),
            exact_total: total,
            metadata: Box::default(),
            origin: PatternOrigin::Authored { chained_total: 0 },
        }
    }

    fn compound(index: usize, components: &[usize], total: u64) -> PatternDescriptor {
        PatternDescriptor {
            key: key(index),
            name: format!("c{index}"),
            exact_total: total,
            metadata: Box::default(),
            origin: PatternOrigin::Compound {
                components: components.iter().map(|index| key(*index)).collect(),
                coincident: Box::default(),
            },
        }
    }

    fn snapshot(derive_compounds: bool, patterns: Vec<PatternDescriptor>) -> CompletedSnapshot {
        CompletedSnapshot {
            search_scope: Default::default(),
            excluded_sentences: 0,
            revision: RevisionId(1),
            corpus_identity: CorpusIdentity::new(""),
            source: "".into(),
            derive_compounds,
            compound_category: CompoundCategory::Compound,
            patterns: patterns.into_boxed_slice(),
            match_refs: Box::default(),
            capture_catalog: Box::default(),
            sections: SectionAvailability::VocabularyTooLarge(SectionRefusal {
                labels: 0,
                patterns: 0,
            }),
            observed_sentences: 0,
            matched_sentences: 0,
            documents: None,
            sentences_matched: 0,
            oversized_omissions: None,
        }
    }

    /// A B C adjacent derives A+B, B+C and A+B+C. For B: A before it, C
    /// after it, and the three-member chain with B in the middle.
    fn nested_fixture() -> CompletedSnapshot {
        snapshot(
            true,
            vec![
                authored(0, "A", 10),
                authored(1, "B", 20),
                authored(2, "C", 5),
                compound(3, &[0, 1], 4),
                compound(4, &[1, 2], 7),
                compound(5, &[0, 1, 2], 3),
            ],
        )
    }

    #[test]
    fn groups_compounds_by_the_patterns_position_and_orders_by_count() {
        let panel = chain_panel(&nested_fixture(), key(1)).expect("B is in three compounds");
        let summary = |rows: &[ChainRow]| {
            rows.iter()
                .map(|row| {
                    (
                        row.compound_key,
                        row.label.clone(),
                        row.length,
                        row.positions.clone(),
                        row.count,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            summary(&panel.before),
            vec![(3, "A".into(), 2, "2".into(), 4)]
        );
        assert_eq!(
            summary(&panel.after),
            vec![(4, "C".into(), 2, "1".into(), 7)]
        );
        assert_eq!(
            summary(&panel.longer),
            vec![(5, "c5".into(), 3, "2".into(), 3)]
        );

        let panel = chain_panel(&nested_fixture(), key(0)).expect("A is in two compounds");
        assert!(panel.before.is_empty());
        assert_eq!(
            summary(&panel.after),
            vec![(3, "B".into(), 2, "1".into(), 4)]
        );
        assert_eq!(panel.longer[0].positions, "1");
    }

    #[test]
    fn a_pattern_repeated_in_one_chain_lists_every_position() {
        let snapshot = snapshot(
            true,
            vec![
                authored(0, "A", 10),
                authored(1, "B", 20),
                compound(2, &[0, 0], 1),
                compound(3, &[0, 1, 0], 2),
            ],
        );
        let panel = chain_panel(&snapshot, key(0)).unwrap();
        assert_eq!(panel.before.len(), 1);
        assert_eq!(panel.after.len(), 1);
        assert_eq!(panel.before[0].positions, "1, 2");
        assert_eq!(panel.longer[0].positions, "1, 3");
    }

    #[test]
    fn the_chain_share_is_the_chained_count_over_the_exact_total() {
        let mut fixture = nested_fixture();
        fixture.patterns[1].origin = PatternOrigin::Authored { chained_total: 7 };
        let share = chain_share(&fixture, key(1)).unwrap();
        assert_eq!((share.chained_total, share.percent_tenths), (7, 350));
        assert_eq!(share.standalone_total, 13);
        assert_eq!(share.percent_label, "35.0");
        assert!(chain_share(&fixture, key(5)).is_none(), "a compound");
        fixture.patterns[0].exact_total = 0;
        assert!(chain_share(&fixture, key(0)).is_none(), "no occurrences");
        fixture.derive_compounds = false;
        assert!(chain_share(&fixture, key(1)).is_none());
    }

    #[test]
    fn absent_without_derivation_for_compounds_and_for_uninvolved_patterns() {
        let fixture = nested_fixture();
        assert!(chain_panel(&fixture, key(5)).is_none(), "a compound");
        assert!(
            chain_panel(&snapshot(true, vec![authored(0, "A", 1)]), key(0)).is_none(),
            "no compound contains A"
        );
        let mut off = nested_fixture();
        off.derive_compounds = false;
        assert!(chain_panel(&off, key(1)).is_none());
        assert!(chain_panel(&fixture, key(9)).is_none(), "unknown key");
    }
}
