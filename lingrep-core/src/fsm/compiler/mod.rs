//! Atomic typed lingrep patterns compilation to capability-classified execution plans.

mod analysis;
mod analyze;
mod assertion;
mod bind;
mod capture_key;
#[cfg(test)]
mod detach_parity;
mod diagnostic;
mod drift;
mod expand;
mod format;
mod guard;
mod limits;
mod lower;
mod materialize;
mod metadata_baseline;
pub(crate) mod model;
mod nfa;
#[cfg(test)]
mod oracle;
mod policy;
mod policy_program;
mod published_baseline;
mod render;
mod schema;
mod sentence_filter;
pub(crate) mod source_text;
mod token_test;

pub(crate) use sentence_filter::derive_sentence_filter;
pub use sentence_filter::{SentenceFilter, SentenceFilterTerm};

pub use analysis::{AttributeReferenceSite, CatalogAnalysis, ExclusionSite, LexiconProjectionSite};
pub use diagnostic::{
    CompileError, Diagnostic, DiagnosticCode, DiagnosticLocation, DiagnosticSeverity,
    RelatedDiagnosticLocation,
};
pub use drift::{
    CatalogDriftReport, DriftBody, DriftCarrier, DriftDefinitionKind, DriftEntry,
    report_catalog_drift,
};
pub use expand::{
    DefinitionExpansion, ExpandError, ExpandedDefinitionKind, expand_pattern_definition,
    render_definition_diagram,
};
pub use format::{FormatError, FormatOutcome, format_pattern_source};
pub use limits::{CompileLimitError, CompileLimits, CompilerStats, TightenedCompileLimits};
pub use render::{render_diagnostics, render_pattern_error};

use crate::fsm::AttrValueType;
use compact_str::CompactString;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use analyze::analyze_program;
use assertion::compile_assertions;
use bind::bind;
use lower::{lower_definition_with_stats, lower_document_with_stats};
use materialize::materialize_program;
use model::{
    BoundDefinition, BoundDefinitionBody, BoundExpr, BoundProgram, DefinitionId,
    DefinitionVisibility, ParsedAttr, ParsedAttrValue, ParsedDefinition, ParsedExpr,
    ParsedGroupDecl, ParsedProgram, ParsedSet, ParsedTest, ParsedTestDecl, SourceId,
    SourceRegistry,
};
use nfa::lower_to_nfa;

use crate::fsm::CompiledLexiconDefinition;
use crate::fsm::matcher::CompiledLexicons;
use crate::fsm::{CatalogPolicy, CompiledCatalog};

/// lingrep patterns text supplied by an application.
///
/// The compiler never performs file I/O; `name` is only a stable diagnostic
/// label supplied by the caller.
#[derive(Clone, Copy, Debug)]
pub struct PatternSource<'a> {
    pub(crate) name: &'a str,
    pub(crate) text: &'a str,
}

impl<'a> PatternSource<'a> {
    /// Associates lingrep patterns text with a caller-defined UTF-8 diagnostic label.
    ///
    /// The source-byte budget in [`CompileLimits`] charges both `name` and
    /// `text` before the compiler takes ownership of the label.
    pub fn named(name: &'a str, text: &'a str) -> Self {
        Self { name, text }
    }
}

/// A complete immutable matcher, non-fatal diagnostics, and compile metrics.
#[derive(Clone)]
pub struct CompileOutput {
    pub matcher: CompiledCatalog,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: CompilerStats,
    /// The catalog as authored: absence-sensitive exclusions with provenance.
    /// Not part of `COMPILER_SCHEMA_IDENTITY` — it describes source, not
    /// compiled semantics, so moving it invalidates nothing.
    pub analysis: CatalogAnalysis,
}

/// Compiles one complete lingrep patterns document into an immutable NFA matcher.
///
/// Parsing, strict lowering, binding, budget checks, and NFA construction are
/// local to this call. Any error discards all intermediate state.
pub fn compile_catalog(
    source: PatternSource<'_>,
    limits: CompileLimits,
) -> Result<CompileOutput, CompileError> {
    let lowered = lower_document_with_stats(&source, &limits)?;
    let source_bytes = lowered.source_bytes;
    let program = lowered.program;
    let mut stats = measure_program(&program, source_bytes, lowered.metadata_bytes)?;
    compile_program(program, &limits, &mut stats, lowered.warnings)
}

/// Parsed typed definitions accumulated by [`crate::fsm::FsmBuilder`].
pub(crate) struct DefinitionAccumulator {
    definitions: Vec<ParsedDefinition>,
    /// Every `set` declaration contributed so far, in registration order.
    ///
    /// Beside `definitions` rather than inside it because a set is not a
    /// definition: it names literals, emits nothing, and is legal only after
    /// `in`. It accumulates the same way for the same reason — the scope a
    /// reference resolves against is the catalog, not the document.
    sets: Vec<ParsedSet>,
    /// Every `test` declaration contributed so far, in registration order.
    ///
    /// Beside `sets` and for the identical reason: a test is a value, not a
    /// definition, and the scope a `[@X]` resolves against is the catalog.
    tests: Vec<ParsedTestDecl>,
    /// Every `group` declaration contributed so far, in registration order.
    ///
    /// Beside `sets` and `tests` and for the same scoping reason: derived
    /// membership is a predicate over the accumulated catalog's definitions,
    /// so a declaration in one document selects patterns from another.
    groups: Vec<ParsedGroupDecl>,
    analyzers: Vec<model::ParsedAnalyzerConstraint>,
    sources: SourceRegistry,
    stats: CompilerStats,
    /// Every non-fatal lowering diagnostic contributed so far.
    ///
    /// Accumulated like the definitions and for the same reason: the catalog is
    /// the accumulation, and a warning about the third registered document has
    /// to survive to the one `build` that reports. Each already holds its own
    /// document's label, line and column, so nothing here is remapped.
    lowering_warnings: Vec<Diagnostic>,
    /// Every `policy` block contributed so far, with the source it arrived
    /// from.
    ///
    /// A `Vec` rather than an `Option` because the conflict is reported at
    /// [`Self::build`], and a diagnostic naming both locations needs both. One
    /// per **accumulated catalog**, not per file — which is why "one block per
    /// file" does not state the invariant: the catalog is the accumulation.
    ///
    /// Operations never concatenate across blocks. Rejecting the second is
    /// what makes that moot, and that is the point: registration order is a
    /// property of the caller, while operation order is semantic.
    policies: Vec<(crate::fsm::policy_program::Program, SourceId)>,
}

impl DefinitionAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            definitions: Vec::new(),
            sets: Vec::new(),
            tests: Vec::new(),
            groups: Vec::new(),
            analyzers: Vec::new(),
            sources: SourceRegistry::default(),
            stats: CompilerStats::default(),
            lowering_warnings: Vec::new(),
            policies: Vec::new(),
        }
    }

    pub(crate) fn add(
        &mut self,
        source: PatternSource<'_>,
        limits: &CompileLimits,
    ) -> Result<(), CompileError> {
        let lowered = lower_definition_with_stats(&source, limits)?;
        let mut definition = lowered.definition;
        let source_label = lowered.sources.source_label(SourceId::new(0));
        let single = ParsedProgram {
            definitions: vec![definition.clone()],
            sources: lowered.sources.clone(),
            // `add` registers one definition; catalog declarations arrive
            // through `add_document`.
            policy: None,
            analyzers: Vec::new(),
            sets: Vec::new(),
            tests: Vec::new(),
            groups: Vec::new(),
        };
        let increment = measure_program(&single, lowered.source_bytes, lowered.metadata_bytes)?;
        let next_stats = checked_stats_add(self.stats, increment, &source_label)?;
        enforce_accumulated_limits(next_stats, limits, &source_label)?;

        let source_id = self.sources.append_single(lowered.sources).map_err(|_| {
            limit_error(
                Arc::clone(&source_label),
                "lingrep patterns source identity exceeds the supported range",
            )
        })?;
        remap_definition_source(&mut definition, source_id);

        self.definitions.push(definition);
        self.lowering_warnings.extend(lowered.warnings);
        self.stats = next_stats;
        Ok(())
    }

    pub(crate) fn add_document(
        &mut self,
        source: PatternSource<'_>,
        limits: &CompileLimits,
    ) -> Result<(), CompileError> {
        let lowered = lower_document_with_stats(&source, limits)?;
        let mut program = lowered.program;
        let source_label = program.sources.source_label(SourceId::new(0));
        let increment = measure_program(&program, lowered.source_bytes, lowered.metadata_bytes)?;
        let next_stats = checked_stats_add(self.stats, increment, &source_label)?;
        enforce_accumulated_limits(next_stats, limits, &source_label)?;

        let source_id = self.sources.append_single(program.sources).map_err(|_| {
            limit_error(
                Arc::clone(&source_label),
                "lingrep patterns source identity exceeds the supported range",
            )
        })?;
        for definition in &mut program.definitions {
            remap_definition_source(definition, source_id);
        }
        for set in &mut program.sets {
            remap_set_source(set, source_id);
        }
        for test in &mut program.tests {
            remap_test_source(test, source_id);
        }
        for group in &mut program.groups {
            remap_group_source(group, source_id);
        }
        if let Some(mut declared) = program.policy.take() {
            // Spans were built against this document's own `SourceId`. Remap
            // them exactly as definitions are, or a diagnostic names whichever
            // file happens to sit at that id in the accumulated registry.
            remap_program_source(&mut declared, source_id);
            self.policies.push((declared, source_id));
        }
        self.definitions.extend(program.definitions);
        self.sets.extend(program.sets);
        self.tests.extend(program.tests);
        self.groups.extend(program.groups);
        self.analyzers.extend(program.analyzers);
        self.lowering_warnings.extend(lowered.warnings);
        self.stats = next_stats;
        Ok(())
    }

    pub(crate) fn build(self, limits: &CompileLimits) -> Result<CompileOutput, CompileError> {
        let mut stats = self.stats;
        let policy = match self.policies.as_slice() {
            [] => None,
            [(declared, _)] => Some(declared.clone()),
            // Reported against the later and naming the earlier, which is the
            // provenance convention's fourth row: the author fixes the block
            // they just added, and needs to be told where the other one is.
            [(_, first), .., (_, second)] => {
                return Err(second_policy_block(&self.sources, *first, *second));
            }
        };
        compile_program(
            ParsedProgram {
                definitions: self.definitions,
                sources: self.sources,
                policy,
                analyzers: self.analyzers,
                sets: self.sets,
                tests: self.tests,
                groups: self.groups,
            },
            limits,
            &mut stats,
            self.lowering_warnings,
        )
    }
}

fn compile_program(
    mut program: ParsedProgram,
    limits: &CompileLimits,
    stats: &mut CompilerStats,
    lowering_warnings: Vec<Diagnostic>,
) -> Result<CompileOutput, CompileError> {
    let analyzer_constraints = program
        .analyzers
        .iter()
        .map(|constraint| {
            crate::fsm::CatalogAnalyzerConstraint::new(
                constraint.family.clone(),
                constraint.resource.clone(),
                constraint.split.clone(),
                constraint.segmentation.clone(),
            )
        })
        .collect();
    // Result policy is read before binding: every input it needs is already in
    // the parsed program, and its errors are about what the catalog *declares*
    // rather than about what its bodies resolve to. Reporting them first means
    // a malformed `policy` statement does not have to wait behind an unrelated
    // body error to be seen.
    let mut policy_errors = policy::reserved_name_errors(&program);
    policy_errors.extend(policy::lexicon_definition_policy_errors(&program));
    // Read alongside policy and for the same reason: it is a statement about
    // what the catalog *declares*, so it should not queue behind an unrelated
    // body error.
    let (declared_dictionaries, dictionary_diagnostics) = policy::extract_dictionary(&program);
    policy_errors.extend(dictionary_diagnostics);
    let (mut catalog_policy, authored_groups, policy_diagnostics) =
        policy::extract_policy(&program);
    let mut policy_warnings = Vec::new();
    for diagnostic in policy_diagnostics {
        if diagnostic.severity() == DiagnosticSeverity::Error {
            policy_errors.push(diagnostic);
        } else {
            policy_warnings.push(diagnostic);
        }
    }
    if !policy_errors.is_empty() {
        return Err(CompileError::from_diagnostics(policy_errors));
    }
    // Minted before binding so the synthesized emitters are bound, materialized
    // and lowered with everything else. A catalog declaring no
    // `policy` block mints nothing and is unaffected.
    // Takes `authored_groups` because a `%group` operand must be expanded to
    // decide which fragments need an emitter, and the `GroupIndex` that would
    // normally do that cannot exist yet: it is built over the definition list
    // this call appends to.
    policy::synthesize_hidden_emitters(&mut program, &mut catalog_policy, &authored_groups);

    // After synthesis, never before: `DefId` indexes the complete definition
    // list and synthesis appends to it. Written exactly once.
    policy::install_group_index(&program, &mut catalog_policy, authored_groups);
    policy::install_definition_domain(&program, &mut catalog_policy);

    // After the index, because `%name` resolves through it. Both `DefId` and
    // `GroupId` are therefore assigned over the post-synthesis catalog.
    policy_warnings.extend(
        policy::bind_policy_program(&program, &mut catalog_policy)
            .map_err(CompileError::from_diagnostics)?,
    );

    let bound = bind(program, limits)?;
    let compiled_lexicon_definitions = bound
        .definitions
        .iter()
        .filter_map(|definition| {
            let BoundDefinitionBody::Lexicon { by, members } = &definition.body else {
                return None;
            };
            Some(CompiledLexiconDefinition {
                name: definition.name.clone(),
                by: by.clone(),
                members: members.clone(),
                metadata: definition
                    .metadata
                    .iter()
                    .map(|(name, entry)| (name.clone(), entry.value.clone()))
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    let compiled_lexicons = CompiledLexicons::compile(compiled_lexicon_definitions)
        .map_err(|error| limit_error("<pattern>", &error.0))?;
    let lexicon_owned_bytes = compiled_lexicons
        .as_ref()
        .map_or(0, CompiledLexicons::owned_bytes);
    // After binding, because a definition's width is a property of its resolved
    // body: until then the reference inside it is a name. This is the one
    // policy-program rule that cannot live with the others in
    // `bind_policy_program`, which runs before the expressions exist.
    policy::check_detach_widths(&catalog_policy, &bound).map_err(CompileError::from_diagnostics)?;
    // After binding, because whether an `exclude` fragment is nullable or names
    // a capture is a property of its resolved body. The digest is installed here
    // for the same reason: it hashes the transitive expansion, which only the
    // bound program has.
    // Resolved once, here, and carried: the digest and the assertion compiler
    // both need each `exclude`'s body, and taking a `DefinitionId` is what stops
    // them looking it up by name again.
    let anchors =
        policy::resolve_anchors(&catalog_policy, &bound).map_err(CompileError::from_diagnostics)?;
    catalog_policy.anchor_digest = policy::anchor_set_digest(&anchors, &bound);
    // After `resolve_anchors` and not merged into it: an `exclude` that cannot
    // work at all — nullable, or naming a capture — is the diagnostic its author
    // needs, and being told to document the depth of a recognizer that will
    // never be built is advice about the wrong mistake. Nothing here reads what
    // binding produced; the placement buys the ordering, not information.
    catalog_policy.nesting_claims = policy::read_nesting_claims(&catalog_policy, &bound)
        .map_err(CompileError::from_diagnostics)?;

    // Computed before the policy is installed, which moves it. Reachability is a
    // question about the bound program and the declarations over it, so nothing
    // downstream of here can change the answer.
    let (mut diagnostics, evaluated) = unused_fragment_warnings(&bound, &catalog_policy);
    let analysis = analysis::analyze(&bound, &evaluated);

    let match_projection_errors =
        capture_key::match_projection_errors(&catalog_policy, &bound.definitions, &bound.sources);
    if !match_projection_errors.is_empty() {
        return Err(CompileError::from_diagnostics(match_projection_errors));
    }

    let assertions = compile_assertions(&bound, limits, &anchors)?;
    let nfa_limits = limits
        .tightened(|remaining| {
            remaining.max_expansion_work -= assertions.cost.expansion_work;
            remaining.max_generated_owned_bytes -= assertions.cost.generated_owned_bytes;
            remaining.max_nfa_states -= assertions.cost.states;
            remaining.max_transitions -= assertions.cost.transitions;
        })
        .expect("assertion costs were checked against the active compile limits");
    let mut execution = materialize_program(&bound, limits, &assertions.occurrences)?;
    // After materialization: the checks need expanded capture occurrences and
    // their paths, which binding alone does not provide.
    let reference_errors =
        capture_key::key_projection_errors(&mut catalog_policy, &execution, &bound.sources);
    if !reference_errors.is_empty() {
        return Err(CompileError::from_diagnostics(reference_errors));
    }
    analyze_program(&mut execution, &bound.sources)?;
    let mut lowered = lower_to_nfa(&bound, &nfa_limits, &execution, &assertions.occurrences)?;
    lowered.matcher.install_assertions(assertions.catalog);
    // Beside the catalog its ids index, which is the only relationship it has:
    // this map records name-to-program, and outside the anchor tests nothing
    // reads it yet. The projection widening an `exclude` fragment needs comes
    // from `install_assertions` above, via the program `compile_assertions`
    // interned — not from this call, which may run at any point without
    // changing the result.
    lowered
        .matcher
        .install_anchor_programs(assertions.anchor_programs);
    // Ahead of the spawn gate below, which asks the matcher whether any pattern
    // needs an assertion session. Half that answer comes from the recognition
    // machine, which is already built; the other half is whether a capture or
    // span-equality plan queries a lookaround, and only this call establishes
    // it. A span-equality definition is skipped by NFA lowering, so its
    // lookaround exists in no other half. The gate returns `Err` on refusal, so
    // ordering it this way costs a refused compilation the plans it then
    // discards, and buys the gate and the per-sentence path one shared answer
    // rather than a compiler-side copy that can drift from it.
    lowered.matcher.install_execution(execution.into_vec());
    lowered.matcher.install_lexicons(compiled_lexicons);
    if lowered.matcher.requires_assertion_session()
        && let Some(decline) = lowered.matcher.spawn_decline()
    {
        return Err(CompileError::from_diagnostics(vec![
            Diagnostic::error_with_source_label(
                DiagnosticCode::LimitExceeded,
                bound.sources.source_label(SourceId::new(0)),
                0..0,
                format!(
                    "{} exceeded: requested {}, maximum {}",
                    decline.budget, decline.requested, decline.maximum
                ),
                1,
                1,
            ),
        ]));
    }
    lowered.matcher.install_catalog_policy(catalog_policy);
    lowered
        .matcher
        .install_declared_dictionaries(declared_dictionaries);
    lowered
        .matcher
        .install_analyzer_constraints(analyzer_constraints);
    lowered.matcher.finalize_required_attributes();
    // Strictly after the attribute projection is frozen: the synthesized
    // emitters must keep matching, so only the public catalog is filtered.
    lowered.matcher.hide_synthesized_patterns();
    stats.expansion_work = lowered.expansion_work + assertions.cost.expansion_work;
    stats.generated_owned_bytes =
        lowered.generated_owned_bytes + assertions.cost.generated_owned_bytes + lexicon_owned_bytes;
    stats.nfa_states = lowered.states + assertions.cost.states;
    stats.transitions = lowered.transitions + assertions.cost.transitions;

    diagnostics.extend(unverifiable_examples_warnings(&bound));
    diagnostics.append(&mut policy_warnings);
    // Raised while reading the syntax tree, long before any of the above, but
    // reported here: `CompileOutput::diagnostics` is one sorted list, and a
    // lowering warning has no reason to sit outside it.
    diagnostics.extend(lowering_warnings);
    diagnostics.sort_by(|left: &Diagnostic, right| {
        left.location()
            .source_name()
            .cmp(right.location().source_name())
            .then_with(|| {
                left.location()
                    .byte_range()
                    .start
                    .cmp(&right.location().byte_range().start)
            })
            .then_with(|| left.code().cmp(&right.code()))
            .then_with(|| left.message.cmp(&right.message))
    });

    Ok(CompileOutput {
        matcher: lowered.matcher,
        diagnostics,
        stats: *stats,
        analysis,
    })
}

/// Warn about every `fragment` that no definition references.
///
/// A `fragment` is referenceable syntax only — it is absent from runtime
/// catalogs and never emits a match. One that nothing references therefore
/// cannot affect any result, but it still costs the reader: the checked-in
/// catalogs run to four figures of lines, and a fragment kept alive by a
/// reference that was edited away is indistinguishable from one that is load
/// bearing.
///
/// This is a warning, not an error: the program is well-formed and its matches
/// are unchanged. It is the first non-fatal diagnostic the compiler produces,
/// so it is also what makes `CompileOutput::diagnostics` non-empty.
///
/// Reachability is computed as a fixpoint from the definitions that can emit,
/// so deleting a fragment's only consumer reports the whole orphaned cascade in
/// one run rather than one link per run.
///
/// A declared anchor's `exclude` fragment seeds the walk as well. It *is*
/// referenced — by the `anchor:` declaration rather than by a body — and both
/// halves of "never referenced, so it can never match" are false of it.
///
/// Returns the warnings and the set of definitions that are actually
/// evaluated. The internal `reachable` fixpoint holds *referenced fragments*;
/// an emitting definition is excluded from the diagnostic by the visibility
/// test below, not by absence from that set. A caller asking "is this
/// definition evaluated?" therefore needs the union, and getting the raw set
/// would answer `false` for every pattern.
fn unused_fragment_warnings(
    bound: &BoundProgram,
    catalog_policy: &CatalogPolicy,
) -> (Vec<Diagnostic>, BTreeSet<DefinitionId>) {
    // Seeding from *every* definition — as this once did — lets an unreferenced
    // fragment keep its own callees alive, so a deletion surfaces one link of
    // the cascade per run instead of all of it at once.
    let by_id: HashMap<DefinitionId, &BoundDefinition> = bound
        .definitions
        .iter()
        .map(|definition| (definition.id, definition))
        .collect();
    let by_name: HashMap<&str, DefinitionId> = bound
        .definitions
        .iter()
        .map(|definition| (definition.name.as_str(), definition.id))
        .collect();

    let mut seed = BTreeSet::new();
    for definition in &bound.definitions {
        if definition.is_emitting()
            && let Some(expr) = definition.expr()
        {
            collect_references(expr, &mut seed);
        }
    }
    for exclude in catalog_policy.anchors.values() {
        if let Some(id) = by_name.get(exclude.as_str()) {
            seed.insert(*id);
        }
    }

    let mut reachable = BTreeSet::new();
    let mut frontier: Vec<DefinitionId> = seed.into_iter().collect();
    while let Some(id) = frontier.pop() {
        if !reachable.insert(id) {
            continue;
        }
        if let Some(definition) = by_id.get(&id) {
            let mut referenced = BTreeSet::new();
            if let Some(expr) = definition.expr() {
                collect_references(expr, &mut referenced);
            }
            frontier.extend(referenced);
        }
    }

    let mut diagnostics: Vec<Diagnostic> = bound
        .definitions
        .iter()
        .filter(|definition| {
            definition.visibility() == Some(DefinitionVisibility::Fragment)
                && !reachable.contains(&definition.id)
        })
        .map(|definition| {
            let (line, column) = bound.sources.line_column(definition.span);
            let range = definition.span.byte_range();
            Diagnostic::warning_with_source_label(
                DiagnosticCode::UnusedFragment,
                bound.sources.source_label(definition.span.source()),
                usize::try_from(range.start).expect("source offsets originated as usize")
                    ..usize::try_from(range.end).expect("source offsets originated as usize"),
                format!(
                    "fragment `{}` is never referenced, so it can never match",
                    definition.name
                ),
                line,
                column,
            )
        })
        .collect();

    // Every non-fragment definition always runs, so it is reachable on its own
    // account — `reachable` above only tracks definitions *referenced from*
    // an emitting body, not the emitting bodies themselves. A pattern's own
    // direct `@SET` use would otherwise be invisible here.
    //
    // Collected rather than left as an iterator because both the set block and
    // the test block below chain it onto `reachable`, and `Chain` consumes its
    // second operand.
    let always_reachable: Vec<DefinitionId> = bound
        .definitions
        .iter()
        .filter(|definition| definition.is_emitting())
        .map(|definition| definition.id)
        .collect();

    // `reachable` is the fixpoint the fragment warning already computed, so
    // this reuses the answer rather than re-deriving it. A set held only by an
    // unreachable definition is therefore unused in the same run that reports
    // the definition, avoiding a follow-on diagnostic in a later run.
    let used: BTreeSet<&CompactString> = reachable
        .iter()
        .copied()
        .chain(always_reachable.iter().copied())
        .filter_map(|id| bound.set_uses.get(&id))
        .flatten()
        .collect();

    diagnostics.extend(
        bound
            .sets
            .iter()
            .filter(|set| !used.contains(&set.name))
            .map(|set| {
                let (line, column) = bound.sources.line_column(set.span);
                let range = set.span.byte_range();
                Diagnostic::warning_with_source_label(
                    DiagnosticCode::UnusedSet,
                    bound.sources.source_label(set.span.source()),
                    usize::try_from(range.start).expect("source offsets originated as usize")
                        ..usize::try_from(range.end).expect("source offsets originated as usize"),
                    format!("set `{}` is never referenced", set.name),
                    line,
                    column,
                )
            }),
    );

    // Same shape as the set block above, over `bound.tests`/`bound.test_uses`
    // instead of `bound.sets`/`bound.set_uses`, and sharing the same
    // `reachable`/`always_reachable` fixpoint: a test held only by an
    // unreachable definition is unused in the same run that reports the
    // definition, exactly as for a set.
    let used_tests: BTreeSet<&CompactString> = reachable
        .iter()
        .copied()
        .chain(always_reachable.iter().copied())
        .filter_map(|id| bound.test_uses.get(&id))
        .flatten()
        .collect();

    diagnostics.extend(
        bound
            .tests
            .iter()
            .filter(|test| !used_tests.contains(&test.name))
            .map(|test| {
                let (line, column) = bound.sources.line_column(test.span);
                let range = test.span.byte_range();
                Diagnostic::warning_with_source_label(
                    DiagnosticCode::UnusedTest,
                    bound.sources.source_label(test.span.source()),
                    usize::try_from(range.start).expect("source offsets originated as usize")
                        ..usize::try_from(range.end).expect("source offsets originated as usize"),
                    format!("test `{}` is never referenced", test.name),
                    line,
                    column,
                )
            }),
    );

    // The set a *caller* means by "evaluated". `reachable` alone holds only
    // definitions referenced from an emitting body, so it answers `false` for
    // every pattern — see this function's doc comment.
    let evaluated: BTreeSet<DefinitionId> =
        reachable.iter().copied().chain(always_reachable).collect();

    (diagnostics, evaluated)
}

/// The metadata keys carrying an executable claim about a **definition that
/// emits**.
///
/// Kept in step with `fsm::examples::VERIFIED_KEYS`, which decides what is
/// actually run. This list decides what is *checked for being runnable*, and
/// the two must not drift: a key verified there but absent here would have its
/// malformed cases pass silently without a dictionary.
///
/// `max_nesting:` is deliberately **not** here, and adding it would be wrong
/// twice over. It is a compound value rather than a string list, and it belongs
/// on a `fragment` — so the two complaints `unverifiable_examples_warnings`
/// raises are both false of it, and the first would tell its author to move it
/// to a pattern, which is the opposite of the rule. `policy::read_nesting_claims`
/// is its dictionary-free check, and it is fatal rather than a warning.
const CLAIM_KEYS: &[&str] = &["examples", "counterexamples"];

/// Warn about a documented claim that can never be executed.
///
/// A claim is the one executable statement a catalog makes about itself, and
/// `fsm::verify_examples` runs every one it can reach. Two shapes it cannot
/// reach, and this is the only thing that reports either without a dictionary:
///
/// * A claim on a `fragment`. Fragments never emit and never appear in
///   `patterns()`, so nothing can match against one.
/// * A claim whose value is not a list of strings. There is nothing to run.
///
/// In both cases the catalog compiles and matches correctly — what is broken is
/// the annotation. A skipped claim and a claim that held look identical, which
/// is the failure mode this whole tier exists to close.
///
/// The span is the `examples:`/`counterexamples:` key that carries the claim,
/// which is the line the author edits. It was the definition's span until
/// metadata became parsed syntax with per-key spans.
fn unverifiable_examples_warnings(bound: &BoundProgram) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for definition in &bound.definitions {
        for key in CLAIM_KEYS {
            let Some(value) = definition.metadata.get(*key).map(|entry| &entry.value) else {
                continue;
            };
            // A fragment with a malformed claim reports only the fragment
            // message. That is the right order: moving it to a pattern is the
            // necessary first step either way.
            let complaint = if definition.visibility() == Some(DefinitionVisibility::Fragment) {
                format!(
                    "fragment `{}` carries `{key}:`, but a fragment never emits, so no claim \
                     on it can be verified — move it to a pattern that references the fragment",
                    definition.name
                )
            } else if !value
                .as_array()
                .is_some_and(|entries| entries.iter().all(serde_json::Value::is_string))
            {
                format!(
                    "`{key}:` on `{}` must be a list of strings to be verifiable; found {value}",
                    definition.name
                )
            } else {
                continue;
            };

            // The claim is written in a metadata value, so it is reported at
            // the key that carries it. A fragment carrying `examples:` at all
            // is a defect of the definition, but the key is still where the
            // author deletes or moves it.
            let span = definition
                .metadata
                .get(*key)
                .map_or(definition.span, |entry| entry.span);
            let (line, column) = bound.sources.line_column(span);
            let range = span.byte_range();
            diagnostics.push(Diagnostic::warning_with_source_label(
                DiagnosticCode::UnverifiableExamples,
                bound.sources.source_label(span.source()),
                usize::try_from(range.start).expect("source offsets originated as usize")
                    ..usize::try_from(range.end).expect("source offsets originated as usize"),
                complaint,
                line,
                column,
            ));
        }
    }
    diagnostics
}

/// Collect every definition this expression references, walking the expression
/// tree but not the referenced definitions' own bodies. The transitive closure
/// is the caller's job — see `unused_fragment_warnings`, which needs it seeded
/// from emitting definitions only.
fn collect_references(expr: &BoundExpr, out: &mut BTreeSet<DefinitionId>) {
    match expr {
        BoundExpr::Definition { id, .. } => {
            out.insert(*id);
        }
        BoundExpr::Sequence(children)
        | BoundExpr::Alternation(children)
        | BoundExpr::AnyOrder {
            operands: children, ..
        } => {
            for child in children {
                collect_references(child, out);
            }
        }
        BoundExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_references(operand, out);
            }
        }
        BoundExpr::Repeat { expr, .. } | BoundExpr::Capture { expr, .. } => {
            collect_references(expr, out)
        }
        BoundExpr::Lookaround(lookaround) => collect_references(&lookaround.body, out),
        BoundExpr::Token(_) | BoundExpr::Assertion(_) | BoundExpr::SameAs { .. } => {}
    }
}

/// Rewrites every operation's span onto the accumulated registry's `SourceId`.
///
/// Here rather than in `fsm::policy_program` because `PolicySpan`'s field is
/// crate-private to the compiler, which is the point: a span is opaque to
/// consumers precisely so that remapping stays the compiler's business.
fn remap_program_source(program: &mut crate::fsm::policy_program::Program, source: SourceId) {
    use crate::fsm::policy_program::Op;
    for op in &mut program.ops {
        let (Op::Resolve { span, .. } | Op::Associate { span, .. } | Op::Admit { span, .. }) = op;
        span.0 = span.0.with_source(source);
    }
}

/// A catalog declaring two `policy` blocks.
///
/// Reported at `build` rather than at registration because the conflict is a
/// property of the **accumulated catalog**, not of either call: each document
/// is individually legal, and which one is "second" is decided by an order the
/// caller chose.
fn second_policy_block(
    sources: &SourceRegistry,
    first: SourceId,
    second: SourceId,
) -> CompileError {
    let earlier = sources.source_label(first);
    let later = sources.source_label(second);
    CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
        DiagnosticCode::Syntax,
        Arc::clone(&later),
        0..0,
        format!(
            "this catalog declares a `policy` block in `{later}` and another in `{earlier}`; \
             operation order is semantic and never concatenates across blocks, so a catalog \
             declares at most one"
        ),
        1,
        1,
    )])
}

fn remap_definition_source(definition: &mut ParsedDefinition, source: SourceId) {
    definition.span = definition.span.with_source(source);
    if let model::ParsedDefinitionBody::Pattern { expr, .. } = &mut definition.body {
        remap_expr_source(expr, source);
    }
}

/// Rewrites a `set` declaration's span into the accumulated registry.
///
/// The counterpart of [`remap_definition_source`]; a declaration carries one
/// span and no expression, so this is the whole of it.
fn remap_set_source(set: &mut ParsedSet, source: SourceId) {
    set.span = set.span.with_source(source);
}

/// Rewrites a `group` declaration's span into the accumulated registry.
///
/// Flat like [`remap_set_source`]: the declaration carries exactly one span,
/// and its predicate holds decoded values with no spans of their own.
fn remap_group_source(group: &mut ParsedGroupDecl, source: SourceId) {
    group.span = group.span.with_source(source);
}

/// Rewrites a `test` declaration's span, and every span inside its body, into
/// the accumulated registry.
///
/// Unlike [`remap_set_source`] this has to descend: a test body is a tree that
/// can hold both a deferred set operand and a reference to another test, and
/// each of those spans is what a diagnostic points at.
fn remap_test_source(test: &mut ParsedTestDecl, source: SourceId) {
    test.span = test.span.with_source(source);
    remap_test_body_source(&mut test.body, source);
}

/// The span-carrying leaves of one token test.
///
/// Both are references — a deferred `in @SET` operand and a `[@TEST]` — and
/// leaving either unmapped would not fail loudly: the span would resolve
/// against whichever document sits at `SourceId(0)` in the accumulated
/// registry, naming the wrong file confidently.
fn remap_test_body_source(test: &mut ParsedTest, source: SourceId) {
    match test {
        ParsedTest::Attr(parsed) => {
            if let ParsedAttrValue::Set { span, .. } = &mut parsed.value {
                *span = span.with_source(source);
            }
        }
        ParsedTest::Reference { span, .. } => *span = span.with_source(source),
        ParsedTest::And(operands) | ParsedTest::Or(operands) => {
            for operand in operands {
                remap_test_body_source(operand, source);
            }
        }
        ParsedTest::Not(inner) => remap_test_body_source(inner, source),
    }
}

fn remap_expr_source(expr: &mut ParsedExpr, source: SourceId) {
    match expr {
        // A token used to carry no span, which is why this arm was empty. A
        // deferred set operand carries one, and it is what an undefined-set
        // diagnostic points at.
        ParsedExpr::Token(test) => remap_test_body_source(test, source),
        ParsedExpr::Assertion(_) => {}
        ParsedExpr::Sequence(expressions) | ParsedExpr::Alternation(expressions) => {
            for expression in expressions {
                remap_expr_source(expression, source);
            }
        }
        // The operator's own span is remapped too, for the reason the token arm
        // above documents: left alone it would resolve against whichever
        // document sits at `SourceId(0)`, and name the wrong file confidently.
        ParsedExpr::Intersection { operands, span } => {
            *span = span.with_source(source);
            for operand in operands {
                remap_expr_source(operand, source);
            }
        }
        ParsedExpr::AnyOrder { operands, span } => {
            *span = span.with_source(source);
            for operand in operands {
                remap_expr_source(operand, source);
            }
        }
        ParsedExpr::Repeat { expr, .. } | ParsedExpr::Group(expr) => {
            remap_expr_source(expr, source)
        }
        ParsedExpr::Lookaround(lookaround) => {
            lookaround.span = lookaround.span.with_source(source);
            remap_expr_source(&mut lookaround.body, source);
        }
        ParsedExpr::NamedGroup { expr, span, .. } => {
            *span = span.with_source(source);
            remap_expr_source(expr, source);
        }
        ParsedExpr::SameAs { span, .. } => *span = span.with_source(source),
        ParsedExpr::Reference { span, .. } => *span = span.with_source(source),
    }
}

fn measure_program(
    program: &ParsedProgram,
    source_bytes: usize,
    metadata_bytes: usize,
) -> Result<CompilerStats, CompileError> {
    let mut stats = CompilerStats {
        source_bytes,
        definitions: program.definitions.len(),
        lexicon_members: program
            .definitions
            .iter()
            .map(|definition| match &definition.body {
                model::ParsedDefinitionBody::Lexicon { members, .. } => members.len(),
                model::ParsedDefinitionBody::Pattern { .. } => 0,
            })
            .sum(),
        metadata_bytes,
        ..CompilerStats::default()
    };

    for definition in &program.definitions {
        if let Some(expr) = definition.expr() {
            measure_expr(expr, 1, &mut stats)?;
        }
    }
    // A `test` body's regexes are charged once, here, rather than at each
    // reference. It contributes no expression node and no depth: a test is a
    // value, and the expression graph a definition builds is unchanged by
    // whether its predicate was named or written inline.
    for test in &program.tests {
        charge_test_regex_bytes(&test.body, &mut stats)?;
    }
    Ok(stats)
}

/// Charges a predicate's regex pattern bytes against the compile budget.
///
/// Every regex-carrying variant must be charged. This was twice-duplicated
/// `if let AttrValueType::Regex(..)`, which the compiler does not make
/// non-exhaustive: a new regex variant would have been omitted silently and
/// the deterministic limit would have stopped being truthful.
fn charge_regex_bytes(
    predicate: &[crate::fsm::SymbolicAttrValue],
    stats: &mut CompilerStats,
) -> Result<(), CompileError> {
    for attribute in predicate {
        charge_regex_bytes_of(&attribute.value, stats)?;
    }
    Ok(())
}

/// The parsed-layer counterpart of [`charge_regex_bytes`].
///
/// A `Set` operand never carries a regex — its members are set-declaration
/// literals — so it charges nothing rather than needing resolution here.
fn charge_parsed_regex_bytes(
    attribute: &ParsedAttr,
    stats: &mut CompilerStats,
) -> Result<(), CompileError> {
    if let ParsedAttrValue::Resolved(value) = &attribute.value {
        charge_regex_bytes_of(value, stats)?;
    }
    Ok(())
}

/// Charges every regex a token test holds, wherever `&`, `|` and `!` put it.
///
/// A `Reference` charges nothing here: its body is charged once at the `test`
/// declaration, which is where the researcher wrote the regex, so a test
/// referenced five times does not spend the budget five times.
fn charge_test_regex_bytes(
    test: &ParsedTest,
    stats: &mut CompilerStats,
) -> Result<(), CompileError> {
    match test {
        ParsedTest::Attr(attribute) => charge_parsed_regex_bytes(attribute, stats),
        ParsedTest::Reference { .. } => Ok(()),
        ParsedTest::And(operands) | ParsedTest::Or(operands) => {
            for operand in operands {
                charge_test_regex_bytes(operand, stats)?;
            }
            Ok(())
        }
        ParsedTest::Not(inner) => charge_test_regex_bytes(inner, stats),
    }
}

fn charge_regex_bytes_of(
    value: &AttrValueType,
    stats: &mut CompilerStats,
) -> Result<(), CompileError> {
    let regex = match value {
        AttrValueType::Regex(regex) | AttrValueType::NotRegex(regex) => regex,
        AttrValueType::Single(_)
        | AttrValueType::OneOf(_)
        | AttrValueType::NotIn(_)
        | AttrValueType::SetHas(_)
        | AttrValueType::SetHasAny(_)
        | AttrValueType::SetNotHas(_)
        | AttrValueType::SetNotHasAny(_)
        | AttrValueType::Contains(_)
        | AttrValueType::NotContains(_) => return Ok(()),
    };
    stats.regex_bytes = stats
        .regex_bytes
        .checked_add(regex.as_str().len())
        .ok_or_else(|| limit_error("<pattern>", "lingrep patterns regex byte count overflowed"))?;
    Ok(())
}

fn measure_expr(
    expr: &ParsedExpr,
    depth: usize,
    stats: &mut CompilerStats,
) -> Result<(), CompileError> {
    stats.expression_nodes = stats.expression_nodes.checked_add(1).ok_or_else(|| {
        limit_error(
            "<pattern>",
            "lingrep patterns expression-node count overflowed",
        )
    })?;
    stats.max_depth = stats.max_depth.max(depth);

    match expr {
        ParsedExpr::Token(test) => charge_test_regex_bytes(test, stats)?,
        ParsedExpr::Assertion(assertion) => {
            if let crate::fsm::PositionAssertion::PreviousToken(predicate) = assertion {
                charge_regex_bytes(predicate, stats)?;
            }
        }
        ParsedExpr::Sequence(expressions) | ParsedExpr::Alternation(expressions) => {
            for expression in expressions {
                measure_expr(expression, depth.saturating_add(1), stats)?;
            }
        }
        ParsedExpr::Intersection { operands, .. } => {
            for operand in operands {
                measure_expr(operand, depth.saturating_add(1), stats)?;
            }
        }
        ParsedExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                measure_expr(operand, depth.saturating_add(1), stats)?;
            }
        }
        ParsedExpr::Repeat { expr, .. } | ParsedExpr::Group(expr) => {
            measure_expr(expr, depth.saturating_add(1), stats)?;
        }
        ParsedExpr::Lookaround(lookaround) => {
            measure_expr(&lookaround.body, depth.saturating_add(1), stats)?;
        }
        ParsedExpr::NamedGroup { expr, .. } => {
            measure_expr(expr, depth.saturating_add(1), stats)?;
        }
        ParsedExpr::SameAs { .. } => {}
        ParsedExpr::Reference { .. } => {
            stats.reference_edges = stats.reference_edges.checked_add(1).ok_or_else(|| {
                limit_error(
                    "<pattern>",
                    "lingrep patterns reference-edge count overflowed",
                )
            })?;
        }
    }
    Ok(())
}

fn checked_stats_add(
    left: CompilerStats,
    right: CompilerStats,
    source_label: &Arc<str>,
) -> Result<CompilerStats, CompileError> {
    let add = |left: usize, right: usize, budget: &str| {
        left.checked_add(right)
            .ok_or_else(|| limit_error(Arc::clone(source_label), budget))
    };
    Ok(CompilerStats {
        source_bytes: add(
            left.source_bytes,
            right.source_bytes,
            "source byte count overflowed",
        )?,
        definitions: add(
            left.definitions,
            right.definitions,
            "definition count overflowed",
        )?,
        lexicon_members: add(
            left.lexicon_members,
            right.lexicon_members,
            "lexicon member count overflowed",
        )?,
        expression_nodes: add(
            left.expression_nodes,
            right.expression_nodes,
            "expression-node count overflowed",
        )?,
        max_depth: left.max_depth.max(right.max_depth),
        metadata_bytes: add(
            left.metadata_bytes,
            right.metadata_bytes,
            "metadata byte count overflowed",
        )?,
        regex_bytes: add(
            left.regex_bytes,
            right.regex_bytes,
            "regex byte count overflowed",
        )?,
        reference_edges: add(
            left.reference_edges,
            right.reference_edges,
            "reference-edge count overflowed",
        )?,
        expansion_work: 0,
        generated_owned_bytes: 0,
        nfa_states: 0,
        transitions: 0,
    })
}

fn enforce_accumulated_limits(
    stats: CompilerStats,
    limits: &CompileLimits,
    source_label: &Arc<str>,
) -> Result<(), CompileError> {
    for (actual, maximum, budget) in [
        (
            stats.source_bytes,
            limits.max_source_bytes(),
            "source bytes",
        ),
        (stats.definitions, limits.max_definitions(), "definitions"),
        (
            stats.lexicon_members,
            limits.max_lexicon_members(),
            "lexicon members",
        ),
        (
            stats.expression_nodes,
            limits.max_expression_nodes(),
            "expression nodes",
        ),
        (stats.max_depth, limits.max_nesting_depth(), "nesting depth"),
        (
            stats.metadata_bytes,
            limits.max_metadata_bytes(),
            "metadata bytes",
        ),
        (stats.regex_bytes, limits.max_regex_bytes(), "regex bytes"),
        (
            stats.reference_edges,
            limits.max_reference_edges(),
            "reference edges",
        ),
    ] {
        if actual > maximum {
            return Err(limit_error(
                Arc::clone(source_label),
                &format!("lingrep patterns builder exceeds the {budget} budget"),
            ));
        }
    }
    Ok(())
}

fn limit_error(source_name: impl Into<Arc<str>>, message: &str) -> CompileError {
    CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
        DiagnosticCode::LimitExceeded,
        source_name.into(),
        0..0,
        message,
        1,
        1,
    )])
}
