//! What a catalog says about attributes it can only test when they are present.
//!
//! Separate from [`super::CompilerStats`], which counts compiled shapes, and
//! from [`crate::fsm::CompiledCatalog`], which holds execution state and no source
//! provenance at all. This records where an author *wrote* something, so it
//! belongs to the compile output rather than to the matcher.

use std::collections::{BTreeMap, BTreeSet};

use compact_str::CompactString;

use super::model::{
    BoundDefinitionBody, BoundEnhancedEndpoint, BoundExpr, BoundProgram, DefinitionId,
    RelationTarget, TokenPredicate,
};
use crate::fsm::types::AttrValueType;
use crate::token_match::AttributeKey;

/// One definition's use of one attribute under an operator that is false when
/// the attribute is absent.
///
/// Carries its own provenance because the consumer is a report, not the
/// matcher: [`crate::fsm::CompiledCatalog`] retains neither the source registry nor
/// definition spans, so a name recovered from it later would be recovered from
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusionSite {
    /// The definition whose body contains the operator — never a referrer.
    pub definition: CompactString,
    pub attribute: AttributeKey,
    pub source_name: CompactString,
    /// 1-indexed line of the definition's own declaration. Per-predicate spans
    /// do not survive binding; a declaration line locates a short fragment.
    pub declaration_line: usize,
    /// How many absence-sensitive occurrences over this attribute the
    /// definition contains.
    pub site_count: usize,
    /// Whether this definition is evaluated at all. An unreferenced fragment
    /// is not, so its exclusion cannot be exposed however partial the
    /// attribute is.
    pub reachable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexiconProjectionSite {
    pub definition: CompactString,
    pub attribute: AttributeKey,
    pub source_name: CompactString,
    pub declaration_line: usize,
    pub reachable: bool,
}

/// One definition's use of one attribute, under any operator or polarity.
///
/// The complete referenced set per definition — token predicates wherever they
/// sit (lookaround bodies included), `same_as` projections, `where`-clause
/// edge attributes, and lexicon `by` keys — so a diagnostic about an
/// attribute the corpus cannot supply can name the definitions that wrote it.
/// Publication-policy projections (`key:`, `transform:`) are deliberately not
/// definition sites; a caller reporting on them names the policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeReferenceSite {
    /// The definition whose body contains the reference — never a referrer.
    pub definition: CompactString,
    pub attribute: AttributeKey,
    pub source_name: CompactString,
    /// 1-indexed line of the definition's own declaration.
    pub declaration_line: usize,
    /// How many references over this attribute the definition contains.
    pub site_count: usize,
    /// Whether this definition is evaluated at all.
    pub reachable: bool,
}

/// Everything this compile learned about the catalog as authored.
#[derive(Clone, Debug, Default)]
pub struct CatalogAnalysis {
    absence_sensitive_exclusions: Box<[ExclusionSite]>,
    lexicon_projections: Box<[LexiconProjectionSite]>,
    attribute_references: Box<[AttributeReferenceSite]>,
}

impl CatalogAnalysis {
    /// Sorted by `(source_name, declaration_line, definition, attribute)`, so a
    /// report's row order is stable across runs and platforms.
    #[must_use]
    pub fn absence_sensitive_exclusions(&self) -> &[ExclusionSite] {
        &self.absence_sensitive_exclusions
    }

    #[must_use]
    pub fn lexicon_projections(&self) -> &[LexiconProjectionSite] {
        &self.lexicon_projections
    }

    /// Sorted like [`Self::absence_sensitive_exclusions`].
    #[must_use]
    pub fn attribute_references(&self) -> &[AttributeReferenceSite] {
        &self.attribute_references
    }

    /// The reference sites naming one attribute, in stored order.
    pub fn references_to<'analysis>(
        &'analysis self,
        attribute: &'analysis AttributeKey,
    ) -> impl Iterator<Item = &'analysis AttributeReferenceSite> {
        self.attribute_references
            .iter()
            .filter(move |site| &site.attribute == attribute)
    }
}

/// True when this atom's operator is false for a token lacking the attribute.
///
/// The complement check is load-bearing and not an optimization: `!(A!=x)` is a
/// true complement, so absence satisfies it and it can never be exposed.
fn is_absence_sensitive(value: &AttrValueType, complement: bool) -> bool {
    !complement
        && matches!(
            value,
            AttrValueType::NotIn(_) | AttrValueType::NotContains(_) | AttrValueType::NotRegex(_)
        )
}

fn collect_predicate(predicate: &TokenPredicate, into: &mut BTreeMap<AttributeKey, usize>) {
    for atom in predicate {
        if is_absence_sensitive(&atom.value, atom.complement) {
            *into.entry(atom.attr.clone()).or_default() += 1;
        }
    }
}

/// Walks one definition's own body. Deliberately does not follow
/// [`BoundExpr::Definition`]: a reference belongs to the definition that
/// declares it, and following it would report a fragment's operator once per
/// referrer.
fn collect_exclusions(expr: &BoundExpr, into: &mut BTreeMap<AttributeKey, usize>) {
    match expr {
        BoundExpr::Token(predicate) => collect_predicate(predicate, into),
        BoundExpr::Lookaround(lookaround) => collect_exclusions(&lookaround.body, into),
        BoundExpr::Sequence(children) | BoundExpr::Alternation(children) => {
            for child in children {
                collect_exclusions(child, into);
            }
        }
        BoundExpr::Intersection { operands, .. } | BoundExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_exclusions(operand, into);
            }
        }
        BoundExpr::Repeat { expr, .. } | BoundExpr::Capture { expr, .. } => {
            collect_exclusions(expr, into);
        }
        BoundExpr::Assertion(_) | BoundExpr::SameAs { .. } | BoundExpr::Definition { .. } => {}
    }
}

/// Walks one definition's own body for *every* attribute use, whatever the
/// operator or polarity. Shares [`collect_exclusions`]'s referrer rule: a
/// reference belongs to the definition that declares it.
fn collect_references(expr: &BoundExpr, into: &mut BTreeMap<AttributeKey, usize>) {
    match expr {
        BoundExpr::Token(predicate) => {
            for atom in predicate {
                *into.entry(atom.attr.clone()).or_default() += 1;
            }
        }
        BoundExpr::Lookaround(lookaround) => collect_references(&lookaround.body, into),
        BoundExpr::Sequence(children) | BoundExpr::Alternation(children) => {
            for child in children {
                collect_references(child, into);
            }
        }
        BoundExpr::Intersection { operands, .. } | BoundExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_references(operand, into);
            }
        }
        BoundExpr::Repeat { expr, .. } | BoundExpr::Capture { expr, .. } => {
            collect_references(expr, into);
        }
        BoundExpr::SameAs { projection, .. } => {
            *into.entry(projection.attribute().clone()).or_default() += 1;
        }
        BoundExpr::Assertion(_) | BoundExpr::Definition { .. } => {}
    }
}

/// Builds the analysis.
///
/// `evaluated` must be every definition that can be evaluated — referenced
/// fragments *and* emitting definitions. See
/// [`super::unused_fragment_warnings`] for why those are not the same set as
/// that pass's internal `reachable`.
pub(crate) fn analyze(bound: &BoundProgram, evaluated: &BTreeSet<DefinitionId>) -> CatalogAnalysis {
    let mut sites = Vec::new();
    let mut projections = Vec::new();
    let mut references = Vec::new();
    for definition in &bound.definitions {
        let mut per_attribute = BTreeMap::new();
        let mut referenced = BTreeMap::new();
        if let Some(expr) = definition.expr() {
            collect_exclusions(expr, &mut per_attribute);
            collect_references(expr, &mut referenced);
        }
        // A `where` clause reads both edge attributes per constraint, and a
        // lexicon scans each `by` projection — references exactly as a
        // predicate's are, just not written inside an expression.
        if !definition.relations().is_empty() {
            for key in ["dep.head", "dep.rel"] {
                let key = AttributeKey::new(key).expect("dependency keys are canonical");
                *referenced.entry(key).or_default() += definition.relations().len();
            }
            for relation in definition.relations() {
                if let RelationTarget::Predicate { predicates, .. } = &relation.target {
                    for predicate in predicates {
                        collect_predicate(predicate, &mut per_attribute);
                        for atom in predicate {
                            *referenced.entry(atom.attr.clone()).or_default() += 1;
                        }
                    }
                }
            }
        }
        if !definition.enhanced_relations().is_empty() {
            let key = AttributeKey::new("dep.enhanced").expect("enhanced key is canonical");
            *referenced.entry(key).or_default() += definition.enhanced_relations().len();
            for relation in definition.enhanced_relations() {
                for endpoint in [&relation.head, &relation.dependent] {
                    match endpoint {
                        BoundEnhancedEndpoint::Binding { predicates, .. } => {
                            for predicate in predicates {
                                collect_predicate(predicate, &mut per_attribute);
                                for atom in predicate {
                                    *referenced.entry(atom.attr.clone()).or_default() += 1;
                                }
                            }
                        }
                        BoundEnhancedEndpoint::BasicHead { .. } => {
                            let key = AttributeKey::new("dep.head")
                                .expect("basic dependency head key is canonical");
                            *referenced.entry(key).or_default() += 1;
                        }
                        BoundEnhancedEndpoint::Reference { .. } => {}
                    }
                }
            }
        }
        let (line, _column) = bound.sources.line_column(definition.span);
        let source_name =
            CompactString::from(&*bound.sources.source_label(definition.span.source()));
        if let BoundDefinitionBody::Lexicon { by, .. } = &definition.body {
            for attribute in by.as_ref() {
                *referenced.entry(attribute.clone()).or_default() += 1;
            }
            projections.extend(by.iter().cloned().map(|attribute| LexiconProjectionSite {
                definition: definition.name.clone(),
                attribute,
                source_name: source_name.clone(),
                declaration_line: line,
                reachable: true,
            }));
        }
        // A lexicon is always scanned; a pattern or fragment is evaluated
        // exactly when the exclusion analysis says it is.
        let reachable = matches!(&definition.body, BoundDefinitionBody::Lexicon { .. })
            || evaluated.contains(&definition.id);
        for (attribute, site_count) in referenced {
            references.push(AttributeReferenceSite {
                definition: definition.name.clone(),
                attribute,
                source_name: source_name.clone(),
                declaration_line: line,
                site_count,
                reachable,
            });
        }
        if per_attribute.is_empty() {
            continue;
        }
        for (attribute, site_count) in per_attribute {
            sites.push(ExclusionSite {
                definition: definition.name.clone(),
                attribute,
                source_name: source_name.clone(),
                declaration_line: line,
                site_count,
                reachable: evaluated.contains(&definition.id),
            });
        }
    }
    sites.sort_by(|left, right| {
        left.source_name
            .cmp(&right.source_name)
            .then_with(|| left.declaration_line.cmp(&right.declaration_line))
            .then_with(|| left.definition.cmp(&right.definition))
            .then_with(|| left.attribute.cmp(&right.attribute))
    });
    projections.sort_by(|left, right| {
        left.source_name
            .cmp(&right.source_name)
            .then_with(|| left.declaration_line.cmp(&right.declaration_line))
            .then_with(|| left.definition.cmp(&right.definition))
            .then_with(|| left.attribute.cmp(&right.attribute))
    });
    references.sort_by(|left, right| {
        left.source_name
            .cmp(&right.source_name)
            .then_with(|| left.declaration_line.cmp(&right.declaration_line))
            .then_with(|| left.definition.cmp(&right.definition))
            .then_with(|| left.attribute.cmp(&right.attribute))
    });
    CatalogAnalysis {
        absence_sensitive_exclusions: sites.into_boxed_slice(),
        lexicon_projections: projections.into_boxed_slice(),
        attribute_references: references.into_boxed_slice(),
    }
}

#[cfg(test)]
mod tests {
    use crate::fsm::{CompileLimits, FsmBuilder, PatternSource, compile_catalog};

    fn sites(text: &str) -> Vec<(String, String, usize)> {
        compile_catalog(PatternSource::named("t.lx", text), CompileLimits::default())
            .expect("catalog compiles")
            .analysis
            .absence_sensitive_exclusions()
            .iter()
            .map(|site| {
                (
                    site.definition.to_string(),
                    site.attribute.pattern_key().to_owned(),
                    site.site_count,
                )
            })
            .collect()
    }

    /// Table-driven over all three absence-sensitive variants. A test covering
    /// only `NotIn` would let an implementation matching one variant pass.
    #[test]
    fn every_absence_sensitive_operator_is_recorded() {
        for operator in ["p := [text!=x].", "p := [text!~x].", "p := [text!~ /x/]."] {
            assert_eq!(
                sites(operator),
                vec![("p".to_owned(), "text".to_owned(), 1)],
                "{operator} must be recorded"
            );
        }
    }

    /// The complement is a *true* complement: it holds for a token missing the
    /// attribute, so it can never be exposed and must not be recorded.
    #[test]
    fn a_complemented_operator_is_not_recorded() {
        for operator in [
            "p := [!(text!=x)].",
            "p := [!(text!~x)].",
            "p := [!(text!~ /x/)].",
        ] {
            assert_eq!(sites(operator), vec![], "{operator} must not be recorded");
        }
    }

    /// A positive operator is also false on absence, but that is the author's
    /// evident intent: `[text=x]` asks for a token that has `orth`. Only the
    /// negated spellings read as "everything except".
    #[test]
    fn positive_operators_are_not_recorded() {
        for operator in ["p := [text=x].", "p := [text in {x}].", "p := [text~x]."] {
            assert_eq!(sites(operator), vec![], "{operator} must not be recorded");
        }
    }

    #[test]
    fn repeated_sites_collapse_with_a_count() {
        assert_eq!(
            sites("p := [text!=x] [text!=y]."),
            vec![("p".to_owned(), "text".to_owned(), 2)]
        );
    }

    /// A lookaround body is evaluated, so an exclusion inside one is exposed
    /// exactly as an inline one is.
    #[test]
    fn an_exclusion_inside_a_lookaround_is_recorded() {
        assert_eq!(
            sites("p := [] followed_by([text!=x])."),
            vec![("p".to_owned(), "text".to_owned(), 1)]
        );
    }

    /// Keyed by the definition containing the text, never the referrer:
    /// `double-wo.lx`'s two `[orthBase!=を]` sites are inside fragments, and
    /// naming the emitting pattern would point an author at a definition that
    /// does not contain the operator to edit.
    #[test]
    fn a_site_is_keyed_by_the_definition_containing_it_not_the_referrer() {
        assert_eq!(
            sites("fragment f := [text!=x].\npattern p := @f."),
            vec![("f".to_owned(), "text".to_owned(), 1)]
        );
    }

    /// The trap this analysis must not fall into. `unused_fragment_warnings`'
    /// `reachable` set holds referenced *fragments*; an emitting definition is
    /// excluded from its diagnostic by a visibility test, not by membership.
    /// Reusing that set unchanged marks every pattern-level exclusion `inert`.
    #[test]
    fn an_exclusion_written_directly_in_a_pattern_is_reachable() {
        let output = compile_catalog(
            PatternSource::named("t.lx", "pattern p := [text!=x]."),
            CompileLimits::default(),
        )
        .expect("catalog compiles");

        let site = &output.analysis.absence_sensitive_exclusions()[0];
        assert!(site.reachable, "a pattern is always evaluated");
    }

    fn builder_sites(documents: &[(&str, &str)]) -> Vec<crate::fsm::ExclusionSite> {
        let mut builder = FsmBuilder::new();
        for (name, text) in documents {
            builder
                .add_document(PatternSource::named(name, text))
                .expect("document compiles");
        }
        builder
            .build()
            .expect("catalog compiles")
            .analysis
            .absence_sensitive_exclusions()
            .to_vec()
    }

    /// Provenance is per record: a catalog assembled from two files has two
    /// source names, and an author given only a line cannot find the second.
    #[test]
    fn each_site_carries_the_source_it_was_written_in() {
        let sites = builder_sites(&[
            ("first.lx", "fragment f := [text!=x]."),
            ("second.lx", "pattern p := @f [lemma!=y]."),
        ]);

        let located: Vec<(String, String)> = sites
            .iter()
            .map(|site| (site.source_name.to_string(), site.definition.to_string()))
            .collect();
        assert_eq!(
            located,
            vec![
                ("first.lx".to_owned(), "f".to_owned()),
                ("second.lx".to_owned(), "p".to_owned()),
            ]
        );
        assert!(sites.iter().all(|site| site.declaration_line == 1));
    }

    /// The counterexample that makes "exposure" honest: an unreferenced
    /// fragment is never evaluated, so no absent token can reach it.
    #[test]
    fn an_unreferenced_fragment_is_recorded_but_not_reachable() {
        let sites = builder_sites(&[(
            "t.lx",
            "fragment orphan := [text!=x].\npattern p := [lemma!=y].",
        )]);

        let reachability: Vec<(String, bool)> = sites
            .iter()
            .map(|site| (site.definition.to_string(), site.reachable))
            .collect();
        assert_eq!(
            reachability,
            vec![("orphan".to_owned(), false), ("p".to_owned(), true)]
        );
    }

    fn references(text: &str) -> Vec<(String, String, usize, bool)> {
        compile_catalog(PatternSource::named("t.lx", text), CompileLimits::default())
            .expect("catalog compiles")
            .analysis
            .attribute_references()
            .iter()
            .map(|site| {
                (
                    site.definition.to_string(),
                    site.attribute.pattern_key().to_owned(),
                    site.site_count,
                    site.reachable,
                )
            })
            .collect()
    }

    /// Unlike the exclusion analysis, every operator and both polarities are
    /// references: the consumer asks "does this catalog read the attribute at
    /// all", not "is it exposed to absence".
    #[test]
    fn every_operator_and_polarity_is_a_reference() {
        assert_eq!(
            references("p := [text=x] [!(lemma=y)] [upos!=z]."),
            vec![
                ("p".to_owned(), "lemma".to_owned(), 1, true),
                ("p".to_owned(), "text".to_owned(), 1, true),
                ("p".to_owned(), "upos".to_owned(), 1, true),
            ]
        );
    }

    /// `same_as` reads its projection attribute without writing a predicate.
    #[test]
    fn a_same_as_projection_is_a_reference() {
        assert_eq!(
            references("p := (?<u> [upos=ADV]) same_as($u, by=unidic.kana)."),
            vec![
                ("p".to_owned(), "unidic.kana".to_owned(), 1, true),
                ("p".to_owned(), "upos".to_owned(), 1, true),
            ]
        );
    }

    /// A `where` clause reads both edge attributes per constraint, and a
    /// lookaround body is read exactly as an inline expression is.
    #[test]
    fn where_edges_and_lookaround_bodies_are_references() {
        assert_eq!(
            references(
                "pattern p := (?<s> [upos=NOUN]) followed_by([lemma=だ]) (?<v> [upos=VERB]) \
                 where $v -[nsubj]-> $s."
            ),
            vec![
                ("p".to_owned(), "dep.head".to_owned(), 1, true),
                ("p".to_owned(), "dep.rel".to_owned(), 1, true),
                ("p".to_owned(), "lemma".to_owned(), 1, true),
                ("p".to_owned(), "upos".to_owned(), 2, true),
            ]
        );
    }

    #[test]
    fn anonymous_dependent_predicates_are_attribute_sites() {
        assert_eq!(
            references(
                "pattern p := (?<v> [upos=VERB]) \
                 where $v -[obj]-> [lemma!=x | xpos=名詞]."
            ),
            vec![
                ("p".to_owned(), "dep.head".to_owned(), 1, true),
                ("p".to_owned(), "dep.rel".to_owned(), 1, true),
                ("p".to_owned(), "lemma".to_owned(), 1, true),
                ("p".to_owned(), "upos".to_owned(), 1, true),
                ("p".to_owned(), "xpos".to_owned(), 1, true),
            ]
        );
        assert_eq!(
            sites(
                "pattern p := (?<v> [upos=VERB]) \
                 where $v -[obj]-> [lemma!=x | xpos=名詞]."
            ),
            vec![("p".to_owned(), "lemma".to_owned(), 1)],
        );
    }

    /// A lexicon's `by` keys are read by every scan, so they are references
    /// and always reachable.
    #[test]
    fn lexicon_by_keys_are_references() {
        assert_eq!(
            references("lexicon terms by [text, norm] := {\"大学院\"}."),
            vec![
                ("terms".to_owned(), "norm".to_owned(), 1, true),
                ("terms".to_owned(), "text".to_owned(), 1, true),
            ]
        );
    }

    /// Reference sites share the exclusion analysis's referrer rule and
    /// reachability: an orphan fragment's references exist but are inert.
    #[test]
    fn an_orphan_fragments_references_are_not_reachable() {
        assert_eq!(
            references("fragment orphan := [text=x].\npattern p := [lemma=y]."),
            vec![
                ("orphan".to_owned(), "text".to_owned(), 1, false),
                ("p".to_owned(), "lemma".to_owned(), 1, true),
            ]
        );
    }

    #[test]
    fn references_to_filters_by_attribute() {
        let output = compile_catalog(
            PatternSource::named("t.lx", "p := [text=x].\nq := [text=y] [lemma=z]."),
            CompileLimits::default(),
        )
        .expect("catalog compiles");
        let attribute = crate::token_match::AttributeKey::new("text").unwrap();

        let referrers: Vec<String> = output
            .analysis
            .references_to(&attribute)
            .map(|site| site.definition.to_string())
            .collect();
        assert_eq!(referrers, vec!["p".to_owned(), "q".to_owned()]);
    }
}
