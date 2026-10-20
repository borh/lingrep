use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use lingrep_core::analysis::VibratoSession;
use lingrep_core::execution::{
    AnalysisBinding, CancellationFlag, ExecutionError, ExecutionLimits, ExecutionProgress,
    OversizedSentenceOmissions, PolicySelection, ProgressSink, RunOutcome, SentenceAdmission,
    TokenizedAdmission, execute_analyzed,
};
use tokio::sync::{Mutex, Semaphore, watch};

use crate::corpus_runtime::{CacheReadiness, CorpusIdentity, CorpusRuntime, ReadyCorpus};
use crate::corpus_set::{CorpusName, CorpusSlot};
use crate::patterns::{compile_web_pattern, format_pattern_compile_error};
use crate::revision::{
    AttemptFailure, CacheReadinessView, CoordinatorEvent, ErrorCategory, RevisionCoordinatorState,
    RevisionId, WorkbenchId,
};
use crate::section_tally::{self, SectionAvailability, SectionRefusal, spawn_span_producer};
use crate::snapshot::{CompletedSnapshot, CompoundCategory};
use crate::snapshot_sink::{SectionScanState, SnapshotSink};
use lingrep_core::fsm::CompileOutput;
use lingrep_core::token_match::AttributeKey;

const DEFAULT_BATCH_SIZE: usize = 1_024;
const LIVE_SUMMARY_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanOptions {
    pub(crate) retained: usize,
    pub(crate) derive_compounds: bool,
    pub(crate) compound_category: CompoundCategory,
}

fn execution_failure() -> AttemptFailure {
    AttemptFailure {
        category: ErrorCategory::Execution,
        message: "The corpus scan could not be completed.".into(),
    }
}

fn integrity_failure() -> AttemptFailure {
    AttemptFailure {
        category: ErrorCategory::Integrity,
        // Same user-facing text as execution_failure per the shared error
        // contract; only the category (telemetry/state) distinguishes it.
        message: "The corpus scan could not be completed.".into(),
    }
}

fn classify_execution_error(error: &ExecutionError) -> AttemptFailure {
    // Log the detail, but expose only the generic message and error category.
    // Never reveal `error`'s display to the browser.
    tracing::warn!(%error, "corpus scan execution error");
    match error {
        ExecutionError::Cache(_) | ExecutionError::Source { .. } => integrity_failure(),
        _ => execution_failure(),
    }
}

fn cache_unavailable_failure() -> AttemptFailure {
    AttemptFailure {
        category: ErrorCategory::Cache,
        message: "The corpus cache is unavailable.".into(),
    }
}

// `Finished` carries the full scan outcome (including the retained-match
// sink); `Panicked` is a rare terminal case with no payload. Boxing would
// only add an allocation on every successful scan to shrink an enum that
// lives for a single match expression, so the size skew is accepted here.
#[allow(clippy::large_enum_variant)]
#[allow(clippy::type_complexity)]
enum ScanTaskResult {
    Finished(
        Result<(RunOutcome, SnapshotSink, Option<OversizedSentenceOmissions>), AttemptFailure>,
    ),
    Panicked,
}

struct ScanResolution {
    event: CoordinatorEvent,
    invalidate_corrupt: bool,
    progress: ExecutionProgress,
}

/// Pure mapping of a finished (or panicked) scan to its terminal coordinator
/// event. The corrupt-cache invalidation is returned as a flag rather than
/// performed here so this stays side-effect-free and unit-testable; the caller
/// runs `invalidate_corrupt().await` when the flag is set.
fn resolve_scan(
    result: ScanTaskResult,
    workbench: WorkbenchId,
    revision: RevisionId,
    identity: CorpusIdentity,
    expected_sentence_count: Option<u64>,
    source: std::sync::Arc<str>,
) -> ScanResolution {
    match result {
        ScanTaskResult::Finished(Ok((RunOutcome::Completed(report), _, ref omissions)))
            if expected_sentence_count.is_some_and(|count| {
                // An omitted oversized sentence legitimately never reaches
                // matching, so completion accounts for the full corpus as
                // matched + omitted; any other delta is corruption.
                count != report.sentences_matched + omissions.as_ref().map_or(0, |o| o.count)
            }) =>
        {
            ScanResolution {
                event: CoordinatorEvent::Failed {
                    workbench,
                    revision,
                    failure: integrity_failure(),
                },
                invalidate_corrupt: true,
                progress: ExecutionProgress::default(),
            }
        }
        // Holds on *every* completed scan, unlike the corpus-count check
        // above: it depends on no optional expectation and on sections being
        // neither available nor refused. The sink is pushed once per sentence
        // the matcher processed, so a divergence is a lost or duplicated
        // publication rather than a tolerable difference.
        //
        // The equality rests on `SnapshotSink` always returning
        // `SinkControl::Continue`. A sink that returned `StopAfter` would
        // complete with `sentences_matched` already counted for the whole
        // partition and publication truncated inside it, and this arm would
        // then read a legitimate early stop as corruption.
        ScanTaskResult::Finished(Ok((RunOutcome::Completed(ref report), ref sink, _)))
            if report.sentences_matched != sink.observed_sentences =>
        {
            tracing::warn!(
                reported = report.sentences_matched,
                observed = sink.observed_sentences,
                "sentence count disagreement between the report and the sink"
            );
            ScanResolution {
                event: CoordinatorEvent::Failed {
                    workbench,
                    revision,
                    failure: integrity_failure(),
                },
                invalidate_corrupt: true,
                progress: ExecutionProgress::default(),
            }
        }
        // The lifecycle guard: `finish` resolves both availability branches, so
        // its absence means the scan never reached finalization and the span
        // stream was never drained. Checked rather than assumed.
        ScanTaskResult::Finished(Ok((RunOutcome::Completed(_), ref sink, _)))
            if sink.finalized_sections.is_none() =>
        {
            ScanResolution {
                event: CoordinatorEvent::Failed {
                    workbench,
                    revision,
                    failure: integrity_failure(),
                },
                invalidate_corrupt: false,
                progress: ExecutionProgress::default(),
            }
        }
        ScanTaskResult::Finished(Ok((
            RunOutcome::Completed(report),
            mut sink,
            oversized_omissions,
        ))) => {
            let sections = sink.finalized_sections.take().unwrap_or(
                // Unreachable: the arm above fails publication when it is
                // `None`. Stated as a value rather than a panic because this
                // function is the one place a scan becomes a snapshot.
                SectionAvailability::VocabularyTooLarge(SectionRefusal {
                    labels: 0,
                    patterns: 0,
                }),
            );
            ScanResolution {
                event: CoordinatorEvent::Completed {
                    workbench,
                    revision,
                    snapshot: Arc::new(CompletedSnapshot {
                        revision,
                        corpus_identity: identity,
                        source: std::sync::Arc::clone(&source),
                        derive_compounds: sink.derive_compounds,
                        compound_category: sink.compound_category,
                        patterns: sink.patterns.into_boxed_slice(),
                        match_refs: sink.refs.into_boxed_slice(),
                        capture_catalog: sink.capture_catalog.into_boxed_slice(),
                        sections,
                        observed_sentences: sink.observed_sentences,
                        sentences_matched: report.sentences_matched,
                        oversized_omissions,
                    }),
                },
                invalidate_corrupt: false,
                progress: ExecutionProgress::default(),
            }
        }
        ScanTaskResult::Finished(Ok((RunOutcome::Cancelled(report), _, _))) => ScanResolution {
            event: CoordinatorEvent::Cancelled {
                workbench,
                revision,
            },
            invalidate_corrupt: false,
            progress: ExecutionProgress {
                sentences_admitted: report.sentences_admitted,
                sentences_matched: report.sentences_matched,
                tokens_matched: report.tokens_matched,
                matches_found: report.matches_found,
                records_emitted: report.records_emitted,
            },
        },
        ScanTaskResult::Finished(Err(failure)) => ScanResolution {
            event: CoordinatorEvent::Failed {
                workbench,
                revision,
                failure,
            },
            invalidate_corrupt: false,
            progress: ExecutionProgress::default(),
        },
        ScanTaskResult::Panicked => ScanResolution {
            event: CoordinatorEvent::Failed {
                workbench,
                revision,
                failure: execution_failure(),
            },
            invalidate_corrupt: false,
            progress: ExecutionProgress::default(),
        },
    }
}

/// Everything the scan needs that can fail before matching begins.
enum Prepared {
    Ready {
        // Boxed: a compiled catalog dwarfs an `AttemptFailure`, and this enum
        // lives for exactly one match expression.
        compiled: Box<CompileOutput>,
        /// `None` when the vocabulary was refused: a producer with no consumer
        /// fills its channel and parks forever, so it is never started.
        producer: Option<tokio::task::JoinHandle<()>>,
        sections: SectionScanState,
    },
    Failed(AttemptFailure),
}

/// Compiles the catalog, reads the corpus's section vocabulary, decides whether
/// an exact projection fits, and — only if it does — starts the span producer.
///
/// The order is load-bearing. Availability is decided *before* the producer
/// exists, because a producer whose consumer was never built parks on a full
/// channel with nothing to drain it.
async fn prepare_scan(ready: &ReadyCorpus, source: &str) -> Prepared {
    let compiled = match compile_web_pattern(source) {
        Ok(output) => output,
        Err(error) => {
            return Prepared::Failed(AttemptFailure {
                category: ErrorCategory::Compile,
                message: format_pattern_compile_error(&error),
            });
        }
    };
    let pattern_count = crate::pattern_catalog::pattern_metadata_pairs(&compiled.matcher).len();

    let provenance = match ready.provenance().await {
        Ok(view) => view,
        Err(error) => {
            tracing::warn!(%error, "provenance view unavailable");
            return Prepared::Failed(integrity_failure());
        }
    };
    // A vocabulary past `MAX_SECTION_LABELS` is `SectionLabelCapacity` here, not
    // a truncated list, and that fails the whole scan rather than refusing the
    // projection the way the ceiling does. The difference is only in how it is
    // reported — 100_001 slots is 800_008 bytes of counters for a single
    // pattern, so such a corpus could never be admitted anyway — and the cap
    // is a defensive bound on what the reader will enumerate at all, which
    // lingrep-web treats as an unreadable sidecar like any other. Reporting it as a
    // refusal would mean naming a label count this path cannot know.
    let vocabulary = match provenance
        .section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
        .await
    {
        Ok(vocabulary) => vocabulary,
        Err(error) => {
            tracing::warn!(%error, "section coverage unavailable");
            return Prepared::Failed(integrity_failure());
        }
    };

    // `admit` runs the ceiling test and calls the closure only if it passes, so
    // the producer cannot be started for a vocabulary that has no consumer.
    let mut producer = None;
    let sections = match section_tally::admit(&vocabulary, pattern_count, || {
        // The producer maps label → slot from this same order, so the slot
        // assignment has one owner.
        let labels = vocabulary
            .iter()
            .map(|entry| entry.label.clone())
            .collect::<Vec<_>>();
        let (handle, events) = spawn_span_producer(provenance, labels);
        producer = Some(handle);
        events
    }) {
        Ok(builder) => SectionScanState::Building(Box::new(builder)),
        Err(refusal) => SectionScanState::Refused(refusal),
    };
    Prepared::Ready {
        compiled: Box::new(compiled),
        producer,
        sections,
    }
}

/// Runs the scan on a blocking thread.
///
/// Takes the already-compiled catalog: the caller needs its pattern count to
/// decide section availability before the span producer is started, and
/// compiling in both places would either double the work or admit sections
/// against a zero-pattern catalog.
#[cfg(test)]
fn run_scan_blocking(
    ready: &ReadyCorpus,
    output: CompileOutput,
    max_refs: usize,
    sections: SectionScanState,
    cancel: &CancellationFlag,
    progress_tx: watch::Sender<Option<ScanProgress>>,
) -> Result<(RunOutcome, SnapshotSink, Option<OversizedSentenceOmissions>), AttemptFailure> {
    run_scan_blocking_with_options(
        ready,
        output,
        max_refs,
        sections,
        cancel,
        progress_tx,
        false,
        CompoundCategory::Compound,
    )
}

fn run_scan_blocking_with_options(
    ready: &ReadyCorpus,
    output: CompileOutput,
    max_refs: usize,
    sections: SectionScanState,
    cancel: &CancellationFlag,
    progress_tx: watch::Sender<Option<ScanProgress>>,
    derive_compounds: bool,
    compound_category: CompoundCategory,
) -> Result<(RunOutcome, SnapshotSink, Option<OversizedSentenceOmissions>), AttemptFailure> {
    let patterns = crate::pattern_catalog::pattern_metadata_pairs(&output.matcher);
    let snapshot = SnapshotSink::new_with_compounds(
        max_refs,
        patterns,
        sections,
        derive_compounds,
        compound_category,
    )
    .map_err(|_| execution_failure())?;
    let mut progress = WatchProgress::new(progress_tx, LIVE_SUMMARY_INTERVAL);
    // `execute` applies publication policy; this surface only selects records.
    let mut sink = snapshot;
    let mut attributes = output.matcher.execution_attributes();
    // Snapshot/KWIC projection needs surface forms even when recognition does
    // not. Keep that output requirement explicit at the execution boundary.
    let text = AttributeKey::new("text").expect("form is a canonical attribute key");
    if !attributes.contains(&text) {
        attributes.push(text);
    }
    // Held rather than borrowed inline: it binds the catalog *and* tells the
    // executor which schema the artifact's stored tokens are in, which is the
    // one question a prepared run cannot answer from an analyzer session.
    let schema = ready.artifact.token_schema().map_err(|error| {
        tracing::warn!(%error, "artifact schema admission failed");
        integrity_failure()
    })?;
    let bound = output
        .matcher
        .bind_with_projection(&schema, attributes.iter().cloned())
        .map_err(|error| {
            tracing::warn!(%error, "catalog binding failed");
            integrity_failure()
        })?;
    // Prepared artifacts may contain oversized sentences. Omit and count them
    // so the result banner can disclose a degraded run.
    let omissions = Arc::new(StdMutex::new(OversizedSentenceOmissions::default()));
    let execution_source = ready
        .artifact
        .bound_sentence_source(bound.projection())
        .map_err(|error| {
            tracing::warn!(%error, "sentence source construction failed");
            integrity_failure()
        })?
        .with_tokenized_admission(TokenizedAdmission::Omit(Arc::clone(&omissions)));
    let mut observer = |_: lingrep_core::execution::TokenizedBatchObservation| Ok(());
    let outcome = execute_analyzed::<_, VibratoSession, _, _, _>(
        execution_source,
        AnalysisBinding::Prepared(schema),
        &bound,
        &ready.pool,
        &ExecutionLimits::default(),
        DEFAULT_BATCH_SIZE,
        cancel,
        &mut sink,
        &mut progress,
        &mut observer,
        PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .map_err(|error| classify_execution_error(&error))?;
    // Finalization runs here, on the blocking thread, because the span source
    // drains its channel with `blocking_recv`, which panics on a runtime
    // worker. Either way the builder — and with it the channel receiver — is
    // dropped before the caller joins the producer.
    if matches!(outcome, RunOutcome::Completed(_)) {
        sink.finish().map_err(|error| {
            tracing::warn!(%error, "span stream did not drain cleanly");
            integrity_failure()
        })?;
    } else {
        sink.discard_sections();
    }
    let oversized_omissions = omissions.lock().map_err(|_| execution_failure())?.summary();
    Ok((outcome, sink, oversized_omissions))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotLookupError {
    #[error("workbench not found")]
    UnknownWorkbench,
    #[error("snapshot revision is no longer displayed")]
    StaleSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorView {
    pub state: RevisionCoordinatorState,
    pub progress: ExecutionProgress,
    /// Provisional exact totals for the current revision's scanned prefix.
    ///
    /// This is deliberately separate from the immutable completed snapshot:
    /// partial scans cannot back detail or export requests.
    pub live_summary: Option<LiveSummary>,
}

/// One pattern's cumulative total over the prefix scanned so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePatternTotal {
    pub name: Arc<str>,
    pub total: u64,
}

/// A revision-bound, provisional summary of the corpus prefix scanned so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSummary {
    pub revision: RevisionId,
    pub rows: Arc<[LivePatternTotal]>,
}

#[derive(Debug, Clone)]
struct ScanProgress {
    progress: ExecutionProgress,
    rows: Arc<[LivePatternTotal]>,
}

struct Entry {
    /// The corpus this workbench was admitted under. Fixed at admission — see
    /// `admit`'s `CorpusConflict` check — so a workbench's revisions can never
    /// straddle two corpora.
    corpus: CorpusName,
    runtime: Arc<CorpusRuntime>,
    state: Mutex<RevisionCoordinatorState>,
    cancellation: StdMutex<Option<CancellationFlag>>,
    view_tx: watch::Sender<CoordinatorView>,
    handle: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    last_touch: AtomicU64,
}

fn new_entry(workbench: &WorkbenchId, slot: &CorpusSlot, touch: u64) -> Entry {
    let state = RevisionCoordinatorState::new(workbench.clone());
    let (view_tx, _) = watch::channel(CoordinatorView {
        state: state.clone(),
        progress: ExecutionProgress::default(),
        live_summary: None,
    });
    Entry {
        corpus: slot.name.clone(),
        runtime: Arc::clone(&slot.runtime),
        state: Mutex::new(state),
        cancellation: StdMutex::new(None),
        view_tx,
        handle: StdMutex::new(None),
        last_touch: AtomicU64::new(touch),
    }
}

struct RegistryInner {
    entries: Mutex<HashMap<WorkbenchId, Arc<Entry>>>,
    permit: Arc<Semaphore>,
    max_refs: usize,
    max_entries: NonZeroUsize,
    tick: AtomicU64,
}

/// Why an admission was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitError {
    /// Every entry is active, subscribed, or unfinished, and the id is new.
    RegistryFull,
    /// The workbench exists and is bound to a different corpus. A workbench's
    /// corpus is fixed at admission, so honouring either corpus would let one
    /// workbench's revisions straddle two.
    CorpusConflict,
}

// `.unwrap()` in const position needs Rust 1.83+; this toolchain is Rust 2024.
const DEFAULT_MAX_ENTRIES: NonZeroUsize = NonZeroUsize::new(256).unwrap();
const DEFAULT_SCAN_CONCURRENCY: NonZeroUsize = NonZeroUsize::new(1).unwrap();

#[derive(Clone)]
pub struct CoordinatorRegistry {
    inner: Arc<RegistryInner>,
}

impl std::fmt::Debug for CoordinatorRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordinatorRegistry")
            .finish_non_exhaustive()
    }
}

impl CoordinatorRegistry {
    pub fn new(max_refs: usize) -> Self {
        Self::with_max_entries(max_refs, DEFAULT_MAX_ENTRIES)
    }

    pub fn with_max_entries(max_refs: usize, max_entries: NonZeroUsize) -> Self {
        Self::with_scan_concurrency(max_refs, max_entries, DEFAULT_SCAN_CONCURRENCY)
    }

    pub fn with_scan_concurrency(
        max_refs: usize,
        max_entries: NonZeroUsize,
        scan_concurrency: NonZeroUsize,
    ) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                entries: Mutex::new(HashMap::new()),
                permit: Arc::new(Semaphore::new(scan_concurrency.get())),
                max_refs,
                max_entries,
                tick: AtomicU64::new(0),
            }),
        }
    }

    #[cfg(test)]
    fn available_scan_permits(&self) -> usize {
        self.inner.permit.available_permits()
    }

    fn next_touch(&self) -> u64 {
        self.inner.tick.fetch_add(1, Ordering::Relaxed)
    }

    /// Test-only convenience over `admit`: same corpus-conflict enforcement,
    /// panicking instead of returning `Result` so callers that only care about
    /// getting an entry don't have to unwrap at every call site. Delegating
    /// (rather than reimplementing the insert) means a test that readmits a
    /// workbench under a different corpus panics here exactly as `admit` would,
    /// instead of silently keeping the first entry's corpus.
    #[cfg(test)]
    async fn entry(&self, workbench: &WorkbenchId, slot: &CorpusSlot) -> Arc<Entry> {
        self.admit(workbench, slot).await.expect(
            "test fixture admitted a workbench under a conflicting corpus or a full registry",
        )
    }

    /// Admit a coordinator entry for `workbench`, evicting the least-recently
    /// touched *evictable* entry (terminal, unsubscribed, and finished) when the
    /// registry is at capacity. An existing id under the same corpus is always
    /// readmitted (and touched); an existing id under a different corpus is
    /// rejected with `AdmitError::CorpusConflict` — a workbench's corpus is fixed
    /// at admission. A new id is rejected with `AdmitError::RegistryFull` when no
    /// entry is evictable.
    async fn admit(
        &self,
        workbench: &WorkbenchId,
        slot: &CorpusSlot,
    ) -> Result<Arc<Entry>, AdmitError> {
        let touch = self.next_touch();
        let mut entries = self.inner.entries.lock().await;
        if let Some(existing) = entries.get(workbench) {
            if existing.corpus != slot.name {
                return Err(AdmitError::CorpusConflict);
            }
            existing.last_touch.store(touch, Ordering::Relaxed);
            return Ok(Arc::clone(existing));
        }
        if entries.len() >= self.inner.max_entries.get() {
            // Least-recently-touched evictable entry: terminal + unsubscribed + finished.
            let mut victim: Option<(WorkbenchId, u64)> = None;
            for (id, entry) in entries.iter() {
                let terminal = entry.state.lock().await.is_terminal();
                let unsubscribed = entry.view_tx.receiver_count() == 0;
                let finished = entry
                    .handle
                    .lock()
                    .expect("handle mutex poisoned")
                    .as_ref()
                    .is_none_or(tokio::task::JoinHandle::is_finished);
                if terminal && unsubscribed && finished {
                    let last_touch = entry.last_touch.load(Ordering::Relaxed);
                    if victim.as_ref().is_none_or(|(_, best)| last_touch < *best) {
                        victim = Some((id.clone(), last_touch));
                    }
                }
            }
            match victim {
                Some((id, _)) => {
                    if let Some(entry) = entries.remove(&id) {
                        if let Some(cancel) = entry
                            .cancellation
                            .lock()
                            .expect("cancellation mutex poisoned")
                            .take()
                        {
                            cancel.cancel();
                        }
                        if let Some(handle) =
                            entry.handle.lock().expect("handle mutex poisoned").take()
                        {
                            handle.abort();
                        }
                    }
                }
                None => return Err(AdmitError::RegistryFull),
            }
        }
        let entry = Arc::new(new_entry(workbench, slot, touch));
        entries.insert(workbench.clone(), Arc::clone(&entry));
        Ok(entry)
    }

    pub async fn accept_next_revision(
        &self,
        workbench: WorkbenchId,
        slot: &CorpusSlot,
        source: String,
    ) -> RevisionId {
        self.try_accept_next_revision(workbench, slot, source, None)
            .await
            .unwrap_or_else(|_| {
                #[expect(
                    clippy::panic,
                    reason = "registry capacity is reserved ahead of admission"
                )]
                {
                    panic!("registry capacity available")
                }
            })
    }

    /// Accept a revision, retaining `requested` KWIC lines per pattern.
    ///
    /// `None` is the server's configured ceiling, which is also what a request
    /// asking for more than the ceiling gets: retention is a display
    /// preference, and a smaller concordance is a better answer than a refused
    /// search. The value is per revision rather than per registry because it is
    /// a property of what one reader asked for, not of how the server was
    /// started.
    pub async fn try_accept_next_revision(
        &self,
        workbench: WorkbenchId,
        slot: &CorpusSlot,
        source: String,
        requested: Option<usize>,
    ) -> Result<RevisionId, AdmitError> {
        self.try_accept_next_revision_with_compounds(
            workbench,
            slot,
            source,
            requested,
            false,
            CompoundCategory::Compound,
        )
        .await
    }

    pub(crate) async fn try_accept_next_revision_with_compounds(
        &self,
        workbench: WorkbenchId,
        slot: &CorpusSlot,
        source: String,
        requested: Option<usize>,
        derive_compounds: bool,
        compound_category: CompoundCategory,
    ) -> Result<RevisionId, AdmitError> {
        let retained = requested.map_or(self.inner.max_refs, |asked| {
            asked.clamp(1, self.inner.max_refs)
        });
        let options = ScanOptions {
            retained,
            derive_compounds,
            compound_category,
        };
        let entry = self.admit(&workbench, slot).await?;
        let readiness = entry.runtime.readiness().borrow().clone();
        let mut state = entry.state.lock().await;
        let next = state.current_attempt().map_or(1, |attempt| {
            attempt
                .revision
                .0
                .checked_add(1)
                .expect("revision id exhausted")
        });
        let revision = RevisionId(next);
        let accepted = state.accept(revision, source.as_str().into(), readiness_view(&readiness));
        assert!(accepted, "coordinator-allocated revision must be monotonic");
        let cancellation = CancellationFlag::new();
        if let Some(cancel) = entry
            .cancellation
            .lock()
            .expect("cancellation mutex poisoned")
            .replace(cancellation.clone())
        {
            cancel.cancel();
        }
        entry.publish(&state, ExecutionProgress::default());
        drop(state);

        let registry = self.clone();
        let entry_for_task = Arc::clone(&entry);
        let handle = tokio::spawn(async move {
            registry
                .run_revision(
                    entry_for_task,
                    workbench,
                    revision,
                    source,
                    options,
                    cancellation,
                )
                .await;
        });
        // The swapped cancellation flag already signalled the prior task to
        // stop; aborting its handle here reclaims it deterministically so
        // a superseded revision cannot linger and block eviction later.
        if let Some(previous) = entry
            .handle
            .lock()
            .expect("handle mutex poisoned")
            .replace(handle)
        {
            previous.abort();
        }
        Ok(revision)
    }

    /// The corpus the workbench was admitted against — its key and its runtime
    /// — or `None` if the workbench is unknown.
    ///
    /// The single authoritative answer to "which corpus does this request
    /// concern". Handlers must not consult the corpus set directly for a
    /// workbench-scoped request: the workbench, not the URL, decides.
    ///
    /// Both halves come back together because the binding is one fact and one
    /// `entries` acquisition: a runtime is what a request *reads*, while only
    /// the key resolves the `CorpusSet` slot that can *name* the corpus to a
    /// researcher. A caller wanting just the runtime uses
    /// [`CoordinatorRegistry::bound_corpus`].
    pub async fn bound_entry(
        &self,
        workbench: &WorkbenchId,
    ) -> Option<(CorpusName, Arc<CorpusRuntime>)> {
        self.inner
            .entries
            .lock()
            .await
            .get(workbench)
            .map(|entry| (entry.corpus.clone(), Arc::clone(&entry.runtime)))
    }

    /// The runtime half of [`CoordinatorRegistry::bound_entry`], for the
    /// callers that only read from the corpus and never name it.
    pub async fn bound_corpus(&self, workbench: &WorkbenchId) -> Option<Arc<CorpusRuntime>> {
        self.bound_entry(workbench)
            .await
            .map(|(_name, runtime)| runtime)
    }

    pub async fn snapshot(&self, workbench: &WorkbenchId) -> Option<CoordinatorView> {
        let touch = self.next_touch();
        let entry = self.inner.entries.lock().await.get(workbench).cloned()?;
        entry.last_touch.store(touch, Ordering::Relaxed);
        let view = entry.view_tx.borrow().clone();
        Some(view)
    }

    pub async fn displayed_snapshot(
        &self,
        workbench: &WorkbenchId,
        expected: RevisionId,
    ) -> Result<Arc<CompletedSnapshot>, SnapshotLookupError> {
        let touch = self.next_touch();
        let entry = self
            .inner
            .entries
            .lock()
            .await
            .get(workbench)
            .cloned()
            .ok_or(SnapshotLookupError::UnknownWorkbench)?;
        entry.last_touch.store(touch, Ordering::Relaxed);
        let state = entry.state.lock().await;
        let snapshot = state
            .displayed_snapshot_arc()
            .ok_or(SnapshotLookupError::StaleSnapshot)?;
        (snapshot.revision == expected)
            .then(|| Arc::clone(snapshot))
            .ok_or(SnapshotLookupError::StaleSnapshot)
    }

    /// Acquire the shared scan permit. The export path holds this INSIDE its
    /// blocking task so it is released when the scan actually ends — not when the
    /// request future drops — and so exports never oversubscribe the Rayon pool.
    pub async fn acquire_scan_permit(&self) -> tokio::sync::OwnedSemaphorePermit {
        std::sync::Arc::clone(&self.inner.permit)
            .acquire_owned()
            .await
            .expect("scan permit semaphore is never closed")
    }

    pub async fn subscribe(
        &self,
        workbench: &WorkbenchId,
    ) -> Option<watch::Receiver<CoordinatorView>> {
        let touch = self.next_touch();
        self.inner.entries.lock().await.get(workbench).map(|entry| {
            entry.last_touch.store(touch, Ordering::Relaxed);
            entry.view_tx.subscribe()
        })
    }

    async fn run_revision(
        &self,
        entry: Arc<Entry>,
        workbench: WorkbenchId,
        revision: RevisionId,
        source: String,
        options: ScanOptions,
        cancel: CancellationFlag,
    ) {
        let Some(ready) = self
            .wait_for_ready(&entry, &entry.runtime, &workbench, revision)
            .await
        else {
            return;
        };
        {
            let mut state = entry.state.lock().await;
            if !state.mark_queued(revision) {
                return;
            }
            entry.publish(&state, ExecutionProgress::default());
        }
        let Ok(_permit) = Arc::clone(&self.inner.permit).acquire_owned().await else {
            return;
        };
        {
            let mut state = entry.state.lock().await;
            if !state.mark_running(revision) {
                return;
            }
            entry.publish(&state, ExecutionProgress::default());
        }
        if cancel.is_cancelled() {
            return;
        }
        let (progress_tx, _) = watch::channel(None::<ScanProgress>);
        let forward_entry = Arc::clone(&entry);
        let mut progress_rx = progress_tx.subscribe();
        let forward = tokio::spawn(async move {
            while progress_rx.changed().await.is_ok() {
                let Some(update) = progress_rx.borrow_and_update().clone() else {
                    continue;
                };
                let state = forward_entry.state.lock().await;
                forward_entry.publish_live(
                    &state,
                    update.progress,
                    LiveSummary {
                        revision,
                        rows: update.rows,
                    },
                );
            }
        });
        // What this revision was accepted with, not the registry's ceiling:
        // the reader chose it and the clamp already happened at admission.
        let max_refs = options.retained;
        let identity = ready.identity.clone();
        let expected_sentence_count = ready.sentence_count().ok();
        let source: std::sync::Arc<str> = source.into();

        // Everything that can fail before the scan starts, with one exit.
        // Nothing here may degrade to an empty vocabulary: publishing an
        // exact-looking all-`None` distribution because a read failed is the
        // one outcome this feature must not have.
        let prepared = prepare_scan(&ready, &source).await;

        let scan = match prepared {
            // No producer was started on this path, so there is nothing to join.
            Prepared::Failed(failure) => {
                forward.abort();
                ScanTaskResult::Finished(Err(failure))
            }
            Prepared::Ready {
                compiled,
                producer,
                sections,
            } => {
                let result = tokio::task::spawn_blocking(move || {
                    run_scan_blocking_with_options(
                        &ready,
                        *compiled,
                        max_refs,
                        sections,
                        &cancel,
                        progress_tx,
                        options.derive_compounds,
                        options.compound_category,
                    )
                })
                .await;
                forward.abort();

                // `run_scan_blocking` owns the sink on every path, so the
                // channel receiver is gone by the time it returns or unwinds —
                // a producer parked on a full channel is already released.
                // Dropping a `JoinHandle` detaches rather than joins, so the
                // await is not optional.
                if let Some(handle) = producer {
                    let _ = handle.await;
                }
                match result {
                    Ok(inner) => ScanTaskResult::Finished(inner),
                    Err(_) => ScanTaskResult::Panicked,
                }
            }
        };
        let resolution = resolve_scan(
            scan,
            workbench,
            revision,
            identity,
            expected_sentence_count,
            source,
        );
        if resolution.invalidate_corrupt {
            entry.runtime.invalidator().invalidate_corrupt().await;
        }
        let terminal_progress = resolution.progress;
        let mut state = entry.state.lock().await;
        if state.apply_terminal(resolution.event) {
            entry.publish(&state, terminal_progress);
        }
    }

    async fn wait_for_ready(
        &self,
        entry: &Entry,
        runtime: &CorpusRuntime,
        workbench: &WorkbenchId,
        revision: RevisionId,
    ) -> Option<ReadyCorpus> {
        let mut readiness = runtime.readiness();
        loop {
            if entry
                .state
                .lock()
                .await
                .current_attempt()
                .is_none_or(|attempt| attempt.revision != revision)
            {
                return None;
            }
            if let Some(ready) = runtime.ready().await {
                return Some(ready);
            }
            if matches!(*readiness.borrow(), CacheReadiness::Failed { .. }) {
                let mut state = entry.state.lock().await;
                let event = CoordinatorEvent::Failed {
                    workbench: workbench.clone(),
                    revision,
                    failure: cache_unavailable_failure(),
                };
                if state.apply_terminal(event) {
                    entry.publish(&state, ExecutionProgress::default());
                }
                return None;
            }
            if readiness.changed().await.is_err() {
                return None;
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut handles = Vec::new();
        for entry in self.inner.entries.lock().await.values() {
            if let Some(cancel) = entry
                .cancellation
                .lock()
                .expect("cancellation mutex poisoned")
                .as_ref()
            {
                cancel.cancel();
            }
            if let Some(handle) = entry.handle.lock().expect("handle mutex poisoned").take() {
                handles.push(handle);
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            for handle in handles {
                let _ = handle.await;
            }
        })
        .await;
        // Rayon work already running cannot be forcibly terminated. After the
        // deadline, identity checks still prevent any stale publication.
    }
}

fn readiness_view(readiness: &CacheReadiness) -> CacheReadinessView {
    match readiness {
        CacheReadiness::Ready { .. } => CacheReadinessView::Ready,
        CacheReadiness::Checking | CacheReadiness::Building { .. } => CacheReadinessView::Building,
        CacheReadiness::Failed { .. } => CacheReadinessView::Failed,
    }
}

impl Entry {
    /// Publish the current view to the watch channel.
    ///
    /// F6 invariant: every caller invokes this **while holding `self.state`'s
    /// lock**. The `state` mutex serializes all senders, so a terminal publish
    /// (issued after `forward.abort()` under the lock) is guaranteed to land
    /// after any in-flight progress publish — "stale progress overwrites
    /// Completed" cannot occur. Do not make this async and do not call it
    /// outside the `state` lock, or that ordering guarantee is lost.
    fn publish(&self, state: &RevisionCoordinatorState, progress: ExecutionProgress) {
        self.view_tx.send_replace(CoordinatorView {
            state: state.clone(),
            progress,
            live_summary: None,
        });
    }

    fn publish_live(
        &self,
        state: &RevisionCoordinatorState,
        progress: ExecutionProgress,
        live_summary: LiveSummary,
    ) {
        self.view_tx.send_replace(CoordinatorView {
            state: state.clone(),
            progress,
            live_summary: Some(live_summary),
        });
    }
}

struct WatchProgress {
    sender: watch::Sender<Option<ScanProgress>>,
    interval: Duration,
    last_publish: Option<Instant>,
}

impl WatchProgress {
    fn new(sender: watch::Sender<Option<ScanProgress>>, interval: Duration) -> Self {
        Self {
            sender,
            interval,
            last_publish: None,
        }
    }
}

impl ProgressSink<SnapshotSink> for WatchProgress {
    fn update(&mut self, progress: ExecutionProgress, sink: &SnapshotSink) {
        let now = Instant::now();
        if self
            .last_publish
            .is_some_and(|last| now.duration_since(last) < self.interval)
        {
            return;
        }
        self.last_publish = Some(now);
        let rows = sink
            .patterns
            .iter()
            .map(|pattern| LivePatternTotal {
                name: Arc::<str>::from(pattern.name.as_str()),
                total: pattern.exact_total,
            })
            .collect::<Vec<_>>()
            .into();
        self.sender
            .send_replace(Some(ScanProgress { progress, rows }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revision::AttemptPhase;
    use crate::revision_status::project_revision_status;
    use crate::snapshot::{MatchRef, PatternDescriptor, PatternKey};
    use lingrep_core::execution::{MatchSink, SentenceBatchSource, execute};
    use lingrep_core::token_match::TokenInfo;

    /// A minimal `CorpusSlot` for tests that need one only to admit an entry,
    /// not to exercise a particular corpus's content.
    fn test_slot() -> CorpusSlot {
        let context = Arc::new(crate::server_context::ServerContext::new("/tmp/dict").unwrap());
        let runtime = Arc::new(CorpusRuntime::new(context, "unused-corpus").unwrap());
        CorpusSlot::new(CorpusName::default_key(), runtime, "unused-corpus".into())
    }

    #[test]
    fn execution_failure_is_a_generic_execution_error() {
        assert_eq!(
            super::execution_failure(),
            AttemptFailure {
                category: ErrorCategory::Execution,
                message: "The corpus scan could not be completed.".into(),
            }
        );
    }

    #[test]
    fn cache_unavailable_failure_is_a_cache_error() {
        assert_eq!(
            super::cache_unavailable_failure(),
            AttemptFailure {
                category: ErrorCategory::Cache,
                message: "The corpus cache is unavailable.".into(),
            }
        );
    }

    #[test]
    fn corrupt_cache_decode_classifies_as_integrity() {
        use lingrep_core::execution::ExecutionError;
        use std::io;
        assert_eq!(
            classify_execution_error(&ExecutionError::Cache(io::Error::other("bad decode"))),
            integrity_failure()
        );
        assert_eq!(integrity_failure().category, ErrorCategory::Integrity);
    }

    #[test]
    fn a_sink_error_stays_a_generic_execution_failure() {
        use lingrep_core::execution::ExecutionError;
        assert_eq!(
            classify_execution_error(&ExecutionError::Sink("boom".into())),
            execution_failure()
        );
        assert_eq!(
            classify_execution_error(&ExecutionError::InvalidLimit {
                field: "logical_batch_sentences",
                message: "must be greater than zero".into(),
            }),
            execution_failure()
        );
    }

    #[test]
    fn entry_publish_forwards_state_and_progress_verbatim() {
        let workbench = WorkbenchId::new("wb");
        let entry = new_entry(&workbench, &test_slot(), 0);
        let mut rx = entry.view_tx.subscribe();
        let mut state = RevisionCoordinatorState::new(workbench.clone());
        assert!(state.accept(
            RevisionId(1),
            "p := [upos=NOUN].".into(),
            CacheReadinessView::Ready
        ));
        let progress = ExecutionProgress {
            sentences_admitted: 4,
            sentences_matched: 4,
            tokens_matched: 9,
            matches_found: 1,
            records_emitted: 1,
        };

        entry.publish(&state, progress);

        let view = rx.borrow_and_update().clone();
        assert_eq!(view.state, state);
        assert_eq!(view.progress, progress);
        assert!(view.live_summary.is_none());
    }

    fn tiny_ready_corpus(verb_sentences: usize) -> (tempfile::TempDir, ReadyCorpus) {
        use lingrep_core::token_match::TokenInfo;
        let sentence = vec![TokenInfo::default().with_orth("食べる").with_pos("VERB")];
        crate::corpus_runtime::test_ready_corpus(vec![sentence; verb_sentences])
    }

    #[test]
    fn run_scan_blocking_completes_and_counts_every_match() {
        let (_dir, ready) = tiny_ready_corpus(3);
        let (progress_tx, mut progress_rx) = watch::channel(None::<ScanProgress>);
        let cancel = CancellationFlag::new();
        let (outcome, sink, omissions) = run_scan_blocking(
            &ready,
            crate::patterns::compile_web_pattern("verb := [upos=VERB].").unwrap(),
            5,
            no_sections(),
            &cancel,
            progress_tx,
        )
        .unwrap();
        assert!(omissions.is_none());
        assert!(matches!(outcome, RunOutcome::Completed(_)));
        assert_eq!(sink.patterns[0].name, "verb");
        assert_eq!(sink.patterns[0].exact_total, 3);
        let live = progress_rx
            .borrow_and_update()
            .clone()
            .expect("the completed logical batch publishes a live prefix");
        assert_eq!(
            live.rows.as_ref(),
            [LivePatternTotal {
                name: Arc::from("verb"),
                total: 3,
            }]
        );
        assert_eq!(live.progress.records_emitted, 3);
    }

    #[test]
    fn scan_options_survive_into_the_published_snapshot() {
        let (_dir, ready) = tiny_ready_corpus(3);
        let output =
            crate::patterns::compile_web_pattern("category: \"S1A\"\na := [upos=VERB].").unwrap();
        let (progress_tx, _) = watch::channel(None::<ScanProgress>);
        let (outcome, sink, omissions) = run_scan_blocking_with_options(
            &ready,
            output,
            5,
            no_sections(),
            &CancellationFlag::new(),
            progress_tx,
            true,
            CompoundCategory::SourceCategories,
        )
        .unwrap();
        assert!(omissions.is_none());
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((outcome, sink, omissions))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            ready.identity.clone(),
            None,
            std::sync::Arc::from("category: \"S1A\"\na := [upos=VERB]."),
        );
        let CoordinatorEvent::Completed { snapshot, .. } = resolution.event else {
            panic!("completed options should publish");
        };
        assert!(snapshot.derive_compounds);
        assert_eq!(
            snapshot.compound_category,
            CompoundCategory::SourceCategories
        );
    }

    #[test]
    fn derived_compounds_do_not_fail_the_scan_that_derives_them() {
        use lingrep_core::token_match::TokenInfo;
        let (_dir, ready) = crate::corpus_runtime::test_ready_corpus(vec![vec![
            TokenInfo::default().with_orth("甲").with_pos("NOUN"),
            TokenInfo::default().with_orth("乙").with_pos("NOUN"),
        ]]);
        let (progress_tx, _) = watch::channel(None::<ScanProgress>);
        let (outcome, sink, omissions) = run_scan_blocking_with_options(
            &ready,
            crate::patterns::compile_web_pattern("w := [].").unwrap(),
            5,
            no_sections(),
            &CancellationFlag::new(),
            progress_tx,
            true,
            CompoundCategory::Compound,
        )
        .expect("a derived chain is published output, not an over-count");
        assert!(matches!(outcome, RunOutcome::Completed(_)));
        assert!(omissions.is_none());
        assert_eq!(
            sink.patterns.len(),
            2,
            "one authored pattern and one derived chain"
        );
        assert_eq!(sink.patterns[0].exact_total, 2);
        assert_eq!(sink.patterns[1].exact_total, 1);
    }

    #[test]
    fn live_progress_is_cumulative_and_rate_limited_before_publication() {
        let (progress_tx, mut progress_rx) = watch::channel(None::<ScanProgress>);
        let mut progress = WatchProgress::new(progress_tx, Duration::MAX);
        let mut sink =
            SnapshotSink::new(0, vec![("p".into(), Box::default())], no_sections()).unwrap();
        sink.patterns[0].exact_total = 1;

        progress.update(ExecutionProgress::default(), &sink);
        sink.patterns[0].exact_total = 9;
        progress.update(
            ExecutionProgress {
                matches_found: 9,
                records_emitted: 9,
                ..ExecutionProgress::default()
            },
            &sink,
        );

        let first = progress_rx
            .borrow_and_update()
            .clone()
            .expect("first prefix snapshot");
        assert_eq!(first.rows[0].total, 1);

        progress.interval = Duration::ZERO;
        progress.update(
            ExecutionProgress {
                matches_found: 9,
                records_emitted: 9,
                ..ExecutionProgress::default()
            },
            &sink,
        );
        let latest = progress_rx
            .borrow_and_update()
            .clone()
            .expect("replacement prefix snapshot");
        assert_eq!(latest.rows[0].total, 9);
        assert_eq!(latest.progress.records_emitted, 9);
    }

    #[test]
    fn workbench_projection_enforces_projected_residency() {
        let token = TokenInfo::default().with_lemma("x".repeat(600_000));
        let (_dir, ready) = crate::corpus_runtime::test_ready_corpus(vec![vec![token; 64]]);
        let compiled = crate::patterns::compile_web_pattern("p := ^ []+ $.").unwrap();
        assert!(compiled.matcher.execution_attributes().is_empty());

        let (progress_tx, _rx) = watch::channel(None::<ScanProgress>);
        let (outcome, sink, omissions) = run_scan_blocking(
            &ready,
            crate::patterns::compile_web_pattern("p := ^ []+ $.").unwrap(),
            5,
            no_sections(),
            &CancellationFlag::new(),
            progress_tx,
        )
        .expect("zero-column projection fits below the resident ceiling");

        assert!(matches!(outcome, RunOutcome::Completed(_)));
        assert_eq!(sink.observed_sentences, 1);
        assert_eq!(sink.patterns[0].exact_total, 1);
        assert!(omissions.is_none());

        let lemma_source = "p := ^ [lemma~x]+ $.";
        let lemma = crate::patterns::compile_web_pattern(lemma_source).unwrap();
        assert_eq!(
            lemma.matcher.execution_attributes(),
            [AttributeKey::new("lemma").unwrap()]
        );
        let bound = lemma
            .matcher
            .bind_with_projection(
                &ready.artifact.token_schema().unwrap(),
                lemma.matcher.execution_attributes(),
            )
            .unwrap();
        let mut projected = ready
            .artifact
            .bound_sentence_source(bound.projection())
            .unwrap();
        let error = projected
            .next_batch(&ExecutionLimits::default())
            .expect_err("the demanded lemma still exceeds the resident ceiling");
        let ExecutionError::TokenizedSentenceTooLarge { actual, limit, .. } = error else {
            panic!("expected TokenizedSentenceTooLarge, got {error:?}");
        };
        assert!(actual > limit);
    }

    /// A corpus whose second sentence cannot be materialized under the
    /// default resident bound must not fail the run: the sentence is omitted
    /// and the completed scan carries the omission summary for the banner.
    #[test]
    fn run_scan_blocking_omits_oversized_sentences_and_reports_them() {
        let token = TokenInfo::default().with_lemma("x".repeat(600_000));
        let small = TokenInfo::default()
            .with_orth("食べる")
            .with_pos("VERB")
            .with_lemma("食べる");
        // 64 x 600k chars > 32 MiB resident bound.
        let sentences = vec![
            vec![small; 2],
            vec![token; 64],
            vec![TokenInfo::default(); 1],
        ];
        let (_dir, ready) = crate::corpus_runtime::test_ready_corpus(sentences);
        let (progress_tx, _rx) = watch::channel(None::<ScanProgress>);
        // Demand the fat attribute: an empty projection would never
        // materialize it, so the resident bound would never trip (projection-
        // based admission, per workbench_projection_enforces_projected_residency).
        let (outcome, sink, omissions) = run_scan_blocking(
            // Match any nonempty lemma. The third (default) sentence has no
            // lemma at all, so only the two small sentences can match.
            &ready,
            crate::patterns::compile_web_pattern("p := ^ [lemma~/.+/]+ $.").unwrap(),
            5,
            no_sections(),
            &CancellationFlag::new(),
            progress_tx,
        )
        .expect("the oversized sentence is omitted, not fatal");
        assert!(matches!(outcome, RunOutcome::Completed(_)));
        // Only sentence 0 matches (both its tokens carry the demanded lemma);
        // sentence 1 is omitted for residency, sentence 2 has no lemma.
        assert_eq!(sink.patterns[0].exact_total, 1);
        let omissions = omissions.expect("completion reports the omissions");
        assert_eq!(omissions.count, 1);
        assert_eq!(omissions.first_ordinal, 1);
        assert!(omissions.first_actual > omissions.limit);
        assert!(omissions.first_source_name.is_some());
    }

    /// The completed status projects the omission summary into a banner the
    /// user sees with the results.
    #[test]
    fn completed_status_carries_the_omission_banner() {
        let workbench = WorkbenchId::new("wb");
        let mut state = RevisionCoordinatorState::new(workbench.clone());
        assert!(state.accept(
            RevisionId(1),
            "p := [upos=NOUN].".into(),
            CacheReadinessView::Ready
        ));
        let mut snapshot = (*snapshot(&workbench, RevisionId(1))).clone();
        snapshot.oversized_omissions = Some(OversizedSentenceOmissions {
            count: 1,
            first_ordinal: 338_066,
            first_source_name: Some(std::sync::Arc::from("00027.md")),
            first_actual: 36_933_888,
            limit: 33_554_432,
        });
        assert!(state.apply_terminal(CoordinatorEvent::Completed {
            workbench,
            revision: RevisionId(1),
            snapshot: Arc::new(snapshot),
        }));
        let status = project_revision_status(Some(&CoordinatorView {
            state,
            progress: ExecutionProgress::default(),
            live_summary: None,
        }));
        let notice = status.omission_notice.expect("banner text present");
        assert!(notice.contains("1 oversized sentence"), "{notice}");
        assert!(
            notice.contains("338066") || notice.contains("338,066"),
            "{notice}"
        );
    }

    /// A completed count check must not invalidate the run for omitted
    /// sentences: matched + omitted equals the artifact's sentence total.
    #[test]
    fn resolve_scan_counts_omitted_sentences_toward_completion_integrity() {
        let sink = completed_sink(6);
        let report = lingrep_core::execution::ExecutionReport {
            sentences_matched: 6,
            ..Default::default()
        };
        let omissions = OversizedSentenceOmissions {
            count: 1,
            first_ordinal: 4,
            first_source_name: None,
            first_actual: 36_933_888,
            limit: 33_554_432,
        };
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((RunOutcome::Completed(report), sink, Some(omissions)))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            Some(7),
            std::sync::Arc::from(""),
        );
        assert!(
            matches!(resolution.event, CoordinatorEvent::Completed { .. }),
            "{:#?}",
            resolution.event
        );
        assert!(!resolution.invalidate_corrupt);

        // Without the omission allowance, the same totals still hard-fail.
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(lingrep_core::execution::ExecutionReport {
                    sentences_matched: 6,
                    ..Default::default()
                }),
                completed_sink(6),
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            Some(7),
            std::sync::Arc::from(""),
        );
        assert!(matches!(resolution.event, CoordinatorEvent::Failed { .. }));
        assert!(resolution.invalidate_corrupt);
    }

    /// A section state over an empty vocabulary: always admitted, so the sweep
    /// runs and every sentence lands in the `None` slot. Used where the test is
    /// about something other than sections but the gates still apply.
    fn no_sections() -> SectionScanState {
        SectionScanState::Building(Box::new(
            crate::section_tally::build(
                &[],
                1,
                crate::section_tally::SpanEvents::from_events(vec![]),
            )
            .expect("an empty vocabulary always fits"),
        ))
    }

    fn test_location(ordinal: u64) -> lingrep_core::execution::SentenceLocation {
        lingrep_core::execution::SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: ordinal,
            document_id: lingrep_core::corpus::DocumentId::new(0),
            document_ordinal: 0,
            document_sentence_ordinal: ordinal,
            source_name: "".into(),
        }
    }

    /// A sink that observed `sentences` published sentences and was finalized,
    /// so only the property under test can fail a gate.
    fn test_sink(sentences: u64) -> SnapshotSink {
        let mut sink =
            SnapshotSink::new(5, vec![("p".to_string(), Box::default())], no_sections()).unwrap();
        for ordinal in 0..sentences {
            sink.push(&test_location(ordinal), &(&[] as &[TokenInfo]), &[])
                .unwrap();
        }
        sink
    }

    fn test_sink_without_sections(sentences: u64) -> SnapshotSink {
        let mut sink = SnapshotSink::new(
            5,
            vec![("p".to_string(), Box::default())],
            SectionScanState::Refused(SectionRefusal {
                labels: 1_000_000,
                patterns: 1,
            }),
        )
        .unwrap();
        for ordinal in 0..sentences {
            sink.push(&test_location(ordinal), &(&[] as &[TokenInfo]), &[])
                .unwrap();
        }
        sink
    }

    fn completion_report(sentences: u64) -> lingrep_core::execution::ExecutionReport {
        lingrep_core::execution::ExecutionReport {
            sentences_matched: sentences,
            ..Default::default()
        }
    }

    /// The control for the gate tests below: same shape, agreeing counts, so a
    /// `Failed` elsewhere can only come from the disagreement under test.
    #[test]
    fn a_finalized_sink_whose_count_agrees_publishes() {
        let mut sink = test_sink(1);
        sink.finish().unwrap();
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(completion_report(1)),
                sink,
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            None,
            std::sync::Arc::from("p := [upos=VERB]."),
        );
        assert!(matches!(
            resolution.event,
            CoordinatorEvent::Completed { .. }
        ));
    }

    #[test]
    fn a_sentence_count_disagreement_fails_publication() {
        let mut sink = test_sink(1);
        // Finalize first. Without this the `finalized_sections.is_none()` guard
        // would fail the scan for the wrong reason, and deleting the
        // sentence-count guard would leave this test green.
        sink.finish().unwrap();
        assert!(sink.finalized_sections.is_some());
        // The sink saw one sentence; the report claims two.
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(completion_report(2)),
                sink,
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            None,
            std::sync::Arc::from("p := [upos=VERB]."),
        );
        assert!(
            matches!(resolution.event, CoordinatorEvent::Failed { .. }),
            "the gate holds without expected_sentence_count"
        );
    }

    #[test]
    fn the_gate_holds_when_sections_are_unavailable() {
        let mut sink = test_sink_without_sections(1);
        sink.finish().unwrap();
        assert!(sink.finalized_sections.is_some());
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(completion_report(7)),
                sink,
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            None,
            std::sync::Arc::from("p := [upos=VERB]."),
        );
        assert!(matches!(resolution.event, CoordinatorEvent::Failed { .. }));
    }

    #[test]
    fn an_unfinalized_sink_never_publishes() {
        // The lifecycle guard in its own test, so it is not doing the
        // sentence-count guard's work in the tests above.
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(completion_report(1)),
                test_sink(1),
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            None,
            std::sync::Arc::from("p := [upos=VERB]."),
        );
        assert!(matches!(resolution.event, CoordinatorEvent::Failed { .. }));
    }

    /// Sections reach the published snapshot, so the panel reads exact counts
    /// rather than recomputing them from the retained refs.
    #[test]
    fn a_published_snapshot_carries_its_section_tally() {
        let mut sink = test_sink(2);
        sink.finish().unwrap();
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(completion_report(2)),
                sink,
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            None,
            std::sync::Arc::from("p := [upos=VERB]."),
        );
        let CoordinatorEvent::Completed { snapshot, .. } = resolution.event else {
            panic!("expected Completed");
        };
        assert_eq!(snapshot.observed_sentences, 2);
        assert_eq!(snapshot.sentences_matched, 2);
        let SectionAvailability::Available(tally) = &snapshot.sections else {
            panic!("an empty vocabulary is admitted");
        };
        // One slot, `None`, holding both published sentences.
        assert_eq!(tally.slots().len(), 1);
        assert_eq!(tally.universe(0), 2);
    }

    fn run_full_scan_for_test(ready: &ReadyCorpus, source: &str) -> (RunOutcome, SnapshotSink) {
        let output = crate::patterns::compile_web_pattern(source).unwrap();
        let patterns = crate::pattern_catalog::pattern_metadata_pairs(&output.matcher);
        let mut sink = SnapshotSink::new(10, patterns, no_sections()).unwrap();
        let execution_source = ready.artifact.sentence_source().unwrap();
        let mut progress = |_: ExecutionProgress, _: &SnapshotSink| {};
        let mut observer = |_: lingrep_core::execution::TokenizedBatchObservation| Ok(());
        let outcome = execute(
            execution_source,
            |_| {
                #[expect(
                    clippy::unreachable,
                    reason = "prepared fixture sentences are already tokenized"
                )]
                {
                    unreachable!("fixture is already tokenized")
                }
            },
            &output.matcher,
            &ready.pool,
            &ExecutionLimits::default(),
            DEFAULT_BATCH_SIZE,
            &CancellationFlag::new(),
            &mut sink,
            &mut progress,
            &mut observer,
            PolicySelection::Apply,
            &SentenceAdmission::All,
        )
        .unwrap();
        (outcome, sink)
    }

    #[test]
    fn workbench_projection_matches_full_source_including_policy_stats() {
        let sentence = vec![
            TokenInfo::default()
                .with_orth("x")
                .with_norm("normalized-x"),
        ];
        let (_dir, ready) = crate::corpus_runtime::test_ready_corpus(vec![sentence]);
        let source = "key: [$item.norm]\npattern p := (?<item> [text=x]).";

        let (progress_tx, _rx) = watch::channel(None::<ScanProgress>);
        let projected = run_scan_blocking(
            &ready,
            crate::patterns::compile_web_pattern(source).unwrap(),
            10,
            no_sections(),
            &CancellationFlag::new(),
            progress_tx,
        )
        .unwrap();
        let full = run_full_scan_for_test(&ready, source);

        // RunOutcome equality includes policy_stats.records_unkeyed. Do not narrow
        // this comparison: it makes the policy-only Norm demand observable.
        assert_eq!(projected.0, full.0);
        assert_eq!(projected.1.refs, full.1.refs);
        assert_eq!(projected.1.patterns, full.1.patterns);
        assert_eq!(projected.1.capture_catalog, full.1.capture_catalog);
        let RunOutcome::Completed(report) = projected.0 else {
            panic!("fixture must complete");
        };
        assert_eq!(report.policy_stats.records_unkeyed, 0);
    }

    /// Compilation moved out of the blocking scan and into `prepare_scan`,
    /// because admission needs the pattern count before a producer may start.
    /// The user-facing mapping is unchanged and is asserted where it now lives.
    #[tokio::test(flavor = "multi_thread")]
    async fn prepare_scan_maps_a_compile_error_to_a_compile_failure() {
        let (_dir, ready) = tiny_ready_corpus(1);
        let Prepared::Failed(failure) = prepare_scan(&ready, "broken := [text~/x/.").await else {
            panic!("a syntactically invalid catalog cannot be prepared");
        };
        assert_eq!(failure.category, ErrorCategory::Compile);
    }

    /// A corpus with no section labels is admitted — an empty vocabulary always
    /// fits — so the sweep runs and the producer is started and joins.
    #[tokio::test(flavor = "multi_thread")]
    async fn prepare_scan_admits_a_corpus_without_sections() {
        let (_dir, ready) = tiny_ready_corpus(1);
        let Prepared::Ready {
            producer, sections, ..
        } = prepare_scan(&ready, "p := [].").await
        else {
            panic!("a valid catalog over a readable corpus prepares");
        };
        assert!(matches!(sections, SectionScanState::Building(_)));
        drop(sections);
        producer
            .expect("an admitted vocabulary starts a producer")
            .await
            .expect("the producer exits once its receiver is dropped");
    }

    fn match_with_capture(pattern: &str, capture_name: &str) -> lingrep_core::fsm::Match {
        use compact_str::CompactString;
        lingrep_core::fsm::Match {
            pattern: CompactString::from(pattern),
            start: 0,
            end: 1,
            captures: vec![lingrep_core::fsm::Capture {
                name: CompactString::from(capture_name),
                path: CompactString::from(capture_name),
                start: 0,
                end: 1,
            }]
            .into_boxed_slice(),
            graph_bindings: Box::default(),
            lexical: None,
        }
    }

    /// A sink that published `sentences` sentences, the first of them with a
    /// match, and was finalized — the shape `resolve_scan` sees on a completed
    /// scan. `sentences` must equal the report's `sentences_matched`, which is
    /// what the publication gate compares.
    fn completed_sink(sentences: u64) -> SnapshotSink {
        let mut sink =
            SnapshotSink::new(5, vec![("p".to_string(), Box::default())], no_sections()).unwrap();
        for ordinal in 0..sentences {
            let matches: &[lingrep_core::fsm::Match] = if ordinal == 0 {
                &[match_with_capture("p", "grp")]
            } else {
                &[]
            };
            sink.push(&test_location(ordinal), &(&[] as &[TokenInfo]), matches)
                .unwrap();
        }
        sink.finish().expect("the fixture stream ends cleanly");
        sink
    }

    fn report(sentences: u64) -> lingrep_core::execution::ExecutionReport {
        lingrep_core::execution::ExecutionReport {
            policy_stats: lingrep_core::fsm::PolicyStats::default(),
            program_stats: lingrep_core::fsm::ProgramStats::default(),
            trace: None,
            sentences_admitted: sentences,
            sentences_matched: sentences,
            tokens_matched: sentences,
            matches_found: sentences,
            records_emitted: sentences,
        }
    }

    #[test]
    fn resolve_scan_maps_a_matching_completion_to_a_snapshot() {
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(report(3)),
                completed_sink(3),
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            Some(3),
            std::sync::Arc::from(""),
        );
        assert!(!resolution.invalidate_corrupt);
        match resolution.event {
            CoordinatorEvent::Completed { snapshot, .. } => {
                assert_eq!(snapshot.revision, RevisionId(1));
                assert_eq!(snapshot.patterns[0].name, "p");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn resolve_scan_completed_carries_source_into_snapshot() {
        let workbench = WorkbenchId::new("wb");
        let revision = RevisionId(1);
        let identity = crate::corpus_runtime::CorpusIdentity::new("fixture");
        let sink = completed_sink(0);
        let source: std::sync::Arc<str> = std::sync::Arc::from("p := [upos=VERB].");
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(lingrep_core::execution::ExecutionReport::default()),
                sink,
                None,
            ))),
            workbench,
            revision,
            identity,
            None,
            std::sync::Arc::clone(&source),
        );
        match resolution.event {
            CoordinatorEvent::Completed { snapshot, .. } => {
                assert_eq!(&*snapshot.source, "p := [upos=VERB].");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn resolve_scan_flags_a_sentence_count_mismatch_for_invalidation() {
        let resolution = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Completed(report(2)),
                completed_sink(2),
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            Some(99),
            std::sync::Arc::from(""),
        );
        assert!(resolution.invalidate_corrupt);
        assert_eq!(
            resolution.event,
            CoordinatorEvent::Failed {
                workbench: WorkbenchId::new("wb"),
                revision: RevisionId(1),
                failure: integrity_failure(),
            }
        );
    }

    #[test]
    fn resolve_scan_discards_cancelled_sink_and_retains_published_progress() {
        let cancelled = resolve_scan(
            ScanTaskResult::Finished(Ok((
                RunOutcome::Cancelled(report(2)),
                completed_sink(2),
                None,
            ))),
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            None,
            std::sync::Arc::from(""),
        );
        assert_eq!(
            cancelled.event,
            CoordinatorEvent::Cancelled {
                workbench: WorkbenchId::new("wb"),
                revision: RevisionId(1)
            }
        );
        assert!(!cancelled.invalidate_corrupt);
        assert_eq!(
            cancelled.progress,
            ExecutionProgress {
                sentences_admitted: 2,
                sentences_matched: 2,
                tokens_matched: 2,
                matches_found: 2,
                records_emitted: 2,
            }
        );
    }

    #[test]
    fn resolve_scan_maps_panic_to_failure() {
        let panicked = resolve_scan(
            ScanTaskResult::Panicked,
            WorkbenchId::new("wb"),
            RevisionId(1),
            crate::corpus_runtime::CorpusIdentity::new("fixture"),
            None,
            std::sync::Arc::from(""),
        );
        assert_eq!(
            panicked.event,
            CoordinatorEvent::Failed {
                workbench: WorkbenchId::new("wb"),
                revision: RevisionId(1),
                failure: execution_failure(),
            }
        );
        assert!(!panicked.invalidate_corrupt);
    }

    fn snapshot(workbench: &WorkbenchId, revision: RevisionId) -> Arc<CompletedSnapshot> {
        let pattern_key = PatternKey::new(0).unwrap();
        Arc::new(CompletedSnapshot {
            sections: crate::section_tally::SectionAvailability::Available(
                crate::section_tally::SectionTally::unlabelled(),
            ),
            observed_sentences: 0,
            sentences_matched: 0,
            revision,
            corpus_identity: crate::corpus_runtime::CorpusIdentity::new(format!(
                "test-corpus:{}",
                workbench.as_str()
            )),
            source: std::sync::Arc::from(""),
            derive_compounds: false,
            compound_category: CompoundCategory::Compound,
            patterns: vec![PatternDescriptor {
                key: pattern_key,
                name: "test".into(),
                exact_total: 2,
                metadata: Box::default(),
            }]
            .into_boxed_slice(),
            match_refs: vec![
                MatchRef {
                    sentence_index: 0,
                    span: 0..1,
                    pattern_key,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                },
                MatchRef {
                    sentence_index: 1,
                    span: 0..1,
                    pattern_key,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                },
            ]
            .into_boxed_slice(),
            capture_catalog: Box::default(),
            oversized_omissions: None,
        })
    }

    #[tokio::test]
    async fn registry_retains_one_latest_bounded_snapshot_per_tab() {
        let slot = test_slot();
        let registry = CoordinatorRegistry::new(2);
        let tab_count = 5;

        for tab in 0..tab_count {
            let workbench = WorkbenchId::new(format!("tab-{tab}"));
            let entry = registry.entry(&workbench, &slot).await;
            let revision_count = if tab == 0 { 20 } else { 1 };
            for number in 1..=revision_count {
                let revision = RevisionId(number);
                let mut state = entry.state.lock().await;
                assert!(state.accept(
                    revision,
                    "pattern test := [upos=NOUN].".into(),
                    CacheReadinessView::Ready
                ));
                assert!(state.apply_terminal(CoordinatorEvent::Completed {
                    workbench: workbench.clone(),
                    revision,
                    snapshot: snapshot(&workbench, revision),
                }));
            }
        }

        let entries = registry
            .inner
            .entries
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut retained_refs = 0;
        for entry in &entries {
            retained_refs += entry
                .state
                .lock()
                .await
                .displayed_snapshot()
                .map_or(0, |snapshot| snapshot.match_refs.len());
        }

        eprintln!(
            "registry entries={}, retained_refs={retained_refs}",
            entries.len()
        );
        assert_eq!(entries.len(), tab_count);
        assert_eq!(retained_refs, tab_count * 2);
    }

    #[test]
    fn scan_concurrency_defaults_to_one_and_is_configurable() {
        let default_registry = CoordinatorRegistry::new(2);
        assert_eq!(default_registry.available_scan_permits(), 1);

        let configured = CoordinatorRegistry::with_scan_concurrency(
            2,
            NonZeroUsize::new(8).unwrap(),
            NonZeroUsize::new(3).unwrap(),
        );
        assert_eq!(configured.available_scan_permits(), 3);
    }

    #[test]
    fn waiting_revision_advances_queued_then_running_then_sticks_terminal() {
        let workbench = WorkbenchId::new("wb");
        let mut state = RevisionCoordinatorState::new(workbench.clone());
        let revision = RevisionId(1);

        // Corpus still building → the attempt starts as WaitingForCorpus.
        assert!(state.accept(
            revision,
            "p := [upos=NOUN].".into(),
            CacheReadinessView::Building
        ));
        assert_eq!(
            state.current_attempt().unwrap().phase,
            AttemptPhase::WaitingForCorpus
        );

        // Scheduler order: WaitingForCorpus -> Queued -> Running.
        assert!(state.mark_queued(revision));
        assert_eq!(state.current_attempt().unwrap().phase, AttemptPhase::Queued);
        assert!(state.mark_running(revision));
        assert_eq!(
            state.current_attempt().unwrap().phase,
            AttemptPhase::Running
        );

        // Terminal is sticky: a completed snapshot lands, then a late progress
        // for the same revision is rejected (the lossy-watch invariant).
        let snapshot = snapshot(&workbench, revision);
        assert!(state.apply_terminal(CoordinatorEvent::Completed {
            workbench: workbench.clone(),
            revision,
            snapshot,
        }));
        assert_eq!(
            state.current_attempt().unwrap().phase,
            AttemptPhase::Completed
        );
        assert!(state.displayed_snapshot().is_some());
        assert!(!state.apply_progress_for_test(&workbench, revision, 99, 99));
    }
}
