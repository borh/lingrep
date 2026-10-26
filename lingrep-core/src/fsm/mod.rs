//! lingrep patterns matching with an explicit build phase and capability dispatch.
//!
//! [`FsmBuilder`] accepts named lingrep patterns definitions and produces an immutable
//! [`CompiledCatalog`]. Plain recognition uses the NFA; observable captures and
//! bounded span equality add sentence-local execution plans.

mod binding;
pub mod compiler;
pub mod examples;
pub mod explanation;
mod identity;
pub mod inline;
pub mod matcher;
pub mod policy;
/// Private: its only public surface is [`ProgramStats`] and
/// [`OperationCounters`], re-exported below. `Candidate`, `Decision` and
/// `ExecContext` are the engine's own vocabulary, and a caller holding one
/// could assemble a state the seam never produces.
mod policy_engine;
pub mod policy_program;
/// The per-record explanation trace: opt-in, capacity-bounded, and public
/// because a caller that asked for one has to be able to read it.
pub mod policy_trace;
mod sentence_matcher;
mod state_machine;
pub mod types;

// Matcher, builder, scheduler, and engine tests live beside the APIs they
// exercise, not in a test module here.

pub use binding::{BoundCatalog, BoundExecutionError, SchemaBindError};
pub use compiler::{
    AttributeReferenceSite, CatalogAnalysis, CatalogDriftReport, CompileError, CompileLimits,
    CompileOutput, CompilerStats, DefinitionExpansion, Diagnostic, DiagnosticCode,
    DiagnosticLocation, DiagnosticSeverity, DriftBody, DriftCarrier, DriftDefinitionKind,
    DriftEntry, ExclusionSite, ExpandError, ExpandedDefinitionKind, FormatError, FormatOutcome,
    LexiconProjectionSite, PatternSource, RelatedDiagnosticLocation, SentenceFilter,
    SentenceFilterTerm, compile_catalog, expand_pattern_definition, format_pattern_source,
    render_definition_diagram, render_diagnostics, render_pattern_error, report_catalog_drift,
};
pub use examples::{
    ClaimFailure, ClaimFailureKind, ClaimReport, ExampleFailure, ExampleFailureKind, ExampleReport,
    verify_anchor_claims, verify_examples,
};
pub use explanation::{ExplainedMatch, SentenceExplanation};
pub use identity::{
    COMPILER_SCHEMA_IDENTITY, GRAMMAR_SEMANTICS_REVISION, POLICY_SEMANTICS_IDENTITY,
};
pub use inline::normalize_inline_pattern;
pub use matcher::{
    AnalyzerCompatibility, AnchorEvaluator, CatalogAnalyzerConstraint, CompiledCatalog,
    CompiledLexiconDefinition, FsmBuilder, PredicateDigest, clear_pools,
};
#[cfg(feature = "bench-instrumentation")]
pub use matcher::{
    AssertionMetrics, ExecutionMetrics, SpawnAtomOperator, SpawnCandidateAccumulator,
    SpawnCandidateCatalogMetrics, SpawnCandidateShapeEffect, SpawnPredicateShapeMetric,
    SpawnSafeAtomMetric,
};
pub use policy::{
    AnchorError, AnchorPositions, AnchorResolutionError, AnchorSetDigest, CatalogPolicy,
    CatalogPolicyBuilder, CompiledGuard, KeyComponent, KeyProjection, KeyTarget,
    LexicalProjectedKeyField, LexicalProjectedKeyValue, NestingClaim, NestingEntry, PatternPolicy,
    PolicyStats, ProjectedKeyField, PublishedRecord, RENDERER_REGISTRY, RecordView,
    SubstituteTarget, TagSource, Terminal, TransformOp, apply_policy, apply_policy_records,
    project_key, strip_hidden,
};
pub use policy_engine::{OperationCounters, ProgramStats};
pub use policy_trace::{PolicyTrace, TracedOutcome, TracedRecord};
pub use sentence_matcher::{BoundSentenceMatcher, SentenceMatcher};
pub use types::*;
