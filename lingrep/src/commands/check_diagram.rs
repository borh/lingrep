use std::io::{self, IsTerminal, Write};

use lingrep_cli_common::SafeText;
use lingrep_core::fsm::{CompileLimits, DiagramCatalog, PatternSource, render_diagnostics};

use crate::catalog_input::LoadedCatalog;
use crate::cli::{CheckArgs, DiagramFormat};
use crate::error::CliError;
use crate::terminal::TerminalArbiter;

use super::CommandOutcome;

pub(super) fn run(
    args: &CheckArgs,
    catalog: &LoadedCatalog,
    terminal: &TerminalArbiter,
) -> Result<CommandOutcome, CliError> {
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
    if args.diagram_format == DiagramFormat::Svg && selected.len() != 1 {
        return Err(CliError::InvalidArgs(
            "SVG output requires one definition; select it with --name NAME".into(),
        ));
    }
    let width = args.diagram_width.map_or_else(
        || {
            if io::stdout().is_terminal() {
                terminal_size::terminal_size()
                    .map_or(80, |(width, _)| usize::from(width.0).clamp(32, 240))
            } else {
                80
            }
        },
        usize::from,
    );
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
        let diagram = diagrams
            .diagram(name, args.expand)
            .map_err(|error| CliError::PatternParse(format!("cannot draw {name}: {error}")))?;
        let rendered = match args.diagram_format {
            DiagramFormat::Unicode => diagram.render_text(width, false),
            DiagramFormat::Ascii => diagram.render_text(width, true),
            DiagramFormat::Svg => diagram.render_svg(width),
        }
        .map_err(|error| CliError::PatternParse(format!("cannot draw {name}: {error}")))?;
        if output.len() + rendered.len() + 1 > 8 << 20 {
            return Err(CliError::InvalidArgs(
                "diagram output exceeds 8 MiB; select fewer definitions with --name".into(),
            ));
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&rendered);
    }
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    if !catalog.output.diagnostics.is_empty() {
        let mut stderr = io::stderr().lock();
        for line in
            render_diagnostics(Some(&catalog.source.text), &catalog.output.diagnostics).lines()
        {
            writeln!(stderr, "{}", SafeText::from_untrusted(line))?;
        }
    }
    write_output(&mut io::stdout().lock(), &output, args.diagram_format)
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
