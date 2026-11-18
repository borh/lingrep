#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::ffi::OsString;
use std::io::{self, Write};

use clap::error::{ContextKind, ContextValue};
use lingrep::cli::{Cli, DiagnosticFormat};
use lingrep::commands::{self, CommandOutcome};
use lingrep::error::{CliError, FailureCode, Termination, exit_code, termination_for_signal};
use lingrep::protocol::diagnostic::{
    DiagnosticData, DiagnosticEnvelope, DiagnosticEvent, DiagnosticLocation, DiagnosticSeverity,
    DiagnosticStage, RelatedDiagnosticLocation, RetryPolicy,
};
use lingrep::protocol::query::SourceSpan;
use lingrep::protocol::write_jsonl;
use lingrep::terminal::TerminalArbiter;
use lingrep_cli_common::SafeText;
use lingrep_core::fsm::{DiagnosticSeverity as CompilerSeverity, render_pattern_error};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

fn main() {
    let code = run_process();
    lingrep_core::test_scale::write_spill_file_report();
    std::process::exit(code);
}

fn run_process() -> i32 {
    lingrep::open_files::raise_soft_limit();
    let raw_args = std::env::args_os().collect::<Vec<_>>();
    let json_diagnostics = json_requested_from(&raw_args);
    let cli = match Cli::try_parse_from(&raw_args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            if json_diagnostics && error.use_stderr() {
                if render_clap_error(&error).is_err() {
                    return 1;
                }
            } else if let Err(delivery) = error.print() {
                return exit_code(
                    if delivery.kind() == io::ErrorKind::BrokenPipe && !error.use_stderr() {
                        Termination::Success
                    } else {
                        Termination::Failure
                    },
                );
            }
            return code;
        }
    };

    // Suite and resource validation runs first: an unsupported
    // suite cannot select any output namespace and therefore publishes no
    // command output.
    let config = match cli.execution_config() {
        Ok(config) => config,
        Err(error) => {
            return if render_error(cli.diagnostic_format(), &error).is_ok() {
                exit_code(error.termination())
            } else {
                1
            };
        }
    };
    let output_mode = cli.output_mode();

    let terminal = TerminalArbiter::new();
    let signals = match ProcessSignals::install(terminal.clone()) {
        Ok(signals) => signals,
        Err(error) => {
            let error = CliError::CommandFailure {
                code: FailureCode::SignalHandler,
                message: format!("failed to install signal handlers: {error}"),
            };
            return if render_error(cli.diagnostic_format(), &error).is_ok() {
                exit_code(error.termination())
            } else {
                1
            };
        }
    };

    let command_result = match commands::dispatch(&cli, &config, &terminal) {
        Err(error)
            if output_mode == lingrep::presentation::OutputMode::Human
                && error.is_stdout_broken_pipe() =>
        {
            Ok(CommandOutcome::DisplayClosed)
        }
        result => result,
    };
    let termination = match command_result {
        Ok(CommandOutcome::Success) => Termination::Success,
        Ok(CommandOutcome::Failure) => Termination::Failure,
        Ok(CommandOutcome::DisplayClosed) => Termination::Success,
        Ok(CommandOutcome::Cancelled) => signals.termination().unwrap_or(Termination::Failure),
        Err(error)
            if output_mode == lingrep::presentation::OutputMode::Tsv
                && error.is_stdout_broken_pipe() =>
        {
            // TSV is intended to compose with bounded Unix consumers such as
            // `head`. It has no completion row, so keep a failure status for
            // callers using `pipefail`, but do not distract the ordinary
            // pipeline with a diagnostic for the reader's expected closure.
            error.termination()
        }
        Err(error) => {
            if render_error(cli.diagnostic_format(), &error).is_ok() {
                error.termination()
            } else {
                Termination::Failure
            }
        }
    };
    exit_code(termination)
}

fn json_requested_from(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != "--")
        .any(|argument| argument.as_os_str() == "--json")
}

fn render_clap_error(error: &clap::Error) -> io::Result<()> {
    let suggestions = [
        ContextKind::SuggestedSubcommand,
        ContextKind::SuggestedArg,
        ContextKind::SuggestedValue,
        ContextKind::Suggested,
    ]
    .into_iter()
    .filter_map(|kind| error.get(kind))
    .flat_map(context_strings)
    .collect();
    let usage = error.get(ContextKind::Usage).map(ToString::to_string);
    let message = clap_message(error);
    let diagnostic = DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(DiagnosticData {
        run_id: None,
        observed_input_digest: None,
        stage: DiagnosticStage::Invocation,
        severity: DiagnosticSeverity::Error,
        code: "invalid_invocation".into(),
        message,
        suggestions,
        usage,
        retry: Some(RetryPolicy::AfterChange),
        location: None,
        related_locations: Vec::new(),
    }));
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    write_jsonl(&mut writer, &diagnostic).map_err(io::Error::from)?;
    writer.flush()
}

fn context_strings(value: &ContextValue) -> Vec<String> {
    match value {
        ContextValue::String(value) => vec![value.clone()],
        ContextValue::Strings(values) => values.clone(),
        ContextValue::StyledStr(value) => vec![value.to_string()],
        ContextValue::StyledStrs(values) => values.iter().map(ToString::to_string).collect(),
        value => vec![value.to_string()],
    }
}

fn clap_message(error: &clap::Error) -> String {
    let kind = error.kind().as_str().unwrap_or("invalid invocation");
    for context in [
        ContextKind::InvalidSubcommand,
        ContextKind::InvalidArg,
        ContextKind::InvalidValue,
        ContextKind::TrailingArg,
    ] {
        if let Some(value) = error.get(context) {
            return format!("{kind}: {value}");
        }
    }
    kind.to_owned()
}

struct ProcessSignals {
    terminal: TerminalArbiter,
}

impl ProcessSignals {
    fn install(terminal: TerminalArbiter) -> io::Result<Self> {
        let observed = terminal.clone();
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        std::thread::Builder::new()
            .name("ling-signals".into())
            .spawn(move || {
                for received in signals.forever() {
                    observed.try_cancel(received);
                }
            })?;
        Ok(Self { terminal })
    }

    fn termination(&self) -> Option<Termination> {
        self.terminal
            .winning_signal()
            .and_then(termination_for_signal)
    }
}

fn render_error(format: DiagnosticFormat, error: &CliError) -> io::Result<()> {
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    match format {
        DiagnosticFormat::Human => {
            if let CliError::CatalogCompile { error, source_text } = error.root_error() {
                for line in render_pattern_error(Some(source_text), error).lines() {
                    writeln!(writer, "{}", SafeText::from_untrusted(line))?;
                }
            } else {
                let message = error.to_string();
                writeln!(
                    writer,
                    "error[{}]: {}",
                    error.code(),
                    SafeText::from_untrusted(&message)
                )?;
            }
            for suggestion in error.suggestions() {
                writeln!(writer, "help: {}", SafeText::from_untrusted(&suggestion))?;
            }
        }
        DiagnosticFormat::Json => {
            if let CliError::CatalogCompile {
                error: compiler_error,
                ..
            } = error.root_error()
            {
                for compiler_diagnostic in &compiler_error.diagnostics {
                    let location = compiler_diagnostic.location();
                    let byte_range = location.byte_range();
                    let diagnostic =
                        DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(DiagnosticData {
                            run_id: error.run_id().map(str::to_owned),
                            observed_input_digest: error.observed_input_digest().map(str::to_owned),
                            stage: DiagnosticStage::Catalog,
                            severity: match compiler_diagnostic.severity() {
                                CompilerSeverity::Error => DiagnosticSeverity::Error,
                                CompilerSeverity::Warning => DiagnosticSeverity::Warning,
                            },
                            code: compiler_diagnostic.code().as_str().into(),
                            message: compiler_diagnostic.message.clone(),
                            suggestions: if compiler_diagnostic.code()
                                == lingrep_core::fsm::DiagnosticCode::LimitExceeded
                            {
                                error.suggestions()
                            } else {
                                Vec::new()
                            },
                            usage: None,
                            retry: (compiler_diagnostic.severity() == CompilerSeverity::Error)
                                .then_some(error.retry_policy()),
                            location: Some(compiler_location(
                                location,
                                byte_range.start,
                                byte_range.end,
                            )),
                            related_locations: compiler_diagnostic
                                .related_locations()
                                .iter()
                                .map(|related| {
                                    let location = related.location();
                                    let byte_range = location.byte_range();
                                    RelatedDiagnosticLocation {
                                        message: related.message().into(),
                                        location: compiler_location(
                                            location,
                                            byte_range.start,
                                            byte_range.end,
                                        ),
                                    }
                                })
                                .collect(),
                        }));
                    write_jsonl(&mut writer, &diagnostic)?;
                }
            } else {
                let diagnostic =
                    DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(error.diagnostic()));
                write_jsonl(&mut writer, &diagnostic)?;
            }
        }
    }
    writer.flush()
}

fn compiler_location(
    location: &lingrep_core::fsm::DiagnosticLocation,
    start: usize,
    end: usize,
) -> DiagnosticLocation {
    DiagnosticLocation {
        source_name: Some(location.source_name().into()),
        source_span: Some(SourceSpan {
            start: start as u64,
            end: end as u64,
        }),
        line: Some(location.line() as u64),
        column: Some(location.column() as u64),
        corpus_sentence_ordinal: None,
        token_index: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_sentence_omission_is_an_input_diagnostic_at_the_first_gap() {
        let error = CliError::OversizedSentencesSkipped {
            count: 2,
            first_source_name: "00027.md".into(),
            first_ordinal: 7,
            first_actual: 187_965,
            limit: 65_536,
        }
        .with_run_id("run-gap".into(), None);

        assert_eq!(error.diagnostic().stage, DiagnosticStage::Input);
        assert_eq!(
            error.diagnostic().location,
            Some(DiagnosticLocation {
                source_name: Some("00027.md".into()),
                source_span: None,
                line: None,
                column: None,
                corpus_sentence_ordinal: Some(7),
                token_index: None,
            })
        );
    }

    #[test]
    fn missing_attribute_diagnostic_preserves_run_and_token_location() {
        let error = CliError::Execution(
            lingrep_core::execution::ExecutionError::MissingRequiredAttribute {
                attribute: "orth",
                source_name: "a.md".into(),
                corpus_sentence_ordinal: 7,
                token_index: 3,
            },
        )
        .with_run_id("run-7".into(), None);

        assert_eq!(error.code(), "missing_required_attribute");
        assert_eq!(error.run_id(), Some("run-7"));
        assert_eq!(error.diagnostic().stage, DiagnosticStage::Matching);
        assert_eq!(
            error.diagnostic().location,
            Some(DiagnosticLocation {
                source_name: Some("a.md".into()),
                source_span: None,
                line: None,
                column: None,
                corpus_sentence_ordinal: Some(7),
                token_index: Some(3),
            })
        );
    }
}
