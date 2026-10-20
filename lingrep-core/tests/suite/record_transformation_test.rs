// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
//! Acceptance cases for the natsume table, aspect minimal pair, and raw
//! stream.
//!
//! `policy_transform_test.rs` pins the executor's semantics against tokens
//! transcribed from probe P1's appendix. This suite is the independent check on
//! that transcription: it reads **real UniDic-CWJ tokenizations**, produced by
//! `ling tokenize` and committed under `tests/fixtures/`, so a transcription
//! that drifted from what the dictionary actually emits fails here rather than
//! passing both halves of one mistake.
//!
//! Regenerate the fixtures with:
//!
//! ```bash
//! printf '飛び立つでしょう\n考えられませんでした\n扱うかです\n突入しちゃう\nで囲んである\nたらしめている\nいるからで\nいるという\n語ります\nしました。\n作り上げたか\n見られなかったが\n' > p1_inputs.txt
//! printf '囲んでいる\n囲んである\n' > aspect_inputs.txt
//! ling tokenize --json --dict-path "$LINGREP_DICTIONARY_PATH" p1_inputs.txt \
//!   > lingrep-core/tests/fixtures/r7_natsume_tokens.jsonl
//! ling tokenize --json --dict-path "$LINGREP_DICTIONARY_PATH" aspect_inputs.txt \
//!   > lingrep-core/tests/fixtures/r7_aspect_tokens.jsonl
//! ```
//!
//! The recipe lives here rather than in
//! `docs/results/2026-08-04-r7-acceptance.md`, which recorded the six-row
//! version and is a dated record: a dated record should not silently acquire
//! a twelve-row input list it never ran.
//!
//! Both fixtures are read attribute-by-attribute through
//! `published_attribute_index`, which is the same mapping
//! `campaign_sentences` uses. A private copy is what let the `orth` → `text`
//! rename go unnoticed: the committed transcript predated it, both halves
//! agreed, and a regenerated fixture would have loaded its surface into a
//! slot no predicate reads.

use lingrep_core::execution::{
    ExecutionError, MatchSink, PolicySink, SentenceLocation, SinkControl,
};
use lingrep_core::fsm::{
    CompileLimits, CompiledCatalog, Match, PatternSource, PublishedRecord, RENDERER_REGISTRY,
    compile_catalog, verify_examples,
};
use lingrep_core::testing::campaign_sentences::published_attribute_index;
use lingrep_core::token_match::{TokenInfo, TokenSequence};

/// One sentence of a committed tokenization fixture.
///
/// Every attribute the dictionary emitted is loaded, not a chosen subset: the
/// point of reading a real tokenization is that the test does not get to decide
/// what the token carries.
fn sentence(fixture: &str, ordinal: u64) -> Vec<TokenInfo> {
    for line in fixture.lines() {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("every fixture line is JSON");
        if value["type"] != "sentence" {
            continue;
        }
        let data = &value["data"];
        if data["corpus_sentence_ordinal"] != ordinal {
            continue;
        }
        return data["tokens"]
            .as_array()
            .expect("a sentence carries a token array")
            .iter()
            .map(|token| {
                let mut info = TokenInfo::default();
                for (key, value) in token["attributes"]
                    .as_object()
                    .expect("a token carries an attribute map")
                {
                    // An unknown key is skipped rather than fatal: the fixture
                    // is the tokenizer's full output and may name attributes
                    // the matcher has no index for.
                    if let Some(attribute) = published_attribute_index(key) {
                        info.set(
                            attribute,
                            value.as_str().expect("attribute values are strings").into(),
                        );
                    }
                }
                info
            })
            .collect();
    }
    panic!("sentence ordinal {ordinal} is not in the fixture");
}

const NATSUME: &str = include_str!("../fixtures/r7_natsume_tokens.jsonl");
const ASPECT: &str = include_str!("../fixtures/r7_aspect_tokens.jsonl");

/// The guard vocabulary probe P1 declared before inspecting any tokenization.
///
/// Quoted verbatim from the results document. A guard rewritten to fit an
/// answer would make the sufficiency verdict vacuous, so these are the only
/// guards any row below may use.
const DROP_TRAILING: &str = r#""[upos in {AUX, ADP}]""#;
const DROP_LEADING: &str = r#""[upos in {CCONJ, ADP, SCONJ, PART}]""#;
const KEEP_VOICE_OR_SURU: &str = r#""[lemma in {れる, られる, 為る}]""#;
const STOP_AT_SCONJ: &str = r#""[upos=SCONJ]""#;
/// natsume's `simple_lemma` mapping.
const SURU_TABLE: &str = r#"{為る: "する"}"#;

// The three widenings the six rows beyond P1's table force. Each is still
// set membership over `upos` or `lemma`, so P1's *closed vocabulary* verdict
// stands: no row is registry-forced, and no guard flavour is new. What is new
// is which members the sets need, and every one of them is read straight off
// natsume's own source rather than fitted to an answer.

/// natsume prefilters `token.pos not in {PUNCT, SYM}` before normalizing, and
/// its `takewhile` stops at `PART` as well as `ADP`/`SCONJ`
/// (`pattern_extraction.py:83`, `:92`). Row 9 needs the punctuation half and
/// row 10 the `PART` half.
const DROP_TRAILING_WIDE: &str = r#""[upos in {AUX, ADP, PART, PUNCT, SYM}]""#;
/// Row 11 keeps a negation: natsume's pairwise loop cuts before `ます`/`た`
/// and so retains `なかっ`, which a plain trailing drop would remove.
const KEEP_VOICE_SURU_OR_NEGATION: &str = r#""[lemma in {れる, られる, 為る, ない}]""#;
/// Row 7 cuts at `と`, an `ADP`, where P1's rows only ever cut at `SCONJ`.
const STOP_AT_FUNCTION_WORD: &str = r#""[upos in {ADP, SCONJ, PART}]""#;

/// One row of the natsume ground-truth table, as a catalog.
struct Row {
    ordinal: u64,
    input: &'static str,
    expected: &'static str,
    transform: String,
}

fn rows() -> Vec<Row> {
    let drop_trailing = |keep: Option<&str>| match keep {
        Some(keep) => {
            format!(
                "{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING}, keep: {keep}}}"
            )
        }
        None => format!("{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING}}}"),
    };
    let drop_leading = format!("{{op: \"drop_edge\", edge: \"leading\", drop: {DROP_LEADING}}}");
    let truncate = format!("{{op: \"truncate_before\", stop: {STOP_AT_SCONJ}}}");
    let substitute_lemma = format!(
        "{{op: \"substitute\", component: \"last\", attr: \"lemma\", table: {SURU_TABLE}}}"
    );
    let join = "{op: \"join\"}";

    vec![
        Row {
            ordinal: 0,
            input: "飛び立つでしょう",
            expected: "飛び立つ",
            transform: format!("[{}, {join}]", drop_trailing(None)),
        },
        Row {
            ordinal: 1,
            input: "考えられませんでした",
            expected: "考えられる",
            transform: format!(
                "[{}, {substitute_lemma}, {join}]",
                drop_trailing(Some(KEEP_VOICE_OR_SURU))
            ),
        },
        Row {
            ordinal: 2,
            input: "扱うかです",
            expected: "扱う",
            transform: format!("[{}, {join}]", drop_trailing(None)),
        },
        Row {
            ordinal: 3,
            input: "突入しちゃう",
            expected: "突入する",
            transform: format!(
                "[{}, {substitute_lemma}, {join}]",
                drop_trailing(Some(KEEP_VOICE_OR_SURU))
            ),
        },
        Row {
            ordinal: 4,
            input: "で囲んである",
            expected: "囲む",
            transform: format!("[{drop_leading}, {truncate}, {substitute_lemma}, {join}]"),
        },
        Row {
            ordinal: 5,
            input: "たらしめている",
            expected: "たらしめる",
            transform: format!("[{truncate}, {substitute_lemma}, {join}]"),
        },
        // Rows 6-11 are the half of natsume's table probe P1 never saw: they
        // come from `tests/test_pattern_extraction.py`'s `NORMALIZATION_CASES`
        // rather than from the module doctest, and they exercise negation
        // retention, polite-past stacking, and quotative/causal truncation.
        Row {
            ordinal: 6,
            input: "いるからで",
            expected: "いる",
            // No substitution: natsume's `simple_lemma` returns `lemma_`
            // rather than `norm_` for 居る, so the written 終止形 is the
            // answer and a lemma substitution would produce 居る.
            transform: format!("[{truncate}, {join}]"),
        },
        Row {
            ordinal: 7,
            input: "いるという",
            expected: "いる",
            transform: format!(
                "[{{op: \"truncate_before\", stop: {STOP_AT_FUNCTION_WORD}}}, {join}]"
            ),
        },
        Row {
            ordinal: 8,
            input: "語ります",
            expected: "語る",
            transform: format!(
                "[{}, {substitute_lemma}, {join}]",
                drop_trailing(Some(KEEP_VOICE_OR_SURU))
            ),
        },
        Row {
            ordinal: 9,
            input: "しました。",
            expected: "する",
            transform: format!(
                "[{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING_WIDE}, \
                  keep: {KEEP_VOICE_OR_SURU}}}, {substitute_lemma}, {join}]"
            ),
        },
        Row {
            ordinal: 10,
            input: "作り上げたか",
            expected: "作り上げる",
            transform: format!(
                "[{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING_WIDE}}}, \
                 {substitute_lemma}, {join}]"
            ),
        },
        Row {
            ordinal: 11,
            input: "見られなかったが",
            expected: "見られない",
            transform: format!(
                "[{truncate}, {{op: \"drop_edge\", edge: \"trailing\", drop: \
                 {DROP_TRAILING}, keep: {KEEP_VOICE_SURU_OR_NEGATION}}}, {substitute_lemma}, \
                 {join}]"
            ),
        },
    ]
}

/// A catalog whose one pattern covers the supplied sentence exactly.
fn row_catalog(transform: &str) -> String {
    format!("transform: {transform}\npattern verb_complex := ^ []+ $.\n")
}

fn compile(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("<acceptance>", source),
        CompileLimits::default(),
    )
    .expect("the acceptance catalog compiles")
    .matcher
}

/// Publishes every record the catalog produces over `tokens`.
fn publish(matcher: &CompiledCatalog, tokens: &[TokenInfo]) -> Vec<PublishedRecord> {
    let mut sink = PolicySink::new(
        Collecting::default(),
        matcher.catalog_policy().clone(),
        matcher.anchor_evaluator(),
    );
    sink.push(&location(), &tokens, &matcher.find_matches(tokens))
        .expect("publication succeeds");
    sink.into_inner().records
}

/// Publishes the raw canonical stream (`--raw-matches`).
fn publish_raw(matcher: &CompiledCatalog, tokens: &[TokenInfo]) -> Vec<Match> {
    let mut sink = PolicySink::raw(
        Collecting::default(),
        matcher.catalog_policy().clone(),
        matcher.anchor_evaluator(),
    );
    sink.push(&location(), &tokens, &matcher.find_matches(tokens))
        .expect("publication succeeds");
    sink.into_inner().matches
}

#[derive(Default)]
struct Collecting {
    records: Vec<PublishedRecord>,
    matches: Vec<Match>,
}

impl MatchSink for Collecting {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.matches.extend_from_slice(matches);
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn push_records(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        let records_emitted = records.len();
        self.records.extend(records);
        Ok(SinkControl::Continue { records_emitted })
    }
}

fn location() -> SentenceLocation {
    SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: 0,
        document_id: lingrep_core::corpus::DocumentId::new(0),
        document_ordinal: 0,
        document_sentence_ordinal: 0,
        source_name: std::sync::Arc::from("r7-acceptance"),
    }
}

#[test]
fn every_natsume_ground_truth_row_is_reproduced_by_the_declared_vocabulary() {
    // Each row declares the operation list recorded for it,
    // over the tokens the dictionary actually produced, and must render the
    // unit natsume's own doctest expects.
    for row in rows() {
        let matcher = compile(&row_catalog(&row.transform));
        let tokens = sentence(NATSUME, row.ordinal);
        let records = publish(&matcher, &tokens);
        let [record] = records.as_slice() else {
            panic!(
                "{}: expected one published record, got {records:?}",
                row.input
            );
        };
        assert_eq!(
            record.unit.as_deref(),
            Some(row.expected),
            "row {} ({})",
            row.ordinal,
            row.input
        );
    }
}

#[test]
fn no_natsume_row_reaches_the_renderer_registry() {
    // P1's prediction, asserted rather than assumed: the total vocabulary
    // expresses all twelve rows — the six it measured and the six from
    // natsume's own test suite — so the escape hatch stays unused. The registry is
    // also still empty, which is what makes "unused" checkable at all — an
    // entry would have to bump `POLICY_SEMANTICS_IDENTITY`, and this
    // assertion is where a silent addition is caught.
    assert!(
        RENDERER_REGISTRY.is_empty(),
        "adding a renderer must bump POLICY_SEMANTICS_IDENTITY in the same commit"
    );
    for row in rows() {
        let matcher = compile(&row_catalog(&row.transform));
        let policy = matcher
            .catalog_policy()
            .get("verb_complex")
            .expect("the row declares a policy");
        assert!(
            policy.render.is_none(),
            "row {} took the registry path",
            row.ordinal
        );
        assert!(
            !policy.transform.is_empty(),
            "row {} declared no transform at all",
            row.ordinal
        );
    }
}

const ASPECT_CATALOG: &str = r#"
key: [$verb.lemma]
tags: {aspect: {asp_teiru: "ている", asp_tearu: "てある"}}
pattern aspect := (?<verb> [upos=VERB]) ((?<asp_teiru> [text in {て, で}] [lemma=居る]) | (?<asp_tearu> [text in {て, で}] [lemma=有る])).
"#;

#[test]
fn the_aspect_minimal_pair_shares_one_key_and_differs_only_in_its_tag() {
    // 囲んでいる and 囲んである are the same verb under two
    // aspectual auxiliaries. Keying on the verb's lemma is what lets them be
    // counted together; the tag is what keeps them distinguishable afterwards.
    // Grouping on the surface would do neither.
    let matcher = compile(ASPECT_CATALOG);

    let teiru = publish(&matcher, &sentence(ASPECT, 0));
    let tearu = publish(&matcher, &sentence(ASPECT, 1));
    let [teiru] = teiru.as_slice() else {
        panic!("囲んでいる published {teiru:?}");
    };
    let [tearu] = tearu.as_slice() else {
        panic!("囲んである published {tearu:?}");
    };

    assert_eq!(
        teiru.key, tearu.key,
        "the two realizations must carry the identical key"
    );
    assert_eq!(
        teiru.key.as_ref().map(|fields| fields
            .iter()
            .map(|field| (field.name.to_string(), field.value.clone()))
            .collect::<Vec<_>>()),
        Some(vec![("$verb.lemma".to_owned(), "囲む".to_owned())])
    );

    let tag = |record: &PublishedRecord| {
        record
            .tags
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(tag(teiru), vec![("aspect".to_owned(), "ている".to_owned())]);
    assert_eq!(tag(tearu), vec![("aspect".to_owned(), "てある".to_owned())]);
}

#[test]
fn the_aspect_catalog_compiles_without_a_capture_multiplicity_diagnostic() {
    // Capture projections reject captures that can occur more than once or
    // under a quantifier. `$verb` is neither, so silence here
    // is the acceptance condition — a diagnostic would mean the check cannot
    // tell a genuine hazard from an ordinary alternation.
    let output = compile_catalog(
        PatternSource::named("<acceptance>", ASPECT_CATALOG),
        CompileLimits::default(),
    )
    .expect("the aspect catalog compiles");
    assert!(
        output.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        output.diagnostics
    );
}

#[test]
fn the_raw_stream_is_untransformed_unkeyed_and_identical_across_catalogs() {
    // Policy is a property of publication, so the raw stream a
    // transforming catalog produces must equal the one its policy-free twin
    // produces — same spans, same names, same captures. If a transform could
    // move that, `--raw-matches` would stop being a way to see what matched.
    let transforming = compile(&row_catalog(&rows()[4].transform));
    let plain = compile("pattern verb_complex := ^ []+ $.\n");
    let tokens = sentence(NATSUME, 4);

    let raw = publish_raw(&transforming, &tokens);
    let control = publish_raw(&plain, &tokens);
    assert_eq!(
        raw, control,
        "declaring a transform must not change the canonical stream"
    );

    let [record] = raw.as_slice() else {
        panic!("expected one raw record, got {raw:?}");
    };
    assert_eq!(
        (record.pattern.as_str(), record.start, record.end),
        ("verb_complex", 0, tokens.len()),
        "the raw span keeps every token the transform would have removed"
    );

    // And the published record over the same input did move — otherwise the
    // assertion above would hold vacuously.
    let published = publish(&transforming, &tokens);
    let [published] = published.as_slice() else {
        panic!("expected one published record");
    };
    assert_eq!(published.unit.as_deref(), Some("囲む"));
    assert_eq!((published.start, published.end), (1, 2));
    assert_eq!(
        published.raw, *record,
        "raw identity travels with the published record"
    );
}

#[test]
fn documented_claims_are_reverified_against_the_records_a_transform_publishes() {
    // The post-policy example check exists because a
    // definition can match its own example and still publish nothing for it;
    // this pins that a `transform:` declaration does not make the second pass
    // skip the catalog, which is how a transform's effect on published identity
    // would go unchecked.
    //
    // Asserting a *tag's* value from an example is not expressible here: the
    // claim syntax is a list of strings. Tag assertions require a separate
    // syntax change that owns their encoding.
    let source = format!(
        "examples: [\"飛び立つでしょう\"]\noutput_name: \"published\"\ntransform: {}\npattern verb_complex := ^ []+ $.\n",
        rows()[0].transform
    );
    let matcher = compile(&source);
    let tokens = sentence(NATSUME, 0);
    let report = verify_examples(&matcher, &|text| {
        assert_eq!(
            text, "飛び立つでしょう",
            "the example reached the tokenizer"
        );
        tokens.clone()
    });

    assert_eq!(report.failures, Vec::new());
    assert_eq!(
        report.post_policy_checked, 1,
        "a transforming catalog must be re-checked, not skipped"
    );
    assert_eq!(
        report.post_policy_failures,
        Vec::new(),
        "the transformed record still publishes under the declared name"
    );
}
