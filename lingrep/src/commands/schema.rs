use std::io::{self, Write};

use lingrep_cli_common::SafeText;

use crate::cli::SchemaArgs;
use crate::error::{CliError, FailureCode};
use crate::presentation::OutputMode;
use crate::protocol::contract::{
    ContractEnvelope, ContractEvent, ContractIndexData, ContractSummaryData,
};
use crate::protocol::discovery::HeaderPolicy;
use crate::protocol::registry;
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run(args: &SchemaArgs, mode: OutputMode) -> Result<CommandOutcome, CliError> {
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    match args.contract {
        None => {
            let summaries = registry::summaries();
            match mode {
                OutputMode::Jsonl => write_jsonl(
                    &mut writer,
                    &ContractEnvelope::new(ContractEvent::ContractIndex(ContractIndexData {
                        contracts: summaries
                            .iter()
                            .map(|summary| ContractSummaryData {
                                id: summary.id.as_str().into(),
                                kind: summary.kind,
                                owners: summary
                                    .owners
                                    .iter()
                                    .map(|owner| (*owner).into())
                                    .collect(),
                                summary: summary.summary.into(),
                            })
                            .collect(),
                    })),
                )
                .map_err(|error| CliError::Io(io::Error::other(error)))?,
                OutputMode::Human => {
                    writeln!(writer, "ID  KIND  OWNERS  SUMMARY").map_err(CliError::Io)?;
                    for summary in summaries {
                        writeln!(
                            writer,
                            "{}  {:?}  {}  {}",
                            SafeText::from_untrusted(summary.id.as_str()),
                            summary.kind,
                            SafeText::from_untrusted(&summary.owners.join(",")),
                            SafeText::from_untrusted(summary.summary)
                        )
                        .map_err(CliError::Io)?;
                    }
                }
                OutputMode::Tsv => return Err(unsupported()),
            }
        }
        Some(id) => {
            let detail =
                registry::get(id)
                    .to_detail()
                    .map_err(|error| CliError::CommandFailure {
                        code: FailureCode::ProtocolContract,
                        message: error.to_string(),
                    })?;
            match mode {
                OutputMode::Jsonl => write_jsonl(
                    &mut writer,
                    &ContractEnvelope::new(ContractEvent::Contract(Box::new(detail))),
                )
                .map_err(|error| CliError::Io(io::Error::other(error)))?,
                OutputMode::Human => {
                    writeln!(
                        writer,
                        "{} ({:?})",
                        SafeText::from_untrusted(&detail.id),
                        detail.kind
                    )
                    .map_err(CliError::Io)?;
                    writeln!(
                        writer,
                        "  owners: {}",
                        SafeText::from_untrusted(&detail.owners.join(", "))
                    )
                    .map_err(CliError::Io)?;
                    writeln!(
                        writer,
                        "  ordering: {}",
                        SafeText::from_untrusted(&detail.ordering)
                    )
                    .map_err(CliError::Io)?;
                    writeln!(
                        writer,
                        "  coverage: {}",
                        SafeText::from_untrusted(&detail.coverage)
                    )
                    .map_err(CliError::Io)?;
                    if let Some(tsv) = &detail.tsv {
                        let columns = tsv
                            .columns
                            .iter()
                            .map(|column| column.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let header = match tsv.header {
                            HeaderPolicy::None => "none",
                        };
                        writeln!(writer, "  columns: {}", SafeText::from_untrusted(&columns))
                            .map_err(CliError::Io)?;
                        writeln!(writer, "  header: {header}").map_err(CliError::Io)?;
                        writeln!(
                            writer,
                            "  escaping: {}",
                            SafeText::from_untrusted(&tsv.escaping)
                        )
                        .map_err(CliError::Io)?;
                    }
                    writeln!(
                        writer,
                        "  broken pipe: {}",
                        SafeText::from_untrusted(&detail.broken_pipe)
                    )
                    .map_err(CliError::Io)?;
                    writeln!(
                        writer,
                        "  exit status: {}",
                        SafeText::from_untrusted(&detail.exit_status)
                    )
                    .map_err(CliError::Io)?;
                }
                OutputMode::Tsv => return Err(unsupported()),
            }
        }
    }
    Ok(CommandOutcome::Success)
}

fn unsupported() -> CliError {
    CliError::UnsupportedOutput {
        command: "contracts",
        output: "tsv",
    }
}
