//! `corpus prune` — deletes the superseded and older-format realizations that
//! `corpus disk` reports, and the count tables bound only to them.
//!
//! Lists what it would delete unless `--execute` is given. Every deletion goes
//! through a `lingrep_corpus` removal that re-checks, immediately before
//! deleting, that the entry is still the canonical artifact or table the scan
//! classified.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use lingrep_corpus::{CountTable, finish_prune_leftover, remove_older_format, remove_realization};

use super::CommandOutcome;
use super::disk::{self, CurrentSelection, RealizationEntry, Scope, TableEntry, display};
use crate::cli::{CorpusPruneArgs, ExecutionCliConfig};
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::disk::{LeftoverStatus, PruneKeep, RealizationStatus, TableBinding};
use crate::protocol::prune::{
    PruneComplete, PruneEnvelope, PruneEvent, PruneKept, PruneRemoval, PruneRun, RemovalReason,
};
use crate::protocol::write_jsonl;
use crate::terminal::TerminalArbiter;

pub fn run(
    args: &CorpusPruneArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let stdout = io::stdout();
    run_to(args, config, terminal, output, &mut stdout.lock())
}

enum Planned<'a> {
    /// The hidden directory of an interrupted prune, and its size.
    Leftover(&'a Path, Option<u64>),
    Realization(&'a RealizationEntry),
    Table(&'a TableEntry),
}

pub(crate) fn run_to<W: Write>(
    args: &CorpusPruneArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
    writer: &mut W,
) -> Result<CommandOutcome, CliError> {
    let scope = Scope::new(args.directory.as_deref(), args.source.as_deref())?;
    let Some(scan) = disk::scan(&scope, Some(&args.inputs), config, terminal, output)? else {
        return Ok(CommandOutcome::Cancelled);
    };

    let kept = keep(&args.keep, &scan.realizations, &scope.directory)?;
    let keep_reason = |entry: &RealizationEntry| {
        entry.kept_by_prune.filter(|reason| {
            !(args.include_all_superseded && reason.deleted_with_include_all_superseded())
        })
    };
    let kept_by_rule = scan
        .realizations
        .iter()
        .filter(|entry| !kept.contains(&entry.path))
        .filter_map(|entry| match entry.status {
            RealizationStatus::Superseded | RealizationStatus::OlderFormat => {
                keep_reason(entry).map(|reason| (entry, reason))
            }
            RealizationStatus::Unreadable if entry.recorded.is_some() => {
                Some((entry, PruneKeep::Damaged))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let removed = scan
        .realizations
        .iter()
        .filter(|entry| match entry.status {
            RealizationStatus::Superseded | RealizationStatus::OlderFormat => {
                !kept.contains(&entry.path) && keep_reason(entry).is_none()
            }
            _ => false,
        })
        .collect::<Vec<_>>();

    // A source whose current realization is absent would lose every readable
    // realization; that needs an explicit choice of what to keep.
    let anchored = scan
        .realizations
        .iter()
        .filter(|entry| kept.contains(&entry.path))
        .filter_map(|entry| entry.source.as_deref())
        .collect::<BTreeSet<_>>();
    let unanchored = scan
        .sources
        .iter()
        .filter(|source| {
            !matches!(source.current, CurrentSelection::Present(_))
                && !anchored.contains(source.name.as_str())
                && removed
                    .iter()
                    .any(|entry| entry.source.as_deref() == Some(source.name.as_str()))
        })
        .map(|source| match &source.current {
            CurrentSelection::Absent(path) => format!(
                "{} has no current artifact (corpus build would write {})",
                source.name,
                path.display()
            ),
            _ => format!(
                "{} is missing, so no artifact of it is current",
                source.name
            ),
        })
        .collect::<Vec<_>>();
    if !unanchored.is_empty() {
        return Err(CliError::InvalidArgs(format!(
            "nothing was deleted: {}; build the current artifact or name one to keep with --keep ARTIFACT",
            unanchored.join("; ")
        )));
    }

    let removed_paths = removed
        .iter()
        .map(|entry| &entry.path)
        .collect::<BTreeSet<_>>();
    let tables = scan.tables.iter().filter(|table| {
        table.binding == TableBinding::Superseded
            && !table.artifacts.is_empty()
            && table
                .artifacts
                .iter()
                .all(|artifact| removed_paths.contains(artifact))
    });
    let interrupted = scan.leftovers.iter().filter_map(|leftover| {
        (leftover.status == LeftoverStatus::InterruptedPrune)
            .then_some(leftover.pruning.as_deref())
            .flatten()
            .map(|path| Planned::Leftover(path, leftover.bytes))
    });
    let plan = interrupted
        .chain(tables.map(Planned::Table))
        .chain(removed.iter().map(|entry| Planned::Realization(entry)))
        .collect::<Vec<_>>();

    let human = Human {
        directory: &scope.directory,
        execute: args.execute,
    };
    let mut publish = |event: PruneEvent| -> Result<(), CliError> {
        let envelope = PruneEnvelope::new(event);
        if output == OutputMode::Jsonl {
            write_jsonl(writer, &envelope).map_err(CliError::corpus)
        } else {
            human
                .write(writer, &envelope.event)
                .map_err(CliError::stdout)
        }
    };
    publish(PruneEvent::Run(PruneRun {
        run_id: crate::commands::query_support::random_id(),
        directory: display(&scope.directory),
        source: scope.source.as_ref().map(|(path, _)| display(path)),
        keep: kept.iter().map(|path| display(path)).collect(),
        include_all_superseded: args.include_all_superseded,
        execute: args.execute,
    }))?;
    for source in &scan.sources {
        publish(PruneEvent::Source(source.record()))?;
    }
    for (entry, reason) in &kept_by_rule {
        publish(PruneEvent::Kept(PruneKept {
            path: display(&entry.path),
            reason: *reason,
            differences: entry.differences.clone(),
            bytes: entry.bytes,
            message: (*reason == PruneKeep::Damaged)
                .then(|| entry.message.clone())
                .flatten(),
        }))?;
    }

    let (mut expected, mut freed) = (0_u64, 0_u64);
    for planned in &plan {
        let (path, reason, measured, differences) = match planned {
            Planned::Realization(entry) => (
                entry.path.as_path(),
                if entry.status == RealizationStatus::OlderFormat {
                    RemovalReason::OlderFormat
                } else {
                    RemovalReason::Superseded
                },
                entry.bytes,
                entry.differences.clone(),
            ),
            Planned::Table(table) => (
                table.path.as_path(),
                RemovalReason::TableOfRemoved,
                table.bytes,
                Vec::new(),
            ),
            Planned::Leftover(path, bytes) => {
                (*path, RemovalReason::InterruptedPrune, *bytes, Vec::new())
            }
        };
        let measured = measured.unwrap_or_default();
        expected += measured;
        let bytes = if args.execute {
            let bytes = delete(planned).map_err(|error| {
                CliError::Corpus(format!("cannot delete {}: {error}", path.display()))
            })?;
            freed += bytes;
            bytes
        } else {
            measured
        };
        publish(PruneEvent::Removal(PruneRemoval {
            path: display(path),
            reason,
            differences,
            bytes,
            deleted: args.execute,
        }))?;
    }
    publish(PruneEvent::Complete(PruneComplete {
        executed: args.execute,
        removals: plan.len() as u64,
        expected_freed_bytes: expected,
        freed_bytes: freed,
    }))?;
    Ok(CommandOutcome::Success)
}

/// Resolves each `--keep` to the readable realization in the scan it names;
/// a damaged one is not readable.
fn keep(
    requested: &[PathBuf],
    realizations: &[RealizationEntry],
    directory: &Path,
) -> Result<BTreeSet<PathBuf>, CliError> {
    let mut kept = BTreeSet::new();
    for path in requested {
        let resolved = fs::canonicalize(path).ok();
        let found = realizations.iter().find(|entry| {
            entry.recorded.is_some()
                && entry.status != RealizationStatus::Unreadable
                && resolved.is_some()
                && fs::canonicalize(&entry.path).ok() == resolved
        });
        let Some(found) = found else {
            return Err(CliError::InvalidArgs(format!(
                "--keep {}: not a readable prepared artifact in {}",
                path.display(),
                directory.display()
            )));
        };
        kept.insert(found.path.clone());
    }
    Ok(kept)
}

fn delete(planned: &Planned<'_>) -> Result<u64, String> {
    match planned {
        Planned::Realization(entry) => {
            let source = entry.source.as_deref().unwrap_or_default();
            match (&entry.recorded, entry.status) {
                (Some(recorded), RealizationStatus::Superseded) => {
                    remove_realization(&entry.path, source, &recorded.analysis_identity)
                        .map_err(|error| error.to_string())
                }
                (_, RealizationStatus::OlderFormat) => {
                    remove_older_format(&entry.path, source).map_err(|error| error.to_string())
                }
                _ => Err("not a removable realization".into()),
            }
        }
        Planned::Leftover(path, _) => {
            finish_prune_leftover(path).map_err(|error| error.to_string())
        }
        Planned::Table(table) => CountTable::remove(
            &table.path,
            table.analysis_identity.as_deref().unwrap_or_default(),
        )
        .map_err(|error| error.to_string()),
    }
}

struct Human<'a> {
    directory: &'a Path,
    execute: bool,
}

impl Human<'_> {
    fn write<W: Write>(&self, writer: &mut W, event: &PruneEvent) -> io::Result<()> {
        match event {
            PruneEvent::Run(run) => {
                let verb = if run.execute {
                    "pruning"
                } else {
                    "would prune"
                };
                match &run.source {
                    Some(source) => writeln!(writer, "{verb} {source} in {}", run.directory),
                    None => writeln!(writer, "{verb} {}", run.directory),
                }
            }
            PruneEvent::Source(source) => match &source.current {
                crate::protocol::disk::SourceCurrent::Present { path } => writeln!(
                    writer,
                    "  source {}: keeping current {}",
                    source.name,
                    self.short(path)
                ),
                _ => Ok(()),
            },
            PruneEvent::Kept(kept) => writeln!(
                writer,
                "  keeping {:<21} {:>10}  {}{}",
                disk::wire(&kept.reason).replace('_', "-"),
                disk::size(kept.bytes),
                self.short(&kept.path),
                match (&kept.message, kept.reason) {
                    (Some(message), _) => format!(": {message}"),
                    (None, reason) if reason.deleted_with_include_all_superseded() => {
                        " (--include-all-superseded deletes it)".to_owned()
                    }
                    (None, _) => String::new(),
                }
            ),
            PruneEvent::Removal(removal) => writeln!(
                writer,
                "  {} {:<16} {:>10}  {}",
                if removal.deleted {
                    "deleted"
                } else {
                    "would delete"
                },
                disk::wire(&removal.reason).replace('_', "-"),
                disk::human_bytes(removal.bytes),
                self.short(&removal.path)
            ),
            PruneEvent::Complete(complete) if self.execute => writeln!(
                writer,
                "complete: {} item(s) deleted, {} freed",
                complete.removals,
                disk::human_bytes(complete.freed_bytes)
            ),
            PruneEvent::Complete(complete) => writeln!(
                writer,
                "complete: {} item(s), {} would be freed; nothing was deleted (pass --execute to delete)",
                complete.removals,
                disk::human_bytes(complete.expected_freed_bytes)
            ),
        }
    }

    fn short(&self, path: &str) -> String {
        Path::new(path).strip_prefix(self.directory).map_or_else(
            |_| path.to_owned(),
            |relative| relative.display().to_string(),
        )
    }
}
