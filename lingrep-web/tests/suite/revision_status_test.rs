use std::sync::Arc;

use lingrep_core::execution::ExecutionProgress;
use lingrep_web::coordinator::CoordinatorView;
use lingrep_web::corpus_runtime::CorpusIdentity;
use lingrep_web::revision::{
    AttemptFailure, CacheReadinessView, CoordinatorEvent, ErrorCategory, RevisionCoordinatorState,
    RevisionId, WorkbenchId,
};
use lingrep_web::revision_status::{RevisionStatusKind, project_revision_status};
use lingrep_web::snapshot::CompletedSnapshot;

fn state() -> RevisionCoordinatorState {
    RevisionCoordinatorState::new(WorkbenchId::new("status"))
}

fn view(state: RevisionCoordinatorState) -> CoordinatorView {
    CoordinatorView {
        state,
        progress: ExecutionProgress::default(),
        live_summary: None,
    }
}

fn snapshot(revision: RevisionId) -> Arc<CompletedSnapshot> {
    Arc::new(CompletedSnapshot {
        search_scope: Default::default(),
        excluded_sentences: 0,
        sections: lingrep_web::section_tally::SectionAvailability::Available(
            lingrep_web::section_tally::SectionTally::unlabelled(),
        ),
        observed_sentences: 0,
        matched_sentences: 0,
        documents: None,
        sentences_matched: 0,
        revision,
        corpus_identity: CorpusIdentity::new("status"),
        source: std::sync::Arc::from(""),
        derive_compounds: false,
        compound_category: lingrep_web::snapshot::CompoundCategory::Compound,
        patterns: Box::default(),
        match_refs: Box::default(),
        capture_catalog: Box::default(),
        oversized_omissions: None,
    })
}

#[test]
fn first_running_revision_is_busy_but_not_stale() {
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "p := [upos=VERB].".into(),
        CacheReadinessView::Ready
    ));
    assert!(state.mark_running(RevisionId(1)));

    let status = project_revision_status(Some(&view(state)));

    assert!(status.busy);
    assert!(!status.stale);
    assert!(!status.terminal);
}

#[test]
fn newer_running_revision_is_stale_only_when_an_older_snapshot_exists() {
    let workbench = WorkbenchId::new("status");
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "old := [upos=NOUN].".into(),
        CacheReadinessView::Ready
    ));
    assert!(state.mark_running(RevisionId(1)));
    assert!(state.apply_terminal(CoordinatorEvent::Completed {
        workbench: workbench.clone(),
        revision: RevisionId(1),
        snapshot: snapshot(RevisionId(1)),
    }));
    assert!(state.accept(
        RevisionId(2),
        "new := [upos=VERB].".into(),
        CacheReadinessView::Ready
    ));
    assert!(state.mark_running(RevisionId(2)));

    let status = project_revision_status(Some(&view(state)));

    assert!(status.stale);
    assert!(status.message.contains("Scanning"));
}

#[test]
fn cancelled_is_superseded_not_failed() {
    let workbench = WorkbenchId::new("status");
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "p := [upos=VERB].".into(),
        CacheReadinessView::Ready
    ));
    assert!(state.apply_terminal(CoordinatorEvent::Cancelled {
        workbench,
        revision: RevisionId(1),
    }));

    let status = project_revision_status(Some(&view(state)));

    assert_eq!(status.kind, RevisionStatusKind::Superseded);
    assert!(status.message.contains("Superseded"));
}

#[test]
fn failed_status_preserves_sanitized_compile_detail() {
    let workbench = WorkbenchId::new("status");
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "p := [upos=VERB".into(),
        CacheReadinessView::Ready
    ));
    assert!(state.apply_terminal(CoordinatorEvent::Failed {
        workbench,
        revision: RevisionId(1),
        failure: AttemptFailure {
            category: ErrorCategory::Compile,
            message: "[UnexpectedToken] workbench.lx:1:12: expected `]`".into(),
        },
    }));

    let status = project_revision_status(Some(&view(state)));

    assert!(status.message.contains("UnexpectedToken"));
    assert_eq!(status.kind, RevisionStatusKind::Error);
}
