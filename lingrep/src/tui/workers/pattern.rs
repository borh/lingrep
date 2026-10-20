use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::tui::types::ErrorCode;
use crossbeam_channel::Sender;
use lingrep_core::execution::{
    CancellationFlag, ExecutionError, MatchSink, RunOutcome, SentenceLocation, SinkControl,
};
use lingrep_core::fsm::Match;
#[cfg(test)]
use lingrep_core::token_match::TokenInfo;

use super::TaskHandle;
use crate::interactive_execution::InteractiveExecution;
use crate::tui::message::ExplorerMessage;
use crate::tui::types::RequestId;

pub(crate) struct PatternWorker {
    request_id: RequestId,
    pattern_text: String,
    execution: Arc<InteractiveExecution>,
    cancel: CancellationFlag,
}

impl PatternWorker {
    pub(crate) fn spawn(
        request_id: RequestId,
        pattern_text: String,
        execution: Arc<InteractiveExecution>,
        tx: Sender<ExplorerMessage>,
    ) -> TaskHandle {
        TaskHandle::spawn(request_id, move |cancel| {
            Self {
                request_id,
                pattern_text,
                execution,
                cancel,
            }
            .run(tx);
        })
    }

    pub(crate) fn run(self, tx: Sender<ExplorerMessage>) {
        let matcher = match crate::tui::compile_catalog_source("tui-worker.lx", &self.pattern_text)
        {
            Ok(matcher) => matcher,
            Err(error) => {
                let _ = tx.send(ExplorerMessage::PatternFailed {
                    request_id: self.request_id,
                    code: Some(ErrorCode::PatternError),
                    error,
                });
                return;
            }
        };
        let mut sink = PatternSink {
            request_id: self.request_id,
            tx: tx.clone(),
            batch: BTreeMap::new(),
        };
        let result =
            self.execution
                .execute(&matcher, &self.cancel, &mut sink, &mut |progress, _| {
                    let _ = tx.send(ExplorerMessage::PatternProgress {
                        request_id: self.request_id,
                        current: progress.sentences_matched as usize,
                        total: 0,
                    });
                });
        match result {
            Ok(RunOutcome::Completed(_)) => {
                let _ = tx.send(ExplorerMessage::PatternFinished {
                    request_id: self.request_id,
                });
            }
            Ok(RunOutcome::Cancelled(_)) => {
                let _ = tx.send(ExplorerMessage::PatternFailed {
                    request_id: self.request_id,
                    code: Some(ErrorCode::Cancelled),
                    error: "Processing cancelled".into(),
                });
            }
            Err(error) => {
                let _ = tx.send(ExplorerMessage::PatternFailed {
                    request_id: self.request_id,
                    code: None,
                    error: error.to_string(),
                });
            }
        }
    }
}

struct PatternSink {
    request_id: RequestId,
    tx: Sender<ExplorerMessage>,
    batch: BTreeMap<String, u64>,
}

impl MatchSink for PatternSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        for pattern in matches
            .iter()
            .map(|matched| matched.pattern.to_string())
            .collect::<BTreeSet<_>>()
        {
            *self.batch.entry(pattern).or_insert(0) += 1;
        }
        // Every match reaches the tally. Collapsing repeats of one pattern
        // within a sentence is aggregation, not discard.
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.tx
            .send(ExplorerMessage::PatternDelta {
                request_id: self.request_id,
                delta_counts: std::mem::take(&mut self.batch).into_iter().collect(),
            })
            .map_err(|_| ExecutionError::Sink("explorer channel closed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_worker(request_id: RequestId) -> (tempfile::TempDir, PatternWorker) {
        let (directory, execution) = crate::interactive_execution::direct_test_execution("猫。\n");
        (
            directory,
            PatternWorker {
                request_id,
                pattern_text: "p := [text=猫].".into(),
                execution,
                cancel: CancellationFlag::new(),
            },
        )
    }

    #[test]
    fn new_pattern_worker_emits_data_before_one_terminal_completion() {
        let (_directory, worker) = direct_worker(RequestId(12));
        let (tx, rx) = crossbeam_channel::unbounded();

        worker.run(tx);
        let messages = rx.try_iter().collect::<Vec<_>>();

        let completion = messages
            .iter()
            .position(|message| {
                matches!(
                    message,
                    ExplorerMessage::PatternFinished {
                        request_id: RequestId(12)
                    }
                )
            })
            .expect("terminal completion");
        assert!(messages[..completion].iter().any(|message| matches!(
            message,
            ExplorerMessage::PatternDelta {
                request_id: RequestId(12),
                delta_counts,
            } if delta_counts == &[("p".to_string(), 1)]
        )));
        assert_eq!(
            messages
                .iter()
                .filter(|message| matches!(message, ExplorerMessage::PatternFinished { .. }))
                .count(),
            1
        );
        assert_eq!(completion + 1, messages.len());
    }

    #[test]
    fn new_pattern_worker_cancellation_is_terminal_without_completion() {
        let (_directory, worker) = direct_worker(RequestId(13));
        worker.cancel.cancel();
        let (tx, rx) = crossbeam_channel::unbounded();

        worker.run(tx);
        let messages = rx.try_iter().collect::<Vec<_>>();

        assert!(messages.iter().any(|message| matches!(
            message,
            ExplorerMessage::PatternFailed {
                request_id: RequestId(13),
                code: Some(ErrorCode::Cancelled),
                ..
            }
        )));
        assert!(!messages.iter().any(|message| matches!(
            message,
            ExplorerMessage::PatternDelta { .. }
                | ExplorerMessage::PatternProgress { .. }
                | ExplorerMessage::PatternFinished { .. }
        )));
    }

    #[test]
    fn pattern_sink_counts_one_hit_per_pattern_per_sentence() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut sink = PatternSink {
            request_id: RequestId(4),
            tx,
            batch: BTreeMap::new(),
        };
        let matches = [
            Match {
                pattern: "p".into(),
                start: 0,
                end: 1,
                captures: Box::new([]),
                graph_bindings: Box::default(),
                lexical: None,
            },
            Match {
                pattern: "p".into(),
                start: 1,
                end: 2,
                captures: Box::new([]),
                graph_bindings: Box::default(),
                lexical: None,
            },
        ];
        sink.push(
            &SentenceLocation {
                source_metadata: None,
                corpus_sentence_ordinal: 0,
                document_id: lingrep_core::corpus::DocumentId::new(0),
                document_ordinal: 0,
                document_sentence_ordinal: 0,
                source_name: "fixture".into(),
            },
            &(&[] as &[TokenInfo]),
            &matches,
        )
        .unwrap();
        sink.finish_batch().unwrap();

        assert!(matches!(
            rx.try_recv(),
            Ok(ExplorerMessage::PatternDelta {
                delta_counts,
                ..
            }) if delta_counts == [("p".to_string(), 1)]
        ));
    }
}
