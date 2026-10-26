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
use lingrep_web::coordinator::CoordinatorRegistry;
use lingrep_web::revision::WorkbenchId;

use crate::v6_support::{default_slot, runtime_fixture};

async fn completed_snapshot(
    source: &str,
    max_refs: usize,
) -> lingrep_web::snapshot::CompletedSnapshot {
    let (_fixture, runtime) = runtime_fixture(
        "capture-snapshot",
        vec![vec![TokenInfo::default().with_orth("食べる")]; 100],
    );
    let slot = default_slot(runtime);
    let registry = CoordinatorRegistry::new(max_refs);
    let workbench = WorkbenchId::new("captures");
    registry
        .accept_next_revision(workbench.clone(), &slot, source.into())
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(snapshot) = registry
                .snapshot(&workbench)
                .await
                .and_then(|view| view.state.displayed_snapshot().cloned())
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn repeated_capture_strings_are_interned_only_for_retained_matches() {
    let snapshot = completed_snapshot("outer := (?<unit> [text=食べる]).", 10).await;
    assert_eq!(snapshot.patterns[0].exact_total, 100);
    assert_eq!(snapshot.match_refs.len(), 10);
    assert_eq!(snapshot.capture_catalog.len(), 1);
    assert!(snapshot.match_refs.iter().all(|reference| {
        reference.captures.len() == 1
            && reference.captures[0].capture_key == snapshot.capture_catalog[0].key
    }));
}

#[tokio::test]
async fn same_capture_name_in_different_patterns_has_distinct_keys() {
    let snapshot = completed_snapshot(
        "first := (?<unit> [text=食べる]).\nsecond := (?<unit> [text=食べる]).",
        1,
    )
    .await;
    assert_eq!(snapshot.capture_catalog.len(), 2);
    assert_ne!(
        snapshot.capture_catalog[0].key,
        snapshot.capture_catalog[1].key
    );
    assert_ne!(
        snapshot.capture_catalog[0].pattern_key,
        snapshot.capture_catalog[1].pattern_key
    );
}

#[tokio::test]
async fn capture_free_matches_keep_the_empty_representation() {
    let snapshot = completed_snapshot("plain := [text=食べる].", 10).await;
    assert!(snapshot.capture_catalog.is_empty());
    assert!(
        snapshot
            .match_refs
            .iter()
            .all(|reference| reference.captures.is_empty())
    );
}
