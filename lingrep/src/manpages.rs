//! Deterministic manual-page rendering and no-replace publication.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use clap::Command;

use crate::cli_surface::CliSurface;
use crate::error::{CliError, FailureCode};
use crate::examples::{examples_for, render_posix};
use crate::protocol::manpages::PageData;

pub struct RenderedPage {
    pub name: String,
    pub bytes: Vec<u8>,
}

pub fn render_all(surface: &CliSurface) -> Result<Vec<RenderedPage>, CliError> {
    let mut paths = Vec::new();
    collect_paths(&surface.command, "", &mut paths);
    paths.into_iter().map(|path| render_page(&path)).collect()
}

fn collect_paths(command: &Command, path: &str, paths: &mut Vec<String>) {
    paths.push(path.to_owned());
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set() && child.get_name() != "help")
    {
        let child_path = if path.is_empty() {
            child.get_name().to_owned()
        } else {
            format!("{path} {}", child.get_name())
        };
        collect_paths(child, &child_path, paths);
    }
}

fn render_page(path: &str) -> Result<RenderedPage, CliError> {
    let fresh = CliSurface::new();
    let command = path
        .split_whitespace()
        .try_fold(&fresh.command, |command, component| {
            command.find_subcommand(component)
        });
    let command = command.ok_or_else(|| CliError::CommandFailure {
        code: FailureCode::Io,
        message: format!("public command disappeared while rendering manual page: {path}"),
    })?;
    let mut bytes = Vec::new();
    clap_mangen::Man::new(command.clone())
        .section("1")
        .date("")
        .render(&mut bytes)
        .map_err(CliError::Io)?;
    let semantic_path = if path.is_empty() { "ling" } else { path };
    let examples = examples_for(semantic_path);
    if !examples.is_empty() {
        let mut section = roff::Roff::new();
        section.control("SH", ["EXAMPLES"]);
        for example in examples {
            section.text([roff::roman(format!(
                "ling {} — {}",
                render_posix(example),
                example.purpose
            ))]);
        }
        bytes.extend_from_slice(section.render().as_bytes());
    }
    Ok(RenderedPage {
        name: page_name(path),
        bytes,
    })
}

fn page_name(path: &str) -> String {
    if path.is_empty() {
        "ling.1".into()
    } else {
        format!("ling-{}.1", path.replace(' ', "-"))
    }
}

pub fn publish_new(output: &Path, pages: &[RenderedPage]) -> Result<Vec<PageData>, CliError> {
    fs::create_dir(output).map_err(CliError::Io)?;
    for page in pages {
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output.join(&page.name))?;
            file.write_all(&page.bytes)?;
            file.flush()
        })();
        if let Err(write_error) = result {
            return match fs::remove_dir_all(output) {
                Ok(()) => Err(CliError::Io(write_error)),
                Err(cleanup_error) => Err(cleanup_failure(output, &write_error, &cleanup_error)),
            };
        }
    }
    Ok(pages
        .iter()
        .map(|page| PageData {
            name: page.name.clone(),
            bytes: page.bytes.len() as u64,
        })
        .collect())
}

fn cleanup_failure(
    output: &Path,
    write_error: &std::io::Error,
    cleanup_error: &std::io::Error,
) -> CliError {
    CliError::CommandFailure {
        code: FailureCode::ManpageCleanupFailed,
        message: format!(
            "failed to publish manual page in owned partial directory {}: {write_error}; cleanup also failed: {cleanup_error}",
            output.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::diagnostic::RetryPolicy;

    #[test]
    fn cleanup_failure_names_the_owned_partial_directory() {
        let error = cleanup_failure(
            Path::new("/tmp/owned-partial-pages"),
            &std::io::Error::other("page write failed"),
            &std::io::Error::other("cleanup failed"),
        );
        assert_eq!(error.code(), "manpage_cleanup_failed");
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
        assert!(error.to_string().contains("/tmp/owned-partial-pages"));
    }
}
