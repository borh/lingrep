use compact_str::CompactString;

use super::model::{ExecutionDefinition, ExecutionExpr};
use crate::fsm::{AttrValueType, PositionAssertion, SymbolicAttrValue};
use crate::token_match::AttributeKey;

const MAX_CLAUSES: usize = 64;
const MAX_TERMS: usize = 256;

type Clauses = Vec<Vec<SentenceFilterTerm>>;

/// A necessary sentence condition: at least one clause must have all its terms
/// witnessed somewhere in the sentence, possibly on different tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SentenceFilter {
    clauses: Box<[Box<[SentenceFilterTerm]>]>,
}

impl SentenceFilter {
    pub fn clauses(&self) -> &[Box<[SentenceFilterTerm]>] {
        &self.clauses
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SentenceFilterTerm {
    attribute: AttributeKey,
    value: CompactString,
}

impl SentenceFilterTerm {
    pub fn attribute(&self) -> &AttributeKey {
        &self.attribute
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

pub(crate) fn derive_sentence_filter(
    definitions: &[ExecutionDefinition],
) -> Option<SentenceFilter> {
    if definitions.is_empty() {
        return None;
    }
    let mut clauses = Vec::new();
    for definition in definitions {
        if !definition.relations.is_empty() || !definition.enhanced_relations.is_empty() {
            return None;
        }
        clauses.extend(expression(&definition.expr)?);
        bounded(&mut clauses)?;
    }
    if clauses.iter().any(Vec::is_empty) {
        return None;
    }
    Some(SentenceFilter {
        clauses: clauses.into_iter().map(Vec::into_boxed_slice).collect(),
    })
}

fn bounded(clauses: &mut Clauses) -> Option<()> {
    if clauses.len() > MAX_CLAUSES || clauses.iter().map(Vec::len).sum::<usize>() > MAX_TERMS {
        return None;
    }
    for clause in clauses.iter_mut() {
        clause.sort_unstable();
        clause.dedup();
    }
    clauses.sort_unstable();
    clauses.dedup();
    Some(())
}

fn conjunction(left: Clauses, right: Clauses) -> Option<Clauses> {
    if left.len().checked_mul(right.len())? > MAX_CLAUSES {
        return None;
    }
    let mut result = Vec::new();
    for a in left {
        for b in &right {
            let mut clause = a.clone();
            clause.extend(b.iter().cloned());
            result.push(clause);
            bounded(&mut result)?;
        }
    }
    Some(result)
}

fn predicate(atoms: &[SymbolicAttrValue]) -> Option<Clauses> {
    let mut clauses = vec![vec![]];
    for atom in atoms {
        if atom.complement {
            return None;
        }
        let values: &[CompactString] = match &atom.value {
            AttrValueType::Single(value) => std::slice::from_ref(value),
            AttrValueType::OneOf(values) => values,
            _ => return None,
        };
        if values.is_empty() || values.len() > MAX_CLAUSES {
            return None;
        }
        let alternatives = values
            .iter()
            .map(|value| {
                vec![SentenceFilterTerm {
                    attribute: atom.attr.clone(),
                    value: value.clone(),
                }]
            })
            .collect();
        clauses = conjunction(clauses, alternatives)?;
    }
    Some(clauses)
}

fn expression(expr: &ExecutionExpr) -> Option<Clauses> {
    match expr {
        ExecutionExpr::Token(atoms) => predicate(atoms),
        ExecutionExpr::Assertion(
            PositionAssertion::SentenceStart | PositionAssertion::SentenceEnd,
        ) => Some(vec![vec![]]),
        ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => None,
        ExecutionExpr::Capture { expr, .. } => expression(expr),
        ExecutionExpr::Repeat { expr, min, .. } => {
            let body = expression(expr)?;
            Some(if *min == 0 { vec![vec![]] } else { body })
        }
        ExecutionExpr::Sequence(children)
        | ExecutionExpr::Intersection {
            operands: children, ..
        }
        | ExecutionExpr::AnyOrder {
            operands: children, ..
        } => {
            let mut clauses = vec![vec![]];
            for child in children {
                clauses = conjunction(clauses, expression(child)?)?;
            }
            Some(clauses)
        }
        ExecutionExpr::Alternation(children) => {
            let mut clauses = Vec::new();
            for child in children {
                clauses.extend(expression(child)?);
                bounded(&mut clauses)?;
            }
            Some(clauses)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::{CompiledCatalog, FsmBuilder, PatternSource};
    use crate::token_match::{TokenAttributeIndex, TokenInfo, TokenSchema};

    fn compile(source: &str) -> CompiledCatalog {
        let mut builder = FsmBuilder::new();
        builder
            .add_document(PatternSource::named("filter.lx", source))
            .unwrap();
        builder
            .build()
            .unwrap_or_else(|error| panic!("{source}: {error:?}"))
            .matcher
    }

    fn admits(filter: &SentenceFilter, words: &[TokenInfo]) -> bool {
        use crate::token_match::TokenSequence;
        filter.clauses().iter().any(|clause| {
            clause.iter().all(|term| {
                (0..words.len()).any(|i| {
                    words
                        .get_key(i, term.attribute())
                        .is_some_and(|v| v.as_str() == term.value())
                })
            })
        })
    }

    #[test]
    fn every_match_implies_admission_across_materialized_branches() {
        let sources = [
            "p := [text=a]. q := [text=b].",
            "set S := {a, b}. fragment F := [text in @S]. p := ^ (?<x> @F) []? [text=c]+ $.",
            "p := ([text=a] | [text=b]) [text=c].",
            "p := [text=a]? [text=b].",
            "p := ([text=a] []* [text=b]) & []{2,4}.",
            "p := any_order((?<a> [text=a]), (?<b> [text=b])).",
            "p := [text=a & lemma=x].",
        ];
        for source in sources {
            let catalog = compile(source);
            let bound = catalog
                .bind(&TokenSchema::from_keys(["text", "lemma"]).unwrap())
                .unwrap();
            let filter = bound.sentence_filter().expect(source);
            let mut excluded = TokenInfo::default();
            excluded.set(TokenAttributeIndex::Orth, "z".into());
            excluded.set(TokenAttributeIndex::Lemma, "z".into());
            assert!(!admits(filter, &[excluded]), "{source}");
            let mut matches = 0;
            for len in 0..=4 {
                for mut code in 0..4usize.pow(len as u32) {
                    let mut words = Vec::new();
                    for _ in 0..len {
                        let mut word = TokenInfo::default();
                        let value = ["a", "b", "c", "z"][code % 4];
                        code /= 4;
                        word.set(TokenAttributeIndex::Orth, value.into());
                        word.set(TokenAttributeIndex::Lemma, "x".into());
                        words.push(word);
                    }
                    if !catalog.find_matches(&words).is_empty() {
                        matches += 1;
                        assert!(admits(filter, &words), "{source}: {words:?}");
                    }
                }
            }
            assert!(matches > 0, "{source}");
        }
    }

    #[test]
    fn uncertain_emitters_and_policy_force_scan() {
        for source in [
            "p := [].",
            "p := [text=a]. q := [].",
            "p := [text=a] | [].",
            "p := [text=a]? [].",
            "p := [text=a] [lemma!=x].",
            "p := [text=a] [!(lemma=x)].",
            "p := [text ~ /a/].",
            "p := [text=a] followed_by([text=b]).",
            "p := (?<x> [text=a]) same_as($x).",
            r#"lexicon words by [text] := {"a"}."#,
            "key: [text]\np := [text=a].",
            "p := (?<v> [text=a]) (?<o> [text=b]) where $v -[obj]-> $o.",
            "p := [text=a]. fragment tail := [text=b]. policy\n exclude @p with: @tail, relation: after, evidence: raw.\n end",
            "p := (?<v> [text=a]) where (?<n> [node.kind=empty]) -[enhanced nsubj]-> $v.",
        ] {
            let catalog = compile(source);
            let bound = catalog
                .bind(
                    &TokenSchema::new(
                        ["text", "lemma", "dep.head", "dep.rel"]
                            .into_iter()
                            .map(|key| {
                                crate::token_match::SchemaAttribute::new(
                                    key,
                                    crate::token_match::AttributeKind::Scalar,
                                )
                                .unwrap()
                            })
                            .chain(std::iter::once(
                                crate::token_match::SchemaAttribute::new(
                                    "dep.enhanced",
                                    crate::token_match::AttributeKind::StringSet,
                                )
                                .unwrap(),
                            )),
                    )
                    .unwrap(),
                )
                .unwrap();
            assert!(bound.sentence_filter().is_none(), "{source}");
        }
    }

    #[test]
    fn binding_and_proof_budgets_decline_unusable_seeds() {
        let catalog = compile("p := [lemma=a].");
        assert!(
            catalog
                .bind(&TokenSchema::from_keys(["text"]).unwrap())
                .unwrap()
                .sentence_filter()
                .is_none()
        );
        let sets = TokenSchema::new([crate::token_match::SchemaAttribute::new(
            "lemma",
            crate::token_match::AttributeKind::StringSet,
        )
        .unwrap()])
        .unwrap();
        assert!(catalog.bind(&sets).is_err());
        let alternatives = (0..65)
            .map(|i| format!("v{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let catalog = compile(&format!("p := [text in {{{alternatives}}}]."));
        assert!(
            catalog
                .bind(&TokenSchema::from_keys(["text"]).unwrap())
                .unwrap()
                .sentence_filter()
                .is_none()
        );
        let long = (0..257)
            .map(|i| format!("[text=v{i}]"))
            .collect::<Vec<_>>()
            .join(" ");
        let catalog = compile(&format!("p := {long}."));
        assert!(
            catalog
                .bind(&TokenSchema::from_keys(["text"]).unwrap())
                .unwrap()
                .sentence_filter()
                .is_none()
        );
        let body = (0..7)
            .map(|i| format!("[text in {{a{i},b{i}}}]"))
            .collect::<Vec<_>>()
            .join(" ");
        let catalog = compile(&format!("p := {body}."));
        assert!(
            catalog
                .bind(&TokenSchema::from_keys(["text"]).unwrap())
                .unwrap()
                .sentence_filter()
                .is_none()
        );
    }
}
