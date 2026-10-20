use std::path::PathBuf;

use clap::Args;

/// Shared dictionary-path argument for the lingrep binaries.
#[derive(Args, Debug, Clone)]
pub struct DictArgs {
    /// Path to the Vibrato dictionary (.dic.zst or .dic).
    // The two spellings named above are the ones every consumer accepts. The
    // loader branches only on the `.zst` suffix and maps anything else as
    // uncompressed. Any consumer-specific acquisition nuance belongs in
    // `docs/cli-usage-guide.md`, not in a shared one-line help string.
    //
    // `hide_env_values` keeps `--help` deterministic: without it, clap prints
    // the live `LINGREP_DICTIONARY_PATH` value, which differs between a bare shell
    // and the nix dev shell (which sets it) — breaking the trycmd `--help`
    // snapshots under `cargo test` / `ci.sh`.
    #[arg(
        short = 'd',
        long = "dict-path",
        required = true,
        env = "LINGREP_DICTIONARY_PATH",
        hide_env_values = true
    )]
    pub dict_path: PathBuf,
}

/// Shared corpus-input arguments: an optional `--input` path (stdin when
/// omitted) and an optional `--input-format` override, resolved by the pure
/// [`lingrep_core::corpus::resolve_input_format`] rule.
#[derive(Args, Debug, Clone)]
pub struct CorpusInputArgs {
    /// Input corpus file or directory; reads from stdin when omitted.
    #[arg(short = 'i', long = "input")]
    pub input: Option<PathBuf>,
    /// Override the inferred corpus format (`plain`, `jsonl`, `markdown`,
    /// `conllu`, `bccwj`, `soranoha`; commands may restrict accepted formats).
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
                PathBuf::from("/tmp/dict"),
                "flag: {flag}"
            );
        }
    }

    /// `--dict-path` is satisfied either by the flag or by the
    /// `LINGREP_DICTIONARY_PATH` env var (`required = true` + `env`). This test is
    /// robust to the ambient environment — the nix dev shell sets that var, so
    /// a plain `cargo test --workspace` runs with it present. When the var is
    /// set and non-empty, env satisfies the requirement and parsing succeeds
    /// (verifying the `env` wiring); otherwise the missing flag errors
    /// (verifying the `required` wiring).
    #[test]
    fn dict_path_required_or_supplied_by_env() {
        let env_set =
            std::env::var_os("LINGREP_DICTIONARY_PATH").is_some_and(|value| !value.is_empty());
        let result = Harness::try_parse_from(["x"]);
        if env_set {
            assert!(
                result.is_ok(),
                "expected LINGREP_DICTIONARY_PATH to satisfy required --dict-path"
            );
        } else {
            assert!(
                result.is_err(),
                "expected missing --dict-path with no LINGREP_DICTIONARY_PATH to error"
            );
        }
    }
}
