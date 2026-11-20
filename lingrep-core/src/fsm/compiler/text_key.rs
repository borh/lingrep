//! `FoldedLiteral` warnings for named sets compared with `text`.
//!
//! A `set` declaration is lowered before any reference names the attribute it
//! is compared with, so the fold of its members is reported at each `text`
//! reference instead of at the declaration.

use super::diagnostic::{Diagnostic, DiagnosticCode};
use super::model::{
    ParsedAttrValue, ParsedDefinitionBody, ParsedExpr, ParsedProgram, ParsedSet, ParsedTest,
    SourceRegistry, SourceSpan,
};
use crate::analysis::text_key;

/// One warning for each reference that compares a set with `text` when the
/// fold changes one of the set's members.
pub(super) fn folded_set_warnings(program: &ParsedProgram) -> Vec<Diagnostic> {
    let mut scan = Scan {
        sets: &program.sets,
        sources: &program.sources,
        warnings: Vec::new(),
        label: String::new(),
    };
    for definition in &program.definitions {
        if let ParsedDefinitionBody::Pattern { expr, .. } = &definition.body {
            scan.label = format!("definition `{}`", definition.name);
            scan.expr(expr);
        }
    }
    for test in &program.tests {
        scan.label = format!("test `{}`", test.name);
        scan.test(&test.body);
    }
    scan.warnings
}

struct Scan<'a> {
    sets: &'a [ParsedSet],
    sources: &'a SourceRegistry,
    warnings: Vec<Diagnostic>,
    /// The definition or test being scanned, named in warnings.
    label: String,
}

impl Scan<'_> {
    fn expr(&mut self, expr: &ParsedExpr) {
        match expr {
            ParsedExpr::Token(test, _) => self.test(test),
            ParsedExpr::Lookaround(lookaround) => self.expr(&lookaround.body),
            ParsedExpr::Sequence(operands)
            | ParsedExpr::Alternation(operands)
            | ParsedExpr::Intersection { operands, .. }
            | ParsedExpr::AnyOrder { operands, .. } => {
                for operand in operands {
                    self.expr(operand);
                }
            }
            ParsedExpr::Repeat { expr, .. }
            | ParsedExpr::Group(expr)
            | ParsedExpr::NamedGroup { expr, .. } => self.expr(expr),
            ParsedExpr::Assertion(_) | ParsedExpr::SameAs { .. } | ParsedExpr::Reference { .. } => {
            }
        }
    }

    fn test(&mut self, test: &ParsedTest) {
        match test {
            ParsedTest::Attr(attr) if attr.attr.as_str() == "text" => {
                if let ParsedAttrValue::Set { name, span, .. } = &attr.value {
                    self.set_reference(name, *span);
                }
            }
            ParsedTest::Attr(_) | ParsedTest::Reference { .. } => {}
            ParsedTest::And(tests) | ParsedTest::Or(tests) => {
                for test in tests {
                    self.test(test);
                }
            }
            ParsedTest::Not(test) => self.test(test),
        }
    }

    fn set_reference(&mut self, name: &str, span: SourceSpan) {
        let Some(set) = self.sets.iter().find(|set| set.name == name) else {
            return;
        };
        let changes: Vec<String> = set
            .members
            .iter()
            .filter_map(|member| {
                let key = text_key(member);
                (key != member.as_str()).then(|| format!("`{member}` as `{key}`"))
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        let message = format!(
            "In {}, the members of `@{name}` compared with `text` match as their folded \
             spellings: {}. Test `surface` to match the exact spelling",
            self.label,
            changes.join(", "),
        );
        let (line, column) = self.sources.line_column(span);
        let range = span.byte_range();
        self.warnings.push(Diagnostic::warning_with_source_label(
            DiagnosticCode::FoldedLiteral,
            self.sources.source_label(span.source()),
            usize::try_from(range.start).expect("source offsets fit usize")
                ..usize::try_from(range.end).expect("source offsets fit usize"),
            message,
            line,
            column,
        ));
    }
}
