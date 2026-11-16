use std::sync::Arc;

use lingrep_core::execution::ExecutionProgress;
use lingrep_web::coordinator::CoordinatorView;
use lingrep_web::corpus_runtime::CorpusIdentity;
use lingrep_web::revision::{
    AttemptFailure, CacheReadinessView, CancelReason, CoordinatorEvent, ErrorCategory,
    RevisionCoordinatorState, RevisionId, WorkbenchId,
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
    snapshot_with_patterns(revision, Box::default())
}

fn snapshot_with_patterns(
    revision: RevisionId,
    patterns: Box<[lingrep_web::snapshot::PatternDescriptor]>,
) -> Arc<CompletedSnapshot> {
    Arc::new(CompletedSnapshot {
        search_scope: Default::default(),
        excluded_sentences: 0,
        admission: Default::default(),
        cli_arguments: String::new(),
        sections: lingrep_web::section_tally::SectionAvailability::Available(
            lingrep_web::section_tally::SectionTally::unlabelled(),
        ),
        observed_sentences: 0,
        matched_sentences: 0,
        documents: None,
        revision,
        corpus_identity: CorpusIdentity::new("status"),
        source: std::sync::Arc::from(""),
        derive_compounds: false,
        overlaps: Default::default(),
        compound_category: lingrep_web::snapshot::CompoundCategory::Compound,
        patterns,
        match_refs: Box::default(),
        result_index: None,
        capture_catalog: Box::default(),
        oversized_omissions: None,
        unavailable_attributes: None,
        vectors_binding: None,
    })
}

#[test]
fn first_running_revision_is_busy_but_not_stale() {
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "p := [upos=VERB].".into(),
        CacheReadinessView::Ready,
        Default::default()
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
        CacheReadinessView::Ready,
        Default::default()
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
        CacheReadinessView::Ready,
        Default::default()
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
        CacheReadinessView::Ready,
        Default::default()
    ));
    assert!(state.apply_terminal(CoordinatorEvent::Cancelled {
        workbench,
        revision: RevisionId(1),
        reason: CancelReason::Superseded,
    }));

    let status = project_revision_status(Some(&view(state)));

    assert_eq!(status.kind, RevisionStatusKind::Superseded);
    assert!(status.message.contains("Superseded"));
}

#[test]
fn a_revision_cancelled_for_an_absent_page_is_not_reported_as_superseded() {
    let workbench = WorkbenchId::new("status");
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "p := [upos=VERB].".into(),
        CacheReadinessView::Ready,
        Default::default()
    ));
    assert!(state.apply_terminal(CoordinatorEvent::Cancelled {
        workbench,
        revision: RevisionId(1),
        reason: CancelReason::Abandoned,
    }));

    let status = project_revision_status(Some(&view(state)));

    assert_eq!(status.kind, RevisionStatusKind::Abandoned);
    assert_eq!(status.message_key, Some("status.abandoned"));
    assert!(status.terminal);
    assert!(!status.busy);
    assert!(status.message.contains("Run the search again"));
}

fn failed_status(
    category: ErrorCategory,
    message: &str,
) -> lingrep_web::revision_status::RevisionStatus {
    let workbench = WorkbenchId::new("status");
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "p := [upos=VERB".into(),
        CacheReadinessView::Ready,
        Default::default()
    ));
    assert!(state.apply_terminal(CoordinatorEvent::Failed {
        workbench,
        revision: RevisionId(1),
        failure: AttemptFailure {
            category,
            message: message.into(),
        },
    }));
    project_revision_status(Some(&view(state)))
}

#[test]
fn a_compile_or_binding_failure_is_a_keyed_sentence_and_a_verbatim_diagnostic() {
    for (category, key, diagnostic) in [
        (
            ErrorCategory::Compile,
            "status.failed_compile",
            "[UnexpectedToken] workbench.lx:1:12: expected `]`",
        ),
        (
            ErrorCategory::Binding,
            "status.failed_binding",
            "The catalog cannot bind to this corpus's token schema (vibrato morphology): x.",
        ),
    ] {
        let status = failed_status(category, diagnostic);

        assert_eq!(status.message_key, Some(key));
        assert_eq!(status.diagnostic.as_deref(), Some(diagnostic));
        assert!(!status.message.contains(diagnostic), "{}", status.message);
        assert_eq!(status.kind, RevisionStatusKind::Error);
    }
}

#[test]
fn failure_status_preserves_scan_guidance_under_a_localized_heading() {
    for category in [ErrorCategory::Execution, ErrorCategory::Integrity] {
        let status = failed_status(category, "Safe recovery instructions.");
        assert_eq!(status.message, "The corpus scan could not be completed.");
        assert_eq!(status.message_key, Some("status.failed_scan"));
        assert_eq!(
            status.diagnostic.as_deref(),
            Some("Safe recovery instructions.")
        );
    }
    let cache = failed_status(ErrorCategory::Cache, "The English sentence.");
    assert_eq!(cache.message, "The English sentence.");
    assert_eq!(cache.message_key, Some("status.failed_cache"));
    assert_eq!(cache.diagnostic, None);
    let changed = failed_status(ErrorCategory::AnalysisChanged, "The English sentence.");
    assert_eq!(changed.message, "The English sentence.");
    assert_eq!(changed.message_key, Some("status.failed_analysis_changed"));
    assert_eq!(changed.diagnostic, None);
}

fn matched_pattern(exact_total: u64) -> lingrep_web::snapshot::PatternDescriptor {
    lingrep_web::snapshot::PatternDescriptor {
        key: lingrep_web::snapshot::PatternKey::new(0).expect("zero is a valid pattern key"),
        name: "p".into(),
        exact_total,
        metadata: Box::default(),
        origin: lingrep_web::snapshot::PatternOrigin::Authored { chained_total: 0 },
    }
}

fn stale_export_revision_after_a_newer_accept(exact_total: u64) -> Option<u64> {
    let workbench = WorkbenchId::new("status");
    let mut state = state();
    assert!(state.accept(
        RevisionId(1),
        "p := [].".into(),
        CacheReadinessView::Ready,
        Default::default()
    ));
    assert!(state.apply_terminal(CoordinatorEvent::Completed {
        workbench,
        revision: RevisionId(1),
        snapshot: snapshot_with_patterns(RevisionId(1), Box::new([matched_pattern(exact_total)])),
    }));
    let current = project_revision_status(Some(&view(state.clone())));
    assert_eq!(current.stale_export_revision, None);
    assert!(state.accept(
        RevisionId(2),
        "q := [].".into(),
        CacheReadinessView::Ready,
        Default::default()
    ));
    project_revision_status(Some(&view(state))).stale_export_revision
}

#[test]
fn stale_results_name_their_revision_for_export_only_when_a_pattern_matched() {
    assert_eq!(stale_export_revision_after_a_newer_accept(4), Some(1));
    assert_eq!(stale_export_revision_after_a_newer_accept(0), None);
}

#[test]
fn a_completed_run_reports_a_vector_file_the_catalog_does_not_declare() {
    use lingrep_corpus::word_vectors::VectorsBinding;

    let notice = |binding: Option<VectorsBinding>| {
        let mut state = state();
        assert!(state.accept(
            RevisionId(1),
            "p := [].".into(),
            CacheReadinessView::Ready,
            Default::default()
        ));
        let mut snapshot = Arc::unwrap_or_clone(snapshot(RevisionId(1)));
        snapshot.vectors_binding = binding;
        assert!(state.apply_terminal(CoordinatorEvent::Completed {
            workbench: WorkbenchId::new("status"),
            revision: RevisionId(1),
            snapshot: Arc::new(snapshot),
        }));
        project_revision_status(Some(&view(state)))
            .notices
            .into_iter()
            .map(|notice| (notice.key, notice.text, notice.args))
            .collect::<Vec<_>>()
    };

    assert_eq!(notice(None), []);
    let declaration = "vectors: {resource: \"walking\", blake3: \"00\"}";
    let undeclared = notice(Some(VectorsBinding::Undeclared {
        declaration: declaration.into(),
    }));
    let [(key, text, args)] = undeclared.as_slice() else {
        panic!("{undeclared:?}");
    };
    assert_eq!(*key, "status.vectors_undeclared");
    assert!(text.ends_with(declaration), "{text}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(args).unwrap()["declaration"],
        declaration
    );

    let mismatch = notice(Some(VectorsBinding::Mismatch {
        declared: vec![("chive".into(), "11".into())],
    }));
    let [(key, text, _)] = mismatch.as_slice() else {
        panic!("{mismatch:?}");
    };
    assert_eq!(*key, "status.vectors_mismatch");
    assert!(text.contains("`chive`"), "{text}");
}
