//! `dictionary install`: download a packaged UniDic dictionary.

use std::io::{self, Write};

use lingrep_cli_common::SafeText;

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
    let installed = install(dictionary, &directory, &args.download_base)?;
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
        write_jsonl(&mut writer, &DictionaryEnvelope::new(data)).map_err(CliError::from)?;
    } else {
        write_human(&mut writer, &data).map_err(CliError::stdout)?;
    }
    Ok(CommandOutcome::Success)
}

fn write_human<W: Write>(writer: &mut W, data: &InstalledData) -> io::Result<()> {
    let state = if data.already_present {
        "already installed"
    } else {
        "installed"
    };
    let path = SafeText::from_untrusted(&data.path);
    writeln!(writer, "{state} {path} ({} bytes)", data.bytes)?;
    writeln!(
        writer,
        "license: {}",
        SafeText::from_untrusted(&data.license)
    )?;
    if data.default {
        writeln!(
            writer,
            "ling uses this dictionary when neither --dict-path nor LINGREP_DICTIONARY_PATH names one"
        )
    } else {
        writeln!(writer, "use it with --dict-path {path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_human_sanitizes_untrusted_path_and_license() {
        for is_default in [true, false] {
            let data = InstalledData {
                name: "cwj".into(),
                path: "/tmp/evil\u{1b}[2J\u{202e}/unidic.dic".into(),
                bytes: 42,
                blake3: "deadbeef".into(),
                license: "GPL\u{0085}v3".into(),
                already_present: false,
                default: is_default,
            };
            let mut buffer = Vec::new();
            write_human(&mut buffer, &data).unwrap();
            let rendered = String::from_utf8(buffer).unwrap();
            assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
            assert!(!rendered.contains('\u{202e}'), "{rendered:?}");
            assert!(!rendered.contains('\u{0085}'), "{rendered:?}");
            assert!(rendered.contains("\\u{001b}[2J\\u{202e}"), "{rendered:?}");
            assert!(rendered.contains("GPL\\u{0085}v3"), "{rendered:?}");
        }
    }
}
