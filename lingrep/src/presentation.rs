use std::io::{self, IsTerminal};
use std::str::FromStr;
use std::time::Duration;

use clap::ValueEnum;

use crate::cli::{CheckFormat, MachineFormat, MatchFormat, TokenizeFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Jsonl,
    Tsv,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ProgressMode {
    #[default]
    Auto,
    Always,
    Never,
}

/// Whether a command writes its results while it runs or only after it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResultDelivery {
    Streaming,
    Deferred,
}

/// Which standard streams are terminals, and whether the terminal supports
/// cursor control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalState {
    pub stdout: bool,
    pub stderr: bool,
    pub cursor_control: bool,
}

impl TerminalState {
    pub fn detect() -> Self {
        Self {
            stdout: io::stdout().is_terminal(),
            stderr: io::stderr().is_terminal(),
            cursor_control: std::env::var_os("TERM").is_some_and(|term| term != "dumb"),
        }
    }
}

/// How a run presents progress on stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgressDisplay {
    Off,
    /// Newline-terminated lines: the first snapshot at once, then at most one
    /// per [`PROGRESS_LINE_INTERVAL`], then the latest unshown snapshot at the end.
    Lines,
    /// One status line rewritten in place, first shown after `delay`, redrawn at
    /// most once per [`TRANSIENT_REFRESH_INTERVAL`], and erased at the end.
    Transient {
        delay: Duration,
    },
}

pub(crate) const PROGRESS_LINE_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const TRANSIENT_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const AUTO_TRANSIENT_DELAY: Duration = Duration::from_secs(1);

impl ProgressMode {
    /// The display for one run. A transient line requires human output that
    /// is written only after the run, with both streams on a terminal that
    /// supports cursor control; `auto` shows nothing otherwise.
    #[must_use]
    pub(crate) fn display(
        self,
        output: OutputMode,
        delivery: ResultDelivery,
        terminals: TerminalState,
    ) -> ProgressDisplay {
        let transient = output == OutputMode::Human
            && delivery == ResultDelivery::Deferred
            && terminals.stdout
            && terminals.stderr
            && terminals.cursor_control;
        match self {
            Self::Never => ProgressDisplay::Off,
            Self::Auto if transient => ProgressDisplay::Transient {
                delay: AUTO_TRANSIENT_DELAY,
            },
            Self::Auto => ProgressDisplay::Off,
            Self::Always if transient => ProgressDisplay::Transient {
                delay: Duration::ZERO,
            },
            Self::Always => ProgressDisplay::Lines,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplicitLimit<T> {
    Value(T),
    All,
}

impl<T: Copy> ExplicitLimit<T> {
    pub fn resolve(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::All => None,
        }
    }
}

impl FromStr for ExplicitLimit<u64> {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "all" {
            return Ok(Self::All);
        }
        let value = value
            .parse::<u64>()
            .map_err(|_| "must be a positive integer or 'all'".to_owned())?;
        if value == 0 {
            return Err("must be greater than zero or 'all'".to_owned());
        }
        Ok(Self::Value(value))
    }
}

#[must_use]
pub fn resolve_match_context(output: OutputMode, context: Option<usize>) -> usize {
    context.unwrap_or(if output == OutputMode::Human { 5 } else { 0 })
}

#[must_use]
pub fn resolve_match_limit(output: OutputMode, limit: Option<ExplicitLimit<u64>>) -> Option<u64> {
    limit.map_or(
        if output == OutputMode::Human {
            Some(100)
        } else {
            None
        },
        ExplicitLimit::resolve,
    )
}

#[must_use]
pub fn resolve_summary_top(output: OutputMode, top: Option<ExplicitLimit<u64>>) -> Option<u64> {
    top.map_or(
        if output == OutputMode::Human {
            Some(20)
        } else {
            None
        },
        ExplicitLimit::resolve,
    )
}

impl From<MatchFormat> for OutputMode {
    fn from(value: MatchFormat) -> Self {
        match value {
            MatchFormat::Jsonl => Self::Jsonl,
            MatchFormat::Tsv => Self::Tsv,
            MatchFormat::Human => Self::Human,
        }
    }
}

impl From<MachineFormat> for OutputMode {
    fn from(value: MachineFormat) -> Self {
        match value {
            MachineFormat::Jsonl => Self::Jsonl,
            MachineFormat::Tsv => Self::Tsv,
        }
    }
}

impl From<CheckFormat> for OutputMode {
    fn from(value: CheckFormat) -> Self {
        match value {
            CheckFormat::Json => Self::Jsonl,
            CheckFormat::Human => Self::Human,
        }
    }
}

impl From<TokenizeFormat> for OutputMode {
    fn from(_: TokenizeFormat) -> Self {
        Self::Jsonl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_display_follows_mode_format_delivery_and_terminals() {
        use OutputMode::{Human, Jsonl, Tsv};
        use ProgressDisplay::{Lines, Off, Transient};
        use ResultDelivery::{Deferred, Streaming};
        let all = TerminalState {
            stdout: true,
            stderr: true,
            cursor_control: true,
        };
        let auto = Transient {
            delay: AUTO_TRANSIENT_DELAY,
        };
        let always = Transient {
            delay: Duration::ZERO,
        };
        let rows = [
            (Human, Deferred, all, [auto, always, Off]),
            (Human, Streaming, all, [Off, Lines, Off]),
            (Jsonl, Deferred, all, [Off, Lines, Off]),
            (Tsv, Deferred, all, [Off, Lines, Off]),
            (
                Human,
                Deferred,
                TerminalState {
                    stdout: false,
                    ..all
                },
                [Off, Lines, Off],
            ),
            (
                Human,
                Deferred,
                TerminalState {
                    stderr: false,
                    ..all
                },
                [Off, Lines, Off],
            ),
            (
                Human,
                Deferred,
                TerminalState {
                    cursor_control: false,
                    ..all
                },
                [Off, Lines, Off],
            ),
        ];
        for (output, delivery, terminals, expected) in rows {
            let actual = [
                ProgressMode::Auto,
                ProgressMode::Always,
                ProgressMode::Never,
            ]
            .map(|mode| mode.display(output, delivery, terminals));
            assert_eq!(actual, expected, "{output:?} {delivery:?} {terminals:?}");
        }
    }

    #[test]
    fn explicit_limit_accepts_positive_values_and_all() {
        assert_eq!("5".parse(), Ok(ExplicitLimit::Value(5)));
        assert_eq!("all".parse(), Ok(ExplicitLimit::All));
        assert!("0".parse::<ExplicitLimit<u64>>().is_err());
    }
}
