use compact_str::CompactString;
use indexmap::IndexMap;
use indexmap::map::Entry;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::diagnostic::{
    CompileError, Diagnostic, DiagnosticCode, DiagnosticLocation, RelatedDiagnosticLocation,
};
use super::limits::CompileLimits;
use super::model::{
    BoundDefinition, BoundDefinitionBody, BoundExpr, BoundProgram, DefinitionId,
    DefinitionVisibility, LookaroundExpr, ParsedAttr, ParsedAttrValue, ParsedDefinition,
    ParsedDefinitionBody, ParsedExpr, ParsedProgram, ParsedTest, SourceId, SourceRegistry,
    SourceSpan,
};
use super::token_test::{ResolvedTest, to_dnf};
use crate::fsm::{AttrValue, AttrValueType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DfsColor {
    White,
    Gray,
    Black,
}

struct PendingDiagnostic {
    source_id: SourceId,
    start: u32,
    code: DiagnosticCode,
    referenced_name: CompactString,
    diagnostic: Diagnostic,
}

#[derive(Clone, Copy)]
struct ReferenceEdge {
    target: DefinitionId,
    span: SourceSpan,
}

struct Preflight<'a> {
    limits: &'a CompileLimits,
    sources: &'a SourceRegistry,
    expression_nodes: usize,
    reference_edges: usize,
}

impl<'a> Preflight<'a> {
    fn new(limits: &'a CompileLimits, sources: &'a SourceRegistry) -> Self {
        Self {
            limits,
            sources,
            expression_nodes: 0,
            reference_edges: 0,
        }
    }

    fn enter_node(&mut self, span: SourceSpan) -> Result<(), CompileError> {
        self.expression_nodes = self.expression_nodes.checked_add(1).ok_or_else(|| {
            limit_error(
                self.sources,
                span,
                "lingrep patterns expression-node count overflowed",
            )
        })?;
        if self.expression_nodes > self.limits.max_expression_nodes() {
            return Err(limit_error(
                self.sources,
                span,
                "lingrep patterns program exceeds the expression-node limit",
            ));
        }
        Ok(())
    }

    fn enter_reference(&mut self, span: SourceSpan) -> Result<(), CompileError> {
        self.reference_edges = self.reference_edges.checked_add(1).ok_or_else(|| {
            limit_error(
                self.sources,
                span,
                "lingrep patterns reference-edge count overflowed",
            )
        })?;
        if self.reference_edges > self.limits.max_reference_edges() {
            return Err(limit_error(
                self.sources,
                span,
                "lingrep patterns program exceeds the reference-edge limit",
            ));
        }
        Ok(())
    }
}

/// Resolves one complete parsed program to a validated, acyclic definition graph.
pub(super) fn bind(
    program: ParsedProgram,
    limits: &CompileLimits,
) -> Result<BoundProgram, CompileError> {
    let ParsedProgram {
        definitions,
        sources,
        // Bound separately, after evidence synthesis has appended to the
        // definition list this function is about to consume.
        policy: _,
        sets,
        tests,
        // Consumed before binding, at the `groups:`-reading site in
        // `compiler/policy.rs`: membership lives in the `%` namespace, which
        // shares nothing with the `@` names this function adjudicates.
        groups: _,
        analyzers: _,
    } = program;
    enforce_definition_budget(&definitions, &sources, limits)?;

    let mut symbols = IndexMap::with_capacity(definitions.len());
    let mut diagnostics = Vec::new();
    for (source_index, definition) in definitions.iter().enumerate() {
        let id = DefinitionId::new(u32::try_from(source_index).map_err(|_| {
            limit_error(
                &sources,
                definition.span,
                "lingrep patterns definition identity exceeds the supported range",
            )
        })?);
        match symbols.entry(definition.name.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(id);
            }
            Entry::Occupied(entry) => {
                let first = &definitions[entry.get().index()];
                diagnostics.push(pending_diagnostic(
                    &sources,
                    definition.span,
                    DiagnosticCode::DuplicateDefinition,
                    definition.name.clone(),
                    format!("duplicate definition {}", definition.name),
                    Some((first.span, "first definition is here")),
                ));
            }
        }
    }

    // The catalog's set table, and the one-referent rule that keeps `@X`
    // unambiguous. The kinds stay stratified — only a set is legal after `in`
    // — but they share one name space, so a reader never needs the position to
    // know what `@X` refers to. The check is here, over the accumulated
    // catalog, because that is the scope: two documents each legal on their
    // own can still collide.
    // One allocation per declared set, shared by every site that resolves to
    // it. Built here rather than at the reference, because building it there
    // is what made resolution cost M·N.
    let mut set_members: IndexMap<CompactString, Arc<[CompactString]>> =
        IndexMap::with_capacity(sets.len());
    let mut set_spans: IndexMap<CompactString, SourceSpan> = IndexMap::with_capacity(sets.len());
    for set in &sets {
        if let Some(id) = symbols.get(&set.name) {
            diagnostics.push(duplicate_name(
                &sources,
                &set.name,
                definitions[id.index()].span,
                set.span,
            ));
            continue;
        }
        match set_spans.entry(set.name.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(set.span);
                let mut members = set.members.clone();
                members.sort_unstable();
                set_members.insert(set.name.clone(), Arc::from(members.into_boxed_slice()));
            }
            // Reported rather than overwritten: `insert` on a duplicate keeps
            // the last declaration and says nothing, which is the silent wrong
            // answer this catalog scope would otherwise buy.
            Entry::Occupied(entry) => {
                diagnostics.push(duplicate_name(&sources, &set.name, *entry.get(), set.span));
            }
        }
    }

    // The catalog's test table, under the identical one-referent rule: a test
    // colliding with a definition, with a set, or with another test is one
    // name with two referents, and `@X` inside `[…]` would have to guess.
    let mut test_bodies: IndexMap<CompactString, ParsedTest> = IndexMap::with_capacity(tests.len());
    let mut test_spans: IndexMap<CompactString, SourceSpan> = IndexMap::with_capacity(tests.len());
    for test in &tests {
        if let Some(id) = symbols.get(&test.name) {
            diagnostics.push(duplicate_name(
                &sources,
                &test.name,
                definitions[id.index()].span,
                test.span,
            ));
            continue;
        }
        if let Some(span) = set_spans.get(&test.name) {
            diagnostics.push(duplicate_name(&sources, &test.name, *span, test.span));
            continue;
        }
        match test_spans.entry(test.name.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(test.span);
                test_bodies.insert(test.name.clone(), test.body.clone());
            }
            // Reported rather than overwritten, for the reason the set loop
            // above documents.
            Entry::Occupied(entry) => {
                diagnostics.push(duplicate_name(
                    &sources,
                    &test.name,
                    *entry.get(),
                    test.span,
                ));
            }
        }
    }

    let inspect_ctx = InspectCtx {
        sources: &sources,
        symbols: &symbols,
        definitions: &definitions,
        set_members: &set_members,
        test_names: &test_bodies,
        limits,
    };
    let mut adjacency = vec![Vec::new(); definitions.len()];
    let mut preflight = Preflight::new(limits, &sources);
    for (definition_index, definition) in definitions.iter().enumerate() {
        let Some(expr) = definition.expr() else {
            continue;
        };
        inspect_expr(
            expr,
            definition.span,
            &inspect_ctx,
            &mut preflight,
            &mut adjacency[definition_index],
            &mut diagnostics,
        )?;
    }

    if !diagnostics.is_empty() {
        return Err(semantic_error(diagnostics));
    }

    let finish_order = validate_acyclic(&definitions, &adjacency, &sources)?;
    let definition_spans = definitions
        .iter()
        .map(|definition| definition.span)
        .collect::<Vec<_>>();
    let mut global_capture_names = BTreeSet::new();
    for definition in &definitions {
        if let Some(expr) = definition.expr() {
            collect_capture_names(expr, &mut global_capture_names);
        }
    }
    // Indexed by `DefinitionId`, alongside `definition_spans`: the definitions
    // vector itself is consumed below, one expression at a time, but a set
    // operand after `in` needs to name the visibility of *some other*
    // definition the same name resolved to — not necessarily the one
    // currently being bound — so that lookup has to survive the consuming
    // loop. Only `visibility` is copied out, because that field is `Copy` and
    // the message it feeds is the only thing this lookup is for.
    let visibilities = definitions
        .iter()
        .map(ParsedDefinition::visibility)
        .collect::<Vec<_>>();
    let mut bound_definitions = Vec::with_capacity(definitions.len());
    let mut binding_diagnostics = Vec::new();
    let mut set_uses: BTreeMap<DefinitionId, BTreeSet<CompactString>> = BTreeMap::new();
    let mut test_uses: BTreeMap<DefinitionId, BTreeSet<CompactString>> = BTreeMap::new();
    // One budget for the whole accumulated catalog, built before the loop and
    // never rebuilt inside it: total substitution work is the quantity the
    // amplifier moves, and it is invisible to any counter that resets.
    let test_budget = TestNodeBudget::new(limits);
    for (index, definition) in definitions.into_iter().enumerate() {
        let id = DefinitionId::new(
            u32::try_from(index).expect("definition IDs were checked before bound allocation"),
        );
        let (body, used_sets, used_tests) = match definition.body {
            ParsedDefinitionBody::Pattern {
                visibility,
                expr,
                relations,
                enhanced_relations,
            } => {
                let (expr, relations, enhanced_relations, used_sets, used_tests) = bind_definition(
                    expr,
                    &relations,
                    &enhanced_relations,
                    &symbols,
                    &set_members,
                    &test_bodies,
                    &test_spans,
                    &visibilities,
                    &sources,
                    &global_capture_names,
                    definition.span,
                    limits,
                    &test_budget,
                    &mut binding_diagnostics,
                );
                (
                    BoundDefinitionBody::Pattern {
                        visibility,
                        expr,
                        relations,
                        enhanced_relations,
                    },
                    used_sets,
                    used_tests,
                )
            }
            ParsedDefinitionBody::Lexicon { by, members } => (
                BoundDefinitionBody::Lexicon { by, members },
                BTreeSet::new(),
                BTreeSet::new(),
            ),
        };
        if !used_sets.is_empty() {
            set_uses.insert(id, used_sets);
        }
        if !used_tests.is_empty() {
            test_uses.insert(id, used_tests);
        }
        bound_definitions.push(BoundDefinition {
            id,
            source: definition.span.source(),
            name: definition.name,
            body,
            metadata: definition.metadata,
            key: definition.key,
            span: definition.span,
        });
    }
    if !binding_diagnostics.is_empty() {
        return Err(semantic_error(binding_diagnostics));
    }
    let bound = BoundProgram {
        definitions: bound_definitions,
        sources,
        sets,
        set_uses,
        tests,
        test_uses,
    };

    validate_nullability(bound, &finish_order, &definition_spans)
}

fn enforce_definition_budget(
    definitions: &[ParsedDefinition],
    sources: &SourceRegistry,
    limits: &CompileLimits,
) -> Result<(), CompileError> {
    if definitions.len() <= limits.max_definitions() {
        return Ok(());
    }

    let span = definitions[limits.max_definitions()].span;
    Err(limit_error(
        sources,
        span,
        "lingrep patterns program exceeds the definition limit",
    ))
}

/// The read-only tables `inspect_expr` needs at every recursion site.
///
/// Bundled for the same reason as [`BindCtx`]: `sources`, `symbols`,
/// `set_members` and `test_names` are invariant across one definition's whole
/// walk.
struct InspectCtx<'a> {
    sources: &'a SourceRegistry,
    symbols: &'a IndexMap<CompactString, DefinitionId>,
    definitions: &'a [ParsedDefinition],
    set_members: &'a IndexMap<CompactString, Arc<[CompactString]>>,
    /// The catalog's test names, read only to tell a `@t` in expression
    /// position from a typo. The bodies are not needed here — nothing in this
    /// walk resolves one — so this takes the same table `resolve_test` reads
    /// and asks it only which names exist.
    test_names: &'a IndexMap<CompactString, ParsedTest>,
    limits: &'a CompileLimits,
}

fn inspect_expr(
    expr: &ParsedExpr,
    definition_span: SourceSpan,
    ctx: &InspectCtx<'_>,
    preflight: &mut Preflight<'_>,
    edges: &mut Vec<ReferenceEdge>,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> Result<(), CompileError> {
    let expression_span = match expr {
        ParsedExpr::Reference { span, .. }
        | ParsedExpr::Intersection { span, .. }
        | ParsedExpr::AnyOrder { span, .. }
        | ParsedExpr::Lookaround(LookaroundExpr { span, .. }) => *span,
        _ => definition_span,
    };
    preflight.enter_node(expression_span)?;
    match expr {
        ParsedExpr::Token(_) | ParsedExpr::Assertion(_) | ParsedExpr::SameAs { .. } => {}
        ParsedExpr::Lookaround(lookaround) => inspect_expr(
            &lookaround.body,
            definition_span,
            ctx,
            preflight,
            edges,
            diagnostics,
        )?,
        ParsedExpr::Sequence(expressions) | ParsedExpr::Alternation(expressions) => {
            for expression in expressions {
                inspect_expr(
                    expression,
                    definition_span,
                    ctx,
                    preflight,
                    edges,
                    diagnostics,
                )?;
            }
        }
        ParsedExpr::Intersection { operands, .. } => {
            for operand in operands {
                inspect_expr(operand, definition_span, ctx, preflight, edges, diagnostics)?;
            }
        }
        ParsedExpr::AnyOrder { operands, span } => {
            if operands.len() < 2 {
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::AnyOrderTooFewOperands,
                    CompactString::new(""),
                    "`any_order` needs at least two named node captures",
                    None,
                ));
            }
            let permutations = (2..=operands.len()).try_fold(1usize, usize::checked_mul);
            if permutations.is_none_or(|count| count > ctx.limits.max_expansion_work()) {
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::AnyOrderTooComplex,
                    CompactString::new(""),
                    format!(
                        "`any_order` has more operand permutations than the expansion-work \
                         budget of {} permits",
                        ctx.limits.max_expansion_work()
                    ),
                    None,
                ));
            }
            for operand in operands {
                if !matches!(operand, ParsedExpr::NamedGroup { .. }) {
                    diagnostics.push(pending_diagnostic(
                        ctx.sources,
                        *span,
                        DiagnosticCode::AnyOrderOperandMustBeCapture,
                        CompactString::new(""),
                        "every `any_order` operand must be a named capture",
                        None,
                    ));
                }
                inspect_expr(operand, definition_span, ctx, preflight, edges, diagnostics)?;
            }
        }
        ParsedExpr::Repeat { expr, min, max } => {
            if max.is_some_and(|max| *min > max) {
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    definition_span,
                    DiagnosticCode::InvalidQuantifier,
                    CompactString::new(""),
                    "quantifier minimum exceeds its maximum",
                    None,
                ));
            }
            inspect_expr(expr, definition_span, ctx, preflight, edges, diagnostics)?;
        }
        ParsedExpr::Group(expr) | ParsedExpr::NamedGroup { expr, .. } => {
            inspect_expr(expr, definition_span, ctx, preflight, edges, diagnostics)?;
        }
        ParsedExpr::Reference { name, span } => {
            preflight.enter_reference(*span)?;
            if let Some(id) = ctx.symbols.get(name) {
                if ctx.definitions[id.index()].expr().is_some() {
                    edges.push(ReferenceEdge {
                        target: *id,
                        span: *span,
                    });
                } else {
                    diagnostics.push(pending_diagnostic(
                        ctx.sources,
                        *span,
                        DiagnosticCode::UndefinedReference,
                        name.clone(),
                        format!("`@{name}` is a lexicon; a lexicon is not an expression"),
                        None,
                    ));
                }
            } else if ctx.set_members.contains_key(name) {
                // The mirror mistake of a set after `in`: the sigil is shared,
                // so a set in pattern position resolves against nothing named
                // `symbols` and would otherwise look like a typo rather than
                // the wrong kind in the wrong place.
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::UndefinedReference,
                    name.clone(),
                    format!("`@{name}` is a set; a set is not an expression"),
                    None,
                ));
            } else if ctx.test_names.contains_key(name) {
                // And the mirror of a definition inside `[…]`, which
                // `resolve_test` already names by kind. A test is a token
                // predicate, so it is legal in exactly one place and this is
                // not it; reporting "undefined" would send the author looking
                // for a typo with the declaration on screen.
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::UndefinedReference,
                    name.clone(),
                    format!(
                        "`@{name}` is a test; a test is a token predicate, so it belongs \
                         inside `[…]` — write `[@{name}]`"
                    ),
                    None,
                ));
            } else {
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::UndefinedReference,
                    name.clone(),
                    format!("undefined reference @{name}"),
                    None,
                ));
            }
        }
    }
    Ok(())
}

fn validate_acyclic(
    definitions: &[ParsedDefinition],
    adjacency: &[Vec<ReferenceEdge>],
    sources: &SourceRegistry,
) -> Result<Vec<DefinitionId>, CompileError> {
    let mut colors = vec![DfsColor::White; definitions.len()];
    let mut finish_order = Vec::with_capacity(definitions.len());

    for start in 0..definitions.len() {
        if colors[start] != DfsColor::White {
            continue;
        }

        let start_id = DefinitionId::new(
            u32::try_from(start).expect("definition IDs were checked before cycle validation"),
        );
        colors[start] = DfsColor::Gray;
        let mut path = vec![start_id];
        let mut frames = vec![(start_id, 0_usize)];

        while let Some((id, next_edge)) = frames.last_mut() {
            let index = id.index();
            if *next_edge == adjacency[index].len() {
                colors[index] = DfsColor::Black;
                finish_order.push(*id);
                frames.pop();
                path.pop();
                continue;
            }

            let edge = adjacency[index][*next_edge];
            let target = edge.target;
            *next_edge += 1;
            match colors[target.index()] {
                DfsColor::White => {
                    colors[target.index()] = DfsColor::Gray;
                    path.push(target);
                    frames.push((target, 0));
                }
                DfsColor::Gray => {
                    let cycle_start = path
                        .iter()
                        .position(|candidate| *candidate == target)
                        .expect("gray definitions must be present in the DFS path");
                    let mut cycle = path[cycle_start..].to_vec();
                    cycle.push(target);
                    let names = cycle
                        .iter()
                        .map(|cycle_id| definitions[cycle_id.index()].name.as_str())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    let definition = &definitions[target.index()];
                    return Err(semantic_error(vec![pending_diagnostic(
                        sources,
                        edge.span,
                        DiagnosticCode::ReferenceCycle,
                        definition.name.clone(),
                        format!("reference cycle: {names}"),
                        None,
                    )]));
                }
                DfsColor::Black => {}
            }
        }
    }

    Ok(finish_order)
}

/// Binds one definition's body and reports which named values it referenced.
///
/// Set and test names come back beside the expression rather than being
/// recovered from it: resolution is where a name stops existing — a bound
/// token holds literal predicates and nothing else — and the reachability walk
/// that needs the names runs after binding.
/// Every argument is one read-only catalog table this definition's walk needs;
/// [`BindCtx`] below bundles them, and this is the one call that assembles it.
fn bind_definition(
    expr: ParsedExpr,
    relations: &[super::model::ParsedRelation],
    enhanced_relations: &[super::model::ParsedEnhancedRelation],
    symbols: &IndexMap<CompactString, DefinitionId>,
    set_members: &IndexMap<CompactString, Arc<[CompactString]>>,
    test_bodies: &IndexMap<CompactString, ParsedTest>,
    test_spans: &IndexMap<CompactString, SourceSpan>,
    visibilities: &[Option<DefinitionVisibility>],
    sources: &SourceRegistry,
    global_capture_names: &BTreeSet<CompactString>,
    definition_span: SourceSpan,
    limits: &CompileLimits,
    test_budget: &TestNodeBudget,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> (
    BoundExpr,
    Box<[super::model::BoundRelation]>,
    Box<[super::model::BoundEnhancedRelation]>,
    BTreeSet<CompactString>,
    BTreeSet<CompactString>,
) {
    let mut declarations = BTreeMap::new();
    collect_capture_declarations(&expr, sources, &mut declarations, diagnostics);
    let ctx = BindCtx {
        symbols,
        set_members,
        test_bodies,
        visibilities,
        declarations: &declarations,
        sources,
        global_capture_names,
        definition_span,
        limits,
        test_budget,
    };
    let mut assigned = BTreeSet::new();
    let mut uses = Uses::default();
    let bound = bind_expr(expr, &ctx, &mut assigned, &mut uses, diagnostics);
    let relations = bind_relations(relations, test_spans, &ctx, &mut uses, diagnostics);
    let enhanced_relations =
        bind_enhanced_relations(enhanced_relations, test_spans, &ctx, &mut uses, diagnostics);
    (bound, relations, enhanced_relations, uses.sets, uses.tests)
}

/// The names one definition's walk resolved, recorded as it went.
///
/// One struct rather than two threaded parameters because the two travel
/// together through every recursion site and are reported together: a set and a
/// test are both catalog-scoped values whose name stops existing at resolution,
/// so the walk that resolves either has to hand both back.
#[derive(Default)]
struct Uses {
    sets: BTreeSet<CompactString>,
    tests: BTreeSet<CompactString>,
}

fn collect_capture_names(expr: &ParsedExpr, names: &mut BTreeSet<CompactString>) {
    match expr {
        ParsedExpr::NamedGroup { name, expr, .. } => {
            names.insert(name.clone());
            collect_capture_names(expr, names);
        }
        ParsedExpr::Sequence(children)
        | ParsedExpr::Alternation(children)
        | ParsedExpr::AnyOrder {
            operands: children, ..
        } => {
            for child in children {
                collect_capture_names(child, names);
            }
        }
        ParsedExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_capture_names(operand, names);
            }
        }
        ParsedExpr::Repeat { expr, .. } | ParsedExpr::Group(expr) => {
            collect_capture_names(expr, names)
        }
        ParsedExpr::Lookaround(lookaround) => collect_capture_names(&lookaround.body, names),
        ParsedExpr::Token(_)
        | ParsedExpr::Assertion(_)
        | ParsedExpr::SameAs { .. }
        | ParsedExpr::Reference { .. } => {}
    }
}

fn collect_capture_declarations(
    expr: &ParsedExpr,
    sources: &SourceRegistry,
    declarations: &mut BTreeMap<CompactString, (super::model::CaptureDeclId, SourceSpan)>,
    diagnostics: &mut Vec<PendingDiagnostic>,
) {
    match expr {
        ParsedExpr::NamedGroup {
            name, expr, span, ..
        } => {
            let next = super::model::CaptureDeclId::new(
                u32::try_from(declarations.len()).expect("capture count is expression-bounded"),
            );
            if let Some((_, first)) = declarations.get(name) {
                diagnostics.push(pending_diagnostic(
                    sources,
                    *span,
                    DiagnosticCode::DuplicateCapture,
                    name.clone(),
                    format!("duplicate capture {name}"),
                    Some((*first, "first capture declaration is here")),
                ));
            } else {
                declarations.insert(name.clone(), (next, *span));
            }
            collect_capture_declarations(expr, sources, declarations, diagnostics);
        }
        ParsedExpr::Sequence(expressions)
        | ParsedExpr::Alternation(expressions)
        | ParsedExpr::AnyOrder {
            operands: expressions,
            ..
        } => {
            for expression in expressions {
                collect_capture_declarations(expression, sources, declarations, diagnostics);
            }
        }
        ParsedExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_capture_declarations(operand, sources, declarations, diagnostics);
            }
        }
        ParsedExpr::Repeat { expr, .. } | ParsedExpr::Group(expr) => {
            collect_capture_declarations(expr, sources, declarations, diagnostics)
        }
        ParsedExpr::Lookaround(lookaround) => {
            collect_capture_declarations(&lookaround.body, sources, declarations, diagnostics)
        }
        ParsedExpr::Token(_)
        | ParsedExpr::Assertion(_)
        | ParsedExpr::SameAs { .. }
        | ParsedExpr::Reference { .. } => {}
    }
}

/// The read-only tables `bind_expr` needs at every recursion site.
///
/// `symbols`, `set_members`, `test_bodies`, `visibilities`, `declarations`,
/// `sources`, `global_capture_names` and `limits` are invariant across one
/// definition's whole walk; only `assigned` and `uses` vary per recursive
/// call, and `diagnostics` is a sink, so both stay separate parameters rather
/// than joining this struct.
struct BindCtx<'a> {
    symbols: &'a IndexMap<CompactString, DefinitionId>,
    set_members: &'a IndexMap<CompactString, Arc<[CompactString]>>,
    /// The catalog's `test` bodies, still unresolved: a test may reference
    /// another, so resolution is a walk rather than a lookup.
    test_bodies: &'a IndexMap<CompactString, ParsedTest>,
    /// Indexed by [`DefinitionId`]; see `visibilities` in `bind` for why this
    /// is a projection rather than the definitions themselves.
    visibilities: &'a [Option<DefinitionVisibility>],
    declarations: &'a BTreeMap<CompactString, (super::model::CaptureDeclId, SourceSpan)>,
    sources: &'a SourceRegistry,
    global_capture_names: &'a BTreeSet<CompactString>,
    /// The span of the definition being bound.
    ///
    /// A token test is spanless — only a reference inside one carries a span —
    /// so a diagnostic about the test as a whole needs the enclosing
    /// declaration to point at, exactly as `inspect_expr`'s `_ =>
    /// definition_span` arm does for a quantifier.
    definition_span: SourceSpan,
    /// The compile budgets a token test's resolution is checked against.
    limits: &'a CompileLimits,
    /// The catalog-wide substitution counters, shared by every definition's
    /// walk. Owned by `bind` rather than by this struct: a budget rebuilt per
    /// definition — or, as it was, per `[…]` — cannot see a total.
    test_budget: &'a TestNodeBudget,
}

/// The substitution budgets shared by every `[…]` in the accumulated catalog.
///
/// Two counters, because a single one cannot be both. `per_test` is the shape
/// budget: a `test a := @b & @b.` chain doubles the *tree* at each level, so
/// eleven lines of source expand past any fixed size while distributing to
/// exactly one branch, and pointing the diagnostic at that chain is what makes
/// it actionable. `document` is the work budget: one such chain named by
/// 65,000 `[…]` occurrences pays its expansion 65,000 times, and a budget that
/// resets per token test can only see each payment, never the total.
///
/// Both are enforced during substitution rather than measured afterwards, so
/// construction cannot exceed a limit before accounting notices.
///
/// `document` is seeded from [`CompileLimits::max_expansion_work`] rather than
/// from a budget of its own: resolved-node construction is expression-visit
/// work of exactly the kind that budget already names, and it is wide enough
/// that no catalog is narrowed — all five tracked catalogs *together* resolve
/// 459 nodes against its 262,144 default, a factor of 570. It is deliberately
/// not charged into [`super::CompilerStats::expansion_work`], which is a
/// published compile metric folded into catalog identity.
///
/// `reported` is here rather than on [`TestResolution`] so an exhausted
/// document budget produces one diagnostic instead of one per remaining `[…]`.
struct TestNodeBudget {
    per_test: usize,
    document: Cell<usize>,
    reported: Cell<bool>,
}

impl TestNodeBudget {
    fn new(limits: &CompileLimits) -> Self {
        Self {
            per_test: limits.max_resolved_test_nodes(),
            document: Cell::new(limits.max_expansion_work()),
            reported: Cell::new(false),
        }
    }
}

/// The state one token test's resolution carries across its recursion.
///
/// `visiting` is the chain of `test` names currently being expanded, so a cycle
/// is a name already on it. `remaining_nodes` is separate from the distribution
/// budget and bounds a different blow-up: `test a := @b & @b.` doubles the
/// *tree* at each level, so a chain of them builds an exponentially large
/// `ResolvedTest` before `to_dnf` ever sees a branch count.
///
/// `reference_spans` mirrors `visiting`, one span per name on the chain: the
/// span of the `@name` that is currently being substituted. A resolved test
/// node otherwise carries no span of its own, so this is what lets the
/// overflow diagnostic point at the reference whose substitution was in
/// flight when the budget ran out, rather than always at the enclosing
/// definition.
struct TestResolution<'a> {
    visiting: Vec<CompactString>,
    reference_spans: Vec<SourceSpan>,
    remaining_nodes: usize,
    /// The catalog-wide counters this test's charges also draw down.
    budget: &'a TestNodeBudget,
    overflowed: bool,
    /// The innermost reference being substituted at the moment a budget first
    /// failed, captured once on the first overflow so a later charge against an
    /// already-exhausted budget cannot overwrite it.
    overflow_span: Option<SourceSpan>,
    /// Whether it was the document budget rather than this test's own that ran
    /// out, which is what the diagnostic has to say: the author of a catalog
    /// refused for total substitution work is not looking at one oversized
    /// test.
    document_exhausted: bool,
}

impl<'a> TestResolution<'a> {
    fn new(budget: &'a TestNodeBudget) -> Self {
        Self {
            visiting: Vec::new(),
            reference_spans: Vec::new(),
            remaining_nodes: budget.per_test,
            budget,
            overflowed: false,
            overflow_span: None,
            document_exhausted: false,
        }
    }

    /// Charges one resolved node against both budgets, reporting whether they
    /// still hold.
    fn admit_node(&mut self) -> bool {
        let document = self.budget.document.get();
        match (self.remaining_nodes.checked_sub(1), document.checked_sub(1)) {
            (Some(remaining), Some(document_remaining)) => {
                self.remaining_nodes = remaining;
                self.budget.document.set(document_remaining);
                true
            }
            (per_test, _) => {
                if !self.overflowed {
                    self.overflow_span = self.reference_spans.last().copied();
                    // A charge that the per-test budget already refuses says
                    // nothing about the document total, so only an otherwise
                    // admissible charge attributes the refusal to the document.
                    self.document_exhausted = per_test.is_some();
                }
                self.overflowed = true;
                false
            }
        }
    }
}

/// Resolves one parsed token test against the accumulated catalog.
///
/// Expansion is by substitution rather than by a pre-resolved table because a
/// test body is a tree, not a value: two tests may share a referent without
/// either being a prefix of the other, and the definition graph's own
/// `validate_acyclic` runs over `DefinitionId`s that a test does not have.
///
/// A refused reference resolves to `Or([])`, the empty disjunction: nothing
/// matches it, which is the conservative answer for a compile that a diagnostic
/// has already stopped.
fn resolve_test(
    test: &ParsedTest,
    ctx: &BindCtx<'_>,
    resolution: &mut TestResolution,
    uses: &mut Uses,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> ResolvedTest {
    if !resolution.admit_node() {
        return ResolvedTest::Or(Vec::new());
    }
    match test {
        ParsedTest::Attr(parsed) => {
            ResolvedTest::Atom(resolve_attr(parsed, ctx, uses, diagnostics))
        }
        ParsedTest::And(operands) => ResolvedTest::And(
            operands
                .iter()
                .map(|operand| resolve_test(operand, ctx, resolution, uses, diagnostics))
                .collect(),
        ),
        ParsedTest::Or(operands) => ResolvedTest::Or(
            operands
                .iter()
                .map(|operand| resolve_test(operand, ctx, resolution, uses, diagnostics))
                .collect(),
        ),
        ParsedTest::Not(inner) => ResolvedTest::Not(Box::new(resolve_test(
            inner,
            ctx,
            resolution,
            uses,
            diagnostics,
        ))),
        ParsedTest::Reference { name, span } => {
            // Recorded before resolution, for the reason a set reference is:
            // resolution is where the name stops existing.
            uses.tests.insert(name.clone());
            if resolution.visiting.iter().any(|visited| visited == name) {
                let mut chain = resolution.visiting.clone();
                chain.push(name.clone());
                let names = chain
                    .iter()
                    .map(CompactString::as_str)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::ReferenceCycle,
                    name.clone(),
                    format!("reference cycle: {names}"),
                    None,
                ));
                return ResolvedTest::Or(Vec::new());
            }
            let Some(body) = ctx.test_bodies.get(name) else {
                // Name the visibility that *is* there rather than guessing,
                // exactly as an undefined set operand does: a `@X` naming a
                // pattern, a fragment or a set is the wrong kind in the right
                // place, and saying "undefined" would send the author looking
                // for a typo.
                let message = match ctx.symbols.get(name) {
                    Some(id) => format!(
                        "`@{name}` is a {}; only a test can appear inside `[…]`",
                        ctx.visibilities[id.index()].map_or("lexicon", DefinitionVisibility::name)
                    ),
                    None if ctx.set_members.contains_key(name) => format!(
                        "`@{name}` is a set; only a test can appear inside `[…]`, and a set \
                         is an operand of `in`"
                    ),
                    None => format!("undefined test @{name}"),
                };
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::UndefinedReference,
                    name.clone(),
                    message,
                    None,
                ));
                return ResolvedTest::Or(Vec::new());
            };
            resolution.visiting.push(name.clone());
            resolution.reference_spans.push(*span);
            let resolved = resolve_test(body, ctx, resolution, uses, diagnostics);
            resolution.reference_spans.pop();
            resolution.visiting.pop();
            resolved
        }
    }
}

/// Resolves one parsed attribute test to its evaluable form.
///
/// Lifted out of `bind_expr`'s token arm unchanged when a token became a tree:
/// a leaf is reached through `&`, `|` and `!` now, and resolving a set operand
/// is the same work wherever it sits.
fn resolve_attr(
    parsed: &ParsedAttr,
    ctx: &BindCtx<'_>,
    uses: &mut Uses,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> crate::fsm::SymbolicAttrValue {
    match &parsed.value {
        ParsedAttrValue::Resolved(value) => AttrValue {
            attr: parsed.attr.clone(),
            value: value.clone(),
            complement: false,
        },
        ParsedAttrValue::Set {
            name,
            negated,
            span,
        } => {
            // Recorded before resolution, because resolution is where the name
            // stops existing: the `AttrValue` this produces holds members and
            // nothing else.
            uses.sets.insert(name.clone());
            let members = if let Some(members) = ctx.set_members.get(name) {
                members.clone()
            } else {
                // Name the visibility that is actually there. A `pattern` is no
                // more legal after `in` than a `fragment` is, so saying
                // "fragment" unconditionally would be wrong exactly half the
                // time, in the message whose whole job is to stop this looking
                // like a typo.
                let message = match ctx.symbols.get(name) {
                    Some(id) => format!(
                        "`@{name}` is a {}; only a set can follow `in`",
                        ctx.visibilities[id.index()].map_or("lexicon", DefinitionVisibility::name)
                    ),
                    None => format!("undefined set @{name}"),
                };
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::UndefinedReference,
                    name.clone(),
                    message,
                    None,
                ));
                Arc::from(Vec::<CompactString>::new())
            };
            AttrValue {
                attr: parsed.attr.clone(),
                // The negation belongs to the reference, not the declaration:
                // one list serves both polarities.
                value: if *negated {
                    AttrValueType::NotIn(members)
                } else {
                    AttrValueType::OneOf(members)
                },
                complement: false,
            }
        }
        ParsedAttrValue::SetAny {
            name,
            negated,
            span,
        } => {
            uses.sets.insert(name.clone());
            let members = if let Some(members) = ctx.set_members.get(name) {
                members.clone()
            } else {
                diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    *span,
                    DiagnosticCode::UndefinedReference,
                    name.clone(),
                    format!("undefined set @{name}"),
                    None,
                ));
                Arc::from(Vec::<CompactString>::new())
            };
            AttrValue {
                attr: parsed.attr.clone(),
                value: if *negated {
                    AttrValueType::SetNotHasAny(members)
                } else {
                    AttrValueType::SetHasAny(members)
                },
                complement: false,
            }
        }
    }
}

/// Resolves and normalizes one bracketed token test under the catalog's shared
/// substitution and branch budgets. Both expression tokens and anonymous
/// dependency targets use this boundary, so named values have one resolution
/// and accounting rule wherever a `[…]` is authored.
fn bind_token_test(
    test: &ParsedTest,
    ctx: &BindCtx<'_>,
    uses: &mut Uses,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> Box<[super::model::TokenPredicate]> {
    let mut resolution = TestResolution::new(ctx.test_budget);
    let resolved = resolve_test(test, ctx, &mut resolution, uses, diagnostics);
    if resolution.overflowed {
        // One diagnostic per catalog once the document budget is gone: every
        // remaining `[…]` overflows on its first charge, and 65,000 copies of
        // the same sentence is not a better report.
        if !ctx.test_budget.reported.replace(true) {
            let message = if resolution.document_exhausted {
                let max_document_nodes = ctx.limits.max_expansion_work();
                format!(
                    "this catalog's token tests expand to more than \
                     {max_document_nodes} nodes in total once their `test` references are \
                     substituted; every `[…]` naming a test pays that test's whole expansion \
                     again"
                )
            } else {
                let max_resolved_test_nodes = ctx.limits.max_resolved_test_nodes();
                format!(
                    "this token test expands to more than {max_resolved_test_nodes} nodes once \
                     its `test` references are substituted; a test that names another test \
                     twice doubles the expansion at each level"
                )
            };
            diagnostics.push(pending_diagnostic(
                ctx.sources,
                resolution.overflow_span.unwrap_or(ctx.definition_span),
                DiagnosticCode::TokenTestExpansionTooLarge,
                CompactString::default(),
                message,
                None,
            ));
        }
        return Box::default();
    }
    match to_dnf(&resolved, ctx.limits.max_token_test_branches()) {
        Ok(branches) => branches.into_boxed_slice(),
        Err(overflow) => {
            diagnostics.push(pending_diagnostic(
                ctx.sources,
                ctx.definition_span,
                DiagnosticCode::TokenTestTooComplex,
                CompactString::default(),
                format!(
                    "this token test distributes to {} branches; the maximum is {}. Name the \
                     repeated part with `test`, or narrow a disjunction.",
                    overflow.branches, overflow.maximum
                ),
                None,
            ));
            // No branch matches, which is the conservative answer for a
            // refused compile; the diagnostic stops execution.
            Box::default()
        }
    }
}

fn bind_expr(
    expr: ParsedExpr,
    ctx: &BindCtx<'_>,
    assigned: &mut BTreeSet<super::model::CaptureDeclId>,
    uses: &mut Uses,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> BoundExpr {
    match expr {
        ParsedExpr::Token(test) => {
            let branches = bind_token_test(&test, ctx, uses, diagnostics);
            if branches.len() == 1 {
                BoundExpr::Token(branches.into_vec().pop().expect("one branch"))
            } else {
                // More than one branch becomes an alternation of one-token
                // expressions. Sound because a token test is exactly one token
                // wide, so agreeing on a start is agreeing on the span. This is
                // the transformation `patterns/sfe.lx:39` performs by hand.
                BoundExpr::Alternation(branches.into_iter().map(BoundExpr::Token).collect())
            }
        }
        ParsedExpr::Assertion(assertion) => BoundExpr::Assertion(assertion),
        ParsedExpr::Lookaround(lookaround) => {
            let mut body_assigned = assigned.clone();
            BoundExpr::Lookaround(super::model::LookaroundExpr {
                direction: lookaround.direction,
                polarity: lookaround.polarity,
                body: Box::new(bind_expr(
                    *lookaround.body,
                    ctx,
                    &mut body_assigned,
                    uses,
                    diagnostics,
                )),
                span: lookaround.span,
            })
        }
        ParsedExpr::Sequence(expressions) => BoundExpr::Sequence(
            expressions
                .into_iter()
                .map(|expr| bind_expr(expr, ctx, assigned, uses, diagnostics))
                .collect(),
        ),
        ParsedExpr::Alternation(expressions) => {
            let incoming = assigned.clone();
            let mut branch_assignments = Vec::with_capacity(expressions.len());
            let mut bound = Vec::with_capacity(expressions.len());
            for expression in expressions {
                let mut branch = incoming.clone();
                bound.push(bind_expr(expression, ctx, &mut branch, uses, diagnostics));
                branch_assignments.push(branch);
            }
            *assigned = branch_assignments
                .into_iter()
                .reduce(|left, right| left.intersection(&right).copied().collect())
                .unwrap_or(incoming);
            BoundExpr::Alternation(bound.into_boxed_slice())
        }
        // Threaded like a sequence rather than branched like an alternation:
        // every operand of `&` matches, so a capture one of them assigns really
        // is assigned afterwards, which is exactly what `Sequence` means and
        // not what `Alternation` means.
        //
        // No accepted program can tell: `analyze` refuses a capture inside an
        // intersection outright, so `assigned` is unchanged across the loop
        // whenever the definition compiles. The arm is written for the meaning
        // rather than for the reachable case, so that relaxing that refusal
        // later does not silently arrive with alternation's scoping.
        ParsedExpr::Intersection { operands, span } => BoundExpr::Intersection {
            operands: operands
                .into_iter()
                .map(|operand| bind_expr(operand, ctx, assigned, uses, diagnostics))
                .collect(),
            span,
        },
        ParsedExpr::AnyOrder { operands, span } => {
            let incoming = assigned.clone();
            let mut newly_assigned = BTreeSet::new();
            let mut bound = Vec::with_capacity(operands.len());
            for operand in operands {
                let mut operand_assigned = incoming.clone();
                bound.push(bind_expr(
                    operand,
                    ctx,
                    &mut operand_assigned,
                    uses,
                    diagnostics,
                ));
                newly_assigned.extend(operand_assigned.difference(&incoming).copied());
            }
            assigned.extend(newly_assigned);
            BoundExpr::AnyOrder {
                operands: bound.into_boxed_slice(),
                span,
            }
        }
        ParsedExpr::Repeat { expr, min, max } => BoundExpr::Repeat {
            expr: {
                let mut body_assigned = assigned.clone();
                let body = bind_expr(*expr, ctx, &mut body_assigned, uses, diagnostics);
                if min >= 1 {
                    *assigned = body_assigned;
                }
                Box::new(body)
            },
            min,
            max,
        },
        ParsedExpr::Group(expr) => bind_expr(*expr, ctx, assigned, uses, diagnostics),
        ParsedExpr::NamedGroup {
            name, expr, span, ..
        } => {
            let (declaration, _) = ctx.declarations[&name];
            let expr = bind_expr(*expr, ctx, assigned, uses, diagnostics);
            assigned.insert(declaration);
            BoundExpr::Capture {
                declaration,
                name,
                expr: Box::new(expr),
                span,
            }
        }
        ParsedExpr::SameAs {
            name,
            projection,
            span,
        } => {
            let declaration = ctx.declarations.get(&name).map(|(id, _)| *id);
            match declaration {
                None => diagnostics.push(pending_diagnostic(
                    ctx.sources,
                    span,
                    if ctx.global_capture_names.contains(&name) {
                        DiagnosticCode::CaptureOutOfScope
                    } else {
                        DiagnosticCode::UndefinedCapture
                    },
                    name.clone(),
                    format!("undefined capture ${name}"),
                    None,
                )),
                Some(id) if !assigned.contains(&id) => {
                    let (_, declaration_span) = ctx.declarations[&name];
                    let code = if declaration_span.byte_range().start > span.byte_range().start {
                        DiagnosticCode::ForwardCaptureReference
                    } else {
                        DiagnosticCode::CaptureNotDefinitelyAssigned
                    };
                    diagnostics.push(pending_diagnostic(
                        ctx.sources,
                        span,
                        code,
                        name.clone(),
                        format!("capture ${name} is not definitely assigned here"),
                        Some((declaration_span, "capture declaration is here")),
                    ));
                }
                Some(_) => {}
            }
            BoundExpr::SameAs {
                declaration: declaration.unwrap_or(super::model::CaptureDeclId::new(u32::MAX)),
                projection,
                span,
            }
        }
        ParsedExpr::Reference { name, span } => BoundExpr::Definition {
            id: *ctx
                .symbols
                .get(&name)
                .expect("references were checked before bound allocation"),
            span,
        },
    }
}

fn validate_nullability(
    bound: BoundProgram,
    finish_order: &[DefinitionId],
    definition_spans: &[SourceSpan],
) -> Result<BoundProgram, CompileError> {
    let mut definition_nullable = vec![false; bound.definitions.len()];
    for id in finish_order {
        if let Some(expr) = bound.definitions[id.index()].expr() {
            definition_nullable[id.index()] = expr_is_nullable(expr, &definition_nullable);
        }
    }

    let mut diagnostics = Vec::new();
    for definition in &bound.definitions {
        if definition.is_ordinary_pattern() && definition_nullable[definition.id.index()] {
            diagnostics.push(pending_diagnostic(
                &bound.sources,
                definition_spans[definition.id.index()],
                DiagnosticCode::NullablePattern,
                definition.name.clone(),
                "emitting patterns must consume at least one token",
                None,
            ));
        }
        if let Some(expr) = definition.expr() {
            collect_nullable_repeat_diagnostics(
                expr,
                definition.id,
                &definition_nullable,
                &bound.definitions,
                definition_spans,
                &bound.sources,
                &mut diagnostics,
            );
        }
    }

    if diagnostics.is_empty() {
        Ok(bound)
    } else {
        Err(semantic_error(diagnostics))
    }
}

/// Whether each definition's body can match the empty span, indexed by
/// [`DefinitionId`].
///
/// Recomputed rather than retained: `validate_nullability` builds this table
/// from the DFS finish order it already has and then drops it with the rest of
/// binding's scratch state, and threading it out would make every consumer of
/// [`BoundProgram`] carry a second value that is derivable from the first. The
/// memoized walk here needs no finish order because `bind` has already rejected
/// every reference cycle, so the recursion is over a DAG.
pub(crate) fn definition_nullability(bound: &BoundProgram) -> Vec<bool> {
    let mut table = vec![false; bound.definitions.len()];
    let mut resolved = vec![false; bound.definitions.len()];
    for definition in &bound.definitions {
        resolve_nullability(definition.id, bound, &mut table, &mut resolved);
    }
    table
}

fn resolve_nullability(
    id: DefinitionId,
    bound: &BoundProgram,
    table: &mut [bool],
    resolved: &mut [bool],
) {
    if resolved[id.index()] {
        return;
    }
    // Marked before recursing so a cycle would terminate rather than overflow.
    // `bind` rejects cycles, so this can only be reached on a DAG; the guard is
    // here to keep that a local property of this function.
    resolved[id.index()] = true;
    let mut referenced = BTreeSet::new();
    if let Some(expr) = bound.definitions[id.index()].expr() {
        super::collect_references(expr, &mut referenced);
    }
    for target in referenced {
        resolve_nullability(target, bound, table, resolved);
    }
    table[id.index()] = bound.definitions[id.index()]
        .expr()
        .is_some_and(|expr| expr_is_nullable(expr, table));
}

fn expr_is_nullable(expr: &BoundExpr, definition_nullable: &[bool]) -> bool {
    match expr {
        BoundExpr::Token(_) => false,
        BoundExpr::Assertion(_) => true,
        BoundExpr::Lookaround(_) => true,
        BoundExpr::Sequence(expressions) => expressions
            .iter()
            .all(|expr| expr_is_nullable(expr, definition_nullable)),
        BoundExpr::Alternation(expressions) => expressions
            .iter()
            .any(|expr| expr_is_nullable(expr, definition_nullable)),
        // `nullable(A & B) = nullable(A) ∧ nullable(B)`: the empty span is in
        // the intersection exactly when it is in every operand. Sound in the
        // direction that matters — it never calls an expression nullable that
        // is not — and incomplete in the other, since two operands can each
        // accept the empty span for reasons that also make the intersection
        // non-empty. Incompleteness here only ever refuses a repeat that would
        // have been legal.
        BoundExpr::Intersection { operands, .. } => operands
            .iter()
            .all(|expr| expr_is_nullable(expr, definition_nullable)),
        BoundExpr::AnyOrder { .. } => false,
        BoundExpr::Repeat { expr, min, .. } => {
            *min == 0 || expr_is_nullable(expr, definition_nullable)
        }
        BoundExpr::Capture { expr, .. } => expr_is_nullable(expr, definition_nullable),
        BoundExpr::SameAs { .. } => false,
        BoundExpr::Definition { id, .. } => definition_nullable[id.index()],
    }
}

fn collect_nullable_repeat_diagnostics(
    expr: &BoundExpr,
    owner: DefinitionId,
    definition_nullable: &[bool],
    definitions: &[BoundDefinition],
    definition_spans: &[SourceSpan],
    sources: &SourceRegistry,
    diagnostics: &mut Vec<PendingDiagnostic>,
) {
    match expr {
        BoundExpr::Token(_)
        | BoundExpr::Assertion(_)
        | BoundExpr::SameAs { .. }
        | BoundExpr::Definition { .. } => {}
        BoundExpr::Lookaround(lookaround) => collect_nullable_repeat_diagnostics(
            &lookaround.body,
            owner,
            definition_nullable,
            definitions,
            definition_spans,
            sources,
            diagnostics,
        ),
        BoundExpr::Sequence(expressions) | BoundExpr::Alternation(expressions) => {
            for expression in expressions {
                collect_nullable_repeat_diagnostics(
                    expression,
                    owner,
                    definition_nullable,
                    definitions,
                    definition_spans,
                    sources,
                    diagnostics,
                );
            }
        }
        BoundExpr::Intersection { operands, .. } | BoundExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_nullable_repeat_diagnostics(
                    operand,
                    owner,
                    definition_nullable,
                    definitions,
                    definition_spans,
                    sources,
                    diagnostics,
                );
            }
        }
        BoundExpr::Repeat { expr, max, .. } => {
            if expr_is_nullable(expr, definition_nullable) {
                let definition = &definitions[owner.index()];
                diagnostics.push(pending_diagnostic(
                    sources,
                    definition_spans[owner.index()],
                    if max.is_none() {
                        DiagnosticCode::NullableUnboundedRepeat
                    } else {
                        DiagnosticCode::NullableRepeat
                    },
                    definition.name.clone(),
                    "repetition requires a non-nullable expression",
                    None,
                ));
            }
            collect_nullable_repeat_diagnostics(
                expr,
                owner,
                definition_nullable,
                definitions,
                definition_spans,
                sources,
                diagnostics,
            );
        }
        BoundExpr::Capture { expr, .. } => collect_nullable_repeat_diagnostics(
            expr,
            owner,
            definition_nullable,
            definitions,
            definition_spans,
            sources,
            diagnostics,
        ),
    }
}

/// Reports one name declared twice, naming the later declaration.
///
/// The two spans are passed unordered because the caller cannot supply them in
/// order: the set loop runs after the symbol loop whatever the source says, so
/// taking its argument order as declaration order made every set/definition
/// collision claim the definition came first — including on a catalog where
/// the `set` plainly did.
///
/// `(SourceId, start)` is the accumulated catalog's own order, not a guess:
/// `SourceRegistry::append_single` assigns ids in registration order, and
/// [`semantic_error`] already sorts by exactly this pair. So the later
/// declaration is the one the author just added and will fix, and the earlier
/// one carries the note.
fn duplicate_name(
    sources: &SourceRegistry,
    name: &CompactString,
    left: SourceSpan,
    right: SourceSpan,
) -> PendingDiagnostic {
    let order = |span: SourceSpan| (span.source(), span.start());
    let (first, second) = if order(left) <= order(right) {
        (left, right)
    } else {
        (right, left)
    };
    pending_diagnostic(
        sources,
        second,
        DiagnosticCode::DuplicateDefinition,
        name.clone(),
        format!("duplicate definition {name}"),
        Some((first, "first definition is here")),
    )
}

/// Resolves each listed `where` bracket's `@name` terms into relation labels.
/// An any-label bracket has no name to resolve and passes through unchanged.
///
/// Runs here, not in the lowerer, for the reason `[x in @S]` resolves here:
/// the scope a reference resolves against is the accumulated catalog, so a
/// bracket in one document may name a set declared in another.
///
/// Three things are reported, all of them properties of the resolved set
/// rather than of the authored text:
///
/// * a `@name` that names nothing, or names a definition or a `test` — the
///   one-referent-per-name rule makes the kind decidable, and the message
///   says which kind was found, exactly as the `in` case does;
/// * a member that is not a well-formed relation label, which is reachable
///   because a `set` names arbitrary literals (`set S := {動詞}.` is legal);
/// * a duplicate after expansion, which the lowerer cannot see because two
///   set references are opaque to it.
///
/// A failed resolution contributes no labels rather than aborting, so one
/// pass reports every bad bracket in the catalog. Binding fails on the
/// collected diagnostics before anything downstream reads the empty set.
fn bind_relations(
    relations: &[super::model::ParsedRelation],
    test_spans: &IndexMap<CompactString, SourceSpan>,
    ctx: &BindCtx<'_>,
    uses: &mut Uses,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> Box<[super::model::BoundRelation]> {
    use super::model::{BoundRelation, ParsedRelationTarget, RelationTarget};

    let mut bound = Vec::with_capacity(relations.len());
    for relation in relations {
        let target = match &relation.target {
            ParsedRelationTarget::Captured {
                polarity,
                capture,
                span,
            } => RelationTarget::Captured {
                polarity: *polarity,
                capture: capture.clone(),
                span: *span,
            },
            ParsedRelationTarget::Anonymous { polarity, span } => RelationTarget::Anonymous {
                polarity: *polarity,
                span: *span,
            },
            ParsedRelationTarget::Predicate {
                polarity,
                test,
                span,
            } => RelationTarget::Predicate {
                polarity: *polarity,
                predicates: bind_token_test(test, ctx, uses, diagnostics),
                span: *span,
            },
        };
        bound.push(BoundRelation {
            head: relation.head.clone(),
            head_span: relation.head_span,
            labels: bind_relation_labels(
                &relation.labels,
                relation.label_span,
                test_spans,
                ctx,
                uses,
                diagnostics,
            ),
            label_span: relation.label_span,
            target,
        });
    }
    bound.into_boxed_slice()
}

fn bind_relation_labels(
    source: &super::model::RelationLabels<super::model::ParsedLabelTerm>,
    label_span: SourceSpan,
    test_spans: &IndexMap<CompactString, SourceSpan>,
    ctx: &BindCtx<'_>,
    uses: &mut Uses,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> super::model::RelationLabels<super::model::RelationLabel> {
    use super::model::{ParsedLabelTerm, RelationLabel, RelationLabels};

    let RelationLabels::Listed(terms) = source else {
        return RelationLabels::Any;
    };
    let mut labels = Vec::with_capacity(terms.len());
    let push = |label: RelationLabel,
                span: SourceSpan,
                labels: &mut Vec<RelationLabel>,
                diagnostics: &mut Vec<PendingDiagnostic>| {
        if labels.contains(&label) {
            diagnostics.push(pending_diagnostic(
                ctx.sources,
                span,
                DiagnosticCode::DuplicateRelationLabel,
                label.authored(),
                format!(
                    "`{}` appears twice in one `where` label set after set expansion; a \
                     dependent carries one relation, so the repeat adds nothing",
                    label.authored()
                ),
                None,
            ));
        } else {
            labels.push(label);
        }
    };
    for term in terms {
        match term {
            ParsedLabelTerm::Label(label) => {
                push(label.clone(), label_span, &mut labels, diagnostics);
            }
            ParsedLabelTerm::SetReference { name, span } => {
                uses.sets.insert(name.clone());
                let Some(members) = ctx.set_members.get(name) else {
                    let message = match (ctx.symbols.get(name), test_spans.get(name)) {
                        (Some(id), _) => format!(
                            "`@{name}` is a {}; only a set can name `where` relation labels",
                            ctx.visibilities[id.index()]
                                .map_or("lexicon", DefinitionVisibility::name)
                        ),
                        (None, Some(_)) => format!(
                            "`@{name}` is a test; only a set can name `where` relation labels"
                        ),
                        (None, None) => format!("undefined set @{name}"),
                    };
                    diagnostics.push(pending_diagnostic(
                        ctx.sources,
                        *span,
                        DiagnosticCode::UndefinedReference,
                        name.clone(),
                        message,
                        None,
                    ));
                    continue;
                };
                for member in members.iter() {
                    match RelationLabel::parse(member) {
                        Some(label) => push(label, *span, &mut labels, diagnostics),
                        None => diagnostics.push(pending_diagnostic(
                            ctx.sources,
                            *span,
                            DiagnosticCode::InvalidRelationLabel,
                            name.clone(),
                            format!(
                                "`@{name}` member `{member}` is not a well-formed dependency \
                                 relation label, so the set cannot name `where` labels"
                            ),
                            None,
                        )),
                    }
                }
            }
        }
    }
    RelationLabels::Listed(labels.into_boxed_slice())
}

fn bind_enhanced_relations(
    relations: &[super::model::ParsedEnhancedRelation],
    test_spans: &IndexMap<CompactString, SourceSpan>,
    ctx: &BindCtx<'_>,
    uses: &mut Uses,
    diagnostics: &mut Vec<PendingDiagnostic>,
) -> Box<[super::model::BoundEnhancedRelation]> {
    use super::model::{BoundEnhancedEndpoint, BoundEnhancedRelation, ParsedEnhancedEndpoint};

    relations
        .iter()
        .map(|relation| {
            let bind_endpoint =
                |endpoint: &ParsedEnhancedEndpoint,
                 uses: &mut Uses,
                 diagnostics: &mut Vec<PendingDiagnostic>| {
                    match endpoint {
                        ParsedEnhancedEndpoint::Reference { name, span } => {
                            BoundEnhancedEndpoint::Reference {
                                name: name.clone(),
                                span: *span,
                            }
                        }
                        ParsedEnhancedEndpoint::Binding { name, test, span } => {
                            BoundEnhancedEndpoint::Binding {
                                name: name.clone(),
                                predicates: bind_token_test(test, ctx, uses, diagnostics),
                                span: *span,
                            }
                        }
                        ParsedEnhancedEndpoint::BasicHead { dependent, span } => {
                            BoundEnhancedEndpoint::BasicHead {
                                dependent: dependent.clone(),
                                span: *span,
                            }
                        }
                    }
                };
            let labels = bind_relation_labels(
                &relation.labels,
                relation.label_span,
                test_spans,
                ctx,
                uses,
                diagnostics,
            );
            BoundEnhancedRelation {
                polarity: relation.polarity,
                head: bind_endpoint(&relation.head, uses, diagnostics),
                labels,
                label_span: relation.label_span,
                edge: relation.edge.clone(),
                dependent: bind_endpoint(&relation.dependent, uses, diagnostics),
            }
        })
        .collect()
}

fn pending_diagnostic(
    sources: &SourceRegistry,
    span: SourceSpan,
    code: DiagnosticCode,
    referenced_name: CompactString,
    message: impl Into<String>,
    related: Option<(SourceSpan, &'static str)>,
) -> PendingDiagnostic {
    let mut diagnostic = diagnostic_at(sources, span, code, message);
    if let Some((related_span, related_message)) = related {
        diagnostic = diagnostic.with_related_location(RelatedDiagnosticLocation::new(
            location(sources, related_span),
            related_message,
        ));
    }
    PendingDiagnostic {
        source_id: span.source(),
        start: span.byte_range().start,
        code,
        referenced_name,
        diagnostic,
    }
}

fn semantic_error(mut diagnostics: Vec<PendingDiagnostic>) -> CompileError {
    diagnostics.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.referenced_name.cmp(&right.referenced_name))
    });
    CompileError {
        diagnostics: diagnostics
            .into_iter()
            .map(|pending| pending.diagnostic)
            .collect(),
    }
}

fn limit_error(
    sources: &SourceRegistry,
    span: SourceSpan,
    message: impl Into<String>,
) -> CompileError {
    CompileError::from_diagnostics(vec![diagnostic_at(
        sources,
        span,
        DiagnosticCode::LimitExceeded,
        message,
    )])
}

fn diagnostic_at(
    sources: &SourceRegistry,
    span: SourceSpan,
    code: DiagnosticCode,
    message: impl Into<String>,
) -> Diagnostic {
    let range = byte_range(span);
    let (line, column) = sources.line_column(span);
    Diagnostic::error_with_source_label(
        code,
        sources.source_label(span.source()),
        range,
        message,
        line,
        column,
    )
}

pub(super) fn location(sources: &SourceRegistry, span: SourceSpan) -> DiagnosticLocation {
    let range = byte_range(span);
    let (line, column) = sources.line_column(span);
    DiagnosticLocation::with_source_label(sources.source_label(span.source()), range, line, column)
}

fn byte_range(span: SourceSpan) -> std::ops::Range<usize> {
    let range = span.byte_range();
    usize::try_from(range.start).expect("source offsets originated as usize")
        ..usize::try_from(range.end).expect("source offsets originated as usize")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::bind;

    use super::super::diagnostic::DiagnosticCode;
    use super::super::limits::CompileLimits;
    use super::super::model::{
        BoundExpr, DefinitionId, DefinitionVisibility, Metadata, ParsedDefinition,
        ParsedDefinitionBody, ParsedExpr, ParsedProgram, ParsedTest, SourceId, SourceRegistry,
        SourceSpan,
    };

    fn metadata() -> Metadata {
        Arc::new(HashMap::new())
    }

    fn span(source: u32, start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(SourceId::new(source), start..end).expect("test span must be valid")
    }

    fn token() -> ParsedExpr {
        // The any-token predicate: an empty conjunction distributes to exactly
        // one empty branch.
        ParsedExpr::Token(ParsedTest::And(Vec::new()))
    }

    fn reference(name: &str) -> ParsedExpr {
        reference_at(name, 0, 0)
    }

    fn reference_at(name: &str, source: u32, start: usize) -> ParsedExpr {
        ParsedExpr::Reference {
            name: name.into(),
            span: span(source, start, start + 1 + name.len()),
        }
    }

    fn definition(name: &str, expr: ParsedExpr, source: u32, start: usize) -> ParsedDefinition {
        ParsedDefinition {
            name: name.into(),
            body: ParsedDefinitionBody::Pattern {
                visibility: DefinitionVisibility::Pattern,
                expr,
                relations: Box::default(),
                enhanced_relations: Box::default(),
            },
            metadata: metadata(),
            key: None,
            span: span(source, start, start + name.len()),
        }
    }

    fn fragment_definition(
        name: &str,
        expr: ParsedExpr,
        source: u32,
        start: usize,
    ) -> ParsedDefinition {
        ParsedDefinition {
            body: ParsedDefinitionBody::Pattern {
                visibility: DefinitionVisibility::Fragment,
                expr,
                relations: Box::default(),
                enhanced_relations: Box::default(),
            },
            ..definition(name, token(), source, start)
        }
    }

    fn program(definitions: Vec<ParsedDefinition>) -> ParsedProgram {
        let source_count = definitions
            .iter()
            .map(|definition| definition.span.source().value())
            .max()
            .map_or(0, |maximum| maximum + 1);
        let mut sources = SourceRegistry::default();
        let source_text = " ".repeat(1024);
        for source in 0..source_count {
            sources
                .register(&format!("source {source}"), &source_text)
                .unwrap();
        }
        ParsedProgram {
            definitions,
            sources,
            policy: None,
            analyzers: Vec::new(),
            sets: Vec::new(),
            tests: Vec::new(),
            groups: Vec::new(),
        }
    }

    #[test]
    fn binds_forward_and_backward_references_to_source_order_ids() {
        let cases = [
            program(vec![
                definition("root", reference("leaf"), 0, 0),
                definition("leaf", token(), 0, 10),
            ]),
            program(vec![
                definition("leaf", token(), 0, 0),
                definition("root", reference("leaf"), 0, 10),
            ]),
        ];

        for (case_index, program) in cases.into_iter().enumerate() {
            let bound = bind(program, &CompileLimits::default())
                .unwrap_or_else(|error| panic!("case {case_index} must bind: {error}"));

            assert_eq!(bound.definitions[0].id, DefinitionId::new(0));
            assert_eq!(bound.definitions[1].id, DefinitionId::new(1));
            let root = bound
                .definitions
                .iter()
                .find(|definition| definition.name == "root")
                .expect("root definition must be present");
            let expected_target = u32::from(case_index == 0);
            assert_eq!(
                root.expr().expect("pattern"),
                &BoundExpr::Definition {
                    id: DefinitionId::new(expected_target),
                    span: span(0, 0, 5),
                }
            );
        }
    }

    #[test]
    fn duplicate_definitions_report_every_later_span_and_relate_the_first() {
        let error = bind(
            program(vec![
                definition("same", token(), 0, 0),
                definition("same", token(), 0, 10),
                definition("same", token(), 0, 20),
            ]),
            &CompileLimits::default(),
        )
        .expect_err("duplicate names must be rejected");

        assert_eq!(error.diagnostics.len(), 2);
        for (diagnostic, primary_start) in error.diagnostics.iter().zip([10, 20]) {
            assert_eq!(diagnostic.code(), DiagnosticCode::DuplicateDefinition);
            assert_eq!(diagnostic.location().byte_range().start, primary_start);
            assert_eq!(diagnostic.related_locations().len(), 1);
            assert_eq!(
                diagnostic.related_locations()[0]
                    .location()
                    .byte_range()
                    .start,
                0
            );
        }
    }

    #[test]
    fn reports_all_undefined_references_in_deterministic_semantic_order() {
        let error = bind(
            program(vec![
                definition(
                    "first",
                    ParsedExpr::Sequence(vec![
                        reference_at("zulu", 0, 20),
                        reference_at("alpha", 0, 20),
                    ]),
                    0,
                    20,
                ),
                definition("second", reference_at("middle", 1, 0), 1, 0),
            ]),
            &CompileLimits::default(),
        )
        .expect_err("all undefined names must be rejected together");

        assert_eq!(error.diagnostics.len(), 3);
        assert!(
            error
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code() == DiagnosticCode::UndefinedReference)
        );
        assert_eq!(
            error
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            [
                "undefined reference @alpha",
                "undefined reference @zulu",
                "undefined reference @middle",
            ]
        );
    }

    #[test]
    fn direct_and_indirect_cycles_include_deterministic_paths() {
        let cases = [
            (
                program(vec![definition("self", reference("self"), 0, 0)]),
                "reference cycle: self -> self",
            ),
            (
                program(vec![
                    definition("a", reference("b"), 0, 0),
                    definition("b", reference("c"), 0, 10),
                    definition("c", reference("a"), 0, 20),
                ]),
                "reference cycle: a -> b -> c -> a",
            ),
        ];

        for (program, expected_message) in cases {
            let error = bind(program, &CompileLimits::default())
                .expect_err("cyclic definitions must be rejected");

            assert_eq!(error.diagnostics.len(), 1);
            assert_eq!(error.diagnostics[0].code(), DiagnosticCode::ReferenceCycle);
            assert_eq!(error.diagnostics[0].message, expected_message);
        }
    }

    #[test]
    fn preserves_shared_metadata_and_reused_definition_edges_without_expansion() {
        let shared_metadata = metadata();
        let root = ParsedDefinition {
            name: "root".into(),
            body: ParsedDefinitionBody::Pattern {
                visibility: DefinitionVisibility::Pattern,
                expr: ParsedExpr::Sequence(vec![reference("shared"), reference("shared")]),
                relations: Box::default(),
                enhanced_relations: Box::default(),
            },
            metadata: shared_metadata.clone(),
            key: None,
            span: span(0, 0, 4),
        };
        let shared = definition("shared", token(), 0, 10);

        let bound = bind(program(vec![root, shared]), &CompileLimits::default())
            .expect("a reused acyclic definition must bind");

        assert!(Arc::ptr_eq(
            &bound.definitions[0].metadata,
            &shared_metadata
        ));
        assert_eq!(
            bound.definitions[0].expr().expect("pattern"),
            &BoundExpr::Sequence(
                vec![
                    BoundExpr::Definition {
                        id: DefinitionId::new(1),
                        span: span(0, 0, 7),
                    },
                    BoundExpr::Definition {
                        id: DefinitionId::new(1),
                        span: span(0, 0, 7),
                    },
                ]
                .into_boxed_slice()
            )
        );
    }

    #[test]
    fn rejects_invalid_quantifier_ranges_from_typed_inputs() {
        let error = bind(
            program(vec![definition(
                "bad",
                ParsedExpr::Repeat {
                    expr: Box::new(token()),
                    min: 3,
                    max: Some(2),
                },
                0,
                0,
            )]),
            &CompileLimits::default(),
        )
        .expect_err("minimum greater than maximum must be rejected");

        assert_eq!(error.diagnostics.len(), 1);
        assert_eq!(
            error.diagnostics[0].code(),
            DiagnosticCode::InvalidQuantifier
        );
    }

    #[test]
    fn rejects_unbounded_repetition_of_directly_or_indirectly_nullable_expressions() {
        let cases = [
            program(vec![fragment_definition(
                "direct",
                ParsedExpr::Repeat {
                    expr: Box::new(ParsedExpr::Sequence(Vec::new())),
                    min: 0,
                    max: None,
                },
                0,
                0,
            )]),
            program(vec![
                fragment_definition(
                    "repeated",
                    ParsedExpr::Repeat {
                        expr: Box::new(reference("nullable")),
                        min: 1,
                        max: None,
                    },
                    0,
                    0,
                ),
                fragment_definition("nullable", ParsedExpr::Sequence(Vec::new()), 0, 20),
            ]),
        ];

        for program in cases {
            let error = bind(program, &CompileLimits::default())
                .expect_err("nullable unbounded repeats must be rejected");

            assert_eq!(error.diagnostics.len(), 1);
            assert_eq!(
                error.diagnostics[0].code(),
                DiagnosticCode::NullableUnboundedRepeat
            );
        }
    }

    #[test]
    fn enforces_definition_node_and_edge_budgets_without_partial_output() {
        let cases = [
            (
                program(vec![definition("a", token(), 0, 0)]),
                CompileLimits::default()
                    .tightened(|limits| limits.max_definitions = 0)
                    .expect("definition limit is tightened"),
                "definition",
            ),
            (
                program(vec![definition(
                    "a",
                    ParsedExpr::Sequence(vec![token(), token()]),
                    0,
                    0,
                )]),
                CompileLimits::default()
                    .tightened(|limits| limits.max_expression_nodes = 2)
                    .expect("expression-node limit is tightened"),
                "expression-node",
            ),
            (
                program(vec![
                    definition(
                        "a",
                        ParsedExpr::Sequence(vec![reference("b"), reference("b")]),
                        0,
                        0,
                    ),
                    definition("b", token(), 0, 10),
                ]),
                CompileLimits::default()
                    .tightened(|limits| limits.max_reference_edges = 1)
                    .expect("reference-edge limit is tightened"),
                "reference-edge",
            ),
        ];

        for (program, limits, budget_name) in cases {
            let error = match bind(program, &limits) {
                Ok(_) => panic!("{budget_name} budget must be atomic"),
                Err(error) => error,
            };
            assert_eq!(error.diagnostics.len(), 1, "budget: {budget_name}");
            assert_eq!(
                error.diagnostics[0].code(),
                DiagnosticCode::LimitExceeded,
                "budget: {budget_name}"
            );
            assert!(
                error.diagnostics[0].message.contains(budget_name),
                "budget: {budget_name}"
            );
        }
    }
}

#[cfg(test)]
mod set_sharing_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::super::limits::CompileLimits;
    use super::super::model::{
        BoundExpr, DefinitionVisibility, Metadata, ParsedAttr, ParsedAttrValue, ParsedDefinition,
        ParsedDefinitionBody, ParsedExpr, ParsedProgram, ParsedSet, ParsedTest, SourceId,
        SourceRegistry, SourceSpan,
    };
    use super::bind;
    use crate::fsm::AttrValueType;
    use crate::token_match::TokenAttributeIndex;

    fn span(start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(SourceId::new(0), start..end).expect("test span must be valid")
    }

    fn metadata() -> Metadata {
        Arc::new(HashMap::new())
    }

    /// A set reference site takes a handle to the declaration's list, not a copy.
    ///
    /// This is the whole point of the shared representation, and nothing
    /// observable through the public API can see it: two sites that each
    /// *copied* the members would compare equal, match identically, and report
    /// the same stats. Only pointer identity separates "shared" from "copied",
    /// so only pointer identity can pin it. Without sharing, resolving M sites
    /// against an N-member set materializes M·N strings from O(N + M) bytes of
    /// source — the amplification that let a catalog inside `max_source_bytes`
    /// allocate past the generated-owned-bytes budget before it was charged.
    #[test]
    fn every_reference_site_shares_the_declarations_one_allocation() {
        let mut sources = SourceRegistry::default();
        sources
            .register("sharing.lx", &" ".repeat(1024))
            .expect("test source registers");

        let site = |start: usize, negated: bool| {
            ParsedExpr::Token(ParsedTest::Attr(ParsedAttr {
                attr: TokenAttributeIndex::Pos1.into(),
                value: ParsedAttrValue::Set {
                    name: "S".into(),
                    negated,
                    span: span(start, start + 2),
                },
            }))
        };
        let definition = |name: &str, expr: ParsedExpr, start: usize| ParsedDefinition {
            name: name.into(),
            body: ParsedDefinitionBody::Pattern {
                visibility: DefinitionVisibility::Pattern,
                expr,
                relations: Box::default(),
                enhanced_relations: Box::default(),
            },
            metadata: metadata(),
            key: None,
            span: span(start, start + name.len()),
        };

        let bound = bind(
            ParsedProgram {
                definitions: vec![
                    definition("first", site(100, false), 10),
                    // The opposite polarity, because one list serves both and
                    // the negation belongs to the reference.
                    definition("second", site(200, true), 20),
                ],
                sources,
                policy: None,
                analyzers: Vec::new(),
                tests: Vec::new(),
                sets: vec![ParsedSet {
                    name: "S".into(),
                    members: vec!["a".into(), "b".into(), "c".into()],
                    span: span(0, 3),
                }],
                groups: Vec::new(),
            },
            &CompileLimits::default(),
        )
        .expect("a two-site catalog binds");

        let members = |expr: &BoundExpr| match expr {
            BoundExpr::Token(attrs) => match &attrs[0].value {
                AttrValueType::OneOf(values) | AttrValueType::NotIn(values) => Arc::clone(values),
                other => panic!("expected a membership list, got {other:?}"),
            },
            other => panic!("expected a token predicate, got {other:?}"),
        };

        let first = members(bound.definitions[0].expr().expect("pattern"));
        let second = members(bound.definitions[1].expr().expect("pattern"));
        assert_eq!(&*first, &*second, "both sites resolve to the same members");
        assert!(
            Arc::ptr_eq(&first, &second),
            "both sites must share one allocation, not hold equal copies"
        );
    }
}
