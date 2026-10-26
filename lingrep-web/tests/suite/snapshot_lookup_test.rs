// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use crate::v6_support::RevisionTestExt;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::coordinator::{CoordinatorRegistry, SnapshotLookupError};
use lingrep_web::corpus_set::CorpusSlot;
use lingrep_web::revision::{RevisionId, WorkbenchId};

use crate::v6_support::{default_slot, runtime_fixture};

fn fixture() -> (
    tempfile::TempDir,
    CoordinatorRegistry,
    CorpusSlot,
    WorkbenchId,
) {
    let (fixture, runtime) = runtime_fixture(
        "lookup",
        vec![vec![TokenInfo::default().with_orth("食べる")]],
    );
    (
        fixture.directory,
        CoordinatorRegistry::new(5),
        default_slot(runtime),
        WorkbenchId::new("lookup"),
    )
}

async fn complete(
    registry: &CoordinatorRegistry,
    slot: &CorpusSlot,
    workbench: &WorkbenchId,
    revision: RevisionId,
) {
    let current = registry
        .snapshot(workbench)
        .await
        .and_then(|view| {
            view.state
                .current_attempt()
                .map(|attempt| attempt.revision.0)
        })
        .unwrap_or(0);
    for expected in (current + 1)..=revision.0 {
        assert_eq!(
            registry
                .accept_next_revision(
                    workbench.clone(),
                    slot,
                    format!("p{expected} := [text=食べる]."),
                )
                .await,
            RevisionId(expected),
        );
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if registry
                .snapshot(workbench)
                .await
                .and_then(|view| view.state.displayed_snapshot().map(|item| item.revision))
                == Some(revision)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn displayed_r3_remains_inspectable_while_r4_is_pending() {
    let (_directory, registry, slot, workbench) = fixture();
    complete(&registry, &slot, &workbench, RevisionId(3)).await;
    registry
        .accept_next_revision(workbench.clone(), &slot, "p4 := [text=食べる].".into())
        .await;

    let snapshot = registry
        .displayed_snapshot(&workbench, RevisionId(3))
        .await
        .unwrap();
    assert_eq!(snapshot.revision, RevisionId(3));
}

#[tokio::test]
async fn r3_is_stale_after_r4_is_displayed() {
    let (_directory, registry, slot, workbench) = fixture();
    complete(&registry, &slot, &workbench, RevisionId(3)).await;
    complete(&registry, &slot, &workbench, RevisionId(4)).await;
    assert_eq!(
        registry.displayed_snapshot(&workbench, RevisionId(3)).await,
        Err(SnapshotLookupError::StaleSnapshot),
    );
}

#[tokio::test]
async fn absent_workbench_is_not_conflated_with_stale_revision() {
    let (_directory, registry, _slot, workbench) = fixture();
    assert_eq!(
        registry.displayed_snapshot(&workbench, RevisionId(1)).await,
        Err(SnapshotLookupError::UnknownWorkbench),
    );
}
