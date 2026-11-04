//! Checks a catalog's attribute literals against its `labels:` declarations.

use std::collections::BTreeSet;

use compact_str::CompactString;

use super::model::{
    BoundEnhancedEndpoint, BoundExpr, BoundProgram, ParsedLabelDeclaration, RelationLabel,
    RelationLabels, RelationTarget, TokenPredicate,
};
use super::{Diagnostic, DiagnosticCode};
use crate::fsm::LabelDeclaration;
use crate::fsm::types::AttrValueType;
use crate::token_match::AttributeKey;

/// The attribute, the value checked against its set, and the authored spelling.
type Literal = (AttributeKey, CompactString, CompactString);

pub(super) fn label_declarations(
    parsed: &[ParsedLabelDeclaration],
    bound: &BoundProgram,
) -> Vec<LabelDeclaration> {
    parsed
        .iter()
        .map(|declaration| {
            let (line, _column) = bound.sources.line_column(declaration.span);
            LabelDeclaration::new(
                declaration.attribute.clone(),
                declaration.set.clone(),
                CompactString::from(&*bound.sources.source_label(declaration.span.source())),
                line,
            )
        })
        .collect()
}

/// One error per (definition, attribute, literal) outside the set declared in
/// the definition's own file.
pub(super) fn literal_errors(
    parsed: &[ParsedLabelDeclaration],
    declarations: &[LabelDeclaration],
    bound: &BoundProgram,
) -> Vec<Diagnostic> {
    let mut errors = Vec::new();
    for definition in &bound.definitions {
        let in_file: Vec<&LabelDeclaration> = parsed
            .iter()
            .zip(declarations)
            .filter(|(parsed, _)| parsed.span.source() == definition.source)
            .map(|(_, declaration)| declaration)
            .collect();
        if in_file.is_empty() {
            continue;
        }
        let mut literals = BTreeSet::new();
        if let Some(expr) = definition.expr() {
            collect_expr(expr, &mut literals);
        }
        for relation in definition.relations() {
            collect_relation_labels(&relation.labels, &mut literals);
            if let RelationTarget::Predicate { predicates, .. } = &relation.target {
                for predicate in predicates {
                    collect_predicate(predicate, &mut literals);
                }
            }
        }
        for relation in definition.enhanced_relations() {
            collect_relation_labels(&relation.labels, &mut literals);
            for endpoint in [&relation.head, &relation.dependent] {
                if let BoundEnhancedEndpoint::Binding { predicates, .. } = endpoint {
                    for predicate in predicates {
                        collect_predicate(predicate, &mut literals);
                    }
                }
            }
        }
        let location = super::bind::location(&bound.sources, definition.span);
        for (attribute, literal, authored) in literals {
            let Some(declaration) = in_file
                .iter()
                .find(|declaration| declaration.attribute() == &attribute)
            else {
                continue;
            };
            if !declaration.admits(&literal) {
                errors.push(Diagnostic::error(
                    DiagnosticCode::LabelOutsideDeclaredSet,
                    location.source_name(),
                    location.byte_range(),
                    format!(
                        "`{}` tests `{}` value `{authored}`, which is outside the label set `{}` declared by `labels:`",
                        definition.name,
                        attribute.as_str(),
                        declaration.set_name()
                    ),
                    location.line(),
                    location.column(),
                ));
            }
        }
    }
    errors
}

fn collect_predicate(predicate: &TokenPredicate, into: &mut BTreeSet<Literal>) {
    for atom in predicate {
        let values: &[CompactString] = match &atom.value {
            AttrValueType::Single(value)
            | AttrValueType::SetHas(value)
            | AttrValueType::SetNotHas(value) => std::slice::from_ref(value),
            AttrValueType::OneOf(values)
            | AttrValueType::NotIn(values)
            | AttrValueType::SetHasAny(values)
            | AttrValueType::SetNotHasAny(values) => values,
            AttrValueType::Contains(_)
            | AttrValueType::NotContains(_)
            | AttrValueType::Regex(_)
            | AttrValueType::NotRegex(_) => &[],
        };
        for value in values {
            into.insert((atom.attr.clone(), value.clone(), value.clone()));
        }
    }
}

/// A `nsubj:*` label is checked as `nsubj` and reported as authored.
fn collect_relation_labels(labels: &RelationLabels<RelationLabel>, into: &mut BTreeSet<Literal>) {
    if let RelationLabels::Listed(labels) = labels {
        let rel = AttributeKey::new("dep.rel").expect("dependency keys are canonical");
        for label in labels {
            let (RelationLabel::Exact(value) | RelationLabel::SubtypeOf(value)) = label;
            into.insert((rel.clone(), value.clone(), label.authored()));
        }
    }
}

/// Walks one definition's own body; a referenced definition is checked
/// against its own file's declaration.
fn collect_expr(expr: &BoundExpr, into: &mut BTreeSet<Literal>) {
    match expr {
        BoundExpr::Token(predicate) => collect_predicate(predicate, into),
        BoundExpr::Lookaround(lookaround) => collect_expr(&lookaround.body, into),
        BoundExpr::Sequence(children) | BoundExpr::Alternation(children) => {
            for child in children {
                collect_expr(child, into);
            }
        }
        BoundExpr::Intersection { operands, .. } | BoundExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_expr(operand, into);
            }
        }
        BoundExpr::Repeat { expr, .. } | BoundExpr::Capture { expr, .. } => {
            collect_expr(expr, into);
        }
        BoundExpr::SameAs { .. } | BoundExpr::Assertion(_) | BoundExpr::Definition { .. } => {}
    }
}
