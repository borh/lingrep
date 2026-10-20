//! Atomic arbitration between cooperative cancellation and successful completion.

use std::io::{self, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI32, Ordering},
};

use lingrep_core::execution::CancellationFlag;

const RUNNING: i32 = 0;
const COMPLETION_CLAIMED: i32 = -1;

pub struct PresentationProgress<W> {
    inner: Arc<Mutex<PresentationProgressInner<W>>>,
}

struct PresentationProgressInner<W> {
    writer: W,
    disabled: bool,
    error: Option<io::Error>,
}

impl<W> Clone for PresentationProgress<W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<W: Write> PresentationProgress<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PresentationProgressInner {
                writer,
                disabled: false,
                error: None,
            })),
        }
    }

    pub fn write_line(&self, line: &str) {
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state
                    .error
                    .get_or_insert_with(|| io::Error::other("progress writer lock poisoned"));
                return;
            }
        };
        if state.disabled {
            return;
        }
        let mut framed = String::with_capacity(line.len() + 1);
        framed.push_str(line);
        framed.push('\n');
        match state.writer.write_all(framed.as_bytes()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                state.disabled = true;
            }
            Err(error) => {
                state.error.get_or_insert(error);
            }
        }
    }

    pub fn finish(&self) -> io::Result<()> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl PresentationProgress<io::Stderr> {
    pub fn stderr() -> Self {
        Self::new(io::stderr())
    }
}

/// One run-scoped terminal state shared by signal handling and command output.
///
/// The first transition from `RUNNING` wins. A winning signal also raises the
/// execution cancellation flag; signals observed after completion are ignored.
#[derive(Clone)]
pub struct TerminalArbiter {
    state: Arc<AtomicI32>,
    cancellation: CancellationFlag,
}

impl Default for TerminalArbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalArbiter {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicI32::new(RUNNING)),
            cancellation: CancellationFlag::new(),
        }
    }

    pub fn cancellation(&self) -> &CancellationFlag {
        &self.cancellation
    }

    /// Claim cancellation for `signal`. Returns whether this signal won.
    pub fn try_cancel(&self, signal: i32) -> bool {
        debug_assert!(signal > 0, "process signals must be positive");
        if self
            .state
            .compare_exchange(RUNNING, signal, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.cancellation.cancel();
            true
        } else {
            false
        }
    }

    /// Claim successful completion. Returns whether completion won.
    pub fn try_claim_completion(&self) -> bool {
        self.state
            .compare_exchange(
                RUNNING,
                COMPLETION_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn winning_signal(&self) -> Option<i32> {
        let state = self.state.load(Ordering::Acquire);
        (state > RUNNING).then_some(state)
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct BreakAfterOne {
        writes: Arc<AtomicUsize>,
    }

    impl io::Write for BreakAfterOne {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let call = self.writes.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Ok(bytes.len())
            } else {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn progress_clones_preserve_the_first_error_and_finish_consumes_it() {
        struct FailingWriter(usize);
        impl io::Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Err(io::Error::other(format!("failure {}", self.0)))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let progress = PresentationProgress::new(FailingWriter(0));
        let clone = progress.clone();
        progress.write_line("first");
        clone.write_line("second");
        assert_eq!(clone.finish().unwrap_err().to_string(), "failure 1");
        assert!(progress.finish().is_ok());
        clone.write_line("third");
        assert_eq!(progress.finish().unwrap_err().to_string(), "failure 3");
    }

    #[test]
    fn a_panicking_writer_reports_poison_on_the_next_write() {
        struct PanickingWriter;
        impl io::Write for PanickingWriter {
            #[expect(
                clippy::panic_in_result_fn,
                reason = "inject a writer panic to characterize lock poisoning"
            )]
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                panic!("writer panic")
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let progress = PresentationProgress::new(PanickingWriter);
        assert!(std::panic::catch_unwind(|| progress.write_line("panic")).is_err());
        assert!(progress.finish().is_ok());
        progress.write_line("poisoned");
        assert_eq!(
            progress.finish().unwrap_err().to_string(),
            "progress writer lock poisoned"
        );
        assert!(progress.finish().is_ok());
    }

    #[test]
    fn concurrent_progress_clones_write_complete_lines() {
        #[derive(Clone)]
        struct SharedWriter(Arc<Mutex<Vec<u8>>>);
        impl io::Write for SharedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                // Split writes deliberately: only the progress lock protects a line.
                self.0.lock().unwrap().push(bytes[0]);
                std::thread::yield_now();
                Ok(1)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let progress = PresentationProgress::new(SharedWriter(Arc::clone(&bytes)));
        std::thread::scope(|scope| {
            for index in 0..8 {
                let progress = progress.clone();
                scope.spawn(move || {
                    for _ in 0..20 {
                        progress.write_line(&format!("worker {index}"));
                    }
                });
            }
        });
        progress.finish().unwrap();
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let mut lines = output.lines().collect::<Vec<_>>();
        lines.sort_unstable();
        let mut expected = (0..8)
            .flat_map(|index| vec![format!("worker {index}"); 20])
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(lines, expected);
    }

    #[test]
    fn first_signal_atomically_wins_over_completion_and_later_signals() {
        let arbiter = TerminalArbiter::new();

        assert!(arbiter.try_cancel(signal_hook::consts::SIGINT));
        assert!(!arbiter.try_claim_completion());
        assert!(!arbiter.try_cancel(signal_hook::consts::SIGTERM));
        assert_eq!(arbiter.winning_signal(), Some(signal_hook::consts::SIGINT));
        assert!(arbiter.cancellation().is_cancelled());
    }

    #[test]
    fn completion_atomically_wins_over_every_later_signal() {
        let arbiter = TerminalArbiter::new();

        assert!(arbiter.try_claim_completion());
        assert!(!arbiter.try_cancel(signal_hook::consts::SIGINT));
        assert!(!arbiter.try_cancel(signal_hook::consts::SIGTERM));
        assert_eq!(arbiter.winning_signal(), None);
        assert!(!arbiter.cancellation().is_cancelled());
    }

    #[test]
    fn closed_progress_writer_disables_itself_without_becoming_an_error() {
        let writes = Arc::new(AtomicUsize::new(0));
        let progress = PresentationProgress::new(BreakAfterOne {
            writes: Arc::clone(&writes),
        });

        progress.write_line("first");
        progress.write_line("second");
        progress.write_line("third");

        assert_eq!(writes.load(Ordering::SeqCst), 2);
        assert!(progress.finish().is_ok());
    }
}
