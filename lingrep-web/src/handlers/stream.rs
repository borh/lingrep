use askama::Template;
use axum::{
    Router,
    extract::{Path, State},
    response::sse::{Event, Sse},
    routing::get,
};
use futures::stream::{Stream, StreamExt};
use std::convert::Infallible;

use crate::coordinator::CoordinatorView;
use crate::pattern_catalog::project_pattern_catalog;
use crate::revision::{AttemptPhase, WorkbenchId};
use crate::revision_status::{RevisionStatusKind, project_revision_status};
use crate::snapshot::CompletedSnapshot;
use crate::state::RequestStore;
use crate::templates::{
    LivePatternResultData, LiveResults, PatternResultData, ResultEventFragment, ResultsExplorer,
    RevisionStatusFragment, format_count,
};

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/stream/{request_id}", get(stream))
}

type BoxedSseStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

use std::pin::Pin;

pub(crate) struct StreamFrame {
    pub name: &'static str,
    pub body: String,
}

/// Pure projection of one coordinator view to its SSE frame. Lifted verbatim
/// from the stream `map` closure so the closure only wires transport; the
/// event-name selection and body strings are the observable SSE contract.
pub(crate) fn render_stream_event(view: &CoordinatorView, request_id: &str) -> StreamFrame {
    let status = project_revision_status(Some(view));
    if let Some(AttemptPhase::Completed) = view.state.current_attempt().map(|attempt| attempt.phase)
    {
        let explorer = render_completed_results(view.state.displayed_snapshot(), request_id);
        let scope_control = view
            .state
            .displayed_snapshot()
            .and_then(|snapshot| {
                crate::section_analysis::render_scope_control(
                    snapshot, request_id, None, None, true,
                )
            })
            .map_or_else(
                || "<div id=\"corpus-analysis-scope\" hx-swap-oob=\"outerHTML\"></div>".to_owned(),
                |fragment| fragment.to_string(),
            );
        let announcement = RevisionStatusFragment { status: &status }
            .render()
            .unwrap_or_else(|_| "<p role=\"status\">Results are current.</p>".into());
        StreamFrame {
            name: "result",
            body: result_event_body(&explorer, &scope_control, &announcement),
        }
    } else {
        let status_html = RevisionStatusFragment { status: &status }
            .render()
            .unwrap_or_else(|_| "<p>Unable to render search status.</p>".into());
        let name = if status.kind == RevisionStatusKind::Error {
            "error"
        } else {
            "progress"
        };
        let live_html = view
            .live_summary
            .as_ref()
            .filter(|summary| {
                view.state.current_attempt().is_some_and(|attempt| {
                    attempt.phase == AttemptPhase::Running && attempt.revision == summary.revision
                })
            })
            .and_then(render_live_results);
        StreamFrame {
            name,
            body: progress_event_body(&status_html, live_html.as_deref()),
        }
    }
}

pub async fn stream(
    State(store): State<RequestStore>,
    Path(request_id): Path<String>,
) -> Sse<BoxedSseStream> {
    let workbench = WorkbenchId::new(&request_id);
    if let Some(rx) = store.coordinators.subscribe(&workbench).await {
        let stream_request_id = request_id.clone();
        let coordinator_stream = tokio_stream::wrappers::WatchStream::new(rx).map(move |view| {
            let frame = render_stream_event(&view, &stream_request_id);
            Ok(Event::default().event(frame.name).data(frame.body))
        });
        return Sse::new(Box::pin(coordinator_stream));
    }

    Sse::new(Box::pin(futures::stream::iter(vec![Ok(
        Event::default().event("error").data(
            r#"<div class="alert alert-error">Request not found. Please try your search again.</div>"#,
        ),
    )])))
}

/// Assemble the completed-`result` SSE body: an out-of-band swap of the rendered
/// results explorer, a clear of the stale banner, and the status announcement.
/// The two `_html` inputs are already-rendered fragments and are emitted verbatim.
fn result_event_body(explorer: &str, scope_control: &str, announcement: &str) -> String {
    ResultEventFragment {
        explorer,
        scope_control,
        announcement,
    }
    .render()
    .unwrap_or_else(|_| {
        format!(
            "<div id=\"results\" hx-swap-oob=\"innerHTML\">{explorer}</div>\
             {scope_control}\
             <div id=\"live-results\" hx-swap-oob=\"innerHTML\"></div>\
             <div id=\"results-stale\" hx-swap-oob=\"innerHTML\"></div>\
             {announcement}"
        )
    })
}

fn render_live_results(summary: &crate::coordinator::LiveSummary) -> Option<String> {
    let rows = summary
        .rows
        .iter()
        .map(|row| LivePatternResultData {
            pattern_name: &row.name,
            count: row.total,
            count_label: format_count(row.total),
        })
        .collect::<Vec<_>>();
    LiveResults { results: &rows }.render().ok()
}

fn progress_event_body(status: &str, live_results: Option<&str>) -> String {
    match live_results {
        Some(live_results) => format!(
            "{status}<div id=\"live-results\" hx-swap-oob=\"innerHTML\">{live_results}</div>"
        ),
        None => format!("{status}<div id=\"live-results\" hx-swap-oob=\"innerHTML\"></div>"),
    }
}

/// Render the completed results explorer for `snapshot` (or the empty explorer
/// when there is none). Pure: this is the projection half of the completed SSE
/// event, lifted out of the stream `map` closure so the closure only wires
/// transport. `displayed_snapshot()` is a pure getter, so the value is taken
/// once.
fn render_completed_results(snapshot: Option<&CompletedSnapshot>, workbench_id: &str) -> String {
    let projection = snapshot
        .map(|snapshot| {
            let catalog = project_pattern_catalog(&snapshot.patterns);
            let results = catalog
                .patterns
                .into_iter()
                .map(|pattern| PatternResultData {
                    pattern_id: pattern.pattern_key,
                    pattern_name: pattern.name,
                    count: pattern.exact_total,
                    count_label: format_count(pattern.exact_total),
                    auto_load: pattern.auto_load,
                    authored_index: pattern.authored_index,
                    metadata_groups: pattern.groups,
                })
                .collect::<Vec<_>>();
            (
                results,
                catalog.grouping_fields,
                catalog.default_grouping_field,
            )
        })
        .unwrap_or_default();
    let (results, grouping_fields, default_grouping_field) = projection;
    let revision = snapshot.map_or(0, |snapshot| snapshot.revision.0);
    let section_analysis = snapshot
        .and_then(|snapshot| crate::section_analysis::render(snapshot, workbench_id, None, None));
    ResultsExplorer {
        workbench_id,
        revision,
        has_results: results.iter().any(|result| result.count > 0),
        results: &results,
        grouping_fields: &grouping_fields,
        default_grouping_field: default_grouping_field.as_deref().unwrap_or(""),
        section_analysis: section_analysis.as_ref(),
    }
    .render()
    .unwrap_or_else(|_| "<div class=\"error\">Unable to render results.</div>".into())
}

#[cfg(test)]
mod tests {
    use super::result_event_body;
    use crate::coordinator::CoordinatorView;
    use crate::revision::{
        AttemptFailure, CacheReadinessView, CoordinatorEvent, ErrorCategory,
        RevisionCoordinatorState, RevisionId, WorkbenchId,
    };
    use lingrep_core::execution::ExecutionProgress;
    use std::sync::Arc;

    #[test]
    fn completed_results_with_no_snapshot_renders_the_empty_explorer() {
        let html = super::render_completed_results(None, "wb");
        assert!(html.contains("No matches"), "{html}");
        assert!(!html.contains("hx-trigger=\"load, click\""), "{html}");
    }

    #[test]
    fn result_event_body_pins_the_oob_swap_wrapper() {
        // The `explorer`/`announcement` inputs are pre-rendered fragments and are
        // emitted verbatim (angle brackets included) — this is the observable SSE
        // `result` payload, so it is pinned to the byte.
        let body = result_event_body("<EXPLORER/>", "<SCOPE/>", "<ANNOUNCE/>");
        assert_eq!(
            body,
            concat!(
                "<div id=\"results\" hx-swap-oob=\"innerHTML\"><EXPLORER/></div>",
                "<SCOPE/>",
                "<div id=\"live-results\" hx-swap-oob=\"innerHTML\"></div>",
                "<div id=\"results-stale\" hx-swap-oob=\"innerHTML\"></div>",
                "<ANNOUNCE/>",
            )
        );
    }

    fn running_view(sentences: u64, tokens: u64) -> CoordinatorView {
        let workbench = WorkbenchId::new("wb");
        let mut state = RevisionCoordinatorState::new(workbench);
        assert!(state.accept(
            RevisionId(1),
            "p := [upos=NOUN].".into(),
            CacheReadinessView::Ready
        ));
        assert!(state.mark_running(RevisionId(1)));
        CoordinatorView {
            state,
            progress: ExecutionProgress {
                sentences_admitted: sentences,
                sentences_matched: sentences,
                tokens_matched: tokens,
                matches_found: 0,
                records_emitted: 0,
            },
            live_summary: None,
        }
    }

    #[test]
    fn running_phase_renders_a_progress_event() {
        let frame = super::render_stream_event(&running_view(3, 7), "wb");
        assert_eq!(frame.name, "progress");
        assert!(
            frame.body.contains("3 sentences, 7 tokens"),
            "{}",
            frame.body
        );
    }

    #[test]
    fn running_phase_publishes_exact_prefix_totals_in_a_separate_panel() {
        let mut view = running_view(3, 7);
        view.live_summary = Some(crate::coordinator::LiveSummary {
            revision: RevisionId(1),
            rows: vec![crate::coordinator::LivePatternTotal {
                name: Arc::from("noun <unsafe>"),
                total: 12_345,
            }]
            .into(),
        });

        let frame = super::render_stream_event(&view, "wb");

        assert_eq!(frame.name, "progress");
        assert!(frame.body.contains("id=\"live-results\""), "{}", frame.body);
        assert!(frame.body.contains("12,345"), "{}", frame.body);
        assert!(frame.body.contains("noun &lt;unsafe&gt;"), "{}", frame.body);
        assert!(!frame.body.contains("noun <unsafe>"), "{}", frame.body);
    }

    #[test]
    fn a_live_summary_from_another_revision_is_not_rendered() {
        let mut view = running_view(3, 7);
        view.live_summary = Some(crate::coordinator::LiveSummary {
            revision: RevisionId(2),
            rows: vec![crate::coordinator::LivePatternTotal {
                name: Arc::from("stale"),
                total: 99,
            }]
            .into(),
        });

        let frame = super::render_stream_event(&view, "wb");

        assert!(!frame.body.contains("stale"), "{}", frame.body);
        assert!(
            frame
                .body
                .contains("id=\"live-results\" hx-swap-oob=\"innerHTML\"></div>")
        );
    }

    #[test]
    fn failed_phase_renders_an_error_event_with_the_failure_message() {
        let workbench = WorkbenchId::new("wb");
        let mut state = RevisionCoordinatorState::new(workbench.clone());
        assert!(state.accept(
            RevisionId(1),
            "p := [upos=NOUN].".into(),
            CacheReadinessView::Ready
        ));
        assert!(state.apply_terminal(CoordinatorEvent::Failed {
            workbench,
            revision: RevisionId(1),
            failure: AttemptFailure {
                category: ErrorCategory::Execution,
                message: "The corpus scan could not be completed.".into(),
            },
        }));
        let view = CoordinatorView {
            state,
            progress: ExecutionProgress::default(),
            live_summary: None,
        };
        let frame = super::render_stream_event(&view, "wb");
        assert_eq!(frame.name, "error");
        assert!(
            frame
                .body
                .contains("The corpus scan could not be completed."),
            "{}",
            frame.body
        );
        assert!(
            frame
                .body
                .contains("id=\"live-results\" hx-swap-oob=\"innerHTML\"></div>")
        );
    }

    #[test]
    fn completed_phase_renders_a_result_event_with_the_oob_wrapper() {
        use crate::corpus_runtime::CorpusIdentity;
        use crate::snapshot::{CompletedSnapshot, PatternDescriptor, PatternKey};
        let workbench = WorkbenchId::new("wb");
        let mut state = RevisionCoordinatorState::new(workbench.clone());
        assert!(state.accept(
            RevisionId(1),
            "p := [upos=NOUN].".into(),
            CacheReadinessView::Ready
        ));
        let snapshot = Arc::new(CompletedSnapshot {
            sections: crate::section_tally::SectionAvailability::Available(
                crate::section_tally::SectionTally::unlabelled(),
            ),
            observed_sentences: 0,
            sentences_matched: 0,
            revision: RevisionId(1),
            corpus_identity: CorpusIdentity::new("fixture"),
            source: std::sync::Arc::from(""),
            derive_compounds: false,
            compound_category: crate::snapshot::CompoundCategory::Compound,
            patterns: vec![PatternDescriptor {
                key: PatternKey::new(0).unwrap(),
                name: "p".into(),
                exact_total: 0,
                metadata: Box::default(),
            }]
            .into_boxed_slice(),
            match_refs: Box::default(),
            capture_catalog: Box::default(),
            oversized_omissions: None,
        });
        assert!(state.apply_terminal(CoordinatorEvent::Completed {
            workbench,
            revision: RevisionId(1),
            snapshot,
        }));
        let view = CoordinatorView {
            state,
            progress: ExecutionProgress::default(),
            live_summary: None,
        };
        let frame = super::render_stream_event(&view, "wb");
        assert_eq!(frame.name, "result");
        assert!(
            frame
                .body
                .contains("id=\"results\" hx-swap-oob=\"innerHTML\""),
            "{}",
            frame.body
        );
        assert!(
            frame
                .body
                .contains("id=\"live-results\" hx-swap-oob=\"innerHTML\"></div>")
        );
    }
}
