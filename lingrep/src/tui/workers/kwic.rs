use std::io;
use std::sync::Arc;

use crate::tui::cache::{KwicLine, PagedResult};
use crate::tui::types::ErrorCode;
use crossbeam_channel::Sender;
use lingrep_core::execution::{
    CancellationFlag, ExecutionError, MatchSink, RunOutcome, SentenceLocation, SinkControl,
};
use lingrep_core::fsm::Match;

use super::{RenderSinkError, TaskHandle};
use crate::direct_source::displayed_text;
use crate::interactive_execution::InteractiveExecution;
use crate::tui::message::ExplorerMessage;
use crate::tui::paged_store::PagedStoreWriter;
use crate::tui::types::{PatternId, RequestId};

const KWIC_CONTEXT_SIZE: usize = 25;

pub(crate) struct KwicWorker {
    request_id: RequestId,
    pattern_id: PatternId,
    pattern_name: String,
    pattern_text: String,
    execution: Arc<InteractiveExecution>,
    cancel: CancellationFlag,
}

impl KwicWorker {
    pub(crate) fn spawn(
        request_id: RequestId,
        pattern_id: PatternId,
        pattern_name: String,
        pattern_text: String,
        execution: Arc<InteractiveExecution>,
        tx: Sender<ExplorerMessage>,
    ) -> TaskHandle {
        TaskHandle::spawn(request_id, move |cancel| {
            Self {
                request_id,
                pattern_id,
                pattern_name,
                pattern_text,
                execution,
                cancel,
            }
            .run(tx);
        })
    }

    pub(crate) fn run(self, tx: Sender<ExplorerMessage>) {
        let matcher = match crate::tui::compile_catalog_source("tui.lx", &self.pattern_text) {
            Ok(matcher) => matcher,
            Err(error) => {
                let _ = tx.send(ExplorerMessage::KwicFailed {
                    request_id: self.request_id,
                    pattern_id: self.pattern_id,
                    code: Some(ErrorCode::PatternError),
                    error,
                });
                return;
            }
        };
        let mut sink = match PagedStoreWriter::new(self.execution.temp_budget()) {
            Ok(writer) => KwicSink {
                pattern_name: self.pattern_name,
                writer,
            },
            Err(error) => {
                send_failure(&tx, self.request_id, self.pattern_id, error);
                return;
            }
        };
        let result =
            self.execution
                .execute(&matcher, &self.cancel, &mut sink, &mut |_progress, _| {});
        match result {
            Ok(RunOutcome::Completed(_)) => {
                match sink.writer.finish().and_then(PagedResult::load_initial) {
                    Ok(result) => {
                        let _ = tx.send(ExplorerMessage::KwicFinished {
                            request_id: self.request_id,
                            pattern_id: self.pattern_id,
                            result,
                        });
                    }
                    Err(error) => send_failure(&tx, self.request_id, self.pattern_id, error),
                }
            }
            Ok(RunOutcome::Cancelled(_)) => {
                let _ = tx.send(ExplorerMessage::KwicFailed {
                    request_id: self.request_id,
                    pattern_id: self.pattern_id,
                    code: Some(ErrorCode::Cancelled),
                    error: "Processing cancelled".into(),
                });
            }
            Err(crate::error::CliError::Execution(ExecutionError::SinkIo(error))) => {
                send_failure(&tx, self.request_id, self.pattern_id, error);
            }
            Err(error) => {
                let _ = tx.send(ExplorerMessage::KwicFailed {
                    request_id: self.request_id,
                    pattern_id: self.pattern_id,
                    code: None,
                    error: error.to_string(),
                });
            }
        }
    }
}

struct KwicSink {
    pattern_name: String,
    writer: PagedStoreWriter<KwicLine>,
}

impl MatchSink for KwicSink {
    type Error = RenderSinkError;

    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, RenderSinkError> {
        // Scoped to one pattern, so matches of every other pattern in the
        // catalog are discarded rather than published.
        let mut records_emitted = 0usize;
        for matched in matches
            .iter()
            .filter(|matched| matched.pattern.as_str() == self.pattern_name)
        {
            let before = matched.start.saturating_sub(KWIC_CONTEXT_SIZE)..matched.start;
            let after = matched.end..(matched.end + KWIC_CONTEXT_SIZE).min(sentence.len());
            let line = KwicLine {
                pre: displayed_text(sentence, before, location)
                    .map_err(RenderSinkError::Display)?,
                key: displayed_text(sentence, matched.start..matched.end, location)
                    .map_err(RenderSinkError::Display)?,
                post: displayed_text(sentence, after, location)
                    .map_err(RenderSinkError::Display)?,
                match_start: matched.start,
                match_end: matched.end,
            };
            self.writer.push(&line).map_err(RenderSinkError::Storage)?;
            records_emitted += 1;
        }
        Ok(SinkControl::Continue { records_emitted })
    }
}

fn send_failure(
    tx: &Sender<ExplorerMessage>,
    request_id: RequestId,
    pattern_id: PatternId,
    error: io::Error,
) {
    let _ = tx.send(ExplorerMessage::KwicFailed {
        request_id,
        pattern_id,
        code: (error.kind() == io::ErrorKind::StorageFull).then_some(ErrorCode::CacheError),
        error: error.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_kwic_worker_emits_failure_not_completion() {
        let (directory, execution) = crate::interactive_execution::direct_test_execution("猫。\n");
        drop(directory);
        let worker = KwicWorker {
            request_id: RequestId(9),
            pattern_id: PatternId(4),
            pattern_name: "p".into(),
            pattern_text: "p := [text=猫].".into(),
            execution,
            cancel: CancellationFlag::new(),
        };
        let (tx, rx) = crossbeam_channel::unbounded();

        worker.run(tx);
        let messages = rx.try_iter().collect::<Vec<_>>();

        assert!(messages.iter().any(|message| matches!(
            message,
            ExplorerMessage::KwicFailed {
                request_id: RequestId(9),
                ..
            }
        )));
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message, ExplorerMessage::KwicFinished { .. }))
        );
    }

    #[test]
    fn kwic_scan_over_a_conllu_root_renders_token_surfaces() {
        let worker = KwicWorker {
            request_id: RequestId(10),
            pattern_id: PatternId(5),
            pattern_name: "subject".into(),
            pattern_text: "subject := [text=子ども & dep.rel=nsubj].".into(),
            execution: crate::interactive_execution::conllu_test_execution(),
            cancel: CancellationFlag::new(),
        };
        let (tx, rx) = crossbeam_channel::unbounded();

        worker.run(tx);
        let messages = rx.try_iter().collect::<Vec<_>>();

        let [ExplorerMessage::KwicFinished { result, .. }] = messages.as_slice() else {
            panic!("expected one completed KWIC result, got {messages:?}");
        };
        let lines = result
            .window
            .iter()
            .map(|line| (line.pre.as_str(), line.key.as_str(), line.post.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(lines, [("新聞を", "子ども", "が読む")]);
    }
}
