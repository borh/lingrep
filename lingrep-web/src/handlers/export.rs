use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use compact_str::CompactString;
use lingrep_core::analysis::VibratoSession;
use lingrep_core::corpus::SentenceId;
use lingrep_core::execution::{
    AnalysisBinding, CancellationFlag, ExecutionError, ExecutionLimits, PolicySelection,
    RunOutcome, SentenceAdmission, execute_analyzed,
};
use lingrep_core::fsm::CompiledCatalog;
use lingrep_core::token_match::AttributeKey;

use crate::corpus_context::{corpus_display_name, identity_digest};
use crate::export::sink::{ExportScanData, ExportSink};
use crate::export::workbook::{AboutMeta, build_workbook};
use crate::patterns::compile_web_pattern;
use crate::provenance_view::section_labels;
use crate::revision::{RevisionId, WorkbenchId};
use crate::snapshot::CompletedSnapshot;
use crate::state::RequestStore;

use super::{BoundProvenance, EndpointInspectionError, inspection_response, read_provenance};

/// Global row cap across every pattern sheet. Past it the scan keeps counting
/// matches but stops appending rows, so the concordance is bounded while the
/// About counts stay truthful.
const MAX_EXPORT_ROWS: usize = 100_000;
/// Keep the spreadsheet useful as a set of examples without letting its
/// whole-corpus rescan retain an unbounded number for one pattern.
const MAX_EXPORT_ROWS_PER_PATTERN: usize = 20;
/// Per-field character cap the sink applies to left/key/right, bounding retained
/// memory. A whole sentence is normally far under this.
// Practical per-field cap chosen from the peak-RSS measurement in
// docs/PERFORMANCE_REQUIREMENTS.md — generous for any real whole-sentence KWIC
// context while keeping the worst-case export memory bounded.
const EXPORT_KWIC_CHAR_CAP: usize = 256;
const DEFAULT_BATCH_SIZE: usize = 1_024;
const PROVENANCE_CHUNK: usize = 1_024;

/// Typed failure of the blocking export scan. `CacheCorruption` routes through
/// the runtime invalidator exactly like the display/inspection decode path;
/// `Cancelled` never yields a workbook.
#[derive(Debug)]
pub(crate) enum ExportScanError {
    Compile(String),
    CacheCorruption,
    Sink(String),
    Cancelled,
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/export/{workbench_id}/{revision}", get(export))
}

/// Pure mapping of the executor outcome to an export result. Factored out so the
/// "Cancelled never yields data" / "cache error → corruption" invariants are
/// unit-testable without a live cache.
fn classify_outcome(
    outcome: Result<RunOutcome, ExecutionError>,
    sink: ExportSink,
) -> Result<ExportScanData, ExportScanError> {
    match outcome {
        Ok(RunOutcome::Completed(_)) => Ok(sink.finish()),
        Ok(RunOutcome::Cancelled(_)) => Err(ExportScanError::Cancelled),
        Err(ExecutionError::Cache(_) | ExecutionError::Source { .. }) => {
            Err(ExportScanError::CacheCorruption)
        }
        Err(other) => Err(ExportScanError::Sink(format!("{other}"))),
    }
}

fn export_run_attributes(fsm: &CompiledCatalog) -> Vec<AttributeKey> {
    let mut attributes = fsm.execution_attributes();
    let text = AttributeKey::new("text").expect("form is a canonical attribute key");
    if !attributes.contains(&text) {
        attributes.push(text);
    }
    attributes
}

/// Runs the export scan to completion, owning `cancel`. Recompiles the accepted
/// source, projects KWIC inline through [`ExportSink`], and classifies the
/// outcome. Only `Completed` yields data.
#[cfg(test)]
fn run_export_scan(
    source: &str,
    artifact: &lingrep_corpus::CorpusArtifact,
    pool: &rayon::ThreadPool,
    cancel: &CancellationFlag,
) -> Result<ExportScanData, ExportScanError> {
    run_export_scan_with_compounds(source, artifact, pool, cancel, false, Default::default())
}

fn run_export_scan_with_compounds(
    source: &str,
    artifact: &lingrep_corpus::CorpusArtifact,
    pool: &rayon::ThreadPool,
    cancel: &CancellationFlag,
    derive_compounds: bool,
    scope: crate::search_scope::BoundScope,
) -> Result<ExportScanData, ExportScanError> {
    let output = compile_web_pattern(source)
        .map_err(|error| ExportScanError::Compile(format!("{error}")))?;
    // Seeded from PUBLISHED names, not from `patterns()`. A record relabeled by
    // `output_name` carries a name `patterns()` does not contain, and this sink
    // rejects an unknown name with a hard execution error — so seeding the
    // other way would fail the whole run for any catalog declaring a rename.
    let policy = output.matcher.catalog_policy();
    let names: Vec<String> = policy
        .published_names(output.matcher.patterns().keys().map(CompactString::as_str))
        .into_iter()
        .map(str::to_owned)
        .collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let export = ExportSink::new_with_compounds(
        &name_refs,
        MAX_EXPORT_ROWS,
        MAX_EXPORT_ROWS_PER_PATTERN,
        EXPORT_KWIC_CHAR_CAP,
        derive_compounds,
    );
    let mut sink = export;
    let attributes = export_run_attributes(&output.matcher);
    let schema = artifact
        .token_schema()
        .map_err(|_| ExportScanError::CacheCorruption)?;
    let bound = output
        .matcher
        .bind_with_projection(&schema, attributes.iter().cloned())
        .map_err(|_| ExportScanError::CacheCorruption)?;
    let source = scope
        .open_source(artifact, bound.projection())
        .map_err(|_| ExportScanError::CacheCorruption)?;
    let source = source.with_cancellation(cancel.clone());
    let mut progress = |_, _: &ExportSink| {};
    let mut observer = |_: lingrep_core::execution::TokenizedBatchObservation| Ok(());
    let outcome = execute_analyzed::<_, VibratoSession, _, _, _>(
        source,
        AnalysisBinding::Prepared(schema),
        &bound,
        pool,
        &ExecutionLimits::default(),
        DEFAULT_BATCH_SIZE,
        cancel,
        &mut sink,
        &mut progress,
        &mut observer,
        PolicySelection::Apply,
        &SentenceAdmission::All,
    );
    classify_outcome(outcome, sink)
}

/// Cancel the scan's flag if the request future is dropped (client disconnect),
/// unless disarmed after a successful await.
struct CancelOnDrop {
    flag: CancellationFlag,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.cancel();
        }
    }
}

/// Resolve the displayed snapshot for `workbench`/`revision`, mapping the lookup
/// error to the same `EndpointInspectionError` the display path (`bound_inspector`)
/// surfaces, so a superseded/evicted revision returns the established response.
async fn resolve(
    store: &RequestStore,
    workbench: &WorkbenchId,
    revision: RevisionId,
) -> Result<Arc<CompletedSnapshot>, EndpointInspectionError> {
    store
        .coordinators
        .displayed_snapshot(workbench, revision)
        .await
        .map_err(EndpointInspectionError::from)
}

async fn export(
    State(store): State<RequestStore>,
    Path((workbench_id, revision)): Path<(String, u64)>,
) -> Response {
    let workbench = WorkbenchId::new(&workbench_id);
    let revision_id = RevisionId(revision);

    // Fast-fail before queueing for the permit.
    if let Err(error) = resolve(&store, &workbench, revision_id).await {
        return inspection_response(error);
    }

    // Serialise with display scans; the permit is MOVED into the blocking task
    // below so it releases when the scan actually ends.
    let permit = store.coordinators.acquire_scan_permit().await;

    // Re-validate: a preceding scan may have made the wait long.
    let snapshot = match resolve(&store, &workbench, revision_id).await {
        Ok(snapshot) => snapshot,
        Err(error) => return inspection_response(error),
    };
    // The workbench's own corpus, not the store's default: the identity check
    // below is only meaningful against the corpus the snapshot was scanned from.
    // The key comes back with the runtime because the About sheet must NAME this
    // same corpus further down, and one binding should cost one lookup.
    let Some((corpus_key, runtime)) = store.coordinators.bound_entry(&workbench).await else {
        return inspection_response(EndpointInspectionError::UnknownWorkbench);
    };
    let Some(ready) = runtime.ready().await else {
        return inspection_response(EndpointInspectionError::StaleCorpus);
    };
    if ready.identity != snapshot.corpus_identity {
        return inspection_response(EndpointInspectionError::StaleCorpus);
    }

    let cancel = CancellationFlag::new();
    let mut guard = CancelOnDrop {
        flag: cancel.clone(),
        armed: true,
    };

    let source = Arc::clone(&snapshot.source);
    let derive_compounds = snapshot.derive_compounds;
    let artifact = Arc::clone(&ready.artifact);
    let pool = Arc::clone(&ready.pool);
    let scan_cancel = cancel.clone();
    let scope = match ready.provenance().await {
        Ok(view) => crate::search_scope::BoundScope::new(snapshot.search_scope.clone(), view),
        Err(_) => return inspection_response(EndpointInspectionError::CacheCorruption),
    };
    let scan = tokio::task::spawn_blocking(move || {
        let _permit = permit; // released when the scan ends, not when the future drops
        run_export_scan_with_compounds(
            &source,
            &artifact,
            &pool,
            &scan_cancel,
            derive_compounds,
            scope,
        )
    })
    .await;
    guard.armed = false; // await returned; disarm the cancel-on-drop guard

    let data = match scan {
        Ok(Ok(data)) => data,
        Ok(Err(ExportScanError::CacheCorruption)) => {
            // The workbench's own corpus, not the store's default.
            runtime.invalidator().invalidate_corrupt().await; // exactly once
            return inspection_response(EndpointInspectionError::CacheCorruption);
        }
        Ok(Err(ExportScanError::Cancelled)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Ok(Err(ExportScanError::Compile(message))) => {
            tracing::warn!(%message, "export recompile failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Ok(Err(ExportScanError::Sink(message))) => {
            tracing::warn!(%message, "export sink failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let provenance = BoundProvenance::open(&ready).await.ok();

    // Section labels via CHUNKED provenance reads.
    let mut ids: Vec<u64> = data
        .patterns
        .iter()
        .flat_map(|pattern| pattern.rows.iter().map(|row| row.sentence_index))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut sections: HashMap<u64, String> = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(PROVENANCE_CHUNK) {
        let sentence_ids: Vec<SentenceId> = chunk.iter().map(|&id| SentenceId::new(id)).collect();
        let page = read_provenance(provenance.as_ref(), &sentence_ids).await;
        for (id, entry) in chunk.iter().zip(page) {
            let joined = entry
                .map(|provenance| section_labels(&provenance.active_regions).join(", "))
                .unwrap_or_default();
            sections.insert(*id, joined);
        }
    }

    // The workbench's own corpus. A runtime carries no researcher-facing name,
    // so the bound KEY resolves the slot that does; the path never leaves the
    // server. A slot that vanished since admission falls back to the generic
    // label rather than naming some other corpus.
    let corpus_name = store
        .corpora
        .resolve(Some(corpus_key.as_str()))
        .map_or_else(
            || crate::corpus_context::UNNAMED_CORPUS.to_owned(),
            |slot| slot.display_name.clone(),
        );
    let about = AboutMeta {
        scan_scope: if snapshot.search_scope.is_unrestricted() {
            "complete unfiltered corpus".into()
        } else {
            "accepted search scope".into()
        },
        filters: serde_json::to_string(&snapshot.search_scope).unwrap_or_default(),
        corpus: corpus_name,
        corpus_id: identity_digest(&snapshot.corpus_identity),
        dictionary: corpus_display_name(&store.config.dict_path),
        revision,
        exact_matches: data.exact_matches,
        exported_rows: data.exported_rows as u64,
        pattern_source: snapshot.source.to_string(),
    };
    let exported_at = chrono::Utc::now().to_rfc3339();

    let bytes = match tokio::task::spawn_blocking(move || {
        build_workbook(&about, &data.patterns, &sections, &exported_at)
    })
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let filename = format!("concordance-{revision}.xlsx");
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                ),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                    .expect("ascii filename is a valid header value"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::sink::ExportSink;
    use lingrep_core::execution::{ExecutionError, RunOutcome, execute};

    #[test]
    fn export_attributes_add_orth_beside_kwic_demand() {
        use lingrep_core::token_match::TokenAttributeIndex;

        let matcher = crate::patterns::compile_web_pattern(
            "transform: [{op: \"truncate_before\", stop: \"[upos=SCONJ]\"}]\n\
             pattern p := ^ []+ $.",
        )
        .unwrap()
        .matcher;

        assert_eq!(
            export_run_attributes(&matcher),
            [TokenAttributeIndex::Pos, TokenAttributeIndex::Orth]
        );
    }

    fn scan_full_export_for_test(
        ready: &crate::corpus_runtime::ReadyCorpus,
        source: &str,
    ) -> ExportScanData {
        let output = crate::patterns::compile_web_pattern(source).unwrap();
        let policy = output.matcher.catalog_policy();
        let names: Vec<String> = policy
            .published_names(output.matcher.patterns().keys().map(CompactString::as_str))
            .into_iter()
            .map(str::to_owned)
            .collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut sink = ExportSink::new(
            &name_refs,
            MAX_EXPORT_ROWS,
            MAX_EXPORT_ROWS_PER_PATTERN,
            EXPORT_KWIC_CHAR_CAP,
        );
        let execution_source = ready.artifact.sentence_source().unwrap();
        let mut progress = |_, _: &ExportSink| {};
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
        );
        classify_outcome(outcome, sink).expect("fixture export must complete")
    }

    #[test]
    fn export_projection_matches_full_source_when_policy_changes_the_span() {
        use lingrep_core::token_match::TokenInfo;

        let tokens = vec![
            TokenInfo::default().with_orth("前").with_pos("NOUN"),
            TokenInfo::default().with_orth("ので").with_pos("SCONJ"),
            TokenInfo::default().with_orth("後").with_pos("NOUN"),
        ];
        let (_dir, ready) = crate::corpus_runtime::test_ready_corpus(vec![tokens]);
        let source = "transform: [{op: \"truncate_before\", stop: \"[upos=SCONJ]\"}]\n\
                      pattern p := ^ []+ $.";

        let projected = run_export_scan(
            source,
            &ready.artifact,
            &ready.pool,
            &CancellationFlag::new(),
        )
        .expect("projected production export must complete");
        let full = scan_full_export_for_test(&ready, source);

        assert_eq!(projected.exact_matches, full.exact_matches);
        assert_eq!(projected.exported_rows, full.exported_rows);
        assert_eq!(projected.patterns.len(), full.patterns.len());
        for (projected_pattern, full_pattern) in projected.patterns.iter().zip(&full.patterns) {
            assert_eq!(projected_pattern.name, full_pattern.name);
            assert_eq!(projected_pattern.exact_matches, full_pattern.exact_matches);
            assert_eq!(projected_pattern.rows.len(), full_pattern.rows.len());
            for (projected_row, full_row) in projected_pattern.rows.iter().zip(&full_pattern.rows) {
                assert_eq!(projected_row.sentence_index, full_row.sentence_index);
                assert_eq!(projected_row.left, full_row.left);
                assert_eq!(projected_row.key, full_row.key);
                assert_eq!(projected_row.right, full_row.right);
            }
        }
        let row = &projected.patterns[0].rows[0];
        assert_eq!(
            (row.left.as_str(), row.key.as_str(), row.right.as_str()),
            ("", "前", "ので後")
        );
    }

    #[test]
    fn cancelled_outcome_never_yields_data() {
        use compact_str::CompactString;
        use lingrep_core::execution::MatchSink;
        use lingrep_core::fsm::Match;
        use lingrep_core::token_match::TokenInfo;

        let mut sink = ExportSink::new(&["p"], 10, 10, 1_024);
        sink.push(
            &lingrep_core::execution::SentenceLocation {
                source_metadata: None,
                corpus_sentence_ordinal: 7,
                document_id: lingrep_core::corpus::DocumentId::new(0),
                document_ordinal: 0,
                document_sentence_ordinal: 7,
                source_name: "".into(),
            },
            &[TokenInfo::default().with_orth("a")],
            &[Match {
                pattern: CompactString::from("p"),
                start: 0,
                end: 1,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
            }],
        )
        .unwrap();
        assert!(matches!(
            classify_outcome(Ok(RunOutcome::Cancelled(Default::default())), sink),
            Err(ExportScanError::Cancelled)
        ));
    }

    #[test]
    fn cache_error_maps_to_corruption() {
        let sink = ExportSink::new(&["p"], 10, 10, 1_024);
        let out = classify_outcome(Err(ExecutionError::Cache(std::io::Error::other("x"))), sink);
        assert!(matches!(out, Err(ExportScanError::CacheCorruption)));
    }

    #[test]
    fn completed_yields_data() {
        let sink = ExportSink::new(&["p"], 10, 10, 1_024);
        assert!(classify_outcome(Ok(RunOutcome::Completed(Default::default())), sink).is_ok());
    }
}
