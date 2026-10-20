// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic)]
//! Helpers shared across the result-policy test modules.
//!
//! These live in their own module rather than in one of the test files that
//! uses them, so that an importing module gets an honest path: a token builder
//! reached as `policy_compile_test::tokens_with_pos` would be claiming a
//! provenance it does not have. They are deliberately *not* in
//! `tests/support/`, because they are policy-suite helpers rather than shared
//! integration-test infrastructure.
//!
//! `compile` is not duplicated here; it stays at its existing home in
//! `policy_compile_test` and is imported.

use crate::policy_compile_test::compile;
use compact_str::CompactString;
use lingrep_core::fsm::{
    AnchorPositions, AnchorSetDigest, CatalogPolicy, ClaimReport, CompiledCatalog, Diagnostic,
    DiagnosticSeverity, Match, Terminal, verify_anchor_claims,
};
use lingrep_core::testing::tokenize_japanese;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};
use std::collections::BTreeMap;

/// The compiled matcher for a catalog expected to succeed.
///
/// Named distinctly from `policy_compile_test::compile`, which returns
/// `Result<CompileOutput, CompileError>` and is the fallible entry point the
/// diagnostic helpers below build on.
///
/// This is also how a catalog expected to *compile* is asserted: the `expect`
/// carries the diagnostics on failure. [`compile_errors`] is for the opposite
/// expectation and panics on success.
pub(crate) fn matcher_for(source: &str) -> CompiledCatalog {
    compile(source)
        .expect("catalog under test must compile")
        .matcher
}

/// The declared policy of a catalog expected to compile.
///
/// A `catalog_policy_for(fixture)` and a `matcher_for(fixture)` are two
/// independent compilations of the same text:
/// an `AnchorPositions` derived from one is checked against an
/// `AnchorSetDigest` carried by the other, and a mismatch is a hard error.
/// Pairing them is nonetheless safe, because the digest is *structural* —
/// computed from the bound program rather than from a per-compilation identity
/// — so structurally identical catalogs agree whichever compilation produced
/// them. `structurally_identical_catalogs_share_a_digest` pins that property.
pub(crate) fn catalog_policy_for(source: &str) -> CatalogPolicy {
    matcher_for(source).catalog_policy().clone()
}

/// Diagnostics from a catalog expected to fail.
///
/// Returns the diagnostics themselves, not their text: a diagnostic's message
/// is presentation, and callers must assert on [`Diagnostic::code`]. Where
/// several refusals share one code, a `message.contains(..)` check naming the
/// specific offending field is a legitimate *secondary* assertion.
///
/// Panicking on unexpected success is what keeps `!compile_errors(..).is_empty()`
/// honest. Returning an empty vector instead would make "refused, but reported
/// nothing" and "compiled fine" the same observation, so a catalog that silently
/// started compiling would keep passing the test written to refuse it.
pub(crate) fn compile_errors(source: &str) -> Vec<Diagnostic> {
    match compile(source) {
        Ok(_) => panic!("expected compilation to fail: {source}"),
        Err(error) => error.diagnostics,
    }
}

/// Warning-severity diagnostics from a catalog expected to compile.
///
/// `CompileOutput` carries a single severity-tagged `diagnostics` list rather
/// than a separate `warnings` list, so the severity filter lives here.
pub(crate) fn compile_warnings(source: &str) -> Vec<Diagnostic> {
    compile(source)
        .expect("catalog under test must compile")
        .diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.severity() == DiagnosticSeverity::Warning)
        .collect()
}

/// Tokens carrying only `pos`.
pub(crate) fn tokens_with_pos(tags: &[&str]) -> Vec<TokenInfo> {
    tags.iter()
        .map(|tag| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Pos, (*tag).into());
            token
        })
        .collect()
}

/// Tokens carrying only `orth`.
///
/// The program tests select by `orth` alone, so a `pos` they never name would
/// be a value the fixture asserts nothing about and a reader has to discount.
pub(crate) fn tokens_with_orth(words: &[&str]) -> Vec<TokenInfo> {
    words
        .iter()
        .map(|orth| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Orth, (*orth).into());
            token
        })
        .collect()
}

/// Tokens carrying `orth` and `pos`, in that order.
///
/// Both fields are `&str`, so an inverted pair still compiles and still runs —
/// the name states the order because nothing else can.
pub(crate) fn tokens_with_orth_and_pos(pairs: &[(&str, &str)]) -> Vec<TokenInfo> {
    pairs
        .iter()
        .map(|(orth, pos)| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Orth, (*orth).into());
            token.set(TokenAttributeIndex::Pos, (*pos).into());
            token
        })
        .collect()
}

/// A catalog whose anchor excludes `[upos=PUNCT]+`, with a depth-0 claim.
///
/// The claim is true, executed, and required: `anchor_claim_test` runs it
/// against a dictionary. The claim is mandatory, so an `exclude`
/// carrying none does not compile. Fixtures derived by `replace` therefore
/// inherit a claim that holds — check it still does when varying the
/// `exclude`.
///
/// Every metadata value is single-line YAML flow style, because the grammar's
/// `metadata_line` is one line: a block sequence under `anchor:` parses as three
/// separate `anchor:`-less lines and is refused as a duplicate metadata key.
pub(crate) const PUNCT_ANCHOR: &str = r#"
anchor: {name: "propositional_end", exclude: "TRAILING_APPARATUS"}
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment TRAILING_APPARATUS := [upos=PUNCT]+.

pattern toiu := [text=という].

policy
  admit @toiu terminal: {exact: true, anchor: propositional_end}.
end
"#;

/// The `admit` mapping in [`PUNCT_ANCHOR`], for fixtures that vary it.
pub(crate) const PUNCT_ANCHOR_TERMINAL: &str = "{exact: true, anchor: propositional_end}";

pub(crate) fn punct_anchor_matcher() -> CompiledCatalog {
    matcher_for(PUNCT_ANCHOR)
}

/// Structurally different from [`PUNCT_ANCHOR`], for the digest-mismatch tests.
pub(crate) fn sym_anchor_matcher() -> CompiledCatalog {
    matcher_for(&PUNCT_ANCHOR.replace("pos=PUNCT", "pos=SYM"))
}

/// [`PUNCT_ANCHOR`] with its `admit` mapping replaced.
///
/// A source-level edit rather than a mutation of the bound program, which has
/// no public setter. Recompiling exercises the path an authored catalog takes.
pub(crate) fn punct_anchor_with_terminal(value: &str) -> String {
    // Asserts the mapping is *present*, not that the result differs: replacing
    // it with itself is a legitimate case (`punct_anchor_with_distance` for
    // `Exact` does exactly that). What this guards is the constant drifting out
    // of sync with the fixture, which would silently make every caller compile
    // the unedited catalog.
    assert!(
        PUNCT_ANCHOR.contains(PUNCT_ANCHOR_TERMINAL),
        "the fixture no longer carries the mapping this replaces"
    );
    PUNCT_ANCHOR.replace(PUNCT_ANCHOR_TERMINAL, value)
}

/// [`punct_anchor_with_terminal`] for a distance, keeping the declared anchor.
pub(crate) fn punct_anchor_with_distance(distance: Terminal) -> String {
    let spelling = match distance {
        Terminal::Exact => "exact: true".to_owned(),
        Terminal::Within(count) => format!("within: {count}"),
    };
    punct_anchor_with_terminal(&format!("{{{spelling}, anchor: propositional_end}}"))
}

/// Hand-built anchor positions carrying [`PUNCT_ANCHOR`]'s digest.
///
/// For engine tests that assert admission against a stated position without
/// running a recognizer over a sentence contrived to produce it.
///
/// The digest is taken from a real compilation rather than fabricated, because
/// `AnchorPositions::new_for_test` has no other way to obtain one — a test can
/// choose the positions, never the anchor set. That compilation is a *second*
/// one beside whatever `matcher_for`/`catalog_policy_for` the test also builds;
/// see [`catalog_policy_for`] for why pairing them is safe.
pub(crate) fn positions(entries: &[(&str, usize)]) -> AnchorPositions {
    AnchorPositions::new_for_test(
        *punct_anchor_matcher().anchor_digest(),
        entries
            .iter()
            .map(|(name, index)| (CompactString::new(*name), *index))
            .collect(),
    )
}

/// The positions of a catalog that declares no anchor.
///
/// What every engine test over a hand-built [`CatalogPolicy`] passes. Such a
/// policy carries `AnchorSetDigest::default()`, which is exactly the digest of
/// a catalog declaring nothing, so this is the set its own compilation would
/// have produced rather than a stand-in.
///
/// It is not a way around the mandatory parameter. A `terminal:` that named an
/// anchor would be refused here with [`lingrep_core::fsm::AnchorError::Unknown`],
/// which is the point: the empty set answers nothing, it does not fall back to
/// the raw sentence end.
pub(crate) fn no_anchors() -> AnchorPositions {
    AnchorPositions::new_for_test(AnchorSetDigest::default(), BTreeMap::new())
}

/// Every declared anchor's `max_nesting` claim, executed.
///
/// Dictionary-backed and deliberately not `#[ignore]`d: `nix develop` sets
/// `LINGREP_DICTIONARY_PATH`, so this runs in CI exactly as the suite's other
/// `tokenize_japanese` tests do. A claim states where a bounded recognizer
/// lands and how deep its sentences nest, and neither question has an answer
/// without tokens.
pub(crate) fn verify_claims(source: &str) -> ClaimReport {
    verify_anchor_claims(&matcher_for(source), &tokenize_japanese)
}

/// The `toiu` emitting pattern with no anchor declared.
///
/// The counterpart to the anchored fixtures: the same emitting pattern in a
/// catalog with no anchor, so that the
/// canonical match stream can be shown to be invariant under anchor
/// declaration.
pub(crate) fn unanchored_matcher() -> CompiledCatalog {
    matcher_for("pattern toiu := [text=という].\n")
}

/// One bare `Match` over `[start, end)`, for tests about the policy engine's
/// ordering rather than about recognition.
pub(crate) fn record(pattern: &str, start: usize, end: usize) -> Match {
    Match {
        pattern: CompactString::new(pattern),
        start,
        end,
        captures: Box::default(),
        graph_bindings: Box::default(),
        lexical: None,
    }
}

/// What `source` publishes over `words`, after policy.
pub(crate) fn published_records(source: &str, words: &[&str]) -> Vec<Match> {
    matcher_for(source)
        .find_published_matches(&tokens_with_orth(words))
        .expect("the fixture declares no anchor, so nothing can fail to resolve")
}
