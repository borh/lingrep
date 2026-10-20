use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use compact_str::CompactString;

use super::compiled::CompiledLexiconDefinition;
use crate::fsm::{LexicalMatch, Match};
use crate::token_match::{AttributeKey, TokenSequence};

#[derive(Clone, Copy, Debug)]
struct LexiconLabel {
    definition_id: u32,
    term_id: u32,
}

#[derive(Clone)]
struct ProjectionAutomaton {
    projection: AttributeKey,
    machine: AhoCorasick,
    labels: Box<[Box<[LexiconLabel]>]>,
    pattern_bytes: usize,
}

#[derive(Clone)]
pub(crate) struct CompiledLexicons {
    definitions: Box<[CompiledLexiconDefinition]>,
    projections: Box<[ProjectionAutomaton]>,
}

#[derive(Debug)]
pub(crate) struct LexiconCompileError(pub(crate) String);

impl CompiledLexicons {
    pub(crate) fn compile(
        definitions: Vec<CompiledLexiconDefinition>,
    ) -> Result<Option<Self>, LexiconCompileError> {
        if definitions.is_empty() {
            return Ok(None);
        }
        let mut projections = Vec::new();
        for projection in [
            AttributeKey::new("text").expect("form is canonical"),
            AttributeKey::new("norm").expect("norm is canonical"),
        ] {
            let mut ids: HashMap<Vec<u8>, usize> = HashMap::new();
            let mut patterns: Vec<Vec<u8>> = Vec::new();
            let mut labels: Vec<Vec<LexiconLabel>> = Vec::new();
            for (definition_id, definition) in definitions.iter().enumerate() {
                if !definition.by.contains(&projection) {
                    continue;
                }
                for (term_id, term) in definition.members.iter().enumerate() {
                    let bytes = term.as_bytes();
                    let pattern_id = if let Some(id) = ids.get(bytes) {
                        *id
                    } else {
                        let id = patterns.len();
                        let owned = bytes.to_vec();
                        ids.insert(owned.clone(), id);
                        patterns.push(owned);
                        labels.push(Vec::new());
                        id
                    };
                    labels[pattern_id].push(LexiconLabel {
                        definition_id: u32::try_from(definition_id).map_err(|_| {
                            LexiconCompileError("too many lexicon definitions".into())
                        })?,
                        term_id: u32::try_from(term_id)
                            .map_err(|_| LexiconCompileError("too many lexicon members".into()))?,
                    });
                }
            }
            if patterns.is_empty() {
                continue;
            }
            let machine = AhoCorasickBuilder::new()
                .match_kind(MatchKind::Standard)
                .build(&patterns)
                .map_err(|error| {
                    LexiconCompileError(format!("cannot compile lexicon automaton: {error}"))
                })?;
            projections.push(ProjectionAutomaton {
                projection,
                machine,
                pattern_bytes: patterns.iter().map(Vec::len).sum(),
                labels: labels
                    .into_iter()
                    .map(Vec::into_boxed_slice)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            });
        }
        Ok(Some(Self {
            definitions: definitions.into_boxed_slice(),
            projections: projections.into_boxed_slice(),
        }))
    }

    pub(crate) fn descriptors(&self) -> &[CompiledLexiconDefinition] {
        &self.definitions
    }

    pub(crate) fn find_matches<S: TokenSequence + ?Sized>(&self, tokens: &S) -> Vec<Match> {
        LEXICON_SCRATCH.with_borrow_mut(|scratch| self.find_matches_with_scratch(tokens, scratch))
    }

    fn find_matches_with_scratch<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        scratch: &mut LexiconScratch,
    ) -> Vec<Match> {
        scratch.hits.clear();
        for automaton in &self.projections {
            scratch.begin(tokens.len());
            let mut run_start = 0usize;
            scratch.reset_run(run_start);
            for token_index in 0..tokens.len() {
                if let Some(value) = tokens
                    .scalar_key(token_index, &automaton.projection)
                    .filter(|value| !value.is_empty())
                {
                    scratch.append(value.as_bytes(), token_index + 1);
                } else {
                    scan_run(automaton, &self.definitions, scratch);
                    run_start = token_index + 1;
                    scratch.reset_run(run_start);
                }
            }
            scan_run(automaton, &self.definitions, scratch);
        }

        let mut matches = Vec::with_capacity(scratch.hits.len());
        for (&(definition_id, start, end, term_id), &mask) in &scratch.hits {
            let definition = &self.definitions[definition_id as usize];
            let matched_by = definition
                .by
                .iter()
                .enumerate()
                .filter_map(|(index, projection)| {
                    ((mask & (1 << index)) != 0).then_some(projection.clone())
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            matches.push(Match {
                pattern: definition.name.clone(),
                start,
                end,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: Some(Box::new(LexicalMatch {
                    term: definition.members[term_id as usize].clone(),
                    matched_by,
                })),
            });
        }
        matches.sort_by(|left, right| {
            (
                left.start,
                left.end,
                &left.pattern,
                &left.lexical.as_ref().expect("lexical match").term,
            )
                .cmp(&(
                    right.start,
                    right.end,
                    &right.pattern,
                    &right.lexical.as_ref().expect("lexical match").term,
                ))
        });
        matches
    }

    pub(crate) fn owned_bytes(&self) -> usize {
        self.projections
            .iter()
            .map(|projection| {
                projection.machine.memory_usage()
                    + projection.pattern_bytes
                    + projection
                        .labels
                        .iter()
                        .map(|labels| labels.len() * std::mem::size_of::<LexiconLabel>())
                        .sum::<usize>()
            })
            .sum::<usize>()
            + self
                .definitions
                .iter()
                .map(|definition| {
                    definition.name.len()
                        + definition
                            .members
                            .iter()
                            .map(CompactString::len)
                            .sum::<usize>()
                        + definition.by.len() * std::mem::size_of::<AttributeKey>()
                })
                .sum::<usize>()
    }
}

fn scan_run(
    automaton: &ProjectionAutomaton,
    definitions: &[CompiledLexiconDefinition],
    scratch: &mut LexiconScratch,
) {
    if scratch.bytes.is_empty() {
        return;
    }
    for found in automaton.machine.find_overlapping_iter(&scratch.bytes) {
        let Some(start) = scratch.boundaries.get(found.start()) else {
            continue;
        };
        let Some(end) = scratch.boundaries.get(found.end()) else {
            continue;
        };
        for label in &automaton.labels[found.pattern().as_usize()] {
            let definition = label.definition_id as usize;
            let projection_index = definitions[definition]
                .by
                .iter()
                .position(|candidate| candidate == &automaton.projection)
                .expect("automaton labels only definitions using its projection");
            let key = (label.definition_id, start, end, label.term_id);
            *scratch.hits.entry(key).or_insert(0) |= 1 << projection_index;
        }
    }
}

enum BoundaryMap {
    Compact(Vec<u32>),
    Wide(Vec<usize>),
}

impl BoundaryMap {
    fn select(&mut self, token_count: usize) {
        if token_count <= u32::MAX as usize {
            if !matches!(self, Self::Compact(_)) {
                *self = Self::Compact(Vec::new());
            }
        } else if !matches!(self, Self::Wide(_)) {
            *self = Self::Wide(Vec::new());
        }
    }

    fn reset(&mut self, start: usize) {
        match self {
            Self::Compact(values) => {
                values.clear();
                values.push(start as u32);
            }
            Self::Wide(values) => {
                values.clear();
                values.push(start);
            }
        }
    }

    fn append(&mut self, byte_len: usize, end: usize) {
        match self {
            Self::Compact(values) => {
                values.extend(std::iter::repeat_n(u32::MAX, byte_len));
                *values.last_mut().expect("one byte appended") = end as u32;
            }
            Self::Wide(values) => {
                values.extend(std::iter::repeat_n(usize::MAX, byte_len));
                *values.last_mut().expect("one byte appended") = end;
            }
        }
    }

    fn get(&self, index: usize) -> Option<usize> {
        match self {
            Self::Compact(values) => values
                .get(index)
                .copied()
                .filter(|value| *value != u32::MAX)
                .map(|value| value as usize),
            Self::Wide(values) => values
                .get(index)
                .copied()
                .filter(|value| *value != usize::MAX),
        }
    }

    fn clear_and_shrink(&mut self) {
        match self {
            Self::Compact(values) => {
                values.clear();
                values.shrink_to_fit();
            }
            Self::Wide(values) => {
                values.clear();
                values.shrink_to_fit();
            }
        }
    }
}

type LexicalKey = (u32, usize, usize, u32);

struct LexiconScratch {
    bytes: Vec<u8>,
    boundaries: BoundaryMap,
    hits: BTreeMap<LexicalKey, u8>,
}

impl Default for LexiconScratch {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            boundaries: BoundaryMap::Compact(Vec::new()),
            hits: BTreeMap::new(),
        }
    }
}

impl LexiconScratch {
    fn begin(&mut self, token_count: usize) {
        self.boundaries.select(token_count);
    }

    fn reset_run(&mut self, start: usize) {
        self.bytes.clear();
        self.boundaries.reset(start);
    }

    fn append(&mut self, bytes: &[u8], token_end: usize) {
        self.bytes.extend_from_slice(bytes);
        self.boundaries.append(bytes.len(), token_end);
    }

    fn clear_and_shrink(&mut self) {
        self.bytes.clear();
        self.bytes.shrink_to_fit();
        self.boundaries.clear_and_shrink();
        self.hits.clear();
    }
}

thread_local! {
    static LEXICON_SCRATCH: RefCell<LexiconScratch> = RefCell::new(LexiconScratch::default());
}

pub(crate) fn clear_scratch() {
    LEXICON_SCRATCH.with_borrow_mut(LexiconScratch::clear_and_shrink);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_match::{TokenAttributeIndex, TokenInfo, TokenInfoTrait};
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn token(orth: Option<&str>, norm: Option<&str>) -> TokenInfo {
        let mut token = TokenInfo::default();
        if let Some(value) = orth {
            token.set(TokenAttributeIndex::Orth, value.into());
        }
        if let Some(value) = norm {
            token.set(TokenAttributeIndex::Norm, value.into());
        }
        token
    }

    fn descriptor(by: Vec<TokenAttributeIndex>, terms: Vec<String>) -> CompiledLexiconDefinition {
        CompiledLexiconDefinition {
            name: "words".into(),
            by: by
                .into_iter()
                .map(AttributeKey::from)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            members: terms
                .into_iter()
                .map(CompactString::from)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            metadata: HashMap::new(),
        }
    }

    fn tuples(matches: Vec<Match>) -> Vec<(usize, usize, String, Vec<AttributeKey>)> {
        matches
            .into_iter()
            .map(|matched| {
                let lexical = matched
                    .lexical
                    .expect("lexical scanner emits lexical payloads");
                (
                    matched.start,
                    matched.end,
                    lexical.term.to_string(),
                    lexical.matched_by.to_vec(),
                )
            })
            .collect()
    }

    /// Deliberately enumerates spans and projections; it shares no production
    /// automaton, byte-boundary map, hit table, or sort helper.
    fn oracle(
        definition: &CompiledLexiconDefinition,
        tokens: &[TokenInfo],
    ) -> Vec<(usize, usize, String, Vec<AttributeKey>)> {
        let mut hits: BTreeMap<(usize, usize, usize), u8> = BTreeMap::new();
        for (projection_index, projection) in definition.by.iter().enumerate() {
            for start in 0..tokens.len() {
                let mut joined = String::new();
                for end in start + 1..=tokens.len() {
                    let Some(value) = tokens[end - 1]
                        .scalar_key(projection)
                        .filter(|value| !value.is_empty())
                    else {
                        break;
                    };
                    joined.push_str(value);
                    for (term_id, term) in definition.members.iter().enumerate() {
                        if joined == term.as_str() {
                            *hits.entry((start, end, term_id)).or_insert(0) |=
                                1 << projection_index;
                        }
                    }
                }
            }
        }
        let mut output = hits
            .into_iter()
            .map(|((start, end, term_id), mask)| {
                let matched_by = definition
                    .by
                    .iter()
                    .enumerate()
                    .filter_map(|(index, projection)| {
                        ((mask & (1 << index)) != 0).then_some(projection.clone())
                    })
                    .collect();
                (
                    start,
                    end,
                    definition.members[term_id].to_string(),
                    matched_by,
                )
            })
            .collect::<Vec<_>>();
        output.sort_by(|left, right| (left.0, left.1, &left.2).cmp(&(right.0, right.1, &right.2)));
        output
    }

    proptest! {
        #[test]
        fn automaton_matches_independent_span_enumerator(
            raw_tokens in prop::collection::vec((prop::option::of("[ab]{0,2}"), prop::option::of("[ab]{0,2}")), 0..=8),
            raw_terms in prop::collection::vec("[ab]{1,4}", 1..=8),
            order in 0u8..4,
        ) {
            let by = match order {
                0 => vec![TokenAttributeIndex::Orth],
                1 => vec![TokenAttributeIndex::Norm],
                2 => vec![TokenAttributeIndex::Orth, TokenAttributeIndex::Norm],
                _ => vec![TokenAttributeIndex::Norm, TokenAttributeIndex::Orth],
            };
            let mut terms = raw_terms;
            terms.sort();
            terms.dedup();
            let definition = descriptor(by, terms);
            let tokens = raw_tokens.iter().map(|(orth, norm)| token(orth.as_deref(), norm.as_deref())).collect::<Vec<_>>();
            let compiled = CompiledLexicons::compile(vec![definition.clone()]).unwrap().unwrap();
            prop_assert_eq!(tuples(compiled.find_matches(&tokens)), oracle(&definition, &tokens));
        }
    }

    #[test]
    fn shared_pattern_bytes_fan_out_to_two_definition_labels() {
        let compiled = CompiledLexicons::compile(vec![
            descriptor(vec![TokenAttributeIndex::Orth], vec!["大学".into()]),
            CompiledLexiconDefinition {
                name: "other".into(),
                ..descriptor(vec![TokenAttributeIndex::Orth], vec!["大学".into()])
            },
        ])
        .unwrap()
        .unwrap();
        assert_eq!(compiled.projections[0].labels.len(), 1);
        assert_eq!(compiled.projections[0].labels[0].len(), 2);
    }
}
