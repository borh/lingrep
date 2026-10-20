use std::io;
use std::sync::Arc;

use crate::tui::cache::KwicLine;
use crate::tui::types::ErrorCode;
use crossbeam_channel::Sender;
use lingrep_core::execution::{
    CancellationFlag, ExecutionError, MatchSink, RunOutcome, SentenceLocation, SinkControl,
};
use lingrep_core::fsm::{CompiledCatalog, Match};

use super::TaskHandle;
use crate::direct_source::matched_surface;
use crate::interactive_execution::InteractiveExecution;
use crate::tui::message::ExplorerMessage;
use crate::tui::paged_store::PagedStoreWriter;
use crate::tui::types::{PatternId, RequestId};

const KWIC_CONTEXT_SIZE: usize = 25;

pub(crate) struct KwicWorker {
    request_id: RequestId,
    pattern_id: PatternId,
    pattern_name: String,
    matcher: Arc<CompiledCatalog>,
    execution: Arc<InteractiveExecution>,
    cancel: CancellationFlag,
}

impl KwicWorker {
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
        let mut sink = match PagedStoreWriter::new(self.execution.temp_budget()) {
            Ok(writer) => KwicSink {
                pattern_name: self.pattern_name,
                writer,
                failure: None,
            },
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
            Ok(RunOutcome::Completed(_)) => match sink.writer.finish() {
                Ok(store) => {
                    let _ = tx.send(ExplorerMessage::KwicFinished {
                        request_id: self.request_id,
                        pattern_id: self.pattern_id,
                        store,
                    });
                }
                Err(error) => send_failure(&tx, self.request_id, self.pattern_id, error),
            },
            Ok(RunOutcome::Cancelled(_)) => {
                let _ = tx.send(ExplorerMessage::KwicFailed {
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
    failure: Option<io::Error>,
}

impl MatchSink for KwicSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
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
                pre: matched_surface(sentence, before, location).map_err(ExecutionError::from)?,
                key: matched_surface(sentence, matched.start..matched.end, location)
                    .map_err(ExecutionError::from)?,
                post: matched_surface(sentence, after, location).map_err(ExecutionError::from)?,
                match_start: matched.start,
                match_end: matched.end,
            };
            if let Err(error) = self.writer.push(&line) {
                let message = error.to_string();
                self.failure = Some(error);
                return Err(ExecutionError::Sink(message));
            }
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
            matcher: Arc::new(
                crate::tui::compile_catalog_source("kwic.lx", "p := [text=猫].")
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
}
