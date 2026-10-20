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
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use lingrep_core::analysis::{
    AnalyzerPlan, InputUnitId, NormalizationPolicy, TextUnit, VibratoPlan, VibratoSession,
};
use lingrep_core::corpus::DocumentId;
use lingrep_core::execution::{
    AnalysisBinding, BoundSentenceInput, CancellationFlag, DEFAULT_MAX_INPUT_RECORD_BYTES,
    EnhancedGraphInput, ExecutionBatch, ExecutionError, ExecutionLimits, ExecutionProgress,
    ExecutionReport, ExecutionSentence, MatchSink, ProgressSink, RunOutcome, SentenceAdmission,
    SentenceBatchSource, SentenceLocation, SinkControl, TextIteratorSource,
    TokenizedBatchObservation, TokenizedBatchObserver, estimate_token_info_bytes, execute,
    execute_analyzed,
};
use lingrep_core::fsm::{CompileLimits, CompiledCatalog, Match, PatternSource, compile_catalog};
use lingrep_core::testing::token;
use lingrep_core::token_match::{
    AttributeKind, BoundTokenSentence, SchemaAttribute, TokenAttributeIndex, TokenInfo,
    TokenSchema, TokenSequence, tokenize_bound,
};

fn matcher(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("runner.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

fn pool() -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap()
}

fn location(
    corpus_sentence_ordinal: u64,
    source_name: &str,
    document_sentence_ordinal: u64,
) -> SentenceLocation {
    SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal,
        document_id: DocumentId::new(corpus_sentence_ordinal + 10),
        document_ordinal: corpus_sentence_ordinal + 20,
        document_sentence_ordinal,
        source_name: Arc::from(source_name),
    }
}

fn located_text_batch(location: SentenceLocation, value: &str) -> ExecutionBatch {
    let id = InputUnitId::new(location.corpus_sentence_ordinal);
    ExecutionBatch::Text(vec![ExecutionSentence {
        location,
        value: TextUnit::sentence(id, value, NormalizationPolicy::None).unwrap(),
    }])
}

fn located_tokenized_batch(location: SentenceLocation, value: &str) -> ExecutionBatch {
    ExecutionBatch::tokenized(vec![ExecutionSentence {
        location,
        value: vec![token(value)],
    }])
}

struct RecordingSource {
    pulls: Arc<AtomicUsize>,
    batches: VecDeque<ExecutionBatch>,
}

impl RecordingSource {
    fn new(pulls: Arc<AtomicUsize>, batches: Vec<ExecutionBatch>) -> Self {
        Self {
            pulls,
            batches: batches.into(),
        }
    }
}

impl SentenceBatchSource for RecordingSource {
    fn wants_lookahead(&self) -> bool {
        false
    }

    fn next_batch(
        &mut self,
        _limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        Ok(self.batches.pop_front())
    }
}

#[derive(Default)]
struct ProtocolRecordingSink {
    locations: Vec<SentenceLocation>,
    matches: Vec<Vec<Match>>,
    published_records: u64,
    push_calls: usize,
    finish_calls: usize,
    stop_after: Option<usize>,
    finish_error: bool,
    cancel: Option<CancellationFlag>,
}

impl ProtocolRecordingSink {
    fn stop_after(pushes: usize) -> Self {
        Self {
            stop_after: Some(pushes),
            ..Self::default()
        }
    }

    fn with_finish_error() -> Self {
        Self {
            finish_error: true,
            ..Self::default()
        }
    }

    fn cancel_during_push(cancel: CancellationFlag) -> Self {
        Self {
            cancel: Some(cancel),
            ..Self::default()
        }
    }

    fn stop_and_cancel_after(pushes: usize, cancel: CancellationFlag) -> Self {
        Self {
            stop_after: Some(pushes),
            cancel: Some(cancel),
            ..Self::default()
        }
    }
}

impl MatchSink for ProtocolRecordingSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.push_calls += 1;
        self.locations.push(location.clone());
        self.matches.push(matches.to_vec());
        self.published_records += matches.len() as u64;
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Ok(if self.stop_after == Some(self.push_calls) {
            SinkControl::StopAfter {
                records_emitted: matches.len(),
            }
        } else {
            SinkControl::Continue {
                records_emitted: matches.len(),
            }
        })
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.finish_calls += 1;
        if self.finish_error {
            Err(ExecutionError::Sink("planned finish failure".to_owned()))
        } else {
            Ok(())
        }
    }
}

fn run_protocol_fixture<S: SentenceBatchSource + Send>(
    source: S,
    sink: &mut ProtocolRecordingSink,
    cancel: &CancellationFlag,
    logical_batch_sentences: usize,
) -> Result<RunOutcome, ExecutionError> {
    execute(
        source,
        |text| vec![token(text)],
        &matcher("plain := [text=猫]."),
        &pool(),
        &ExecutionLimits::default(),
        logical_batch_sentences,
        cancel,
        sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
}

#[test]
fn canonical_vibrato_session_preserves_bounded_matcher_batches() {
    let compiled = matcher("plain := [text=猫].");
    let plan = VibratoPlan::new(lingrep_core::testing::test_tokenizer().clone());
    let bound = compiled.bind(plan.schema()).unwrap();
    let source = RecordingSource::new(
        Arc::new(AtomicUsize::new(0)),
        vec![ExecutionBatch::Text(vec![
            ExecutionSentence {
                location: location(0, "direct", 0),
                value: TextUnit::sentence(InputUnitId::new(0), "猫", NormalizationPolicy::None)
                    .unwrap(),
            },
            ExecutionSentence {
                location: location(1, "direct", 1),
                value: TextUnit::sentence(InputUnitId::new(1), "犬", NormalizationPolicy::None)
                    .unwrap(),
            },
        ])],
    );
    let mut sink = ProtocolRecordingSink::default();
    let mut observations = Vec::new();

    let outcome = execute_analyzed(
        source,
        AnalysisBinding::Session(plan.create_session()),
        &bound,
        &pool(),
        &ExecutionLimits::default(),
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut |observation: TokenizedBatchObservation| {
            observations.push(observation);
            Ok(())
        },
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].sentence_count, 2);
    assert_eq!(
        sink.matches.iter().map(Vec::len).collect::<Vec<_>>(),
        [1, 0]
    );
}

#[test]
fn prepared_bound_batches_execute_without_an_analyzer_session() {
    let compiled = matcher("plain := [text=猫].");
    let schema = lingrep_core::token_match::TokenSchema::unidic_morphology();
    let bound = compiled.bind(&schema).unwrap();
    let sentence = tokenize_bound(
        "猫",
        lingrep_core::testing::test_tokenizer(),
        bound.projection(),
    )
    .unwrap();
    let charge = sentence.resident_bytes().unwrap();
    let source = RecordingSource::new(
        Arc::new(AtomicUsize::new(0)),
        vec![ExecutionBatch::bound_tokenized_with_charges(
            vec![ExecutionSentence {
                location: location(0, "prepared.vp", 0),
                value: BoundSentenceInput::new(sentence, EnhancedGraphInput::NotRequested),
            }],
            vec![charge],
        )],
    );
    let mut sink = ProtocolRecordingSink::default();

    let outcome = execute_analyzed::<_, VibratoSession, _, _, _>(
        source,
        AnalysisBinding::Prepared(schema),
        &bound,
        &pool(),
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(sink.matches.iter().map(Vec::len).collect::<Vec<_>>(), [1]);
}

/// A prepared artifact is not necessarily a Vibrato one, and the executor
/// must take its schema from the run rather than assume the analyzer that
/// happened to exist first.
///
/// Before `AnalysisBinding`, a prepared run with no session fell back to
/// `TokenSchema::unidic_morphology()` and then rejected its own batches:
/// `corpus build --analyzer sudachi` published a well-formed artifact that
/// every read of failed with `bound lingrep patterns schema does not match the analyzer
/// schema`. The schema here is deliberately neither Vibrato's nor any
/// analyzer this build can construct — the property is that the *run* names
/// it, not that the executor recognizes it.
#[test]
fn a_prepared_run_executes_under_its_own_non_vibrato_schema() {
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    assert_ne!(
        schema.digest(),
        TokenSchema::unidic_morphology().digest(),
        "the fixture must not be the schema the old fallback assumed"
    );
    let bound = matcher("noun := [upos=NOUN].").bind(&schema).unwrap();
    let sentence = BoundTokenSentence::from_projected_values(
        bound.projection(),
        2,
        ["NOUN", "VERB"]
            .into_iter()
            .map(|value| Some(value.into()))
            .collect(),
    )
    .unwrap();
    let charge = sentence.resident_bytes().unwrap();
    let source = RecordingSource::new(
        Arc::new(AtomicUsize::new(0)),
        vec![ExecutionBatch::bound_tokenized_with_charges(
            vec![ExecutionSentence {
                location: location(0, "prepared.lingcorpus", 0),
                value: BoundSentenceInput::new(sentence, EnhancedGraphInput::NotRequested),
            }],
            vec![charge],
        )],
    );
    let mut sink = ProtocolRecordingSink::default();

    let outcome = execute_analyzed::<_, VibratoSession, _, _, _>(
        source,
        AnalysisBinding::Prepared(schema),
        &bound,
        &pool(),
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(sink.matches.iter().map(Vec::len).collect::<Vec<_>>(), [1]);
}

/// The guard the fallback was standing in for is still there, and now
/// contests the schema the run actually named.
#[test]
fn a_prepared_run_refuses_a_binding_from_another_schema() {
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    let bound = matcher("noun := [upos=NOUN].").bind(&schema).unwrap();
    let source = RecordingSource::new(Arc::new(AtomicUsize::new(0)), Vec::new());
    let mut sink = ProtocolRecordingSink::default();

    let error = execute_analyzed::<_, VibratoSession, _, _, _>(
        source,
        AnalysisBinding::Prepared(TokenSchema::unidic_morphology()),
        &bound,
        &pool(),
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(
        matches!(error, ExecutionError::Source { message, .. }
            if message.contains("does not match")),
        "a mismatched prepared schema must still be refused"
    );
}

#[test]
fn location_survives_text_and_tokenized_sources() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let text_location = location(7, "text/a.md", 2);
    let tokenized_location = location(8, "cache/b.md", 3);
    let source = RecordingSource::new(
        Arc::clone(&pulls),
        vec![
            located_text_batch(text_location.clone(), "猫"),
            located_tokenized_batch(tokenized_location.clone(), "猫"),
        ],
    );
    let mut sink = ProtocolRecordingSink::default();

    let outcome = run_protocol_fixture(source, &mut sink, &CancellationFlag::new(), 2).unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(sink.locations, [text_location, tokenized_location]);
    assert_eq!(pulls.load(Ordering::SeqCst), 3);
}

#[test]
fn stop_after_first_push_does_not_pull_another_batch() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let expected = location(0, "a.md", 0);
    let source = RecordingSource::new(
        Arc::clone(&pulls),
        vec![
            located_tokenized_batch(expected.clone(), "猫"),
            located_tokenized_batch(location(1, "b.md", 0), "猫"),
        ],
    );
    let mut sink = ProtocolRecordingSink::stop_after(1);

    let outcome = run_protocol_fixture(source, &mut sink, &CancellationFlag::new(), 10).unwrap();
    let RunOutcome::Completed(report) = outcome else {
        panic!("intentional stop must complete the published prefix");
    };

    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    assert_eq!(sink.locations, vec![expected]);
    assert_eq!(report.records_emitted, 1);
}

/// Opts an inner source into the runner's one-batch lookahead, which
/// [`SentenceBatchSource`] declines by default.
struct Lookahead<S>(S);

impl<S: SentenceBatchSource> SentenceBatchSource for Lookahead<S> {
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        self.0.next_batch(limits)
    }

    fn wants_lookahead(&self) -> bool {
        true
    }
}

struct FailingAfterSource {
    batches: VecDeque<ExecutionBatch>,
}

impl SentenceBatchSource for FailingAfterSource {
    fn wants_lookahead(&self) -> bool {
        false
    }

    fn next_batch(
        &mut self,
        _limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        match self.batches.pop_front() {
            Some(batch) => Ok(Some(batch)),
            None => Err(ExecutionError::Source {
                ordinal: 99,
                message: "planned source failure".to_owned(),
            }),
        }
    }
}

#[test]
fn lookahead_preserves_batch_order() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let expected = [
        location(0, "a.md", 0),
        location(1, "b.md", 0),
        location(2, "c.md", 0),
    ];
    let source = Lookahead(RecordingSource::new(
        Arc::clone(&pulls),
        expected
            .iter()
            .map(|location| located_tokenized_batch(location.clone(), "猫"))
            .collect(),
    ));
    let mut sink = ProtocolRecordingSink::default();

    let outcome = run_protocol_fixture(source, &mut sink, &CancellationFlag::new(), 1).unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(sink.locations, expected);
    assert_eq!(pulls.load(Ordering::SeqCst), 4);
}

#[test]
fn lookahead_delivers_every_earlier_batch_before_a_source_error() {
    let expected = [location(0, "a.md", 0), location(1, "b.md", 0)];
    let source = Lookahead(FailingAfterSource {
        batches: expected
            .iter()
            .map(|location| located_tokenized_batch(location.clone(), "猫"))
            .collect(),
    });
    let mut sink = ProtocolRecordingSink::default();

    let error = run_protocol_fixture(source, &mut sink, &CancellationFlag::new(), 1).unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::Source { ordinal: 99, ref message }
            if message == "planned source failure"
    ));
    assert_eq!(sink.locations, expected);
}

#[test]
fn lookahead_pulls_at_most_one_batch_past_an_early_stop() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let expected = location(0, "a.md", 0);
    let source = Lookahead(RecordingSource::new(
        Arc::clone(&pulls),
        vec![
            located_tokenized_batch(expected.clone(), "猫"),
            located_tokenized_batch(location(1, "b.md", 0), "猫"),
            located_tokenized_batch(location(2, "c.md", 0), "猫"),
        ],
    ));
    let mut sink = ProtocolRecordingSink::stop_after(1);

    let outcome = run_protocol_fixture(source, &mut sink, &CancellationFlag::new(), 1).unwrap();
    let RunOutcome::Completed(report) = outcome else {
        panic!("intentional stop must complete the published prefix");
    };

    assert_eq!(sink.locations, vec![expected]);
    assert_eq!(report.records_emitted, 1);
    let pulled = pulls.load(Ordering::SeqCst);
    assert!(
        pulled <= 2,
        "lookahead is one batch deep, so a stop after the first batch reads at most two: {pulled}"
    );
}

#[derive(Clone, Copy)]
enum ReportControl {
    Continue,
    Stop,
}

struct CountReportingSink {
    expected_supplied: usize,
    reported: usize,
    control: ReportControl,
    pushes: usize,
}

impl MatchSink for CountReportingSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        assert_eq!(matches.len(), self.expected_supplied);
        self.pushes += 1;
        Ok(match self.control {
            ReportControl::Continue => SinkControl::Continue {
                records_emitted: self.reported,
            },
            ReportControl::Stop => SinkControl::StopAfter {
                records_emitted: self.reported,
            },
        })
    }
}

fn run_count_reporting_fixture(
    supplied: usize,
    reported: usize,
    control: ReportControl,
) -> (CountReportingSink, Result<RunOutcome, ExecutionError>) {
    let (source, _) = StaticSource::new([ExecutionBatch::tokenized(vec![ExecutionSentence {
        location: location(0, "a.md", 0),
        value: (0..supplied).map(|_| token("猫")).collect(),
    }])]);
    let mut sink = CountReportingSink {
        expected_supplied: supplied,
        reported,
        control,
        pushes: 0,
    };
    let outcome = execute(
        source,
        |_| unreachable!("tokenized source bypasses tokenization"),
        &matcher("plain := [text=猫]."),
        &pool(),
        &ExecutionLimits::default(),
        10,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    );
    (sink, outcome)
}

/// A sink that keeps running while publishing less than it was handed must be
/// able to say so.
///
/// `Continue` used to carry no count, so the runner substituted the supplied
/// slice length: every filtering sink — anything that drops, dedupes, or
/// selects — was silently credited with emitting records it discarded. The
/// substitution was invisible precisely because it was always plausible, and
/// `StopAfter` already proved the accounting could be honest for the one case
/// that happened to terminate the run.
#[test]
fn continuing_sink_reports_the_records_it_emitted_not_the_records_supplied() {
    // Publishes one of the two and keeps going: the shape of every
    // deduplicating or selecting sink in the workspace.
    let (sink, outcome) = run_count_reporting_fixture(2, 1, ReportControl::Continue);
    let outcome = outcome.unwrap();
    let RunOutcome::Completed(report) = outcome else {
        panic!("a filtering sink does not stop the run")
    };

    assert_eq!(sink.pushes, 1);
    assert_eq!(
        report.matches_found, 2,
        "the matcher's own count is unaffected by what the sink published"
    );
    assert_eq!(
        report.records_emitted, 1,
        "records_emitted must count what the sink emitted, not what it was handed"
    );
}

/// A continuing sink cannot claim more than it was handed.
///
/// `StopAfter` has always been validated this way. Leaving `Continue`
/// unvalidated would make the honest count unenforceable in exactly the case
/// that runs for a whole corpus rather than terminating after one sentence.
#[test]
fn a_continuing_sink_claiming_more_than_supplied_is_a_sink_error() {
    let (_, outcome) = run_count_reporting_fixture(1, 99, ReportControl::Continue);
    let error = outcome.expect_err("an impossible emission count is a protocol violation");

    assert!(
        matches!(&error, ExecutionError::Sink(message)
            if message.contains("99") && message.contains('1')),
        "the error must name both the claim and the slice: {error}"
    );
}

#[test]
fn stopping_sink_reports_only_the_successfully_published_match_prefix() {
    let (sink, outcome) = run_count_reporting_fixture(2, 1, ReportControl::Stop);
    let outcome = outcome.unwrap();
    let RunOutcome::Completed(report) = outcome else {
        panic!("intentional prefix stop completes")
    };

    assert_eq!(sink.pushes, 1);
    assert_eq!(report.matches_found, 2);
    assert_eq!(report.records_emitted, 1);
}

#[test]
fn stop_after_first_push_prevents_another_partition_push() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let expected = location(0, "a.md", 0);
    let source = RecordingSource::new(
        Arc::clone(&pulls),
        vec![ExecutionBatch::tokenized(vec![
            ExecutionSentence {
                location: expected.clone(),
                value: vec![token("猫")],
            },
            ExecutionSentence {
                location: location(1, "a.md", 1),
                value: vec![token("猫")],
            },
        ])],
    );
    let mut sink = ProtocolRecordingSink::stop_after(1);

    let outcome = run_protocol_fixture(source, &mut sink, &CancellationFlag::new(), 1).unwrap();
    let RunOutcome::Completed(report) = outcome else {
        panic!("intentional stop must complete the published prefix");
    };

    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    assert_eq!(sink.locations, vec![expected]);
    assert_eq!(report.records_emitted, 1);
}

#[test]
fn stop_and_cancellation_at_logical_boundary_complete_the_successful_prefix() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let expected = location(0, "a.md", 0);
    let source = RecordingSource::new(
        Arc::clone(&pulls),
        vec![
            ExecutionBatch::tokenized(vec![
                ExecutionSentence {
                    location: expected.clone(),
                    value: vec![token("猫")],
                },
                ExecutionSentence {
                    location: location(1, "a.md", 1),
                    value: vec![token("猫")],
                },
            ]),
            located_tokenized_batch(location(2, "b.md", 0), "猫"),
        ],
    );
    let cancel = CancellationFlag::new();
    let mut sink = ProtocolRecordingSink::stop_and_cancel_after(1, cancel.clone());

    let outcome = run_protocol_fixture(source, &mut sink, &cancel, 1).unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Completed(expected_report(1, 1, 1, 1, 1))
    );
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    assert_eq!(sink.locations, vec![expected]);
    assert_eq!(sink.push_calls, 1);
    assert_eq!(sink.finish_calls, 1);
}

#[test]
fn finish_batch_cannot_erase_a_successful_push() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let source = RecordingSource::new(
        pulls,
        vec![located_tokenized_batch(location(0, "a.md", 0), "猫")],
    );
    let mut sink = ProtocolRecordingSink::with_finish_error();

    let error = run_protocol_fixture(source, &mut sink, &CancellationFlag::new(), 1).unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::Sink(ref message) if message == "planned finish failure"
    ));
    assert_eq!(sink.published_records, 1);
    assert_eq!(sink.push_calls, 1);
}

#[test]
fn cancellation_requested_inside_push_is_observed_before_the_next_push() {
    let cancel = CancellationFlag::new();
    let pulls = Arc::new(AtomicUsize::new(0));
    let source = RecordingSource::new(
        pulls,
        vec![ExecutionBatch::tokenized(vec![
            ExecutionSentence {
                location: location(0, "a.md", 0),
                value: vec![token("猫")],
            },
            ExecutionSentence {
                location: location(1, "a.md", 1),
                value: vec![token("猫")],
            },
        ])],
    );
    let mut sink = ProtocolRecordingSink::cancel_during_push(cancel.clone());

    let outcome = run_protocol_fixture(source, &mut sink, &cancel, 10).unwrap();

    assert!(matches!(outcome, RunOutcome::Cancelled(_)));
    assert_eq!(sink.published_records, 1);
    assert_eq!(sink.push_calls, 1);
}

fn expected_report(
    sentences_admitted: u64,
    sentences_matched: u64,
    tokens_matched: u64,
    matches_found: u64,
    records_emitted: u64,
) -> ExecutionReport {
    ExecutionReport {
        policy_stats: lingrep_core::fsm::PolicyStats::default(),
        program_stats: lingrep_core::fsm::ProgramStats::default(),
        trace: None,
        sentences_admitted,
        sentences_matched,
        tokens_matched,
        matches_found,
        records_emitted,
    }
}

#[test]
fn generic_execution_preserves_direct_canonical_matches() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let sentences = [vec![token("a")], vec![token("b")]];
    let source = RecordingSource::new(
        pulls,
        vec![ExecutionBatch::tokenized(
            sentences
                .iter()
                .cloned()
                .enumerate()
                .map(|(ordinal, value)| ExecutionSentence {
                    location: location(ordinal as u64, "recording.lingcorpus", ordinal as u64),
                    value,
                })
                .collect(),
        )],
    );
    let fsm = matcher("plain := [text=a].");
    let mut sink = ProtocolRecordingSink::default();

    let outcome = execute(
        source,
        |_| unreachable!("tokenized source bypasses tokenization"),
        &fsm,
        &pool(),
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(
        sink.matches,
        sentences
            .iter()
            .map(|sentence| fsm.find_matches(sentence))
            .collect::<Vec<_>>()
    );
}

#[test]
fn cancellation_prevents_the_next_batch() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let source = RecordingSource::new(
        Arc::clone(&pulls),
        vec![
            located_tokenized_batch(location(0, "recording.lingcorpus", 0), "a"),
            located_tokenized_batch(location(1, "recording.lingcorpus", 1), "a"),
        ],
    );
    let cancel = CancellationFlag::new();
    let mut sink = ProtocolRecordingSink::cancel_during_push(cancel.clone());
    let mut progress_updates = Vec::new();

    let outcome = execute(
        source,
        |_| unreachable!("tokenized source bypasses tokenization"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        1,
        &cancel,
        &mut sink,
        &mut |update: ExecutionProgress, _sink: &ProtocolRecordingSink| {
            progress_updates.push(update);
        },
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Cancelled(expected_report(1, 1, 1, 1, 1))
    );
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    assert_eq!(sink.matches.len(), 1);
    assert_eq!(
        progress_updates,
        [ExecutionProgress {
            sentences_admitted: 1,
            sentences_matched: 1,
            tokens_matched: 1,
            matches_found: 1,
            records_emitted: 1,
        }]
    );
}

#[test]
fn parallel_default_matches_the_independent_serial_transcript() {
    let sentences = [
        vec![token("a")],
        vec![token("a"), token("b")],
        vec![token("x")],
        Vec::new(),
    ];
    let fsm = matcher("pattern plain := [text=a].\npattern captured := (?<unit> [text=a]).");
    let expected_matches = sentences
        .iter()
        .map(|sentence| fsm.find_matches(sentence))
        .collect::<Vec<_>>();
    let expected_locations = (0..sentences.len())
        .map(|ordinal| location(ordinal as u64, "recording.lingcorpus", ordinal as u64))
        .collect::<Vec<_>>();

    for threads in [1, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        for logical_batch_sentences in [1, 3, 17] {
            let source = RecordingSource::new(
                Arc::new(AtomicUsize::new(0)),
                vec![ExecutionBatch::tokenized(
                    expected_locations
                        .iter()
                        .cloned()
                        .zip(sentences.iter().cloned())
                        .map(|(location, value)| ExecutionSentence { location, value })
                        .collect(),
                )],
            );
            let mut sink = ProtocolRecordingSink::default();

            let outcome = execute(
                source,
                |_| unreachable!("tokenized source bypasses tokenization"),
                &fsm,
                &pool,
                &ExecutionLimits::default(),
                logical_batch_sentences,
                &CancellationFlag::new(),
                &mut sink,
                &mut NoopProgress,
                &mut NoopObserver,
                lingrep_core::execution::PolicySelection::Apply,
                &SentenceAdmission::All,
            )
            .unwrap();

            assert!(matches!(outcome, RunOutcome::Completed(_)));
            assert_eq!(sink.locations, expected_locations);
            assert_eq!(sink.matches, expected_matches);
        }
    }
}

#[test]
fn generic_execution_freezes_ordinals_progress_and_lifecycle_across_sources_and_threads() {
    let expected_locations = vec![
        location(0, "recording.lingcorpus", 0),
        location(1, "recording.lingcorpus", 1),
        location(2, "recording.lingcorpus", 2),
    ];
    let expected_progress = vec![
        ExecutionProgress {
            sentences_admitted: 2,
            sentences_matched: 2,
            tokens_matched: 2,
            matches_found: 1,
            records_emitted: 1,
        },
        ExecutionProgress {
            sentences_admitted: 3,
            sentences_matched: 3,
            tokens_matched: 3,
            matches_found: 2,
            records_emitted: 2,
        },
    ];
    let mut baseline = None;

    for source_variant in ["compressed", "uncompressed"] {
        for threads in [1, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let source = RecordingSource::new(
                Arc::new(AtomicUsize::new(0)),
                vec![
                    ExecutionBatch::tokenized(vec![
                        ExecutionSentence {
                            location: expected_locations[0].clone(),
                            value: vec![token("a")],
                        },
                        ExecutionSentence {
                            location: expected_locations[1].clone(),
                            value: vec![token("b")],
                        },
                    ]),
                    ExecutionBatch::tokenized(vec![ExecutionSentence {
                        location: expected_locations[2].clone(),
                        value: vec![token("a")],
                    }]),
                ],
            );
            let mut sink = ProtocolRecordingSink::default();
            let mut progress = Vec::new();

            let outcome = execute(
                source,
                |_| unreachable!("tokenized source bypasses tokenization"),
                &matcher("plain := [text=a]."),
                &pool,
                &ExecutionLimits::default(),
                2,
                &CancellationFlag::new(),
                &mut sink,
                &mut |update: ExecutionProgress, _sink: &ProtocolRecordingSink| {
                    progress.push(update);
                },
                &mut NoopObserver,
                lingrep_core::execution::PolicySelection::Apply,
                &SentenceAdmission::All,
            )
            .unwrap();

            assert_eq!(
                outcome,
                RunOutcome::Completed(expected_report(3, 3, 3, 2, 2))
            );
            assert_eq!(sink.locations, expected_locations);
            assert_eq!(sink.finish_calls, 2);
            assert_eq!(progress, expected_progress);
            let transcript = (sink.locations, sink.matches, sink.finish_calls, progress);
            if let Some(baseline) = &baseline {
                assert_eq!(
                    &transcript, baseline,
                    "source={source_variant} threads={threads} must preserve the lifecycle transcript"
                );
            } else {
                baseline = Some(transcript);
            }
        }
    }
}

#[test]
fn pre_cancelled_execution_decodes_and_publishes_nothing() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let source = RecordingSource::new(
        Arc::clone(&pulls),
        vec![located_tokenized_batch(
            location(0, "recording.lingcorpus", 0),
            "a",
        )],
    );
    let cancel = CancellationFlag::new();
    cancel.cancel();
    let mut sink = ProtocolRecordingSink::default();

    let outcome = run_protocol_fixture(source, &mut sink, &cancel, 1).unwrap();

    assert_eq!(outcome, RunOutcome::Cancelled(ExecutionReport::default()));
    assert!(sink.locations.is_empty());
    assert_eq!(pulls.load(Ordering::SeqCst), 0);
}

struct ProtocolFailingSink {
    calls: usize,
}

impl MatchSink for ProtocolFailingSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.calls += 1;
        if self.calls == 2 {
            Err(ExecutionError::Sink("planned sink failure".to_owned()))
        } else {
            Ok(SinkControl::Continue {
                records_emitted: matches.len(),
            })
        }
    }
}

#[test]
fn sink_failure_stops_publication_immediately() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let source = RecordingSource::new(
        pulls,
        vec![ExecutionBatch::tokenized(
            (0..8)
                .map(|ordinal| ExecutionSentence {
                    location: location(ordinal, "recording.lingcorpus", ordinal),
                    value: vec![token("a")],
                })
                .collect(),
        )],
    );
    let mut sink = ProtocolFailingSink { calls: 0 };

    let error = execute(
        source,
        |_| unreachable!("tokenized source bypasses tokenization"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        4,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::Sink(ref message) if message == "planned sink failure"
    ));
    assert_eq!(sink.calls, 2);
}

#[test]
fn zero_logical_batch_size_is_rejected_before_cache_decode() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let source = RecordingSource::new(
        Arc::clone(&pulls),
        vec![located_tokenized_batch(
            location(0, "recording.lingcorpus", 0),
            "a",
        )],
    );

    let error = execute(
        source,
        |_| unreachable!("invalid limit must win before source pull"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        0,
        &CancellationFlag::new(),
        &mut ProtocolRecordingSink::default(),
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::InvalidLimit {
            field: "logical_batch_sentences",
            ..
        }
    ));
    assert_eq!(pulls.load(Ordering::SeqCst), 0);
}

fn text_batch(values: impl IntoIterator<Item = (u64, String)>) -> ExecutionBatch {
    ExecutionBatch::Text(
        values
            .into_iter()
            .map(|(ordinal, value)| ExecutionSentence {
                location: location(ordinal, "", ordinal),
                value: TextUnit::sentence(
                    InputUnitId::new(ordinal),
                    value,
                    NormalizationPolicy::None,
                )
                .unwrap(),
            })
            .collect(),
    )
}

fn tokenized_batch(values: impl IntoIterator<Item = (u64, Vec<TokenInfo>)>) -> ExecutionBatch {
    ExecutionBatch::tokenized(
        values
            .into_iter()
            .map(|(ordinal, value)| ExecutionSentence {
                location: location(ordinal, "", ordinal),
                value,
            })
            .collect(),
    )
}

fn execute_one_batch<K, O>(
    batch: ExecutionBatch,
    pattern: &str,
    limits: &ExecutionLimits,
    logical_batch_sentences: usize,
    cancel: &CancellationFlag,
    sink: &mut K,
    observer: &mut O,
) -> Result<RunOutcome, ExecutionError>
where
    K: MatchSink,
    O: TokenizedBatchObserver,
{
    let (source, _) = StaticSource::new([batch]);
    execute(
        source,
        |_| unreachable!("tokenized sources must bypass tokenization"),
        &matcher(pattern),
        &pool(),
        limits,
        logical_batch_sentences,
        cancel,
        sink,
        &mut NoopProgress,
        observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
}

/// A catalog declaring one `admit`, and two sentences it judges differently.
///
/// `sentence_end` is the reserved anchor, so this needs no `anchor:`
/// declaration — which keeps the fixture to the one thing it is about.
const ADMIT_CATALOG: &str = "pattern cat := [text=猫].\n\
                             policy\n  \
                             admit @cat terminal: {exact: true, anchor: sentence_end}.\n\
                             end\n";

/// A completed run reports what each policy statement did.
///
/// The counters existed and were reachable only by an embedder holding its own
/// `PolicySink`. A researcher at the CLI could not see them, which is the gap
/// `policy_engine`'s module doc named. This is the channel that closes it.
#[test]
fn a_completed_run_reports_per_statement_counters() {
    let mut sink = RecordingSink::default();
    let outcome = execute_one_batch(
        // 猫 ends the first sentence and opens the second, so the `admit`
        // sees two candidates and keeps one.
        ExecutionBatch::tokenized(vec![
            ExecutionSentence {
                location: location(0, "a.md", 0),
                value: vec![token("犬"), token("猫")],
            },
            ExecutionSentence {
                location: location(1, "a.md", 1),
                value: vec![token("猫"), token("犬")],
            },
        ]),
        ADMIT_CATALOG,
        &ExecutionLimits::default(),
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopObserver,
    )
    .unwrap();

    let RunOutcome::Completed(report) = outcome else {
        panic!("the run must complete");
    };
    let per_statement = report.program_stats.per_operation();
    assert_eq!(per_statement.len(), 1, "the catalog declares one statement");
    assert_eq!(per_statement[0].before, 2, "{per_statement:?}");
    assert_eq!(per_statement[0].dropped, 1, "{per_statement:?}");
}

/// A policy-free catalog reports an empty program, not absent counters.
///
/// `ProgramStats::for_program` sizes from the program, so "declared nothing"
/// and "declared something that never fired" stay distinguishable: the first
/// is an empty slice, the second a slice of zeroes. A reader that cannot tell
/// them apart cannot answer the question an inert statement raises.
#[test]
fn a_policy_free_run_reports_an_empty_program() {
    let mut sink = RecordingSink::default();
    let outcome = execute_one_batch(
        ExecutionBatch::tokenized(vec![ExecutionSentence {
            location: location(0, "a.md", 0),
            value: vec![token("猫")],
        }]),
        "plain := [text=猫].",
        &ExecutionLimits::default(),
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopObserver,
    )
    .unwrap();

    let RunOutcome::Completed(report) = outcome else {
        panic!("the run must complete");
    };
    assert!(report.program_stats.per_operation().is_empty());
}

struct StaticSource {
    batches: VecDeque<ExecutionBatch>,
    pulls: Arc<AtomicUsize>,
}

impl StaticSource {
    fn new(batches: impl IntoIterator<Item = ExecutionBatch>) -> (Self, Arc<AtomicUsize>) {
        let pulls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                batches: batches.into_iter().collect(),
                pulls: Arc::clone(&pulls),
            },
            pulls,
        )
    }
}

impl SentenceBatchSource for StaticSource {
    fn wants_lookahead(&self) -> bool {
        false
    }

    fn next_batch(
        &mut self,
        _limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        Ok(self.batches.pop_front())
    }
}

#[derive(Default)]
struct NoopObserver;

impl TokenizedBatchObserver for NoopObserver {
    fn observe(&mut self, _observation: TokenizedBatchObservation) -> Result<(), ExecutionError> {
        Ok(())
    }
}

#[derive(Default)]
struct NoopProgress;

impl<K: ?Sized> ProgressSink<K> for NoopProgress {
    fn update(&mut self, _progress: ExecutionProgress, _sink: &K) {}
}

#[derive(Debug, Default)]
struct RecordingSink {
    records: Vec<(u64, Vec<TokenInfo>, Vec<Match>)>,
    finished: usize,
}

impl MatchSink for RecordingSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.records.push((
            location.corpus_sentence_ordinal,
            (0..sentence.len())
                .map(|index| TokenInfo::from_sequence(sentence, index).expect("valid index"))
                .collect(),
            matches.to_vec(),
        ));
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.finished += 1;
        Ok(())
    }
}

struct TimelineObserver(Arc<Mutex<Vec<String>>>);

impl TokenizedBatchObserver for TimelineObserver {
    fn observe(&mut self, observation: TokenizedBatchObservation) -> Result<(), ExecutionError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("observer:{}", observation.sentence_count));
        Ok(())
    }
}

struct TimelineSink(Arc<Mutex<Vec<String>>>);

impl MatchSink for TimelineSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        let ordinal = location.corpus_sentence_ordinal;
        self.0.lock().unwrap().push(format!("push:{ordinal}"));
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.0.lock().unwrap().push("finish".to_owned());
        Ok(())
    }
}

struct TimelineProgress(Arc<Mutex<Vec<String>>>);

impl ProgressSink<TimelineSink> for TimelineProgress {
    fn update(&mut self, progress: ExecutionProgress, _sink: &TimelineSink) {
        self.0
            .lock()
            .unwrap()
            .push(format!("progress:{}", progress.sentences_matched));
    }
}

#[test]
fn lifecycle_is_observe_push_finish_then_cumulative_progress() {
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let source = TextIteratorSource::new(["a", "b"].into_iter().map(str::to_owned));
    let mut sink = TimelineSink(Arc::clone(&timeline));
    let mut progress = TimelineProgress(Arc::clone(&timeline));
    let mut observer = TimelineObserver(Arc::clone(&timeline));

    let outcome = execute(
        source,
        |text| vec![token(text)],
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits {
            max_stage_sentences: 2,
            ..ExecutionLimits::default()
        },
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut progress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Completed(expected_report(2, 2, 2, 1, 1))
    );
    assert_eq!(
        *timeline.lock().unwrap(),
        [
            "observer:1",
            "push:0",
            "observer:1",
            "push:1",
            "finish",
            "progress:2"
        ]
    );
}

#[test]
fn physical_batches_do_not_change_logical_finalization_cadence() {
    let observer_calls = Arc::new(AtomicUsize::new(0));
    struct CountingObserver(Arc<AtomicUsize>);
    impl TokenizedBatchObserver for CountingObserver {
        fn observe(
            &mut self,
            _observation: TokenizedBatchObservation,
        ) -> Result<(), ExecutionError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let mut sink = RecordingSink::default();
    let mut progress_updates = Vec::new();
    let outcome = execute(
        TextIteratorSource::new(["a", "b"].into_iter().map(str::to_owned)),
        |text| vec![token(text)],
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits {
            max_stage_sentences: 1,
            ..ExecutionLimits::default()
        },
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut |update: ExecutionProgress, _sink: &RecordingSink| progress_updates.push(update),
        &mut CountingObserver(Arc::clone(&observer_calls)),
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(observer_calls.load(Ordering::SeqCst), 2);
    assert_eq!(sink.finished, 1);
    assert_eq!(progress_updates.len(), 1);
    assert_eq!(progress_updates[0].sentences_matched, 2);
}

#[derive(Default)]
struct SpillSink {
    pushes: usize,
    spill_runs: usize,
}

impl SpillSink {
    fn spill_run_count(&self) -> usize {
        self.spill_runs
    }
}

impl MatchSink for SpillSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.pushes += 1;
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.spill_runs += 1;
        Ok(())
    }
}

#[test]
fn logical_boundaries_finalize_at_1000_2000_and_partial_eof() {
    let mut sink = SpillSink::default();
    let mut observer_calls = 0usize;
    let mut progress_updates = Vec::new();
    let outcome = execute(
        TextIteratorSource::new((0..2501).map(|index| index.to_string())),
        |text| vec![token(text)],
        &matcher("plain := [text=never]."),
        &pool(),
        &ExecutionLimits::default(),
        1000,
        &CancellationFlag::new(),
        &mut sink,
        &mut |update: ExecutionProgress, sink: &SpillSink| {
            progress_updates.push((update.sentences_matched, sink.spill_run_count()));
        },
        &mut |observation: TokenizedBatchObservation| {
            observer_calls += 1;
            assert!(observation.sentence_count <= ExecutionLimits::default().max_stage_sentences);
            Ok(())
        },
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Completed(expected_report(2501, 2501, 2501, 0, 0))
    );
    assert!(observer_calls > 3);
    assert_eq!(sink.pushes, 2501);
    assert_eq!(progress_updates, [(1000, 1), (2000, 2), (2501, 3)]);
}

#[test]
fn zero_logical_batch_limit_fails_before_source_pull() {
    let (source, pulls) = StaticSource::new([text_batch([(0, "a".to_owned())])]);
    let error = execute(
        source,
        |text| vec![token(text)],
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        0,
        &CancellationFlag::new(),
        &mut RecordingSink::default(),
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::InvalidLimit {
            field: "logical_batch_sentences",
            ..
        }
    ));
    assert_eq!(pulls.load(Ordering::SeqCst), 0);
}

#[test]
fn empty_input_has_no_finalization_or_progress() {
    let (source, _pulls) = StaticSource::new([]);
    let mut sink = RecordingSink::default();
    let mut progress = Vec::new();
    let outcome = execute(
        source,
        |_| Vec::new(),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut |update: ExecutionProgress, _sink: &RecordingSink| progress.push(update),
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(outcome, RunOutcome::Completed(ExecutionReport::default()));
    assert_eq!(sink.finished, 0);
    assert!(progress.is_empty());
}

#[test]
fn cancellation_before_first_pull_returns_the_empty_lifecycle_report() {
    let (source, pulls) = StaticSource::new([text_batch([(0, "a".to_owned())])]);
    let cancel = CancellationFlag::new();
    cancel.cancel();
    let mut sink = RecordingSink::default();

    let outcome = execute(
        source,
        |text| vec![token(text)],
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        1,
        &cancel,
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(outcome, RunOutcome::Cancelled(ExecutionReport::default()));
    assert_eq!(pulls.load(Ordering::SeqCst), 0);
    assert!(sink.records.is_empty());
}

#[test]
fn cancellation_from_tokenizer_stops_before_observation_and_publication() {
    let cancel = CancellationFlag::new();
    let callback_cancel = cancel.clone();
    let observer_calls = Arc::new(AtomicUsize::new(0));
    struct CountingObserver(Arc<AtomicUsize>);
    impl TokenizedBatchObserver for CountingObserver {
        fn observe(
            &mut self,
            _observation: TokenizedBatchObservation,
        ) -> Result<(), ExecutionError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let mut sink = RecordingSink::default();

    let outcome = execute(
        TextIteratorSource::new(["a"].into_iter().map(str::to_owned)),
        move |text| {
            callback_cancel.cancel();
            vec![token(text)]
        },
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        1,
        &cancel,
        &mut sink,
        &mut NoopProgress,
        &mut CountingObserver(Arc::clone(&observer_calls)),
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(outcome, RunOutcome::Cancelled(ExecutionReport::default()));
    assert_eq!(observer_calls.load(Ordering::SeqCst), 0);
    assert!(sink.records.is_empty());
}

#[test]
fn cancellation_from_observer_stops_before_sink_publication() {
    let cancel = CancellationFlag::new();
    let observer_cancel = cancel.clone();
    let mut sink = RecordingSink::default();
    let outcome = execute(
        TextIteratorSource::new(["a"].into_iter().map(str::to_owned)),
        |text| vec![token(text)],
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        1,
        &cancel,
        &mut sink,
        &mut NoopProgress,
        &mut move |_observation: TokenizedBatchObservation| {
            observer_cancel.cancel();
            Ok(())
        },
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Cancelled(ExecutionReport {
            sentences_admitted: 1,
            ..ExecutionReport::default()
        })
    );
    assert!(sink.records.is_empty());
}

struct CancellingSink {
    cancel: CancellationFlag,
    records: Vec<u64>,
    finished: usize,
}

impl MatchSink for CancellingSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.records.push(location.corpus_sentence_ordinal);
        if self.records.len() == 1 {
            self.cancel.cancel();
        }
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.finished += 1;
        Ok(())
    }
}

#[test]
fn sink_cancellation_is_observed_before_the_next_push() {
    let cancel = CancellationFlag::new();
    let (source, pulls) = StaticSource::new([tokenized_batch([
        (0, vec![token("a")]),
        (1, vec![token("a")]),
        (2, vec![token("a")]),
    ])]);
    let mut sink = CancellingSink {
        cancel: cancel.clone(),
        records: Vec::new(),
        finished: 0,
    };
    let mut progress = Vec::new();

    let outcome = execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        2,
        &cancel,
        &mut sink,
        &mut |update: ExecutionProgress, _sink: &CancellingSink| progress.push(update),
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Cancelled(expected_report(2, 2, 2, 2, 1))
    );
    assert_eq!(sink.records, [0]);
    assert_eq!(sink.finished, 0);
    assert!(progress.is_empty());
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
}

#[test]
fn sink_cancellation_preserves_irreversible_prefix_without_finishing_batch() {
    let cancel = CancellationFlag::new();
    let (source, _pulls) = StaticSource::new([tokenized_batch([
        (0, vec![token("a")]),
        (1, vec![token("a")]),
        (2, vec![token("a")]),
    ])]);
    let mut sink = CancellingSink {
        cancel: cancel.clone(),
        records: Vec::new(),
        finished: 0,
    };

    let outcome = execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        10,
        &cancel,
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Cancelled(expected_report(3, 3, 3, 3, 1))
    );
    assert_eq!(sink.records, [0]);
    assert_eq!(sink.finished, 0);
}

#[test]
fn cancellation_from_progress_returns_the_just_finalized_report_without_next_pull() {
    let cancel = CancellationFlag::new();
    let progress_cancel = cancel.clone();
    let (source, pulls) = StaticSource::new([
        tokenized_batch([(0, vec![token("a")]), (1, vec![token("a")])]),
        tokenized_batch([(2, vec![token("a")])]),
    ]);
    let mut sink = RecordingSink::default();

    let outcome = execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        2,
        &cancel,
        &mut sink,
        &mut move |_update: ExecutionProgress, _sink: &RecordingSink| {
            progress_cancel.cancel();
        },
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Cancelled(expected_report(2, 2, 2, 2, 2))
    );
    assert_eq!(sink.records.len(), 2);
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
}

struct CancelAtEofSource {
    batch: Option<ExecutionBatch>,
    cancel: CancellationFlag,
}

impl SentenceBatchSource for CancelAtEofSource {
    fn wants_lookahead(&self) -> bool {
        false
    }

    fn next_batch(
        &mut self,
        _limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        if let Some(batch) = self.batch.take() {
            Ok(Some(batch))
        } else {
            self.cancel.cancel();
            Ok(None)
        }
    }
}

#[test]
fn cancellation_raised_while_pulling_eof_does_not_finalize_a_partial_logical_batch() {
    let cancel = CancellationFlag::new();
    let source = CancelAtEofSource {
        batch: Some(tokenized_batch([(0, vec![token("a")])])),
        cancel: cancel.clone(),
    };
    let mut sink = RecordingSink::default();
    let mut progress = Vec::new();

    let outcome = execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        2,
        &cancel,
        &mut sink,
        &mut |update: ExecutionProgress, _sink: &RecordingSink| progress.push(update),
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Cancelled(expected_report(1, 1, 1, 1, 1))
    );
    assert_eq!(sink.records.len(), 1);
    assert_eq!(sink.finished, 0);
    assert!(progress.is_empty());
}

struct CancelOnPullSource {
    batch: Option<ExecutionBatch>,
    cancel: CancellationFlag,
}

impl SentenceBatchSource for CancelOnPullSource {
    fn wants_lookahead(&self) -> bool {
        false
    }

    fn next_batch(
        &mut self,
        _limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        self.cancel.cancel();
        Ok(self.batch.take())
    }
}

#[test]
fn cancellation_raised_while_pulling_a_batch_stops_before_tokenization_or_publication() {
    let cancel = CancellationFlag::new();
    let source = CancelOnPullSource {
        batch: Some(text_batch([(0, "a".to_owned())])),
        cancel: cancel.clone(),
    };
    let tokenizations = AtomicUsize::new(0);
    let observer_calls = AtomicUsize::new(0);
    let mut sink = RecordingSink::default();
    let mut progress = Vec::new();

    let outcome = execute(
        source,
        |text| {
            tokenizations.fetch_add(1, Ordering::SeqCst);
            vec![token(text)]
        },
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        1,
        &cancel,
        &mut sink,
        &mut |update: ExecutionProgress, _sink: &RecordingSink| progress.push(update),
        &mut |_observation: TokenizedBatchObservation| {
            observer_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(outcome, RunOutcome::Cancelled(ExecutionReport::default()));
    assert_eq!(tokenizations.load(Ordering::SeqCst), 0);
    assert_eq!(observer_calls.load(Ordering::SeqCst), 0);
    assert!(sink.records.is_empty());
    assert_eq!(sink.finished, 0);
    assert!(progress.is_empty());
}

#[test]
fn cancellation_raised_with_an_invalid_batch_wins_before_validation() {
    let cancel = CancellationFlag::new();
    let source = CancelOnPullSource {
        batch: Some(ExecutionBatch::Text(Vec::new())),
        cancel: cancel.clone(),
    };

    let outcome = execute(
        source,
        |_| unreachable!("post-pull cancellation must precede batch validation"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        1,
        &cancel,
        &mut RecordingSink::default(),
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(outcome, RunOutcome::Cancelled(ExecutionReport::default()));
}

struct ResidencyObserver {
    limit: usize,
    estimates: Vec<usize>,
}

impl TokenizedBatchObserver for ResidencyObserver {
    fn observe(&mut self, observation: TokenizedBatchObservation) -> Result<(), ExecutionError> {
        assert!(observation.charged_bytes <= self.limit);
        self.estimates.push(observation.charged_bytes);
        Ok(())
    }
}

#[test]
fn raw_stage_is_partitioned_by_tokenized_residency_without_reordering() {
    let payload = "x".repeat(256);
    let values = (0..512)
        .map(|index| format!("{index:03}-{payload}"))
        .collect::<Vec<_>>();
    let one_sentence_estimate = estimate_token_info_bytes(&[token(&values[0])]).unwrap();
    let max_tokenized_bytes = one_sentence_estimate * 7;
    let limits = ExecutionLimits {
        max_sentence_bytes: 512,
        max_input_record_bytes: DEFAULT_MAX_INPUT_RECORD_BYTES,
        max_stage_sentences: 512,
        max_stage_input_bytes: 512 * 512,
        max_tokenized_bytes,
    };
    let mut sink = RecordingSink::default();
    let mut observer = ResidencyObserver {
        limit: max_tokenized_bytes,
        estimates: Vec::new(),
    };

    let outcome = execute(
        TextIteratorSource::new(values.clone().into_iter()),
        |text| vec![token(text)],
        &matcher("plain := [text~/^000-/]."),
        &pool(),
        &limits,
        1000,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(observer.estimates.len() > 1);
    assert!(
        observer
            .estimates
            .iter()
            .all(|estimate| *estimate <= max_tokenized_bytes)
    );
    assert_eq!(
        sink.records
            .iter()
            .map(|(ordinal, sentence, _)| (*ordinal, sentence[0].orth().unwrap().to_owned()))
            .collect::<Vec<_>>(),
        values
            .into_iter()
            .enumerate()
            .map(|(ordinal, value)| (ordinal as u64, value))
            .collect::<Vec<_>>()
    );
}

#[test]
fn text_tokenization_observes_matches_and_drops_each_sentence_before_the_next() {
    struct TimelineResidencySink(Arc<Mutex<Vec<String>>>);
    impl MatchSink for TimelineResidencySink {
        fn push(
            &mut self,
            _location: &SentenceLocation,
            sentence: &dyn TokenSequence,
            matches: &[Match],
        ) -> Result<SinkControl, ExecutionError> {
            self.0.lock().unwrap().push(format!(
                "push:{}",
                sentence.get(0, TokenAttributeIndex::Orth).expect("orth")
            ));
            Ok(SinkControl::Continue {
                records_emitted: matches.len(),
            })
        }
    }

    let values = (0..4).map(|index| format!("v{index}")).collect::<Vec<_>>();
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let tokenizer_timeline = Arc::clone(&timeline);
    let observer_timeline = Arc::clone(&timeline);

    execute(
        TextIteratorSource::new(values.into_iter()),
        move |text| {
            tokenizer_timeline
                .lock()
                .unwrap()
                .push(format!("tokenize:{text}"));
            vec![token(text)]
        },
        &matcher("plain := []."),
        &pool(),
        &ExecutionLimits::default(),
        100,
        &CancellationFlag::new(),
        &mut TimelineResidencySink(Arc::clone(&timeline)),
        &mut NoopProgress,
        &mut move |observation: TokenizedBatchObservation| {
            assert_eq!(observation.sentence_count, 1);
            observer_timeline
                .lock()
                .unwrap()
                .push(format!("observe:{}", observation.sentence_count));
            Ok(())
        },
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        *timeline.lock().unwrap(),
        [
            "tokenize:v0",
            "observe:1",
            "push:v0",
            "tokenize:v1",
            "observe:1",
            "push:v1",
            "tokenize:v2",
            "observe:1",
            "push:v2",
            "tokenize:v3",
            "observe:1",
            "push:v3",
        ]
    );
}

#[test]
fn oversized_tokenized_singleton_fails_before_any_batch_publication() {
    let small = vec![token("small")];
    let oversized = vec![token(&"x".repeat(2048))];
    let actual = estimate_token_info_bytes(&oversized).unwrap();
    let limits = ExecutionLimits {
        max_tokenized_bytes: actual - 1,
        ..ExecutionLimits::default()
    };
    let (source, _pulls) = StaticSource::new([tokenized_batch([(0, small), (1, oversized)])]);
    let mut sink = RecordingSink::default();
    let mut observer = ResidencyObserver {
        limit: limits.max_tokenized_bytes,
        estimates: Vec::new(),
    };

    let error = execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &matcher("plain := [text=small]."),
        &pool(),
        &limits,
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::TokenizedSentenceTooLarge {
            ordinal: 1,
            actual: observed,
            limit,
        } if observed == actual && limit == actual - 1
    ));
    assert!(sink.records.is_empty());
    assert!(observer.estimates.is_empty());
}

#[test]
fn empty_tokenized_sentences_still_count_and_preserve_ordinals() {
    let (source, _pulls) = StaticSource::new([tokenized_batch([
        (10, Vec::new()),
        (11, vec![token("a")]),
        (12, Vec::new()),
    ])]);
    let mut sink = RecordingSink::default();
    let mut progress = Vec::new();

    let outcome = execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits::default(),
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut |update: ExecutionProgress, _sink: &RecordingSink| progress.push(update),
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Completed(expected_report(3, 3, 1, 1, 1))
    );
    assert_eq!(
        sink.records
            .iter()
            .map(|(ordinal, _, _)| *ordinal)
            .collect::<Vec<_>>(),
        [10, 11, 12]
    );
    assert_eq!(
        progress
            .iter()
            .map(|update| update.sentences_matched)
            .collect::<Vec<_>>(),
        [2, 3]
    );
}

#[test]
fn executor_revalidates_custom_source_sentence_count_before_tokenization() {
    let (source, _pulls) = StaticSource::new([text_batch([
        (0, "a".to_owned()),
        (1, "b".to_owned()),
        (2, "c".to_owned()),
    ])]);
    let tokenizations = AtomicUsize::new(0);
    let mut sink = RecordingSink::default();
    let error = execute(
        source,
        |text| {
            tokenizations.fetch_add(1, Ordering::SeqCst);
            vec![token(text)]
        },
        &matcher("plain := [text=a]."),
        &pool(),
        &ExecutionLimits {
            max_stage_sentences: 2,
            ..ExecutionLimits::default()
        },
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(error, ExecutionError::InvalidLimit { .. }));
    assert_eq!(tokenizations.load(Ordering::SeqCst), 0);
    assert!(sink.records.is_empty());
}

#[test]
fn executor_revalidates_custom_source_raw_bytes_before_tokenization() {
    let (source, _pulls) = StaticSource::new([text_batch([(7, "x".repeat(11))])]);
    let tokenizations = AtomicUsize::new(0);
    let mut sink = RecordingSink::default();
    let error = execute(
        source,
        |text| {
            tokenizations.fetch_add(1, Ordering::SeqCst);
            vec![token(text)]
        },
        &matcher("plain := [text=x]."),
        &pool(),
        &ExecutionLimits {
            max_sentence_bytes: 10,
            max_stage_input_bytes: 10,
            ..ExecutionLimits::default()
        },
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::SentenceTooLarge {
            ordinal: 7,
            actual: 11,
            limit: 10,
            ..
        }
    ));
    assert_eq!(tokenizations.load(Ordering::SeqCst), 0);
    assert!(sink.records.is_empty());
}

#[test]
fn executor_revalidates_custom_source_aggregate_raw_bytes_before_tokenization() {
    let (source, _pulls) = StaticSource::new([text_batch([
        (0, "123456".to_owned()),
        (1, "abcdef".to_owned()),
    ])]);
    let tokenizations = AtomicUsize::new(0);
    let error = execute(
        source,
        |text| {
            tokenizations.fetch_add(1, Ordering::SeqCst);
            vec![token(text)]
        },
        &matcher("plain := [text=x]."),
        &pool(),
        &ExecutionLimits {
            max_sentence_bytes: 10,
            max_stage_input_bytes: 10,
            ..ExecutionLimits::default()
        },
        2,
        &CancellationFlag::new(),
        &mut RecordingSink::default(),
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(error, ExecutionError::InvalidLimit { .. }));
    assert_eq!(tokenizations.load(Ordering::SeqCst), 0);
}

#[test]
fn executor_revalidates_custom_source_token_residency_before_observation() {
    let oversized = vec![token(&"x".repeat(2048))];
    let actual = estimate_token_info_bytes(&oversized).unwrap();
    let (source, _pulls) = StaticSource::new([tokenized_batch([(4, oversized)])]);
    let observer_calls = AtomicUsize::new(0);
    let mut sink = RecordingSink::default();

    let error = execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &matcher("plain := [text=x]."),
        &pool(),
        &ExecutionLimits {
            max_tokenized_bytes: actual - 1,
            ..ExecutionLimits::default()
        },
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut |_observation: TokenizedBatchObservation| {
            observer_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ExecutionError::TokenizedSentenceTooLarge { ordinal: 4, .. }
    ));
    assert_eq!(observer_calls.load(Ordering::SeqCst), 0);
    assert!(sink.records.is_empty());
}

#[test]
fn executor_preserves_complete_canonical_matches_including_captures() {
    let sentence = vec![token("a"), token("b"), token("a")];
    let fsm = matcher(concat!(
        "pattern captured := (?<unit> [text=a]).\n",
        "pattern equal := (?<unit> [text=a]) [text=b] same_as($unit).\n",
    ));
    let expected = fsm.find_matches(&sentence);
    let (source, _pulls) = StaticSource::new([tokenized_batch([(42, sentence.clone())])]);
    let mut sink = RecordingSink::default();

    execute(
        source,
        |_| unreachable!("tokenized source must bypass tokenizer"),
        &fsm,
        &pool(),
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(sink.records, [(42, sentence, expected)]);
}

// ---------------------------------------------------------------------------
// Scope conditioning: an excluded sentence is never presented to matching.
// ---------------------------------------------------------------------------

fn admitting(documents: impl IntoIterator<Item = u64>) -> SentenceAdmission {
    let mut set = roaring::RoaringTreemap::new();
    for document in documents {
        set.insert(document);
    }
    SentenceAdmission::Documents(set)
}

fn execute_one_batch_scoped<K, O>(
    batch: ExecutionBatch,
    pattern: &str,
    scope: &SentenceAdmission,
    logical_batch_sentences: usize,
    sink: &mut K,
    observer: &mut O,
) -> Result<RunOutcome, ExecutionError>
where
    K: MatchSink,
    O: TokenizedBatchObserver,
{
    let (source, _) = StaticSource::new([batch]);
    execute(
        source,
        |_| unreachable!("tokenized sources must bypass tokenization"),
        &matcher(pattern),
        &pool(),
        &ExecutionLimits::default(),
        logical_batch_sentences,
        &CancellationFlag::new(),
        sink,
        &mut NoopProgress,
        observer,
        lingrep_core::execution::PolicySelection::Apply,
        scope,
    )
}

#[test]
fn an_excluded_sentence_is_never_matched() {
    // `location(ordinal, …)` assigns document_id = ordinal + 10, so admitting
    // {10, 12} admits ordinals 0 and 2 only.
    let mut sink = RecordingSink::default();

    let outcome = execute_one_batch_scoped(
        scoped_batch(4),
        "plain := [text=猫].",
        &admitting([10, 12]),
        4,
        &mut sink,
        &mut NoopObserver,
    )
    .unwrap();

    let RunOutcome::Completed(report) = outcome else {
        panic!("the run must complete");
    };
    assert_eq!(
        report.sentences_admitted, 2,
        "only admitted sentences are counted"
    );
    assert_eq!(report.sentences_matched, 2);
    assert_eq!(
        sink.records
            .iter()
            .map(|(ordinal, _, _)| *ordinal)
            .collect::<Vec<_>>(),
        vec![0, 2],
        "ordinals are never renumbered by a skip"
    );
}

#[test]
fn a_wholly_excluded_batch_completes_the_run_without_an_empty_partition() {
    // `validate_batch_count` refuses an empty batch from a source; a partition
    // that empties after skipping must simply not run, not become that error.
    let mut sink = RecordingSink::default();

    let outcome = execute_one_batch_scoped(
        scoped_batch(4),
        "plain := [text=猫].",
        &admitting([999]),
        4,
        &mut sink,
        &mut NoopObserver,
    )
    .unwrap();

    let RunOutcome::Completed(report) = outcome else {
        panic!("a wholly excluded batch still completes");
    };
    assert_eq!(report.sentences_admitted, 0);
    assert!(sink.records.is_empty());
}

#[test]
fn admitting_everything_is_indistinguishable_from_an_unscoped_run() {
    let mut sink = RecordingSink::default();

    let outcome = execute_one_batch_scoped(
        scoped_batch(4),
        "plain := [text=猫].",
        &SentenceAdmission::All,
        4,
        &mut sink,
        &mut NoopObserver,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Completed(expected_report(4, 4, 4, 4, 4))
    );
}

fn scoped_batch(count: u64) -> ExecutionBatch {
    tokenized_batch((0..count).map(|ordinal| (ordinal, vec![token("猫")])))
}

/// Records the ordinal of every published sentence.
///
/// lingrep-web's section sweep cannot move its cursor backwards, so it depends on
/// publication being ordered. `MatchSink`'s "one ordered sentence" is prose;
/// this pins the property across partition boundaries, where batching is the
/// only thing that could plausibly reorder it.
struct OrdinalRecorder(Vec<u64>);

impl MatchSink for OrdinalRecorder {
    fn push(
        &mut self,
        location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.0.push(location.corpus_sentence_ordinal);
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[test]
fn publication_ordinals_never_decrease_across_partitions() {
    const SENTENCES: u64 = 512;
    let batches = (0..SENTENCES)
        .map(|ordinal| located_tokenized_batch(location(ordinal, "fixture", ordinal), "猫"))
        .collect::<Vec<_>>();
    let source = RecordingSource::new(Arc::new(AtomicUsize::new(0)), batches);
    let mut sink = OrdinalRecorder(Vec::new());
    // Small enough that the runner closes a partition long before the corpus
    // ends: reordering, if it existed, would appear at those boundaries. One
    // one-token sentence already costs `estimate_token_info_bytes`, so this
    // bound admits a handful of sentences per partition, not all 512.
    let per_sentence = estimate_token_info_bytes(&[token("猫")]).expect("one token is measurable");
    let limits = ExecutionLimits {
        max_tokenized_bytes: per_sentence * 8,
        ..ExecutionLimits::default()
    };
    // One observation per matching partition, so this counts the boundaries the
    // ordering claim is about. A test that pinned ordering across partitions
    // while only ever producing one partition would prove nothing.
    let partitions = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&partitions);
    let mut observer = move |_: TokenizedBatchObservation| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let outcome = execute(
        source,
        |text| vec![token(text)],
        &matcher("plain := [text=猫]."),
        &pool(),
        &limits,
        8,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .expect("the scan completes");

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert!(
        partitions.load(Ordering::SeqCst) > 1,
        "the byte bound must actually split the corpus, or the boundaries this \
         pins never occur: {} partitions",
        partitions.load(Ordering::SeqCst)
    );
    assert_eq!(
        sink.0.len() as u64,
        SENTENCES,
        "every sentence is published exactly once"
    );
    assert!(
        sink.0.windows(2).all(|pair| pair[0] <= pair[1]),
        "publication reordered: {:?}",
        &sink.0[..sink.0.len().min(32)]
    );
}

#[test]
fn source_metadata_follows_every_analyzer_created_sentence() {
    use lingrep_core::analysis::{SegmentationMode, TextUnitKind};
    use lingrep_core::execution::NativeAnalysisSource;
    let plan = VibratoPlan::with_mode(
        lingrep_core::testing::test_tokenizer().clone(),
        SegmentationMode::Analyzer,
    );
    let bound = matcher("p := [].").bind(plan.schema()).unwrap();
    let metadata = Arc::new(
        serde_json::json!({"video_id":"clip", "start_ms":0})
            .as_object()
            .unwrap()
            .clone(),
    );
    let source = || {
        let mut location = location(0, "captions.jsonl", 0);
        location.source_metadata = Some(metadata.clone());
        RecordingSource::new(
            Arc::new(AtomicUsize::new(0)),
            vec![ExecutionBatch::Text(vec![ExecutionSentence {
                location,
                value: TextUnit::admit(
                    InputUnitId::new(0),
                    TextUnitKind::Segment,
                    "猫。犬。",
                    NormalizationPolicy::None,
                )
                .unwrap(),
            }])],
        )
    };
    let mut native = NativeAnalysisSource::new(
        source(),
        plan.create_session(),
        bound.projection().clone(),
        SegmentationMode::Analyzer,
    );
    let ExecutionBatch::BoundTokenized { sentences, .. } = native
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap()
    else {
        panic!("expected analyzed sentences");
    };
    assert_eq!(sentences.len(), 2);
    for sentence in sentences {
        assert_eq!(sentence.location.source_metadata.as_ref(), Some(&metadata));
    }
    let mut sink = ProtocolRecordingSink::default();
    execute_analyzed(
        source(),
        AnalysisBinding::Session(plan.create_session()),
        &bound,
        &pool(),
        &ExecutionLimits::default(),
        2,
        &CancellationFlag::new(),
        &mut sink,
        &mut NoopProgress,
        &mut NoopObserver,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();
    assert_eq!(sink.locations.len(), 2);
    for location in sink.locations {
        assert_eq!(location.source_metadata.as_ref(), Some(&metadata));
    }
}
