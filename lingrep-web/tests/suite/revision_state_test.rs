use std::sync::Arc;

use lingrep_web::corpus_runtime::CorpusIdentity;
use lingrep_web::revision::{
    AttemptPhase, CacheReadinessView, CoordinatorEvent, RevisionCoordinatorState, RevisionId,
    WorkbenchId,
};
use lingrep_web::snapshot::CompletedSnapshot;

fn snapshot(revision: u64) -> Arc<CompletedSnapshot> {
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
        revision: RevisionId(revision),
        corpus_identity: CorpusIdentity::new("corpus"),
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
fn late_completion_after_cancellation_cannot_publish() {
    let workbench = WorkbenchId::new("wb");
    let mut state = RevisionCoordinatorState::new(workbench.clone());
    state.accept(RevisionId(1), "one".into(), CacheReadinessView::Ready);
    state.apply_terminal(CoordinatorEvent::Completed {
        workbench: workbench.clone(),
        revision: RevisionId(1),
        snapshot: snapshot(1),
    });
    state.accept(RevisionId(2), "two".into(), CacheReadinessView::Ready);
    state.accept(RevisionId(3), "three".into(), CacheReadinessView::Ready);
    state.apply_terminal(CoordinatorEvent::Completed {
        workbench,
        revision: RevisionId(2),
        snapshot: snapshot(2),
    });
    assert_eq!(state.displayed_snapshot().unwrap().revision, RevisionId(1));
}

#[test]
fn waiting_for_corpus_is_the_current_attempt() {
    let mut state = RevisionCoordinatorState::new(WorkbenchId::new("wb"));
    state.accept(RevisionId(1), "one".into(), CacheReadinessView::Building);
    assert_eq!(
        state.current_attempt().unwrap().phase,
        AttemptPhase::WaitingForCorpus
    );
    state.accept(RevisionId(2), "two".into(), CacheReadinessView::Building);
    assert_eq!(state.current_attempt().unwrap().revision, RevisionId(2));
}

#[test]
fn waiting_attempt_is_explicitly_promoted_before_running() {
    let mut state = RevisionCoordinatorState::new(WorkbenchId::new("wb"));
    state.accept(RevisionId(1), "one".into(), CacheReadinessView::Building);

    assert!(state.mark_queued(RevisionId(1)));
    assert_eq!(state.current_attempt().unwrap().phase, AttemptPhase::Queued);
    assert!(state.mark_running(RevisionId(1)));
}

#[test]
fn protocol_rejects_wrong_owner_regressing_progress_and_duplicate_terminal() {
    let workbench = WorkbenchId::new("wb");
    let mut state = RevisionCoordinatorState::new(workbench.clone());
    state.accept(RevisionId(1), "one".into(), CacheReadinessView::Ready);
    assert!(state.mark_running(RevisionId(1)));
    assert!(!state.apply(CoordinatorEvent::Progress {
        workbench: WorkbenchId::new("other"),
        revision: RevisionId(1),
        sentences: 10,
        tokens: 20,
    }));
    assert!(state.apply(CoordinatorEvent::Progress {
        workbench: workbench.clone(),
        revision: RevisionId(1),
        sentences: 10,
        tokens: 20,
    }));
    assert!(!state.apply(CoordinatorEvent::Progress {
        workbench: workbench.clone(),
        revision: RevisionId(1),
        sentences: 9,
        tokens: 20,
    }));
    let completed = CoordinatorEvent::Completed {
        workbench,
        revision: RevisionId(1),
        snapshot: snapshot(1),
    };
    assert!(state.apply_terminal(completed.clone()));
    let once = state.clone();
    assert!(!state.apply_terminal(completed));
    assert_eq!(state, once);
}

#[test]
fn cloning_state_shares_the_source_allocation() {
    let mut state = RevisionCoordinatorState::new(WorkbenchId::new("wb"));
    assert!(state.accept(
        RevisionId(1),
        "p := [upos=NOUN].".into(),
        CacheReadinessView::Ready
    ));
    let cloned = state.clone();
    assert!(
        Arc::ptr_eq(
            &state.current_attempt().unwrap().source,
            &cloned.current_attempt().unwrap().source,
        ),
        "cloning the coordinator state must share the source allocation, not deep-copy it"
    );
}

#[hegel::test(test_cases = 300, derandomize = true)]
fn displayed_revision_never_regresses(tc: hegel::TestCase) {
    let revisions =
        tc.draw(hegel::generators::vecs(hegel::generators::integers::<u8>()).max_size(80));
    let workbench = WorkbenchId::new("wb");
    let mut state = RevisionCoordinatorState::new(workbench.clone());
    let mut last = None;
    for value in revisions {
        let revision = RevisionId(u64::from(value % 8));
        state.accept(revision, "source".into(), CacheReadinessView::Ready);
        state.apply_terminal(CoordinatorEvent::Completed {
            workbench: workbench.clone(),
            revision,
            snapshot: snapshot(revision.0),
        });
        let current = state.displayed_snapshot().map(|item| item.revision);
        if let (Some(before), Some(after)) = (last, current) {
            assert!(after >= before);
        }
        last = current;
    }
}
