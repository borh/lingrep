//! `dictionary install`: download a packaged UniDic dictionary.

use std::io::{self, Write};

use crate::cli::DictionaryInstallArgs;
use crate::error::CliError;
use crate::packaged_dictionaries::{DEFAULT, MISSING_DICTIONARY_HINT, install, install_directory};
use crate::presentation::OutputMode;
use crate::protocol::dictionary::{DictionaryEnvelope, InstalledData};
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run_install(
    args: &DictionaryInstallArgs,
    mode: OutputMode,
) -> Result<CommandOutcome, CliError> {
    if mode == OutputMode::Tsv {
        return Err(CliError::UnsupportedOutput {
            command: "dictionary install",
            output: "tsv",
        });
    }
    let dictionary = args.name.0;
    let directory = install_directory().ok_or_else(|| {
        CliError::Dictionary(format!(
            "neither XDG_DATA_HOME nor HOME is set, so there is no install directory; {MISSING_DICTIONARY_HINT}"
        ))
    })?;
    let installed = install(dictionary, &directory)?;
    let data = InstalledData {
        name: dictionary.name.into(),
        path: installed.path.display().to_string(),
        bytes: installed.bytes,
        blake3: dictionary.blake3.into(),
        license: dictionary.license.into(),
        already_present: installed.already_present,
        default: dictionary == DEFAULT,
    };
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if mode == OutputMode::Jsonl {
        write_jsonl(&mut writer, &DictionaryEnvelope::new(data))
            .map_err(|error| CliError::Io(io::Error::other(error)))?;
    } else {
        write_human(&mut writer, &data)?;
    }
    Ok(CommandOutcome::Success)
}

fn write_human<W: Write>(writer: &mut W, data: &InstalledData) -> io::Result<()> {
    let state = if data.already_present {
        "already installed"
    } else {
        "installed"
    };
    writeln!(writer, "{state} {} ({} bytes)", data.path, data.bytes)?;
    writeln!(writer, "license: {}", data.license)?;
    if data.default {
        writeln!(
            writer,
            "ling uses this dictionary when neither --dict-path nor LINGREP_DICTIONARY_PATH names one"
        )
    } else {
        writeln!(writer, "use it with --dict-path {}", data.path)
    }
}
