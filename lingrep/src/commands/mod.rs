//! Command dispatch boundaries.

pub(crate) mod attributes;
pub mod check;
pub mod completions;
pub mod corpus;
mod corpus_metadata;
pub mod describe;
pub mod doctor;
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
pub mod sentence_final;
pub mod summarize;
pub mod tokenize;
pub mod variants;

use lingrep_core::corpus::admission_identity_digest;
use lingrep_corpus::AdmissionIdentity;

use crate::cli::{Cli, Command, CorpusCommand, ExecutionCliConfig};
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
        return explain::run(cli);
    }
    match &cli.command {
        Command::Doctor(args) => doctor::run(args, output),
        Command::Check(args) => check::run(args, config, terminal, output),
        Command::Matches(args) => matches::run(args, config, terminal, output, cli.color),
        Command::Summarize(args) => summarize::run(args, config, terminal, output),
        Command::Corpus(args) => match &args.command {
            CorpusCommand::Sources(args) => corpus::run_sources(args, config, terminal, output),
            CorpusCommand::Build(args) => corpus::run_build(args, config, terminal, output),
            CorpusCommand::Inspect(args) => corpus::run_inspect(args, config, terminal, output),
            CorpusCommand::Variants(args) => variants::run(args, output),
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
        Command::SentenceFinal(args) => sentence_final::run(args, config, terminal, output),
        Command::Describe(args) => describe::run(args, output),
        Command::Completions(args) => completions::run(args, output),
        Command::Manpages(args) => manpages::run(args, output),
        Command::Schema(args) => schema::run(args, output),
    }
}

fn admission_identity() -> AdmissionIdentity {
    AdmissionIdentity::from_bytes(admission_identity_digest())
}
