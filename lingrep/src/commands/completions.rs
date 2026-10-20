use std::io::{self, Write};

use clap::CommandFactory;

use crate::Cli;
use crate::cli::{CompletionShell, CompletionsArgs};
use crate::error::{CliError, FailureCode};
use crate::presentation::OutputMode;
use crate::protocol::completions::{CompletionData, CompletionEnvelope};
use crate::protocol::discovery::ContractId;
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run(args: &CompletionsArgs, output: OutputMode) -> Result<CommandOutcome, CliError> {
    let mut script = Vec::new();
    clap_complete::generate(shell(args.shell), &mut Cli::command(), "ling", &mut script);
    let script = generated_script(args.shell, script)?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    match output {
        OutputMode::Human => writer.write_all(script.as_bytes()).map_err(CliError::Io)?,
        OutputMode::Jsonl => {
            write_jsonl(
                &mut writer,
                &CompletionEnvelope::new(CompletionData {
                    shell: format!("{:?}", args.shell).to_lowercase(),
                    script,
                }),
            )
            .map_err(|error| CliError::Io(io::Error::other(error)))?;
        }
        OutputMode::Tsv => {
            return Err(CliError::UnsupportedOutput {
                command: "completions",
                output: "tsv",
            });
        }
    }
    Ok(CommandOutcome::Success)
}

/// `clap_complete` 4.6 emits positional possible values for Bash and Zsh, but
/// not for Fish, Elvish, or PowerShell. Keep the small compatibility patch here,
/// at the shell-projection boundary, and derive its vocabulary from the same
/// enum the parser accepts.
fn generated_script(shell: CompletionShell, bytes: Vec<u8>) -> Result<String, CliError> {
    let script =
        String::from_utf8(bytes).map_err(|error| generator_contract_error(error.to_string()))?;
    with_contract_id_completions(shell, script)
}

fn with_contract_id_completions(
    shell: CompletionShell,
    mut script: String,
) -> Result<String, CliError> {
    let missing = missing_contract_ids(&script);
    if missing.is_empty() {
        return Ok(script);
    }

    match shell {
        CompletionShell::Bash | CompletionShell::Zsh => {
            return Err(generator_contract_error(format!(
                "{shell:?} completion omitted positional contract IDs: {}",
                missing
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        CompletionShell::Fish => {
            for id in missing {
                script.push_str(&format!(
                    "complete -c ling -n \"__fish_ling_using_subcommand contracts\" -f -a '{}' -d 'Stable machine contract'\n",
                    id.as_str()
                ));
            }
        }
        CompletionShell::Elvish => {
            let marker = "        &'ling;contracts'= {\n";
            let values = missing
                .iter()
                .map(|id| {
                    format!(
                        "            cand {} 'Stable machine contract'\n",
                        id.as_str()
                    )
                })
                .collect::<String>();
            script = inject_after(script, marker, &values)?;
        }
        CompletionShell::Powershell => {
            let marker = "        'ling;contracts' {\n";
            let values = missing
                .iter()
                .map(|id| {
                    format!(
                        "            [CompletionResult]::new('{}', '{}', [CompletionResultType]::ParameterValue, 'Stable machine contract')\n",
                        id.as_str(),
                        id.as_str()
                    )
                })
                .collect::<String>();
            script = inject_after(script, marker, &values)?;
        }
    }
    Ok(script)
}

fn inject_after(mut script: String, marker: &str, values: &str) -> Result<String, CliError> {
    let offset = script.find(marker).ok_or_else(|| {
        generator_contract_error("completion generator omitted the contracts command")
    })? + marker.len();
    script.insert_str(offset, values);
    Ok(script)
}

fn generator_contract_error(message: impl Into<String>) -> CliError {
    CliError::CommandFailure {
        code: FailureCode::ProtocolContract,
        message: message.into(),
    }
}

fn missing_contract_ids(script: &str) -> Vec<ContractId> {
    ContractId::ALL
        .iter()
        .copied()
        .filter(|id| !script.contains(id.as_str()))
        .collect()
}

fn shell(value: CompletionShell) -> clap_complete::Shell {
    match value {
        CompletionShell::Bash => clap_complete::Shell::Bash,
        CompletionShell::Zsh => clap_complete::Shell::Zsh,
        CompletionShell::Fish => clap_complete::Shell::Fish,
        CompletionShell::Elvish => clap_complete::Shell::Elvish,
        CompletionShell::Powershell => clap_complete::Shell::PowerShell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Termination, exit_code, termination_for_failure};
    use crate::protocol::diagnostic::RetryPolicy;

    fn all_ids() -> String {
        ContractId::ALL
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn complete_native_output_is_byte_identical() {
        let native = all_ids();
        assert_eq!(
            with_contract_id_completions(CompletionShell::Bash, native.clone()).unwrap(),
            native
        );
    }

    #[test]
    fn fish_repairs_every_missing_id_not_only_the_first() {
        let first = ContractId::ALL[0].as_str().to_owned();
        let repaired = with_contract_id_completions(CompletionShell::Fish, first).unwrap();
        for id in ContractId::ALL {
            assert_eq!(
                repaired.matches(id.as_str()).count(),
                1,
                "wrong candidate count for {}",
                id.as_str()
            );
        }
    }

    #[test]
    fn native_shell_regression_is_a_protocol_contract_failure() {
        let error = with_contract_id_completions(CompletionShell::Bash, String::new()).unwrap_err();
        assert_eq!(error.code(), "protocol_contract");
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
        let CliError::CommandFailure { code, .. } = error else {
            panic!("expected command failure");
        };
        assert_eq!(termination_for_failure(code), Termination::Failure);
        assert_eq!(exit_code(Termination::Failure), 1);
    }

    #[test]
    fn missing_injection_marker_is_a_protocol_contract_failure() {
        let error =
            with_contract_id_completions(CompletionShell::Elvish, String::new()).unwrap_err();
        assert_eq!(error.code(), "protocol_contract");
    }

    #[test]
    fn invalid_generator_utf8_is_a_protocol_contract_failure() {
        let error = generated_script(CompletionShell::Bash, vec![0xff]).unwrap_err();
        assert_eq!(error.code(), "protocol_contract");
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
        assert_eq!(error.termination(), Termination::Failure);
        assert_eq!(exit_code(error.termination()), 1);
    }
}
