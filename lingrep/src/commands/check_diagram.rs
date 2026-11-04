use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use lingrep_cli_common::SafeText;
use lingrep_core::fsm::{CompileLimits, DiagramCatalog, PatternSource, render_diagnostics};

use crate::catalog_input::LoadedCatalog;
use crate::cli::{CheckArgs, DiagramFormat};
use crate::error::{CliError, FailureCode};
use crate::terminal::TerminalArbiter;

use super::CommandOutcome;

const MAX_OUTPUT_BYTES: usize = 8 << 20;
const MAX_FILE_STEM_BYTES: usize = 64;

pub(super) fn run(
    args: &CheckArgs,
    catalog: &LoadedCatalog,
    terminal: &TerminalArbiter,
) -> Result<CommandOutcome, CliError> {
    if args.output_dir.is_some() && args.diagram_format != DiagramFormat::Svg {
        return Err(CliError::InvalidArgs(
            "--output-dir requires --diagram-format svg".into(),
        ));
    }
    let diagrams = DiagramCatalog::parse(
        PatternSource::named(&catalog.source.name, &catalog.source.text),
        CompileLimits::default(),
    )
    .map_err(|error| CliError::PatternParse(error.to_string()))?;
    let available = diagrams.names().collect::<Vec<_>>();
    for name in &args.diagram_names {
        if !available.contains(&name.as_str()) {
            let error = diagrams.diagram(name, false).err().map_or_else(
                || format!("no diagram for {name}"),
                |error| error.to_string(),
            );
            return Err(CliError::InvalidArgs(error));
        }
    }
    let selected = available
        .into_iter()
        .filter(|name| {
            args.diagram_names.is_empty()
                || args.diagram_names.iter().any(|requested| requested == name)
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(CliError::InvalidArgs(
            "the catalog has no patterns or fragments to draw".into(),
        ));
    }
    if args.diagram_format == DiagramFormat::Svg && args.output_dir.is_none() && selected.len() != 1
    {
        return Err(CliError::InvalidArgs(
            "SVG output on stdout requires one definition; select it with --name NAME or write \
             one file per definition with --output-dir DIR"
                .into(),
        ));
    }
    let width = args.diagram_width.map_or_else(
        || {
            if args.output_dir.is_none() && io::stdout().is_terminal() {
                terminal_size::terminal_size()
                    .map_or(80, |(width, _)| usize::from(width.0).clamp(32, 240))
            } else {
                80
            }
        },
        usize::from,
    );
    if let Some(destination) = &args.output_dir {
        let Some(receipt) = publish_svg_files(
            &diagrams,
            &selected,
            args.expand,
            width,
            destination,
            terminal,
        )?
        else {
            return Ok(CommandOutcome::Cancelled);
        };
        write_diagnostics(catalog)?;
        return write_receipt(&mut io::stdout().lock(), &receipt);
    }
    let mut output = if args.diagram_format == DiagramFormat::Svg {
        String::new()
    } else {
        diagrams
            .render_publication_note(width)
            .map_err(|error| CliError::PatternParse(error.to_string()))?
    };
    for name in selected {
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
        let rendered = render(&diagrams, name, args.expand, width, args.diagram_format)?;
        if output.len() + rendered.len() + 1 > MAX_OUTPUT_BYTES {
            return Err(output_limit_exceeded());
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&rendered);
    }
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    write_diagnostics(catalog)?;
    write_output(&mut io::stdout().lock(), &output, args.diagram_format)
}

fn render(
    diagrams: &DiagramCatalog,
    name: &str,
    expand: bool,
    width: usize,
    format: DiagramFormat,
) -> Result<String, CliError> {
    let diagram = diagrams
        .diagram(name, expand)
        .map_err(|error| CliError::PatternParse(format!("cannot draw {name}: {error}")))?;
    match format {
        DiagramFormat::Unicode => diagram.render_text(width, false),
        DiagramFormat::Ascii => diagram.render_text(width, true),
        DiagramFormat::Svg => diagram.render_svg(width),
    }
    .map_err(|error| CliError::PatternParse(format!("cannot draw {name}: {error}")))
}

fn output_limit_exceeded() -> CliError {
    CliError::InvalidArgs(
        "diagram output exceeds 8 MiB; select fewer definitions with --name".into(),
    )
}

/// Returns `None`, leaving no `destination`, if cancellation is requested
/// before the staging directory is renamed to `destination`.
fn publish_svg_files(
    diagrams: &DiagramCatalog,
    selected: &[&str],
    expand: bool,
    width: usize,
    destination: &Path,
    terminal: &TerminalArbiter,
) -> Result<Option<Vec<(String, PathBuf)>>, CliError> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".ling-diagrams-")
        .tempdir_in(parent)?;
    let ordinal_width = selected.len().to_string().len();
    let mut receipt = Vec::with_capacity(selected.len());
    let mut written = 0;
    for (index, name) in selected.iter().enumerate() {
        if terminal.cancellation().is_cancelled() {
            return Ok(None);
        }
        let svg = render(diagrams, name, expand, width, DiagramFormat::Svg)?;
        written += svg.len();
        if written > MAX_OUTPUT_BYTES {
            return Err(output_limit_exceeded());
        }
        let file_name = svg_file_name(index + 1, ordinal_width, name);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.path().join(&file_name))?
            .write_all(svg.as_bytes())?;
        receipt.push(((*name).to_owned(), destination.join(file_name)));
    }
    if !terminal.try_claim_completion() {
        return Ok(None);
    }
    lingrep_corpus::rename_noreplace(staging.path(), destination).map_err(|error| {
        match error.kind() {
            io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty => {
                CliError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", destination.display()),
                ))
            }
            _ => CliError::Io(error),
        }
    })?;
    let _published = staging.keep();
    Ok(Some(receipt))
}

fn svg_file_name(ordinal: usize, ordinal_width: usize, name: &str) -> String {
    let mut stem = String::new();
    for character in name.chars() {
        let character = if character.is_alphanumeric() || character == '_' {
            character
        } else {
            '_'
        };
        if stem.len() + character.len_utf8() > MAX_FILE_STEM_BYTES {
            break;
        }
        stem.push(character);
    }
    format!("{ordinal:0ordinal_width$}-{stem}.svg")
}

fn write_diagnostics(catalog: &LoadedCatalog) -> Result<(), CliError> {
    if !catalog.output.diagnostics.is_empty() {
        let mut stderr = io::stderr().lock();
        for line in
            render_diagnostics(Some(&catalog.source.text), &catalog.output.diagnostics).lines()
        {
            writeln!(stderr, "{}", SafeText::from_untrusted(line))?;
        }
    }
    Ok(())
}

fn write_receipt(
    writer: &mut impl Write,
    receipt: &[(String, PathBuf)],
) -> Result<CommandOutcome, CliError> {
    let result = receipt.iter().try_for_each(|(name, path)| {
        writeln!(
            writer,
            "{}\t{}",
            SafeText::from_untrusted(name),
            SafeText::from_untrusted(&path.display().to_string())
        )
    });
    match result.and_then(|()| writer.flush()) {
        Ok(()) => Ok(CommandOutcome::Success),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            Ok(CommandOutcome::DisplayClosed)
        }
        Err(error) => Err(CliError::CommandFailure {
            code: FailureCode::PublicationCommittedOutputFailed,
            message: format!(
                "diagrams were published but their receipt could not be written: {error}"
            ),
        }),
    }
}

fn write_output(
    writer: &mut impl Write,
    output: &str,
    format: DiagramFormat,
) -> Result<CommandOutcome, CliError> {
    match writer
        .write_all(output.as_bytes())
        .and_then(|()| writer.flush())
    {
        Ok(()) => Ok(CommandOutcome::Success),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe && format != DiagramFormat::Svg => {
            Ok(CommandOutcome::DisplayClosed)
        }
        Err(error) => Err(CliError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn svg_file_names_keep_only_word_characters_within_the_stem_bound() {
        for (ordinal, width, name, expected) in [
            (7, 2, "../a/b\u{202e}", "07-___a_b_.svg"),
            (1, 1, "", "1-.svg"),
            (3, 3, "CON:\u{0}\n", "003-CON___.svg"),
            (1, 1, &"x".repeat(65), &format!("1-{}.svg", "x".repeat(64))),
            (
                1,
                1,
                &"動".repeat(22),
                &format!("1-{}.svg", "動".repeat(21)),
            ),
        ] {
            assert_eq!(svg_file_name(ordinal, width, name), expected, "{name:?}");
        }
    }

    #[test]
    fn closed_output_returns_display_closed_for_text_and_an_io_error_for_svg() {
        for format in [DiagramFormat::Unicode, DiagramFormat::Ascii] {
            assert_eq!(
                write_output(&mut Closed, "diagram", format).unwrap(),
                CommandOutcome::DisplayClosed
            );
        }
        assert!(matches!(
            write_output(&mut Closed, "<svg/>", DiagramFormat::Svg),
            Err(CliError::Io(_))
        ));
    }
}
