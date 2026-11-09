//! Atomic arbitration between cooperative cancellation and successful completion.

use std::io::{self, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI32, Ordering},
};
use std::time::{Duration, Instant};

use lingrep_core::execution::CancellationFlag;

use crate::presentation::{
    OutputMode, PROGRESS_LINE_INTERVAL, ProgressDisplay, ProgressMode, ResultDelivery,
    TRANSIENT_REFRESH_INTERVAL, TerminalState,
};

const RUNNING: i32 = 0;
const COMPLETION_CLAIMED: i32 = -1;

/// Erases the current terminal line and returns the cursor to its start.
const ERASE_LINE: &str = "\r\x1b[2K";

/// The run's stderr, shared by its progress presenter and every diagnostic
/// written while that presenter may show a transient status line. One lock
/// orders status writes, erasure and diagnostics.
pub(crate) struct RunStderr<W: Write = io::Stderr> {
    state: Arc<Mutex<RunStderrState<W>>>,
}

struct RunStderrState<W: Write> {
    writer: W,
    status_visible: bool,
}

impl<W: Write> Clone for RunStderr<W> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<W: Write> std::fmt::Debug for RunStderr<W> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RunStderr")
    }
}

impl<W: Write> RunStderr<W> {
    pub fn new(writer: W) -> Self {
        Self {
            state: Arc::new(Mutex::new(RunStderrState {
                writer,
                status_visible: false,
            })),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, RunStderrState<W>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Attempts to erase a visible status line, then runs `write` and flushes.
    /// Only diagnostic write or flush failures are returned.
    pub fn diagnostic(&self, write: impl FnOnce(&mut W) -> io::Result<()>) -> io::Result<()> {
        let mut state = self.state();
        let _ = state.erase();
        write(&mut state.writer)?;
        state.writer.flush()
    }

    fn show_status(&self, text: &str) -> io::Result<()> {
        let mut state = self.state();
        state.status_visible = true;
        state.write(format!("{ERASE_LINE}{text}").as_bytes())
    }

    fn erase_status(&self) -> io::Result<()> {
        self.state().erase()
    }
}

impl<W: Write> RunStderrState<W> {
    fn erase(&mut self) -> io::Result<()> {
        if self.status_visible {
            self.write(ERASE_LINE.as_bytes())?;
            self.status_visible = false;
        }
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }
}

type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;
type Snapshot = Box<dyn FnOnce() -> String + Send>;

/// Run-owned optional progress on stderr. A failed write disables progress for
/// the rest of the run and never fails it.
pub(crate) struct PresentationProgress<W: Write> {
    inner: Arc<Mutex<PresentationProgressInner<W>>>,
}

struct PresentationProgressInner<W: Write> {
    stderr: RunStderr<W>,
    display: ProgressDisplay,
    clock: Clock,
    width: usize,
    last_shown: Option<Duration>,
    last_line: Option<String>,
    pending: Option<Snapshot>,
}

impl<W: Write> Clone for PresentationProgress<W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<W: Write> PresentationProgress<W> {
    fn with_clock(
        stderr: RunStderr<W>,
        display: ProgressDisplay,
        clock: Clock,
        width: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PresentationProgressInner {
                stderr,
                display,
                clock,
                width,
                last_shown: None,
                last_line: None,
                pending: None,
            })),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, PresentationProgressInner<W>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Submits the run's current counts to the display. `render` is called only
    /// when the display writes this snapshot.
    pub fn snapshot(&self, render: impl FnOnce() -> String + Send + 'static) {
        let mut state = self.state();
        let now = (state.clock)();
        let last_shown = state.last_shown;
        let due = |interval| last_shown.is_none_or(|shown| now.saturating_sub(shown) >= interval);
        match state.display {
            ProgressDisplay::Off => {}
            ProgressDisplay::Lines => {
                if due(PROGRESS_LINE_INTERVAL) {
                    state.pending = None;
                    state.last_shown = Some(now);
                    state.emit_snapshot_line(render());
                } else {
                    state.pending = Some(Box::new(render));
                }
            }
            ProgressDisplay::Transient { delay } => {
                if now >= delay && due(TRANSIENT_REFRESH_INTERVAL) {
                    state.last_shown = Some(now);
                    let line = render();
                    state.emit_transient(&line);
                }
            }
        }
    }

    /// Writes one status line that `--progress always` enables, such as the
    /// corpus that the run reads.
    pub fn write_line(&self, line: &str) {
        let mut state = self.state();
        if state.display != ProgressDisplay::Off {
            state.emit_line(line);
        }
    }

    /// Ends one stage of a run: writes the stage's latest unshown line, so the
    /// next stage's snapshots cannot replace it.
    pub fn end_stage(&self) {
        let mut state = self.state();
        if let Some(render) = state.pending.take() {
            state.emit_snapshot_line(render());
        }
    }

    /// Ends progress for the run: writes the latest unshown line, or erases the
    /// transient status line, before the run writes anything else.
    pub fn finish(&self) {
        let mut state = self.state();
        if let Some(render) = state.pending.take() {
            state.emit_snapshot_line(render());
        }
        state.erase_transient();
        state.display = ProgressDisplay::Off;
    }
}

impl<W: Write> PresentationProgressInner<W> {
    fn emit_snapshot_line(&mut self, line: String) {
        if self.last_line.as_ref() != Some(&line) {
            self.emit_line(&line);
            self.last_line = Some(line);
        }
    }

    fn emit_line(&mut self, line: &str) {
        let written = self.stderr.diagnostic(|writer| writeln!(writer, "{line}"));
        self.disable_on_error(written);
    }

    fn emit_transient(&mut self, line: &str) {
        let shown: String = line.chars().take(self.width.saturating_sub(1)).collect();
        let written = self.stderr.show_status(&shown);
        self.disable_on_error(written);
    }

    fn erase_transient(&mut self) {
        let written = self.stderr.erase_status();
        self.disable_on_error(written);
    }

    fn disable_on_error(&mut self, written: io::Result<()>) {
        if written.is_err() {
            self.display = ProgressDisplay::Off;
            self.pending = None;
        }
    }
}

impl<W: Write> Drop for PresentationProgressInner<W> {
    /// An error or cancellation can end a run without `finish`.
    fn drop(&mut self) {
        let _ = self.stderr.erase_status();
    }
}

impl PresentationProgress<io::Stderr> {
    /// Creates progress for one run on `stderr`. The display is chosen from
    /// `requested`, `output`, `delivery` and which streams are terminals.
    pub fn stderr(
        stderr: &RunStderr,
        requested: ProgressMode,
        output: OutputMode,
        delivery: ResultDelivery,
    ) -> Self {
        let display = requested.display(output, delivery, TerminalState::detect());
        let width = terminal_size::terminal_size().map_or(80, |(width, _)| usize::from(width.0));
        let started = Instant::now();
        Self::with_clock(
            stderr.clone(),
            display,
            Arc::new(move || started.elapsed()),
            width,
        )
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
    signals: Arc<tokio::sync::watch::Sender<u32>>,
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
            signals: Arc::new(tokio::sync::watch::channel(0).0),
        }
    }

    /// The number of `try_cancel` calls so far, updated on each call, whether
    /// or not the signal won.
    pub fn signal_count(&self) -> tokio::sync::watch::Receiver<u32> {
        self.signals.subscribe()
    }

    pub fn cancellation(&self) -> &CancellationFlag {
        &self.cancellation
    }

    /// Claim cancellation for `signal`. Returns whether this signal won.
    pub fn try_cancel(&self, signal: i32) -> bool {
        debug_assert!(signal > 0, "process signals must be positive");
        self.signals
            .send_modify(|count| *count = count.saturating_add(1));
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

    fn manual_clock() -> (Arc<Mutex<Duration>>, Clock) {
        let now = Arc::new(Mutex::new(Duration::ZERO));
        let read = Arc::clone(&now);
        (now, Arc::new(move || *read.lock().unwrap()))
    }

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);
    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Captured {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn at(now: &Arc<Mutex<Duration>>, millis: u64) {
        *now.lock().unwrap() = Duration::from_millis(millis);
    }

    #[derive(Clone, Copy, Debug)]
    enum StatusFault {
        PartialWrite,
        Flush,
        Erase,
    }

    struct FailStatusOnce {
        output: Captured,
        fault: Option<StatusFault>,
        partial_pending: bool,
    }

    impl Write for FailStatusOnce {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.partial_pending) {
                return Err(io::Error::other("status write failed"));
            }
            match self.fault {
                Some(StatusFault::PartialWrite) => {
                    self.fault = None;
                    self.partial_pending = true;
                    self.output.write(&bytes[..bytes.len() - 1])
                }
                Some(StatusFault::Erase) if bytes == ERASE_LINE.as_bytes() => {
                    self.fault = None;
                    Err(io::Error::other("status erasure failed"))
                }
                _ => self.output.write(bytes),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            if matches!(self.fault, Some(StatusFault::Flush)) {
                self.fault = None;
                return Err(io::Error::other("status flush failed"));
            }
            Ok(())
        }
    }

    #[test]
    fn diagnostic_is_written_after_status_erasure_fails() {
        let output = Captured::default();
        let stderr = RunStderr::new(FailStatusOnce {
            output: output.clone(),
            fault: Some(StatusFault::Erase),
            partial_pending: false,
        });
        stderr.show_status("status").unwrap();
        stderr
            .diagnostic(|writer| writeln!(writer, "warning: recovered"))
            .unwrap();
        assert!(output.text().ends_with("warning: recovered\n"));
        stderr.erase_status().unwrap();
        assert!(output.text().ends_with(ERASE_LINE));
    }

    #[test]
    fn failed_status_output_is_erased_before_diagnostic() {
        for (fault, status) in [
            (StatusFault::PartialWrite, "statu"),
            (StatusFault::Flush, "status"),
        ] {
            let output = Captured::default();
            let stderr = RunStderr::new(FailStatusOnce {
                output: output.clone(),
                fault: Some(fault),
                partial_pending: false,
            });
            let (now, clock) = manual_clock();
            let progress = PresentationProgress::with_clock(
                stderr.clone(),
                ProgressDisplay::Transient {
                    delay: Duration::ZERO,
                },
                clock,
                80,
            );
            progress.snapshot(|| "status".into());
            at(&now, 250);
            progress.snapshot(|| "disabled".into());
            stderr
                .diagnostic(|writer| writeln!(writer, "warning: recovered"))
                .unwrap();
            progress.finish();
            assert_eq!(
                output.text(),
                format!("{ERASE_LINE}{status}{ERASE_LINE}warning: recovered\n"),
                "{fault:?}"
            );
        }
    }

    #[test]
    fn line_display_writes_the_first_snapshot_then_one_per_interval_and_the_latest_at_finish() {
        let (now, clock) = manual_clock();
        let out = Captured::default();
        let progress = PresentationProgress::with_clock(
            RunStderr::new(out.clone()),
            ProgressDisplay::Lines,
            clock,
            80,
        );
        for (millis, count) in [(0, 1), (4_999, 2), (5_000, 3), (6_000, 4), (7_000, 5)] {
            at(&now, millis);
            progress.snapshot(move || format!("n={count}"));
        }
        progress.finish();
        assert_eq!(out.text(), "n=1\nn=3\nn=5\n");
    }

    #[test]
    fn line_display_does_not_repeat_a_final_snapshot_already_written() {
        let (now, clock) = manual_clock();
        let out = Captured::default();
        let progress = PresentationProgress::with_clock(
            RunStderr::new(out.clone()),
            ProgressDisplay::Lines,
            clock,
            80,
        );
        progress.snapshot(|| "n=1".into());
        at(&now, 6_000);
        progress.snapshot(|| "n=2".into());
        progress.finish();
        progress.finish();
        assert_eq!(out.text(), "n=1\nn=2\n");
    }

    #[test]
    fn an_identical_final_snapshot_is_not_written_twice() {
        let (now, clock) = manual_clock();
        let out = Captured::default();
        let progress = PresentationProgress::with_clock(
            RunStderr::new(out.clone()),
            ProgressDisplay::Lines,
            clock,
            80,
        );
        progress.snapshot(|| "n=1".into());
        at(&now, 1_000);
        progress.snapshot(|| "n=1".into());
        progress.end_stage();
        progress.finish();
        assert_eq!(out.text(), "n=1\n");
    }

    #[test]
    fn ending_a_stage_writes_its_latest_unshown_line() {
        let (now, clock) = manual_clock();
        let out = Captured::default();
        let progress = PresentationProgress::with_clock(
            RunStderr::new(out.clone()),
            ProgressDisplay::Lines,
            clock,
            80,
        );
        progress.snapshot(|| "read 1".into());
        at(&now, 1_000);
        progress.snapshot(|| "read 2".into());
        progress.end_stage();
        at(&now, 2_000);
        progress.snapshot(|| "stored 1".into());
        progress.finish();
        assert_eq!(out.text(), "read 1\nread 2\nstored 1\n");
    }

    #[test]
    fn an_unshown_snapshot_is_never_rendered() {
        let (now, clock) = manual_clock();
        let out = Captured::default();
        let rendered = Arc::new(AtomicUsize::new(0));
        let render = |rendered: &Arc<AtomicUsize>| {
            let rendered = Arc::clone(rendered);
            move || {
                rendered.fetch_add(1, Ordering::SeqCst);
                String::new()
            }
        };
        let transient = ProgressDisplay::Transient {
            delay: Duration::from_secs(1),
        };
        let progress =
            PresentationProgress::with_clock(RunStderr::new(out.clone()), transient, clock, 80);
        at(&now, 999);
        progress.snapshot(render(&rendered));
        progress.finish();
        let off = PresentationProgress::with_clock(
            RunStderr::new(out.clone()),
            ProgressDisplay::Off,
            manual_clock().1,
            80,
        );
        off.snapshot(render(&rendered));
        off.finish();
        assert_eq!(rendered.load(Ordering::SeqCst), 0);
        assert_eq!(out.text(), "");
    }

    #[test]
    fn transient_display_starts_after_the_delay_refreshes_at_most_every_quarter_second_and_erases_at_finish()
     {
        let (now, clock) = manual_clock();
        let out = Captured::default();
        let transient = ProgressDisplay::Transient {
            delay: Duration::from_secs(1),
        };
        let progress =
            PresentationProgress::with_clock(RunStderr::new(out.clone()), transient, clock, 8);
        for (millis, count) in [(500, 1), (1_000, 2), (1_249, 3), (1_250, 4)] {
            at(&now, millis);
            progress.snapshot(move || format!("count {count}"));
        }
        progress.finish();
        let erase = ERASE_LINE;
        assert_eq!(out.text(), format!("{erase}count 2{erase}count 4{erase}"));
    }

    #[test]
    fn a_requested_line_erases_the_transient_status_first() {
        let out = Captured::default();
        let transient = ProgressDisplay::Transient {
            delay: Duration::ZERO,
        };
        let progress = PresentationProgress::with_clock(
            RunStderr::new(out.clone()),
            transient,
            manual_clock().1,
            80,
        );
        progress.snapshot(|| "status".into());
        progress.write_line("corpus: reading x");
        progress.finish();
        let erase = ERASE_LINE;
        assert_eq!(
            out.text(),
            format!("{erase}status{erase}corpus: reading x\n")
        );
    }

    #[test]
    fn dropping_a_run_without_finish_erases_the_transient_status() {
        let out = Captured::default();
        let transient = ProgressDisplay::Transient {
            delay: Duration::ZERO,
        };
        let progress = PresentationProgress::with_clock(
            RunStderr::new(out.clone()),
            transient,
            manual_clock().1,
            80,
        );
        progress.snapshot(|| "status".into());
        drop(progress);
        let erase = ERASE_LINE;
        assert_eq!(out.text(), format!("{erase}status{erase}"));
    }

    #[test]
    fn a_diagnostic_during_a_visible_status_line_starts_on_an_erased_line() {
        let out = Captured::default();
        let stderr = RunStderr::new(out.clone());
        let transient = ProgressDisplay::Transient {
            delay: Duration::ZERO,
        };
        let (now, clock) = manual_clock();
        let progress = PresentationProgress::with_clock(stderr.clone(), transient, clock, 80);
        progress.snapshot(|| "status 1".into());
        stderr
            .diagnostic(|writer| writeln!(writer, "warning: one"))
            .unwrap();
        stderr
            .diagnostic(|writer| writeln!(writer, "warning: two"))
            .unwrap();
        at(&now, 250);
        progress.snapshot(|| "status 2".into());
        progress.finish();
        let erase = ERASE_LINE;
        assert_eq!(
            out.text(),
            format!("{erase}status 1{erase}warning: one\nwarning: two\n{erase}status 2{erase}")
        );
    }

    #[test]
    fn a_diagnostic_failure_is_returned_to_its_writer() {
        struct Closed;
        impl io::Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let written = RunStderr::new(Closed).diagnostic(|writer| writeln!(writer, "warning"));
        assert_eq!(written.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn a_diagnostic_from_another_thread_never_follows_status_text_on_one_line() {
        #[derive(Clone, Default)]
        struct ByteAtATime(Arc<Mutex<Vec<u8>>>);
        impl io::Write for ByteAtATime {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().push(bytes[0]);
                std::thread::yield_now();
                Ok(1)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let out = ByteAtATime::default();
        let stderr = RunStderr::new(out.clone());
        let transient = ProgressDisplay::Transient {
            delay: Duration::ZERO,
        };
        // Each reading is a refresh interval later, so every snapshot is shown.
        let ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let clock: Clock = Arc::new(move || {
            TRANSIENT_REFRESH_INTERVAL
                * u32::try_from(ticks.fetch_add(1, Ordering::SeqCst)).unwrap()
        });
        let progress = PresentationProgress::with_clock(stderr.clone(), transient, clock, 80);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..200 {
                    stderr
                        .diagnostic(|writer| writeln!(writer, "diagnostic"))
                        .unwrap();
                }
            });
            for count in 0..200 {
                progress.snapshot(move || format!("status {count}"));
            }
        });
        progress.finish();
        let output = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        let mut pieces = output.split("diagnostic\n").collect::<Vec<_>>();
        pieces.pop();
        assert_eq!(pieces.len(), 200);
        for before in pieces {
            assert!(
                before.is_empty() || before.ends_with('\n') || before.ends_with(ERASE_LINE),
                "status text precedes a diagnostic: {before:?}"
            );
        }
    }

    #[test]
    fn any_write_failure_disables_progress_without_an_error() {
        struct FailFirst(Arc<AtomicUsize>);
        impl io::Write for FailFirst {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let writes = Arc::new(AtomicUsize::new(0));
        let progress = PresentationProgress::with_clock(
            RunStderr::new(FailFirst(Arc::clone(&writes))),
            ProgressDisplay::Lines,
            manual_clock().1,
            80,
        );
        progress.snapshot(|| "first".into());
        progress.write_line("second");
        progress.snapshot(|| "third".into());
        progress.finish();
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_progress_clones_write_complete_lines() {
        #[derive(Clone)]
        struct SharedWriter(Arc<Mutex<Vec<u8>>>);
        impl io::Write for SharedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                // One byte per call: only the progress lock keeps a line whole.
                self.0.lock().unwrap().push(bytes[0]);
                std::thread::yield_now();
                Ok(1)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let progress = PresentationProgress::with_clock(
            RunStderr::new(SharedWriter(Arc::clone(&bytes))),
            ProgressDisplay::Lines,
            manual_clock().1,
            80,
        );
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
        progress.finish();
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
}
