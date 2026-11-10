use std::path::PathBuf;

use clap::Args;

/// Shared dictionary-path argument for the lingrep binaries.
#[derive(Args, Debug, Clone)]
pub struct DictArgs {
    /// Path to the Vibrato dictionary (.dic.zst or .dic); overrides
    /// LINGREP_DICTIONARY_PATH and the installed default dictionary.
    // The two spellings named above are the ones every consumer accepts. The
    // loader branches only on the `.zst` suffix and maps anything else as
    // uncompressed. Any consumer-specific acquisition nuance belongs in
    // `docs/cli-usage-guide.md`, not in a shared one-line help string.
    #[arg(short = 'd', long = "dict-path", value_name = "PATH")]
    pub dict_path: Option<PathBuf>,
}

impl DictArgs {
    /// `--dict-path`, otherwise [`fallback_dictionary_path`].
    #[must_use]
    pub fn resolve(&self) -> Option<PathBuf> {
        self.dict_path.clone().or_else(fallback_dictionary_path)
    }
}

/// File name of the dictionary `ling dictionary install` installs by default.
pub const DEFAULT_DICTIONARY_FILE_NAME: &str = "unidic-cwj-202512.dic";

/// How to supply a Vibrato dictionary, for errors that report none.
pub const MISSING_DICTIONARY_HINT: &str =
    "run `ling dictionary install`, pass --dict-path, or set LINGREP_DICTIONARY_PATH";

/// `$XDG_DATA_HOME/lingrep/dictionaries`, or `~/.local/share/lingrep/dictionaries`
/// when `XDG_DATA_HOME` is unset or empty, matching `install.sh`.
#[must_use]
pub fn install_directory() -> Option<PathBuf> {
    let non_empty = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let data_home = non_empty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data_home.join("lingrep").join("dictionaries"))
}

/// `LINGREP_DICTIONARY_PATH` when set and non-empty, otherwise the installed
/// default dictionary when that file exists.
#[must_use]
pub fn fallback_dictionary_path() -> Option<PathBuf> {
    std::env::var_os("LINGREP_DICTIONARY_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            install_directory()
                .map(|directory| directory.join(DEFAULT_DICTIONARY_FILE_NAME))
                .filter(|path| path.is_file())
        })
}

/// Shared corpus-input arguments: an optional `--input` path (stdin when
/// omitted) and an optional `--input-format` override, resolved by the pure
/// [`lingrep_core::corpus::resolve_input_format`] rule.
#[derive(Args, Debug, Clone)]
pub struct CorpusInputArgs {
    /// Input corpus file or directory; reads from stdin when omitted.
    #[arg(short = 'i', long = "input")]
    pub input: Option<PathBuf>,
    /// Override the inferred corpus format (commands may restrict accepted formats).
    #[arg(long = "input-format", value_enum)]
    pub input_format: Option<lingrep_core::corpus::CorpusFormat>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use crate::DictArgs;

    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        dict: DictArgs,
    }

    #[test]
    fn dict_path_flag_spellings_parse() {
        for flag in ["--dict-path", "-d"] {
            let harness = Harness::try_parse_from(["x", flag, "/tmp/dict"]).unwrap();
            assert_eq!(
                harness.dict.dict_path,
                Some(PathBuf::from("/tmp/dict")),
                "flag: {flag}"
            );
        }
    }
}
