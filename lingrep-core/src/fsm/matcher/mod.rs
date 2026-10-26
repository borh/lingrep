//! Immutable NFA matcher and its mutable pattern-definition builder.

pub(crate) mod assertion;
mod builder;
mod capture;
mod compiled;
mod contract;
mod dense;
mod emit;
mod evaluated;
mod exec;
mod lexicon;
mod pool;
/// Crate-visible so the result-policy engine can evaluate transform guards
/// through the matcher's own predicate semantics rather than a second
/// implementation of attribute comparison.
pub(crate) mod predicate;
mod program;
mod spawn;
#[cfg(feature = "bench-instrumentation")]
pub(crate) mod spawn_candidate_metrics;
pub(crate) mod spawn_dispatch;

// Re-export public API
pub use contract::{canonical_matches, canonicalize_matches};

#[cfg(feature = "bench-instrumentation")]
pub use assertion::AssertionMetrics;
pub use builder::FsmBuilder;
#[cfg(feature = "bench-instrumentation")]
pub use compiled::ExecutionMetrics;
pub use compiled::{
    AnalyzerCompatibility, AnchorEvaluator, CatalogAnalyzerConstraint, CompiledCatalog,
    CompiledLexiconDefinition,
};
pub(crate) use lexicon::CompiledLexicons;
pub use predicate::PredicateDigest;
#[cfg(feature = "bench-instrumentation")]
pub use spawn_candidate_metrics::{
    SpawnAtomOperator, SpawnCandidateAccumulator, SpawnCandidateCatalogMetrics,
    SpawnCandidateShapeEffect, SpawnPredicateShapeMetric, SpawnSafeAtomMetric,
};

// Re-export the pool-release hook for external use if needed
pub use pool::clear_pools;

/// A catalog declaring one anchor, whose `exclude` strips a trailing
/// punctuation run.
///
/// Shared by the `assertion` and `compiled` test modules, which vary it in
/// opposite directions: one guards the `exclude` with a lookaround, the other
/// adds one to the pattern.
///
/// A duplicate of the integration suite's `policy_helpers::PUNCT_ANCHOR`, and
/// deliberately: it cannot be imported from there, because
/// [`assertion::AssertionSession`] and [`assertion::AssertionProgramId`] are
/// `pub(crate)` and the tests using this therefore cannot live in the
/// integration suite. The `max_nesting:` claim is mandatory for declared
/// anchors, so the fixture must carry one.
/// The claim itself is executed by the suite's copy, in `anchor_claim_test`;
/// nothing here has a dictionary.
#[cfg(test)]
pub(crate) const PUNCT_ANCHOR: &str = concat!(
    "anchor: {name: \"propositional_end\", exclude: \"TRAILING_APPARATUS\"}\n",
    "max_nesting: {depth: 0, witness: [\"これは重要である|。\"], ",
    "divergence: [\"これは重要である|（田中）。\"]}\n",
    "fragment TRAILING_APPARATUS := [upos=PUNCT]+.\n\n",
    "pattern toiu := [text=という].\n\n",
    "policy\n",
    "  admit @toiu terminal: {exact: true, anchor: propositional_end}.\n",
    "end\n",
);

/// The compiled matcher for a test catalog expected to compile.
///
/// `name` is the source label a diagnostic quotes. It is a parameter rather
/// than a constant because this helper is shared: labelling every catalog
/// `anchor.lx` was honest while this was one module's inline fixture builder,
/// and would misattribute the moment a test here compiled anything else.
#[cfg(test)]
pub(crate) fn compile_for_test(name: &str, source: &str) -> CompiledCatalog {
    crate::fsm::compile_catalog(
        crate::fsm::PatternSource::named(name, source),
        crate::fsm::CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("fixture must compile: {:?}", error.diagnostics))
    .matcher
}

#[cfg(test)]
mod integration_tests {
    use super::FsmBuilder;
    use crate::fsm::PatternSource;

    #[test]
    fn test_fsm_basic_construction() {
        let mut fsm = FsmBuilder::new();
        let result = fsm.add_definition(PatternSource::named(
            "test.lx",
            "pattern test := [upos=NOUN].",
        ));
        assert!(result.is_ok());
    }
}
