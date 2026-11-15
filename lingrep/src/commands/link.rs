use std::ffi::OsString;
use std::io::Write;

use crate::catalog_input::load_and_compile_with_vectors;
use crate::cli::{
    AnalyzerArg, Cli, Command, NormalizationArg, ProfileArg, QueryInputArgs, ResourceArgs,
    SegmentationArg,
};
use crate::error::CliError;
use crate::terminal::TerminalArbiter;

use super::CommandOutcome;
use super::query_scope::parse_scope;

enum Target<'a> {
    Link(&'a str),
    Serve {
        address: &'a str,
        input: &'a QueryInputArgs,
        resources: &'a ResourceArgs,
    },
}

struct Request<'a> {
    target: Target<'a>,
    catalog: &'a crate::cli::CatalogArgs,
    clause: Option<&'a str>,
    overlaps: crate::cli::OverlapsArg,
    vectors: Option<&'a std::path::Path>,
}

fn request(cli: &Cli) -> Option<Request<'_>> {
    let (link, serve, input, resources, catalog, selection, overlaps, vectors) = match &cli.command
    {
        Command::Matches(args) => (
            &args.link,
            &args.serve,
            &args.input,
            &args.resources,
            &args.catalog,
            &args.selection,
            args.overlaps,
            &args.vectors,
        ),
        Command::Summarize(args) => (
            &args.link,
            &args.serve,
            &args.input,
            &args.resources,
            &args.catalog,
            &args.selection,
            args.overlaps,
            &args.vectors,
        ),
        _ => return None,
    };
    let target = match (link.as_deref(), serve.as_deref()) {
        (Some(link), _) => Target::Link(link),
        (None, Some(address)) => Target::Serve {
            address,
            input,
            resources,
        },
        (None, None) => return None,
    };
    Some(Request {
        target,
        catalog,
        clause: selection.clause.as_deref(),
        overlaps,
        vectors: vectors.as_deref(),
    })
}

pub fn requested(cli: &Cli) -> bool {
    request(cli).is_some()
}

pub fn run(cli: &Cli, terminal: &TerminalArbiter) -> Result<CommandOutcome, CliError> {
    let request = request(cli).expect("dispatch checked `requested`");
    let name = match request.target {
        Target::Link(link) => {
            link.split_once('=')
                .filter(|(name, base)| !name.is_empty() && !base.is_empty())
                .ok_or_else(|| {
                    CliError::InvalidArgs(
                        "--link takes NAME=URL, such as science=http://127.0.0.1:3000/".into(),
                    )
                })?
                .0
        }
        Target::Serve { .. } => "default",
    };
    let catalog =
        load_and_compile_with_vectors(&request.catalog.catalog_input()?, request.vectors)?;
    parse_scope(request.clause)?;
    let query = lingrep_web::permalink::workbench_link(
        &catalog.source.text,
        name,
        request.clause,
        request.overlaps.into(),
    )
    .map_err(|error| {
        CliError::InvalidArgs(match error {
            lingrep_web::permalink::PermalinkError::TooLarge => {
                "the catalog and clause are too large for a link".into()
            }
            lingrep_web::permalink::PermalinkError::Malformed => {
                format!("--link: `{name}` is not a corpus name")
            }
        })
    })?;
    match request.target {
        Target::Link(link) => {
            let base = link.split_once('=').map_or(link, |(_, base)| base);
            let base = base.split(['?', '#']).next().unwrap_or(base);
            let separator = if base.ends_with('/') { "" } else { "/" };
            writeln!(std::io::stdout(), "{base}{separator}{query}").map_err(CliError::Io)?;
        }
        Target::Serve {
            address,
            input,
            resources,
        } => {
            lingrep_web::serve::run_until_opening(
                &server_args(address, input, resources, request.vectors)?,
                terminal.signal_count(),
                &query,
            )
            .map_err(CliError::InvalidArgs)?;
        }
    }
    Ok(CommandOutcome::Success)
}

fn server_args(
    address: &str,
    input: &QueryInputArgs,
    resources: &ResourceArgs,
    vectors: Option<&std::path::Path>,
) -> Result<lingrep_web::Args, CliError> {
    if resources.analyzer != AnalyzerArg::Vibrato
        || resources.analyzer_config.is_some()
        || resources.profile != ProfileArg::Morphology
        || resources.segmentation != SegmentationArg::Input
        || resources.normalization != NormalizationArg::Nfc
        || resources.sudachi_dict.is_some()
        || resources.sudachi_resources.is_some()
        || resources.sudachi_split.is_some()
        || resources.sudachi_edition.is_some()
        || resources.external_executable.is_some()
        || !resources.external_args.is_empty()
    {
        return Err(CliError::InvalidArgs(
            "--serve supports only Vibrato morphology with input segmentation and NFC normalization; use ling corpus build with the desired analyzer settings, then --corpus ARTIFACT --serve".into(),
        ));
    }
    let (host, port) = address
        .rsplit_once(':')
        .filter(|(host, port)| !host.is_empty() && !port.is_empty())
        .ok_or_else(|| {
            CliError::InvalidArgs("--serve takes HOST:PORT, such as --serve=127.0.0.1:3000".into())
        })?;
    let corpus = input
        .corpus
        .as_ref()
        .or(input.root.as_ref())
        .filter(|path| path.as_os_str() != "-")
        .ok_or_else(|| CliError::InvalidArgs("--serve cannot read the corpus from stdin".into()))?;
    let arguments: [OsString; 7] = [
        "ling serve".into(),
        "--host".into(),
        host.into(),
        "--port".into(),
        port.into(),
        "--input".into(),
        corpus.into(),
    ];
    let mut args = <lingrep_web::Args as clap::Parser>::try_parse_from(arguments)
        .map_err(|error| CliError::InvalidArgs(error.to_string()))?;
    args.dict.dict_path.clone_from(&input.dict_path);
    args.input.input_format = input.input_format;
    args.markdown = input.markdown.clone();
    if let Some(vectors) = vectors {
        args.vectors = Some(vectors.to_owned());
    }
    Ok(args)
}
