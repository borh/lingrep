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
    /// Number of match records the sink reported publishing.
    ///
    /// What the sink emitted, not what it was handed: a filtering sink counts
    /// only what it published. Compare against `matches_found` to see what
    /// publication dropped.
    pub records_emitted: u64,
}

/// Cumulative lifecycle counters for one execution run.
///
/// Bounded by the catalog rather than by the run: every field is a scalar
/// except [`Self::program_stats`], which holds one entry per authored policy
/// statement, and [`Self::trace`], which holds at most the number of records the
/// caller asked to have explained. A run over ten million sentences reports the
/// same shape as a run over one. That bound is what the struct's earlier
/// "fixed-size" wording was worth, and it survives; only the `Copy` derive it
/// also implied is gone.
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
    /// Number of match records the sink reported publishing.
    ///
    /// What the sink emitted, not what it was handed: a filtering sink counts
    /// only what it published. Compare against `matches_found` to see what
    /// publication dropped.
    pub records_emitted: u64,
    /// What the publication seam suppressed or dropped during this run.
    ///
    /// Reported here because the seam is owned by [`execute_analyzed`], not by the
    /// caller's sink: a surface that wants to report policy activity can no
    /// longer reach into a decorator it does not construct.
    pub policy_stats: crate::fsm::PolicyStats,
    /// What each statement of the catalog's policy program did, in authored
    /// order.
    ///
    /// Empty for a catalog declaring no `policy` block. Sized by the program,
    /// so an authored statement that never fired reports zeroes rather than
    /// being absent — "ran and did nothing" and "was never reached" are the
    /// same observation only if the entry is missing, and the first is what a
    /// researcher debugging an inert statement is looking for.
    ///
    /// Reaches here by the same route and for the same reason as
    /// [`Self::policy_stats`]: the seam is [`execute_analyzed`]'s, so a surface wanting
    /// to report policy activity cannot reach into a decorator it does not
    /// construct.
    pub program_stats: crate::fsm::ProgramStats,
    /// The per-record explanation trace, or `None` when tracing is disabled.
    ///
    /// `None` unless the publication request carried a capacity, and bounded by
    /// that capacity when present — which is what keeps the struct's opening
    /// claim true. A caller that asks for an unbounded explanation cannot: the
    /// request is a number.
    pub trace: Option<crate::fsm::PolicyTrace>,
    /// Indexed like [`CompiledCatalog::label_declarations`](crate::fsm::CompiledCatalog::label_declarations),
    /// or empty when no sentence was observed.
    pub out_of_set_labels: Vec<crate::fsm::OutOfSetValues>,
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
        matches: &[Match],
    ) -> Result<(), ExecutionError> {
        self.sentences_matched = self
            .sentences_matched
            .checked_add(u64::from(!matches.is_empty()))
            .ok_or(ExecutionError::ResourceAccounting)?;
        self.tokens_scanned = self
            .tokens_scanned
            .checked_add(
                u64::try_from(sentence.len()).map_err(|_| ExecutionError::ResourceAccounting)?,
            )
            .ok_or(ExecutionError::ResourceAccounting)?;
        self.matches_found = self
            .matches_found
            .checked_add(
                u64::try_from(matches.len()).map_err(|_| ExecutionError::ResourceAccounting)?,
            )
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

/// A mutable borrow publishes exactly as the sink it borrows.
///
/// This exists so [`execute_analyzed`] can own the publication seam: it wraps the
/// caller's `&mut K` in the policy decorator internally, which requires the
/// borrow itself to be a sink. Without it every caller would have to construct
/// the decorator, which is precisely the arrangement that let `summarize` ship
/// without one.
impl<T: MatchSink + ?Sized> MatchSink for &mut T {
    fn wants_parallel_records_partition(&self) -> bool {
        (**self).wants_parallel_records_partition()
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
    let mut adapted = SealedProgress { inner: progress };
    let mut analyzer = SessionAnalyzer {
        session: binding.session_mut(),
    };
    let outcome = execute_bound_with_matcher(
        source,
        &mut analyzer,
        &schema,
        fsm.projection(),
        pool,
        limits,
        logical_batch_sentences,
        cancel,
        &mut sealed,
        &mut adapted,
        observer,
        scope,
        fsm.compiled.label_declarations(),
        fsm.compiled.lookaround_reach(),
        |sentences, links, targets| {
            BoundSentenceMatcher::new(fsm)
                .match_in_windows(pool, sentences, links, targets)
                .expect("the executor admits only this binding's sentence schema")
        },
    )?;
    if matches!(outcome, RunOutcome::Completed(_))
        && let Some(session) = analyzer.session
    {
        session.finish().map_err(|error| ExecutionError::Source {
            ordinal: 0,
            message: error.to_string(),
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

/// Presents the caller's sink to the caller's progress observer.
///
/// The seam is an implementation detail of [`execute_analyzed`], so a `ProgressSink`
/// written against the caller's own sink type keeps working unchanged.
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

struct AnalyzedWords {
    analyzer_segmented: bool,
    sentences: Vec<(u32, BoundTokenSentence)>,
}

struct SessionAnalyzer<'a, A> {
    session: Option<&'a mut A>,
}

impl<A: AnalyzerSession> SessionAnalyzer<'_, A> {
    fn analyze_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<Vec<AnalyzedWords>, ExecutionError> {
        let first_ordinal = inputs.first().map_or(0, |input| input.id().get());
        let identities = inputs
            .iter()
            .map(|input| {
                (
                    input.id(),
                    input.kind() == crate::analysis::TextUnitKind::Segment,
                )
            })
            .collect::<Vec<_>>();
        let session = self
            .session
            .as_deref_mut()
            .ok_or_else(|| ExecutionError::Source {
                ordinal: first_ordinal,
                message: "text input requires an analyzer session".to_owned(),
            })?;
        let batch = session
            .analyze_bounded_batch(inputs, projection)
            .map_err(|error| ExecutionError::Source {
                ordinal: first_ordinal,
                message: error.to_string(),
            })?;
        let analyzed = batch.into_inputs().into_vec();
        if analyzed.len() != identities.len() {
            return Err(ExecutionError::Source {
                ordinal: first_ordinal,
                message: "analyzer changed input-mode cardinality or identity".to_owned(),
            });
        }
        analyzed
            .into_iter()
            .zip(identities)
            .map(|(analyzed_input, (input_id, analyzer_segmented))| {
                if analyzed_input.input().id() != input_id {
                    return Err(ExecutionError::Source {
                        ordinal: input_id.get(),
                        message: "analyzer changed input-mode cardinality or identity".to_owned(),
                    });
                }
                let (_, sentences) = analyzed_input.into_parts();
                Ok(AnalyzedWords {
                    analyzer_segmented,
                    sentences: sentences
                        .into_vec()
                        .into_iter()
                        .map(|sentence| {
                            (sentence.analyzer_sentence_ordinal(), sentence.into_words())
                        })
                        .collect(),
                })
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_bound_with_matcher<S, A, K, P, O, M>(
    source: S,
    analyzer: &mut SessionAnalyzer<'_, A>,
    schema: &TokenSchema,
    projection: &BoundProjection,
    pool: &rayon::ThreadPool,
    limits: &ExecutionLimits,
    logical_batch_sentences: usize,
    cancel: &CancellationFlag,
    sink: &mut K,
    progress: &mut P,
    observer: &mut O,
    scope: &SentenceAdmission,
    labels: &[crate::fsm::LabelDeclaration],
    reach: BTreeMap<LookaroundScope, LookaroundReach>,
    mut match_batch: M,
) -> Result<RunOutcome, ExecutionError>
where
    S: SentenceBatchSource + Send,
    A: AnalyzerSession,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
    M: FnMut(&[BoundSentenceInput], &[bool], Range<usize>) -> Vec<Vec<Match>>,
{
    limits.validate()?;
    if logical_batch_sentences == 0 {
        return Err(ExecutionError::InvalidLimit {
            field: "logical_batch_sentences",
            message: "must be greater than zero".to_owned(),
        });
    }

    std::thread::scope(|threads| {
        let mut state = PublicationState::new(labels);
        let mut partitioner = Partitioner::new(reach);
        macro_rules! run_with {
            () => {
                |locations: &[SentenceLocation],
                 held: &[BoundSentenceInput],
                 links: &[bool],
                 targets: Range<usize>,
                 charged_bytes: usize,
                 state: &mut PublicationState| {
                    run_partition(
                        locations,
                        held,
                        links,
                        targets,
                        charged_bytes,
                        logical_batch_sentences,
                        pool,
                        cancel,
                        sink,
                        progress,
                        observer,
                        &mut match_batch,
                        state,
                    )
                }
            };
        }
        let mut next_analyzed_corpus_sentence = 0_u64;
        let mut next_document_sentence = std::collections::BTreeMap::<u64, u64>::new();
        let mut last_unit: Option<(DocumentId, u64)> = None;
        let mut feed = BatchFeed::new(threads, source, limits, cancel);
        loop {
            if cancel.is_cancelled() {
                return Ok(state.cancelled());
            }
            let Some(batch) = feed.next()? else {
                if cancel.is_cancelled() {
                    return Ok(state.cancelled());
                }
                let control = partitioner.finish(
                    limits.max_tokenized_bytes,
                    logical_batch_sentences,
                    &mut state,
                    &mut run_with!(),
                )?;
                if control == BatchControl::Cancelled {
                    return Ok(state.cancelled());
                }
                if state.logical_pending != 0 {
                    state.finalize(sink, progress)?;
                }
                return Ok(if cancel.is_cancelled() {
                    state.cancelled()
                } else {
                    RunOutcome::Completed(state.current)
                });
            };
            if cancel.is_cancelled() {
                return Ok(state.cancelled());
            }

            let control = match batch {
                ExecutionBatch::Text(sentences) => {
                    validate_batch_count(sentences.len(), limits)?;
                    validate_raw_sentences(&sentences, limits)?;
                    let mut projected = Vec::with_capacity(sentences.len());
                    let mut estimates = Vec::with_capacity(sentences.len());
                    let mut adjacency = Vec::with_capacity(sentences.len());
                    // One dispatch for the whole batch, so a lane-parallel
                    // session has a batch to spread. Ordinal assignment stays
                    // strictly sequential below, because it is what makes a
                    // run's sentence numbering independent of how the analysis
                    // was scheduled.
                    let mut locations = Vec::with_capacity(sentences.len());
                    let mut inputs = Vec::with_capacity(sentences.len());
                    for ExecutionSentence { location, value } in sentences {
                        locations.push(location);
                        inputs.push(value);
                    }
                    let analyzed_batch = analyzer.analyze_batch(inputs, projection)?;
                    for (location, analyzed) in locations.into_iter().zip(analyzed_batch) {
                        let unit = (location.document_id, location.document_sentence_ordinal);
                        let mut adjacent = last_unit
                            .is_some_and(|(document, ordinal)| (document, ordinal + 1) == unit);
                        last_unit = Some(unit);
                        for (_analyzer_ordinal, sentence) in analyzed.sentences {
                            adjacency.push(adjacent);
                            adjacent = true;
                            let output_location = if analyzed.analyzer_segmented {
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
                            let estimate = sentence
                                .resident_bytes()
                                .map_err(|_| ExecutionError::ResourceAccounting)?;
                            if estimate > limits.max_tokenized_bytes {
                                return Err(ExecutionError::TokenizedSentenceTooLarge {
                                    ordinal: output_location.corpus_sentence_ordinal,
                                    actual: estimate,
                                    limit: limits.max_tokenized_bytes,
                                });
                            }
                            projected.push(ExecutionSentence {
                                location: output_location,
                                value: sentence,
                            });
                            estimates.push(estimate);
                        }
                    }
                    validate_batch_count(projected.len(), limits)?;
                    let projected = projected
                        .into_iter()
                        .map(|sentence| ExecutionSentence {
                            location: sentence.location,
                            value: BoundSentenceInput::new(
                                sentence.value,
                                EnhancedGraphInput::NotRequested,
                            ),
                        })
                        .collect();
                    partitioner.push_batch(
                        projected,
                        estimates,
                        Some(adjacency),
                        scope,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        &mut state,
                        &mut run_with!(),
                    )?
                }
                ExecutionBatch::Tokenized { sentences, charges } => {
                    validate_batch_count(sentences.len(), limits)?;
                    let estimates = match charges {
                        Some(charges) => admit_supplied_charges(&sentences, charges, limits)?,
                        None => validate_tokenized_sentences(&sentences, limits)?,
                    };
                    let projected = sentences
                        .into_iter()
                        .map(|sentence| {
                            BoundTokenSentence::project_for_schema(
                                schema,
                                projection,
                                &sentence.value,
                            )
                            .map(|value| ExecutionSentence {
                                location: sentence.location,
                                value,
                            })
                            .map_err(|_| ExecutionError::ResourceAccounting)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let projected = projected
                        .into_iter()
                        .map(|sentence| ExecutionSentence {
                            location: sentence.location,
                            value: BoundSentenceInput::new(
                                sentence.value,
                                EnhancedGraphInput::NotRequested,
                            ),
                        })
                        .collect();
                    partitioner.push_batch(
                        projected,
                        estimates,
                        None,
                        scope,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        &mut state,
                        &mut run_with!(),
                    )?
                }
                ExecutionBatch::BoundTokenized { sentences, charges } => {
                    validate_batch_count(sentences.len(), limits)?;
                    let estimates = admit_bound_charges(&sentences, charges, limits, projection)?;
                    partitioner.push_batch(
                        sentences,
                        estimates,
                        None,
                        scope,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        &mut state,
                        &mut run_with!(),
                    )?
                }
            };
            if control == BatchControl::Cancelled {
                return Ok(state.cancelled());
            }
            if control == BatchControl::Stop {
                if state.logical_pending != 0 {
                    state.finalize(sink, progress)?;
                }
                return Ok(RunOutcome::Completed(state.current));
            }
        }
    })
}

#[derive(Debug, Default)]
struct PublicationState {
    current: ExecutionReport,
    logical_pending: usize,
    labels: Box<[crate::fsm::LabelDeclaration]>,
}

impl PublicationState {
    fn admit(&mut self, sentence_count: usize) -> Result<(), ExecutionError> {
        self.current.admit(sentence_count)
    }

    fn new(labels: &[crate::fsm::LabelDeclaration]) -> Self {
        Self {
            labels: labels.into(),
            ..Self::default()
        }
    }

    fn observe_match(
        &mut self,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<(), ExecutionError> {
        crate::fsm::labels::observe_out_of_set(
            &self.labels,
            sentence,
            &mut self.current.out_of_set_labels,
        );
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

    /// The run's accounting at a cancellation checkpoint.
    ///
    /// Clones, which costs nothing here: `program_stats` is assembled by
    /// [`execute_analyzed`] at teardown from the sink, so the accumulator this reads
    /// still carries the empty program. If that ever changes, this becomes a
    /// per-checkpoint vector clone and should move to a `take`.
    fn cancelled(&self) -> RunOutcome {
        RunOutcome::Cancelled(self.current.clone())
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
                context: String::new(),
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
                });
            }
            Ok(actual)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn publish_partition<V: TokenSequence + Sync, K, P>(
    locations: &[SentenceLocation],
    sentences: &[V],
    found: &[Vec<Match>],
    logical_batch_sentences: usize,
    pool: &rayon::ThreadPool,
    cancel: &CancellationFlag,
    sink: &mut K,
    progress: &mut P,
    state: &mut PublicationState,
) -> Result<BatchControl, ExecutionError>
where
    K: MatchSink,
    P: ProgressSink<K>,
{
    if cancel.is_cancelled() {
        return Ok(BatchControl::Cancelled);
    }
    let (controls, cancelled) = sink.push_partition(pool, locations, sentences, found, cancel)?;
    let controls_len = controls.len();
    for (index, control) in controls.into_iter().enumerate() {
        let matches = &found[index];
        let (records_emitted, stop) = control.emitted_records(matches.len())?;
        state.observe_emission(records_emitted)?;
        if stop {
            if state.logical_pending == logical_batch_sentences {
                state.finalize(sink, progress)?;
            }
            return Ok(BatchControl::Stop);
        }
    }
    if state.logical_pending == logical_batch_sentences {
        state.finalize(sink, progress)?;
    }
    if cancelled {
        Ok(BatchControl::Cancelled)
    } else {
        if controls_len != found.len() {
            return Err(ExecutionError::Sink(
                "partition sink returned fewer controls without stopping or cancellation".into(),
            ));
        }
        Ok(BatchControl::Continue)
    }
}

trait PartitionRun<V>:
    FnMut(
    &[SentenceLocation],
    &[V],
    &[bool],
    Range<usize>,
    usize,
    &mut PublicationState,
) -> Result<BatchControl, ExecutionError>
{
}

impl<V, R> PartitionRun<V> for R where
    R: FnMut(
        &[SentenceLocation],
        &[V],
        &[bool],
        Range<usize>,
        usize,
        &mut PublicationState,
    ) -> Result<BatchControl, ExecutionError>
{
}

struct Partitioner<V> {
    windows: WindowLinks,
    values: Vec<V>,
    links: Vec<bool>,
    margin: usize,
    ready: usize,
    ready_bytes: usize,
    held_bytes: usize,
    locations: Vec<SentenceLocation>,
    estimates: Vec<usize>,
}

impl<V: TokenSequence> Partitioner<V> {
    fn new(reach: BTreeMap<LookaroundScope, LookaroundReach>) -> Self {
        Self {
            windows: WindowLinks::new(reach),
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

    #[allow(clippy::too_many_arguments)]
    fn push_batch<R: PartitionRun<V>>(
        &mut self,
        sentences: Vec<ExecutionSentence<V>>,
        estimates: Vec<usize>,
        adjacency: Option<Vec<bool>>,
        scope: &SentenceAdmission,
        max_tokenized_bytes: usize,
        logical_batch_sentences: usize,
        state: &mut PublicationState,
        run: &mut R,
    ) -> Result<BatchControl, ExecutionError> {
        debug_assert_eq!(sentences.len(), estimates.len());
        let mut adjacency = adjacency.map(Vec::into_iter);
        for (sentence, estimate) in sentences.into_iter().zip(estimates) {
            let explicit = adjacency.as_mut().and_then(Iterator::next);
            if !scope.admits(&sentence.location) {
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
                    > max_tokenized_bytes
            {
                let control = self.advance(
                    false,
                    max_tokenized_bytes,
                    logical_batch_sentences,
                    state,
                    run,
                )?;
                if control != BatchControl::Continue {
                    return Ok(control);
                }
                let control = self.flush(state, run)?;
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
            if self.windows.scope_count() > 0 && self.held_bytes > max_tokenized_bytes {
                self.retire();
                if self.held_bytes > max_tokenized_bytes {
                    return Err(ExecutionError::LookaroundWindowTooLarge {
                        actual: self.held_bytes,
                        limit: max_tokenized_bytes,
                    });
                }
            }
            let control = self.advance(
                false,
                max_tokenized_bytes,
                logical_batch_sentences,
                state,
                run,
            )?;
            if control != BatchControl::Continue {
                return Ok(control);
            }
        }
        self.flush(state, run)
    }

    fn sentence_overhead(&self) -> usize {
        std::mem::size_of::<V>()
            + std::mem::size_of::<SentenceLocation>()
            + std::mem::size_of::<usize>()
            + self.windows.scope_count()
    }

    fn finish<R: PartitionRun<V>>(
        &mut self,
        max_tokenized_bytes: usize,
        logical_batch_sentences: usize,
        state: &mut PublicationState,
        run: &mut R,
    ) -> Result<BatchControl, ExecutionError> {
        let control = self.advance(
            true,
            max_tokenized_bytes,
            logical_batch_sentences,
            state,
            run,
        )?;
        if control != BatchControl::Continue {
            return Ok(control);
        }
        self.flush(state, run)
    }

    fn advance<R: PartitionRun<V>>(
        &mut self,
        end: bool,
        max_tokenized_bytes: usize,
        logical_batch_sentences: usize,
        state: &mut PublicationState,
        run: &mut R,
    ) -> Result<BatchControl, ExecutionError> {
        while self.ready < self.values.len() && (end || self.followed(self.ready)) {
            let estimate = self.estimates[self.ready];
            let next_bytes = self
                .ready_bytes
                .checked_add(estimate)
                .ok_or(ExecutionError::ResourceAccounting)?;
            let logical_capacity = logical_batch_sentences - state.logical_pending;
            let ready = self.ready - self.margin;
            if ready > 0 && (next_bytes > max_tokenized_bytes || ready == logical_capacity) {
                let control = self.flush(state, run)?;
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

    fn flush<R: PartitionRun<V>>(
        &mut self,
        state: &mut PublicationState,
        run: &mut R,
    ) -> Result<BatchControl, ExecutionError> {
        if self.ready == self.margin {
            return Ok(BatchControl::Continue);
        }
        let control = run(
            &self.locations[..self.ready - self.margin],
            &self.values,
            &self.links[..self.values.len() * self.windows.scope_count()],
            self.margin..self.ready,
            self.ready_bytes,
            state,
        )?;
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

#[allow(clippy::too_many_arguments)]
fn run_partition<V: TokenSequence + Sync, K, P, O, M>(
    locations: &[SentenceLocation],
    held: &[V],
    links: &[bool],
    targets: Range<usize>,
    charged_bytes: usize,
    logical_batch_sentences: usize,
    pool: &rayon::ThreadPool,
    cancel: &CancellationFlag,
    sink: &mut K,
    progress: &mut P,
    observer: &mut O,
    match_batch: &mut M,
    state: &mut PublicationState,
) -> Result<BatchControl, ExecutionError>
where
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
    M: FnMut(&[V], &[bool], Range<usize>) -> Vec<Vec<Match>>,
{
    if cancel.is_cancelled() {
        return Ok(BatchControl::Cancelled);
    }
    let sentences = &held[targets.clone()];
    state.admit(sentences.len())?;
    observer.observe(TokenizedBatchObservation {
        sentence_count: sentences.len(),
        charged_bytes,
    })?;
    if cancel.is_cancelled() {
        return Ok(BatchControl::Cancelled);
    }

    let found = match_batch(held, links, targets);
    debug_assert_eq!(found.len(), sentences.len());
    for (sentence, matches) in sentences.iter().zip(&found) {
        state.observe_match(sentence, matches)?;
    }
    publish_partition(
        locations,
        sentences,
        &found,
        logical_batch_sentences,
        pool,
        cancel,
        sink,
        progress,
        state,
    )
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
            let mut partitioner =
                Partitioner::new(BTreeMap::from([(LookaroundScope::Document, reach)]));
            let mut state = PublicationState::new(&[]);
            let mut rejected = false;
            for ordinal in 0..100 {
                let sentence = ExecutionSentence {
                    location: SentenceLocation::run_local(ordinal),
                    value: Vec::<TokenInfo>::new(),
                };
                let result = partitioner.push_batch(
                    vec![sentence],
                    vec![1],
                    None,
                    &SentenceAdmission::All,
                    1024,
                    1,
                    &mut state,
                    &mut |_: &[SentenceLocation],
                          _: &[Vec<TokenInfo>],
                          _: &[bool],
                          _: Range<usize>,
                          _: usize,
                          _: &mut PublicationState| {
                        Ok(BatchControl::Continue)
                    },
                );
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
        let mut partitioner = Partitioner::new(BTreeMap::from([(
            LookaroundScope::Document,
            LookaroundReach {
                before: 0,
                after: 2,
            },
        )]));
        let mut state = PublicationState::new(&[]);
        for ordinal in 0..2 {
            let result = partitioner.push_batch(
                vec![ExecutionSentence {
                    location: SentenceLocation::run_local(ordinal),
                    value: vec![token("a")],
                }],
                vec![2048],
                None,
                &SentenceAdmission::All,
                4096,
                1,
                &mut state,
                &mut |_: &[SentenceLocation],
                      _: &[Vec<TokenInfo>],
                      _: &[bool],
                      _: Range<usize>,
                      _: usize,
                      _: &mut PublicationState| {
                    panic!("matching started before context and residency checks completed")
                },
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
        let mut partitioner = Partitioner::new(BTreeMap::from([(
            LookaroundScope::Document,
            LookaroundReach {
                before: 1,
                after: 0,
            },
        )]));
        let mut state = PublicationState::new(&[]);
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
                    &SentenceAdmission::All,
                    1024,
                    1,
                    &mut state,
                    &mut |_: &[SentenceLocation],
                          _: &[Vec<TokenInfo>],
                          _: &[bool],
                          _: Range<usize>,
                          _: usize,
                          _: &mut PublicationState| {
                        Ok(BatchControl::Continue)
                    },
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
            &schema,
            &projection,
            pool,
            limits,
            logical_batch_sentences,
            cancel,
            sink,
            progress,
            observer,
            scope,
            labels,
            BTreeMap::new(),
            |sentences, _, targets| matcher(&sentences[targets]),
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
                _ => assert!(matches!(result, Err(ExecutionError::Source { .. }))),
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
