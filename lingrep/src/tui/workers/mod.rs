use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use lingrep_core::execution::CancellationFlag;

use crate::tui::cache::{KwicLine, SurfaceEntry};
use crate::tui::message::ExplorerMessage;
use crate::tui::paged_store::{DEFAULT_PAGE_ROWS, PagedStore};
use crate::tui::types::{PatternId, RequestId};
use crate::tui::watcher::PatternWatcher;

mod kwic;
mod pattern;
mod surface;

pub(crate) use kwic::KwicWorker;
pub(crate) use pattern::PatternWorker;
pub(crate) use surface::SurfaceWorker;

pub(crate) struct TaskHandle {
    pub request_id: RequestId,
    pub cancel: CancellationFlag,
}

impl TaskHandle {
    fn spawn(request_id: RequestId, run: impl FnOnce(CancellationFlag) + Send + 'static) -> Self {
        let cancel = CancellationFlag::new();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || run(worker_cancel));
        Self { request_id, cancel }
    }
}

pub(crate) enum IoJob {
    ReloadPatternFile {
        path: PathBuf,
        reload_count: u64,
    },
    LoadSurfaceWindow {
        request_id: RequestId,
        pattern_id: PatternId,
        page_request_id: u64,
        store: Arc<PagedStore<SurfaceEntry>>,
        start: usize,
    },
    LoadKwicWindow {
        request_id: RequestId,
        pattern_id: PatternId,
        page_request_id: u64,
        store: Arc<PagedStore<KwicLine>>,
        start: usize,
    },
}

#[derive(Default)]
pub(crate) struct TaskHandles {
    pub pattern: Option<TaskHandle>,
    pub surface: Option<TaskHandle>,
    pub kwic: Option<TaskHandle>,
    io_tx: Option<Sender<IoJob>>,
}

impl TaskHandles {
    /// Dispatches a blocking filesystem or paged-store read to a single
    /// dedicated background I/O thread so the UI loop never blocks on disk I/O
    /// and rapid scrolling never spawns unbounded threads.
    pub(crate) fn dispatch_io(&mut self, job: IoJob, reply_tx: &Sender<ExplorerMessage>) {
        let io_tx = self.io_tx.get_or_insert_with(|| {
            let (tx, rx) = crossbeam_channel::unbounded::<IoJob>();
            let reply_tx = reply_tx.clone();
            std::thread::spawn(move || {
                while let Ok(job) = rx.recv() {
                    let message = execute_io_job(job);
                    if reply_tx.send(message).is_err() {
                        break;
                    }
                }
            });
            tx
        });
        let _ = io_tx.send(job);
    }
}

fn execute_io_job(job: IoJob) -> ExplorerMessage {
    match job {
        IoJob::ReloadPatternFile { path, reload_count } => {
            let result =
                lingrep_core::text_input::read_utf8_file(&path).map_err(|error| error.to_string());
            ExplorerMessage::PatternFileLoaded {
                path,
                reload_count,
                result,
            }
        }
        IoJob::LoadSurfaceWindow {
            request_id,
            pattern_id,
            page_request_id,
            store,
            start,
        } => {
            let result = store
                .read_window(start as u64, DEFAULT_PAGE_ROWS)
                .map_err(|error| error.to_string());
            ExplorerMessage::SurfaceWindowLoaded {
                request_id,
                pattern_id,
                page_request_id,
                window_start: start,
                result,
            }
        }
        IoJob::LoadKwicWindow {
            request_id,
            pattern_id,
            page_request_id,
            store,
            start,
        } => {
            let result = store
                .read_window(start as u64, DEFAULT_PAGE_ROWS)
                .map_err(|error| error.to_string());
            ExplorerMessage::KwicWindowLoaded {
                request_id,
                pattern_id,
                page_request_id,
                window_start: start,
                result,
            }
        }
    }
}

/// RAII handle for the background pattern-file watcher thread; cancels it on
/// drop so every exit path of `loop_::run` stops polling cleanly.
pub(crate) struct WatcherGuard {
    cancel: CancellationFlag,
}

impl WatcherGuard {
    pub(crate) fn spawn(
        path: PathBuf,
        poll_interval: Duration,
        tx: Sender<ExplorerMessage>,
    ) -> Self {
        let cancel = CancellationFlag::new();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            if !path.exists() {
                return;
            }
            let mut watcher = PatternWatcher::new(path);
            while !worker_cancel.is_cancelled() {
                std::thread::sleep(poll_interval);
                if worker_cancel.is_cancelled() {
                    break;
                }
                if let Some(change) = watcher.check()
                    && tx
                        .send(ExplorerMessage::PatternFileChanged(change))
                        .is_err()
                {
                    break;
                }
            }
        });
        Self { cancel }
    }
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
