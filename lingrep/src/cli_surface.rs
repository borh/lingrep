//! Joined parser facts and protocol semantics for discovery surfaces.

use clap::{Arg, Command, CommandFactory};

use crate::Cli;
use crate::examples::{examples_for, semantics_for};
use crate::protocol::capabilities::{ArgumentData, CapabilityData, ExampleData};

pub struct CliSurface {
    pub command: Command,
}

impl Default for CliSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl CliSurface {
    #[must_use]
    pub fn new() -> Self {
        let mut command = Cli::command();
        command.build();
        Self { command }
    }

    pub fn describe(&self, requested: &[String]) -> Result<Vec<CapabilityData>, String> {
        let mut entries = Vec::new();
        collect_capabilities(&self.command, "", &mut entries);
        if requested.is_empty() {
            return Ok(entries);
        }
        let name = requested.join(" ");
        let selected = entries
            .into_iter()
            .filter(|entry| entry.command == name)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            Err(format!("unknown command `{name}`"))
        } else {
            Ok(selected)
        }
    }
}

fn collect_capabilities(command: &Command, prefix: &str, entries: &mut Vec<CapabilityData>) {
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set() && child.get_name() != "help")
    {
        let path = if prefix.is_empty() {
            child.get_name().to_owned()
        } else {
            format!("{prefix} {}", child.get_name())
        };
        entries.push(capability(child, &path));
        collect_capabilities(child, &path, entries);
    }
}

fn capability(command: &Command, path: &str) -> CapabilityData {
    let has_tsv = command.get_arguments().any(|arg| arg.get_id() == "tsv");
    let mut output_modes = vec!["human".into(), "json".into()];
    if has_tsv {
        output_modes.push("tsv".into());
    }
    if path == "explore" {
        output_modes = vec!["human".into()];
    }
    CapabilityData {
        command: path.into(),
        purpose: command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default(),
        output_modes,
        schemas: Vec::new(),
        tsv_contracts: Vec::new(),
        input_modes: semantics_for(path).map_or_else(Vec::new, |semantics| {
            semantics
                .input_modes
                .iter()
                .map(|mode| (*mode).into())
                .collect()
        }),
        arguments: command
            .get_arguments()
            .map(|arg| argument(command, arg))
            .collect(),
        environment: command
            .get_arguments()
            .filter_map(|arg| arg.get_env().map(|env| env.to_string_lossy().into_owned()))
            .chain(
                semantics_for(path)
                    .into_iter()
                    .flat_map(|semantics| semantics.input_modes)
                    .any(|mode| *mode == "direct")
                    .then(|| "LINGREP_DICTIONARY_PATH".to_owned()),
            )
            .collect(),
        defaults: command
            .get_arguments()
            .flat_map(|arg| {
                arg.get_default_values()
                    .iter()
                    .map(move |value| format!("{}={}", arg.get_id(), value.to_string_lossy()))
            })
            .collect(),
        exit_codes: vec![
            "0=success".into(),
            "1=runtime failure".into(),
            "2=invalid invocation".into(),
            "130=SIGINT".into(),
            "143=SIGTERM".into(),
        ],
        examples: examples_for(path)
            .iter()
            .map(|example| ExampleData {
                id: example.id.into(),
                purpose: example.purpose.into(),
                argv: example.argv.iter().map(|token| (*token).into()).collect(),
                placeholders: example
                    .placeholders
                    .iter()
                    .map(|placeholder| (placeholder.name.into(), placeholder.description.into()))
                    .collect(),
            })
            .collect(),
    }
}

fn argument(command: &Command, arg: &Arg) -> ArgumentData {
    ArgumentData {
        name: arg.get_id().to_string(),
        required: arg.is_required_set(),
        values: arg
            .get_possible_values()
            .into_iter()
            .map(|value| value.get_name().to_owned())
            .collect(),
        conflicts: command
            .get_arg_conflicts_with(arg)
            .into_iter()
            .map(|other| other.get_id().to_string())
            .collect(),
    }
}
