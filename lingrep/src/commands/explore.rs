//! Interactive TUI orchestration over the shared bounded executor.

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use super::CommandOutcome;
use crate::catalog_input::load_and_compile;
use crate::cli::{ExecutionCliConfig, ExploreArgs};
use crate::error::{CliError, FailureCode};
use crate::interactive_execution::InteractiveExecution;
use crate::terminal::TerminalArbiter;
use crate::tui::{self, ExplorerOutcome};

pub fn run(
    args: &ExploreArgs,
    config: &ExecutionCliConfig,
    terminal_state: &TerminalArbiter,
) -> Result<CommandOutcome, CliError> {
    // Catalog validation intentionally precedes terminal takeover so a syntax
    // error is an ordinary usage diagnostic with no alternate-screen bytes.
    let catalog = load_and_compile(&args.query.catalog.catalog_input()?)?;
    let execution = Arc::new(InteractiveExecution::new(&args.query.input, config)?);
    if !execution.is_reopenable() {
        return Err(CliError::InvalidArgs(
            "explore requires a reopenable direct root or prepared corpus; stdin cannot support detail rescans"
                .into(),
        ));
    }
    execution.preflight()?;
    if !io::stderr().is_terminal() {
        return Err(CliError::CommandFailure {
            code: FailureCode::Io,
            message: "explore requires an interactive terminal on stderr".into(),
        });
    }

    let mut stderr = io::stderr();
    let terminal_ops = CrosstermTerminalOps;
    let mut session = TerminalSession::enter(&mut stderr, &terminal_ops).map_err(CliError::Io)?;
    let result = (|| {
        let backend = CrosstermBackend::new(io::stderr());
        let mut terminal = Terminal::new(backend)?;
        let mut model = tui::Model::new().with_pattern_text(catalog.source.text);
        if let Some(path) = &args.query.catalog.patterns {
            model = model.with_pattern_file(path.clone());
        }
        tui::run(
            &mut terminal,
            model,
            execution,
            terminal_state.cancellation(),
        )
    })();
    session.restore().map_err(CliError::Io)?;

    match result.map_err(CliError::Io)? {
        ExplorerOutcome::Cancelled => Ok(CommandOutcome::Cancelled),
        ExplorerOutcome::Completed if terminal_state.try_claim_completion() => {
            Ok(CommandOutcome::Success)
        }
        ExplorerOutcome::Completed => Ok(CommandOutcome::Cancelled),
    }
}

trait TerminalOps {
    fn enable_raw(&self) -> io::Result<()>;
    fn disable_raw(&self) -> io::Result<()>;
    fn enter_screen(&self, writer: &mut impl Write) -> io::Result<()>;
    fn leave_screen(&self, writer: &mut impl Write) -> io::Result<()>;
    fn enable_mouse(&self, writer: &mut impl Write) -> io::Result<()>;
    fn disable_mouse(&self, writer: &mut impl Write) -> io::Result<()>;
}

struct CrosstermTerminalOps;

impl TerminalOps for CrosstermTerminalOps {
    fn enable_raw(&self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw(&self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn enter_screen(&self, writer: &mut impl Write) -> io::Result<()> {
        execute!(writer, EnterAlternateScreen)
    }

    fn leave_screen(&self, writer: &mut impl Write) -> io::Result<()> {
        execute!(writer, LeaveAlternateScreen)
    }

    fn enable_mouse(&self, writer: &mut impl Write) -> io::Result<()> {
        execute!(writer, EnableMouseCapture)
    }

    fn disable_mouse(&self, writer: &mut impl Write) -> io::Result<()> {
        execute!(writer, DisableMouseCapture)
    }
}

struct TerminalSession<'a, W: Write, O: TerminalOps> {
    writer: &'a mut W,
    ops: &'a O,
    raw_enabled: bool,
    screen_attempted: bool,
    mouse_attempted: bool,
    restored: bool,
}

impl<'a, W, O> TerminalSession<'a, W, O>
where
    W: Write,
    O: TerminalOps,
{
    fn enter(writer: &'a mut W, ops: &'a O) -> io::Result<Self> {
        ops.enable_raw()?;
        let mut session = Self {
            writer,
            ops,
            raw_enabled: true,
            screen_attempted: false,
            mouse_attempted: false,
            restored: false,
        };
        // Once raw mode succeeds, conservatively attempt every terminal
        // compensation on any later setup error. A command write may fail
        // after the terminal consumed part of its escape sequence.
        session.screen_attempted = true;
        session.mouse_attempted = true;
        if let Err(error) = session.ops.enter_screen(session.writer) {
            let _ = session.restore();
            return Err(error);
        }
        if let Err(error) = session.ops.enable_mouse(session.writer) {
            let _ = session.restore();
            return Err(error);
        }
        Ok(session)
    }

    fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        let mut first_error = None;
        if self.mouse_attempted
            && let Err(error) = self.ops.disable_mouse(self.writer)
        {
            first_error = Some(error);
        }
        if self.screen_attempted
            && let Err(error) = self.ops.leave_screen(self.writer)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
        if self.raw_enabled
            && let Err(error) = self.ops.disable_raw()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl<W, O> Drop for TerminalSession<'_, W, O>
where
    W: Write,
    O: TerminalOps,
{
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default)]
    struct FakeOps {
        fail_at: Cell<Option<&'static str>>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeOps {
        fn call(&self, name: &'static str) -> io::Result<()> {
            self.calls.borrow_mut().push(name);
            if self.fail_at.get() == Some(name) {
                Err(io::Error::other(format!("{name} failed")))
            } else {
                Ok(())
            }
        }
    }

    impl TerminalOps for FakeOps {
        fn enable_raw(&self) -> io::Result<()> {
            self.call("enable_raw")
        }
        fn disable_raw(&self) -> io::Result<()> {
            self.call("disable_raw")
        }
        fn enter_screen(&self, _writer: &mut impl Write) -> io::Result<()> {
            self.call("enter_screen")
        }
        fn leave_screen(&self, _writer: &mut impl Write) -> io::Result<()> {
            self.call("leave_screen")
        }
        fn enable_mouse(&self, _writer: &mut impl Write) -> io::Result<()> {
            self.call("enable_mouse")
        }
        fn disable_mouse(&self, _writer: &mut impl Write) -> io::Result<()> {
            self.call("disable_mouse")
        }
    }

    #[test]
    fn staged_setup_failure_attempts_every_relevant_cleanup() {
        for (failure, expected) in [
            ("enable_raw", vec!["enable_raw"]),
            (
                "enter_screen",
                vec![
                    "enable_raw",
                    "enter_screen",
                    "disable_mouse",
                    "leave_screen",
                    "disable_raw",
                ],
            ),
            (
                "enable_mouse",
                vec![
                    "enable_raw",
                    "enter_screen",
                    "enable_mouse",
                    "disable_mouse",
                    "leave_screen",
                    "disable_raw",
                ],
            ),
        ] {
            let ops = FakeOps::default();
            ops.fail_at.set(Some(failure));
            let mut writer = Vec::new();
            assert!(TerminalSession::enter(&mut writer, &ops).is_err());
            assert_eq!(*ops.calls.borrow(), expected, "{failure}");
        }
    }

    #[test]
    fn restore_continues_after_each_cleanup_failure() {
        for failure in ["disable_mouse", "leave_screen", "disable_raw"] {
            let ops = FakeOps::default();
            let mut writer = Vec::new();
            let mut session = TerminalSession::enter(&mut writer, &ops).unwrap();
            ops.fail_at.set(Some(failure));
            assert!(session.restore().is_err(), "{failure}");
            assert_eq!(
                &ops.calls.borrow()[3..],
                ["disable_mouse", "leave_screen", "disable_raw"],
                "{failure}"
            );
        }
    }
}
