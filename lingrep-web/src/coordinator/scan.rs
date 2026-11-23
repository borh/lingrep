use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use lingrep_core::analysis::VibratoSession;
use lingrep_core::execution::{
    AnalysisBinding, CancellationFlag, ExecutionError, ExecutionLimits, ExecutionProgress,
    MatchSink, OversizedSentenceOmissions, PolicySelection, ProgressSink, RunOutcome,
    SentenceAdmission, TokenizedAdmission, execute_analyzed,
};
use lingrep_core::fsm::policy_program::Overlaps;
use lingrep_core::fsm::{BoundCatalog, CompileOutput, CompiledCatalog, SchemaBindError};
use lingrep_core::token_match::{AttributeKey, TokenSchema};
use tokio::sync::watch;

use super::{CoordinatorRegistry, LivePatternTotal};
use crate::corpus_runtime::{CorpusIdentity, ReadyCorpus};
use crate::patterns::{compile_web_pattern, format_pattern_compile_error};
use crate::revision::{
    AttemptFailure, CancelReason, CoordinatorEvent, ErrorCategory, RevisionId, WorkbenchId,
};
use crate::section_tally::{self, spawn_span_producer};
use crate::snapshot::CompoundCategory;
use crate::snapshot_sink::{
    SectionScanState, SnapshotSink, SnapshotSinkInitError, SnapshotSinkMeta,
};

pub(crate) const DEFAULT_BATCH_SIZE: usize = 1_024;
const LIVE_SUMMARY_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub(crate) struct ScanOptions {
    pub(crate) pattern: Option<String>,
    pub(crate) index_results: bool,
    pub(crate) expected_analysis_identity: Option<String>,
    pub(crate) expected_vectors_blake3: Option<blake3::Hash>,
    pub(crate) search_scope: crate::search_scope::SearchScope,
    pub(crate) retained: usize,
    pub(crate) derive_compounds: bool,
    pub(crate) compound_category: CompoundCategory,
    pub(crate) document_reach: bool,
    pub(crate) overlaps: Overlaps,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            pattern: None,
            index_results: false,
            expected_analysis_identity: None,
            expected_vectors_blake3: None,
            search_scope: Default::default(),
            retained: 0,
            derive_compounds: false,
            compound_category: CompoundCategory::Compound,
            document_reach: false,
            overlaps: Overlaps::default(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ScanSetupError {
    Schema(lingrep_corpus::CorpusError),
    Bind(SchemaBindError),
    Source(lingrep_corpus::CorpusError),
}

/// Computes the sorted, deduplicated attribute projection needed to execute
/// `matcher` and render display surfaces under `schema`.
pub(crate) fn scan_projection_attributes(
    matcher: &CompiledCatalog,
    schema: &TokenSchema,
) -> Arc<[AttributeKey]> {
    let mut attributes = lingrep_core::execution::cached_run_attributes(matcher);
    attributes.extend(lingrep_core::token_match::display_surface_attributes(
        schema,
    ));
    attributes.sort_unstable();
    attributes.dedup();
    attributes.into()
}

/// Bound catalog, prepared sentence source, and omission counter shared by
/// live workbench scans and XLSX export rescans.
pub(crate) struct BoundScan {
    pub(crate) schema: TokenSchema,
    pub(crate) attributes: Arc<[AttributeKey]>,
    pub(crate) bound: BoundCatalog,
    pub(crate) source: lingrep_corpus::PreparedSentenceSource,
    pub(crate) omissions: Arc<StdMutex<OversizedSentenceOmissions>>,
}

impl BoundScan {
    pub(crate) fn prepare(
        matcher: &CompiledCatalog,
        artifact: &lingrep_corpus::CorpusArtifact,
        pool: &Arc<rayon::ThreadPool>,
        scope: crate::search_scope::BoundScope,
        cancel: &CancellationFlag,
    ) -> Result<Self, ScanSetupError> {
        let schema = artifact.token_schema().map_err(ScanSetupError::Schema)?;
        let attributes = scan_projection_attributes(matcher, &schema);
        let bound = matcher
            .bind_with_projection(&schema, attributes.iter().cloned())
            .map_err(ScanSetupError::Bind)?;
        // Prepared artifacts may contain oversized sentences. Omit and count
        // them so the result banner or export About sheet can disclose a
        // degraded run.
        let omissions = Arc::new(StdMutex::new(OversizedSentenceOmissions::default()));
        let source = scope
            .open_source(
                artifact,
                bound.projection(),
                matcher.lookaround_division_kinds(),
                Arc::clone(pool),
            )
            .map_err(ScanSetupError::Source)?
            .with_tokenized_admission(TokenizedAdmission::Omit(Arc::clone(&omissions)))
            .with_cancellation(cancel.clone());
        Ok(Self {
            schema,
            attributes,
            bound,
            source,
            omissions,
        })
    }

    pub(crate) fn execute<S, P>(
        self,
        pool: &rayon::ThreadPool,
        cancel: &CancellationFlag,
        sink: &mut S,
        progress: &mut P,
    ) -> (
        Result<RunOutcome, ExecutionError>,
        Arc<StdMutex<OversizedSentenceOmissions>>,
    )
    where
        S: MatchSink,
        P: ProgressSink<S>,
    {
        let mut observer = |_: lingrep_core::execution::TokenizedBatchObservation| Ok(());
        let outcome = execute_analyzed::<_, VibratoSession, _, _, _>(
            self.source,
            AnalysisBinding::Prepared(self.schema),
            &self.bound,
            pool,
            &ExecutionLimits::default(),
            DEFAULT_BATCH_SIZE,
            cancel,
            sink,
            progress,
            &mut observer,
            PolicySelection::Apply,
            &SentenceAdmission::All,
        );
        (outcome, self.omissions)
    }
}

pub(super) fn execution_failure() -> AttemptFailure {
    AttemptFailure {
        category: ErrorCategory::Execution,
        message:
            "Try the search again. If it fails again, ask the server operator to check the logs."
                .into(),
    }
}

pub(super) fn integrity_failure() -> AttemptFailure {
    AttemptFailure {
        category: ErrorCategory::Integrity,
        message: "The corpus data could not be verified or read. Search again when the corpus is ready; if it remains unavailable, ask the server operator to check the logs.".into(),
    }
}

fn binding_failure(
    error: &SchemaBindError,
    analyzer: &lingrep_corpus::AnalyzerRecord,
) -> AttemptFailure {
    AttemptFailure {
        category: ErrorCategory::Binding,
        message: format!(
            "The catalog cannot bind to this corpus's token schema ({} {}): {error}.",
            analyzer.family, analyzer.profile
        ),
    }
}

pub(super) fn cli_arguments(
    ready: &ReadyCorpus,
    clause: Option<&str>,
    overlaps: Overlaps,
) -> String {
    use crate::frequency::shell_word;
    let artifact = ready.artifact.directory().file_name().map_or_else(
        || "CORPUS".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut command = format!(
        "--patterns query.lx --corpus {}",
        shell_word(&format!("./{artifact}"))
    );
    if let Some(clause) = clause {
        command.push_str(&format!(" --where {}", shell_word(clause)));
    }
    if let Some(value) = crate::templates::overlaps_value(overlaps) {
        command.push_str(&format!(" --overlaps {value}"));
    }
    command
}

fn scope_failure(error: crate::search_scope::ResolveError) -> AttemptFailure {
    match error {
        crate::search_scope::ResolveError::Unanswerable(message) => AttemptFailure {
            category: ErrorCategory::Scope,
            message,
        },
        crate::search_scope::ResolveError::Corpus(error) => {
            tracing::warn!(%error, "scope resolution failed");
            integrity_failure()
        }
    }
}

pub(super) fn classify_execution_error(error: &ExecutionError) -> AttemptFailure {
    tracing::warn!(%error, "corpus scan execution error");
    match error {
        ExecutionError::Matching(lingrep_core::fsm::BoundExecutionError::Resource(error)) => {
            AttemptFailure {
                category: ErrorCategory::Execution,
                message: match error {
                    lingrep_core::fsm::MatchResourceError::BatchOutput { .. } => format!(
                        "{error} Narrow the pattern to produce fewer matches in one sentence."
                    ),
                    lingrep_core::fsm::MatchResourceError::Sentence { .. }
                    | lingrep_core::fsm::MatchResourceError::InterpretationReplay { .. }
                    | lingrep_core::fsm::MatchResourceError::Cancelled => error.to_string(),
                },
            }
        }
        ExecutionError::TokenizedSentenceTooLarge { actual, limit, .. }
        | ExecutionError::LookaroundWindowTooLarge { actual, limit } => AttemptFailure {
            category: ErrorCategory::Execution,
            message: format!(
                "The analyzed input exceeds the server's token-memory limit: estimated {actual} bytes; limit {limit}. Narrow the search scope or ask the server operator to review the limit."
            ),
        },
        ExecutionError::SentenceTooLarge { actual, limit, .. } => AttemptFailure {
            category: ErrorCategory::Execution,
            message: format!(
                "An input sentence is {actual} bytes; the server limit is {limit}. Ask the server operator to review the source and sentence limit."
            ),
        },
        ExecutionError::SinkLimit(message) => AttemptFailure {
            category: ErrorCategory::ResultLimit,
            message: message.clone(),
        },
        ExecutionError::Cache(_) | ExecutionError::Source { .. } => integrity_failure(),
        _ => execution_failure(),
    }
}

pub(super) fn cache_unavailable_failure() -> AttemptFailure {
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
pub(super) enum ScanTaskResult {
    Finished(
        Result<(RunOutcome, SnapshotSink, Option<OversizedSentenceOmissions>), AttemptFailure>,
    ),
    Panicked,
}

pub(super) struct ScanResolution {
    pub(super) event: CoordinatorEvent,
    pub(super) invalidate_corrupt: bool,
    pub(super) progress: ExecutionProgress,
}

pub(super) fn resolve_scan(
    result: ScanTaskResult,
    workbench: WorkbenchId,
    revision: RevisionId,
    identity: CorpusIdentity,
    expected_sentence_count: Option<u64>,
    source: Arc<str>,
    cancel_reason: CancelReason,
) -> ScanResolution {
    match result {
        ScanTaskResult::Finished(Ok((RunOutcome::Completed(report), ref sink, ref omissions)))
            if expected_sentence_count.is_some_and(|count| {
                count
                    != report.sentences_admitted
                        + sink.excluded_sentences()
                        + omissions.as_ref().map_or(0, |o| o.count)
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
        ScanTaskResult::Finished(Ok((RunOutcome::Completed(ref report), ref sink, _)))
            if report.sentences_admitted != sink.observed_sentences() =>
        {
            tracing::warn!(
                reported = report.sentences_admitted,
                observed = sink.observed_sentences(),
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
        // The lifecycle guard: `finish` transitions sections to `Finalized`, so
        // `into_completed_snapshot` returning `None` means the scan never
        // reached finalization and the span stream was never drained.
        ScanTaskResult::Finished(Ok((RunOutcome::Completed(_), sink, oversized_omissions))) => {
            match sink.into_completed_snapshot(revision, identity, source, oversized_omissions) {
                Some(snapshot) => ScanResolution {
                    event: CoordinatorEvent::Completed {
                        workbench,
                        revision,
                        snapshot: Arc::new(snapshot),
                    },
                    invalidate_corrupt: false,
                    progress: ExecutionProgress::default(),
                },
                None => ScanResolution {
                    event: CoordinatorEvent::Failed {
                        workbench,
                        revision,
                        failure: integrity_failure(),
                    },
                    invalidate_corrupt: false,
                    progress: ExecutionProgress::default(),
                },
            }
        }
        ScanTaskResult::Finished(Ok((RunOutcome::Cancelled(report), _, _))) => ScanResolution {
            event: CoordinatorEvent::Cancelled {
                workbench,
                revision,
                reason: cancel_reason,
            },
            invalidate_corrupt: false,
            progress: ExecutionProgress {
                sentences_admitted: report.sentences_admitted,
                sentences_matched: report.sentences_matched,
                tokens_scanned: report.tokens_scanned,
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
pub(super) enum Prepared {
    Ready {
        // Boxed: a compiled catalog dwarfs an `AttemptFailure`, and this enum
        // lives for exactly one match expression.
        compiled: Box<CompileOutput>,
        provenance: Arc<lingrep_corpus::ProvenanceView>,
        /// `None` when `admit` rejected the vocabulary: a producer with no
        /// consumer fills its channel and parks forever, so it is never started.
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
pub(super) async fn prepare_scan(
    registry: &CoordinatorRegistry,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    ready: &ReadyCorpus,
    source: &str,
    pattern: Option<&str>,
    expected_vectors: Option<blake3::Hash>,
    scope: &crate::search_scope::SearchScope,
) -> Prepared {
    let source = source.to_owned();
    let pattern = pattern.map(str::to_owned);
    let compiled = match registry
        .pattern_task(move || {
            let _permit = permit;
            pattern.as_deref().map_or_else(
                || compile_web_pattern(&source),
                |pattern| crate::patterns::compile_web_catalog_pattern(&source, pattern),
            )
        })
        .await
    {
        Ok(compiled) => compiled,
        Err(error) => {
            tracing::warn!(%error, "pattern preparation failed");
            return Prepared::Failed(AttemptFailure {
                category: ErrorCategory::Execution,
                message: "The pattern could not be checked. Try the search again.".into(),
            });
        }
    };
    if let Err(message) = crate::patterns::check_question_vectors(
        &compiled,
        crate::patterns::word_vectors(),
        expected_vectors,
    ) {
        return Prepared::Failed(AttemptFailure {
            category: ErrorCategory::VectorsChanged,
            message,
        });
    }
    let compiled = match compiled {
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
    let mut vocabulary = match provenance
        .section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
        .await
    {
        Ok(vocabulary) => vocabulary,
        Err(error) => {
            tracing::warn!(%error, "section coverage unavailable");
            return Prepared::Failed(integrity_failure());
        }
    };

    if let Some(selected) = scope.included_sections() {
        let selected = selected.iter().map(String::as_str).collect::<HashSet<_>>();
        vocabulary.retain(|section| selected.contains(section.label.as_str()));
    }

    // `admit` runs the ceiling test and calls the closure only if it passes, so
    // the producer cannot be started for a vocabulary that has no consumer.
    let scope_provenance = Arc::clone(&provenance);
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
        provenance: scope_provenance,
        producer,
        sections,
    }
}

/// Runs the scan on a blocking thread with default options.
#[cfg(test)]
pub(super) fn run_scan_blocking(
    ready: &ReadyCorpus,
    output: CompileOutput,
    max_refs: usize,
    sections: SectionScanState,
    cancel: &CancellationFlag,
    progress_tx: watch::Sender<Option<ScanProgress>>,
) -> Result<(RunOutcome, SnapshotSink, Option<OversizedSentenceOmissions>), AttemptFailure> {
    let options = ScanOptions {
        retained: max_refs,
        ..ScanOptions::default()
    };
    run_scan_blocking_with_options(ready, output, &options, sections, None, cancel, progress_tx)
}

/// Runs the scan on a blocking thread.
///
/// Takes the already-compiled catalog: the caller needs its pattern count to
/// decide section availability before the span producer is started, and
/// compiling in both places would either double the work or admit sections
/// against a zero-pattern catalog.
pub(super) fn run_scan_blocking_with_options(
    ready: &ReadyCorpus,
    mut output: CompileOutput,
    options: &ScanOptions,
    sections: SectionScanState,
    provenance: Option<Arc<lingrep_corpus::ProvenanceView>>,
    cancel: &CancellationFlag,
    progress_tx: watch::Sender<Option<ScanProgress>>,
) -> Result<(RunOutcome, SnapshotSink, Option<OversizedSentenceOmissions>), AttemptFailure> {
    let (resolved, scope) = match provenance {
        Some(provenance) => {
            let resolved = crate::search_scope::resolve(
                &options.search_scope,
                &provenance,
                &tokio::runtime::Handle::current(),
            )
            .map_err(scope_failure)?;
            let scope =
                crate::search_scope::BoundScope::new(resolved.admission.clone(), provenance);
            (resolved, scope)
        }
        None => Default::default(),
    };
    output.matcher = output.matcher.with_overlaps(options.overlaps);
    let bound_scan =
        BoundScan::prepare(&output.matcher, &ready.artifact, &ready.pool, scope, cancel).map_err(
            |error| match error {
                ScanSetupError::Schema(error) => {
                    tracing::warn!(%error, "artifact schema admission failed");
                    integrity_failure()
                }
                ScanSetupError::Bind(error) => binding_failure(&error, ready.artifact.analyzer()),
                ScanSetupError::Source(error) => {
                    tracing::warn!(%error, "sentence source construction failed");
                    integrity_failure()
                }
            },
        )?;
    let unavailable_attributes =
        (!bound_scan.bound.unavailable_attributes().is_empty()).then(|| {
            let analyzer = ready.artifact.analyzer();
            crate::snapshot::UnavailableAttributes {
                keys: bound_scan.bound.unavailable_attributes().into(),
                schema: format!("{} {}", analyzer.family, analyzer.profile).into(),
            }
        });
    let patterns = crate::pattern_catalog::pattern_metadata_pairs(&output.matcher);
    let metadata_conflicts = crate::pattern_catalog::pattern_metadata_conflicts(&output.matcher);
    let meta = SnapshotSinkMeta {
        search_scope: options.search_scope.clone(),
        admission: resolved.admission,
        excluded_sentences: resolved.excluded_sentences,
        cli_arguments: cli_arguments(ready, resolved.clause.as_deref(), options.overlaps),
        overlaps: options.overlaps,
        unavailable_attributes,
        vectors_binding: crate::patterns::bound_word_vectors(&output.matcher),
        derive_compounds: options.derive_compounds,
        compound_category: options.compound_category,
        document_reach: options.document_reach,
    };
    let result_index_attributes = options
        .index_results
        .then(|| Arc::clone(&bound_scan.attributes));
    let mut sink = SnapshotSink::new_for_scan(
        options.retained,
        patterns,
        metadata_conflicts,
        sections,
        result_index_attributes,
        meta,
    )
    .map_err(|error| match error {
        SnapshotSinkInitError::PatternSpaceExhausted => execution_failure(),
        SnapshotSinkInitError::ResultIndex(error) => AttemptFailure {
            category: ErrorCategory::Execution,
            message: error.to_string(),
        },
    })?;
    let mut progress = WatchProgress::new(progress_tx, LIVE_SUMMARY_INTERVAL);
    let (outcome, omissions) = bound_scan.execute(&ready.pool, cancel, &mut sink, &mut progress);
    let outcome = outcome.map_err(|error| classify_execution_error(&error))?;
    // Finalization runs here, on the blocking thread, because the span source
    // drains its channel with `blocking_recv`, which panics on a runtime
    // worker. Either way the builder — and with it the channel receiver — is
    // dropped before the caller joins the producer.
    if matches!(outcome, RunOutcome::Completed(_)) {
        sink.finish().map_err(|error| {
            tracing::warn!(%error, "span stream did not drain cleanly");
            integrity_failure()
        })?;
        sink.finish_result_index().map_err(|error| AttemptFailure {
            category: match error {
                crate::result_index::ResultIndexError::BudgetExceeded => ErrorCategory::ResultLimit,
                _ => ErrorCategory::Execution,
            },
            message: error.to_string(),
        })?;
    } else {
        sink.discard_sections();
    }
    let oversized_omissions = omissions.lock().map_err(|_| execution_failure())?.summary();
    Ok((outcome, sink, oversized_omissions))
}

#[derive(Debug, Clone)]
pub(super) struct ScanProgress {
    pub(super) progress: ExecutionProgress,
    pub(super) rows: Arc<[LivePatternTotal]>,
    pub(super) preview: Arc<[crate::result_index::ResultOccurrence]>,
    pub(super) capture_catalog: Arc<[crate::snapshot::CaptureDescriptor]>,
    pub(super) publication_attributes: Arc<[AttributeKey]>,
}

pub(super) struct WatchProgress {
    sender: watch::Sender<Option<ScanProgress>>,
    pub(super) interval: Duration,
    last_publish: Option<Instant>,
}

impl WatchProgress {
    pub(super) fn new(sender: watch::Sender<Option<ScanProgress>>, interval: Duration) -> Self {
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
            .patterns()
            .iter()
            .map(|pattern| LivePatternTotal {
                name: Arc::<str>::from(pattern.name.as_str()),
                total: pattern.exact_total,
            })
            .collect::<Vec<_>>()
            .into();
        let preview = sink.preview();
        self.sender.send_replace(Some(ScanProgress {
            progress,
            rows,
            preview: preview.to_vec().into(),
            publication_attributes: sink.publication_attributes(),
            capture_catalog: if preview.is_empty() {
                Arc::from([])
            } else {
                sink.capture_catalog().to_vec().into()
            },
        }));
    }
}
