//! Checks that `detach` never meets its strict bound in the tracked catalogs.
//!
//! `decide_detach` removes a trailing `DA` region only when
//! `operand.start > subject.start`: a region that covers the entire record is
//! not trailing, so the record is kept whole. The catalog's thirteen copula
//! subjects each open on a non-copula head token (もの, 可能, はず, ため, わけ,
//! よう, こと, と), so no `DA` record should start where its subject starts.
//! This test makes that a checked fact over every example sentence and the
//! committed campaign sentences.
//!
//! In `src` rather than `tests/` because it calls the private
//! [`definition_of`](crate::fsm::policy_engine), so the evidence predicate is
//! the engine's own. [`published_baseline`](super::published_baseline) covers
//! group selection, evidence mode, capture handling and statement order; this
//! test names the definition when the bound is met.

#![cfg(test)]

use super::compile_catalog;
use super::limits::CompileLimits;
use crate::fsm::PatternSource;
use crate::fsm::policy_engine::definition_of;
use crate::testing::catalogs::tracked_catalog_sources;
use crate::testing::tokenize_japanese;

/// The fragment whose trailing records `detach` removes.
const COPULA: &str = "DA";

/// The group holding the thirteen copula subjects.
const SUBJECT_GROUP: &str = "trailing_copula";

#[test]
fn no_copula_evidence_record_starts_where_its_subject_starts() {
    let mut collisions: Vec<String> = Vec::new();
    let mut subjects_seen = 0usize;

    for (name, text) in tracked_catalog_sources() {
        let output = compile_catalog(PatternSource::named(&name, &text), CompileLimits::default())
            .unwrap_or_else(|error| panic!("{name} must compile: {error}"));
        let policy = output.matcher.catalog_policy();

        let subjects: Vec<&str> = output
            .matcher
            .patterns()
            .keys()
            .map(compact_str::CompactString::as_str)
            .filter(|definition| {
                policy
                    .get(definition)
                    .is_some_and(|entry| entry.groups.iter().any(|group| group == SUBJECT_GROUP))
            })
            .collect();
        if subjects.is_empty() {
            continue;
        }
        subjects_seen += subjects.len();

        let mut sentences: Vec<Vec<crate::token_match::TokenInfo>> = Vec::new();
        for pattern in output.matcher.patterns().values() {
            let Some(examples) = pattern
                .metadata
                .get("examples")
                .and_then(|value| value.as_array())
            else {
                continue;
            };
            sentences.extend(
                examples
                    .iter()
                    .filter_map(|entry| entry.as_str())
                    .map(tokenize_japanese),
            );
        }
        sentences.extend(
            crate::testing::campaign_sentences::committed_sentences()
                .into_iter()
                .map(|(_, tokens)| tokens),
        );

        let domain = policy.definitions();
        let copula = domain
            .id(COPULA)
            .expect("the shipped catalog declares the copula fragment");

        for tokens in &sentences {
            let matches = output.matcher.find_matches(tokens);
            for subject in matches
                .iter()
                .filter(|record| subjects.contains(&record.pattern.as_str()))
            {
                for evidence in &matches {
                    if definition_of(&evidence.pattern, domain, &policy.hidden) == Some(copula)
                        && evidence.end == subject.end
                        && evidence.start == subject.start
                    {
                        collisions.push(format!(
                            "{} in {name} at {}..{}",
                            subject.pattern, subject.start, subject.end
                        ));
                    }
                }
            }
        }
    }

    // A selector that matched nothing would make this test pass vacuously.
    assert_eq!(
        subjects_seen, 13,
        "the shipped catalog declares thirteen copula subjects"
    );
    assert!(
        collisions.is_empty(),
        "a DA record covers a subject whole, so `detach` keeps it: {collisions:?}"
    );
}
