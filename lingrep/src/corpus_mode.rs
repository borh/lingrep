//! Which corpus a query reads: ROOT directly, or a prepared artifact.
//!
//! Filesystem queries analyze ROOT directly by default. `--cache` explicitly
//! requests reuse of a compatible prepared artifact beside ROOT, or creation
//! when none exists. Stdin has no reopenable name and cannot be cached.
//!
//! Resolution happens before query execution. For `--cache`, it observes ROOT
//! in full to derive the exact source identity, then states the same build
//! request `corpus build` states. The two agree on which artifact answers to a
//! source by construction rather than by convention.

use std::path::{Path, PathBuf};

use lingrep_cli_common::MarkdownReaderArgs;

use crate::cli::{CacheArgs, ExecutionCliConfig, QueryInputArgs};
use crate::commands::corpus::{BuildRequest, EnsuredArtifact, PublicationRole, ensure_artifact};
use crate::error::CliError;
use crate::presentation::ProgressMode;
use crate::terminal::TerminalArbiter;

/// Which corpus the run will read, and how that was decided.
#[derive(Debug)]
pub struct ResolvedCorpus {
    /// The artifact to read, or `None` to analyze ROOT directly.
    pub corpus: Option<PathBuf>,
    /// How the corpus was chosen.
    ///
    /// *Which* corpus answered is already in the run record: `RunCorpus`
    /// distinguishes `prepared` — with the artifact's own source and analysis
    /// identity — from `direct`, and everything a consumer must treat
    /// differently between the two paths follows from that distinction rather
    /// than from this one. So this is reported to a person, not to the
    /// protocol, and no contract moves.
    pub origin: CorpusOrigin,
}

/// How a run's corpus was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorpusOrigin {
    /// The caller named an artifact with `--corpus`.
    Addressed,
    /// A matching artifact was found beside ROOT and reused.
    Reused,
    /// No artifact matched, so `--cache` built one.
    Prepared,
    /// ROOT is analyzed directly because caching was not requested.
    Direct,
}

/// Decides which corpus a query reads, before any reading begins.
///
/// `markdown` is the query's own section policy: it decides which lines become
/// sentences at all, so it is part of what the artifact *is* and reaches the
/// identity this resolution matches against.
pub fn resolve(
    input: &QueryInputArgs,
    cache: &CacheArgs,
    markdown: &MarkdownReaderArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    progress: &mut impl std::io::Write,
) -> Result<ResolvedCorpus, CliError> {
    let resolved = decide(input, cache, markdown, config, terminal, progress)?;
    if config.progress == ProgressMode::Always {
        match (&resolved.corpus, resolved.origin) {
            (Some(path), CorpusOrigin::Reused) => writeln!(
                progress,
                "corpus: reusing the prepared artifact at {}",
                path.display()
            ),
            (Some(path), CorpusOrigin::Prepared) => {
                writeln!(progress, "corpus: prepared and reading {}", path.display())
            }
            (Some(path), CorpusOrigin::Addressed) => {
                writeln!(progress, "corpus: reading {}", path.display())
            }
            (_, _) => writeln!(
                progress,
                "corpus: analyzing the input directly; no prepared artifact was read or written"
            ),
        }
        .map_err(CliError::Io)?;
    }
    Ok(resolved)
}

/// Analyzing ROOT directly: no artifact is read and none is written.
fn direct_run() -> ResolvedCorpus {
    ResolvedCorpus {
        corpus: None,
        origin: CorpusOrigin::Direct,
    }
}

/// What the decision settles without reading anything.
#[derive(Debug)]
enum Preflight {
    /// Settled: this is the corpus, whatever is on disk.
    Decided(ResolvedCorpus),
    /// Not settled: look beside this root for a matching artifact.
    Search(PathBuf),
}

/// Settles explicit/direct inputs and identifies named roots that may be cached.
fn preflight(input: &QueryInputArgs, cache: &CacheArgs) -> Preflight {
    if let Some(path) = &input.corpus {
        return Preflight::Decided(ResolvedCorpus {
            corpus: Some(path.clone()),
            origin: CorpusOrigin::Addressed,
        });
    }
    let direct = || Preflight::Decided(direct_run());
    if !cache.cache {
        return direct();
    }
    let Some(root) = input.root.as_deref() else {
        return direct();
    };
    // Stdin has no reopenable logical name, so it can neither address an
    // artifact nor be the source of one. The explicit request is rejected by
    // `decide`; preflight keeps only side-effect-free routing decisions.
    if root == Path::new("-") {
        return direct();
    }
    Preflight::Search(root.to_path_buf())
}

fn decide(
    input: &QueryInputArgs,
    cache: &CacheArgs,
    markdown: &MarkdownReaderArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    progress: &mut impl std::io::Write,
) -> Result<ResolvedCorpus, CliError> {
    if cache.cache && input.root.as_deref() == Some(Path::new("-")) {
        return Err(CliError::InvalidArgs(
            "--cache requires a filesystem ROOT; stdin cannot be reopened".into(),
        ));
    }
    let root = match preflight(input, cache) {
        Preflight::Decided(resolved) => return Ok(resolved),
        Preflight::Search(root) => root,
    };

    let request = BuildRequest {
        root,
        output: None,
        dict_path: input.dict_path.clone(),
        markdown: markdown.resolve().map_err(CliError::InvalidArgs)?,
        input_format: input.input_format,
    };
    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;
    let found = runtime.block_on(ensure_artifact(
        &request,
        config,
        terminal,
        true,
        PublicationRole::IntermediateCommit,
        progress,
    ));
    // Caching is explicit, so lookup and preparation failures must be visible
    // rather than silently changing the requested execution mode.
    let found = found?;
    match found {
        Some(EnsuredArtifact::Reused(path)) => Ok(ResolvedCorpus {
            origin: CorpusOrigin::Reused,
            corpus: Some(path),
        }),
        Some(EnsuredArtifact::Prepared(path)) => Ok(ResolvedCorpus {
            origin: CorpusOrigin::Prepared,
            corpus: Some(path),
        }),
        // Cancelled, or nothing to reuse and nothing asked to be built. A
        // cancelled run stops at the terminal's own check; an absent artifact
        // is simply a direct run.
        None => Ok(direct_run()),
    }
}

#[cfg(test)]
mod tests {
    // Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;

    fn input(root: Option<&str>) -> QueryInputArgs {
        QueryInputArgs {
            dict_path: None,
            corpus: None,
            markdown: MarkdownReaderArgs::default(),
            root: root.map(PathBuf::from),
            input_format: None,
        }
    }

    fn decided(args: &QueryInputArgs, cache: &CacheArgs) -> ResolvedCorpus {
        match preflight(args, cache) {
            Preflight::Decided(resolved) => resolved,
            Preflight::Search(root) => {
                panic!(
                    "expected a decision without reading, got a search of {}",
                    root.display()
                )
            }
        }
    }

    #[test]
    fn an_addressed_artifact_is_taken_as_spelled() {
        let mut args = input(None);
        args.corpus = Some(PathBuf::from("corpus.lingcorpus"));
        let resolved = decided(&args, &CacheArgs::default());
        assert_eq!(resolved.origin, CorpusOrigin::Addressed);
        assert_eq!(
            resolved.corpus.as_deref(),
            Some(Path::new("corpus.lingcorpus"))
        );
    }

    #[test]
    fn default_declines_an_artifact_without_looking_for_one() {
        let args = input(Some("corpus.jsonl"));
        let resolved = decided(&args, &CacheArgs::default());
        assert_eq!(resolved.origin, CorpusOrigin::Direct);
        assert_eq!(resolved.corpus, None);
    }

    /// A filesystem root is cached only when requested.
    #[test]
    fn cache_searches_for_a_filesystem_root() {
        let args = input(Some("corpus.jsonl"));
        match preflight(&args, &CacheArgs { cache: true }) {
            Preflight::Search(root) => assert_eq!(root, PathBuf::from("corpus.jsonl")),
            other => panic!("expected a search, got {other:?}"),
        }
    }

    #[test]
    fn stdin_resolves_directly_by_default() {
        let input = input(Some("-"));
        assert_eq!(
            decided(&input, &CacheArgs::default()).origin,
            CorpusOrigin::Direct
        );
    }
}
