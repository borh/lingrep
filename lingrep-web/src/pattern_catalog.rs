use std::collections::{HashMap, HashSet};

use crate::snapshot::{PatternDescriptor, PatternMetadataValue};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPattern {
    pub pattern_key: usize,
    pub authored_index: usize,
    pub name: String,
    pub exact_total: u64,
    pub groups: Box<[MetadataGroup]>,
    pub auto_load: bool,
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
    use crate::snapshot::PatternKey;
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
        }
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
}
