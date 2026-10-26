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
//! Spill codec gates.
//!
//! These force the spill path itself; small end-to-end fixtures can stay below
//! pending-key and pending-byte limits, so `flush()` is never reached and no
//! run is ever written, merged, or decoded.
//!
//! Each benchmark below forces many level-0 runs, multi-level compaction,
//! finalization, and a full decode of the merged run.

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lingrep_core::spill::{SortedRunCounts, SpillPolicy, TempBudget};

/// Small pending caps force many level-0 runs and multi-level compaction.
fn forced_policy() -> SpillPolicy {
    SpillPolicy {
        max_pending_bytes: 64 * 1024,
        max_pending_keys: 512,
        merge_fan_in: 4,
    }
}

/// Resembles `ling` string-key summary workloads, whose
/// projected keys are long, repetitive, and share long sorted prefixes.
fn string_keys(criterion: &mut Criterion) {
    criterion.bench_function("spill_string_multilevel_finalize_visit", |bencher| {
        bencher.iter(|| {
            let budget = Arc::new(TempBudget::new(u64::MAX));
            let mut counts =
                SortedRunCounts::<String>::with_policy(forced_policy(), budget).unwrap();
            for index in 0..40_000u32 {
                counts
                    .add(
                        format!("名詞/普通名詞/一般/*/*/*/コテン/古典/{:08}", index % 30_000),
                        1,
                    )
                    .unwrap();
            }
            let mut visited = 0usize;
            counts
                .visit_sorted(|_key, count| {
                    visited += 1;
                    black_box(count);
                    Ok(())
                })
                .unwrap();
            black_box(visited)
        })
    });
}

/// Small integer-key records compress poorly and pay frame overhead. Expected
/// to regress.
fn integer_keys(criterion: &mut Criterion) {
    criterion.bench_function("spill_u64_multilevel_finalize_visit", |bencher| {
        bencher.iter(|| {
            let budget = Arc::new(TempBudget::new(u64::MAX));
            let mut counts = SortedRunCounts::<u64>::with_policy(forced_policy(), budget).unwrap();
            for index in 0..40_000u64 {
                counts.add(index % 30_000, 1).unwrap();
            }
            let mut visited = 0usize;
            counts
                .visit_sorted(|_key, count| {
                    visited += 1;
                    black_box(count);
                    Ok(())
                })
                .unwrap();
            black_box(visited)
        })
    });
}

criterion_group!(benches, string_keys, integer_keys);
criterion_main!(benches);
