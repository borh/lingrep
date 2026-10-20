//! The publication seam: what an inner sink actually receives once policy is
//! installed.
//!
//! The engine's own semantics are pinned in `policy_engine_test.rs`. What is
//! pinned here is the decoration itself — that the inner sink sees published
//! records and only published records, that raw mode still strips synthetics,
//! that statistics reach the reporting channel, and that the decorator does not
//! quietly discard the inner sink's token demand.

use std::sync::{Arc, OnceLock};

use crate::policy_helpers::record;
use compact_str::CompactString;
use lingrep_core::corpus::DocumentId;
use lingrep_core::execution::{
    CancellationFlag, ExecutionError, MatchSink, PolicySink, SentenceLocation, SinkControl,
};
use lingrep_core::fsm::{
    AnchorEvaluator, CatalogPolicy, CompileLimits, CompiledCatalog, Match, PatternSource,
    ProjectedKeyField, PublishedRecord, compile_catalog,
};
use lingrep_core::testing::token;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo, TokenSequence};

fn location() -> SentenceLocation {
    SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: 0,
        document_id: DocumentId::new(0),
        document_ordinal: 0,
        document_sentence_ordinal: 0,
        source_name: Arc::from("policy-sink-test"),
    }
}

/// Records exactly what it was handed, plus the demand it was asked for.
#[derive(Default)]
struct Recording {
    pushed: Vec<Vec<Match>>,
    batches_finished: usize,
}

impl MatchSink for Recording {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.pushed.push(matches.to_vec());
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.batches_finished += 1;
        Ok(())
    }
}

/// The anchor evaluator of a catalog that declares no anchor.
///
/// Every `PolicySink` constructor takes one, and the hand-built policies in
/// this file carry `AnchorSetDigest::default()` — which is exactly this
/// catalog's, an anchor-free one. What anchored admission *decides* is pinned
/// in `policy_engine_test`; what is pinned here is the decoration.
///
/// The matcher is a `OnceLock` rather than a per-test local because
/// `AnchorEvaluator` borrows it: a helper returning one needs a matcher that
/// outlives every sink, and the alternative is naming a local at each call site
/// purely to keep it alive.
fn anchor_free() -> AnchorEvaluator<'static> {
    static MATCHER: OnceLock<CompiledCatalog> = OnceLock::new();
    MATCHER
        .get_or_init(|| {
            compile_catalog(
                PatternSource::named("<anchor-free>", "pattern plain := [text=x].\n"),
                CompileLimits::default(),
            )
            .expect("the anchor-free catalog compiles")
            .matcher
        })
        .anchor_evaluator()
}

/// The decorated catalog: one renamed, sentence-final-admitted definition and
/// one synthesized evidence emitter.
///
/// Compiled rather than hand-built, because admission is a bound `admit`
/// statement now and a test cannot install a program on a
/// `CatalogPolicyBuilder` — `install_program` is crate-private, deliberately,
/// so a policy's program is always the one its own compilation produced.
///
/// The bodies are arbitrary: every test here pushes hand-built records, so what
/// the definitions match never runs. `carrier` exists only to give the `detach`
/// a subject, which is what synthesizes `__policy__copula`; no pushed record
/// carries its name, so that statement never fires.
const DECORATED: &str = r#"
fragment copula := [text=zzz].

output_name: "published"
pattern renamed := [text=a].
pattern other := [text=b].
pattern carrier := [text=c].

policy
  admit @renamed terminal: {exact: true, anchor: sentence_end}.
  detach @carrier with: @copula, relation: at_end, evidence: raw.
end
"#;

fn catalog() -> CatalogPolicy {
    compile_catalog(
        PatternSource::named("<decorated>", DECORATED),
        CompileLimits::default(),
    )
    .expect("the decorated catalog compiles")
    .matcher
    .catalog_policy()
    .clone()
}

/// How many records the catalog's one `admit` refused.
///
/// An `admit` records its drops per statement, making the count attributable to
/// the decision that produced it.
fn dropped_by_admit<S: MatchSink>(sink: &PolicySink<'_, S>) -> u64 {
    sink.program_stats()
        .per_operation()
        .first()
        .expect("the decorated catalog declares two statements")
        .dropped
}

#[test]
fn the_inner_sink_sees_published_records_only() {
    let mut sink = PolicySink::new(Recording::default(), catalog(), anchor_free());
    let sentence = [token("a"), token("b"), token("c")];
    let matches = [
        record("renamed", 0, 3),          // terminal-admitted, relabeled
        record("renamed", 0, 2),          // ends early — dropped
        record("__policy__copula", 2, 3), // evidence — never published
        record("other", 1, 2),            // no policy — passes through
    ];

    sink.push(&location(), &sentence, &matches)
        .expect("push succeeds");

    assert_eq!(
        dropped_by_admit(&sink),
        1,
        "the record ending one token short is refused by the `admit`"
    );

    let sink = sink.into_inner();
    assert_eq!(
        sink.pushed,
        vec![vec![record("published", 0, 3), record("other", 1, 2)]],
        "relabeled, terminal-filtered, synthetics stripped, raw order kept"
    );
}

#[test]
fn raw_mode_strips_synthetics_and_applies_nothing_else() {
    let mut sink = PolicySink::raw(Recording::default(), catalog(), anchor_free());
    let sentence = [token("a"), token("b"), token("c")];
    let matches = [
        record("renamed", 0, 2), // would fail `terminal` — kept in raw mode
        record("__policy__copula", 2, 3),
    ];

    sink.push(&location(), &sentence, &matches)
        .expect("push succeeds");
    assert!(sink.is_raw());
    // Stripping a synthetic is not a suppression; nothing is counted.
    assert_eq!(sink.stats(), Default::default());

    let sink = sink.into_inner();
    assert_eq!(
        sink.pushed,
        vec![vec![record("renamed", 0, 2)]],
        "raw output is the catalog's own canonical matches, unrenamed"
    );
}

#[test]
fn finish_batch_reaches_the_inner_sink() {
    let mut sink = PolicySink::new(Recording::default(), catalog(), anchor_free());
    sink.finish_batch().expect("batch finishes");
    assert_eq!(sink.into_inner().batches_finished, 1);
}

#[test]
fn a_policy_free_catalog_publishes_the_canonical_stream_unchanged() {
    let mut sink = PolicySink::new(
        Recording::default(),
        CatalogPolicy::default(),
        anchor_free(),
    );
    let sentence = [token("a"), token("b")];
    let matches = [record("a", 0, 1), record("b", 1, 2)];

    sink.push(&location(), &sentence, &matches)
        .expect("push succeeds");
    assert_eq!(sink.stats(), Default::default());
    assert_eq!(sink.into_inner().pushed, vec![matches.to_vec()]);
}

/// Records the derived fields exposed to an opting-in sink.
#[derive(Default)]
struct ReadingRecords {
    /// One entry per record: published name, span, unit, key, tags.
    pushed: Vec<PublishedRecord>,
    /// Set when the projecting default was taken instead — the two entry points
    /// must be distinguishable, or a sink could silently receive neither.
    projected: Vec<Vec<Match>>,
}

#[derive(Default)]
struct PartitionReadingRecords {
    pushed: Vec<Vec<PublishedRecord>>,
    partition_calls: usize,
    serial_record_calls: usize,
}

impl MatchSink for PartitionReadingRecords {
    fn wants_parallel_records_partition(&self) -> bool {
        true
    }

    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        _matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        Err(ExecutionError::Sink(
            "the applying path must retain derived records".into(),
        ))
    }

    fn push_records(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        self.serial_record_calls += 1;
        let records_emitted = records.len();
        self.pushed.push(records);
        Ok(SinkControl::Continue { records_emitted })
    }

    fn push_records_partition<V: TokenSequence + Sync>(
        &mut self,
        _pool: &rayon::ThreadPool,
        _locations: &[SentenceLocation],
        _sentences: &[V],
        records: Vec<Vec<PublishedRecord>>,
        _cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        self.partition_calls += 1;
        let controls = records
            .iter()
            .map(|records| SinkControl::Continue {
                records_emitted: records.len(),
            })
            .collect();
        self.pushed.extend(records);
        Ok((controls, false))
    }
}

impl MatchSink for ReadingRecords {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.projected.push(matches.to_vec());
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
        self.pushed.extend(records);
        Ok(SinkControl::Continue { records_emitted })
    }
}

/// A catalog that derives the unit, key, and tags from one pattern, and the
/// sentence it matches.
///
/// Compiled and matched for real rather than hand-built: `key:` projects off a
/// capture, and a hand-built [`Match`] carries none, so a hand-built fixture
/// would report every record unkeyed and pass a weaker test than it looks.
fn deriving() -> (CatalogPolicy, Vec<TokenInfo>, Vec<Match>) {
    let source = r#"
key: [$noun.lemma]
transform: [{op: "drop_edge", edge: "trailing", drop: "[upos=AUX]"}, {op: "join"}]
tags: {origin: "fixture"}
derived := (?<noun> [upos=NOUN]) [upos=AUX]*.
"#;
    let matcher = compile_catalog(
        PatternSource::named("<test>", source),
        CompileLimits::default(),
    )
    .expect("the catalog compiles")
    .matcher;
    let sentence = vec![
        attributed("研究", "研究", "NOUN"),
        attributed("です", "です", "AUX"),
    ];
    let matches = matcher.find_matches(&sentence);
    (matcher.catalog_policy().clone(), sentence, matches)
}

fn attributed(orth: &str, lemma: &str, pos: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Text, orth.into());
    token.set(TokenAttributeIndex::Lemma, lemma.into());
    token.set(TokenAttributeIndex::Pos, pos.into());
    token
}

#[test]
fn a_sink_that_reads_records_receives_what_policy_derived() {
    // `apply_policy` discards derived fields, so before
    // this entry point existed a transform could produce a unit that no
    // publishing surface could reach — the executor's output was write-only.
    let (policy, sentence, matches) = deriving();
    let mut sink = PolicySink::new(ReadingRecords::default(), policy, anchor_free());

    sink.push(&location(), &sentence, &matches)
        .expect("push succeeds");

    let inner = sink.into_inner();
    assert!(
        inner.projected.is_empty(),
        "the applying mode must not fall back to the projecting default"
    );
    let [published] = inner.pushed.as_slice() else {
        panic!("exactly one published record: {:?}", inner.pushed);
    };
    assert_eq!(published.name, "derived");
    assert_eq!((published.start, published.end), (0, 1));
    assert_eq!(published.unit.as_deref(), Some("研究"));
    assert_eq!(
        published.key.as_deref(),
        Some(
            [ProjectedKeyField {
                name: "$noun.lemma".into(),
                value: "研究".to_owned(),
            }]
            .as_slice()
        )
    );
    assert_eq!(
        published.tags,
        vec![(CompactString::new("origin"), "fixture".to_owned())]
    );
    assert_eq!(
        (
            published.raw.pattern.as_str(),
            published.raw.start,
            published.raw.end
        ),
        ("derived", 0, 2),
        "raw identity survives the transform that shortened the published span"
    );
}

#[test]
fn a_sink_that_reads_only_matches_sees_the_published_projection() {
    // The default preserves sinks that consume only spans and names and
    // keeps receiving the same stream. Pinning it is what makes adding the
    // second entry point safe rather than a silent behavior change for every
    // sink in the workspace.
    let (policy, sentence, matches) = deriving();
    let mut sink = PolicySink::new(Recording::default(), policy, anchor_free());

    sink.push(&location(), &sentence, &matches)
        .expect("push succeeds");

    let pushed = sink.into_inner().pushed;
    let [sentence_records] = pushed.as_slice() else {
        panic!("exactly one pushed sentence");
    };
    let [published] = sentence_records.as_slice() else {
        panic!("exactly one published match");
    };
    assert_eq!(
        (published.pattern.as_str(), published.start, published.end),
        ("derived", 0, 1),
        "the derived fields are dropped, the published span and name are not"
    );
}

#[test]
fn applied_partition_matches_serial_records_and_statistics() {
    let sentence = vec![token("a"), token("b"), token("c")];
    let sentences = vec![sentence.clone(), sentence.clone(), sentence.clone()];
    let locations = vec![located(3), located(4), located(5)];
    let found = vec![
        vec![record("renamed", 0, 3), record("renamed", 0, 2)],
        vec![record("other", 1, 2)],
        vec![record("renamed", 0, 2)],
    ];

    let mut serial = PolicySink::new(ReadingRecords::default(), catalog(), anchor_free());
    for ((location, sentence), matches) in locations.iter().zip(&sentences).zip(&found) {
        serial.push(location, sentence, matches).unwrap();
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .build()
        .unwrap();
    let mut parallel =
        PolicySink::new(PartitionReadingRecords::default(), catalog(), anchor_free());
    let (controls, cancelled) = parallel
        .push_partition(
            &pool,
            &locations,
            &sentences,
            &found,
            &CancellationFlag::new(),
        )
        .unwrap();

    assert!(!cancelled);
    assert_eq!(controls.len(), sentences.len());
    assert_eq!(parallel.stats(), serial.stats());
    assert_eq!(parallel.program_stats(), serial.program_stats());

    let serial_records = serial.into_inner().pushed;
    let parallel = parallel.into_inner();
    assert_eq!(parallel.partition_calls, 1);
    assert_eq!(parallel.serial_record_calls, 0);
    assert_eq!(
        parallel.pushed.into_iter().flatten().collect::<Vec<_>>(),
        serial_records
    );
}

#[test]
fn trace_keeps_an_opted_in_sink_on_the_serial_partition_path() {
    let sentence = vec![token("a"), token("b"), token("c")];
    let sentences = vec![sentence.clone(), sentence];
    let locations = vec![located(7), located(8)];
    let found = vec![vec![record("other", 1, 2)], vec![record("other", 0, 1)]];
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let mut sink = PolicySink::new(PartitionReadingRecords::default(), catalog(), anchor_free())
        .with_trace(64);

    let (controls, cancelled) = sink
        .push_partition(
            &pool,
            &locations,
            &sentences,
            &found,
            &CancellationFlag::new(),
        )
        .unwrap();

    assert!(!cancelled);
    assert_eq!(controls.len(), 2);
    assert_eq!(
        sink.trace()
            .unwrap()
            .records()
            .iter()
            .map(|record| record.sentence.as_str())
            .collect::<Vec<_>>(),
        vec!["policy-sink-test:7", "policy-sink-test:8"]
    );
    let inner = sink.into_inner();
    assert_eq!(inner.partition_calls, 0);
    assert_eq!(inner.serial_record_calls, 2);
}

#[test]
fn raw_mode_never_offers_records_because_it_derived_none() {
    let mut sink = PolicySink::raw(ReadingRecords::default(), catalog(), anchor_free());
    let sentence = [token("a"), token("b"), token("c")];

    sink.push(
        &location(),
        &sentence,
        &[record("renamed", 0, 2), record("__policy__copula", 2, 3)],
    )
    .expect("push succeeds");

    let inner = sink.into_inner();
    assert!(
        inner.pushed.is_empty(),
        "raw mode must not synthesize records"
    );
    assert_eq!(inner.projected, vec![vec![record("renamed", 0, 2)]]);
}

#[test]
fn terminal_is_measured_against_the_sentence_not_the_retained_tokens() {
    // `sentence.len()` is the true token count even under a partial demand —
    // the runner rejects a completion that changes it. This pins the reading
    // the engine relies on: a record ending at token 3 of a 5-token sentence
    // is not sentence-final, however few tokens were retained.
    let mut sink = PolicySink::new(Recording::default(), catalog(), anchor_free());
    let sentence = [token("a"), token("b"), token("c"), token("d"), token("e")];
    let matches = [record("renamed", 0, 3)];

    sink.push(&location(), &sentence, &matches)
        .expect("push succeeds");
    assert_eq!(dropped_by_admit(&sink), 1);
    assert_eq!(sink.into_inner().pushed, vec![Vec::<Match>::new()]);
}

/// A location whose two ordinals differ, so a test can tell them apart.
///
/// [`location`] numbers both from zero, which makes every field agree and would
/// let a label built from the wrong one pass.
fn located(document_sentence_ordinal: u64) -> SentenceLocation {
    SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: document_sentence_ordinal + 100,
        document_sentence_ordinal,
        ..location()
    }
}

/// The sink stamps each traced record with its sentence.
///
/// `SentenceLocation` is an `execution` type and the engine is in `fsm`, which
/// does not depend on it. The sink is the one place holding both, so it formats
/// the label and the engine stays free of the layer above it — which means
/// nothing below this seam can pin the format, and nothing above it can.
///
/// Two sentences, because the label is sink state: one that stamped only the
/// first sentence, or stamped once and never again, publishes the same records.
#[test]
fn a_traced_record_carries_its_sentence_label() {
    let mut sink = PolicySink::new(Recording::default(), catalog(), anchor_free()).with_trace(64);
    let sentence = [token("a"), token("b"), token("c")];

    sink.push(&located(7), &sentence, &[record("other", 1, 2)])
        .expect("push succeeds");
    sink.push(&located(8), &sentence, &[record("other", 0, 1)])
        .expect("push succeeds");

    let labels: Vec<&str> = sink
        .trace()
        .expect("a trace was installed")
        .records()
        .iter()
        .map(|traced| traced.sentence.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["policy-sink-test:7", "policy-sink-test:8"],
        "`source_name:document_sentence_ordinal` locates a sentence in the file \
         a researcher has open, which the corpus ordinal does not"
    );
}

/// A trace on a raw sink is accepted and collects nothing.
///
/// Raw publication applies no policy, so there is nothing to explain. The
/// constructor documents that it accepts the request rather than refusing it —
/// the selection is the caller's, and a sink that rejected a harmless one would
/// push that branch into every caller. An accepted request that quietly
/// collected *something* would be worse than a refusal, so pin the emptiness.
#[test]
fn a_trace_on_a_raw_sink_collects_nothing() {
    let mut sink = PolicySink::raw(Recording::default(), catalog(), anchor_free()).with_trace(64);

    sink.push(
        &located(7),
        &[token("a"), token("b"), token("c")],
        &[record("renamed", 0, 2), record("__policy__copula", 2, 3)],
    )
    .expect("push succeeds");

    assert!(
        sink.trace()
            .expect("a trace was installed")
            .records()
            .is_empty(),
        "raw mode judged nothing, so it has nothing to explain"
    );
    assert!(
        !sink.trace().expect("a trace was installed").truncated(),
        "collecting nothing is not a truncation"
    );
}

/// A catalog whose anchor strips a **fixed-width** two-token suffix.
///
/// Not `PUNCT_ANCHOR`, and the difference is what the sink test can detect.
/// With `exclude: [upos=PUNCT]+`, evaluating the anchor over a sentence
/// truncated by one token returns the *original* answer under both branches: if
/// the punctuation run starts at `s < n-1` the truncated slice still ends in the
/// same run, and if `s == n-1` the truncated slice has no punctuation suffix and
/// falls back to `tokens.len() == n-1 == s`. A longer trailing run does not
/// help — the greedy shape is the problem, not its length. A fixed-width
/// `exclude` moves with the slice, so `[] []` over four tokens answers `2`
/// whole and `1` truncated.
const FIXED_WIDTH_ANCHOR: &str = r#"
anchor: {name: "propositional_end", exclude: "LAST_TWO"}
max_nesting: {depth: 0, witness: ["これは重要で|ある。"], divergence: ["これは重要である|（田中）。"]}
fragment LAST_TWO := [] [].

pattern toiu := [text=という].

policy
  admit @toiu terminal: {exact: true, anchor: propositional_end}.
end
"#;

/// The seam evaluates the catalog's anchor, over the sentence it was handed.
///
/// Every other test in this file uses an anchor-free catalog, so a sink that
/// held its evaluator and never called it — or called it against the wrong
/// tokens — would be invisible here: `anchor_free()` answers the empty set and
/// no `admit` references it. This is the production path for the whole feature,
/// and `find_published_matches` does not exercise it: that is a separate seam,
/// which is also why `each_definition_measures_against_the_anchor_it_names`
/// does not cover this despite using the same `exclude` shape.
///
/// Both halves of "called it against the wrong tokens" are pinned: an empty
/// slice makes the anchor fall back to `0`, and a slice one token short moves it
/// to `1`. Neither equals the record's end, so both are refused and both fail
/// this assertion.
#[test]
fn the_sink_admits_against_the_declared_anchor() {
    let matcher = compile_catalog(
        PatternSource::named("<anchored>", FIXED_WIDTH_ANCHOR),
        CompileLimits::default(),
    )
    .expect("the anchored catalog compiles")
    .matcher;
    // Four tokens, of which the anchor strips the last two: the record ends at
    // token 2, which is exactly the anchor and two short of the raw end.
    let sentence = crate::policy_helpers::tokens_with_orth_and_pos(&[
        ("研究", "NOUN"),
        ("という", "NOUN"),
        ("もの", "NOUN"),
        ("。", "PUNCT"),
    ]);
    let matches = matcher.find_matches(&sentence);
    let mut sink = PolicySink::new(
        Recording::default(),
        matcher.catalog_policy().clone(),
        matcher.anchor_evaluator(),
    );

    sink.push(&location(), &sentence, &matches)
        .expect("push succeeds");

    assert_eq!(dropped_by_admit(&sink), 0);
    assert_eq!(
        sink.into_inner().pushed,
        vec![vec![record("toiu", 1, 2)]],
        "the record ends at the anchor, two tokens short of the raw end"
    );
}

/// The sink reports an unresolvable anchor as an `ExecutionError`, not silence.
///
/// `a_rejected_call_leaves_statistics_untouched` in `policy_engine_test` pins
/// the engine's refusal; what is pinned here is the seam's wrapping of it —
/// `ExecutionError::PolicyAnchor` and the `map_err` that produces it, which
/// nothing else asserts. Pairing a catalog's policy with a structurally
/// different catalog's evaluator is the wiring mistake the whole digest check
/// exists to catch, and it is only reachable through a constructor.
#[test]
fn the_sink_refuses_positions_from_a_different_catalog() {
    let punct = crate::policy_helpers::punct_anchor_matcher();
    let sym = crate::policy_helpers::sym_anchor_matcher();
    let sentence =
        crate::policy_helpers::tokens_with_orth_and_pos(&[("という", "NOUN"), ("。", "PUNCT")]);
    let matches = punct.find_matches(&sentence);
    let mut sink = PolicySink::new(
        Recording::default(),
        punct.catalog_policy().clone(),
        sym.anchor_evaluator(),
    );

    let error = sink
        .push(&location(), &sentence, &matches)
        .expect_err("mismatched positions are refused");

    match error {
        ExecutionError::PolicyAnchor(inner) => {
            assert_eq!(inner.anchor, "propositional_end");
            assert_eq!(inner.cause, lingrep_core::fsm::AnchorError::CatalogMismatch);
        }
        other => panic!("expected a policy-anchor failure, got {other:?}"),
    }
    assert!(
        sink.into_inner().pushed.is_empty(),
        "nothing is published from a sentence the engine declined to judge"
    );
}
