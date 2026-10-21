use std::io::{self, Write};
use std::path::Path;

use crate::catalog_input::load_and_compile;
use crate::cli::{Cli, Command, CorpusCommand, QueryInputArgs};
use crate::error::CliError;
use crate::presentation::{ExplicitLimit, OutputMode};
use crate::protocol::invocation::{InvocationData, InvocationEnvelope};
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn requested(cli: &Cli) -> bool {
    match &cli.command {
        Command::Matches(args) => args.plan,
        Command::Summarize(args) => args.plan,
        Command::Tokenize(args) => args.plan,
        Command::Attributes(args) => args.plan,
        Command::SentenceFinal(args) => args.plan,
        Command::Corpus(args) => match &args.command {
            CorpusCommand::Build(args) => args.plan,
            CorpusCommand::Inspect(args) => args.plan,
            CorpusCommand::Sources(args) => args.plan,
            CorpusCommand::Variants(args) => args.plan,
        },
        Command::Doctor(_)
        | Command::Dictionary(_)
        | Command::Check(_)
        | Command::Explore(_)
        | Command::Describe(_)
        | Command::Completions(_)
        | Command::Manpages(_)
        | Command::Schema(_) => false,
    }
}

pub fn run(cli: &Cli) -> Result<CommandOutcome, CliError> {
    let (command, input_kind, limits) = resolve(cli)?;
    let (catalog_source, catalog_fingerprint) = resolve_catalog(cli)?;
    let (dictionary_path, dictionary_source) = resolve_dictionary(cli)?;
    let data = InvocationData {
        command,
        catalog_source,
        catalog_fingerprint,
        dictionary_path,
        dictionary_source,
        input_kind,
        output_mode: match cli.output_mode() {
            OutputMode::Human => "human",
            OutputMode::Jsonl => "json",
            OutputMode::Tsv => "tsv",
        }
        .into(),
        color: format!("{:?}", cli.color).to_lowercase(),
        progress: progress(cli),
        limits,
        schemas: schemas(cli),
    };
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    match cli.output_mode() {
        OutputMode::Jsonl => write_jsonl(&mut writer, &InvocationEnvelope::new(data))
            .map_err(|error| CliError::Io(io::Error::other(error)))?,
        OutputMode::Human | OutputMode::Tsv => {
            writeln!(writer, "command: {}", data.command).map_err(CliError::Io)?;
            writeln!(writer, "input: {}", data.input_kind).map_err(CliError::Io)?;
            writeln!(writer, "output: {}", data.output_mode).map_err(CliError::Io)?;
            writeln!(writer, "color: {}", data.color).map_err(CliError::Io)?;
            writeln!(writer, "progress: {}", data.progress).map_err(CliError::Io)?;
            writeln!(writer, "limits: {}", data.limits).map_err(CliError::Io)?;
        }
    }
    Ok(CommandOutcome::Success)
}

fn resolve(cli: &Cli) -> Result<(String, String, serde_json::Value), CliError> {
    match &cli.command {
        Command::Matches(args) => query(
            "matches",
            &args.input,
            &args.resources,
            args.context,
            args.limit.as_ref().map(|v| v.resolve()),
            args.cache.cache,
        ),
        Command::Summarize(args) => query(
            "summarize",
            &args.input,
            &args.resources,
            None,
            args.top.as_ref().map(|v| v.resolve()),
            args.cache.cache,
        ),
        Command::Tokenize(args) => query(
            "tokenize",
            &args.input,
            &args.resources,
            None,
            args.limit.as_ref().map(|v| v.resolve()),
            args.cache.cache,
        ),
        Command::Attributes(args) => query(
            "coverage",
            &args.input,
            &args.resources,
            None,
            None,
            args.cache.cache,
        ),
        Command::SentenceFinal(args) => query(
            "sentence-final",
            &args.query.input,
            &args.query.resources.as_resource_args(),
            None,
            None,
            false,
        ),
        Command::Corpus(args) => match &args.command {
            CorpusCommand::Build(args) => {
                stat(&args.root)?;
                Ok((
                    "corpus build".into(),
                    "direct".into(),
                    serde_json::json!({"workers": args.resources.workers}),
                ))
            }
            CorpusCommand::Inspect(args) => {
                read_manifest(&args.artifact)?;
                Ok((
                    "corpus inspect".into(),
                    "prepared".into(),
                    serde_json::json!({"verify": args.verify, "documents": args.documents}),
                ))
            }
            CorpusCommand::Sources(args) => {
                stat(&args.root)?;
                Ok((
                    "corpus inventory".into(),
                    "direct".into(),
                    serde_json::json!({"max_documents": args.traversal.max_documents}),
                ))
            }
            CorpusCommand::Variants(args) => {
                stat(&args.target)?;
                Ok((
                    "corpus variants".into(),
                    "prepared".into(),
                    serde_json::json!({
                        "limit": args.limit.and_then(ExplicitLimit::resolve),
                        "prune": args.prune,
                    }),
                ))
            }
        },
        _ => Err(CliError::InvalidArgs(
            "--plan is not supported by this command".into(),
        )),
    }
}

fn query(
    command: &str,
    input: &QueryInputArgs,
    resources: &crate::cli::ResourceArgs,
    context: Option<usize>,
    limit: Option<Option<u64>>,
    cache_requested: bool,
) -> Result<(String, String, serde_json::Value), CliError> {
    let input_kind = if let Some(artifact) = &input.corpus {
        read_manifest(artifact)?;
        "prepared"
    } else if input.root.as_deref() == Some(Path::new("-")) {
        stat(&input.direct_dictionary_path()?)?;
        "stdin"
    } else {
        let root = input
            .root
            .as_ref()
            .ok_or_else(|| CliError::InvalidArgs("ROOT or --corpus is required".into()))?;
        stat(root)?;
        let dictionary = input.direct_dictionary_path()?;
        stat(&dictionary)?;
        if cache_requested { "cached" } else { "direct" }
    };
    Ok((
        command.into(),
        input_kind.into(),
        serde_json::json!({
            "workers": resources.workers,
            "context": context,
            "limit": limit.flatten(),
            "batch_sentences": resources.batch_sentences,
            "batch_raw_bytes": resources.batch_raw_bytes,
            "max_record_bytes": resources.max_record_bytes,
            "max_tokenized_bytes": resources.max_tokenized_bytes,
        }),
    ))
}

fn resolve_catalog(cli: &Cli) -> Result<(Option<String>, Option<String>), CliError> {
    // Optional catalog: `None` only when neither spelling was given. Falling
    // through to the catch-all below would report no provenance for a run that
    // compiles a catalog and reports exclusions from it.
    if let Command::Attributes(args) = &cli.command {
        return match args.catalog.catalog_input() {
            None => Ok((None, None)),
            Some(input) => {
                let catalog = load_and_compile(&input)?;
                Ok((Some(catalog.source.name), Some(catalog.fingerprint)))
            }
        };
    }
    let catalog = match &cli.command {
        Command::Matches(args) => Some(&args.catalog),
        Command::Summarize(args) => Some(&args.catalog),
        Command::SentenceFinal(args) => Some(&args.query.catalog),
        Command::Tokenize(_) | Command::Corpus(_) => None,
        _ => None,
    };
    let Some(catalog) = catalog else {
        return Ok((None, None));
    };
    let catalog = load_and_compile(&catalog.catalog_input()?)?;
    Ok((Some(catalog.source.name), Some(catalog.fingerprint)))
}

fn resolve_dictionary(cli: &Cli) -> Result<(Option<String>, Option<String>), CliError> {
    let explicit_or_env = |input: &QueryInputArgs| -> Result<_, CliError> {
        if input.corpus.is_some() {
            return Ok((None, Some("prepared".into())));
        }
        let source = if input.dict_path.is_some() {
            "argument"
        } else if std::env::var_os("LINGREP_DICTIONARY_PATH").is_some_and(|value| !value.is_empty())
        {
            "environment"
        } else {
            "installed"
        };
        let path = input.direct_dictionary_path()?;
        Ok((Some(path.display().to_string()), Some(source.into())))
    };
    match &cli.command {
        Command::Matches(args) => explicit_or_env(&args.input),
        Command::Summarize(args) => explicit_or_env(&args.input),
        Command::Tokenize(args) => explicit_or_env(&args.input),
        Command::Attributes(args) => explicit_or_env(&args.input),
        Command::SentenceFinal(args) => explicit_or_env(&args.query.input),
        Command::Corpus(args) => match &args.command {
            CorpusCommand::Build(args) => Ok((
                args.dict_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                Some("argument".into()),
            )),
            CorpusCommand::Inspect(_) | CorpusCommand::Sources(_) | CorpusCommand::Variants(_) => {
                Ok((None, None))
            }
        },
        _ => Ok((None, None)),
    }
}

fn schemas(cli: &Cli) -> Vec<String> {
    if cli.output_mode() != OutputMode::Jsonl {
        return Vec::new();
    }
    let schema = match &cli.command {
        Command::Matches(_) | Command::Summarize(_) => "lingrep.query/v2",
        Command::Tokenize(_) => "lingrep.tokenize/v1",
        Command::Attributes(_) => "lingrep.attributes/v1",
        Command::SentenceFinal(_) => "lingrep.sentence-final/v1",
        Command::Corpus(_) => "lingrep.sources/v1",
        _ => return Vec::new(),
    };
    vec![schema.into(), "lingrep.diagnostic/v1".into()]
}

fn stat(path: &Path) -> Result<(), CliError> {
    std::fs::metadata(path).map(|_| ()).map_err(|error| {
        CliError::Io(io::Error::new(
            error.kind(),
            format!("{}: {error}", path.display()),
        ))
    })
}

fn read_manifest(artifact: &Path) -> Result<(), CliError> {
    let path = artifact.join("manifest.json");
    let bytes = std::fs::read(&path).map_err(CliError::Io)?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .map(|_| ())
        .map_err(|error| CliError::Parse(format!("{}: {error}", path.display())))
}

fn progress(cli: &Cli) -> String {
    let value = match &cli.command {
        Command::Matches(args) => args.resources.progress,
        Command::Summarize(args) => args.resources.progress,
        Command::Tokenize(args) => args.resources.progress,
        Command::Attributes(args) => args.resources.progress,
        Command::SentenceFinal(args) => args.query.resources.progress,
        Command::Corpus(args) => match &args.command {
            CorpusCommand::Build(args) => args.resources.progress,
            CorpusCommand::Sources(args) => args.progress,
            CorpusCommand::Inspect(_) | CorpusCommand::Variants(_) => {
                crate::presentation::ProgressMode::Never
            }
        },
        _ => crate::presentation::ProgressMode::Never,
    };
    format!("{value:?}").to_lowercase()
}
