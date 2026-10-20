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
use std::sync::Arc;

use lingrep_web::server_context::ServerContext;

/// The permit bounds builds only. A cache-open must never queue behind a build,
/// which is why the permit wraps `build_artifact` and not `prepare_artifact`.
#[tokio::test]
async fn the_build_permit_admits_exactly_one_builder() {
    let context = Arc::new(ServerContext::new("/tmp/dict").unwrap());
    let permit = context.build_permit();

    let first = permit.clone().acquire_owned().await.unwrap();
    assert_eq!(permit.available_permits(), 0);

    let second = tokio::spawn({
        let permit = permit.clone();
        async move { permit.acquire_owned().await.map(|_| ()) }
    });
    // The second builder is queued, not admitted.
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    drop(first);
    second.await.unwrap().unwrap();
}
