//! Attribute-dependent validation and normalization warnings for named sets.
//!
//! A `set` declaration is lowered before any reference names the attribute it
//! is compared with, so the fold of its members is reported at each `text`
//! reference instead of at the declaration, and the NFC form of its members
//! is reported once every reference has been read.

use super::diagnostic::{CompileError, Diagnostic, DiagnosticCode, DiagnosticSeverity};
use super::model::{
    ParsedAttrValue, ParsedDefinitionBody, ParsedEnhancedEndpoint, ParsedExpr, ParsedProgram,
    ParsedRelationTarget, ParsedSet, ParsedTest, SourceRegistry, SourceSpan,
};
use super::policy::diagnostic_at;
use crate::analysis::text_key;
use compact_str::CompactString;
use std::collections::HashMap;

pub(super) fn check_set_references(
    program: &ParsedProgram,
) -> Result<Vec<Diagnostic>, CompileError> {
    if program.sets.is_empty() {
        return Ok(Vec::new());
    }
    let mut scan = Scan {
        sets: &program.sets,
        sources: &program.sources,
        warnings: Vec::new(),
        label: String::new(),
        surface_only: HashMap::new(),
        collisions: program
            .sets
            .iter()
            .filter_map(|set| nfc_collision(set).map(|pair| (set.name.as_str(), pair)))
            .collect(),
    };
    for definition in &program.definitions {
        if let ParsedDefinitionBody::Pattern {
            expr,
            relations,
            enhanced_relations,
            ..
        } = &definition.body
        {
            scan.label = format!("definition `{}`", definition.name);
            scan.expr(expr)?;
            for test in definition.measures.iter().filter_map(|m| m.source.test()) {
                scan.test(test)?;
            }
            for relation in relations {
                if let ParsedRelationTarget::Predicate { test, .. } = &relation.target {
                    scan.test(test)?;
                }
            }
            for relation in enhanced_relations {
                for endpoint in [&relation.head, &relation.dependent] {
                    if let ParsedEnhancedEndpoint::Binding { test, .. } = endpoint {
                        scan.test(test)?;
                    }
                }
            }
        }
    }
    for test in &program.tests {
        scan.label = format!("test `{}`", test.name);
        scan.test(&test.body)?;
    }
    for set in &program.sets {
        if scan.surface_only.get(&set.name) != Some(&true) {
            scan.warnings
                .extend(set.normalized_literals.iter().cloned());
        }
    }
    Ok(scan.warnings)
}

fn nfc_collision(set: &ParsedSet) -> Option<(&str, &str)> {
    if set.exact_members == set.members {
        return None;
    }
    let mut seen = HashMap::new();
    for (exact, normalized) in set.exact_members.iter().zip(&set.members) {
        if let Some(first) = seen.insert(normalized, exact.as_str()) {
            return Some((first, exact));
        }
    }
    None
}

struct Scan<'a> {
    sets: &'a [ParsedSet],
    sources: &'a SourceRegistry,
    warnings: Vec<Diagnostic>,
    collisions: HashMap<&'a str, (&'a str, &'a str)>,
    /// The definition or test being scanned, named in warnings.
    label: String,
    /// For each set name referenced so far, whether every reference compares
    /// it with `surface`.
    surface_only: HashMap<CompactString, bool>,
}

impl Scan<'_> {
    fn expr(&mut self, expr: &ParsedExpr) -> Result<(), CompileError> {
        match expr {
            ParsedExpr::Token(test, _) => self.test(test)?,
            ParsedExpr::Lookaround(lookaround) => self.expr(&lookaround.body)?,
            ParsedExpr::Sequence(operands)
            | ParsedExpr::Alternation(operands)
            | ParsedExpr::Intersection { operands, .. }
            | ParsedExpr::AnyOrder { operands, .. } => {
                for operand in operands {
                    self.expr(operand)?;
                }
            }
            ParsedExpr::Repeat { expr, .. }
            | ParsedExpr::Group(expr)
            | ParsedExpr::NamedGroup { expr, .. } => self.expr(expr)?,
            ParsedExpr::Assertion(_) | ParsedExpr::SameAs { .. } | ParsedExpr::Reference { .. } => {
            }
        }
        Ok(())
    }

    fn test(&mut self, test: &ParsedTest) -> Result<(), CompileError> {
        match test {
            ParsedTest::Attr(attr) => match &attr.value {
                ParsedAttrValue::Set { name, span, .. }
                | ParsedAttrValue::SetAny { name, span, .. } => {
                    self.reference(name, attr.attr.as_str(), *span)?;
                }
                ParsedAttrValue::Resolved(_) | ParsedAttrValue::Near { .. } => {}
            },
            ParsedTest::Reference { .. } => {}
            ParsedTest::And(tests) | ParsedTest::Or(tests) => {
                for test in tests {
                    self.test(test)?;
                }
            }
            ParsedTest::Not(test) => self.test(test)?,
        }
        Ok(())
    }

    fn reference(
        &mut self,
        name: &CompactString,
        attribute: &str,
        span: SourceSpan,
    ) -> Result<(), CompileError> {
        let surface = attribute == "surface";
        *self.surface_only.entry(name.clone()).or_insert(true) &= surface;
        if !surface && let Some((first, second)) = self.collisions.get(name.as_str()) {
            let message = format!(
                "In {}, `@{name}` members `{first}` ({}) and `{second}` ({}) \
                 have the same NFC spelling for `{attribute}`",
                self.label,
                super::lower::code_points(first),
                super::lower::code_points(second),
            );
            return Err(CompileError::from_diagnostics(vec![diagnostic_at(
                self.sources,
                span,
                DiagnosticCode::Syntax,
                message,
                DiagnosticSeverity::Error,
            )]));
        }
        if attribute == "text" {
            self.folded_members(name, span);
        }
        Ok(())
    }

    fn folded_members(&mut self, name: &str, span: SourceSpan) {
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
