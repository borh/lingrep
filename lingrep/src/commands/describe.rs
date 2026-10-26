use std::io::{self, Write};

use lingrep_cli_common::SafeText;

use crate::cli::DescribeArgs;
use crate::cli_surface::CliSurface;
use crate::error::CliError;
use crate::examples::{examples_for, render_posix};
use crate::presentation::OutputMode;
use crate::protocol::capabilities::CapabilityEnvelope;
use crate::protocol::discovery::ContractKind;
use crate::protocol::registry;
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run(args: &DescribeArgs, output: OutputMode) -> Result<CommandOutcome, CliError> {
    let mut entries = CliSurface::new()
        .describe(&args.command)
        .map_err(CliError::InvalidArgs)?;
    for entry in &mut entries {
        let contracts = registry::owned_by(&entry.command);
        entry.schemas = contracts
            .iter()
            .filter(|contract| contract.kind == ContractKind::Jsonl)
            .map(|contract| contract.id.as_str().into())
            .collect();
        entry.tsv_contracts = contracts
            .iter()
            .filter(|contract| contract.kind == ContractKind::Tsv)
            .map(|contract| contract.id.as_str().into())
            .collect();
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for entry in entries {
        match output {
            OutputMode::Jsonl => write_jsonl(&mut writer, &CapabilityEnvelope::new(entry))
                .map_err(|error| CliError::Io(io::Error::other(error)))?,
            OutputMode::Human => {
                writeln!(
                    writer,
                    "{} — {}\n  output: {}\n  options: ling {} --help-all",
                    SafeText::from_untrusted(&entry.command),
                    SafeText::from_untrusted(&entry.purpose),
                    entry.output_modes.join(", "),
                    SafeText::from_untrusted(&entry.command)
                )?;
                for example in examples_for(&entry.command) {
                    writeln!(
                        writer,
                        "\n  example — {}",
                        SafeText::from_untrusted(example.purpose)
                    )
                    .map_err(CliError::Io)?;
                    writeln!(writer, "    ling {}", render_posix(example)).map_err(CliError::Io)?;
                    for placeholder in example.placeholders {
                        writeln!(
                            writer,
                            "    {}: {}",
                            SafeText::from_untrusted(placeholder.name),
                            SafeText::from_untrusted(placeholder.description)
                        )
                        .map_err(CliError::Io)?;
                    }
                }
            }
            OutputMode::Tsv => {
                return Err(CliError::UnsupportedOutput {
                    command: "describe",
                    output: "tsv",
                });
            }
        }
    }
    Ok(CommandOutcome::Success)
}
