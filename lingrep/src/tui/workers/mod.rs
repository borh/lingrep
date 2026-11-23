use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    cancel: CancellationFlag,
    thread: JoinHandle<()>,
}

impl TaskHandle {
    fn spawn(request_id: RequestId, run: impl FnOnce(CancellationFlag) + Send + 'static) -> Self {
        let cancel = CancellationFlag::new();
        let worker_cancel = cancel.clone();
        let thread = std::thread::spawn(move || run(worker_cancel));
        Self {
            request_id,
            cancel,
            thread,
        }
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
    /// Cancelled workers that may still be unwinding.
    cancelled: Vec<JoinHandle<()>>,
    io_tx: Option<Sender<IoJob>>,
    io_thread: Option<JoinHandle<()>>,
}

impl TaskHandles {
    /// Cancels a worker and keeps its thread for [`Self::shutdown`].
    pub(crate) fn cancel(&mut self, handle: TaskHandle) {
        handle.cancel.cancel();
        self.cancelled.retain(|thread| !thread.is_finished());
        self.cancelled.push(handle.thread);
    }

    pub(crate) fn cancel_all(&mut self) {
        for handle in [self.pattern.take(), self.surface.take(), self.kwic.take()]
            .into_iter()
            .flatten()
        {
            self.cancel(handle);
        }
    }

    /// Cancels every worker and waits up to `wait` for the threads to end.
    ///
    /// A worker owns its scan's temporary files until it ends, and a process
    /// exit does not unwind a running thread, so a thread still running after
    /// `wait` leaves its files behind. A worker blocked on sending to the
    /// explorer channel ends only once the channel's receiver is dropped.
    pub(crate) fn shutdown(&mut self, wait: Duration) {
        self.cancel_all();
        self.io_tx = None;
        let mut threads = std::mem::take(&mut self.cancelled);
        threads.extend(self.io_thread.take());
        let deadline = Instant::now() + wait;
        while threads.iter().any(|thread| !thread.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        for thread in threads {
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }

    /// Dispatches a blocking filesystem or paged-store read to a single
    /// dedicated background I/O thread so the UI loop never blocks on disk I/O
    /// and rapid scrolling never spawns unbounded threads.
    pub(crate) fn dispatch_io(&mut self, job: IoJob, reply_tx: &Sender<ExplorerMessage>) {
        let io_tx = self.io_tx.get_or_insert_with(|| {
            let (tx, rx) = crossbeam_channel::unbounded::<IoJob>();
            let reply_tx = reply_tx.clone();
            self.io_thread = Some(std::thread::spawn(move || {
                while let Ok(job) = rx.recv() {
                    let message = execute_io_job(job);
                    if reply_tx.send(message).is_err() {
                        break;
                    }
                }
            }));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(directory: &std::path::Path) -> Vec<PathBuf> {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    #[test]
    fn quitting_during_detail_scans_leaves_no_temporary_file() {
        // The scans' files are told apart from other tests' by a temporary
        // directory of their own, which is per process.
        let temporary = tempfile::tempdir().unwrap();
        let module = module_path!().split_once("::").unwrap().1;
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env("TMPDIR", temporary.path())
            .args(["--ignored", "--exact"])
            .arg(format!("{module}::exercise_quit_during_detail_scans"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "subprocess with a temporary directory of its own"]
    fn exercise_quit_during_detail_scans() {
        let (corpus, execution) =
            crate::interactive_execution::direct_test_execution(&"猫。\n\n".repeat(200_000));
        let temporary = std::env::temp_dir();
        let before = entries(&temporary);
        let (tx, rx) = crossbeam_channel::bounded(64);
        let mut handles = TaskHandles {
            surface: Some(SurfaceWorker::spawn(
                RequestId(1),
                PatternId(1),
                "p".into(),
                "p := [text=犬].".into(),
                Arc::clone(&execution),
                tx.clone(),
            )),
            kwic: Some(KwicWorker::spawn(
                RequestId(2),
                PatternId(1),
                "p".into(),
                "p := [text=犬].".into(),
                Arc::clone(&execution),
                tx.clone(),
            )),
            ..TaskHandles::default()
        };
        // A spill directory for the surface tally, and the KWIC store's data
        // and index files.
        let started = Instant::now();
        while entries(&temporary).len() < before.len() + 3 {
            assert!(started.elapsed() < Duration::from_secs(60));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(rx.try_recv().is_err(), "the scans are still running");

        drop(rx);
        let quit = Instant::now();
        handles.shutdown(Duration::from_secs(60));
        assert_eq!(entries(&temporary), before);
        assert!(
            quit.elapsed() < Duration::from_secs(2),
            "{:?}",
            quit.elapsed()
        );
        drop(corpus);
    }
}
