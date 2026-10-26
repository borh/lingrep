//! Canonical bounded matching execution and publication lifecycle.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::analysis::{AnalyzerSession, TextUnit};
use crate::fsm::{
    BoundCatalog, BoundSentenceMatcher, CompiledCatalog, Match, PublishedRecord, SentenceMatcher,
};
use crate::token_match::{
    BoundProjection, BoundTokenSentence, TokenInfo, TokenSchema, TokenSequence,
};

use super::{
    BoundSentenceInput, EnhancedGraphInput, ExecutionBatch, ExecutionError, ExecutionLimits,
    ExecutionSentence, PolicySink, Publication, SentenceAdmission, SentenceBatchSource,
    SentenceLocation, estimate_token_info_bytes,
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
    /// Number of sentences processed by the matcher.
    pub sentences_matched: u64,
    /// Number of tokens processed by the matcher.
    pub tokens_matched: u64,
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
    /// Number of sentences processed by the matcher.
    pub sentences_matched: u64,
    /// Number of tokens processed by the matcher.
    pub tokens_matched: u64,
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
    /// Reported here because the seam is owned by [`execute`], not by the
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
    /// [`Self::policy_stats`]: the seam is [`execute`]'s, so a surface wanting
    /// to report policy activity cannot reach into a decorator it does not
    /// construct.
    pub program_stats: crate::fsm::ProgramStats,
    /// The per-record explanation trace, when the caller asked for one.
    ///
    /// `None` unless the publication request carried a capacity, and bounded by
    /// that capacity when present — which is what keeps the struct's opening
    /// claim true. A caller that asks for an unbounded explanation cannot: the
    /// request is a number.
    pub trace: Option<crate::fsm::PolicyTrace>,
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
            .checked_add(1)
            .ok_or(ExecutionError::ResourceAccounting)?;
        self.tokens_matched = self
            .tokens_matched
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
            tokens_matched: self.tokens_matched,
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
    /// The default deliberately ignores it and retains the sentence-at-a-time
    /// behavior of [`Self::push`]. Cancellation is checked at the same
    /// boundaries as [`Self::push`], and a stopping sink is never
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
/// This exists so [`execute`] can own the publication seam: it wraps the
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

/// Where a run's token schema comes from.
///
/// The executor needs the schema its batches' tokens are in: it refuses a
/// catalog binding compiled against a different one, and re-projects a raw
/// tokenized batch through it. There are exactly two answers, and this type
/// exists so that neither can be left unsaid.
///
/// It replaces `Option<AnalyzerSession>`, where "no session" meant "assume
/// [`TokenSchema::unidic_morphology`]" — correct for the only analyzer that
/// existed when that line was written, and a hard error for every other one.
/// A Sudachi artifact published cleanly from `corpus build --analyzer
/// sudachi` and then failed every read with `bound lingrep patterns schema does not match
/// the analyzer schema`, because the executor asserted the Vibrato schema
/// against a projection bound to the artifact's own. Naming the schema is now
/// the only way to ask for a prepared run.
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
        |sentences| {
            BoundSentenceMatcher::new(fsm)
                .match_all(pool, sentences)
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

/// Executes the canonical matcher over caller-owned `TokenInfo` values.
///
/// New analyzer-backed callers should bind once and use [`execute_analyzed`].
/// This entry point remains for callers that already own canonical `TokenInfo`
/// values, including artifact construction.
#[allow(clippy::too_many_arguments)]
pub fn execute<S, F, K, P, O>(
    source: S,
    tokenize: F,
    fsm: &CompiledCatalog,
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
    F: Fn(&str) -> Vec<TokenInfo> + Sync,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
{
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
    let outcome = execute_with_matcher(
        source,
        tokenize,
        pool,
        limits,
        logical_batch_sentences,
        cancel,
        &mut sealed,
        &mut adapted,
        observer,
        scope,
        |sentences| SentenceMatcher::new(fsm).match_all(pool, sentences),
    )?;
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
/// The seam is an implementation detail of [`execute`], so a `ProgressSink`
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
    mut match_batch: M,
) -> Result<RunOutcome, ExecutionError>
where
    S: SentenceBatchSource + Send,
    A: AnalyzerSession,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
    M: FnMut(&[BoundSentenceInput]) -> Vec<Vec<Match>>,
{
    limits.validate()?;
    if logical_batch_sentences == 0 {
        return Err(ExecutionError::InvalidLimit {
            field: "logical_batch_sentences",
            message: "must be greater than zero".to_owned(),
        });
    }

    std::thread::scope(|threads| {
        let mut state = PublicationState::default();
        let mut next_analyzed_corpus_sentence = 0_u64;
        let mut next_document_sentence = std::collections::BTreeMap::<u64, u64>::new();
        let mut feed = BatchFeed::new(threads, source, limits, cancel);
        loop {
            if cancel.is_cancelled() {
                return Ok(state.cancelled());
            }
            let Some(batch) = feed.next()? else {
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
                        for (_analyzer_ordinal, sentence) in analyzed.sentences {
                            let output_location = if analyzed.analyzer_segmented {
                                let document_sentence = next_document_sentence
                                    .entry(location.document_id.get())
                                    .or_default();
                                let output = SentenceLocation {
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
                    execute_tokenized_partitions(
                        projected,
                        estimates,
                        scope,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        &mut state,
                        |locations, sentences, charged_bytes, state| {
                            run_partition(
                                locations,
                                sentences,
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
                        },
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
                    execute_tokenized_partitions(
                        projected,
                        estimates,
                        scope,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        &mut state,
                        |locations, sentences, charged_bytes, state| {
                            run_partition(
                                locations,
                                sentences,
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
                        },
                    )?
                }
                ExecutionBatch::BoundTokenized { sentences, charges } => {
                    validate_batch_count(sentences.len(), limits)?;
                    let estimates = admit_bound_charges(&sentences, charges, limits, projection)?;
                    execute_tokenized_partitions(
                        sentences,
                        estimates,
                        scope,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        &mut state,
                        |locations, sentences, charged_bytes, state| {
                            run_partition(
                                locations,
                                sentences,
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
                        },
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
}

impl PublicationState {
    fn admit(&mut self, sentence_count: usize) -> Result<(), ExecutionError> {
        self.current.admit(sentence_count)
    }

    fn observe_match(
        &mut self,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<(), ExecutionError> {
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
    /// [`execute`] at teardown from the sink, so the accumulator this reads
    /// still carries the empty program. If that ever changes, this becomes a
    /// per-checkpoint vector clone and should move to a `take`.
    fn cancelled(&self) -> RunOutcome {
        RunOutcome::Cancelled(self.current.clone())
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_with_matcher<S, F, K, P, O, M>(
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
    mut match_batch: M,
) -> Result<RunOutcome, ExecutionError>
where
    S: SentenceBatchSource + Send,
    F: Fn(&str) -> Vec<TokenInfo> + Sync,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
    M: FnMut(&[Vec<TokenInfo>]) -> Vec<Vec<Match>>,
{
    limits.validate()?;
    if logical_batch_sentences == 0 {
        return Err(ExecutionError::InvalidLimit {
            field: "logical_batch_sentences",
            message: "must be greater than zero".to_owned(),
        });
    }

    std::thread::scope(|threads| {
        let mut state = PublicationState::default();
        let mut feed = BatchFeed::new(threads, source, limits, cancel);
        loop {
            if cancel.is_cancelled() {
                return Ok(state.cancelled());
            }

            let batch = feed.next()?;
            if cancel.is_cancelled() {
                return Ok(state.cancelled());
            }
            let Some(batch) = batch else {
                if state.logical_pending != 0 {
                    state.finalize(sink, progress)?;
                }
                if cancel.is_cancelled() {
                    return Ok(state.cancelled());
                }
                return Ok(RunOutcome::Completed(state.current));
            };

            let control = match batch {
                ExecutionBatch::Text(sentences) => {
                    validate_batch_count(sentences.len(), limits)?;
                    validate_raw_sentences(&sentences, limits)?;
                    execute_text_partitions(
                        sentences,
                        &tokenize,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        pool,
                        cancel,
                        sink,
                        progress,
                        observer,
                        &mut match_batch,
                        &mut state,
                    )?
                }
                ExecutionBatch::Tokenized { sentences, charges } => {
                    validate_batch_count(sentences.len(), limits)?;
                    let estimates = match charges {
                        Some(charges) => admit_supplied_charges(&sentences, charges, limits)?,
                        None => validate_tokenized_sentences(&sentences, limits)?,
                    };
                    execute_partitions(
                        sentences,
                        estimates,
                        scope,
                        limits.max_tokenized_bytes,
                        logical_batch_sentences,
                        pool,
                        cancel,
                        sink,
                        progress,
                        observer,
                        &mut match_batch,
                        &mut state,
                    )?
                }
                ExecutionBatch::BoundTokenized { sentences, .. } => {
                    return Err(ExecutionError::Source {
                        ordinal: sentences
                            .first()
                            .map_or(0, |sentence| sentence.location.corpus_sentence_ordinal),
                        message: "bound token batch reached the legacy test matcher".to_owned(),
                    });
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

/// Supplies batches to the executor, optionally one ahead of publication.
///
/// A source that declares [`SentenceBatchSource::wants_lookahead`] is run on
/// its own thread so its read, verify, decompress and decode overlap the
/// matching and publication of the previous batch. Everything else pulls
/// inline, exactly as before.
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

#[allow(clippy::too_many_arguments)]
// Raw text intentionally enters the matcher as singleton token partitions.
// This gives up cross-sentence matcher parallelism on this adapter, but proves
// that no completed tokenized sentence remains resident while the next one is
// tokenized. Logical batch accounting still finalizes at the configured size.
fn execute_text_partitions<F, K, P, O, M>(
    sentences: Vec<ExecutionSentence<TextUnit>>,
    tokenize: &F,
    max_tokenized_bytes: usize,
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
    F: Fn(&str) -> Vec<TokenInfo> + Sync,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
    M: FnMut(&[Vec<TokenInfo>]) -> Vec<Vec<Match>>,
{
    for ExecutionSentence { location, value } in sentences {
        if cancel.is_cancelled() {
            return Ok(BatchControl::Cancelled);
        }
        let tokens = tokenize(value.text());
        if cancel.is_cancelled() {
            return Ok(BatchControl::Cancelled);
        }
        let estimate = estimate_token_info_bytes(&tokens)?;
        if estimate > max_tokenized_bytes {
            return Err(ExecutionError::TokenizedSentenceTooLarge {
                ordinal: location.corpus_sentence_ordinal,
                actual: estimate,
                limit: max_tokenized_bytes,
            });
        }
        let control = run_partition(
            &[location],
            &[tokens],
            estimate,
            logical_batch_sentences,
            pool,
            cancel,
            sink,
            progress,
            observer,
            match_batch,
            state,
        )?;
        if control != BatchControl::Continue {
            return Ok(control);
        }
    }
    Ok(BatchControl::Continue)
}

/// Admits a batch whose source already knows what each sentence costs.
///
/// The shape is checked, because a mismatched charge list would silently pair a
/// sentence with another sentence's residency, and the per-sentence limit is
/// checked, because [`ExecutionError::TokenizedSentenceTooLarge`] is a
/// guarantee the executor owes its caller whatever the source claims. The
/// arithmetic itself is not rechecked — verifying it means the same per-token
/// walk that supplying it exists to avoid.
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

#[allow(clippy::too_many_arguments)]
fn execute_partitions<K, P, O, M>(
    sentences: Vec<ExecutionSentence<Vec<TokenInfo>>>,
    estimates: Vec<usize>,
    scope: &SentenceAdmission,
    max_tokenized_bytes: usize,
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
    M: FnMut(&[Vec<TokenInfo>]) -> Vec<Vec<Match>>,
{
    debug_assert_eq!(sentences.len(), estimates.len());
    execute_tokenized_partitions(
        sentences,
        estimates,
        scope,
        max_tokenized_bytes,
        logical_batch_sentences,
        state,
        |locations, sentences, charged_bytes, state| {
            run_partition(
                locations,
                sentences,
                charged_bytes,
                logical_batch_sentences,
                pool,
                cancel,
                sink,
                progress,
                observer,
                match_batch,
                state,
            )
        },
    )
}

fn execute_tokenized_partitions<V, R>(
    sentences: Vec<ExecutionSentence<V>>,
    estimates: Vec<usize>,
    scope: &SentenceAdmission,
    max_tokenized_bytes: usize,
    logical_batch_sentences: usize,
    state: &mut PublicationState,
    mut run: R,
) -> Result<BatchControl, ExecutionError>
where
    V: TokenSequence + Sync,
    R: FnMut(
        &[SentenceLocation],
        &mut [V],
        usize,
        &mut PublicationState,
    ) -> Result<BatchControl, ExecutionError>,
{
    debug_assert_eq!(sentences.len(), estimates.len());
    let mut partition_locations = Vec::new();
    let mut partition_sentences = Vec::new();
    let mut partition_bytes = 0usize;

    for (sentence, estimate) in sentences.into_iter().zip(estimates) {
        if !scope.admits(&sentence.location) {
            // Dropped before matching, which is what scope conditioning
            // requires. The sentence keeps its ordinal: on a prepared artifact
            // an ordinal is identity, and two runs over one artifact must
            // agree about what sentence 40,000 is.
            //
            // Partitions are built by pushing rather than by subslicing, so an
            // omitted sentence leaves every within-partition length invariant
            // intact.
            continue;
        }
        let next_bytes = partition_bytes
            .checked_add(estimate)
            .ok_or(ExecutionError::ResourceAccounting)?;
        let logical_capacity = logical_batch_sentences - state.logical_pending;
        if !partition_sentences.is_empty()
            && (next_bytes > max_tokenized_bytes || partition_sentences.len() == logical_capacity)
        {
            let control = run(
                &partition_locations,
                &mut partition_sentences,
                partition_bytes,
                state,
            )?;
            if control != BatchControl::Continue {
                return Ok(control);
            }
            partition_locations.clear();
            partition_sentences.clear();
            partition_bytes = 0;
        }

        partition_bytes = partition_bytes
            .checked_add(estimate)
            .ok_or(ExecutionError::ResourceAccounting)?;
        partition_locations.push(sentence.location);
        partition_sentences.push(sentence.value);
    }

    if partition_sentences.is_empty() {
        Ok(BatchControl::Continue)
    } else {
        run(
            &partition_locations,
            &mut partition_sentences,
            partition_bytes,
            state,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn run_partition<V: TokenSequence + Sync, K, P, O, M>(
    locations: &[SentenceLocation],
    sentences: &[V],
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
    M: FnMut(&[V]) -> Vec<Vec<Match>>,
{
    if cancel.is_cancelled() {
        return Ok(BatchControl::Cancelled);
    }
    state.admit(sentences.len())?;
    observer.observe(TokenizedBatchObservation {
        sentence_count: sentences.len(),
        charged_bytes,
    })?;
    if cancel.is_cancelled() {
        return Ok(BatchControl::Cancelled);
    }

    let found = match_batch(sentences);
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

    struct NoopProgress;

    impl ProgressSink<Sink> for NoopProgress {
        fn update(&mut self, _progress: ExecutionProgress, _sink: &Sink) {}
    }

    #[test]
    fn source_cancellation_prevents_tokenization_observation_matching_and_publication() {
        let cancel = CancellationFlag::new();
        let tokenizations = AtomicUsize::new(0);
        let mut observer_calls = 0usize;
        let mut matcher_calls = 0usize;
        let mut progress_updates = 0usize;
        let mut sink = Sink::default();

        let outcome = execute_with_matcher(
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

        let outcome = execute_with_matcher(
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

        let outcome = execute_with_matcher(
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
                sentences_matched: 1,
                tokens_matched: 1,
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

        let outcome = execute_with_matcher(
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
