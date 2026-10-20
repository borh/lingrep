//! Fragment-inclusive metadata baseline.
//!
//! `repository_catalog_fingerprints_are_stable_across_cst_normalization` reads
//! `matcher.patterns()`, and a `fragment` is absent from the runtime catalog by
//! design. This snapshot covers every definition, including fragments, so
//! metadata changes cannot hide behind the runtime projection.

#![cfg(test)]

use std::collections::BTreeMap;

use serde_json::Value;

use super::limits::CompileLimits;
use super::lower::lower_document_with_stats;
use super::model::DefinitionVisibility;
use crate::fsm::PatternSource;
use crate::testing::catalogs::tracked_catalog_sources;

/// Sorts every object's keys so the snapshot does not depend on map ordering.
///
/// `serde_json`'s `preserve_order` feature is unified differently between a
/// package build and a workspace build, so a raw `to_string` is not stable
/// across them. `pattern_compiler_contract.rs` carries `canonical_metadata` for the
/// same reason; this is the same idea over the same value type.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        scalar => scalar.clone(),
    }
}

#[test]
fn repository_catalog_metadata_including_fragments() {
    let limits = CompileLimits::default();
    let baseline = tracked_catalog_sources()
        .into_iter()
        .map(|(name, text)| {
            let lowered = lower_document_with_stats(&PatternSource::named(&name, &text), &limits)
                .unwrap_or_else(|error| panic!("{name} must lower: {error}"));
            let definitions = lowered
                .program
                .definitions
                .iter()
                .map(|definition| {
                    let visibility = match definition.visibility() {
                        Some(DefinitionVisibility::Pattern) => "pattern",
                        Some(DefinitionVisibility::Fragment) => "fragment",
                        None => "lexicon",
                    };
                    let metadata = definition
                        .metadata
                        .iter()
                        .map(|(key, entry)| (key.to_string(), canonical(&entry.value).to_string()))
                        .collect::<BTreeMap<_, _>>();
                    (format!("{visibility} {}", definition.name), metadata)
                })
                .collect::<BTreeMap<_, _>>();
            (name, definitions)
        })
        .collect::<BTreeMap<_, _>>();
    insta::assert_debug_snapshot!(baseline);
}
