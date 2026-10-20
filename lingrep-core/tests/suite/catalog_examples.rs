// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
//! Every executable claim in every tracked catalog must hold against the
//! catalog that carries it.
//!
//! Two claim shapes, and both are run here: `examples:`/`counterexamples:` on
//! an emitting pattern, and an anchor's `max_nesting:` on the `exclude`
//! fragment it names. They have separate entry points in `lingrep-core`
//! because a compound value on a fragment cannot join a string-list table, and
//! separate tests here for the same reason — but the same reason for existing:
//! a claim nothing runs is prose that decays.
//!
//! The check existed for `sf-experimental.lx` alone, as a test in that
//! catalog's own file. Seven documented examples in `double-wo.lx` were
//! therefore unverified prose: nothing would have noticed had a refactor
//! stopped them matching.
//!
//! The catalog set is discovered, not listed. A named list would recreate the
//! same gap one level up: the four catalogs that hold no examples today would
//! be absent from it, and the first `examples:` added to one of them would go
//! unverified while this file went on saying "every tracked catalog".

use lingrep_core::fsm::{
    ClaimReport, CompileLimits, CompiledCatalog, ExampleFailureKind, ExampleReport, PatternSource,
    compile_catalog, verify_anchor_claims, verify_examples,
};
use lingrep_core::testing::catalogs::tracked_catalog_sources;
use lingrep_core::testing::tokenize_japanese;

fn matcher_for(name: &str, text: &str) -> CompiledCatalog {
    compile_catalog(PatternSource::named(name, text), CompileLimits::default())
        .unwrap_or_else(|error| panic!("{name} must compile: {error}"))
        .matcher
}

fn report_for(name: &str, text: &str) -> ExampleReport {
    verify_examples(&matcher_for(name, text), &tokenize_japanese)
}

/// Every declared anchor's `max_nesting` claim, executed.
///
/// The sibling of [`report_for`], not an extension of it: `verify_examples`
/// reaches claims through `patterns()` and a fragment never appears there.
fn claims_for(name: &str, text: &str) -> ClaimReport {
    verify_anchor_claims(&matcher_for(name, text), &tokenize_japanese)
}

#[test]
fn every_tracked_catalog_example_matches_its_own_definition() {
    let mut failures = Vec::new();
    for (name, text) in tracked_catalog_sources() {
        for failure in report_for(&name, &text).failures {
            failures.push(format!(
                "{name}: {} ({:?}): {}",
                failure.definition, failure.kind, failure.example
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "documented claims the catalog contradicts:\n{}",
        failures.join("\n")
    );
}

#[test]
fn the_verified_example_counts_are_what_the_catalogs_contain() {
    // Guards the check against becoming vacuous. An empty failure list is the
    // same value whether every example passed or none were found, so a refactor
    // that stopped discovering them would look exactly like success. These
    // counts are measured, not aspirational.
    //
    // Every tracked catalog appears, including the four holding nothing
    // executable: a zero that is written down is a claim, and a catalog that
    // gains its first `examples:` breaks this test until someone states the
    // new number. Absence from the list would instead be silence.
    //
    // These count *entries*, not `examples:` keys: sf-experimental.lx has 86
    // keys holding 116 strings between them, and it is the strings that get
    // tokenized. Counting keys would report 86 examples verified while
    // silently executing 111, or — worse, after a future edit — while
    // executing fewer.
    //
    // patterns/sfe.lx contributes two examples and two counterexamples on
    // `必要がある`; placing them on the emitting pattern keeps all four
    // executable.
    let counts: Vec<(String, usize)> = tracked_catalog_sources()
        .into_iter()
        .map(|(name, text)| {
            let checked = report_for(&name, &text).checked;
            (name, checked)
        })
        .collect();

    let expected: Vec<(String, usize)> = [
        ("lingrep/tests/fixtures/lexicon-research-mix.lx", 0),
        ("lingrep/tests/fixtures/lexicon-semantic.lx", 0),
        ("lingrep/tests/fixtures/one.lx", 0),
        ("lingrep/tests/fixtures/pron-complement.lx", 0),
        ("lingrep/tests/fixtures/pron-exclusion.lx", 0),
        ("lingrep/tests/fixtures/sentence-final.lx", 0),
        (
            "lingrep-core/tests/fixtures/catalogs/apparatus-depth2.lx",
            0,
        ),
        ("patterns/alternation.lx", 22),
        ("patterns/collexeme.lx", 31),
        // English catalog: `verify_examples` tokenizes with
        // `tokenize_japanese`, so it declares no `examples:` at all.
        ("patterns/comment-register.lx", 0),
        ("patterns/concord.lx", 36),
        ("patterns/counting.lx", 9),
        ("patterns/distributions.lx", 0),
        ("patterns/double-wo-dep.lx", 0),
        ("patterns/double-wo.lx", 8),
        ("patterns/ngrams.lx", 0),
        ("patterns/npv-collocation.lx", 0),
        ("patterns/passive-agent-dep.lx", 0),
        ("patterns/passive-oblique-realization.lx", 0),
        ("patterns/portable-content-words.lx", 0),
        ("patterns/reduplication.lx", 8),
        ("patterns/sentence-final.lx", 0),
        ("patterns/sf-experimental.lx", 116),
        ("patterns/sfe.lx", 4),
        ("patterns/sketch-grammar.lx", 18),
        ("patterns/test-simple.lx", 0),
        ("patterns/ud-dependency.lx", 0),
        ("patterns/ud-enhanced-argument.lx", 0),
        ("patterns/ud-enhanced-consistency.lx", 0),
        ("patterns/ud-enhanced-propagation.lx", 0),
        ("patterns/ud-nonprojectivity.lx", 0),
        ("patterns/ud-object-realization.lx", 0),
        ("patterns/ud-subtree-profile.lx", 0),
        ("patterns/ud-valency.lx", 0),
        ("patterns/wo-predicate-dep.lx", 0),
        ("scripts/test/test_metadata.lx", 0),
        ("tests/fixtures/selective-patterns.lx", 0),
    ]
    .into_iter()
    .map(|(name, checked)| (name.to_owned(), checked))
    .collect();

    assert_eq!(counts, expected);
}

#[test]
fn a_malformed_examples_key_is_a_failure_not_a_skip() {
    // `examples: "text"` and `examples: [42]` are annotations nothing can
    // execute. Skipping them silently reproduces exactly the failure mode this
    // whole tier exists to close: an unverifiable claim that reads as verified.
    for source in [
        "examples: \"not-a-list\"\np := [unidic.pos1=代名詞].\n",
        "examples: [42]\np := [unidic.pos1=代名詞].\n",
    ] {
        let report = report_for("malformed.lx", source);

        assert_eq!(report.checked, 0, "{source}");
        assert_eq!(report.failures.len(), 1, "{source}");
        assert_eq!(report.failures[0].kind, ExampleFailureKind::Malformed);
    }
}

#[test]
fn a_counterexample_that_matches_is_reported() {
    // A classifier's risk is concentrated in what it excludes, and a positive
    // example cannot express exclusion at all.
    let report = report_for(
        "counterexample.lx",
        "counterexamples: [\"これ\"]\np := [unidic.pos1=代名詞].\n",
    );

    assert_eq!(report.checked, 1);
    assert_eq!(report.failures.len(), 1, "{report:?}");
    assert_eq!(report.failures[0].kind, ExampleFailureKind::UnexpectedMatch);
    assert_eq!(report.failures[0].example, "これ");
}

#[test]
fn a_counterexample_that_does_not_match_is_silent_but_still_counted() {
    // `checked` must include counterexamples. Otherwise a catalog whose risk
    // lives entirely in its exclusions reports zero claims checked while
    // verifying all of them, and the count stops meaning anything.
    let report = report_for(
        "counterexample.lx",
        "counterexamples: [\"これ\"]\np := [unidic.pos1=動詞].\n",
    );

    assert!(report.failures.is_empty(), "{report:?}");
    assert_eq!(report.checked, 1);
}

#[test]
fn both_keys_on_one_definition_are_verified_and_counted_together() {
    let report = report_for(
        "both.lx",
        "examples: [\"これ\"]\ncounterexamples: [\"走る\"]\np := [unidic.pos1=代名詞].\n",
    );

    assert!(report.failures.is_empty(), "{report:?}");
    assert_eq!(report.checked, 2);
}

/// Two catalogs declare an anchor, so this is no longer vacuous:
/// `patterns/sf-experimental.lx` at depth 0 and the depth-2 recognizer in
/// `docs/results/2026-08-04-anchor-nesting-claim/`.
/// `the_verified_nesting_claim_counts_are_what_the_catalogs_contain` is what
/// keeps a third from joining silently.
#[test]
fn every_tracked_catalog_nesting_claim_holds() {
    let mut failures = Vec::new();
    for (name, text) in tracked_catalog_sources() {
        for failure in claims_for(&name, &text).failures {
            failures.push(format!(
                "{name}: {} ({:?}): {} — {}",
                failure.anchor, failure.kind, failure.entry, failure.detail
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "nesting claims the catalog contradicts:\n{}",
        failures.join("\n")
    );
}

/// The same vacuity guard the example counts carry, for the same reason.
///
/// Every tracked catalog appears, including the ones declaring no anchor: a
/// zero that is written down is a claim, and the first catalog to declare an
/// anchor breaks this test until someone states the new number. Absence from
/// the list would instead be silence — and silence here reads exactly like a
/// claim that held.
///
/// The count is `witness` plus `divergence` entries, because it is entries that
/// get tokenized and executed. Counting anchors would report one claim verified
/// while executing any number of sentences, or none.
#[test]
fn the_verified_nesting_claim_counts_are_what_the_catalogs_contain() {
    let counts: Vec<(String, usize)> = tracked_catalog_sources()
        .into_iter()
        .map(|(name, text)| {
            let checked = claims_for(&name, &text).checked;
            (name, checked)
        })
        .collect();

    let expected: Vec<(String, usize)> = [
        ("lingrep/tests/fixtures/lexicon-research-mix.lx", 0),
        ("lingrep/tests/fixtures/lexicon-semantic.lx", 0),
        ("lingrep/tests/fixtures/one.lx", 0),
        ("lingrep/tests/fixtures/pron-complement.lx", 0),
        ("lingrep/tests/fixtures/pron-exclusion.lx", 0),
        ("lingrep/tests/fixtures/sentence-final.lx", 0),
        (
            "lingrep-core/tests/fixtures/catalogs/apparatus-depth2.lx",
            2,
        ),
        ("patterns/alternation.lx", 0),
        ("patterns/collexeme.lx", 0),
        ("patterns/comment-register.lx", 0),
        ("patterns/concord.lx", 0),
        ("patterns/counting.lx", 0),
        ("patterns/distributions.lx", 0),
        ("patterns/double-wo-dep.lx", 0),
        ("patterns/double-wo.lx", 0),
        ("patterns/ngrams.lx", 0),
        ("patterns/npv-collocation.lx", 0),
        ("patterns/passive-agent-dep.lx", 0),
        ("patterns/passive-oblique-realization.lx", 0),
        ("patterns/portable-content-words.lx", 0),
        ("patterns/reduplication.lx", 0),
        ("patterns/sentence-final.lx", 2),
        ("patterns/sf-experimental.lx", 2),
        ("patterns/sfe.lx", 0),
        ("patterns/sketch-grammar.lx", 0),
        ("patterns/test-simple.lx", 0),
        ("patterns/ud-dependency.lx", 0),
        ("patterns/ud-enhanced-argument.lx", 0),
        ("patterns/ud-enhanced-consistency.lx", 0),
        ("patterns/ud-enhanced-propagation.lx", 0),
        ("patterns/ud-nonprojectivity.lx", 0),
        ("patterns/ud-object-realization.lx", 0),
        ("patterns/ud-subtree-profile.lx", 0),
        ("patterns/ud-valency.lx", 0),
        ("patterns/wo-predicate-dep.lx", 0),
        ("scripts/test/test_metadata.lx", 0),
        ("tests/fixtures/selective-patterns.lx", 0),
    ]
    .into_iter()
    .map(|(name, checked)| (name.to_owned(), checked))
    .collect();

    assert_eq!(counts, expected);
}
