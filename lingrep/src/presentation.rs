use std::str::FromStr;

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
    fn explicit_limit_accepts_positive_values_and_all() {
        assert_eq!("5".parse(), Ok(ExplicitLimit::Value(5)));
        assert_eq!("all".parse(), Ok(ExplicitLimit::All));
        assert!("0".parse::<ExplicitLimit<u64>>().is_err());
    }
}
