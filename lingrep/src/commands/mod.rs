//! Command dispatch boundaries.

mod analyzer;
pub(crate) mod attributes;
pub mod check;
mod check_diagram;
pub mod completions;
pub mod corpus;
mod corpus_metadata;
pub mod describe;
pub mod dictionary;
pub mod doctor;
mod ending_report;
pub mod explain;
pub mod explore;
pub mod manpages;
pub mod matches;
pub(crate) mod query_completion;
pub(crate) mod query_projection;
pub(crate) mod query_resolution;
pub(crate) mod query_scope;
pub(crate) mod query_source;
pub(crate) mod query_support;
pub mod schema;
pub mod summarize;
pub mod table;
pub mod tokenize;
pub mod variants;

use lingrep_core::corpus::admission_identity_digest;
use lingrep_corpus::AdmissionIdentity;

use crate::cli::{
    AnalyzerCommand, Cli, Command, CorpusCommand, DictionaryCommand, ExecutionCliConfig,
};
use crate::error::CliError;
use crate::terminal::TerminalArbiter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    Success,
    Failure,
    Cancelled,
    DisplayClosed,
}

pub fn dispatch(
    cli: &Cli,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
) -> Result<CommandOutcome, CliError> {
    let output = cli.output_mode();
    if explain::requested(cli) {
        return explain::run(cli, config);
    }
    match &cli.command {
        Command::Analyzer(args) => match &args.command {
            AnalyzerCommand::Prepare(args) => analyzer::run(args, config, output),
        },
        Command::Doctor(args) => doctor::run(args, config, output),
        Command::Dictionary(args) => match &args.command {
            DictionaryCommand::Install(args) => dictionary::run_install(args, output),
        },
        Command::Check(args)
            if args.diagram && output == crate::presentation::OutputMode::Jsonl =>
        {
            Err(CliError::UnsupportedOutput {
                command: "check --diagram",
                output: "json",
            })
        }
        Command::Check(args) => check::run(args, config, terminal, output),
        Command::Matches(args) => matches::run(args, config, terminal, output, cli.color),
        Command::Summarize(args) => summarize::run(args, config, terminal, output),
        Command::Table(args) => table::run(args, output),
        Command::Corpus(args) => match &args.command {
            CorpusCommand::Sources(args) => corpus::run_sources(args, config, terminal, output),
            CorpusCommand::Build(args) => corpus::run_build(args, config, terminal, output),
            CorpusCommand::Inspect(args) => corpus::run_inspect(args, config, terminal, output),
            CorpusCommand::Variants(args) => variants::run(args, output),
            CorpusCommand::Render(_) if output == crate::presentation::OutputMode::Jsonl => {
                Err(CliError::UnsupportedOutput {
                    command: "corpus render",
                    output: "json",
                })
            }
            CorpusCommand::Render(args) => corpus::run_render(args, config, terminal),
        },
        Command::Tokenize(args) => tokenize::run(args, config, terminal, output),
        Command::Attributes(args) => attributes::run(args, config, terminal, output),
        Command::Explore(_) if output == crate::presentation::OutputMode::Jsonl => {
            Err(CliError::UnsupportedOutput {
                command: "explore",
                output: "json",
            })
        }
        Command::Explore(args) => explore::run(args, config, terminal),
        #[cfg(feature = "web")]
        Command::Serve(_) if output == crate::presentation::OutputMode::Jsonl => {
            Err(CliError::UnsupportedOutput {
                command: "serve",
                output: "json",
            })
        }
        #[cfg(feature = "web")]
        Command::Serve(args) => lingrep_web::serve::run_until(args, terminal.signal_count())
            .map(|()| CommandOutcome::Success)
            .map_err(CliError::InvalidArgs),
        Command::Describe(args) => describe::run(args, output),
        Command::Completions(args) => completions::run(args, output),
        Command::Manpages(args) => manpages::run(args, output),
        Command::Schema(args) => schema::run(args, output),
    }
}

fn admission_identity() -> AdmissionIdentity {
    AdmissionIdentity::from_bytes(admission_identity_digest())
}
