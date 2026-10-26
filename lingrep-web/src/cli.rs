//! Command-line argument parsing for lingrep-web.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Parser;
use lingrep_cli_common::{CorpusInputArgs, DictArgs, MarkdownReaderArgs};
use lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES;

/// An optional absolute path prefix under which lingrep-web serves its HTTP routes.
///
/// A root deployment uses `/`. Mounted deployments use a slash-separated path
/// such as `/research/lingrep-web`; the browser-facing links stay relative, so the
/// same application can also sit behind a proxy that rewrites the request URI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BasePath(String);

impl BasePath {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        value.parse()
    }
}

impl FromStr for BasePath {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "/" {
            return Ok(Self(value.to_owned()));
        }
        if !value.starts_with('/') {
            return Err("base path must start with '/'".to_owned());
        }
        if value.ends_with('/') {
            return Err("base path must not end with '/' unless it is '/'".to_owned());
        }
        if value.split('/').skip(1).any(|segment| {
            segment.is_empty()
                || matches!(segment, "." | "..")
                || segment.chars().any(|character| {
                    character.is_control()
                        || character.is_whitespace()
                        || matches!(character, '?' | '#')
                })
        }) {
            return Err(
                "base path must contain non-empty URL path segments without whitespace, '?' or '#'"
                    .to_owned(),
            );
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Parser)]
#[command(
    name = "lingrep-web",
    about = "Web interface for lingrep pattern matching",
    mut_arg("input", |arg| arg.help("Corpus input path; stdin is unsupported. Omit only when using --corpus."))
)]
pub struct Args {
    #[command(flatten)]
    pub dict: DictArgs,

    /// Corpus input: a `.jsonl`/`.md` file, a Markdown directory, or a
    /// prepared `.lingcorpus` artifact; stdin is unsupported (the server requires
    /// a path). `--input-format` overrides shared format inference and is
    /// rejected for prepared artifacts, which record their own reader configuration.
    #[command(flatten)]
    pub input: CorpusInputArgs,

    /// Section selection for Markdown input, applied when the corpus is read.
    #[command(flatten)]
    pub markdown: MarkdownReaderArgs,

    /// Address to bind. Loopback by default — change with `LINGREP_WEB_HOST` or
    /// `--host` to expose the service to other hosts (typically behind a
    /// reverse proxy that adds TLS and access control).
    #[arg(
        long = "host",
        env = "LINGREP_WEB_HOST",
        default_value = "127.0.0.1",
        hide_env_values = true
    )]
    pub host: String,

    /// Port to listen on
    #[arg(
        short,
        long,
        env = "LINGREP_WEB_PORT",
        default_value = "3000",
        hide_env_values = true
    )]
    pub port: u16,

    /// Optional absolute path prefix used by a reverse proxy mount. Browser
    /// links remain relative, so a proxy can also rewrite its own root to this
    /// internal path.
    #[arg(long = "base-path", default_value = "/")]
    pub base_path: BasePath,

    /// Maximum KWIC lines retained for each pattern
    #[arg(long, default_value_t = 1_000)]
    pub max_kwic_lines_per_pattern: usize,

    /// Maximum number of concurrently retained workbench sessions
    #[arg(long, default_value_t = 256)]
    pub max_active_sessions: usize,

    /// Maximum bytes accepted for one physical input record: a JSONL document,
    /// or a plaintext or Markdown line. A record larger than this stops the
    /// corpus build, reporting the record and its source.
    #[arg(long, default_value_t = DEFAULT_MAX_INPUT_RECORD_BYTES)]
    pub max_record_bytes: usize,

    /// lingrep patterns grammar file whose contents seed the pattern editor on the index
    /// page. Read once at startup; an unreadable file starts an empty editor.
    #[arg(long = "patterns", default_value = "patterns/sentence-final.lx")]
    pub patterns: PathBuf,

    /// Directory searched recursively for `*.lx` catalogs offered in the UI's
    /// catalog list. Defaults to the parent directory of `--patterns`, so the
    /// stock defaults discover `patterns/`. An unreadable directory yields an
    /// empty list rather than a startup failure.
    #[arg(long = "pattern-root")]
    pub pattern_root: Option<PathBuf>,

    /// Additional corpus, as `NAME=PATH`. Repeatable. `NAME` is the URL key used
    /// by shareable links and must match `[A-Za-z0-9_-]{1,64}`; `default` is
    /// reserved for `--input`. All declared corpora share one dictionary and one
    /// reader configuration. `PATH` is a corpus source, or a prepared
    /// `.lingcorpus` artifact — read directly, and so unaffected by that shared
    /// reader configuration. Preparing an artifact with `ling corpus build` allows
    /// lingrep-web to serve formats without direct reader support, such as `bccwj`.
    #[arg(long = "corpus", value_name = "NAME=PATH")]
    pub corpus: Vec<String>,
}

/// The corpora `args` declares, in declaration order, `--input` first.
///
/// Returns a message suitable for `eprintln!` + exit 2 on any refusal, matching
/// how `Config::validate` failures are already reported.
pub fn declared_corpora(
    args: &Args,
) -> Result<Vec<(crate::corpus_set::CorpusName, PathBuf, String)>, String> {
    let mut declared = Vec::new();
    if let Some(input) = args.input.input.as_ref() {
        declared.push((
            crate::corpus_set::CorpusName::default_key(),
            input.clone(),
            crate::corpus_context::corpus_display_name(&input.display().to_string()),
        ));
    }
    for entry in &args.corpus {
        let Some((name, path)) = entry.split_once('=') else {
            return Err(format!("--corpus expects NAME=PATH; got {entry:?}"));
        };
        // parse_declared, not parse: this is operator input, so the reserved
        // `default` key is refused here. The wire parser uses `parse`, which
        // admits it, because `k=default` is an ordinary permalink.
        // Name the declaration, not just the fault: `NameError` describes what
        // is wrong with the *name*, and with several `--corpus` flags an
        // operator otherwise cannot tell which one was rejected. The malformed
        // -shape branch above already names its entry.
        let key = crate::corpus_set::CorpusName::parse_declared(name)
            .map_err(|error| format!("--corpus {entry:?}: {error}"))?;
        if declared.iter().any(|(existing, _, _)| existing == &key) {
            return Err(format!(
                "--corpus {:?} is declared more than once",
                key.as_str()
            ));
        }
        declared.push((key, PathBuf::from(path), name.to_owned()));
    }
    if declared.len() > 1 && args.input.input_format.is_some() {
        return Err(
            "--input-format applies to every declared corpus and so cannot be used with more \
             than one; drop it and let the format be inferred per path"
                .to_owned(),
        );
    }
    // Refused rather than ignored. A prepared artifact is read directly and no
    // reader configuration applies to it, so an accepted `--input-format` here
    // would silently do nothing — and reader configuration reaches artifact
    // identity, so an operator has every reason to believe it did something.
    if let Some((_, path, _)) = declared.first()
        && declared.len() == 1
        && args.input.input_format.is_some()
        && lingrep_corpus::is_prepared_artifact(path)
    {
        return Err(format!(
            "--input-format does not apply to the prepared artifact {}: an artifact records the \
             reader it was built with, and is read directly; drop the flag",
            path.display()
        ));
    }
    Ok(declared)
}

/// Reads the seed grammar for the index-page pattern editor. A read failure is
/// deliberately non-fatal: the server starts with an empty editor and logs a
/// warning, so lingrep-web runs regardless of the directory the default relative
/// path resolves against.
#[must_use]
pub fn read_default_pattern(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "default pattern file unreadable; starting with an empty editor"
            );
            String::new()
        }
    }
}

/// The catalog discovery root for `args`: the explicit `--pattern-root` when
/// given, otherwise the directory implied by `--patterns`.
#[must_use]
pub fn resolve_pattern_root(args: &Args) -> PathBuf {
    args.pattern_root
        .clone()
        .unwrap_or_else(|| crate::catalog_index::default_root(&args.patterns))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use clap::{CommandFactory, Parser};

    use super::{Args, DEFAULT_MAX_INPUT_RECORD_BYTES};

    #[test]
    fn input_help_requires_a_path_instead_of_promising_stdin() {
        let help = Args::command().render_long_help().to_string();
        assert!(!help.contains("reads from stdin when omitted"), "{help}");
        assert!(help.contains("stdin is unsupported"), "{help}");
        assert!(help.contains("--corpus"), "{help}");
    }

    #[test]
    fn dictionary_argument_uses_devshell_environment_default() {
        let command = Args::command();
        let dict = command
            .get_arguments()
            .find(|argument| argument.get_id() == "dict_path")
            .expect("dictionary argument should exist");

        assert_eq!(dict.get_env(), Some(OsStr::new("LINGREP_DICTIONARY_PATH")));
    }

    #[test]
    fn host_defaults_to_loopback() {
        let args = super::Args::try_parse_from(["lingrep-web", "--dict-path", "/tmp/d"])
            .expect("defaults should parse");
        assert_eq!(args.host, "127.0.0.1");
    }

    #[test]
    fn base_path_defaults_to_root_and_accepts_nested_mounts() {
        let args = super::Args::try_parse_from(["lingrep-web", "--dict-path", "/tmp/d"])
            .expect("defaults should parse");
        assert_eq!(args.base_path.as_str(), "/");

        let mounted = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--base-path",
            "/research/lingrep-web",
        ])
        .expect("nested mount should parse");
        assert_eq!(mounted.base_path.as_str(), "/research/lingrep-web");
    }

    #[test]
    fn base_path_refuses_ambiguous_or_unsafe_values() {
        for invalid in [
            "",
            "lingrep-web",
            "/lingrep-web/",
            "/vp//web",
            "/vp?web",
            "/vp#web",
        ] {
            let result = super::Args::try_parse_from([
                "lingrep-web",
                "--dict-path",
                "/tmp/d",
                "--base-path",
                invalid,
            ]);
            let error = match result {
                Ok(_) => panic!("invalid base path must be refused: {invalid:?}"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("base path"),
                "{invalid:?}: {error}"
            );
        }
    }

    #[test]
    fn host_flag_overrides_default() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--host",
            "0.0.0.0",
        ])
        .expect("--host should parse");
        assert_eq!(args.host, "0.0.0.0");
    }

    #[test]
    fn host_argument_uses_lingrep_web_host_env_var() {
        let command = Args::command();
        let host = command
            .get_arguments()
            .find(|argument| argument.get_id() == "host")
            .expect("host argument should exist");
        assert_eq!(host.get_env(), Some(OsStr::new("LINGREP_WEB_HOST")));

        let port = command
            .get_arguments()
            .find(|argument| argument.get_id() == "port")
            .expect("port argument should exist");
        assert_eq!(port.get_env(), Some(OsStr::new("LINGREP_WEB_PORT")));
    }

    #[test]
    fn surface_limit_flag_is_absent() {
        let command = Args::command();
        assert!(
            command
                .get_arguments()
                .all(|argument| argument.get_id() != "max_surface_forms_per_pattern")
        );
    }

    #[test]
    fn patterns_defaults_to_sf_experimental_when_absent() {
        // Supply --dict-path explicitly so the test is deterministic regardless
        // of whether the dev shell's LINGREP_DICTIONARY_PATH satisfies the requirement.
        let args = super::Args::try_parse_from(["lingrep-web", "--dict-path", "/tmp/d"])
            .expect("defaults should parse");
        assert_eq!(
            args.patterns,
            std::path::PathBuf::from("patterns/sentence-final.lx")
        );
    }

    #[test]
    fn record_limit_defaults_to_the_shared_record_bound_and_is_overridable() {
        let args = super::Args::try_parse_from(["lingrep-web", "--dict-path", "/tmp/d"])
            .expect("defaults should parse");
        assert_eq!(args.max_record_bytes, DEFAULT_MAX_INPUT_RECORD_BYTES);
        // Deliberately not the stage batch budget: a document larger than one
        // batch is ordinary and must not be refused for that reason.
        assert_ne!(
            args.max_record_bytes,
            lingrep_core::execution::DEFAULT_MAX_STAGE_INPUT_BYTES
        );

        let raised = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--max-record-bytes",
            "4194304",
        ])
        .expect("explicit --max-record-bytes should parse");
        assert_eq!(raised.max_record_bytes, 4 * 1024 * 1024);
    }

    #[test]
    fn patterns_flag_overrides_the_default() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--patterns",
            "x.lx",
        ])
        .expect("explicit --patterns should parse");
        assert_eq!(args.patterns, std::path::PathBuf::from("x.lx"));
    }

    #[test]
    fn read_default_pattern_returns_file_contents() {
        let path = std::env::temp_dir().join("lingrep_web_default_pattern_test.lx");
        std::fs::write(&path, "seed := [upos=VERB].\n").unwrap();
        assert_eq!(super::read_default_pattern(&path), "seed := [upos=VERB].\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_default_pattern_degrades_to_empty_on_missing_file() {
        let path = std::path::Path::new("/lingrep-web/no/such/default-pattern.lx");
        assert_eq!(super::read_default_pattern(path), String::new());
    }

    #[test]
    fn pattern_root_defaults_to_the_parent_of_patterns() {
        let args = super::Args::try_parse_from(["lingrep-web", "--dict-path", "/tmp/d"])
            .expect("defaults should parse");
        assert_eq!(
            super::resolve_pattern_root(&args),
            std::path::PathBuf::from("patterns")
        );
    }

    #[test]
    fn pattern_root_follows_an_explicit_patterns_path() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--patterns",
            "custom/dir/x.lx",
        ])
        .expect("explicit --patterns should parse");
        assert_eq!(
            super::resolve_pattern_root(&args),
            std::path::PathBuf::from("custom/dir")
        );
    }

    #[test]
    fn explicit_pattern_root_overrides_the_derived_default() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--pattern-root",
            "elsewhere",
        ])
        .expect("explicit --pattern-root should parse");
        assert_eq!(
            super::resolve_pattern_root(&args),
            std::path::PathBuf::from("elsewhere")
        );
    }

    #[test]
    fn input_alone_declares_the_default_corpus_keyed_default() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--input",
            "corpora/日本語コーパス.jsonl",
        ])
        .unwrap();
        let declared = super::declared_corpora(&args).unwrap();
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].0.as_str(), "default");
        // The non-ASCII stem survives as the display name.
        assert_eq!(declared[0].2, "日本語コーパス");
    }

    #[test]
    fn corpus_flags_are_declaration_ordered_after_input() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--input",
            "a.jsonl",
            "--corpus",
            "sci=b.jsonl",
            "--corpus",
            "news=c.jsonl",
        ])
        .unwrap();
        let declared = super::declared_corpora(&args).unwrap();
        let keys: Vec<_> = declared.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys, ["default", "sci", "news"]);
    }

    #[test]
    fn a_duplicate_name_is_refused() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--corpus",
            "sci=b.jsonl",
            "--corpus",
            "sci=c.jsonl",
        ])
        .unwrap();
        assert!(super::declared_corpora(&args).unwrap_err().contains("sci"));
    }

    #[test]
    fn an_explicit_default_name_is_refused_as_reserved() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--corpus",
            "default=b.jsonl",
        ])
        .unwrap();
        assert!(
            super::declared_corpora(&args)
                .unwrap_err()
                .contains("reserved")
        );
    }

    /// With several declarations, "found '!'" alone does not say which flag to
    /// fix. Every rejection names the declaration that caused it.
    #[test]
    fn an_invalid_name_names_the_declaration_that_carried_it() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--corpus",
            "ok=a.jsonl",
            "--corpus",
            "bad!name=b.jsonl",
        ])
        .unwrap();
        let error = super::declared_corpora(&args).unwrap_err();
        assert!(
            error.contains("bad!name=b.jsonl"),
            "the rejection must name the offending declaration: {error}"
        );
        assert!(
            !error.contains("ok=a.jsonl"),
            "and must not name an innocent one: {error}"
        );
    }

    #[test]
    fn a_malformed_declaration_names_the_expected_shape() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--corpus",
            "no-equals-sign",
        ])
        .unwrap();
        assert!(
            super::declared_corpora(&args)
                .unwrap_err()
                .contains("NAME=PATH")
        );
    }

    /// Format is inferred per path when the override is absent, so a mixed set
    /// works; a global override silently applied to every corpus would not, and
    /// per CLAUDE.md the reader config reaches cache identity.
    #[test]
    fn input_format_is_refused_with_several_corpora() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--input",
            "a.jsonl",
            "--corpus",
            "sci=b.jsonl",
            "--input-format",
            "jsonl",
        ])
        .unwrap();
        let error = super::declared_corpora(&args).unwrap_err();
        assert!(error.contains("--input-format"));
        // Names the recovery, not just the refusal.
        assert!(error.contains("inferred"));
    }

    #[test]
    fn input_format_is_still_allowed_with_a_single_corpus() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--input",
            "a.jsonl",
            "--input-format",
            "jsonl",
        ])
        .unwrap();
        assert!(super::declared_corpora(&args).is_ok());
    }
}

#[cfg(test)]
mod prepared_artifact_tests {
    use clap::Parser;

    /// Stands in for a prepared artifact: `is_prepared_artifact` is structural,
    /// so a directory carrying a `manifest.json` is one as far as declaration
    /// classification is concerned. Whether it *opens* is `Config::validate`'s
    /// question, tested there against a real fixture.
    fn artifact_shaped(directory: &std::path::Path) -> std::path::PathBuf {
        let path = directory.join("prepared.lingcorpus");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("manifest.json"), "{}").unwrap();
        path
    }

    #[test]
    fn a_prepared_artifact_can_be_declared_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = artifact_shaped(directory.path());
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--corpus",
            &format!("bccwj={}", artifact.display()),
        ])
        .unwrap();
        let declared = super::declared_corpora(&args).unwrap();
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].0.as_str(), "bccwj");
        assert_eq!(declared[0].1, artifact);
        // The declared key is the caption, not the artifact's long content-
        // addressed stem — which is the reason to prefer `--corpus NAME=PATH`
        // over `--input` for an artifact.
        assert_eq!(declared[0].2, "bccwj");
    }

    /// Silently ignoring it would be the trap: reader configuration reaches
    /// artifact identity, so an operator who passes it has every reason to
    /// believe it took effect.
    #[test]
    fn input_format_is_refused_for_a_prepared_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = artifact_shaped(directory.path());
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--input",
            artifact.to_str().unwrap(),
            "--input-format",
            "jsonl",
        ])
        .unwrap();
        let error = super::declared_corpora(&args).unwrap_err();
        assert!(error.contains("--input-format"), "{error}");
        assert!(error.contains("prepared artifact"), "{error}");
        // Names the recovery, not just the refusal.
        assert!(error.contains("drop the flag"), "{error}");
    }

    /// The pre-existing rule is unchanged for ordinary sources.
    #[test]
    fn input_format_still_applies_to_a_single_source_corpus() {
        let args = super::Args::try_parse_from([
            "lingrep-web",
            "--dict-path",
            "/tmp/d",
            "--input",
            "a.jsonl",
            "--input-format",
            "jsonl",
        ])
        .unwrap();
        assert!(super::declared_corpora(&args).is_ok());
    }
}
