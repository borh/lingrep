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
use std::io;
use std::sync::Arc;

use std::io::Write;

use lingrep_core::spill::{
    BudgetedWriter, SortedRunCounts, SpillCountOverflow, SpillOperation, SpillOperationError,
    SpillPolicy, TempBudget, TempCapacityError,
};

fn error_chain_contains<T>(error: &(dyn std::error::Error + 'static)) -> bool
where
    T: std::error::Error + 'static,
{
    error.downcast_ref::<T>().is_some()
        || error
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            .is_some_and(|source| error_chain_contains::<T>(source))
        || error.source().is_some_and(error_chain_contains::<T>)
}

fn two_runs(budget: Arc<TempBudget>) -> SortedRunCounts<String> {
    let mut counts = SortedRunCounts::new_budgeted(1, 16, budget).unwrap();
    counts.add("a".repeat(40), 1).unwrap();
    counts.add("b".repeat(40), 1).unwrap();
    counts
}

fn retained_two_run_bytes() -> u64 {
    let budget = Arc::new(TempBudget::new(u64::MAX));
    let counts = two_runs(budget.clone());
    let bytes = budget.used_bytes();
    drop(counts);
    bytes
}

fn forced_finalize_failure() -> io::Error {
    let budget = Arc::new(TempBudget::new(retained_two_run_bytes()));
    let mut counts = two_runs(budget);
    counts.finalize().unwrap_err()
}

fn forced_visit_failure() -> io::Error {
    let budget = Arc::new(TempBudget::new(retained_two_run_bytes()));
    let mut counts = two_runs(budget);
    counts.visit_sorted(|_, _| Ok(())).unwrap_err()
}

fn forced_compact_failure() -> io::Error {
    let budget = Arc::new(TempBudget::new(retained_two_run_bytes()));
    let mut counts = SortedRunCounts::new_budgeted(1, 2, budget).unwrap();
    counts.add("a".repeat(40), 1).unwrap();
    counts.add("b".repeat(40), 1).unwrap_err()
}

fn spill_operations(error: &(dyn std::error::Error + 'static)) -> Vec<SpillOperation> {
    fn collect(error: &(dyn std::error::Error + 'static), operations: &mut Vec<SpillOperation>) {
        if let Some(envelope) = error.downcast_ref::<SpillOperationError>() {
            operations.push(envelope.operation());
        }
        if let Some(source) = error
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
        {
            collect(source, operations);
        } else if let Some(source) = error.source() {
            collect(source, operations);
        }
    }

    let mut operations = Vec::new();
    collect(error, &mut operations);
    operations
}

#[test]
fn temp_budget_tracks_peak_across_growth_and_release() {
    let budget = Arc::new(TempBudget::new(100));
    let mut first = budget.reserve(10).unwrap();
    first.grow(5).unwrap();
    let second = budget.reserve(7).unwrap();
    assert_eq!(budget.used_bytes(), 22);
    assert_eq!(budget.peak_used_bytes(), 22);
    drop(second);
    drop(first);
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(budget.peak_used_bytes(), 22);
}

#[test]
fn visiting_empty_counts_is_a_noop() {
    let mut counts = SortedRunCounts::<String>::new(2, 4).unwrap();
    let mut visited = false;
    counts
        .visit_sorted(|_, _| {
            visited = true;
            Ok(())
        })
        .unwrap();
    assert!(!visited);
}

#[test]
fn one_reservation_grows_and_releases_its_total() {
    let budget = Arc::new(TempBudget::new(10));
    {
        let mut reservation = budget.reserve(3).unwrap();
        reservation.grow(4).unwrap();
        assert_eq!(reservation.held_bytes(), 7);
        assert_eq!(budget.used_bytes(), 7);
        assert_eq!(
            reservation.grow(4).unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        assert_eq!(reservation.held_bytes(), 7);
    }
    assert_eq!(budget.used_bytes(), 0);
}

#[test]
fn typed_limit_failure_carries_rejection_snapshot_without_mutation() {
    let budget = Arc::new(TempBudget::new(10));
    let held = budget.try_reserve(7).unwrap();
    let error = budget.try_reserve(4).unwrap_err();
    let TempCapacityError::LimitExceeded(limit) = error else {
        panic!("expected logical limit rejection");
    };
    assert_eq!(limit.requested_additional_bytes(), 4);
    assert_eq!(limit.budget().used_bytes(), 7);
    assert_eq!(limit.budget().limit_bytes(), 10);
    assert_eq!(budget.used_bytes(), 7);
    drop(held);
}

#[test]
fn compatibility_limit_error_retains_typed_source() {
    let budget = Arc::new(TempBudget::new(1));
    let error = budget.reserve(2).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert!(
        error
            .get_ref()
            .is_some_and(|source| source.downcast_ref::<TempCapacityError>().is_some())
    );
}

#[test]
fn typed_grow_failure_leaves_reservation_and_budget_unchanged() {
    let budget = Arc::new(TempBudget::new(10));
    let mut reservation = budget.try_reserve(7).unwrap();
    let error = reservation.try_grow(4).unwrap_err();
    assert!(matches!(error, TempCapacityError::LimitExceeded(_)));
    assert_eq!(reservation.held_bytes(), 7);
    assert_eq!(budget.used_bytes(), 7);
}

#[test]
fn second_sorted_visit_replays_without_allocating_another_run() {
    let budget = Arc::new(TempBudget::new(1 << 20));
    let mut counts = SortedRunCounts::new_budgeted(1, 4, budget.clone()).unwrap();
    counts.add("b".to_owned(), 1).unwrap();
    counts.add("a".to_owned(), 2).unwrap();
    counts.visit_sorted(|_, _| Ok(())).unwrap();
    let after_first = budget.used_bytes();
    counts.visit_sorted(|_, _| Ok(())).unwrap();
    assert_eq!(budget.used_bytes(), after_first);
}

#[test]
fn successful_visit_collapses_live_spill_state() {
    let budget = Arc::new(TempBudget::new(1 << 20));
    let mut counts = SortedRunCounts::new_budgeted(1, 8, budget).unwrap();
    for key in ["d", "c", "b", "a"] {
        counts.add(key.to_owned(), 1).unwrap();
    }
    let live_before_visit = counts.run_count();
    assert_eq!(live_before_visit, 4);
    counts.visit_sorted(|_, _| Ok(())).unwrap();
    assert_eq!(counts.buffered_keys(), 0);
    assert_eq!(counts.run_count(), 1);
    assert!(counts.run_count() < live_before_visit);
}

#[test]
fn spill_snapshot_retains_high_water_and_record_denominators() {
    let budget = Arc::new(TempBudget::new(1 << 20));
    let mut counts = SortedRunCounts::new_budgeted(1, 2, budget).unwrap();
    for key in ["d", "c", "b", "a"] {
        counts.add(key.to_owned(), 1).unwrap();
    }
    let before = counts.snapshot();
    assert_eq!(before.runs_created_total(), 7);
    assert_eq!(before.peak_run_count(), 3);
    assert_eq!(before.compaction_merge_count(), 3);
    assert_eq!(before.records_written_total(), 12);
    assert_eq!(before.records_read_total(), 8);
    assert_eq!(before.max_run_level(), 2);

    counts.visit_sorted(|_, _| Ok(())).unwrap();
    let after = counts.snapshot();
    assert_eq!(after.buffered_keys(), 0);
    assert_eq!(after.run_count(), 1);
    assert!(after.peak_run_count() > after.run_count());
    assert_eq!(after.records_read_total(), 12);
}

#[test]
fn repeated_visits_increment_reads_without_rewriting_runs() {
    let mut counts = SortedRunCounts::new(1, 2).unwrap();
    counts.add("b".to_owned(), 1).unwrap();
    counts.add("a".to_owned(), 1).unwrap();
    counts.visit_sorted(|_, _| Ok(())).unwrap();
    let first = counts.snapshot();
    counts.visit_sorted(|_, _| Ok(())).unwrap();
    let second = counts.snapshot();
    assert_eq!(
        second.records_written_total(),
        first.records_written_total()
    );
    assert_eq!(second.records_read_total(), first.records_read_total() + 2);
}

#[test]
fn finalization_failure_captures_runs_before_they_drop() {
    let measurement_budget = Arc::new(TempBudget::new(u64::MAX));
    let measured = two_runs(measurement_budget.clone());
    let retained_input_bytes = measurement_budget.used_bytes();
    assert_eq!(measured.snapshot().run_count(), 2);
    drop(measured);

    let budget = Arc::new(TempBudget::new(retained_input_bytes));
    let mut counts = two_runs(budget.clone());
    let live_before = counts.snapshot().run_count();
    assert_eq!(live_before, 2);

    let error = counts.finalize().unwrap_err();
    let envelope = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<SpillOperationError>())
        .expect("spill operation envelope");
    assert_eq!(envelope.operation(), SpillOperation::Finalize);
    assert_eq!(envelope.spill().run_count(), live_before);
    assert!(envelope.budget().used_bytes() > 0);
}

#[test]
fn spill_operation_is_stamped_once_by_the_owning_operation() {
    assert_eq!(
        spill_operations(&forced_finalize_failure()),
        vec![SpillOperation::Finalize]
    );
    assert_eq!(
        spill_operations(&forced_visit_failure()),
        vec![SpillOperation::Visit]
    );
    assert_eq!(
        spill_operations(&forced_compact_failure()),
        vec![SpillOperation::Compact]
    );
}

#[test]
fn compact_failure_snapshot_precedes_drained_run_release() {
    let retained = retained_two_run_bytes();
    let budget = Arc::new(TempBudget::new(retained));
    let mut counts = SortedRunCounts::new_budgeted(1, 2, budget.clone()).unwrap();
    counts.add("a".repeat(40), 1).unwrap();
    let error = counts.add("b".repeat(40), 1).unwrap_err();
    let envelope = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<SpillOperationError>())
        .expect("spill operation envelope");
    assert_eq!(envelope.operation(), SpillOperation::Compact);
    assert_eq!(envelope.spill().run_count(), 2);
    assert!(envelope.budget().used_bytes() > budget.used_bytes());
}

const PARTIAL_FLUSH_KEYS: usize = 5_000;

fn partial_flush_key(index: usize) -> String {
    format!("{}{index:06}", "a".repeat(40))
}

#[test]
fn partial_flush_failure_snapshot_precedes_reservation_release() {
    // Runs are zstd streams, so a two-record scenario charges nothing before
    // failing: the encoder buffers both records and emits its whole frame in
    // one write, which the budget refuses atomically. Pre-drop and post-drop
    // usage would both read zero, leaving the assertion below nothing to
    // catch.
    //
    // This workload is large enough that the encoder emits several blocks, so
    // bytes really are charged before a later block is refused — which is the
    // partial-flush state this test exists to pin.
    let measurement_budget = Arc::new(TempBudget::new(u64::MAX));
    let mut measured =
        SortedRunCounts::new_budgeted(PARTIAL_FLUSH_KEYS, 8, measurement_budget.clone()).unwrap();
    for index in 0..PARTIAL_FLUSH_KEYS {
        measured.add(partial_flush_key(index), 1).unwrap();
    }
    measured.flush().unwrap();
    let full_run_bytes = measurement_budget.used_bytes();
    drop(measured);
    assert!(full_run_bytes > 0, "a flushed run must charge something");

    let budget = Arc::new(TempBudget::new(full_run_bytes / 2));
    let mut counts = SortedRunCounts::new_budgeted(PARTIAL_FLUSH_KEYS, 8, budget.clone()).unwrap();
    let mut error = None;
    for index in 0..PARTIAL_FLUSH_KEYS {
        if let Err(failure) = counts.add(partial_flush_key(index), 1) {
            error = Some(failure);
            break;
        }
    }
    let error = match error {
        Some(error) => error,
        None => counts.flush().expect_err("half a run cannot fit"),
    };
    let envelope = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<SpillOperationError>())
        .expect("spill operation envelope");
    assert_eq!(envelope.operation(), SpillOperation::Flush);
    assert_eq!(envelope.spill().buffered_keys(), PARTIAL_FLUSH_KEYS);
    assert_eq!(envelope.spill().peak_buffered_keys(), PARTIAL_FLUSH_KEYS);
    assert_eq!(
        budget.used_bytes(),
        0,
        "the failed run released its reservation on the way out"
    );
    assert!(
        envelope.budget().used_bytes() > budget.used_bytes(),
        "the envelope must report pre-drop usage, not the released value"
    );
}

#[test]
fn taking_a_snapshot_does_not_flush_or_finalize() {
    let mut counts = SortedRunCounts::new(8, 8).unwrap();
    counts.add("pending".to_owned(), 1).unwrap();
    let before_runs = counts.run_count();
    let before_pending = counts.buffered_keys();
    let first = counts.snapshot();
    let second = counts.snapshot();
    assert_eq!(first, second);
    assert_eq!(counts.run_count(), before_runs);
    assert_eq!(counts.buffered_keys(), before_pending);
}

#[test]
fn logical_budget_failure_is_currently_storage_full() {
    let budget = Arc::new(TempBudget::new(1));
    let error = budget.reserve(2).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
}

#[test]
fn tiered_compaction_has_logarithmic_write_amplification() {
    let budget = Arc::new(TempBudget::new(1 << 30));
    let policy = SpillPolicy {
        max_pending_bytes: 1,
        max_pending_keys: 1,
        merge_fan_in: 4,
    };
    let mut counts = SortedRunCounts::with_policy(policy, budget).unwrap();
    for index in 0..257 {
        counts.add(format!("key-{index:04}"), 1).unwrap();
    }
    counts.visit_sorted(|_, _| Ok(())).unwrap();

    let initial = counts.initial_run_bytes_written();
    let total = counts.total_run_bytes_written();
    assert!(initial > 0);
    assert!(
        total <= initial * 6,
        "write amplification was {total}/{initial}"
    );
    assert_eq!(counts.run_count(), 1);
}

#[test]
fn pending_byte_and_key_caps_flush_independently() {
    let budget = Arc::new(TempBudget::new(1 << 20));
    let mut byte_bounded = SortedRunCounts::with_policy(
        SpillPolicy {
            max_pending_bytes: 80,
            max_pending_keys: 100,
            merge_fan_in: 4,
        },
        budget.clone(),
    )
    .unwrap();
    byte_bounded.add("a-long-key".repeat(20), 1).unwrap();
    assert_eq!(byte_bounded.buffered_keys(), 0);
    assert_eq!(byte_bounded.run_count(), 1);

    let mut key_bounded = SortedRunCounts::with_policy(
        SpillPolicy {
            max_pending_bytes: 1 << 20,
            max_pending_keys: 2,
            merge_fan_in: 4,
        },
        budget,
    )
    .unwrap();
    key_bounded.add("a".to_string(), 1).unwrap();
    key_bounded.add("b".to_string(), 1).unwrap();
    assert_eq!(key_bounded.buffered_keys(), 0);
    assert_eq!(key_bounded.run_count(), 1);
}

#[test]
fn budgeted_runs_fail_before_exceeding_their_limit() {
    let budget = Arc::new(TempBudget::new(1));
    let mut counts = SortedRunCounts::new_budgeted(1, 2, budget.clone()).unwrap();

    let error = counts.add("a".to_owned(), 1).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert_eq!(budget.used_bytes(), 0);
}

#[test]
fn sorted_runs_merge_duplicate_keys_in_lexical_order() {
    let mut counts = SortedRunCounts::new(2, 4).unwrap();
    counts.add(("b".to_owned(), "one".to_owned()), 1).unwrap();
    counts.add(("a".to_owned(), "two".to_owned()), 1).unwrap();
    counts.add(("b".to_owned(), "one".to_owned()), 2).unwrap();
    counts.add(("a".to_owned(), "one".to_owned()), 3).unwrap();

    let mut observed = Vec::new();
    counts
        .visit_sorted(|key, count| {
            observed.push((key.clone(), count));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        observed,
        vec![
            (("a".to_owned(), "one".to_owned()), 3),
            (("a".to_owned(), "two".to_owned()), 1),
            (("b".to_owned(), "one".to_owned()), 3),
        ]
    );
}

#[test]
fn pending_counts_reject_usize_overflow() {
    let mut counts = SortedRunCounts::new(2, 4).unwrap();
    counts.add("a".to_owned(), usize::MAX).unwrap();

    let error = counts.add("a".to_owned(), 1).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error_chain_contains::<SpillCountOverflow>(&error));
}

#[test]
fn merged_runs_reject_usize_overflow() {
    let mut counts = SortedRunCounts::new(1, 4).unwrap();
    counts.add("a".to_owned(), usize::MAX).unwrap();
    counts.add("a".to_owned(), 1).unwrap();

    let error = counts.visit_sorted(|_, _| Ok(())).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error_chain_contains::<SpillCountOverflow>(&error));
}

#[test]
fn budget_accounting_overflow_is_not_limit_exhaustion() {
    let budget = Arc::new(TempBudget::new(u64::MAX));
    let held = budget.try_reserve(u64::MAX).unwrap();
    let error = budget.try_reserve(1).unwrap_err();
    assert!(matches!(
        error,
        TempCapacityError::AccountingOverflow { .. }
    ));
    assert_eq!(budget.snapshot().used_bytes(), u64::MAX);
    drop(held);
}

#[test]
fn budget_snapshot_tracks_limit_current_peak_and_headroom() {
    let budget = Arc::new(TempBudget::new(100));
    let first = budget.try_reserve(40).unwrap();
    {
        let _second = budget.try_reserve(20).unwrap();
        assert_eq!(budget.snapshot().peak_used_bytes(), 60);
    }
    let snapshot = budget.snapshot();
    assert_eq!(snapshot.limit_bytes(), 100);
    assert_eq!(snapshot.used_bytes(), 40);
    assert_eq!(snapshot.peak_used_bytes(), 60);
    assert_eq!(snapshot.headroom_bytes(), 60);
    drop(first);
}

#[test]
fn run_writer_charges_each_record_before_writing_it() {
    let budget = Arc::new(TempBudget::new(u64::MAX));
    let mut counts = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 2,
            merge_fan_in: 4,
        },
        budget.clone(),
    )
    .unwrap();
    counts.add("alpha".to_string(), 1).unwrap();
    counts.add("beta".to_string(), 1).unwrap();
    counts.flush().unwrap();

    let snapshot = counts.snapshot();
    assert_eq!(snapshot.run_count(), 1);
    assert_eq!(snapshot.records_written_total(), 2);
    // Charged bytes must equal the run's recorded size exactly.
    assert_eq!(budget.used_bytes(), snapshot.initial_run_bytes_written());
}

#[test]
fn merged_runs_preserve_lexical_order_and_fold_duplicates_across_levels() {
    let budget = Arc::new(TempBudget::new(u64::MAX));
    let mut counts = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 2,
            merge_fan_in: 2,
        },
        budget,
    )
    .unwrap();
    for key in [
        "delta", "alpha", "charlie", "alpha", "bravo", "charlie", "alpha",
    ] {
        counts.add(key.to_string(), 1).unwrap();
    }
    let mut visited = Vec::new();
    counts
        .visit_sorted(|key, count| {
            visited.push((key.clone(), count));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        visited,
        vec![
            ("alpha".to_string(), 3),
            ("bravo".to_string(), 1),
            ("charlie".to_string(), 2),
            ("delta".to_string(), 1),
        ]
    );
}

#[test]
fn budgeted_writer_charges_before_writing_and_refuses_to_exceed_its_limit() {
    let budget = Arc::new(TempBudget::new(8));
    let reservation = budget.reserve(0).unwrap();
    let mut writer = BudgetedWriter::new(Vec::new(), reservation);

    writer.write_all(b"12345678").unwrap();
    assert_eq!(budget.used_bytes(), 8);

    let error = writer.write_all(b"9").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert_eq!(budget.used_bytes(), 8, "a refused write charges nothing");

    let (bytes, _reservation) = writer.into_parts();
    assert_eq!(bytes, b"12345678", "no byte reached the sink unreserved");
}

#[test]
fn persisted_runs_are_zstd_streams_and_cost_less_than_their_records() {
    let budget = Arc::new(TempBudget::new(u64::MAX));
    let mut counts = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 20_000,
            merge_fan_in: 4,
        },
        budget.clone(),
    )
    .unwrap();
    // Highly repetitive keys, matching the shape of projected n-gram keys.
    for index in 0..10_000u32 {
        counts
            .add(
                format!("名詞/普通名詞/一般/*/*/*/コテン/古典/{index:08}"),
                1,
            )
            .unwrap();
    }
    counts.flush().unwrap();

    let snapshot = counts.snapshot();
    // Each record is ~85 bytes uncompressed; 40 bytes each is a conservative
    // floor that raw JSONL cannot meet but compressed runs clear easily.
    let raw_lower_bound = 10_000u64 * 40;
    assert!(
        snapshot.initial_run_bytes_written() < raw_lower_bound,
        "stored bytes {} should be well under the uncompressed lower bound {}",
        snapshot.initial_run_bytes_written(),
        raw_lower_bound
    );
    assert_eq!(
        budget.used_bytes(),
        snapshot.initial_run_bytes_written(),
        "charged bytes must equal stored bytes"
    );

    // The run must decode: this is what proves it is a valid zstd stream,
    // not merely a small file.
    let mut visited = 0usize;
    counts
        .visit_sorted(|_key, count| {
            visited += 1;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
    assert_eq!(visited, 10_000);
}

#[test]
fn budget_exhausted_during_the_frame_epilogue_still_reports_pre_drop_evidence() {
    // A 32-key workload is far below zstd's block size, so essentially the
    // whole compressed frame is emitted during `finish` rather than during
    // `push`. That is what makes this test target the epilogue path.
    let measurement = Arc::new(TempBudget::new(u64::MAX));
    let mut measured = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 64,
            merge_fan_in: 4,
        },
        measurement.clone(),
    )
    .unwrap();
    for index in 0..32u32 {
        measured.add(format!("key-{index:04}"), 1).unwrap();
    }
    measured.flush().unwrap();
    let exact = measurement.used_bytes();
    assert!(exact > 0, "a flushed run must charge something");

    let budget = Arc::new(TempBudget::new(exact - 1));
    let mut counts = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 64,
            merge_fan_in: 4,
        },
        budget.clone(),
    )
    .unwrap();
    for index in 0..32u32 {
        counts.add(format!("key-{index:04}"), 1).unwrap();
    }
    let error = counts.flush().unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert!(error_chain_contains::<TempCapacityError>(&error));
    let envelope = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<SpillOperationError>())
        .expect("flush failure carries a spill operation envelope");
    assert_eq!(envelope.operation(), SpillOperation::Flush);

    // Load-bearing: the envelope must report PRE-drop usage. A weaker
    // assertion such as `used_bytes() <= exact - 1` passes at zero, which is
    // exactly the post-drop value this test exists to reject.
    let live_after_return = budget.used_bytes();
    assert_eq!(
        live_after_return, 0,
        "the failed run released its reservation on the way out"
    );
    assert!(
        envelope.budget().used_bytes() < exact,
        "the rejection never exceeds the limit"
    );
}

#[test]
fn a_failed_compaction_does_not_record_a_completed_merge() {
    let error = forced_compact_failure();
    let envelope = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<SpillOperationError>())
        .expect("compact failure carries a spill operation envelope");
    assert_eq!(envelope.operation(), SpillOperation::Compact);
    assert_eq!(
        envelope.spill().compaction_merge_count(),
        0,
        "a merge that failed must not be counted as completed"
    );
}

#[test]
fn small_integer_keys_stay_correct_when_compression_does_not_shrink_them() {
    // Small integer-key runs may be larger compressed than raw. Correctness,
    // not size, is the requirement here.
    let budget = Arc::new(TempBudget::new(u64::MAX));
    let mut counts = SortedRunCounts::<u64>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 1,
            merge_fan_in: 2,
        },
        budget,
    )
    .unwrap();
    for key in [9u64, 3, 7, 3, 1, 9, 9] {
        counts.add(key, 1).unwrap();
    }
    let mut visited = Vec::new();
    counts
        .visit_sorted(|key, count| {
            visited.push((*key, count));
            Ok(())
        })
        .unwrap();
    assert_eq!(visited, vec![(1, 1), (3, 2), (7, 1), (9, 3)]);
}

#[test]
fn pre_insert_and_post_insert_flush_produce_equal_runs() {
    // `add` takes a cheaper path when no byte cap is in force: it inserts
    // first and flushes after, where the general path flushes first. The two
    // must agree on run boundaries, counts and order, so run the same keys
    // through a key-capped policy and a byte-capped one whose byte cap is
    // slack enough that only the key cap can ever fire.
    fn drain(mut counts: SortedRunCounts<String>) -> (Vec<(String, usize)>, u64) {
        let mut rows = Vec::new();
        counts
            .visit_sorted(|key, count| {
                rows.push((key.clone(), count));
                Ok(())
            })
            .unwrap();
        (rows, counts.total_run_bytes_written())
    }

    let keys: Vec<String> = (0..500)
        .map(|index| format!("key-{:03}", index % 137))
        .collect();
    let mut key_capped = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 8,
            merge_fan_in: 4,
        },
        Arc::new(TempBudget::new(1 << 30)),
    )
    .unwrap();
    let mut byte_capped = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX - 1,
            max_pending_keys: 8,
            merge_fan_in: 4,
        },
        Arc::new(TempBudget::new(1 << 30)),
    )
    .unwrap();
    for key in &keys {
        key_capped.add(key.clone(), 1).unwrap();
        byte_capped.add(key.clone(), 1).unwrap();
    }
    assert_eq!(key_capped.run_count(), byte_capped.run_count());

    let (fast_rows, fast_bytes) = drain(key_capped);
    let (general_rows, general_bytes) = drain(byte_capped);
    assert_eq!(fast_rows, general_rows);
    assert_eq!(fast_bytes, general_bytes);
    assert_eq!(fast_rows.len(), 137);
    assert!(fast_rows.iter().all(|(_, count)| *count >= 3));
}

#[test]
fn framed_records_round_trip_across_a_wide_merge() {
    // Runs are framed as `[u32 length][MessagePack payload]`. Exercise both
    // sides of the length prefix — payloads under and over the 127 bytes that
    // change MessagePack's own string header width — through enough levels of
    // compaction that most records are decoded and re-encoded more than once.
    let budget = Arc::new(TempBudget::new(1 << 30));
    let mut counts = SortedRunCounts::<String>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: 4,
            merge_fan_in: 4,
        },
        budget,
    )
    .unwrap();
    let keys: Vec<String> = (0..64)
        .map(|index| {
            if index % 3 == 0 {
                format!("{index:03}-{}", "\u{3042}".repeat(60))
            } else {
                format!("{index:03}")
            }
        })
        .collect();
    for key in &keys {
        counts.add(key.clone(), 1).unwrap();
        counts.add(key.clone(), 2).unwrap();
    }
    let mut seen = Vec::new();
    counts
        .visit_sorted(|key, count| {
            seen.push((key.clone(), count));
            Ok(())
        })
        .unwrap();
    let mut expected: Vec<(String, usize)> = keys.into_iter().map(|key| (key, 3)).collect();
    expected.sort();
    assert_eq!(seen, expected);
}

#[test]
fn unspilled_counts_visit_and_drain_without_creating_runs() {
    let budget = Arc::new(TempBudget::new(u64::MAX));
    let mut counts = SortedRunCounts::new_budgeted(1000, 16, Arc::clone(&budget)).unwrap();
    counts.add("b".to_owned(), 2).unwrap();
    counts.add("a".to_owned(), 1).unwrap();
    counts.add("b".to_owned(), 3).unwrap();

    let mut visited = Vec::new();
    counts
        .visit_sorted(|key, count| {
            visited.push((key.clone(), count));
            Ok(())
        })
        .unwrap();
    assert_eq!(visited, [("a".to_owned(), 1), ("b".to_owned(), 5)]);
    assert_eq!(counts.run_count(), 0, "a resident visit writes no run");
    assert_eq!(counts.total_run_bytes_written(), 0);
    assert_eq!(
        budget.snapshot().peak_used_bytes(),
        0,
        "no temp bytes touched"
    );

    // The visit did not consume the buffer; the drain does, in the same order.
    let mut drain = counts.into_sorted_drain().unwrap();
    let mut drained = Vec::new();
    while let Some(entry) = drain.next_entry().unwrap() {
        drained.push(entry);
    }
    assert_eq!(drained, visited);
    assert!(drain.next_entry().unwrap().is_none());
}

#[test]
fn drained_streams_agree_between_resident_and_spilled_buffers() {
    let entries: Vec<(String, usize)> = ["d", "a", "c", "a", "b", "d", "a"]
        .iter()
        .map(|key| ((*key).to_owned(), 1))
        .collect();

    let drain_all = |mut counts: SortedRunCounts<String>| {
        for (key, count) in &entries {
            counts.add(key.clone(), *count).unwrap();
        }
        let mut drain = counts.into_sorted_drain().unwrap();
        let mut drained = Vec::new();
        while let Some(entry) = drain.next_entry().unwrap() {
            drained.push(entry);
        }
        drained
    };

    let budget = Arc::new(TempBudget::new(u64::MAX));
    let resident = drain_all(SortedRunCounts::new_budgeted(1000, 16, Arc::clone(&budget)).unwrap());
    let spilled = drain_all(SortedRunCounts::new_budgeted(1, 16, budget).unwrap());
    assert_eq!(resident, spilled);
    assert_eq!(
        resident,
        [
            ("a".to_owned(), 3),
            ("b".to_owned(), 1),
            ("c".to_owned(), 1),
            ("d".to_owned(), 2)
        ]
    );
}
