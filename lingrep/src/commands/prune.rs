//! Plans corpus and count-table removals and executes them only with `--execute`.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use lingrep_cli_common::SafeText;
use lingrep_corpus::{CountTable, finish_prune_leftover, remove_older_format, remove_realization};

use super::CommandOutcome;
use super::disk::{self, CurrentSelection, RealizationEntry, Scope, TableEntry, display};
use crate::cli::{CorpusPruneArgs, ExecutionCliConfig};
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::disk::{LeftoverStatus, PruneKeep, RealizationStatus};
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
    let mut kept_by_rule = scan
        .realizations
        .iter()
        .filter(|entry| !kept.contains(&entry.path))
        .filter_map(|entry| {
            let reason = match entry.status {
                RealizationStatus::Superseded
                | RealizationStatus::OlderFormat
                | RealizationStatus::Damaged => keep_reason(entry)?,
                _ => return None,
            };
            Some(PruneKept {
                path: display(&entry.path),
                reason,
                differences: entry.differences.clone(),
                apparent_bytes: entry.bytes,
                message: kept_message(reason, entry.message.as_deref(), &entry.foreign),
            })
        })
        .chain(scan.leftovers.iter().filter_map(|leftover| {
            let reason = leftover.kept_by_prune?;
            Some(PruneKept {
                path: display(leftover.pruning.as_deref()?),
                reason,
                differences: Vec::new(),
                apparent_bytes: leftover.bytes,
                message: kept_message(reason, None, &leftover.foreign),
            })
        }))
        .collect::<Vec<_>>();
    let removable = scan
        .realizations
        .iter()
        .filter(|entry| match entry.status {
            RealizationStatus::Superseded | RealizationStatus::OlderFormat => {
                !kept.contains(&entry.path) && keep_reason(entry).is_none()
            }
            _ => false,
        })
        .collect::<Vec<_>>();
    let (tables, protected) = disk::plan_tables(
        &scan.tables,
        &removable.iter().map(|entry| entry.path.as_path()).collect(),
    );
    kept_by_rule.extend(
        scan.tables
            .iter()
            .filter(|table| {
                !table.artifacts.is_empty()
                    && table
                        .artifacts
                        .iter()
                        .all(|artifact| protected.contains_key(artifact.as_path()))
            })
            .filter_map(|table| {
                let reason = table.kept_by_prune?;
                Some(PruneKept {
                    path: display(&table.path),
                    reason,
                    differences: Vec::new(),
                    apparent_bytes: table.bytes,
                    message: kept_message(reason, None, &table.foreign),
                })
            }),
    );
    let (removed, held): (Vec<_>, Vec<_>) = removable
        .into_iter()
        .partition(|entry| !protected.contains_key(entry.path.as_path()));
    kept_by_rule.extend(held.into_iter().map(|entry| {
        PruneKept {
            path: display(&entry.path),
            reason: PruneKeep::KeptTable,
            differences: entry.differences.clone(),
            apparent_bytes: entry.bytes,
            message: protected
                .get(entry.path.as_path())
                .map(|table| format!("binds the kept count table {}", table.path.display())),
        }
    }));
    kept_by_rule.sort_by(|left, right| left.path.cmp(&right.path));

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
        .map(|source| {
            let keepable = scan.realizations.iter().any(|entry| {
                entry.source.as_deref() == Some(source.name.as_str()) && keepable(entry)
            });
            let keep = if keepable {
                ", or name one to keep with --keep ARTIFACT"
            } else {
                ""
            };
            match &source.current {
                CurrentSelection::Absent(path) => format!(
                    "{} has no current artifact; build it with these inputs (corpus build would write {}){keep}",
                    source.name,
                    path.display()
                ),
                _ if keepable => format!(
                    "{} is missing, so no artifact of it is current; name one to keep with --keep ARTIFACT",
                    source.name
                ),
                _ => format!(
                    "{} is missing and has no readable artifact to keep with --keep; restore it and build it, or delete its artifacts yourself",
                    source.name
                ),
            }
        })
        .collect::<Vec<_>>();
    if !unanchored.is_empty() {
        return Err(CliError::InvalidArgs(format!(
            "nothing was deleted: {}",
            unanchored.join("; ")
        )));
    }

    let interrupted = scan.leftovers.iter().filter_map(|leftover| {
        (leftover.status == LeftoverStatus::InterruptedPrune && leftover.kept_by_prune.is_none())
            .then_some(leftover.pruning.as_deref())
            .flatten()
            .map(|path| Planned::Leftover(path, leftover.bytes))
    });
    let plan = interrupted
        .chain(tables.into_iter().map(Planned::Table))
        .chain(removed.iter().map(|entry| Planned::Realization(entry)))
        .collect::<Vec<_>>();

    let human = Human {
        directory: &scope.directory,
        execute: args.execute,
    };
    let mut publish = |event: PruneEvent| -> Result<(), CliError> {
        let envelope = PruneEnvelope::new(event);
        if output == OutputMode::Jsonl {
            write_jsonl(writer, &envelope).map_err(CliError::from)
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
    for kept in kept_by_rule {
        publish(PruneEvent::Kept(kept))?;
    }

    let planned_apparent_bytes = plan
        .iter()
        .map(|planned| planned.measured().unwrap_or_default())
        .sum();
    let (mut deleted, mut deleted_apparent_bytes, mut failed) = (0_u64, 0_u64, 0_u64);
    for planned in &plan {
        let (path, reason, differences) = match planned {
            Planned::Realization(entry) => (
                entry.path.as_path(),
                if entry.status == RealizationStatus::OlderFormat {
                    RemovalReason::OlderFormat
                } else {
                    RemovalReason::Superseded
                },
                entry.differences.clone(),
            ),
            Planned::Table(table) => (
                table.path.as_path(),
                RemovalReason::TableOfRemoved,
                Vec::new(),
            ),
            Planned::Leftover(path, _) => (*path, RemovalReason::InterruptedPrune, Vec::new()),
        };
        let measured = planned.measured().unwrap_or_default();
        let (apparent_bytes, failure) = if args.execute {
            match delete(planned) {
                Ok(bytes) => {
                    deleted += 1;
                    deleted_apparent_bytes += bytes;
                    (bytes, None)
                }
                Err(error) => {
                    failed += 1;
                    (measured, Some(error))
                }
            }
        } else {
            (measured, None)
        };
        let stop = failure.is_some();
        publish(PruneEvent::Removal(PruneRemoval {
            path: display(path),
            reason,
            differences,
            apparent_bytes,
            failure,
        }))?;
        if stop {
            break;
        }
    }
    publish(PruneEvent::Complete(PruneComplete {
        executed: args.execute,
        removals: plan.len() as u64,
        deleted,
        failed,
        planned_apparent_bytes,
        deleted_apparent_bytes,
    }))?;
    Ok(if failed == 0 {
        CommandOutcome::Success
    } else {
        CommandOutcome::Failure
    })
}

impl Planned<'_> {
    fn measured(&self) -> Option<u64> {
        match self {
            Planned::Leftover(_, bytes) => *bytes,
            Planned::Realization(entry) => entry.bytes,
            Planned::Table(table) => table.bytes,
        }
    }
}

/// Whether `--keep` can name `entry`: a readable realization that is not
/// damaged.
fn keepable(entry: &RealizationEntry) -> bool {
    entry.recorded.is_some() && entry.status != RealizationStatus::Damaged
}

/// The `message` of a kept record: the validation failure of a damaged
/// realization, or the entries that are not artifact components.
fn kept_message(reason: PruneKeep, failure: Option<&str>, foreign: &[String]) -> Option<String> {
    match reason {
        PruneKeep::Damaged => failure.map(str::to_owned),
        PruneKeep::ForeignEntries => Some(format!(
            "holds entries that are not components: {}",
            foreign.join(", ")
        )),
        _ => None,
    }
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
            keepable(entry) && resolved.is_some() && fs::canonicalize(&entry.path).ok() == resolved
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
                    Some(source) => writeln!(
                        writer,
                        "{verb} {} in {}",
                        SafeText::from_untrusted(source),
                        SafeText::from_untrusted(&run.directory)
                    ),
                    None => writeln!(
                        writer,
                        "{verb} {}",
                        SafeText::from_untrusted(&run.directory)
                    ),
                }
            }
            PruneEvent::Source(source) => match &source.current {
                crate::protocol::disk::SourceCurrent::Present { path } => {
                    let short = self.short(path);
                    writeln!(
                        writer,
                        "  source {}: keeping current {}",
                        SafeText::from_untrusted(&source.name),
                        SafeText::from_untrusted(&short)
                    )
                }
                _ => Ok(()),
            },
            PruneEvent::Kept(kept) => {
                let short = self.short(&kept.path);
                writeln!(
                    writer,
                    "  keeping {:<21} {:>10}  {}{}",
                    disk::wire(&kept.reason).replace('_', "-"),
                    disk::size(kept.apparent_bytes),
                    SafeText::from_untrusted(&short),
                    match (&kept.message, kept.reason) {
                        (Some(message), _) => format!(": {}", SafeText::from_untrusted(message)),
                        (None, reason) if reason.deleted_with_include_all_superseded() => {
                            " (--include-all-superseded deletes it)".to_owned()
                        }
                        (None, _) => String::new(),
                    }
                )
            }
            PruneEvent::Removal(removal) => {
                let verb = match (&removal.failure, self.execute) {
                    (Some(_), _) => "failed to delete",
                    (None, true) => "deleted",
                    (None, false) => "would delete",
                };
                let short = self.short(&removal.path);
                writeln!(
                    writer,
                    "  {verb} {:<16} {:>10}  {}{}",
                    disk::wire(&removal.reason).replace('_', "-"),
                    disk::human_bytes(removal.apparent_bytes),
                    SafeText::from_untrusted(&short),
                    removal
                        .failure
                        .as_ref()
                        .map_or_else(String::new, |failure| {
                            format!(": {}", SafeText::from_untrusted(failure))
                        })
                )
            }
            PruneEvent::Complete(complete) if complete.failed > 0 => writeln!(
                writer,
                "incomplete: {} of {} item(s) deleted ({} apparent size) before a deletion failed; \
                 run it again once the failure is fixed",
                complete.deleted,
                complete.removals,
                disk::human_bytes(complete.deleted_apparent_bytes)
            ),
            PruneEvent::Complete(complete) if self.execute => writeln!(
                writer,
                "complete: {} item(s) deleted, {} apparent size",
                complete.deleted,
                disk::human_bytes(complete.deleted_apparent_bytes)
            ),
            PruneEvent::Complete(complete) => writeln!(
                writer,
                "complete: {} item(s), {} apparent size; nothing was deleted (pass --execute to delete)",
                complete.removals,
                disk::human_bytes(complete.planned_apparent_bytes)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::disk::{DiskSource, SourceCurrent};

    #[test]
    fn human_output_sanitizes_untrusted_paths_and_messages() {
        let hostile = "/tmp/evil\u{1b}[2J\u{202e}dir";
        let human = Human {
            directory: Path::new("/tmp"),
            execute: false,
        };
        let events = [
            PruneEvent::Run(PruneRun {
                run_id: "r1".into(),
                directory: hostile.into(),
                source: Some("src\u{1b}[31m.txt".into()),
                keep: Vec::new(),
                include_all_superseded: false,
                execute: false,
            }),
            PruneEvent::Source(DiskSource {
                name: "src\u{202e}.txt".into(),
                path: hostile.into(),
                current: SourceCurrent::Present {
                    path: format!("{hostile}/cur.lingcorpus"),
                },
            }),
            PruneEvent::Kept(PruneKept {
                path: format!("{hostile}/kept.lingcorpus"),
                reason: PruneKeep::ForeignEntries,
                differences: Vec::new(),
                apparent_bytes: Some(10),
                message: Some("holds entries: \u{0085}extra\u{1b}[2J".into()),
            }),
            PruneEvent::Removal(PruneRemoval {
                path: format!("{hostile}/old.lingcorpus"),
                reason: RemovalReason::Superseded,
                differences: Vec::new(),
                apparent_bytes: 20,
                failure: Some("denied\u{202e}msg".into()),
            }),
        ];

        let mut buffer = Vec::new();
        for event in &events {
            human.write(&mut buffer, event).unwrap();
        }
        let rendered = String::from_utf8(buffer).unwrap();
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
        assert!(!rendered.contains('\u{202e}'), "{rendered:?}");
        assert!(!rendered.contains('\u{0085}'), "{rendered:?}");
        assert!(rendered.contains("\\u{001b}[2J\\u{202e}"), "{rendered:?}");
        assert!(
            rendered.contains("\\u{0085}extra\\u{001b}[2J"),
            "{rendered:?}"
        );
        assert!(rendered.contains("denied\\u{202e}msg"), "{rendered:?}");
    }
}
