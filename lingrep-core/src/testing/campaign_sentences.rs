//! The sentence set a whole-catalog measurement or baseline runs over.
//!
//! Two input sets, assembled together because every consumer needs both. Each
//! catalog's own `examples:` are committed beside the definitions they
//! exercise and cover those definitions by construction. But an example is
//! authored *for* its definition, so a defect that only shows on a sentence
//! nobody wrote as an example would pass unseen; the committed fixtures are
//! the sentences this repository has that nobody wrote for that purpose.
//!
//! **Two fixture shapes, deliberately kept apart.** The `r7_*` fixtures are
//! pre-tokenized transcripts: each `type: "sentence"` line carries the
//! tokenizer's full output, and reading it is the point — the caller does not
//! get to decide what a token carries. `synthetic.jsonl` is text with
//! `"tokens": null` and must go through the tokenizer like a catalog example.
//! Feeding the second through the first's reader yields an empty token array
//! and a silently vacuous result, which is the failure this split prevents.
//!
//! This lives in `lingrep_core::testing` rather than under `fsm::compiler` because
//! it has two consumers in different crates: the published-record baseline
//! inside this crate, and the `lingrep-perf` counted-work screen outside it. A
//! `#[cfg(test)]` module is invisible to the second, and a second assembly
//! would be a second thing to forget to update — the same hazard
//! [`catalogs`](super::catalogs) exists to prevent for the catalog set.
//!
//! The parsing logic is ported from `record_transformation_test.rs`'s private
//! `sentence`, not shared with it: that function lives in the integration-test
//! crate and cannot be called from `src`.

use std::str::FromStr;

use crate::fsm::CompiledCatalog;
use crate::token_match::{TokenAttributeIndex, TokenInfo};

/// A committed sentence and the label a diff should name it by.
pub type LabelledSentence = (String, Vec<TokenInfo>);

/// Committed sentences outside the catalogs' `examples:`, as of this baseline.
///
/// Twelve from `r7_natsume_tokens.jsonl`, two from `r7_aspect_tokens.jsonl`,
/// three from `synthetic.jsonl`. Small, and stated so a claim resting on this
/// set can be scoped honestly: it covers these plus the catalogs' own
/// examples, and no more.
pub const COMMITTED_FIXTURE_SENTENCES: usize = 17;

const NATSUME: &str = include_str!("../../tests/fixtures/r7_natsume_tokens.jsonl");
const ASPECT: &str = include_str!("../../tests/fixtures/r7_aspect_tokens.jsonl");
const SYNTHETIC: &str = include_str!("../../tests/corpus/synthetic.jsonl");

/// Every sentence a whole-catalog run over `matcher` should see, in a fixed
/// order: the catalog's own `examples:` first, then the committed fixtures.
///
/// The order is stable so a snapshot or a delta keyed on the label does not
/// churn. Panics if the fixture loader reads a different number of sentences
/// than [`COMMITTED_FIXTURE_SENTENCES`] records — asserted exactly rather than
/// as a floor, because a loader that quietly stops finding half its input
/// would otherwise narrow every consumer's coverage in silence.
pub fn baseline_sentences(matcher: &CompiledCatalog) -> Vec<LabelledSentence> {
    let mut sentences = Vec::new();
    for (definition, pattern) in matcher.patterns() {
        let Some(examples) = pattern
            .metadata
            .get("examples")
            .and_then(|value| value.as_array())
        else {
            continue;
        };
        for example in examples.iter().filter_map(|entry| entry.as_str()) {
            sentences.push((
                format!("{definition} :: {example}"),
                super::tokenize_japanese(example),
            ));
        }
    }

    let fixtures = committed_sentences();
    assert_eq!(
        fixtures.len(),
        COMMITTED_FIXTURE_SENTENCES,
        "the fixture loader read a different number of sentences than it did \
         when this baseline was recorded"
    );
    sentences.extend(
        fixtures
            .into_iter()
            .map(|(label, tokens)| (format!("fixture {label}"), tokens)),
    );
    sentences
}

/// Every committed sentence, in a fixed order.
///
/// Ordered by file and then by position within it so a baseline is stable: a
/// consumer keys on the label, but a reordering would still churn the diff for
/// no reason.
pub fn committed_sentences() -> Vec<LabelledSentence> {
    let mut sentences = Vec::new();
    for (name, text) in [("r7_natsume", NATSUME), ("r7_aspect", ASPECT)] {
        sentences.extend(pre_tokenized(name, text));
    }
    sentences.extend(needs_tokenizing("synthetic", SYNTHETIC));
    sentences
}

/// Sentences from a `lingrep.tokenize/v1` transcript.
fn pre_tokenized(name: &str, text: &str) -> Vec<LabelledSentence> {
    let mut sentences = Vec::new();
    for line in text.lines() {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("every fixture line is JSON");
        if value["type"] != "sentence" {
            continue;
        }
        let data = &value["data"];
        let ordinal = data["corpus_sentence_ordinal"]
            .as_u64()
            .expect("a sentence carries an ordinal");
        let tokens = data["tokens"]
            .as_array()
            .expect("a sentence carries a token array")
            .iter()
            .map(token_from_json)
            .collect();
        sentences.push((format!("{name}:{ordinal}"), tokens));
    }
    assert!(
        !sentences.is_empty(),
        "{name} contributed no sentences, so it is silently not covered"
    );
    sentences
}

/// Sentences from a corpus fixture that stores text rather than tokens.
fn needs_tokenizing(name: &str, text: &str) -> Vec<LabelledSentence> {
    let mut sentences = Vec::new();
    for (position, line) in text.lines().enumerate() {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("every fixture line is JSON");
        let Some(sentence) = value["text"].as_str() else {
            continue;
        };
        // The guard that keeps this reader honest: a fixture that gained real
        // tokens would be silently re-tokenized here, producing a different
        // answer than the transcript it now carries.
        assert!(
            value["tokens"].is_null(),
            "{name}:{position} carries tokens; read it as a transcript instead"
        );
        sentences.push((
            format!("{name}:{position}"),
            super::tokenize_japanese(sentence),
        ));
    }
    assert!(
        !sentences.is_empty(),
        "{name} contributed no sentences, so it is silently not covered"
    );
    sentences
}

/// Committed dependency-parsed sentences, as of this baseline.
///
/// Synthetic and stated as such: no committed corpus fixture in this crate
/// carries basic dependencies, and a CoNLL-U-bound catalog's `examples:`
/// cannot be tokenized into dependency structure by any packaged analyzer.
/// Four hand-annotated sentences exercise every `patterns/ud-dependency.lx`
/// pattern, including both argument orders, and a fifth carries a competing
/// capture assignment for `patterns/double-wo-dep.lx` — the shape the other
/// four cannot express, and which the counted-work screen was blind to until
/// it had a witness (`docs/design/2026-08-29-where-capture-assignment.md`).
pub const COMMITTED_CONLLU_SENTENCES: usize = 5;

const CONLLU_WITNESSES: &str = include_str!("../../tests/fixtures/ud-dependency-witness.conllu");

/// Every committed dependency-parsed sentence, in fixture order.
///
/// The CoNLL-U counterpart of [`committed_sentences`]: what a whole-catalog
/// measurement of a [`ConlluSyntax`]-bound catalog runs over. Returned as
/// parsed sentences rather than projected tokens because projection needs the
/// bound catalog's own attribute set.
///
/// [`ConlluSyntax`]: super::catalogs::CampaignBinding::ConlluSyntax
pub fn conllu_campaign_sentences() -> Vec<(String, crate::corpus::conllu::Sentence)> {
    let reader = crate::corpus::conllu::Reader::new(
        CONLLU_WITNESSES.as_bytes(),
        "ud-dependency-witness.conllu",
    );
    let sentences: Vec<(String, crate::corpus::conllu::Sentence)> = reader
        .enumerate()
        .map(|(ordinal, sentence)| {
            (
                format!("conllu_witness:{ordinal}"),
                sentence.expect("the committed witness fixture parses"),
            )
        })
        .collect();
    assert_eq!(
        sentences.len(),
        COMMITTED_CONLLU_SENTENCES,
        "the witness loader read a different number of sentences than it did \
         when this baseline was recorded"
    );
    sentences
}

fn token_from_json(token: &serde_json::Value) -> TokenInfo {
    let mut info = TokenInfo::default();
    for (key, value) in token["attributes"]
        .as_object()
        .expect("a token carries an attribute map")
    {
        // An unknown key is skipped rather than fatal: the fixture is the
        // tokenizer's full output and may name attributes the matcher has no
        // index for.
        if let Some(attribute) = published_attribute_index(key) {
            info.set(
                attribute,
                value.as_str().expect("attribute values are strings").into(),
            );
        }
    }
    info
}

/// The storage slot a **published** attribute name loads into.
///
/// The transcript writes the names a run publishes — portable `text`, `upos`,
/// `xpos`, and provider-prefixed `unidic.*` — while [`TokenAttributeIndex`]
/// still spells the physical UniDic columns. Reading a transcript with
/// `from_str` alone therefore drops every provider-prefixed column and, worse,
/// loads `text` into the physical `Text` slot, which the schema does not
/// publish and no predicate reads: `[text=…]` reads `Orth`. A fixture
/// regenerated after the `orth` → `text` rename silently lost its surface and
/// its part of speech.
pub fn published_attribute_index(key: &str) -> Option<TokenAttributeIndex> {
    match key {
        "text" => Some(TokenAttributeIndex::Orth),
        "upos" => Some(TokenAttributeIndex::Pos),
        "xpos" => Some(TokenAttributeIndex::Tag),
        other => TokenAttributeIndex::from_str(other.strip_prefix("unidic.").unwrap_or(other)).ok(),
    }
}
