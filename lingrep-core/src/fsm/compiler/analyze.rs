use std::collections::{BTreeMap, BTreeSet};

use super::diagnostic::{CompileError, Diagnostic, DiagnosticCode};
use super::model::{
    CaptureId, ExecutionCapability, ExecutionDefinition, ExecutionEnhancedEndpoint, ExecutionExpr,
    ExecutionRelation, RelationLabel, RelationLabels, RelationPolarity, RelationTarget,
    SourceRegistry, SourceSpan,
};

const MAX_BACKREFERENCED_WIDTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExprWidth {
    pub(crate) min: usize,
    pub(crate) max: Option<usize>,
}

pub(crate) fn analyze_program(
    definitions: &mut [ExecutionDefinition],
    sources: &SourceRegistry,
) -> Result<(), CompileError> {
    for definition in definitions {
        // Over the root, not only over capture bodies. `collect_capture_widths`
        // reaches `width` only beneath a `Capture`, so an intersection written
        // outside one would never have its operand widths compared, so the
        // provably-empty intersection check would silently never run.
        width(&definition.expr, sources)?;
        let mut capture_widths = BTreeMap::new();
        collect_capture_widths(&definition.expr, sources, &mut capture_widths)?;
        let mut referenced = BTreeSet::new();
        let mut diagnostics = Vec::new();
        inspect_intersection_operands(&definition.expr, false, sources, &mut diagnostics);
        inspect_any_order_operands(&definition.expr, sources, &mut diagnostics)?;
        inspect_same_as(
            &definition.expr,
            false,
            &capture_widths,
            &mut referenced,
            sources,
            &mut diagnostics,
        );
        if referenced.len() > 1
            && let Some(span) = first_same_as_span(&definition.expr)
        {
            diagnostics.push(diagnostic(
                sources,
                span,
                DiagnosticCode::MultipleBackreferencedCaptures,
                "one definition may use only one capture identity for span equality",
            ));
        }
        // An edge endpoint is one word: `dep.head` designates a single token,
        // so a wider or variable-width operand has no head ordinal to compare
        // against. Checked here because widths exist here; the operand's own
        // span carries the refusal.
        for relation in &definition.relations {
            let mut operands = vec![(relation.head, relation.head_span)];
            if let RelationTarget::Captured { capture, span, .. } = &relation.target {
                operands.push((*capture, *span));
            }
            for (capture, span) in operands {
                let exact_one = capture_widths
                    .get(&capture)
                    .is_some_and(|width| width.min == 1 && width.max == Some(1));
                if !exact_one {
                    diagnostics.push(diagnostic(
                        sources,
                        span,
                        DiagnosticCode::WhereCaptureNotSingleToken,
                        "a `where` operand must capture statically exactly one \
                         token: a dependency edge endpoint is a single word",
                    ));
                }
            }
        }
        for relation in &definition.enhanced_relations {
            for endpoint in [&relation.head, &relation.dependent] {
                let (capture, span) = match endpoint {
                    ExecutionEnhancedEndpoint::Surface(capture) => (*capture, definition.span),
                    ExecutionEnhancedEndpoint::BasicHead(dependent) => {
                        (*dependent, definition.span)
                    }
                    ExecutionEnhancedEndpoint::Graph { .. } => continue,
                };
                let exact_one = capture_widths
                    .get(&capture)
                    .is_some_and(|width| width.min == 1 && width.max == Some(1));
                if !exact_one {
                    diagnostics.push(diagnostic(
                        sources,
                        span,
                        DiagnosticCode::WhereCaptureNotSingleToken,
                        "an enhanced graph surface or `basic_head` endpoint must derive from a capture that is statically exactly one token",
                    ));
                }
            }
        }
        for (index, relation) in definition.relations.iter().enumerate() {
            for other in &definition.relations[index + 1..] {
                let (
                    RelationTarget::Captured {
                        capture: dependent,
                        polarity,
                        ..
                    },
                    RelationTarget::Captured {
                        capture: other_dependent,
                        polarity: other_polarity,
                        ..
                    },
                ) = (&relation.target, &other.target)
                else {
                    continue;
                };
                if other_dependent != dependent {
                    continue;
                }
                let intersects = relation_labels_intersect(&relation.labels, &other.labels);
                if *polarity == RelationPolarity::Positive
                    && *other_polarity == RelationPolarity::Positive
                    && !intersects
                {
                    diagnostics.push(diagnostic(
                        sources,
                        other.label_span,
                        DiagnosticCode::WhereContradictoryLabels,
                        "two `where` constraints share a dependent capture with disjoint \
                         label sets; a token carries exactly one `dep.rel`, so both can \
                         never hold and this pattern could never match — write one \
                         constraint with `-[a|b]->` if either label will do",
                    ));
                }
                let contradictory = relation.head == other.head
                    && match (polarity, other_polarity) {
                        (RelationPolarity::Positive, RelationPolarity::Negative) => {
                            relation_labels_subset(&relation.labels, &other.labels)
                        }
                        (RelationPolarity::Negative, RelationPolarity::Positive) => {
                            relation_labels_subset(&other.labels, &relation.labels)
                        }
                        _ => false,
                    };
                if contradictory {
                    diagnostics.push(diagnostic(
                        sources,
                        other.label_span,
                        DiagnosticCode::WhereContradictoryExistence,
                        "a negative basic edge covers every label admitted by a positive \
                         edge between the same captured endpoints",
                    ));
                }
            }
        }
        // Anonymous negative existence is a conjunction-wide exclusion. It is
        // contradictory only when it covers every label admitted by a
        // positive constraint on the same head; mere overlap still allows the
        // positive witness to use a label outside the exclusion.
        for (index, relation) in definition.relations.iter().enumerate() {
            for other in &definition.relations[index + 1..] {
                if relation.head != other.head {
                    continue;
                }
                let relation_polarity = relation_existence_polarity(relation);
                let other_polarity = relation_existence_polarity(other);
                let contradictory = if relation_polarity == Some(RelationPolarity::Negative)
                    && other_polarity == Some(RelationPolarity::Positive)
                {
                    relation_labels_subset(&other.labels, &relation.labels)
                } else if relation_polarity == Some(RelationPolarity::Positive)
                    && other_polarity == Some(RelationPolarity::Negative)
                {
                    relation_labels_subset(&relation.labels, &other.labels)
                } else {
                    false
                };
                if contradictory {
                    diagnostics.push(diagnostic(
                        sources,
                        other.label_span,
                        DiagnosticCode::WhereContradictoryExistence,
                        "a negative `where` constraint on this head covers every label \
                         admitted by a positive constraint, so no complete dependency \
                         observation can satisfy both",
                    ));
                }
            }
        }
        for (index, relation) in definition.enhanced_relations.iter().enumerate() {
            for other in &definition.enhanced_relations[index + 1..] {
                if relation.head != other.head || relation.dependent != other.dependent {
                    continue;
                }
                let contradictory = if relation.polarity == RelationPolarity::Negative
                    && other.polarity == RelationPolarity::Positive
                {
                    relation_labels_subset(&other.labels, &relation.labels)
                } else if relation.polarity == RelationPolarity::Positive
                    && other.polarity == RelationPolarity::Negative
                {
                    relation_labels_subset(&relation.labels, &other.labels)
                } else {
                    false
                };
                if contradictory {
                    diagnostics.push(diagnostic(
                        sources,
                        definition.span,
                        DiagnosticCode::WhereContradictoryExistence,
                        "a negative enhanced edge covers every label admitted by a positive \
                         edge between the same endpoints, so no complete enhanced observation \
                         can satisfy both",
                    ));
                }
            }
        }
        // A clause is a conjunction, so every capture it names must be bound
        // in one match. Two operands in different branches of a single-entry
        // alternation never are, and `relations_hold` reads an unbound slot as
        // false — so the definition compiles, binds, and matches nothing with
        // no diagnostic anywhere. The `WhereSelfEdge` defect class again.
        //
        // Checked across the whole clause rather than per constraint:
        // `$c -[x]-> $a, $c -[y]-> $b` fails for exactly the reason
        // `$a -[x]-> $b` does.
        if !definition.relations.is_empty() || !definition.enhanced_relations.is_empty() {
            let guards = branch_guards(&definition.expr);
            let mut operands: Vec<(CaptureId, SourceSpan)> = Vec::new();
            for relation in &definition.relations {
                operands.push((relation.head, relation.head_span));
                if let RelationTarget::Captured { capture, span, .. } = &relation.target {
                    operands.push((*capture, *span));
                }
            }
            for relation in &definition.enhanced_relations {
                for endpoint in [&relation.head, &relation.dependent] {
                    match endpoint {
                        ExecutionEnhancedEndpoint::Surface(capture) => {
                            operands.push((*capture, definition.span));
                        }
                        ExecutionEnhancedEndpoint::BasicHead(dependent) => {
                            operands.push((*dependent, definition.span));
                        }
                        ExecutionEnhancedEndpoint::Graph { .. } => {}
                    }
                }
            }
            for (index, (capture, _)) in operands.iter().enumerate() {
                for (other, span) in &operands[index + 1..] {
                    if !provably_exclusive(guards.get(capture), guards.get(other)) {
                        continue;
                    }
                    diagnostics.push(diagnostic(
                        sources,
                        *span,
                        DiagnosticCode::WhereExclusiveCaptures,
                        "two `where` operands are in different branches of one \
                         alternation, so no match can bind both and this pattern \
                         could never match — a clause needs every operand it names \
                         bound in the same match",
                    ));
                }
            }
        }
        if !diagnostics.is_empty() {
            return Err(CompileError::from_diagnostics(diagnostics));
        }
        definition.capability = if !referenced.is_empty() {
            ExecutionCapability::SpanEquality
        } else if contains_intersection(&definition.expr) {
            // Above `Captures`, because the two are not comparable amounts of
            // the same thing: a capture-bearing definition is *recognized* by
            // the dense machine and replayed only to recover bindings, while an
            // intersection-bearing one cannot be recognized there at all.
            // Below `SpanEquality` only because that variant already routes the
            // whole definition to the evaluator that now handles both.
            ExecutionCapability::Intersection
        } else if contains_any_order(&definition.expr) {
            ExecutionCapability::AnyOrder
        } else if !definition.relations.is_empty() || !definition.enhanced_relations.is_empty() {
            // Also above `Captures`, for the ordering reason the capability's
            // own doc states: the relation filter must see every candidate
            // span, and the dense sink keeps only the longest per start before
            // captures exist. A definition that also uses `same_as` or `&`
            // keeps that capability — the routing answer is the same, and the
            // program applies the constraints wherever candidates are
            // enumerated.
            ExecutionCapability::Relations
        } else if contains_capture(&definition.expr) {
            ExecutionCapability::Captures
        } else {
            ExecutionCapability::Plain
        };
    }
    Ok(())
}

fn relation_labels_intersect(
    left: &RelationLabels<RelationLabel>,
    right: &RelationLabels<RelationLabel>,
) -> bool {
    match (left, right) {
        (RelationLabels::Any, _) | (_, RelationLabels::Any) => true,
        (RelationLabels::Listed(left), RelationLabels::Listed(right)) => left.iter().any(|left| {
            right
                .iter()
                .any(|right| relation_label_intersects(left, right))
        }),
    }
}

fn relation_label_intersects(left: &RelationLabel, right: &RelationLabel) -> bool {
    match (left, right) {
        (RelationLabel::Exact(left), RelationLabel::Exact(right)) => left == right,
        (RelationLabel::Exact(exact), RelationLabel::SubtypeOf(_)) => right.matches(exact),
        (RelationLabel::SubtypeOf(_), RelationLabel::Exact(exact)) => left.matches(exact),
        (RelationLabel::SubtypeOf(left), RelationLabel::SubtypeOf(right)) => left == right,
    }
}

fn relation_labels_subset(
    subset: &RelationLabels<RelationLabel>,
    superset: &RelationLabels<RelationLabel>,
) -> bool {
    match (subset, superset) {
        (_, RelationLabels::Any) => true,
        (RelationLabels::Any, RelationLabels::Listed(_)) => false,
        (RelationLabels::Listed(subset), RelationLabels::Listed(superset)) => {
            subset.iter().all(|candidate| {
                superset
                    .iter()
                    .any(|cover| relation_label_subset(candidate, cover))
            })
        }
    }
}

fn relation_label_subset(subset: &RelationLabel, superset: &RelationLabel) -> bool {
    match (subset, superset) {
        (RelationLabel::Exact(left), RelationLabel::Exact(right)) => left == right,
        (RelationLabel::Exact(exact), RelationLabel::SubtypeOf(_)) => superset.matches(exact),
        (RelationLabel::SubtypeOf(left), RelationLabel::SubtypeOf(right)) => left == right,
        (RelationLabel::SubtypeOf(_), RelationLabel::Exact(_)) => false,
    }
}

fn relation_existence_polarity(relation: &ExecutionRelation) -> Option<RelationPolarity> {
    match &relation.target {
        RelationTarget::Captured {
            polarity: RelationPolarity::Positive,
            ..
        } => Some(RelationPolarity::Positive),
        RelationTarget::Captured {
            polarity: RelationPolarity::Negative,
            ..
        } => None,
        RelationTarget::Anonymous { polarity, .. } => Some(*polarity),
        RelationTarget::Predicate {
            polarity: RelationPolarity::Positive,
            ..
        } => Some(RelationPolarity::Positive),
        RelationTarget::Predicate {
            polarity: RelationPolarity::Negative,
            ..
        } => None,
    }
}

/// One alternation branch a capture sits inside, as `(alternation, branch)`.
///
/// The alternation is identified by a pre-order ordinal rather than by
/// pointer, so two structurally identical alternations in one definition stay
/// distinct.
type BranchGuard = (usize, usize);

/// Where each capture sits, as the single-entry alternation branches enclosing
/// it.
///
/// **Single-entry** is the analysis. An alternation reached at most once per
/// derivation takes exactly one branch, so a capture in another branch is
/// never assigned. An alternation under a repetition that can run twice is
/// reached once per iteration, so two iterations legally bind two branches'
/// captures — measured, not assumed: `((?<a> …) | (?<b> …))+ … where $c -[…]->
/// $a, $a -[…]-> $b` matches five times on the committed CoNLL-U witnesses,
/// and a rule keyed on branch membership alone would delete it.
///
/// Such an alternation therefore contributes no guard at all, which makes the
/// refusal below sound by construction: a guard exists only where the branch
/// choice is binding.
///
/// `Intersection` needs no arm of its own beyond the structural walk, because
/// [`inspect_intersection_operands`] refuses a capture inside `&`.
fn branch_guards(expr: &ExecutionExpr) -> BTreeMap<CaptureId, Vec<BranchGuard>> {
    fn walk(
        expr: &ExecutionExpr,
        reentrant: bool,
        guards: &mut Vec<BranchGuard>,
        next_alternation: &mut usize,
        out: &mut BTreeMap<CaptureId, Vec<BranchGuard>>,
    ) {
        match expr {
            ExecutionExpr::Alternation(branches) => {
                let ordinal = *next_alternation;
                *next_alternation += 1;
                for (index, branch) in branches.iter().enumerate() {
                    if reentrant {
                        walk(branch, reentrant, guards, next_alternation, out);
                        continue;
                    }
                    guards.push((ordinal, index));
                    walk(branch, reentrant, guards, next_alternation, out);
                    guards.pop();
                }
            }
            ExecutionExpr::Repeat { expr, max, .. } => walk(
                expr,
                // `{n,1}` and `?` enter their body once, so a branch chosen
                // inside one is still chosen exactly once.
                reentrant || *max != Some(1),
                guards,
                next_alternation,
                out,
            ),
            ExecutionExpr::Capture { id, expr, .. } => {
                // Recorded before descending, so a nested capture inherits its
                // parent's guards and the two are never exclusive with each
                // other — which is what keeps `(?<a> (?<b> []))` legal as two
                // operands naming one token.
                out.insert(*id, guards.clone());
                walk(expr, reentrant, guards, next_alternation, out);
            }
            ExecutionExpr::Sequence(children) => {
                for child in children {
                    walk(child, reentrant, guards, next_alternation, out);
                }
            }
            ExecutionExpr::Intersection { operands, .. }
            | ExecutionExpr::AnyOrder { operands, .. } => {
                for operand in operands {
                    walk(operand, reentrant, guards, next_alternation, out);
                }
            }
            ExecutionExpr::Token(_)
            | ExecutionExpr::Assertion(_)
            | ExecutionExpr::SameAs { .. } => {}
        }
    }

    let mut out = BTreeMap::new();
    walk(expr, false, &mut Vec::new(), &mut 0, &mut out);
    out
}

/// Whether no single match can bind both captures.
///
/// True when one single-entry alternation holds them in different branches.
/// An absent guard list is a capture the walk never reached, which
/// `WhereCaptureUndefined` has already reported; answering `false` keeps this
/// refusal from adding a second diagnostic for one mistake.
fn provably_exclusive(left: Option<&Vec<BranchGuard>>, right: Option<&Vec<BranchGuard>>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    left.iter().any(|(alternation, branch)| {
        right
            .iter()
            .any(|(other, taken)| other == alternation && taken != branch)
    })
}

/// The token-count interval an expression can consume.
///
/// Takes the registry because one arm reports an intersection whose operands
/// admit no common width as a compile error rather than a pattern that quietly
/// never matches.
pub(crate) fn width(
    expr: &ExecutionExpr,
    sources: &SourceRegistry,
) -> Result<ExprWidth, CompileError> {
    Ok(match expr {
        ExecutionExpr::Token(_) | ExecutionExpr::SameAs { .. } => ExprWidth {
            min: 1,
            max: Some(1),
        },
        ExecutionExpr::Assertion(_) => ExprWidth {
            min: 0,
            max: Some(0),
        },
        ExecutionExpr::Capture { expr, .. } => width(expr, sources)?,
        ExecutionExpr::AnyOrder { operands, .. } => {
            let mut min = 0usize;
            for operand in operands {
                min = min
                    .checked_add(width(operand, sources)?.min)
                    .ok_or_else(width_overflow)?;
            }
            ExprWidth { min, max: None }
        }
        // The tightest interval every operand admits. An unbounded operand
        // bounds nothing above, so it contributes no maximum rather than
        // erasing another operand's.
        //
        // The emptiness test is sound but deliberately incomplete: disjoint
        // intervals prove there is no span both operands could match, while
        // overlapping intervals prove nothing at all — two operands can agree
        // on every width and still share no string. Refusing more than this
        // would need language emptiness, which is not a width question.
        ExecutionExpr::Intersection { operands, span } => {
            let mut result = ExprWidth { min: 0, max: None };
            for operand in operands {
                let child = width(operand, sources)?;
                result.min = result.min.max(child.min);
                result.max = match (result.max, child.max) {
                    (Some(held), Some(next)) => Some(held.min(next)),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                };
            }
            if let Some(max) = result.max
                && max < result.min
            {
                return Err(CompileError::from_diagnostics(vec![diagnostic(
                    sources,
                    *span,
                    DiagnosticCode::EmptyIntersection,
                    &format!(
                        "this intersection can never match: its operands need spans of at least \
                         {} and at most {} tokens, and no span is both",
                        result.min, max
                    ),
                )]));
            }
            result
        }
        ExecutionExpr::Sequence(expressions) => {
            let mut result = ExprWidth {
                min: 0,
                max: Some(0),
            };
            for expression in expressions {
                let child = width(expression, sources)?;
                result.min = result
                    .min
                    .checked_add(child.min)
                    .ok_or_else(width_overflow)?;
                result.max = match (result.max, child.max) {
                    (Some(left), Some(right)) => {
                        Some(left.checked_add(right).ok_or_else(width_overflow)?)
                    }
                    _ => None,
                };
            }
            result
        }
        ExecutionExpr::Alternation(expressions) => {
            let mut iter = expressions.iter();
            let Some(first) = iter.next() else {
                return Ok(ExprWidth {
                    min: 0,
                    max: Some(0),
                });
            };
            let mut result = width(first, sources)?;
            for expression in iter {
                let child = width(expression, sources)?;
                result.min = result.min.min(child.min);
                result.max = match (result.max, child.max) {
                    (Some(left), Some(right)) => Some(left.max(right)),
                    _ => None,
                };
            }
            result
        }
        ExecutionExpr::Repeat { expr, min, max } => {
            let child = width(expr, sources)?;
            ExprWidth {
                min: child.min.checked_mul(*min).ok_or_else(width_overflow)?,
                max: match (child.max, max) {
                    (Some(width), Some(count)) => {
                        Some(width.checked_mul(*count).ok_or_else(width_overflow)?)
                    }
                    (Some(0), None) => Some(0),
                    _ => None,
                },
            }
        }
    })
}

fn collect_capture_widths(
    expr: &ExecutionExpr,
    sources: &SourceRegistry,
    widths: &mut BTreeMap<CaptureId, ExprWidth>,
) -> Result<(), CompileError> {
    match expr {
        ExecutionExpr::Capture { id, expr, .. } => {
            widths.insert(*id, width(expr, sources)?);
            collect_capture_widths(expr, sources, widths)?;
        }
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::AnyOrder {
            operands: children, ..
        } => {
            for child in children {
                collect_capture_widths(child, sources, widths)?;
            }
        }
        ExecutionExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_capture_widths(operand, sources, widths)?;
            }
        }
        ExecutionExpr::Repeat { expr, .. } => collect_capture_widths(expr, sources, widths)?,
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {}
    }
    Ok(())
}

fn inspect_any_order_operands(
    expr: &ExecutionExpr,
    sources: &SourceRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), CompileError> {
    match expr {
        ExecutionExpr::AnyOrder { operands, span } => {
            for operand in operands {
                if let ExecutionExpr::Capture {
                    expr: body,
                    span: capture_span,
                    ..
                } = operand
                {
                    let operand_width = width(body, sources)?;
                    if operand_width.min != 1 || operand_width.max != Some(1) {
                        diagnostics.push(diagnostic(
                            sources,
                            *capture_span,
                            DiagnosticCode::AnyOrderOperandNotSingleToken,
                            "an `any_order` operand must capture statically exactly one token",
                        ));
                    }
                    if let Some(nested_span) = first_capture_span(body) {
                        diagnostics.push(diagnostic(
                            sources,
                            nested_span,
                            DiagnosticCode::AnyOrderNestedCapture,
                            "an `any_order` node capture may not contain another capture",
                        ));
                    }
                } else {
                    // Source-shape validation reports the authored operand.
                    // Keep this defensive branch for a future materialization
                    // rewrite that might otherwise erase that invariant.
                    diagnostics.push(diagnostic(
                        sources,
                        *span,
                        DiagnosticCode::AnyOrderOperandMustBeCapture,
                        "every `any_order` operand must materialize as a named capture",
                    ));
                }
                inspect_any_order_operands(operand, sources, diagnostics)?;
            }
        }
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::Intersection {
            operands: children, ..
        } => {
            for child in children {
                inspect_any_order_operands(child, sources, diagnostics)?;
            }
        }
        ExecutionExpr::Repeat { expr, .. } | ExecutionExpr::Capture { expr, .. } => {
            inspect_any_order_operands(expr, sources, diagnostics)?;
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {}
    }
    Ok(())
}

fn first_capture_span(expr: &ExecutionExpr) -> Option<SourceSpan> {
    match expr {
        ExecutionExpr::Capture { span, .. } => Some(*span),
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::Intersection {
            operands: children, ..
        }
        | ExecutionExpr::AnyOrder {
            operands: children, ..
        } => children.iter().find_map(first_capture_span),
        ExecutionExpr::Repeat { expr, .. } => first_capture_span(expr),
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {
            None
        }
    }
}

fn inspect_same_as(
    expr: &ExecutionExpr,
    beneath_unbounded: bool,
    widths: &BTreeMap<CaptureId, ExprWidth>,
    referenced: &mut BTreeSet<CaptureId>,
    sources: &SourceRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match expr {
        ExecutionExpr::SameAs { capture, span, .. } => {
            referenced.insert(*capture);
            let width = widths[capture];
            let failure = if width.min == 0 {
                Some((
                    DiagnosticCode::BackreferencedCaptureNullable,
                    "span-equality capture may be empty",
                ))
            } else if width.max.is_none() {
                Some((
                    DiagnosticCode::BackreferencedCaptureUnbounded,
                    "span-equality capture is unbounded",
                ))
            } else if width.max.is_some_and(|max| max > MAX_BACKREFERENCED_WIDTH) {
                Some((
                    DiagnosticCode::BackreferencedCaptureTooWide,
                    "span-equality capture exceeds eight tokens",
                ))
            } else if beneath_unbounded {
                Some((
                    DiagnosticCode::UnboundedSpanEqualityRepeat,
                    "span equality may not occur beneath unbounded repetition",
                ))
            } else {
                None
            };
            if let Some((code, message)) = failure {
                diagnostics.push(diagnostic(sources, *span, code, message));
            }
        }
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::AnyOrder {
            operands: children, ..
        } => {
            for child in children {
                inspect_same_as(
                    child,
                    beneath_unbounded,
                    widths,
                    referenced,
                    sources,
                    diagnostics,
                );
            }
        }
        ExecutionExpr::Intersection { operands, .. } => {
            for operand in operands {
                inspect_same_as(
                    operand,
                    beneath_unbounded,
                    widths,
                    referenced,
                    sources,
                    diagnostics,
                );
            }
        }
        ExecutionExpr::Repeat { expr, max, .. } => inspect_same_as(
            expr,
            beneath_unbounded || max.is_none(),
            widths,
            referenced,
            sources,
            diagnostics,
        ),
        ExecutionExpr::Capture { expr, .. } => inspect_same_as(
            expr,
            beneath_unbounded,
            widths,
            referenced,
            sources,
            diagnostics,
        ),
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) => {}
    }
}

/// Refuses a capture or a `same_as` inside an operand of `&`.
///
/// **A capture is refused in every operand, not merely in all but the first.**
/// Allowing it on one side would make `A & B` and `B & A` produce different
/// `Match` values, and `&` is commutative — so the operator would stop obeying
/// its own algebra to record which operand the author happened to write first.
/// It would also need a rule for which of several accepting paths donates the
/// slot, which nothing else in the language has to answer. Refusing both sides
/// needs no such rule, keeps commutativity exact, and can be relaxed later
/// without invalidating anything already authored.
///
/// `same_as` is refused for a second, independent reason: `deduplicate` keys on
/// `(position, referenced slot)`, and with no referenced slot that key collapses
/// to position alone, which is what bounds every candidate set by the token
/// count and keeps the evaluator polynomial.
fn inspect_intersection_operands(
    expr: &ExecutionExpr,
    inside: bool,
    sources: &SourceRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match expr {
        ExecutionExpr::Capture { expr, span, .. } => {
            if inside {
                diagnostics.push(diagnostic(
                    sources,
                    *span,
                    DiagnosticCode::CaptureInsideIntersection,
                    "a capture may not appear inside `&`: the operands are intersected as \
                     languages, not as bindings, so there is no defined answer for which \
                     operand's match the name would refer to — bind the name outside the \
                     intersection instead",
                ));
            }
            inspect_intersection_operands(expr, inside, sources, diagnostics);
        }
        ExecutionExpr::SameAs { span, .. } => {
            if inside {
                diagnostics.push(diagnostic(
                    sources,
                    *span,
                    DiagnosticCode::SpanEqualityInsideIntersection,
                    "`same_as` may not appear inside `&`",
                ));
            }
        }
        ExecutionExpr::Intersection { operands, .. } => {
            for operand in operands {
                inspect_intersection_operands(operand, true, sources, diagnostics);
            }
        }
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::AnyOrder {
            operands: children, ..
        } => {
            for child in children {
                inspect_intersection_operands(child, inside, sources, diagnostics);
            }
        }
        ExecutionExpr::Repeat { expr, .. } => {
            inspect_intersection_operands(expr, inside, sources, diagnostics);
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) => {}
    }
}

fn contains_intersection(expr: &ExecutionExpr) -> bool {
    match expr {
        ExecutionExpr::Intersection { .. } => true,
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::AnyOrder {
            operands: children, ..
        } => children.iter().any(contains_intersection),
        ExecutionExpr::Repeat { expr, .. } | ExecutionExpr::Capture { expr, .. } => {
            contains_intersection(expr)
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {
            false
        }
    }
}

fn contains_any_order(expr: &ExecutionExpr) -> bool {
    match expr {
        ExecutionExpr::AnyOrder { .. } => true,
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::Intersection {
            operands: children, ..
        } => children.iter().any(contains_any_order),
        ExecutionExpr::Repeat { expr, .. } | ExecutionExpr::Capture { expr, .. } => {
            contains_any_order(expr)
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {
            false
        }
    }
}

fn contains_capture(expr: &ExecutionExpr) -> bool {
    match expr {
        ExecutionExpr::Capture { .. } => true,
        ExecutionExpr::Sequence(children) | ExecutionExpr::Alternation(children) => {
            children.iter().any(contains_capture)
        }
        ExecutionExpr::Intersection { operands, .. } | ExecutionExpr::AnyOrder { operands, .. } => {
            operands.iter().any(contains_capture)
        }
        ExecutionExpr::Repeat { expr, .. } => contains_capture(expr),
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {
            false
        }
    }
}

fn first_same_as_span(expr: &ExecutionExpr) -> Option<SourceSpan> {
    match expr {
        ExecutionExpr::SameAs { span, .. } => Some(*span),
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Alternation(children)
        | ExecutionExpr::AnyOrder {
            operands: children, ..
        } => children.iter().find_map(first_same_as_span),
        ExecutionExpr::Intersection { operands, .. } => {
            operands.iter().find_map(first_same_as_span)
        }
        ExecutionExpr::Repeat { expr, .. } | ExecutionExpr::Capture { expr, .. } => {
            first_same_as_span(expr)
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) => None,
    }
}

fn diagnostic(
    sources: &SourceRegistry,
    span: SourceSpan,
    code: DiagnosticCode,
    message: &str,
) -> Diagnostic {
    let range = span.byte_range();
    let (line, column) = sources.line_column(span);
    Diagnostic::error_with_source_label(
        code,
        sources.source_label(span.source()),
        usize::try_from(range.start).expect("source offset fits usize")
            ..usize::try_from(range.end).expect("source offset fits usize"),
        message,
        line,
        column,
    )
}

fn width_overflow() -> CompileError {
    CompileError::new("<pattern>", "expression width overflowed")
}

#[cfg(test)]
mod relation_label_tests {
    use super::*;

    fn label(text: &str) -> RelationLabel {
        RelationLabel::parse(text).expect("test label is valid")
    }

    fn listed(labels: &[&str]) -> RelationLabels<RelationLabel> {
        RelationLabels::Listed(
            labels
                .iter()
                .map(|label_text| label(label_text))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    #[test]
    fn subtype_label_intersection_observes_exact_subtypes_but_not_bare_labels() {
        assert!(relation_labels_intersect(
            &listed(&["obl:*"]),
            &listed(&["obl:tmod"]),
        ));
        assert!(!relation_labels_intersect(
            &listed(&["obl:*"]),
            &listed(&["obl"]),
        ));
        assert!(!relation_labels_intersect(
            &listed(&["obl:*"]),
            &listed(&["nsubj:pass"]),
        ));
    }

    #[test]
    fn label_subset_is_directional_for_exact_and_family_matchers() {
        assert!(relation_labels_subset(
            &listed(&["obl:tmod"]),
            &listed(&["obl:*"]),
        ));
        assert!(!relation_labels_subset(
            &listed(&["obl:*"]),
            &listed(&["obl:tmod"]),
        ));
        assert!(relation_labels_subset(
            &listed(&["obj"]),
            &listed(&["obj", "obl"]),
        ));
        assert!(!relation_labels_subset(
            &RelationLabels::Any,
            &listed(&["obj"]),
        ));
    }
}
