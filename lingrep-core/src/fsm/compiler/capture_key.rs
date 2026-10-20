//! Validating captures referenced by policy projections.
//!
//! Extraction (in `policy`) checks only a declaration's shape: a `$` reference
//! and a real attribute name. Whether the capture it names actually exists,
//! resolves to exactly one occurrence, and covers the tokens the site can read
//! needs the *materialized* capture structure, which exists only after binding.
//! That is why this runs as its own pass rather than inside extraction.
//!
//! Two sites project off a capture and so come through here: `key:` fields and
//! `tags:` form (c). Both reject ambiguous captures rather than choosing an
//! arbitrary occurrence. The sites differ in one respect, [`WidthRule`]: `key:` has a
//! component spelling that says which reading of a wide capture is meant, and
//! `tags:` does not.
//!
//! Every check here is an **error**, not a warning, and each for the same
//! reason: the failure it catches is invisible at runtime. A value silently
//! projected off the wrong occurrence, or an arbitrary token of a wide span,
//! would look exactly like one projected off the right one.

use compact_str::CompactString;

use super::model::{
    BoundDefinition, BoundExpr, ExecutionDefinition, ExecutionExpr, ExecutionGraphBindingDecl,
    SourceRegistry,
};
use super::{Diagnostic, DiagnosticCode, RelatedDiagnosticLocation};
use crate::fsm::policy::{CatalogPolicy, KeyComponent, KeyTarget, TagSource};

pub(crate) fn match_projection_errors(
    policy: &CatalogPolicy,
    definitions: &[BoundDefinition],
    sources: &SourceRegistry,
) -> Vec<Diagnostic> {
    let kind_errors = definitions.iter().filter_map(|definition| {
        let entry = policy.get(&definition.name)?;
        if entry.key.is_empty() { return None; }
        let lexical_definition = definition.expr().is_none();
        let has_lexical_projection = entry.key.iter().any(|field| matches!(field.target, KeyTarget::LexicalTerm | KeyTarget::LexicalSegments));
        let invalid = if lexical_definition { !has_lexical_projection || entry.key.iter().any(|field| matches!(field.target, KeyTarget::Match | KeyTarget::Capture { .. })) } else { has_lexical_projection };
        invalid.then(|| {
            let key = definition.key.as_ref().expect("policy key retains its source");
            let message = if lexical_definition {
                format!("`key:` on lexicon `{}` may contain only `term` and `segments(orth|norm)` projections", definition.name)
            } else {
                format!("`key:` on ordinary pattern `{}` cannot use lexical `term` or `segments(...)` projections", definition.name)
            };
            error_at_span(sources, key.source_span, DiagnosticCode::InvalidPolicyValue, message)
        })
    });
    let width_errors = definitions
        .iter()
        .filter_map(|definition| {
            let expr = definition.expr()?;
            let entry = policy.get(&definition.name)?;
            let field = entry
                .key
                .iter()
                .find(|field| matches!(field.target, KeyTarget::Match))?;
            (bound_min_width(expr, definitions) != Some(1)
                || bound_max_width(expr, definitions) != Some(1))
            .then(|| {
                let key = definition
                    .key
                    .as_ref()
                    .expect("policy key retains its source");
                let mut diagnostic = error_at_span(
                    sources,
                    key.source_span,
                    DiagnosticCode::MultiTokenMatchProjection,
                    format!(
                        "`key:` on `{}` projects `{}` from the whole match, but that definition \
                         is not proven to consume exactly one token",
                        definition.name,
                        field.attribute.pattern_key()
                    ),
                );
                if key.inherited {
                    diagnostic = diagnostic.with_related_location(RelatedDiagnosticLocation::new(
                        location(sources, definition.span),
                        format!("definition `{}` inherits this key", definition.name),
                    ));
                }
                diagnostic
            })
        })
        .collect::<Vec<_>>();
    kind_errors.chain(width_errors).collect()
}

/// Reports every policy capture reference that cannot be projected
/// unambiguously.
pub(crate) fn key_projection_errors(
    policy: &mut CatalogPolicy,
    execution: &[ExecutionDefinition],
    sources: &SourceRegistry,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for definition in execution {
        let Some(entry) = policy.get_mut(&definition.name) else {
            continue;
        };
        for field in &mut entry.key {
            let KeyTarget::Capture { name, component } = &field.target else {
                continue;
            };
            let graph_declaration =
                definition
                    .graph_bindings
                    .iter()
                    .find(|declaration| match declaration {
                        ExecutionGraphBindingDecl::Node(candidate)
                        | ExecutionGraphBindingDecl::Edge(candidate) => candidate == name,
                    });
            if let Some(declaration) = graph_declaration {
                if *component != crate::fsm::KeyComponent::Sole {
                    diagnostics.push(error(
                        sources,
                        definition,
                        "key",
                        DiagnosticCode::InvalidPolicyValue,
                        format!(
                            "`key:` graph binding `${name}` has no token span, so it cannot use a span component"
                        ),
                    ));
                    continue;
                }
                match declaration {
                    ExecutionGraphBindingDecl::Node(_) => {
                        if !matches!(
                            field.attribute.as_str(),
                            "text" | "lemma" | "upos" | "xpos" | "node.kind" | "node.id"
                        ) {
                            diagnostics.push(error(
                                sources,
                                definition,
                                "key",
                                DiagnosticCode::InvalidPolicyValue,
                                format!(
                                    "`key:` graph node `${name}` can project node.kind, node.id, text, lemma, upos, or xpos, not `{}`",
                                    field.attribute
                                ),
                            ));
                            continue;
                        }
                        field.retarget_graph_node(name.clone());
                    }
                    ExecutionGraphBindingDecl::Edge(_) => {
                        if field.attribute.as_str() != "dep.rel" {
                            diagnostics.push(error(
                                sources,
                                definition,
                                "key",
                                DiagnosticCode::InvalidPolicyValue,
                                format!(
                                    "`key:` enhanced edge `${name}` can project only `rel`, not `{}`",
                                    field.attribute
                                ),
                            ));
                            continue;
                        }
                        field.retarget_graph_edge(name.clone());
                    }
                }
                continue;
            }
            check(
                &mut diagnostics,
                sources,
                definition,
                "key",
                &format!("`key:` on `{}`", definition.name),
                name,
                WidthRule::Component(*component),
            );
        }
        for step in &entry.transform {
            // A scope names the span an operation rewrites, so the width rule
            // is the loosest one: any capture with a token in it will do.
            let Some(name) = step.scope.as_deref() else {
                continue;
            };
            check(
                &mut diagnostics,
                sources,
                definition,
                "transform",
                &format!("`transform:` scope on `{}`", definition.name),
                name,
                WidthRule::Scope,
            );
        }
        for (tag, source) in &entry.tags {
            // Only form (c) reads a capture's *tokens*. Form (b) asks whether a
            // capture participated and carries the value the author wrote for
            // it, which is one value however many times the capture ran or how
            // wide it is; form (a) names no capture at all.
            let TagSource::Projection { capture, .. } = source else {
                continue;
            };
            check(
                &mut diagnostics,
                sources,
                definition,
                "tags",
                &format!("`tags:` field `{tag}` on `{}`", definition.name),
                capture,
                WidthRule::SingleToken,
            );
        }
    }
    diagnostics
}

/// Checks one `$capture` reference written at `site`, under metadata `key`.
///
/// `site` is the declaration named as the author wrote it, because the two
/// callers are otherwise indistinguishable in the message and telling a `tags:`
/// author about `key:` is worse than saying nothing. `key` is the metadata key
/// the reference is written inside, which is where the diagnostic points; the
/// two differ because a `tags:` site also names the tag field, and one key
/// carries every field.
fn check(
    diagnostics: &mut Vec<Diagnostic>,
    sources: &SourceRegistry,
    definition: &ExecutionDefinition,
    key: &str,
    site: &str,
    name: &str,
    rule: WidthRule,
) {
    let mut occurrences = Vec::new();
    collect(&definition.expr, name, false, &mut occurrences);
    match occurrences.as_slice() {
        // (iii) the name resolves to no capture at all.
        [] => diagnostics.push(error(
            sources,
            definition,
            key,
            DiagnosticCode::AmbiguousCaptureReference,
            format!("{site} references `${name}`, which is not a capture in this pattern"),
        )),
        [occurrence] => {
            // (ii) a single occurrence, but repeated. P3 measured that a
            // quantified capture compiles silently and retains only its last
            // iteration, so nothing in the output reveals which one was read.
            if occurrence.quantified {
                diagnostics.push(error(
                    sources,
                    definition,
                    key,
                    DiagnosticCode::AmbiguousCaptureReference,
                    format!(
                        "{site} references `${name}`, which sits under a quantifier — only the \
                         last iteration is retained, so the value would silently come from one \
                         arbitrary occurrence"
                    ),
                ));
            } else if let Some(refusal) = width_refusal(rule, occurrence) {
                diagnostics.push(error(
                    sources,
                    definition,
                    key,
                    DiagnosticCode::MultiTokenCaptureProjection,
                    format!(
                        "{site} projects an attribute off `${name}`, which can cover {} — \
                         {refusal}",
                        match occurrence.max_width {
                            None => "any number of tokens".to_owned(),
                            Some(0) => "no token".to_owned(),
                            Some(width) => format!("up to {width} tokens"),
                        }
                    ),
                ));
            }
        }
        // (i) one authored name, several expanded occurrences — the
        // double-fragment-reference form, unique only by path.
        many => diagnostics.push(error(
            sources,
            definition,
            key,
            DiagnosticCode::AmbiguousCaptureReference,
            format!(
                "{site} references `${name}`, which expands to {} captures ({}) — the reference \
                 does not say which",
                many.len(),
                many.iter()
                    .map(|occurrence| format!("`{}`", occurrence.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// How many of a capture's tokens the site that referenced it can read.
#[derive(Clone, Copy)]
enum WidthRule {
    /// `key:`, whose component says which reading of a wide capture is meant.
    Component(KeyComponent),
    /// `tags:` form (c), which reads one token and has no spelling for more.
    ///
    /// Deliberately not `Component(KeyComponent::Sole)`: the refusal must not
    /// offer `{first|last|span}`, because `tags:` does not accept them.
    SingleToken,
    /// A `transform:` `scope:`, which names a span to rewrite rather than a
    /// value to read. Any width does, so long as there is something there:
    /// a zero-width scope would make every operation in the step a no-op.
    Scope,
}

/// Why this site cannot read this occurrence, or `None` if it can.
///
/// The bare key form and a tag projection both claim exactly one token and are
/// held to it. The explicit key components waive that claim — the author has
/// said which reading they mean — and need only a token to read: an unbounded
/// capture is admissible because it is sentence-local, so its span is concrete
/// by projection time.
///
/// A zero-width capture is refused everywhere. `span` over no tokens would key
/// on the empty string, and an empty string is a value rather than an absence,
/// so it would silently merge records that share nothing.
fn width_refusal(rule: WidthRule, occurrence: &Occurrence) -> Option<&'static str> {
    match rule {
        WidthRule::Component(KeyComponent::Sole) | WidthRule::SingleToken
            if occurrence.max_width == Some(1) =>
        {
            None
        }
        WidthRule::Component(KeyComponent::Sole) => Some(
            "a bare key field needs exactly one token — write `{first: attr}`, `{last: attr}` or \
             `{span: attr}` to say which reading is meant",
        ),
        WidthRule::SingleToken => Some(
            "a tag projection reads one token, so the capture it names must cover exactly one; \
             narrow the capture, or annotate with a capture-presence mapping instead",
        ),
        WidthRule::Component(_) if occurrence.max_width == Some(0) => {
            Some("a projection needs a token to read, and this capture has none")
        }
        WidthRule::Scope if occurrence.max_width == Some(0) => Some(
            "a transform scope needs a token to rewrite, and this capture has none — every \
             operation confined to it would do nothing",
        ),
        WidthRule::Component(_) | WidthRule::Scope => None,
    }
}

struct Occurrence {
    path: CompactString,
    /// Whether any enclosing repeat can run other than exactly once.
    quantified: bool,
    /// `None` when unbounded.
    max_width: Option<usize>,
}

fn collect(expr: &ExecutionExpr, name: &str, repeated: bool, found: &mut Vec<Occurrence>) {
    match expr {
        ExecutionExpr::Capture {
            name: capture,
            path,
            expr,
            ..
        } => {
            if capture == name {
                found.push(Occurrence {
                    path: path.clone(),
                    quantified: repeated,
                    max_width: max_width(expr),
                });
            }
            // Recurse regardless: a same-named capture may nest inside.
            collect(expr, name, repeated, found);
        }
        ExecutionExpr::Sequence(parts)
        | ExecutionExpr::Alternation(parts)
        | ExecutionExpr::AnyOrder {
            operands: parts, ..
        } => {
            for part in parts {
                collect(part, name, repeated, found);
            }
        }
        // Unreachable from a definition that compiled — `analyze` refuses a
        // capture inside an intersection — and walked anyway, because a
        // walker that stops at a variant is how a later relaxation of that
        // refusal silently stops finding occurrences.
        ExecutionExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect(operand, name, repeated, found);
            }
        }
        ExecutionExpr::Repeat { expr, min, max } => {
            collect(expr, name, repeated || *min != 1 || *max != Some(1), found);
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {}
    }
}

/// The most tokens an expression can consume, or `None` when unbounded.
///
/// Deliberately an over-approximation: it never reports a width smaller than
/// the true maximum, so a body that passes really does cover one token. Being
/// wrong in the other direction would let a multi-token key through.
fn max_width(expr: &ExecutionExpr) -> Option<usize> {
    match expr {
        ExecutionExpr::Token(_) => Some(1),
        // Zero-width by construction: assertions and `same_as` consume nothing
        // in the projection sense — `same_as` is a constraint on a span that
        // some other element already consumed.
        ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => Some(0),
        ExecutionExpr::Sequence(parts) => parts
            .iter()
            .try_fold(0usize, |total, part| Some(total + max_width(part)?)),
        ExecutionExpr::Alternation(parts) => parts
            .iter()
            .try_fold(0usize, |widest, part| Some(widest.max(max_width(part)?))),
        // Gaps between operands are implicit and unbounded.
        ExecutionExpr::AnyOrder { .. } => None,
        // An over-approximation, as this function's doc requires: the true
        // maximum is the *narrowest* operand's, and taking the widest is never
        // smaller than that. Reporting the narrowest would be tighter and
        // wrong in the direction that lets a multi-token key through, because
        // an unbounded operand contributes `None` and would erase the bound.
        ExecutionExpr::Intersection { operands, .. } => {
            operands.iter().try_fold(0usize, |widest, operand| {
                Some(widest.max(max_width(operand)?))
            })
        }
        ExecutionExpr::Repeat { expr, max, .. } => max_width(expr)?.checked_mul((*max)?).or(None),
        ExecutionExpr::Capture { expr, .. } => max_width(expr),
    }
}

fn bound_max_width(expr: &BoundExpr, definitions: &[BoundDefinition]) -> Option<usize> {
    match expr {
        BoundExpr::Token(_) => Some(1),
        BoundExpr::Assertion(_) | BoundExpr::Lookaround(_) | BoundExpr::SameAs { .. } => Some(0),
        BoundExpr::Sequence(parts) => parts.iter().try_fold(0usize, |total, part| {
            total.checked_add(bound_max_width(part, definitions)?)
        }),
        BoundExpr::Alternation(parts) => parts.iter().try_fold(0usize, |widest, part| {
            Some(widest.max(bound_max_width(part, definitions)?))
        }),
        // Gaps between operands are implicit and unbounded.
        BoundExpr::AnyOrder { .. } => None,
        BoundExpr::Intersection { operands, .. } => {
            operands.iter().try_fold(0usize, |widest, part| {
                Some(widest.max(bound_max_width(part, definitions)?))
            })
        }
        BoundExpr::Repeat { expr, max, .. } => {
            bound_max_width(expr, definitions)?.checked_mul((*max)?)
        }
        BoundExpr::Capture { expr, .. } => bound_max_width(expr, definitions),
        BoundExpr::Definition { id, .. } => definitions[id.index()]
            .expr()
            .and_then(|expr| bound_max_width(expr, definitions)),
    }
}

fn bound_min_width(expr: &BoundExpr, definitions: &[BoundDefinition]) -> Option<usize> {
    match expr {
        BoundExpr::Token(_) => Some(1),
        BoundExpr::Assertion(_) | BoundExpr::Lookaround(_) | BoundExpr::SameAs { .. } => Some(0),
        BoundExpr::Sequence(parts) => parts.iter().try_fold(0usize, |total, part| {
            total.checked_add(bound_min_width(part, definitions)?)
        }),
        BoundExpr::Alternation(parts) => parts
            .iter()
            .map(|part| bound_min_width(part, definitions))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .min(),
        BoundExpr::AnyOrder { operands, .. } => operands.iter().try_fold(0usize, |total, part| {
            total.checked_add(bound_min_width(part, definitions)?)
        }),
        BoundExpr::Intersection { operands, .. } => operands
            .iter()
            .map(|part| bound_min_width(part, definitions))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max(),
        BoundExpr::Repeat { expr, min, .. } => {
            bound_min_width(expr, definitions)?.checked_mul(*min)
        }
        BoundExpr::Capture { expr, .. } => bound_min_width(expr, definitions),
        BoundExpr::Definition { id, .. } => definitions[id.index()]
            .expr()
            .and_then(|expr| bound_min_width(expr, definitions)),
    }
}

fn key_error(
    sources: &SourceRegistry,
    definition: &ExecutionDefinition,
    code: DiagnosticCode,
    message: String,
) -> Diagnostic {
    let Some(key) = &definition.key else {
        return error_at_span(sources, definition.span, code, message);
    };
    let mut diagnostic = error_at_span(sources, key.source_span, code, message);
    if key.inherited {
        diagnostic = diagnostic.with_related_location(RelatedDiagnosticLocation::new(
            location(sources, definition.span),
            format!("definition `{}` inherits this key", definition.name),
        ));
    }
    diagnostic
}

/// Reports at the metadata key that carried the offending capture reference.
///
/// These diagnostics were built at `0..0` — no span at all — because metadata
/// was an opaque line with nothing to point at. The reference is written
/// inside a `key:` or `tags:` value, so that key is where it is reported.
fn error(
    sources: &SourceRegistry,
    definition: &ExecutionDefinition,
    key: &str,
    code: DiagnosticCode,
    message: String,
) -> Diagnostic {
    if key == "key" {
        return key_error(sources, definition, code, message);
    }
    let label = sources.source_label(definition.source);
    let Some(entry) = definition.metadata.get(key) else {
        return Diagnostic::error_with_source_label(code, label, 0..0, message, 1, 1);
    };
    let (line, column) = sources.line_column(entry.span);
    let range = entry.span.byte_range();
    Diagnostic::error_with_source_label(
        code,
        label,
        usize::try_from(range.start).expect("source offsets originated as usize")
            ..usize::try_from(range.end).expect("source offsets originated as usize"),
        message,
        line,
        column,
    )
}

fn error_at_span(
    sources: &SourceRegistry,
    span: super::model::SourceSpan,
    code: DiagnosticCode,
    message: String,
) -> Diagnostic {
    let location = location(sources, span);
    Diagnostic::error_with_source_label(
        code,
        std::sync::Arc::from(location.source_name().to_owned()),
        location.byte_range(),
        message,
        location.line(),
        location.column(),
    )
}

fn location(sources: &SourceRegistry, span: super::model::SourceSpan) -> super::DiagnosticLocation {
    let (line, column) = sources.line_column(span);
    let range = span.byte_range();
    super::DiagnosticLocation::with_source_label(
        sources.source_label(span.source()),
        usize::try_from(range.start).expect("source offsets originated as usize")
            ..usize::try_from(range.end).expect("source offsets originated as usize"),
        line,
        column,
    )
}
