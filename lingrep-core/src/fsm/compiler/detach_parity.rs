//! The premise that made `detach` a safe replacement for `trim_suffix:`.
//!
//! Both removed a `DA` region ending at the record's end. The retired
//! `apply_trim` selected edges with `edge.start >= record.start` and then
//! clamped — `record.end = new_end.max(record.start + 1)` — so an edge covering
//! the whole record left one token. `decide_detach` selects with
//! `operand.start > subject.start` and does not clamp, because
//! `docs/design/2026-08-06-policy-sublanguage-syntax.md` §3 replaced the clamp
//! with a strict bound: *a region that is the entire record is not a trailing
//! region*. Nothing else about the two differed.
//!
//! So they disagreed exactly when a `DA` record starts where a subject record
//! starts. `docs/results/2026-08-02-r1-sf-experimental-migration.md:95` argues
//! that cannot happen here, because every one of the thirteen copula
//! definitions opens on a non-copula head token (もの, 可能, はず, ため, わけ,
//! よう, こと, と). This makes that argument a checked fact.
//!
//! **Still live after the retirement**, because it never was a statement about
//! the old mechanism. It says the catalog's thirteen subjects never meet
//! `detach`'s strict bound — the condition under which the bound would discard
//! a record the researcher expects to see. Deleting it with `apply_trim` would
//! retire the proof along with the thing it justified.
//!
//! In `src` rather than `tests/` because it calls the private
//! [`definition_of`](crate::fsm::policy_engine): an evidence predicate the test
//! reimplemented would agree with itself rather than with the engine, and the
//! whole point is to ask the engine's question. That question was
//! `policy::is_evidence_for` until the last key-driven pass retired; the
//! program resolves a record's name through the `__policy__` alias table
//! instead, and this follows it rather than keeping the old predicate alive for
//! one test.
//!
//! **What this is not.** It is not the parity proof.
//! [`published_baseline`](super::published_baseline) is — this cannot see an
//! error in group selection, evidence mode, capture handling, or statement
//! order. It exists so that when the baseline moves, the failure names the
//! definition instead of showing a snapshot diff.

#![cfg(test)]

use super::compile_catalog;
use super::limits::CompileLimits;
use crate::fsm::PatternSource;
use crate::fsm::policy_engine::definition_of;
use crate::testing::catalogs::tracked_catalog_sources;
use crate::testing::tokenize_japanese;

/// The fragment whose records both mechanisms remove.
const COPULA: &str = "DA";

/// The group the migration puts the thirteen copula definitions in.
const MIGRATED_GROUP: &str = "trailing_copula";

#[test]
fn no_copula_evidence_record_starts_where_its_subject_starts() {
    let mut collisions: Vec<String> = Vec::new();
    let mut subjects_seen = 0usize;

    for (name, text) in tracked_catalog_sources() {
        let output = compile_catalog(PatternSource::named(&name, &text), CompileLimits::default())
            .unwrap_or_else(|error| panic!("{name} must compile: {error}"));
        let policy = output.matcher.catalog_policy();

        // Accepted either spelling until the migration landed; `trim_suffix:`
        // is retired now, so membership is the only selector left. The
        // `subjects_seen` count below is what keeps that reduction honest --
        // it was 13 under the old selector and must still be 13 under this one.
        let subjects: Vec<&str> = output
            .matcher
            .patterns()
            .keys()
            .map(compact_str::CompactString::as_str)
            .filter(|definition| {
                policy
                    .get(definition)
                    .is_some_and(|entry| entry.groups.iter().any(|group| group == MIGRATED_GROUP))
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

    // A selector that matched nothing would make this vacuously green, and the
    // migration would lose its only localised diagnostic without anything
    // saying so.
    assert_eq!(
        subjects_seen, 13,
        "the shipped catalog declares thirteen copula subjects"
    );
    assert!(
        collisions.is_empty(),
        "a DA record covers a subject whole, so `detach`'s strict bound and \
         `trim_suffix:`'s clamp disagree here: {collisions:?}"
    );
}
