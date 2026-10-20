use std::io::{self, Write};

use crate::cli::ManpagesArgs;
use crate::cli_surface::CliSurface;
use crate::error::{CliError, FailureCode};
use crate::manpages::{publish_new, render_all};
use crate::presentation::OutputMode;
use crate::protocol::manpages::{ManpagesData, ManpagesEnvelope};
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run(args: &ManpagesArgs, mode: OutputMode) -> Result<CommandOutcome, CliError> {
    let pages = render_all(&CliSurface::new())?;
    let committed = publish_new(&args.output, &pages)?;
    let data = ManpagesData {
        output: args.output.display().to_string(),
        pages: committed,
    };
    let stdout = io::stdout();
    write_status(&mut stdout.lock(), mode, data)
}

pub(crate) fn write_status<W: Write>(
    writer: &mut W,
    mode: OutputMode,
    data: ManpagesData,
) -> Result<CommandOutcome, CliError> {
    match mode {
        OutputMode::Human => match writeln!(
            writer,
            "generated {} manual page(s) in {}",
            data.pages.len(),
            data.output
        ) {
            Ok(()) => Ok(CommandOutcome::Success),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                Ok(CommandOutcome::DisplayClosed)
            }
            Err(error) => Err(CliError::Io(error)),
        },
        OutputMode::Jsonl => write_jsonl(writer, &ManpagesEnvelope::new(data))
            .map(|()| CommandOutcome::Success)
            .map_err(|error| CliError::CommandFailure {
                code: FailureCode::PublicationCommittedOutputFailed,
                message: format!(
                    "manual pages were committed but their status record could not be written: {error}"
                ),
            }),
        OutputMode::Tsv => Err(CliError::UnsupportedOutput {
            command: "manpages",
            output: "tsv",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::diagnostic::RetryPolicy;

    struct BrokenWriter;

    impl Write for BrokenWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn machine_status_failure_after_commit_requires_changed_target() {
        let data = ManpagesData {
            output: "/tmp/committed-pages".into(),
            pages: Vec::new(),
        };
        let error = write_status(&mut BrokenWriter, OutputMode::Jsonl, data).unwrap_err();
        assert_eq!(error.code(), "publication_committed_output_failed");
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
    }
}
