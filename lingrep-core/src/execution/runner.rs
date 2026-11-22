//! Canonical bounded matching execution and publication lifecycle.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::analysis::{AnalyzerSession, TextUnit};
use std::collections::BTreeMap;
use std::ops::Range;

use crate::corpus::DocumentId;
use crate::fsm::{
    BoundCatalog, BoundSentenceMatcher, LookaroundReach, LookaroundScope, Match, PublishedRecord,
};
use crate::token_match::{
    BoundProjection, BoundTokenSentence, TokenInfo, TokenSchema, TokenSequence,
};

use super::{
    BoundSentenceInput, EnhancedGraphInput, ExecutionBatch, ExecutionError, ExecutionLimits,
    ExecutionSentence, PolicySink, Publication, SentenceAdmission, SentenceBatchSource,
    SentenceLocation, WindowLinks, estimate_token_info_bytes, window_at,
};

/// A clonable cooperative-cancellation signal shared with an execution run.
#[derive(Debug, Clone, Default)]
pub struct CancellationFlag(pub(super) Arc<AtomicBool>);

impl CancellationFlag {
    /// Creates an unset cancellation flag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation at the executor's next safe checkpoint.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Cumulative lifecycle counters published after a complete logical batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionProgress {
    /// Number of sentences admitted to a matching partition.
    pub sentences_admitted: u64,
    /// Number of sentences with at least one canonical match, of any pattern.
    pub sentences_matched: u64,
    /// Number of tokens processed by the matcher.
    pub tokens_scanned: u64,
    /// Number of canonical matches produced by the matcher.
    pub matches_found: u64,
    /// Number of match records that the sink reported publishing after filtering.
    pub records_emitted: u64,
}

/// Cumulative lifecycle counters and policy reports for one execution run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionReport {
    /// Number of sentences admitted to a matching partition.
    pub sentences_admitted: u64,
    /// Number of sentences with at least one canonical match, of any pattern.
    pub sentences_matched: u64,
    /// Number of tokens processed by the matcher.
    pub tokens_scanned: u64,
    /// Number of canonical matches produced by the matcher.
    pub matches_found: u64,
    /// Number of match records that the sink reported publishing after filtering.
    pub records_emitted: u64,
    /// Counts of records suppressed or dropped by publication policy.
    pub policy_stats: crate::fsm::PolicyStats,
    /// Per-statement policy counts in authored order, including zero-count statements.
    /// Empty when the catalog declares no policy block.
    pub program_stats: crate::fsm::ProgramStats,
    /// At most the requested number of record explanations, or `None` when tracing is disabled.
    pub trace: Option<crate::fsm::PolicyTrace>,
    /// Indexed like [`CompiledCatalog::label_declarations`](crate::fsm::CompiledCatalog::label_declarations),
    /// or empty when no sentence was observed.
    pub out_of_set_labels: Vec<crate::fsm::OutOfSetValues>,
    /// Whether any admitted sentence carried a span of each kind, indexed like
    /// [`CompiledCatalog::span_constraint_kinds`](crate::fsm::CompiledCatalog::span_constraint_kinds),
    /// or empty when no sentence was observed.
    pub span_kinds_observed: Vec<bool>,
}

impl ExecutionReport {
    fn admit(&mut self, sentence_count: usize) -> Result<(), ExecutionError> {
        self.sentences_admitted = self
            .sentences_admitted
            .checked_add(
                u64::try_from(sentence_count).map_err(|_| ExecutionError::ResourceAccounting)?,
            )
            .ok_or(ExecutionError::ResourceAccounting)?;
        Ok(())
    }

    fn observe_match(
        &mut self,
        sentence: &dyn TokenSequence,
        matches: usize,
    ) -> Result<(), ExecutionError> {
        self.sentences_matched = self
            .sentences_matched
            .checked_add(u64::from(matches != 0))
            .ok_or(ExecutionError::ResourceAccounting)?;
        self.tokens_scanned = self
            .tokens_scanned
            .checked_add(
                u64::try_from(sentence.len()).map_err(|_| ExecutionError::ResourceAccounting)?,
            )
            .ok_or(ExecutionError::ResourceAccounting)?;
        self.matches_found = self
            .matches_found
            .checked_add(u64::try_from(matches).map_err(|_| ExecutionError::ResourceAccounting)?)
            .ok_or(ExecutionError::ResourceAccounting)?;
        Ok(())
    }

    fn observe_emission(&mut self, records_emitted: usize) -> Result<(), ExecutionError> {
        self.records_emitted = self
            .records_emitted
            .checked_add(
                u64::try_from(records_emitted).map_err(|_| ExecutionError::ResourceAccounting)?,
            )
            .ok_or(ExecutionError::ResourceAccounting)?;
        Ok(())
    }

    /// The `Copy` scalar projection, for a progress observer.
    ///
    /// Borrows rather than consuming: the accumulator this reads is the run's
    /// live one and is read once per finalized batch. It copies five integers
    /// and leaves the per-statement counters where they are.
    fn progress(&self) -> ExecutionProgress {
        ExecutionProgress {
            sentences_admitted: self.sentences_admitted,
            sentences_matched: self.sentences_matched,
            tokens_scanned: self.tokens_scanned,
            matches_found: self.matches_found,
            records_emitted: self.records_emitted,
        }
    }
}

/// Terminal state of a successful or cooperatively cancelled run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The source reached clean EOF and every logical batch was finalized.
    Completed(ExecutionReport),
    /// Cancellation stopped the run at a safe boundary.
    ///
    /// The report includes every admitted, matched, and irreversibly emitted
    /// record observed before the cancellation checkpoint.
    Cancelled(ExecutionReport),
}

/// Intentional continuation result from one successful sink push.
///
/// Both variants carry the count, because a sink that publishes less than it
/// was handed is the ordinary case, not the terminating one: filtering,
/// deduplicating, and selecting sinks all continue. `Continue` once carried no
/// count and the runner substituted the supplied slice length, which credited
/// every such sink with records it discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkControl {
    /// Continue with the next sentence, having published exactly this many
    /// records from the supplied match slice.
    ///
    /// This is a *count*, not a prefix: a filtering sink publishes a subset in
    /// canonical order, not necessarily a leading run. The runner uses it only
    /// for [`ExecutionReport::records_emitted`], so the distinction from
    /// [`Self::StopAfter`] is real but costs nothing here.
    ///
    /// **Count the supplied records that influenced published output.** A sink
    /// that folds every match into an aggregate emits all of them even though
    /// it writes one row — aggregation is not discard. A sink that skips a
    /// match entirely, because it belongs to another pattern or fails a
    /// predicate, did not emit it. Under that rule `matches_found -
    /// records_emitted` is what publication threw away, which is the number
    /// worth reporting.
    Continue { records_emitted: usize },
    /// Complete without another source pull after publishing exactly this many
    /// leading records from the supplied match slice.
    ///
    /// Genuinely a prefix: a limit stops mid-slice, so the records after the
    /// cut were never offered anywhere.
    StopAfter { records_emitted: usize },
}

impl SinkControl {
    /// Resolve this control against the supplied canonical match slice.
    ///
    /// Returns the honest emission count and whether the run stops. Neither
    /// variant may claim more records than it was handed; a sink that does has
    /// miscounted, and accepting the claim would corrupt run accounting
    /// silently.
    pub fn emitted_records(self, supplied: usize) -> Result<(usize, bool), ExecutionError> {
        let (records_emitted, stop) = match self {
            Self::Continue { records_emitted } => (records_emitted, false),
            Self::StopAfter { records_emitted } => (records_emitted, true),
        };
        if records_emitted <= supplied {
            Ok((records_emitted, stop))
        } else {
            Err(ExecutionError::Sink(format!(
                "sink reported {records_emitted} emitted records from a slice of {supplied}"
            )))
        }
    }
}

/// Concurrent tasks a partition-publication stage fans out to on the shared
/// pool, whatever the pool's width.
///
/// Publication parallelism saturates well before matching does: past eight
/// concurrent lowering tasks the extra workers contend on memory and the
/// allocator instead of saving wall, which is what pushed the fused-lowering
/// candidate past its parallel-efficiency floor at the sixteen-worker
/// default. Each parallel stage at the seam therefore splits its partition
/// into at most this many tasks — bounding concurrency without a second
/// pool — leaving the remaining workers to matching. The width matches the
/// counting-shard clamp, and the choice is measured, not derived.
pub const PARTITION_PUBLICATION_WIDTH: usize = 8;

/// Receives complete canonical matches for one ordered sentence.
pub trait MatchSink {
    /// Whether this sink has a partition implementation worth preparing in
    /// parallel at the publication seam.
    ///
    /// False keeps ordinary record publishers on the established serial path;
    /// a sink opts in only when its [`Self::push_records_partition`] override
    /// can consume the partition-shaped result directly.
    fn wants_parallel_records_partition(&self) -> bool {
        false
    }

    /// Whether this sink derives its result from the sentences alone when the
    /// catalog is [`crate::fsm::CompiledCatalog::token_windows`].
    ///
    /// When true for such a catalog, the run skips matching and calls
    /// [`Self::push_windows`] in place of [`Self::push_partition`].
    fn counts_token_windows(&self) -> bool {
        false
    }

    /// Publishes one partition of a token-window catalog.
    ///
    /// Each control's `records_emitted` is the sentence's window count over
    /// every definition, the number of records matching would have published.
    fn push_windows<V: TokenSequence + Sync>(
        &mut self,
        _pool: &rayon::ThreadPool,
        _locations: &[SentenceLocation],
        _sentences: &[V],
        _cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        Err(ExecutionError::Sink(
            "sink does not count token windows".into(),
        ))
    }

    /// Publishes one sentence and all of its canonical matches.
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError>;

    /// Publishes one sentence's records together with its derived unit, key,
    /// and tags.
    ///
    /// Defaults to projecting each record onto its published span and name and
    /// calling [`MatchSink::push`]. A sink with no use for derived fields
    /// needs no change and cannot accidentally publish them; overriding this is
    /// how a sink opts in.
    ///
    /// Only [`crate::execution::PolicySink`] in its applying mode calls this.
    /// The raw and passthrough modes have no records to offer — nothing derived
    /// any — so they call `push`, and a policy-free catalog never allocates a
    /// record it would not read.
    fn push_records(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        let matches: Vec<Match> = records
            .into_iter()
            .map(PublishedRecord::into_match)
            .collect();
        self.push(location, sentence, &matches)
    }

    /// Publishes one matched partition, preserving sentence order by default.
    ///
    /// The caller-owned pool is part of this seam so a decorator may prepare
    /// independent sentences in parallel without constructing another pool.
    /// The default implementation ignores the pool and keeps the
    /// sentence-at-a-time behavior of [`Self::push`]. Cancellation is checked
    /// at the same boundaries as [`Self::push`], and a stopping sink is never
    /// offered a later sentence.
    fn push_partition<V: TokenSequence + Sync>(
        &mut self,
        _pool: &rayon::ThreadPool,
        locations: &[SentenceLocation],
        sentences: &[V],
        found: &[Vec<Match>],
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        let mut controls = Vec::with_capacity(found.len());
        for ((location, sentence), matches) in locations.iter().zip(sentences).zip(found) {
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
            let control = self.push(location, sentence, matches)?;
            controls.push(control);
            if matches!(control, SinkControl::StopAfter { .. }) {
                return Ok((controls, false));
            }
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
        }
        Ok((controls, false))
    }

    /// Publishes derived records for one partition in sentence order.
    ///
    /// This is the partition counterpart of [`Self::push_records`]. An
    /// applying publication decorator can shape records in parallel, while an
    /// inner sink that has no partition specialization keeps exactly its
    /// existing ordered calls and stop behavior.
    fn push_records_partition<V: TokenSequence + Sync>(
        &mut self,
        _pool: &rayon::ThreadPool,
        locations: &[SentenceLocation],
        sentences: &[V],
        records: Vec<Vec<PublishedRecord>>,
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        let mut controls = Vec::with_capacity(records.len());
        for ((location, sentence), records) in locations.iter().zip(sentences).zip(records) {
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
            let control = self.push_records(location, sentence, records)?;
            controls.push(control);
            if matches!(control, SinkControl::StopAfter { .. }) {
                return Ok((controls, false));
            }
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
        }
        Ok((controls, false))
    }

    /// Finalizes one complete logical publication batch.
    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        Ok(())
    }
}

impl<T: MatchSink + ?Sized> MatchSink for &mut T {
    fn wants_parallel_records_partition(&self) -> bool {
        (**self).wants_parallel_records_partition()
    }

    fn counts_token_windows(&self) -> bool {
        (**self).counts_token_windows()
    }

    fn push_windows<V: TokenSequence + Sync>(
        &mut self,
        pool: &rayon::ThreadPool,
        locations: &[SentenceLocation],
        sentences: &[V],
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        (**self).push_windows(pool, locations, sentences, cancel)
    }

    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        (**self).push(location, sentence, matches)
    }

    /// Forwards rather than inheriting the default, which would project the
    /// records away before the borrowed sink ever saw them.
    fn push_records(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        (**self).push_records(location, sentence, records)
    }

    fn push_partition<V: TokenSequence + Sync>(
        &mut self,
        pool: &rayon::ThreadPool,
        locations: &[SentenceLocation],
        sentences: &[V],
        found: &[Vec<Match>],
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        (**self).push_partition(pool, locations, sentences, found, cancel)
    }

    fn push_records_partition<V: TokenSequence + Sync>(
        &mut self,
        pool: &rayon::ThreadPool,
        locations: &[SentenceLocation],
        sentences: &[V],
        records: Vec<Vec<PublishedRecord>>,
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        (**self).push_records_partition(pool, locations, sentences, records, cancel)
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        (**self).finish_batch()
    }
}

/// Receives cumulative progress after a logical batch has been finalized.
pub trait ProgressSink<K: ?Sized> {
    /// Observes counters and immutable finalized sink state.
    fn update(&mut self, progress: ExecutionProgress, sink: &K);
}

impl<K: ?Sized, F> ProgressSink<K> for F
where
    F: FnMut(ExecutionProgress, &K),
{
    fn update(&mut self, progress: ExecutionProgress, sink: &K) {
        self(progress, sink);
    }
}

/// Authoritative residency accounting for one tokenized execution batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenizedBatchObservation {
    /// Number of sentences in the batch.
    pub sentence_count: usize,
    /// The exact tokenized-residency charge already computed by the executor.
    pub charged_bytes: usize,
}

/// Observes one final residency-compliant tokenized execution batch.
pub trait TokenizedBatchObserver {
    /// Observes batch accounting after tokenization and before matching.
    fn observe(&mut self, observation: TokenizedBatchObservation) -> Result<(), ExecutionError>;
}

impl<F> TokenizedBatchObserver for F
where
    F: FnMut(TokenizedBatchObservation) -> Result<(), ExecutionError>,
{
    fn observe(&mut self, observation: TokenizedBatchObservation) -> Result<(), ExecutionError> {
        self(observation)
    }
}

/// Selects the schema for execution: text uses the analyzer session's schema, while prepared batches must match the explicitly supplied schema.
pub enum AnalysisBinding<A> {
    /// Text batches, analyzed by this session; its schema governs.
    Session(A),
    /// Batches whose tokens are already in this schema, as a prepared
    /// artifact's are. No analyzer is constructed or dispatched.
    Prepared(TokenSchema),
}

impl<A: AnalyzerSession> AnalysisBinding<A> {
    fn schema(&self) -> TokenSchema {
        match self {
            Self::Session(session) => session.schema().clone(),
            Self::Prepared(schema) => schema.clone(),
        }
    }

    fn session_mut(&mut self) -> Option<&mut A> {
        match self {
            Self::Session(session) => Some(session),
            Self::Prepared(_) => None,
        }
    }
}

/// Executes a bound catalog through one lane-owned native analyzer session.
///
/// A prepared source passes [`AnalysisBinding::Prepared`] with the schema its
/// stored tokens are in; its bound batches never construct or dispatch an
/// analyzer. A text batch presented under that binding is rejected.
#[allow(clippy::too_many_arguments)]
pub fn execute_analyzed<S, A, K, P, O>(
    source: S,
    mut binding: AnalysisBinding<A>,
    fsm: &BoundCatalog,
    pool: &rayon::ThreadPool,
    limits: &ExecutionLimits,
    logical_batch_sentences: usize,
    cancel: &CancellationFlag,
    sink: &mut K,
    progress: &mut P,
    observer: &mut O,
    policy: impl Into<Publication>,
    scope: &SentenceAdmission,
) -> Result<RunOutcome, ExecutionError>
where
    S: SentenceBatchSource + Send,
    A: AnalyzerSession,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
{
    let schema = binding.schema();
    if fsm.projection().schema_digest() != schema.digest() {
        return Err(ExecutionError::Source {
            ordinal: 0,
            message: "bound lingrep patterns schema does not match the analyzer schema".to_owned(),
        });
    }
    let publication: Publication = policy.into();
    let mut sealed = PolicySink::with_selection(
        sink,
        fsm.catalog_policy().clone(),
        fsm.anchor_evaluator(),
        publication.selection(),
    );
    if let Some(capacity) = publication.trace_capacity() {
        sealed = sealed.with_trace(capacity);
    }
    let window_widths = sealed
        .counts_token_windows()
        .then(|| fsm.compiled.token_windows())
        .flatten()
        .map(|windows| {
            windows
                .iter()
                .map(|window| window.width)
                .collect::<Vec<_>>()
        });
    let mut adapted = SealedProgress { inner: progress };
    let mut analyzer = SessionAnalyzer {
        session: binding.session_mut(),
    };
    let outcome = execute_bound_with_matcher(
        source,
        &mut analyzer,
        BoundExecutionPlan {
            schema: &schema,
            projection: fsm.projection(),
            limits,
            scope,
            reach: fsm.compiled.lookaround_reach(),
        },
        PartitionRunner {
            pool,
            cancel,
            logical_batch_sentences,
            sink: &mut sealed,
            progress: &mut adapted,
            observer,
            window_widths: window_widths.as_deref(),
            match_batch: |sentences: &[BoundSentenceInput],
                          links: &[bool],
                          targets: Range<usize>| {
                BoundSentenceMatcher::new(fsm)
                    .with_cancellation(cancel)
                    .match_in_windows(pool, sentences, links, targets)
                    .map_err(ExecutionError::Matching)
            },
            state: PublicationState::new(
                fsm.compiled.label_declarations(),
                fsm.compiled
                    .span_constraint_kinds()
                    .keys()
                    .cloned()
                    .collect(),
            ),
        },
    )?;
    if matches!(outcome, RunOutcome::Completed(_))
        && let Some(session) = analyzer.session
    {
        session.finish().map_err(|error| ExecutionError::Analysis {
            ordinal: None,
            source_name: None,
            source: Box::new(error),
        })?;
    }
    let stats = sealed.stats();
    let (program_stats, trace) = sealed.into_reporting();
    Ok(match outcome {
        RunOutcome::Completed(mut report) => {
            report.policy_stats = stats;
            report.program_stats = program_stats;
            report.trace = trace;
            RunOutcome::Completed(report)
        }
        RunOutcome::Cancelled(mut report) => {
            report.policy_stats = stats;
            report.program_stats = program_stats;
            report.trace = trace;
            RunOutcome::Cancelled(report)
        }
    })
}

struct SealedProgress<'p, P: ?Sized> {
    inner: &'p mut P,
}

impl<K: MatchSink, P: ProgressSink<K> + ?Sized> ProgressSink<PolicySink<'_, &mut K>>
    for SealedProgress<'_, P>
{
    fn update(&mut self, progress: ExecutionProgress, sink: &PolicySink<'_, &mut K>) {
        self.inner.update(progress, &**sink.inner());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchControl {
    Continue,
    Cancelled,
    Stop,
}

pub(super) struct AnalyzedTextInput {
    pub location: SentenceLocation,
    pub analyzer_segmented: bool,
    pub analyzed: crate::analysis::AnalyzedInput,
}

pub(super) fn analyze_text_batch<A: AnalyzerSession>(
    session: &mut A,
    sentences: Vec<ExecutionSentence<TextUnit>>,
    projection: &BoundProjection,
    error_ordinal: u64,
) -> Result<Vec<AnalyzedTextInput>, ExecutionError> {
    let mut locations = Vec::with_capacity(sentences.len());
    let mut inputs = Vec::with_capacity(sentences.len());
    let mut identities = Vec::with_capacity(sentences.len());
    for ExecutionSentence { location, value } in sentences {
        identities.push((
            value.id(),
            value.kind() == crate::analysis::TextUnitKind::Segment,
        ));
        locations.push(location);
        inputs.push(value);
    }
    let batch = session
        .analyze_bounded_batch(inputs, projection)
        .map_err(|error| ExecutionError::Analysis {
            ordinal: Some(error_ordinal),
            source_name: None,
            source: Box::new(error),
        })?;
    let analyzed = batch.into_inputs().into_vec();
    if analyzed.len() != identities.len() {
        return Err(ExecutionError::Source {
            ordinal: error_ordinal,
            message: "analyzer changed input-mode cardinality or identity".to_owned(),
        });
    }
    analyzed
        .into_iter()
        .zip(locations)
        .zip(identities)
        .map(|((analyzed, location), (input_id, analyzer_segmented))| {
            if analyzed.input().id() != input_id {
                return Err(ExecutionError::Source {
                    ordinal: input_id.get(),
                    message: "analyzer changed input-mode cardinality or identity".to_owned(),
                });
            }
            Ok(AnalyzedTextInput {
                location,
                analyzer_segmented,
                analyzed,
            })
        })
        .collect()
}

struct SessionAnalyzer<'a, A> {
    session: Option<&'a mut A>,
}

impl<A: AnalyzerSession> SessionAnalyzer<'_, A> {
    fn analyze_batch(
        &mut self,
        sentences: Vec<ExecutionSentence<TextUnit>>,
        projection: &BoundProjection,
    ) -> Result<Vec<AnalyzedTextInput>, ExecutionError> {
        let first_ordinal = sentences.first().map_or(0, |input| input.value.id().get());
        let session = self
            .session
            .as_deref_mut()
            .ok_or_else(|| ExecutionError::Source {
                ordinal: first_ordinal,
                message: "text input requires an analyzer session".to_owned(),
            })?;
        analyze_text_batch(session, sentences, projection, first_ordinal)
    }
}

struct BoundExecutionPlan<'a> {
    schema: &'a TokenSchema,
    projection: &'a BoundProjection,
    limits: &'a ExecutionLimits,
    scope: &'a SentenceAdmission,
    reach: BTreeMap<LookaroundScope, LookaroundReach>,
}

fn execute_bound_with_matcher<S, A, K, P, O, M>(
    source: S,
    analyzer: &mut SessionAnalyzer<'_, A>,
    plan: BoundExecutionPlan<'_>,
    mut runner: PartitionRunner<'_, K, P, O, M>,
) -> Result<RunOutcome, ExecutionError>
where
    S: SentenceBatchSource + Send,
    A: AnalyzerSession,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
    M: FnMut(
        &[BoundSentenceInput],
        &[bool],
        Range<usize>,
    ) -> Result<Vec<Vec<Match>>, ExecutionError>,
{
    plan.limits.validate()?;
    if runner.logical_batch_sentences == 0 {
        return Err(ExecutionError::InvalidLimit {
            field: "logical_batch_sentences",
            message: "must be greater than zero".to_owned(),
        });
    }

    let cancel = runner.cancel;
    std::thread::scope(|threads| {
        let mut partitioner = Partitioner::new(
            plan.reach,
            plan.scope,
            plan.limits.max_tokenized_bytes,
            runner.logical_batch_sentences,
        );
        let mut next_analyzed_corpus_sentence = 0_u64;
        let mut next_document_sentence = std::collections::BTreeMap::<u64, u64>::new();
        let mut last_unit: Option<(DocumentId, u64)> = None;
        let mut feed = BatchFeed::new(threads, source, plan.limits, cancel);
        loop {
            if cancel.is_cancelled() {
                return Ok(runner.into_cancelled());
            }
            let Some(batch) = feed.next()? else {
                if cancel.is_cancelled() {
                    return Ok(runner.into_cancelled());
                }
                let control = partitioner.finish(&mut runner)?;
                if control == BatchControl::Cancelled {
                    return Ok(runner.into_cancelled());
                }
                runner.finalize_pending()?;
                return Ok(if cancel.is_cancelled() {
                    runner.into_cancelled()
                } else {
                    runner.into_completed()
                });
            };
            if cancel.is_cancelled() {
                return Ok(runner.into_cancelled());
            }

            let control = match batch {
                ExecutionBatch::Text(sentences) => {
                    validate_batch_count(sentences.len(), plan.limits)?;
                    validate_raw_sentences(&sentences, plan.limits)?;
                    let mut projected = Vec::with_capacity(sentences.len());
                    let mut estimates = Vec::with_capacity(sentences.len());
                    let mut adjacency = Vec::with_capacity(sentences.len());
                    // One dispatch for the whole batch, so a lane-parallel
                    // session has a batch to spread. Ordinal assignment stays
                    // strictly sequential below, because it is what makes a
                    // run's sentence numbering independent of how the analysis
                    // was scheduled.
                    let analyzed_batch = analyzer.analyze_batch(sentences, plan.projection)?;
                    for AnalyzedTextInput {
                        location,
                        analyzer_segmented,
                        analyzed,
                    } in analyzed_batch
                    {
                        let unit = (location.document_id, location.document_sentence_ordinal);
                        let mut adjacent = last_unit
                            .is_some_and(|(document, ordinal)| (document, ordinal + 1) == unit);
                        last_unit = Some(unit);
                        for sentence in analyzed.into_parts().1.into_vec() {
                            let words = sentence.into_words();
                            adjacency.push(adjacent);
                            adjacent = true;
                            let output_location = if analyzer_segmented {
                                let document_sentence = next_document_sentence
                                    .entry(location.document_id.get())
                                    .or_default();
                                let output = SentenceLocation {
                                    lookaround_divisions: location.lookaround_divisions.clone(),
                                    source_metadata: location.source_metadata.clone(),
                                    corpus_sentence_ordinal: next_analyzed_corpus_sentence,
                                    document_id: location.document_id,
                                    document_ordinal: location.document_ordinal,
                                    document_sentence_ordinal: *document_sentence,
                                    source_name: location.source_name.clone(),
                                };
                                *document_sentence = document_sentence
                                    .checked_add(1)
                                    .ok_or(ExecutionError::ResourceAccounting)?;
                                next_analyzed_corpus_sentence = next_analyzed_corpus_sentence
                                    .checked_add(1)
                                    .ok_or(ExecutionError::ResourceAccounting)?;
                                output
                            } else {
                                next_analyzed_corpus_sentence = location
                                    .corpus_sentence_ordinal
                                    .checked_add(1)
                                    .ok_or(ExecutionError::ResourceAccounting)?;
                                location.clone()
                            };
                            let estimate = words
                                .resident_bytes()
                                .map_err(|_| ExecutionError::ResourceAccounting)?;
                            if estimate > plan.limits.max_tokenized_bytes {
                                return Err(ExecutionError::TokenizedSentenceTooLarge {
                                    ordinal: output_location.corpus_sentence_ordinal,
                                    actual: estimate,
                                    limit: plan.limits.max_tokenized_bytes,
                                    source_name: Some(output_location.source_name.to_string()),
                                });
                            }
                            projected.push(ExecutionSentence {
                                location: output_location,
                                value: BoundSentenceInput::new(
                                    words,
                                    EnhancedGraphInput::NotRequested,
                                ),
                            });
                            estimates.push(estimate);
                        }
                    }
                    validate_batch_count(projected.len(), plan.limits)?;
                    partitioner.push_batch(projected, estimates, Some(adjacency), &mut runner)?
                }
                ExecutionBatch::Tokenized { sentences, charges } => {
                    validate_batch_count(sentences.len(), plan.limits)?;
                    let estimates = match charges {
                        Some(charges) => admit_supplied_charges(&sentences, charges, plan.limits)?,
                        None => validate_tokenized_sentences(&sentences, plan.limits)?,
                    };
                    let projected = sentences
                        .into_iter()
                        .map(|sentence| {
                            BoundTokenSentence::project_for_schema(
                                plan.schema,
                                plan.projection,
                                &sentence.value,
                            )
                            .map(|value| ExecutionSentence {
                                location: sentence.location,
                                value: BoundSentenceInput::new(
                                    value,
                                    EnhancedGraphInput::NotRequested,
                                ),
                            })
                            .map_err(|_| ExecutionError::ResourceAccounting)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    partitioner.push_batch(projected, estimates, None, &mut runner)?
                }
                ExecutionBatch::BoundTokenized { sentences, charges } => {
                    validate_batch_count(sentences.len(), plan.limits)?;
                    let estimates =
                        admit_bound_charges(&sentences, charges, plan.limits, plan.projection)?;
                    partitioner.push_batch(sentences, estimates, None, &mut runner)?
                }
            };
            if control == BatchControl::Cancelled {
                return Ok(runner.into_cancelled());
            }
            if control == BatchControl::Stop {
                runner.finalize_pending()?;
                return Ok(runner.into_completed());
            }
        }
    })
}

#[derive(Debug, Default)]
struct PublicationState {
    current: ExecutionReport,
    logical_pending: usize,
    labels: Box<[crate::fsm::LabelDeclaration]>,
    span_kinds: Box<[String]>,
}

impl PublicationState {
    fn admit(&mut self, sentence_count: usize) -> Result<(), ExecutionError> {
        self.current.admit(sentence_count)
    }

    fn new(labels: &[crate::fsm::LabelDeclaration], span_kinds: Box<[String]>) -> Self {
        Self {
            labels: labels.into(),
            span_kinds,
            ..Self::default()
        }
    }

    fn observe_match(
        &mut self,
        sentence: &dyn TokenSequence,
        matches: usize,
    ) -> Result<(), ExecutionError> {
        crate::fsm::labels::observe_out_of_set(
            &self.labels,
            sentence,
            &mut self.current.out_of_set_labels,
        );
        if !self.span_kinds.is_empty() {
            let observed = &mut self.current.span_kinds_observed;
            observed.resize(self.span_kinds.len(), false);
            for (kind, observed) in self.span_kinds.iter().zip(observed) {
                *observed = *observed || sentence.spans().iter().any(|span| span.kind == *kind);
            }
        }
        self.current.observe_match(sentence, matches)
    }

    fn observe_emission(&mut self, records_emitted: usize) -> Result<(), ExecutionError> {
        self.current.observe_emission(records_emitted)?;
        self.logical_pending = self
            .logical_pending
            .checked_add(1)
            .ok_or(ExecutionError::ResourceAccounting)?;
        Ok(())
    }

    fn finalize<K, P>(&mut self, sink: &mut K, progress: &mut P) -> Result<(), ExecutionError>
    where
        K: MatchSink,
        P: ProgressSink<K>,
    {
        sink.finish_batch()?;
        progress.update(self.current.progress(), &*sink);
        self.logical_pending = 0;
        Ok(())
    }
}

enum BatchFeed<'a, S> {
    Inline {
        source: S,
        limits: &'a ExecutionLimits,
    },
    Piped {
        batches: std::sync::mpsc::Receiver<Result<Option<ExecutionBatch>, ExecutionError>>,
    },
}

impl<'a, S: SentenceBatchSource + Send> BatchFeed<'a, S> {
    fn new<'scope>(
        scope: &'scope std::thread::Scope<'scope, 'a>,
        mut source: S,
        limits: &'a ExecutionLimits,
        cancel: &'a CancellationFlag,
    ) -> Self
    where
        S: 'scope,
        'a: 'scope,
    {
        if !source.wants_lookahead() {
            return Self::Inline { source, limits };
        }
        // A rendezvous channel, not a buffered one. With capacity 0 the
        // producer decodes exactly one batch ahead and then blocks holding it,
        // so at most two batches are resident: the one being published and the
        // one just decoded. `sync_channel(1)` would allow a third — one
        // buffered plus one held by a blocked sender.
        let (sender, batches) = std::sync::mpsc::sync_channel(0);
        scope.spawn(move || {
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                let pulled = source.next_batch(limits);
                let eof = matches!(pulled, Ok(None) | Err(_));
                // A send failure means the consumer stopped and dropped the
                // receiver. Nothing is owed to it, including the error.
                if sender.send(pulled).is_err() || eof {
                    return;
                }
            }
        });
        Self::Piped { batches }
    }

    /// Returns the next batch, `None` at clean EOF, propagating source errors
    /// in the position the source produced them.
    fn next(&mut self) -> Result<Option<ExecutionBatch>, ExecutionError> {
        match self {
            Self::Inline { source, limits } => source.next_batch(limits),
            // A disconnect without a terminator means the producer observed
            // cancellation; the executor's own check reports that.
            Self::Piped { batches } => batches.recv().unwrap_or(Ok(None)),
        }
    }
}

fn validate_batch_count(actual: usize, limits: &ExecutionLimits) -> Result<(), ExecutionError> {
    if actual == 0 {
        return Err(ExecutionError::InvalidLimit {
            field: "source_batch_sentences",
            message: "source returned an empty batch".to_owned(),
        });
    }
    if actual > limits.max_stage_sentences {
        return Err(ExecutionError::InvalidLimit {
            field: "source_batch_sentences",
            message: format!(
                "source returned {actual} sentences; limit is {}",
                limits.max_stage_sentences
            ),
        });
    }
    Ok(())
}

fn validate_raw_sentences(
    sentences: &[ExecutionSentence<TextUnit>],
    limits: &ExecutionLimits,
) -> Result<(), ExecutionError> {
    let mut stage_bytes = 0usize;
    for sentence in sentences {
        let actual = sentence.value.len();
        if actual > limits.max_sentence_bytes {
            return Err(ExecutionError::SentenceTooLarge {
                ordinal: sentence.location.corpus_sentence_ordinal,
                actual,
                limit: limits.max_sentence_bytes,
                source_name: Some(sentence.location.source_name.to_string()),
            });
        }
        stage_bytes = stage_bytes
            .checked_add(actual)
            .ok_or(ExecutionError::ResourceAccounting)?;
    }
    if sentences.len() > 1 && stage_bytes > limits.max_stage_input_bytes {
        return Err(ExecutionError::InvalidLimit {
            field: "source_batch_input_bytes",
            message: format!(
                "source returned {stage_bytes} UTF-8 bytes; limit is {}",
                limits.max_stage_input_bytes
            ),
        });
    }
    Ok(())
}

fn admit_supplied_charges(
    sentences: &[ExecutionSentence<Vec<TokenInfo>>],
    charges: Vec<usize>,
    limits: &ExecutionLimits,
) -> Result<Vec<usize>, ExecutionError> {
    if charges.len() != sentences.len() {
        return Err(ExecutionError::InvalidLimit {
            field: "source_batch_charges",
            message: format!(
                "source supplied {} charges for {} sentences",
                charges.len(),
                sentences.len()
            ),
        });
    }
    for (sentence, charge) in sentences.iter().zip(&charges) {
        if *charge > limits.max_tokenized_bytes {
            return Err(ExecutionError::TokenizedSentenceTooLarge {
                ordinal: sentence.location.corpus_sentence_ordinal,
                actual: *charge,
                limit: limits.max_tokenized_bytes,
                source_name: Some(sentence.location.source_name.to_string()),
            });
        }
    }
    Ok(charges)
}

fn admit_bound_charges(
    sentences: &[ExecutionSentence<BoundSentenceInput>],
    charges: Vec<usize>,
    limits: &ExecutionLimits,
    projection: &BoundProjection,
) -> Result<Vec<usize>, ExecutionError> {
    if charges.len() != sentences.len() {
        return Err(ExecutionError::InvalidLimit {
            field: "source_batch_charges",
            message: format!(
                "source supplied {} charges for {} sentences",
                charges.len(),
                sentences.len()
            ),
        });
    }
    for (sentence, charge) in sentences.iter().zip(&charges) {
        if sentence.value.words().schema_digest() != projection.schema_digest() {
            return Err(ExecutionError::Source {
                ordinal: sentence.location.corpus_sentence_ordinal,
                message: "bound token sentence schema does not match the execution binding"
                    .to_owned(),
            });
        }
        if *charge > limits.max_tokenized_bytes {
            return Err(ExecutionError::TokenizedSentenceTooLarge {
                ordinal: sentence.location.corpus_sentence_ordinal,
                actual: *charge,
                limit: limits.max_tokenized_bytes,
                source_name: Some(sentence.location.source_name.to_string()),
            });
        }
    }
    Ok(charges)
}

fn validate_tokenized_sentences(
    sentences: &[ExecutionSentence<Vec<TokenInfo>>],
    limits: &ExecutionLimits,
) -> Result<Vec<usize>, ExecutionError> {
    sentences
        .iter()
        .map(|sentence| {
            let actual = estimate_token_info_bytes(&sentence.value)?;
            if actual > limits.max_tokenized_bytes {
                return Err(ExecutionError::TokenizedSentenceTooLarge {
                    ordinal: sentence.location.corpus_sentence_ordinal,
                    actual,
                    limit: limits.max_tokenized_bytes,
                    source_name: Some(sentence.location.source_name.to_string()),
                });
            }
            Ok(actual)
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Found<'a> {
    Matches(&'a [Vec<Match>]),
    /// Window counts of a token-window catalog that was not matched.
    Windows(&'a [usize]),
}

struct ReadyPartition<'a, V> {
    locations: &'a [SentenceLocation],
    held: &'a [V],
    links: &'a [bool],
    targets: Range<usize>,
    charged_bytes: usize,
}

trait PartitionConsumer<V> {
    fn logical_pending(&self) -> usize;

    fn consume_partition(
        &mut self,
        partition: ReadyPartition<'_, V>,
    ) -> Result<BatchControl, ExecutionError>;
}

struct PartitionRunner<'a, K, P, O, M> {
    pool: &'a rayon::ThreadPool,
    cancel: &'a CancellationFlag,
    logical_batch_sentences: usize,
    sink: &'a mut K,
    progress: &'a mut P,
    observer: &'a mut O,
    window_widths: Option<&'a [usize]>,
    match_batch: M,
    state: PublicationState,
}

impl<K, P, O, M> PartitionRunner<'_, K, P, O, M>
where
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
{
    fn finalize_pending(&mut self) -> Result<(), ExecutionError> {
        if self.state.logical_pending != 0 {
            self.state.finalize(self.sink, self.progress)?;
        }
        Ok(())
    }

    fn into_cancelled(self) -> RunOutcome {
        RunOutcome::Cancelled(self.state.current)
    }

    fn into_completed(self) -> RunOutcome {
        RunOutcome::Completed(self.state.current)
    }

    fn publish_partition<V: TokenSequence + Sync>(
        &mut self,
        locations: &[SentenceLocation],
        sentences: &[V],
        found: Found<'_>,
    ) -> Result<BatchControl, ExecutionError> {
        if self.cancel.is_cancelled() {
            return Ok(BatchControl::Cancelled);
        }
        let (controls, cancelled) = match found {
            Found::Matches(found) => {
                self.sink
                    .push_partition(self.pool, locations, sentences, found, self.cancel)?
            }
            Found::Windows(_) => {
                self.sink
                    .push_windows(self.pool, locations, sentences, self.cancel)?
            }
        };
        let controls_len = controls.len();
        for (index, control) in controls.into_iter().enumerate() {
            let supplied = match found {
                Found::Matches(found) => found[index].len(),
                Found::Windows(windows) => windows[index],
            };
            let (records_emitted, stop) = control.emitted_records(supplied)?;
            self.state.observe_emission(records_emitted)?;
            if stop {
                if self.state.logical_pending == self.logical_batch_sentences {
                    self.state.finalize(self.sink, self.progress)?;
                }
                return Ok(BatchControl::Stop);
            }
        }
        if self.state.logical_pending == self.logical_batch_sentences {
            self.state.finalize(self.sink, self.progress)?;
        }
        if cancelled {
            Ok(BatchControl::Cancelled)
        } else {
            if controls_len != sentences.len() {
                return Err(ExecutionError::Sink(
                    "partition sink returned fewer controls without stopping or cancellation"
                        .into(),
                ));
            }
            Ok(BatchControl::Continue)
        }
    }

    fn run_partition<V: TokenSequence + Sync>(
        &mut self,
        partition: ReadyPartition<'_, V>,
    ) -> Result<BatchControl, ExecutionError>
    where
        M: FnMut(&[V], &[bool], Range<usize>) -> Result<Vec<Vec<Match>>, ExecutionError>,
    {
        let ReadyPartition {
            locations,
            held,
            links,
            targets,
            charged_bytes,
        } = partition;
        if self.cancel.is_cancelled() {
            return Ok(BatchControl::Cancelled);
        }
        let sentences = &held[targets.clone()];
        self.state.admit(sentences.len())?;
        self.observer.observe(TokenizedBatchObservation {
            sentence_count: sentences.len(),
            charged_bytes,
        })?;
        if self.cancel.is_cancelled() {
            return Ok(BatchControl::Cancelled);
        }

        if let Some(widths) = self.window_widths {
            let windows = sentences
                .iter()
                .map(|sentence| {
                    widths
                        .iter()
                        .map(|width| (sentence.len() + 1).saturating_sub(*width))
                        .sum::<usize>()
                })
                .collect::<Vec<_>>();
            for (sentence, windows) in sentences.iter().zip(&windows) {
                self.state.observe_match(sentence, *windows)?;
            }
            return self.publish_partition(locations, sentences, Found::Windows(&windows));
        }

        let mut next = targets.start;
        let mut width = targets.len();
        while next < targets.end {
            if self.cancel.is_cancelled() {
                return Ok(BatchControl::Cancelled);
            }
            let end = next + width.min(targets.end - next);
            let found = match (self.match_batch)(held, links, next..end) {
                Err(ExecutionError::Matching(crate::fsm::BoundExecutionError::Resource(
                    crate::fsm::MatchResourceError::Cancelled,
                ))) => return Ok(BatchControl::Cancelled),
                Err(ExecutionError::Matching(crate::fsm::BoundExecutionError::Resource(
                    crate::fsm::MatchResourceError::BatchOutput { .. },
                ))) if end - next > 1 => {
                    width = (end - next) / 2;
                    continue;
                }
                result => result?,
            };
            let sentences = &held[next..end];
            debug_assert_eq!(found.len(), sentences.len());
            for (sentence, matches) in sentences.iter().zip(&found) {
                self.state.observe_match(sentence, matches.len())?;
            }
            let control = self.publish_partition(
                &locations[next - targets.start..end - targets.start],
                sentences,
                Found::Matches(&found),
            )?;
            if control != BatchControl::Continue {
                return Ok(control);
            }
            next = end;
        }
        Ok(BatchControl::Continue)
    }
}

impl<V, K, P, O, M> PartitionConsumer<V> for PartitionRunner<'_, K, P, O, M>
where
    V: TokenSequence + Sync,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
    M: FnMut(&[V], &[bool], Range<usize>) -> Result<Vec<Vec<Match>>, ExecutionError>,
{
    fn logical_pending(&self) -> usize {
        self.state.logical_pending
    }

    fn consume_partition(
        &mut self,
        partition: ReadyPartition<'_, V>,
    ) -> Result<BatchControl, ExecutionError> {
        self.run_partition(partition)
    }
}

struct Partitioner<'a, V> {
    windows: WindowLinks,
    scope: &'a SentenceAdmission,
    max_tokenized_bytes: usize,
    logical_batch_sentences: usize,
    values: Vec<V>,
    links: Vec<bool>,
    margin: usize,
    ready: usize,
    ready_bytes: usize,
    held_bytes: usize,
    locations: Vec<SentenceLocation>,
    estimates: Vec<usize>,
}

impl<'a, V: TokenSequence> Partitioner<'a, V> {
    fn new(
        reach: BTreeMap<LookaroundScope, LookaroundReach>,
        scope: &'a SentenceAdmission,
        max_tokenized_bytes: usize,
        logical_batch_sentences: usize,
    ) -> Self {
        Self {
            windows: WindowLinks::new(reach),
            scope,
            max_tokenized_bytes,
            logical_batch_sentences,
            values: Vec::new(),
            links: Vec::new(),
            margin: 0,
            ready: 0,
            ready_bytes: 0,
            held_bytes: 0,
            locations: Vec::new(),
            estimates: Vec::new(),
        }
    }

    fn push_batch(
        &mut self,
        sentences: Vec<ExecutionSentence<V>>,
        estimates: Vec<usize>,
        adjacency: Option<Vec<bool>>,
        consumer: &mut impl PartitionConsumer<V>,
    ) -> Result<BatchControl, ExecutionError> {
        debug_assert_eq!(sentences.len(), estimates.len());
        let mut adjacency = adjacency.map(Vec::into_iter);
        for (sentence, estimate) in sentences.into_iter().zip(estimates) {
            let explicit = adjacency.as_mut().and_then(Iterator::next);
            if !self.scope.admits(&sentence.location) {
                self.windows.interrupt();
                continue;
            }
            self.windows
                .push(&sentence.location, explicit, &mut self.links);
            let incoming = estimate
                .checked_add(self.sentence_overhead())
                .ok_or(ExecutionError::ResourceAccounting)?;
            if self.windows.scope_count() > 0
                && self
                    .held_bytes
                    .checked_add(incoming)
                    .ok_or(ExecutionError::ResourceAccounting)?
                    > self.max_tokenized_bytes
            {
                let control = self.advance(false, consumer)?;
                if control != BatchControl::Continue {
                    return Ok(control);
                }
                let control = self.flush(consumer)?;
                if control != BatchControl::Continue {
                    return Ok(control);
                }
                self.retire();
            }
            self.held_bytes = self
                .held_bytes
                .checked_add(incoming)
                .ok_or(ExecutionError::ResourceAccounting)?;
            self.values.push(sentence.value);
            self.locations.push(sentence.location);
            self.estimates.push(estimate);
            if self.windows.scope_count() > 0 && self.held_bytes > self.max_tokenized_bytes {
                self.retire();
                if self.held_bytes > self.max_tokenized_bytes {
                    return Err(ExecutionError::LookaroundWindowTooLarge {
                        actual: self.held_bytes,
                        limit: self.max_tokenized_bytes,
                    });
                }
            }
            let control = self.advance(false, consumer)?;
            if control != BatchControl::Continue {
                return Ok(control);
            }
        }
        self.flush(consumer)
    }

    fn sentence_overhead(&self) -> usize {
        std::mem::size_of::<V>()
            + std::mem::size_of::<SentenceLocation>()
            + std::mem::size_of::<usize>()
            + self.windows.scope_count()
    }

    fn finish(
        &mut self,
        consumer: &mut impl PartitionConsumer<V>,
    ) -> Result<BatchControl, ExecutionError> {
        let control = self.advance(true, consumer)?;
        if control != BatchControl::Continue {
            return Ok(control);
        }
        self.flush(consumer)
    }

    fn advance(
        &mut self,
        end: bool,
        consumer: &mut impl PartitionConsumer<V>,
    ) -> Result<BatchControl, ExecutionError> {
        while self.ready < self.values.len() && (end || self.followed(self.ready)) {
            let estimate = self.estimates[self.ready];
            let next_bytes = self
                .ready_bytes
                .checked_add(estimate)
                .ok_or(ExecutionError::ResourceAccounting)?;
            let logical_capacity = self.logical_batch_sentences - consumer.logical_pending();
            let ready = self.ready - self.margin;
            if ready > 0 && (next_bytes > self.max_tokenized_bytes || ready == logical_capacity) {
                let control = self.flush(consumer)?;
                if control != BatchControl::Continue {
                    return Ok(control);
                }
            }
            self.ready_bytes = self
                .ready_bytes
                .checked_add(estimate)
                .ok_or(ExecutionError::ResourceAccounting)?;
            self.ready += 1;
        }
        Ok(BatchControl::Continue)
    }

    fn followed(&self, index: usize) -> bool {
        let scopes = self.windows.scope_count();
        self.windows.reach().enumerate().all(|(scope, reach)| {
            !window_at(&self.values, &self.links, scopes, scope, reach, index).open_after
                || self.links.get(self.values.len() * scopes + scope) == Some(&false)
        })
    }

    fn flush(
        &mut self,
        consumer: &mut impl PartitionConsumer<V>,
    ) -> Result<BatchControl, ExecutionError> {
        if self.ready == self.margin {
            return Ok(BatchControl::Continue);
        }
        let control = consumer.consume_partition(ReadyPartition {
            locations: &self.locations[..self.ready - self.margin],
            held: &self.values,
            links: &self.links[..self.values.len() * self.windows.scope_count()],
            targets: self.margin..self.ready,
            charged_bytes: self.ready_bytes,
        })?;
        self.retire();
        Ok(control)
    }

    fn retire(&mut self) {
        let retired = self.ready - self.margin;
        self.locations.drain(..retired);
        self.margin = self.ready;
        self.ready_bytes = 0;

        let scopes = self.windows.scope_count();
        let kept_from = self
            .windows
            .reach()
            .enumerate()
            .map(|(scope, reach)| {
                let mut start = self.margin;
                let mut tokens = 0;
                while start > 0 && tokens < reach.before {
                    if self.links.get(start * scopes + scope) == Some(&false) {
                        break;
                    }
                    start -= 1;
                    tokens += self.values[start].len();
                }
                start
            })
            .min()
            .unwrap_or(self.margin);
        let overhead = self.sentence_overhead();
        for estimate in self.estimates.drain(..kept_from) {
            self.held_bytes -= estimate + overhead;
        }
        self.values.drain(..kept_from);
        self.links.drain(..kept_from * self.windows.scope_count());
        self.margin -= kept_from;
        self.ready -= kept_from;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::execution::{
        ExecutionBatch, ExecutionLimits, ExecutionSentence, SentenceBatchSource,
    };
    use crate::token_match::{TokenAttributeIndex, TokenInfo};

    #[test]
    fn output_budget_retries_preserve_context_and_publish_each_sentence_once() {
        for mode in [
            "complete",
            "stop",
            "cancel",
            "replay_cancel",
            "single_error",
            "other_error",
        ] {
            let cancel = CancellationFlag::new();
            let locations = (0..6).map(SentenceLocation::run_local).collect::<Vec<_>>();
            let compiled = crate::fsm::compile_catalog(
                crate::fsm::PatternSource::named(
                    "retry.lx",
                    if mode == "replay_cancel" {
                        "interpret $x as slot {selected: true}\npattern p := (?<x> [text=a])."
                    } else {
                        "pattern p := (?<x> [text=a]) followed_by([text=a], within=document)."
                    },
                ),
                crate::fsm::CompileLimits::default(),
            )
            .unwrap()
            .matcher;
            let schema = TokenSchema::unidic_morphology();
            let bound = compiled.bind(&schema).unwrap();
            let held = (0..8)
                .map(|_| {
                    BoundSentenceInput::new(
                        BoundTokenSentence::project(bound.projection(), &[token("a")]).unwrap(),
                        EnhancedGraphInput::NotRequested,
                    )
                })
                .collect::<Vec<_>>();
            let links = vec![true; 8];
            let mut attempts = Vec::new();
            let mut observations = Vec::new();
            let mut progress = Vec::new();
            let mut pushed = Vec::new();
            struct RecordingSink<'a> {
                rows: &'a mut Vec<(u64, Vec<Match>)>,
                cancel: &'a CancellationFlag,
                mode: &'a str,
            }
            impl MatchSink for RecordingSink<'_> {
                fn push(
                    &mut self,
                    location: &SentenceLocation,
                    _: &dyn TokenSequence,
                    matches: &[Match],
                ) -> Result<SinkControl, ExecutionError> {
                    self.rows
                        .push((location.corpus_sentence_ordinal, matches.to_vec()));
                    if self.mode == "cancel" {
                        self.cancel.cancel();
                    }
                    if self.mode == "stop" {
                        Ok(SinkControl::StopAfter {
                            records_emitted: matches.len(),
                        })
                    } else {
                        Ok(SinkControl::Continue {
                            records_emitted: matches.len(),
                        })
                    }
                }
            }
            let matched = BoundSentenceMatcher::new(&bound)
                .match_in_windows(&pool(), &held, &links, 1..2)
                .unwrap()
                .remove(0)
                .remove(0);
            let mut sink = RecordingSink {
                rows: &mut pushed,
                cancel: &cancel,
                mode,
            };
            let pool = pool();
            let mut progress_sink = |value, _: &RecordingSink<'_>| progress.push(value);
            let mut observer = |observation: TokenizedBatchObservation| {
                observations.push((observation.sentence_count, observation.charged_bytes));
                Ok(())
            };
            let mut runner = PartitionRunner {
                pool: &pool,
                cancel: &cancel,
                logical_batch_sentences: 6,
                sink: &mut sink,
                progress: &mut progress_sink,
                observer: &mut observer,
                window_widths: None,
                match_batch: |sentences: &[BoundSentenceInput],
                              context: &[bool],
                              targets: Range<usize>| {
                    assert_eq!(sentences.len(), 8);
                    assert_eq!(context, links);
                    attempts.push(targets.clone());
                    if mode == "other_error" {
                        return Err(ExecutionError::Sink("unrelated".into()));
                    }
                    if mode == "replay_cancel" {
                        cancel.cancel();
                    } else if targets.len() > 1 || mode == "single_error" {
                        return Err(ExecutionError::Matching(
                            crate::fsm::BoundExecutionError::Resource(
                                crate::fsm::MatchResourceError::BatchOutput { limit: 1 },
                            ),
                        ));
                    }
                    BoundSentenceMatcher::new(&bound)
                        .with_cancellation(&cancel)
                        .match_in_windows(&pool, sentences, context, targets)
                        .map_err(ExecutionError::Matching)
                },
                state: PublicationState::default(),
            };
            let result = runner.run_partition(ReadyPartition {
                locations: &locations,
                held: &held,
                links: &links,
                targets: 1..7,
                charged_bytes: 999,
            });
            let state = runner.state;
            assert_eq!(observations, [(6, 999)]);
            assert_eq!(state.current.sentences_admitted, 6);
            match mode {
                "complete" => {
                    assert_eq!(result.unwrap(), BatchControl::Continue);
                    assert_eq!(
                        pushed,
                        (0..6)
                            .map(|i| (i, vec![matched.clone()]))
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(state.current.matches_found, 6);
                    assert_eq!(progress.len(), 1);
                }
                "stop" | "cancel" => {
                    assert_eq!(
                        result.unwrap(),
                        if mode == "stop" {
                            BatchControl::Stop
                        } else {
                            BatchControl::Cancelled
                        }
                    );
                    assert_eq!(pushed, [(0, vec![matched.clone()])]);
                    assert_eq!(attempts.last(), Some(&(1..2)));
                }
                "replay_cancel" => {
                    assert_eq!(result.unwrap(), BatchControl::Cancelled);
                    assert!(pushed.is_empty());
                    assert_eq!(attempts.len(), 1);
                    assert_eq!(attempts[0], 1..7);
                    assert_eq!(state.current.matches_found, 0);
                }
                "single_error" => {
                    assert!(matches!(
                        result,
                        Err(ExecutionError::Matching(
                            crate::fsm::BoundExecutionError::Resource(
                                crate::fsm::MatchResourceError::BatchOutput { .. }
                            )
                        ))
                    ));
                    assert!(pushed.is_empty());
                    assert_eq!(attempts.last(), Some(&(1..2)));
                }
                _ => {
                    assert!(matches!(result, Err(ExecutionError::Sink(_))));
                    assert!(pushed.is_empty());
                    assert_eq!(attempts.len(), 1);
                    assert_eq!(attempts[0], 1..7);
                }
            }
        }
    }

    struct TestConsumer<F>(F);

    impl<V, F> PartitionConsumer<V> for TestConsumer<F>
    where
        F: FnMut() -> Result<BatchControl, ExecutionError>,
    {
        fn logical_pending(&self) -> usize {
            0
        }

        fn consume_partition(
            &mut self,
            _partition: ReadyPartition<'_, V>,
        ) -> Result<BatchControl, ExecutionError> {
            (self.0)()
        }
    }

    #[test]
    fn empty_sentence_context_is_bounded_by_the_residency_limit() {
        for reach in [
            LookaroundReach {
                before: 1,
                after: 0,
            },
            LookaroundReach {
                before: 0,
                after: 1,
            },
        ] {
            let mut partitioner = Partitioner::new(
                BTreeMap::from([(LookaroundScope::Document, reach)]),
                &SentenceAdmission::All,
                1024,
                1,
            );
            let mut consumer = TestConsumer(|| Ok(BatchControl::Continue));
            let mut rejected = false;
            for ordinal in 0..100 {
                let sentence = ExecutionSentence {
                    location: SentenceLocation::run_local(ordinal),
                    value: Vec::<TokenInfo>::new(),
                };
                let result = partitioner.push_batch(vec![sentence], vec![1], None, &mut consumer);
                if let Err(error) = result {
                    assert!(
                        matches!(error, ExecutionError::LookaroundWindowTooLarge { actual, limit: 1024 } if actual > 1024)
                    );
                    rejected = true;
                    break;
                }
            }
            assert!(
                rejected,
                "empty-sentence context exceeded the byte limit without an error: {reach:?}"
            );
        }
    }

    #[test]
    fn lookaround_payload_charges_are_checked_before_matching() {
        let mut partitioner = Partitioner::new(
            BTreeMap::from([(
                LookaroundScope::Document,
                LookaroundReach {
                    before: 0,
                    after: 2,
                },
            )]),
            &SentenceAdmission::All,
            4096,
            1,
        );
        let mut consumer = TestConsumer(|| {
            panic!("matching started before context and residency checks completed")
        });
        for ordinal in 0..2 {
            let result = partitioner.push_batch(
                vec![ExecutionSentence {
                    location: SentenceLocation::run_local(ordinal),
                    value: vec![token("a")],
                }],
                vec![2048],
                None,
                &mut consumer,
            );
            if ordinal == 0 {
                assert!(result.is_ok());
            } else {
                assert!(
                    matches!(result, Err(ExecutionError::LookaroundWindowTooLarge { actual, limit: 4096 }) if actual > 4096)
                );
            }
        }
    }

    #[test]
    fn preceding_context_is_retired_at_a_document_boundary() {
        let mut partitioner = Partitioner::new(
            BTreeMap::from([(
                LookaroundScope::Document,
                LookaroundReach {
                    before: 1,
                    after: 0,
                },
            )]),
            &SentenceAdmission::All,
            1024,
            1,
        );
        let mut consumer = TestConsumer(|| Ok(BatchControl::Continue));
        for ordinal in 0..10 {
            let mut location = SentenceLocation::run_local(ordinal);
            location.document_id = DocumentId::new(ordinal);
            location.document_sentence_ordinal = 0;
            partitioner
                .push_batch(
                    vec![ExecutionSentence {
                        location,
                        value: Vec::<TokenInfo>::new(),
                    }],
                    vec![1],
                    None,
                    &mut consumer,
                )
                .unwrap();
            assert_eq!(partitioner.values.len(), 1);
        }
    }

    fn token(orth: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token
    }

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
    }

    struct Source(VecDeque<ExecutionBatch>);

    impl SentenceBatchSource for Source {
        fn wants_lookahead(&self) -> bool {
            false
        }

        fn next_batch(
            &mut self,
            _limits: &ExecutionLimits,
        ) -> Result<Option<ExecutionBatch>, ExecutionError> {
            Ok(self.0.pop_front())
        }
    }

    struct CancellingSource {
        batch: Option<ExecutionBatch>,
        cancel: CancellationFlag,
    }

    impl SentenceBatchSource for CancellingSource {
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

    fn source(batches: impl IntoIterator<Item = Vec<(u64, Vec<TokenInfo>)>>) -> Source {
        Source(
            batches
                .into_iter()
                .map(|sentences| {
                    ExecutionBatch::tokenized(
                        sentences
                            .into_iter()
                            .map(|(ordinal, value)| ExecutionSentence {
                                location: SentenceLocation::run_local(ordinal),
                                value,
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    #[derive(Default)]
    struct Sink {
        pushes: usize,
        finishes: usize,
    }

    impl MatchSink for Sink {
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
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn default_partition_publication_preserves_order_and_stop() {
        #[derive(Default)]
        struct OrderedSink(Vec<u64>);

        impl MatchSink for OrderedSink {
            fn push(
                &mut self,
                location: &SentenceLocation,
                _sentence: &dyn TokenSequence,
                _matches: &[Match],
            ) -> Result<SinkControl, ExecutionError> {
                self.0.push(location.corpus_sentence_ordinal);
                if location.corpus_sentence_ordinal == 2 {
                    Ok(SinkControl::StopAfter { records_emitted: 0 })
                } else {
                    Ok(SinkControl::Continue { records_emitted: 0 })
                }
            }
        }

        let locations = (0..4).map(SentenceLocation::run_local).collect::<Vec<_>>();
        let sentences = (0..4).map(|_| vec![token("a")]).collect::<Vec<_>>();
        let found = vec![Vec::new(); 4];
        let mut sink = OrderedSink::default();
        let (controls, cancelled) = sink
            .push_partition(
                &pool(),
                &locations,
                &sentences,
                &found,
                &CancellationFlag::new(),
            )
            .unwrap();

        assert!(!cancelled);
        assert_eq!(sink.0, vec![0, 1, 2]);
        assert_eq!(controls.len(), 3);
        assert!(matches!(
            controls.last(),
            Some(SinkControl::StopAfter { .. })
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_fixture_with_matcher<S, F, K, P, O, M>(
        source: S,
        tokenize: F,
        pool: &rayon::ThreadPool,
        limits: &ExecutionLimits,
        logical_batch_sentences: usize,
        cancel: &CancellationFlag,
        sink: &mut K,
        progress: &mut P,
        observer: &mut O,
        scope: &SentenceAdmission,
        labels: &[crate::fsm::LabelDeclaration],
        mut matcher: M,
    ) -> Result<RunOutcome, ExecutionError>
    where
        S: SentenceBatchSource + Send,
        F: Fn(&str) -> Vec<TokenInfo>,
        K: MatchSink,
        P: ProgressSink<K>,
        O: TokenizedBatchObserver,
        M: FnMut(&[BoundSentenceInput]) -> Vec<Vec<Match>>,
    {
        let mut session = crate::testing::execution::FixtureSession::new(tokenize);
        let schema = session.schema().clone();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        execute_bound_with_matcher(
            source,
            &mut SessionAnalyzer {
                session: Some(&mut session),
            },
            BoundExecutionPlan {
                schema: &schema,
                projection: &projection,
                limits,
                scope,
                reach: BTreeMap::new(),
            },
            PartitionRunner {
                pool,
                cancel,
                logical_batch_sentences,
                sink,
                progress,
                observer,
                window_widths: None,
                match_batch: |sentences: &[BoundSentenceInput],
                              _: &[bool],
                              targets: Range<usize>| {
                    Ok(matcher(&sentences[targets]))
                },
                state: PublicationState::new(labels, Box::default()),
            },
        )
    }

    struct NoopProgress;

    impl ProgressSink<Sink> for NoopProgress {
        fn update(&mut self, _progress: ExecutionProgress, _sink: &Sink) {}
    }

    #[test]
    fn analyzer_finish_runs_only_after_completion_and_propagates_failure() {
        use crate::analysis::{AnalysisError, ValidatedAnalysisBatch};

        struct ProbeSession {
            schema: TokenSchema,
            calls: Arc<AtomicUsize>,
            fail: bool,
        }
        impl crate::analysis::analyzer::sealed::Session for ProbeSession {}
        impl AnalyzerSession for ProbeSession {
            fn schema(&self) -> &TokenSchema {
                &self.schema
            }
            fn analyze_bounded_batch(
                &mut self,
                _: Vec<TextUnit>,
                _: &BoundProjection,
            ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
                Err(AnalysisError::UnsupportedProjection)
            }
            fn finish(&mut self) -> Result<(), AnalysisError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail {
                    Err(AnalysisError::InvalidAlignment)
                } else {
                    Ok(())
                }
            }
        }
        struct ProbeSink(bool);
        impl MatchSink for ProbeSink {
            fn push(
                &mut self,
                _: &SentenceLocation,
                _: &dyn TokenSequence,
                _: &[Match],
            ) -> Result<SinkControl, ExecutionError> {
                if self.0 {
                    Ok(SinkControl::StopAfter { records_emitted: 0 })
                } else {
                    Ok(SinkControl::Continue { records_emitted: 0 })
                }
            }
        }

        for mode in ["eof", "stop", "cancel", "invalid_limit", "finish_error"] {
            let catalog = crate::testing::compile_test_pattern("[]");
            let schema = TokenSchema::unidic_morphology();
            let bound = catalog.bind(&schema).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let session = ProbeSession {
                schema,
                calls: Arc::clone(&calls),
                fail: mode == "finish_error",
            };
            let cancel = CancellationFlag::new();
            if mode == "cancel" {
                cancel.cancel();
            }
            let mut limits = ExecutionLimits::default();
            if mode == "invalid_limit" {
                limits.max_stage_sentences = 0;
            }
            let result = execute_analyzed(
                source([vec![(0, vec![token("a")])], vec![(1, vec![token("b")])]]),
                AnalysisBinding::Session(session),
                &bound,
                &pool(),
                &limits,
                2,
                &cancel,
                &mut ProbeSink(mode == "stop"),
                &mut |_, _: &ProbeSink| {},
                &mut |_: TokenizedBatchObservation| Ok(()),
                super::super::PolicySelection::Apply,
                &SentenceAdmission::All,
            );
            match mode {
                "eof" | "stop" => assert!(matches!(result, Ok(RunOutcome::Completed(_)))),
                "cancel" => assert!(matches!(result, Ok(RunOutcome::Cancelled(_)))),
                "invalid_limit" => {
                    assert!(matches!(result, Err(ExecutionError::InvalidLimit { .. })))
                }
                _ => assert!(matches!(result, Err(ExecutionError::Analysis {
                    ordinal: None, source_name: None, source,
                }) if matches!(*source, AnalysisError::InvalidAlignment))),
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(matches!(mode, "eof" | "stop" | "finish_error")),
                "{mode}"
            );
        }
    }

    #[test]
    fn source_cancellation_prevents_tokenization_observation_matching_and_publication() {
        let cancel = CancellationFlag::new();
        let tokenizations = AtomicUsize::new(0);
        let mut observer_calls = 0usize;
        let mut matcher_calls = 0usize;
        let mut progress_updates = 0usize;
        let mut sink = Sink::default();

        let outcome = execute_fixture_with_matcher(
            CancellingSource {
                batch: Some(ExecutionBatch::Text(vec![ExecutionSentence {
                    location: SentenceLocation::run_local(0),
                    value: TextUnit::sentence(
                        crate::analysis::InputUnitId::new(0),
                        "a",
                        crate::analysis::NormalizationPolicy::None,
                    )
                    .unwrap(),
                }])),
                cancel: cancel.clone(),
            },
            |text| {
                tokenizations.fetch_add(1, Ordering::SeqCst);
                vec![token(text)]
            },
            &pool(),
            &ExecutionLimits::default(),
            1,
            &cancel,
            &mut sink,
            &mut |_progress: ExecutionProgress, _sink: &Sink| {
                progress_updates += 1;
            },
            &mut |_observation: TokenizedBatchObservation| {
                observer_calls += 1;
                Ok(())
            },
            &SentenceAdmission::All,
            &[],
            |sentences| {
                matcher_calls += 1;
                vec![Vec::new(); sentences.len()]
            },
        )
        .unwrap();

        assert_eq!(outcome, RunOutcome::Cancelled(ExecutionReport::default()));
        assert_eq!(tokenizations.load(Ordering::SeqCst), 0);
        assert_eq!(observer_calls, 0);
        assert_eq!(matcher_calls, 0);
        assert_eq!(sink.pushes, 0);
        assert_eq!(sink.finishes, 0);
        assert_eq!(progress_updates, 0);
    }

    #[test]
    fn observer_cancellation_prevents_the_private_matcher_hook() {
        let cancel = CancellationFlag::new();
        let observer_cancel = cancel.clone();
        let mut matcher_calls = 0usize;
        let mut sink = Sink::default();

        let outcome = execute_fixture_with_matcher(
            source([vec![(0, vec![token("a")])]]),
            |_| {
                #[expect(clippy::unreachable, reason = "tokenized source bypasses tokenization")]
                {
                    unreachable!("tokenized source bypasses tokenization")
                }
            },
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
            &SentenceAdmission::All,
            &[],
            |sentences| {
                matcher_calls += 1;
                vec![Vec::new(); sentences.len()]
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            RunOutcome::Cancelled(ExecutionReport {
                policy_stats: crate::fsm::PolicyStats::default(),
                sentences_admitted: 1,
                ..ExecutionReport::default()
            })
        );
        assert_eq!(matcher_calls, 0);
        assert_eq!(sink.pushes, 0);
    }

    #[test]
    fn matcher_cancellation_prevents_sink_publication() {
        let cancel = CancellationFlag::new();
        let matcher_cancel = cancel.clone();
        let mut matcher_calls = 0usize;
        let mut sink = Sink::default();

        let outcome = execute_fixture_with_matcher(
            source([vec![(0, vec![token("a")])]]),
            |_| {
                #[expect(clippy::unreachable, reason = "tokenized source bypasses tokenization")]
                {
                    unreachable!("tokenized source bypasses tokenization")
                }
            },
            &pool(),
            &ExecutionLimits::default(),
            1,
            &cancel,
            &mut sink,
            &mut NoopProgress,
            &mut |_observation: TokenizedBatchObservation| Ok(()),
            &SentenceAdmission::All,
            &[],
            |sentences| {
                matcher_calls += 1;
                matcher_cancel.cancel();
                vec![Vec::new(); sentences.len()]
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            RunOutcome::Cancelled(ExecutionReport {
                sentences_admitted: 1,
                tokens_scanned: 1,
                ..ExecutionReport::default()
            })
        );
        assert_eq!(matcher_calls, 1);
        assert_eq!(sink.pushes, 0);
    }

    #[test]
    fn physical_matcher_calls_do_not_finalize_the_logical_batch_early() {
        let mut matcher_calls = 0usize;
        let mut observer_calls = 0usize;
        let mut progress_updates = 0usize;
        let mut sink = Sink::default();

        struct Progress<'a>(&'a mut usize);
        impl ProgressSink<Sink> for Progress<'_> {
            fn update(&mut self, _progress: ExecutionProgress, _sink: &Sink) {
                *self.0 += 1;
            }
        }

        let outcome = execute_fixture_with_matcher(
            source([vec![(0, vec![token("a")])], vec![(1, vec![token("b")])]]),
            |_| {
                #[expect(clippy::unreachable, reason = "tokenized source bypasses tokenization")]
                {
                    unreachable!("tokenized source bypasses tokenization")
                }
            },
            &pool(),
            &ExecutionLimits::default(),
            2,
            &CancellationFlag::new(),
            &mut sink,
            &mut Progress(&mut progress_updates),
            &mut |_observation: TokenizedBatchObservation| {
                observer_calls += 1;
                Ok(())
            },
            &SentenceAdmission::All,
            &[],
            |sentences| {
                matcher_calls += 1;
                vec![Vec::new(); sentences.len()]
            },
        )
        .unwrap();

        assert!(matches!(outcome, RunOutcome::Completed(_)));
        assert_eq!(observer_calls, 2);
        assert_eq!(matcher_calls, 2);
        assert_eq!(sink.pushes, 2);
        assert_eq!(sink.finishes, 1);
        assert_eq!(progress_updates, 1);
    }
}
