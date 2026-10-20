use std::cmp::Ordering;
use std::io;
use std::sync::Arc;

use crate::tui::types::ErrorCode;
use crossbeam_channel::Sender;
use lingrep_core::execution::{
    CancellationFlag, ExecutionError, MatchSink, RunOutcome, SentenceLocation, SinkControl,
};
use lingrep_core::fsm::{CompiledCatalog, Match};
use lingrep_core::spill::SortedRunCounts;
use serde::{Deserialize, Serialize};

use super::TaskHandle;
use crate::direct_source::matched_surface;
use crate::interactive_execution::InteractiveExecution;
use crate::tui::cache::SurfaceEntry;
use crate::tui::message::ExplorerMessage;
use crate::tui::paged_store::{PagedStore, PagedStoreWriter};
use crate::tui::types::{PatternId, RequestId};

pub(crate) struct SurfaceWorker {
    request_id: RequestId,
    pattern_id: PatternId,
    pattern_name: String,
    matcher: Arc<CompiledCatalog>,
    execution: Arc<InteractiveExecution>,
    cancel: CancellationFlag,
}

impl SurfaceWorker {
    pub(crate) fn spawn(
        request_id: RequestId,
        pattern_id: PatternId,
        pattern_name: String,
        matcher: Arc<CompiledCatalog>,
        execution: Arc<InteractiveExecution>,
        tx: Sender<ExplorerMessage>,
    ) -> TaskHandle {
        TaskHandle::spawn(request_id, move |cancel| {
            Self {
                request_id,
                pattern_id,
                pattern_name,
                matcher,
                execution,
                cancel,
            }
            .run(tx);
        })
    }

    pub(crate) fn run(self, tx: Sender<ExplorerMessage>) {
        let budget = self.execution.temp_budget();
        let mut sink = match SurfaceSink::new(self.pattern_name, budget.clone()) {
            Ok(sink) => sink,
            Err(error) => {
                send_failure(&tx, self.request_id, self.pattern_id, error);
                return;
            }
        };
        let result = self.execution.execute(
            &self.matcher,
            &self.cancel,
            &mut sink,
            &mut |_progress, _| {},
        );
        match result {
            Ok(RunOutcome::Completed(_)) => match sink.finish(budget) {
                Ok(store) => {
                    let _ = tx.send(ExplorerMessage::SurfaceFinished {
                        request_id: self.request_id,
                        pattern_id: self.pattern_id,
                        store,
                    });
                }
                Err(error) => send_failure(&tx, self.request_id, self.pattern_id, error),
            },
            Ok(RunOutcome::Cancelled(_)) => {
                let _ = tx.send(ExplorerMessage::SurfaceFailed {
                    request_id: self.request_id,
                    pattern_id: self.pattern_id,
                    code: Some(ErrorCode::Cancelled),
                    error: "Processing cancelled".into(),
                });
            }
            Err(error) => {
                if let Some(error) = sink.failure.take() {
                    send_failure(&tx, self.request_id, self.pattern_id, error);
                    return;
                }
                let _ = tx.send(ExplorerMessage::SurfaceFailed {
                    request_id: self.request_id,
                    pattern_id: self.pattern_id,
                    code: None,
                    error: error.to_string(),
                });
            }
        }
    }
}

const MAX_BUFFERED_SURFACES: usize = 4_096;
const MAX_SPILL_RUNS: usize = 16;

struct SurfaceSink {
    pattern: String,
    counts: SortedRunCounts<String>,
    failure: Option<io::Error>,
}

impl SurfaceSink {
    fn new(pattern: String, budget: Arc<lingrep_core::spill::TempBudget>) -> io::Result<Self> {
        Ok(Self {
            pattern,
            counts: SortedRunCounts::new_budgeted(MAX_BUFFERED_SURFACES, MAX_SPILL_RUNS, budget)?,
            failure: None,
        })
    }

    fn finish(
        self,
        budget: Arc<lingrep_core::spill::TempBudget>,
    ) -> io::Result<Arc<PagedStore<SurfaceEntry>>> {
        let Self {
            mut counts,
            failure,
            ..
        } = self;
        if let Some(error) = failure {
            return Err(error);
        }
        let mut ordered =
            SortedRunCounts::new_budgeted(MAX_BUFFERED_SURFACES, MAX_SPILL_RUNS, budget.clone())?;
        counts.visit_sorted(|form, count| {
            ordered.add(
                SurfaceOrder {
                    count,
                    form: form.clone(),
                },
                1,
            )
        })?;
        drop(counts);

        let mut writer = PagedStoreWriter::new(budget)?;
        ordered.visit_sorted(|entry, _| {
            writer.push(&SurfaceEntry {
                form: entry.form.clone(),
                count: u64::try_from(entry.count)
                    .map_err(|_| io::Error::other("surface count exceeds u64"))?,
            })
        })?;
        drop(ordered);
        writer.finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SurfaceOrder {
    count: usize,
    form: String,
}

impl PartialOrd for SurfaceOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SurfaceOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .count
            .cmp(&self.count)
            .then_with(|| self.form.as_bytes().cmp(other.form.as_bytes()))
    }
}

fn send_failure(
    tx: &Sender<ExplorerMessage>,
    request_id: RequestId,
    pattern_id: PatternId,
    error: io::Error,
) {
    let _ = tx.send(ExplorerMessage::SurfaceFailed {
        request_id,
        pattern_id,
        code: (error.kind() == io::ErrorKind::StorageFull).then_some(ErrorCode::CacheError),
        error: error.to_string(),
    });
}

impl MatchSink for SurfaceSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        // Scoped to one pattern; the rest of the catalog's matches are
        // discarded. A match contributes one record however many token forms
        // it tallies — the unit is the match, not the token.
        let mut records_emitted = 0usize;
        for matched in matches
            .iter()
            .filter(|matched| matched.pattern.as_str() == self.pattern)
        {
            for token_index in matched.start..matched.end {
                let text = matched_surface(sentence, token_index..token_index + 1, location)
                    .map_err(ExecutionError::from)?;
                if let Err(error) = self.counts.add(text, 1) {
                    let message = error.to_string();
                    self.failure = Some(error);
                    return Err(ExecutionError::Sink(message));
                }
            }
            records_emitted += 1;
        }
        Ok(SinkControl::Continue { records_emitted })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_entries_are_count_descending_then_byte_ordered() {
        let budget = Arc::new(lingrep_core::spill::TempBudget::new(u64::MAX));
        let mut sink = SurfaceSink::new("p".into(), budget.clone()).unwrap();
        sink.counts.add("low".into(), 2).unwrap();
        sink.counts.add("high".into(), 7).unwrap();
        sink.counts.add("middle".into(), 5).unwrap();
        let store = sink.finish(budget).unwrap();
        assert_eq!(
            store.read_page(0).unwrap(),
            [
                SurfaceEntry {
                    form: "high".into(),
                    count: 7
                },
                SurfaceEntry {
                    form: "middle".into(),
                    count: 5
                },
                SurfaceEntry {
                    form: "low".into(),
                    count: 2
                }
            ]
        );
    }

    #[test]
    fn high_cardinality_surface_store_is_exact_ordered_and_releases_shared_budget() {
        let budget = Arc::new(lingrep_core::spill::TempBudget::new(u64::MAX));
        let mut sink = SurfaceSink::new("p".into(), budget.clone()).unwrap();
        let unique = 5_000;
        for index in 0..unique {
            sink.counts
                .add(format!("surface-{index:05}"), index % 7 + 1)
                .unwrap();
        }
        let store = sink.finish(budget.clone()).unwrap();
        assert_eq!(store.len(), unique as u64);
        let mut all = Vec::with_capacity(unique);
        for page in 0..unique.div_ceil(crate::tui::paged_store::DEFAULT_PAGE_ROWS) {
            all.extend(store.read_page(page as u64).unwrap());
        }
        assert_eq!(all.len(), unique);
        assert!(all.windows(2).all(|pair| {
            pair[0].count > pair[1].count
                || (pair[0].count == pair[1].count
                    && pair[0].form.as_bytes() <= pair[1].form.as_bytes())
        }));
        assert!(
            budget.peak_used_bytes() > budget.used_bytes(),
            "external aggregation phases and final paged data must share one high-water mark"
        );
        drop(store);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[test]
    fn temporary_exhaustion_is_a_typed_cache_failure_without_completion() {
        let (tx, rx) = crossbeam_channel::unbounded();
        send_failure(
            &tx,
            RequestId(5),
            PatternId(6),
            io::Error::new(io::ErrorKind::StorageFull, "temporary budget exhausted"),
        );
        let messages = rx.try_iter().collect::<Vec<_>>();
        assert!(matches!(
            messages.as_slice(),
            [ExplorerMessage::SurfaceFailed {
                request_id: RequestId(5),
                pattern_id: PatternId(6),
                code: Some(ErrorCode::CacheError),
                ..
            }]
        ));
    }

    #[test]
    fn new_surface_worker_emits_failure_not_completion() {
        let (directory, execution) = crate::interactive_execution::direct_test_execution("猫。\n");
        drop(directory);
        let worker = SurfaceWorker {
            request_id: RequestId(9),
            pattern_id: PatternId(4),
            pattern_name: "p".into(),
            matcher: Arc::new(
                crate::tui::compile_catalog_source("surface.lx", "p := [text=猫].")
                    .expect("compile fixture"),
            ),
            execution,
            cancel: CancellationFlag::new(),
        };
        let (tx, rx) = crossbeam_channel::unbounded();

        worker.run(tx);
        let messages = rx.try_iter().collect::<Vec<_>>();

        assert!(messages.iter().any(|message| matches!(
            message,
            ExplorerMessage::SurfaceFailed {
                request_id: RequestId(9),
                ..
            }
        )));
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message, ExplorerMessage::SurfaceFinished { .. }))
        );
    }
}
