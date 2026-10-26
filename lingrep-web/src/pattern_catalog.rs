use std::collections::{HashMap, HashSet};

use crate::snapshot::{CompletedSnapshot, PatternDescriptor, PatternMetadataValue, PatternOrigin};

/// Scanned sentences with at least one published match, over every scanned
/// sentence. Omitted oversized sentences are in neither figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceCoverage {
    pub matched: u64,
    pub scanned: u64,
    pub matched_label: String,
    pub scanned_label: String,
    /// `matched / scanned` in percent with one decimal.
    pub percent_label: String,
}

/// `None` before any sentence was scanned.
pub fn sentence_coverage(snapshot: &CompletedSnapshot) -> Option<SentenceCoverage> {
    let scanned = snapshot.observed_sentences;
    if scanned == 0 {
        return None;
    }
    let matched = snapshot.matched_sentences.min(scanned);
    let tenths = matched * 1000 / scanned;
    Some(SentenceCoverage {
        matched,
        scanned,
        matched_label: crate::templates::format_count(matched),
        scanned_label: crate::templates::format_count(scanned),
        percent_label: format!("{}.{}", tenths / 10, tenths % 10),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupingField {
    pub key: String,
    pub label: String,
    pub populated: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataGroup {
    pub field: String,
    pub typed_key: String,
    pub label: String,
}

/// The authored pattern most of a compound's occurrences coincide with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoincidenceBadge {
    pub pattern_name: String,
    pub coincident: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPattern {
    pub pattern_key: usize,
    pub authored_index: usize,
    pub name: String,
    pub exact_total: u64,
    pub groups: Box<[MetadataGroup]>,
    pub auto_load: bool,
    pub coincident: Option<CoincidenceBadge>,
}

fn coincidence_badge(
    patterns: &[PatternDescriptor],
    pattern: &PatternDescriptor,
) -> Option<CoincidenceBadge> {
    let PatternOrigin::Compound { coincident, .. } = &pattern.origin else {
        return None;
    };
    let (authored, count) = coincident.first()?;
    let name = patterns
        .get(authored.index())
        .filter(|candidate| candidate.key == *authored)?
        .name
        .clone();
    Some(CoincidenceBadge {
        pattern_name: name,
        coincident: *count,
        total: pattern.exact_total,
    })
}

pub struct PatternCatalogProjection {
    pub patterns: Vec<CatalogPattern>,
    pub grouping_fields: Vec<GroupingField>,
    pub default_grouping_field: Option<String>,
}

/// Projects distinct published names and scalar metadata, with ordinary patterns before lexicons.
pub fn pattern_metadata_pairs(
    matcher: &lingrep_core::fsm::CompiledCatalog,
) -> Vec<(String, Box<[(String, PatternMetadataValue)]>)> {
    let definitions = matcher
        .patterns()
        .iter()
        .map(|(name, pattern)| (name.as_str(), &pattern.metadata))
        .chain(
            matcher
                .lexicons()
                .iter()
                .map(|lexicon| (lexicon.name(), lexicon.metadata())),
        );
    matcher
        .catalog_policy()
        .published_entries(definitions)
        .map(|(name, metadata)| {
            let mut metadata = metadata
                .iter()
                .filter_map(|(key, value)| {
                    let value = match value {
                        serde_json::Value::String(value) => {
                            PatternMetadataValue::String(value.clone())
                        }
                        serde_json::Value::Number(value) => {
                            PatternMetadataValue::Number(value.to_string())
                        }
                        serde_json::Value::Bool(value) => PatternMetadataValue::Bool(*value),
                        _ => return None,
                    };
                    Some((key.to_string(), value))
                })
                .collect::<Vec<_>>();
            metadata.sort_by(|left, right| left.0.cmp(&right.0));
            (name.to_owned(), metadata.into_boxed_slice())
        })
        .collect()
}

fn display(value: &PatternMetadataValue) -> (String, String) {
    match value {
        PatternMetadataValue::String(value) => (format!("s:{value}"), value.clone()),
        PatternMetadataValue::Number(value) => (format!("n:{value}"), value.clone()),
        PatternMetadataValue::Bool(value) => (format!("b:{value}"), value.to_string()),
    }
}

pub fn project_pattern_catalog(patterns: &[PatternDescriptor]) -> PatternCatalogProjection {
    let mut fields: HashMap<&str, (usize, HashSet<String>)> = HashMap::new();
    for pattern in patterns {
        for (key, value) in &pattern.metadata {
            let entry = fields.entry(key).or_default();
            entry.0 += 1;
            entry.1.insert(display(value).0);
        }
    }
    let preference = |key: &str| match key {
        "category" => 0,
        "subcategory" => 1,
        "family" => 2,
        "type" => 3,
        _ => 4,
    };
    let mut grouping_fields = fields
        .into_iter()
        // A compiler-owned key is publication mechanics, and the heuristic
        // below cannot tell that from research metadata: it only asks whether a
        // key partitions the catalog usefully, and `priority_order` answers yes
        // for the wrong reason — it partitions `sf-experimental.lx` exactly as
        // `layer` does, because a `resolve` statement needed a total order over
        // the same layers. Offering both puts two spellings of one question in
        // the browser, of which the numeric one is the less legible.
        //
        // Withheld by ownership rather than by name, so a policy key added
        // later is withheld the day it lands rather than the day someone
        // notices it in the selector.
        .filter(|(key, _)| !lingrep_core::fsm::policy::compiler_owned_key(key))
        .filter(|(_, (populated, distinct))| *populated >= 2 && distinct.len() * 2 <= *populated)
        .map(|(key, (populated, _))| GroupingField {
            key: key.into(),
            label: key.into(),
            populated,
        })
        .collect::<Vec<_>>();
    grouping_fields.sort_by(|a, b| {
        preference(&a.key)
            .cmp(&preference(&b.key))
            .then_with(|| b.populated.cmp(&a.populated))
            .then_with(|| a.key.cmp(&b.key))
    });
    let candidates = grouping_fields
        .iter()
        .map(|field| field.key.as_str())
        .collect::<HashSet<_>>();
    let mut projected = patterns
        .iter()
        .enumerate()
        .map(|(authored_index, pattern)| CatalogPattern {
            pattern_key: pattern.key.index(),
            authored_index,
            name: pattern.name.clone(),
            exact_total: pattern.exact_total,
            groups: pattern
                .metadata
                .iter()
                .filter(|(key, _)| candidates.contains(key.as_str()))
                .map(|(field, value)| {
                    let (typed_key, label) = display(value);
                    MetadataGroup {
                        field: field.clone(),
                        typed_key,
                        label,
                    }
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            auto_load: false,
            coincident: coincidence_badge(patterns, pattern),
        })
        .collect::<Vec<_>>();
    projected.sort_by(|a, b| {
        b.exact_total
            .cmp(&a.exact_total)
            .then_with(|| a.authored_index.cmp(&b.authored_index))
    });
    if let Some(pattern) = projected.iter_mut().find(|pattern| pattern.exact_total > 0) {
        pattern.auto_load = true;
    }
    PatternCatalogProjection {
        patterns: projected,
        default_grouping_field: grouping_fields.first().map(|field| field.key.clone()),
        grouping_fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{PatternKey, PatternOrigin};
    fn pattern(index: usize, name: &str, total: u64, category: Option<&str>) -> PatternDescriptor {
        PatternDescriptor {
            key: PatternKey::new(index).unwrap(),
            name: name.into(),
            exact_total: total,
            metadata: category
                .map(|value| {
                    vec![(
                        "category".into(),
                        PatternMetadataValue::String(value.into()),
                    )]
                    .into_boxed_slice()
                })
                .unwrap_or_default(),
            origin: PatternOrigin::Authored { chained_total: 0 },
        }
    }
    #[test]
    fn a_compound_is_badged_with_its_most_coincident_authored_pattern() {
        let mut compound = pattern(2, "複合項目〔こと ＋ できる〕", 412, None);
        compound.origin = PatternOrigin::Compound {
            components: Box::from([PatternKey::new(0).unwrap(), PatternKey::new(1).unwrap()]),
            coincident: Box::from([
                (PatternKey::new(3).unwrap(), 400),
                (PatternKey::new(0).unwrap(), 12),
            ]),
        };
        let projection = project_pattern_catalog(&[
            pattern(0, "こと", 500, None),
            pattern(1, "できる", 450, None),
            compound,
            pattern(3, "ことができる", 400, None),
        ]);
        let badges = projection
            .patterns
            .iter()
            .map(|pattern| (pattern.name.as_str(), pattern.coincident.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            badges,
            vec![
                ("こと", None),
                ("できる", None),
                (
                    "複合項目〔こと ＋ できる〕",
                    Some(CoincidenceBadge {
                        pattern_name: "ことができる".into(),
                        coincident: 400,
                        total: 412,
                    })
                ),
                ("ことができる", None),
            ]
        );
    }

    #[test]
    fn orders_by_frequency_and_projects_candidates() {
        let projection = project_pattern_catalog(&[
            pattern(0, "zero", 0, None),
            pattern(1, "tie-authored-first", 2, Some("a")),
            pattern(2, "highest", 3, Some("a")),
            pattern(3, "tie-authored-second", 2, None),
        ]);
        assert_eq!(
            projection
                .patterns
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "highest",
                "tie-authored-first",
                "tie-authored-second",
                "zero"
            ]
        );
        assert_eq!(
            projection.default_grouping_field.as_deref(),
            Some("category")
        );
        assert!(projection.patterns[0].auto_load);
    }

    /// A compiler-owned policy key is never a grouping candidate.
    ///
    /// The heuristic asks "does this key partition the catalog usefully", and a
    /// policy key answers yes for the wrong reason: `priority_order` partitions
    /// `sf-experimental.lx` four ways because a `resolve` statement needed a
    /// total order over its layers, not because a researcher has a question
    /// shaped like it. Offering it alongside `layer` — the same 77 patterns,
    /// the same four-way split, renumbered — puts two spellings of one question
    /// in the browser and makes the numeric one look like the authoritative
    /// answer.
    ///
    /// The fixture carries both keys to prove that suppressing
    /// `priority_order` preserves the researcher's key. A filter over the
    /// wrong predicate would drop both.
    #[test]
    fn a_policy_key_is_not_offered_as_a_grouping() {
        let with_both = |index: usize, layer: &str, rank: u8| PatternDescriptor {
            key: PatternKey::new(index).unwrap(),
            name: format!("p{index}"),
            exact_total: 0,
            metadata: vec![
                ("layer".into(), PatternMetadataValue::String(layer.into())),
                (
                    "priority_order".into(),
                    PatternMetadataValue::Number(rank.to_string()),
                ),
                // Not a policy key, and a list, so it fails the heuristic on
                // its own terms rather than by this rule.
                (
                    "examples".into(),
                    PatternMetadataValue::String(format!("e{index}")),
                ),
            ]
            .into_boxed_slice(),
            origin: PatternOrigin::Authored { chained_total: 0 },
        };
        let projection = project_pattern_catalog(&[
            with_both(0, "S1", 2),
            with_both(1, "S1", 2),
            with_both(2, "S2", 1),
            with_both(3, "S2", 1),
        ]);

        let offered = projection
            .grouping_fields
            .iter()
            .map(|field| field.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            offered,
            vec!["layer"],
            "the researcher's key survives and the policy key does not"
        );
        assert_eq!(projection.default_grouping_field.as_deref(), Some("layer"));

        // And a suppressed key is absent from the per-pattern groups too, not
        // merely from the selector. A pattern still carrying a `priority_order`
        // group would let the UI render a grouping it never offered.
        assert!(
            projection.patterns.iter().all(|pattern| {
                pattern
                    .groups
                    .iter()
                    .all(|group| group.field != "priority_order")
            }),
            "{:?}",
            projection.patterns
        );
    }

    fn coverage_snapshot(matched: u64, scanned: u64) -> CompletedSnapshot {
        CompletedSnapshot {
            search_scope: Default::default(),
            excluded_sentences: 0,
            revision: crate::revision::RevisionId(1),
            corpus_identity: crate::corpus_runtime::CorpusIdentity::new(""),
            source: "".into(),
            derive_compounds: false,
            compound_category: crate::snapshot::CompoundCategory::Compound,
            patterns: Box::default(),
            match_refs: Box::default(),
            capture_catalog: Box::default(),
            sections: crate::section_tally::SectionAvailability::Available(
                crate::section_tally::SectionTally::unlabelled(),
            ),
            observed_sentences: scanned,
            matched_sentences: matched,
            documents: None,
            sentences_matched: scanned,
            oversized_omissions: None,
        }
    }

    #[test]
    fn coverage_is_absent_before_any_sentence_and_truncates_to_tenths() {
        assert!(sentence_coverage(&coverage_snapshot(0, 0)).is_none());
        let third = sentence_coverage(&coverage_snapshot(1, 3)).unwrap();
        assert_eq!(
            (third.matched, third.scanned, third.percent_label.as_str()),
            (1, 3, "33.3")
        );
        assert_eq!(
            sentence_coverage(&coverage_snapshot(2, 3))
                .unwrap()
                .percent_label,
            "66.6"
        );
        assert_eq!(
            sentence_coverage(&coverage_snapshot(0, 7))
                .unwrap()
                .percent_label,
            "0.0"
        );
        assert_eq!(
            sentence_coverage(&coverage_snapshot(7, 7))
                .unwrap()
                .percent_label,
            "100.0"
        );
    }
}
