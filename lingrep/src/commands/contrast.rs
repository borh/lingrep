//! `ling contrast`: each pattern's entries ranked by G² between two
//! `--where` scopes, read from a count table.

use std::io::{self, Write};
use std::sync::Arc;
use std::time::Instant;

use lingrep_core::spill::{SpillPolicy, TempBudget};
use lingrep_corpus::{
    Admission, BlockingProvenance, ContrastRow, CorpusArtifact, CountTable, CountTableError,
    DocumentMask, ScopeRelation,
};

use super::CommandOutcome;
use super::query_support::{execution_output_error, execution_output_io};
use super::table::{
    document_admission, document_clause, group_data, group_display, open_table, table_error,
};
use crate::catalog_input::load_and_compile;
use crate::cli::ContrastArgs;
use crate::error::CliError;
use crate::output::sanitize_terminal;
use crate::presentation::{OutputMode, resolve_summary_top};
use crate::protocol::contrast::{
    self as protocol, ContrastComplete, ContrastEnvelope, ContrastEvent, ContrastPattern,
    ContrastRun, ContrastScopes, ContrastSide, ScopeAdmission,
};
use crate::protocol::write_jsonl;
use crate::terminal::TerminalArbiter;

const RESIDENT_ROWS: usize = 1 << 20;
const MERGE_FAN_IN: usize = 16;

fn scope_mask(
    table: &CountTable,
    reader: Option<&BlockingProvenance>,
    flag: &str,
    clause: Option<&str>,
) -> Result<DocumentMask, CliError> {
    let option = format!("contrast {flag}");
    let (Some(clause), Some(reader)) = (document_clause(&option, clause)?, reader) else {
        return Ok(table.mask(|_| true));
    };
    let admitted = document_admission(reader, &clause, &option)?;
    Ok(table.mask(|id| admitted.contains(id)))
}

fn admission(admission: Admission) -> ScopeAdmission {
    ScopeAdmission {
        documents: admission.documents_admitted,
        sentences: admission.sentences_admitted,
        tokens: admission.tokens_admitted,
    }
}

fn side(row: &ContrastRow, scope: usize) -> ContrastSide {
    ContrastSide {
        count: row.scopes[scope].count,
        works: row.scopes[scope].works,
        per_million: row.contrast.rates_per_million[scope],
    }
}

fn count(count: lingrep_corpus::ScopeCount) -> protocol::ScopeCount {
    protocol::ScopeCount {
        count: count.count,
        works: count.works,
    }
}

enum Ranking<'a> {
    Top(std::vec::IntoIter<ContrastRow>),
    Spilled(Box<lingrep_corpus::ContrastRows<'a>>, u64),
}

impl Iterator for Ranking<'_> {
    type Item = Result<ContrastRow, CountTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Top(rows) => rows.next().map(Ok),
            Self::Spilled(rows, remaining) => {
                *remaining = remaining.checked_sub(1)?;
                rows.next()
            }
        }
    }
}

pub fn run(
    args: &ContrastArgs,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let started = Instant::now();
    let catalog = load_and_compile(&args.catalog.catalog_input()?)?;
    let top = resolve_summary_top(output, args.top);
    let artifact = CorpusArtifact::open(&args.corpus)?;
    let (table, key) = open_table(&args.table, &artifact, &catalog.fingerprint)?;
    let reader = (args.clause.is_some() || args.versus.is_some())
        .then(|| artifact.provenance_reader())
        .transpose()?;
    let masks = [
        scope_mask(&table, reader.as_deref(), "--where", args.clause.as_deref())?,
        scope_mask(
            &table,
            reader.as_deref(),
            "--versus",
            args.versus.as_deref(),
        )?,
    ];

    let queried = Instant::now();
    let scopes = table.scopes([&masks[0], &masks[1]]).map_err(table_error)?;
    let totals = [
        table.pattern_totals(&masks[0]).map_err(table_error)?,
        table.pattern_totals(&masks[1]).map_err(table_error)?,
    ];
    let budget = Arc::new(TempBudget::new(args.max_temp_bytes));
    let cancellation = terminal.cancellation();
    let resident = top
        .and_then(|top| usize::try_from(top).ok())
        .filter(|top| *top <= RESIDENT_ROWS);
    let rankings = (0..table.pattern_names().len())
        .map(|pattern| {
            Ok(if let Some(top) = resident {
                let ranked = table.contrast_top(pattern, &scopes, top, cancellation)?;
                (ranked.entries, Ranking::Top(ranked.rows.into_iter()))
            } else {
                let rows = table.ranked_contrast_rows(
                    pattern,
                    &scopes,
                    SpillPolicy {
                        max_pending_bytes: usize::MAX,
                        max_pending_keys: RESIDENT_ROWS,
                        merge_fan_in: MERGE_FAN_IN,
                    },
                    Arc::clone(&budget),
                    cancellation,
                )?;
                (
                    rows.entries,
                    Ranking::Spilled(Box::new(rows), top.unwrap_or(u64::MAX)),
                )
            })
        })
        .collect::<Result<Vec<_>, CountTableError>>();
    if cancellation.is_cancelled() {
        return Ok(CommandOutcome::Cancelled);
    }
    let rankings = rankings.map_err(table_error)?;

    let names = table.pattern_names().collect::<Vec<_>>();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let jsonl = output == OutputMode::Jsonl;
    let emit = |writer: &mut io::StdoutLock<'_>, event: ContrastEvent| {
        write_jsonl(writer, &ContrastEnvelope::new(event))
            .map_err(|error| CliError::Execution(execution_output_error(error)))
    };
    let line = |writer: &mut io::StdoutLock<'_>, text: String| {
        writeln!(writer, "{text}").map_err(|error| CliError::Execution(execution_output_io(error)))
    };
    let scope_record = ContrastScopes {
        where_scope: admission(scopes.admission()[0]),
        versus: admission(scopes.admission()[1]),
        shared: admission(scopes.shared()),
        relation: match scopes.relation() {
            ScopeRelation::Disjoint => protocol::ScopeRelation::Disjoint,
            ScopeRelation::Overlapping => protocol::ScopeRelation::Overlapping,
            ScopeRelation::Identical => protocol::ScopeRelation::Identical,
        },
    };
    if jsonl {
        emit(
            &mut writer,
            ContrastEvent::Run(ContrastRun {
                table: args.table.display().to_string(),
                analysis_identity: key.analysis_identity.clone(),
                catalog_fingerprint: key.catalog_fingerprint.clone(),
                matcher_revision: key.matcher_revision.clone(),
                where_clause: args.clause.clone(),
                versus: args.versus.clone(),
                requested_top: top,
            }),
        )?;
        emit(&mut writer, ContrastEvent::Scopes(scope_record))?;
    } else {
        line(
            &mut writer,
            format!(
                "--where: {} documents, {} tokens; --versus: {} documents, {} tokens; {}",
                scope_record.where_scope.documents,
                scope_record.where_scope.tokens,
                scope_record.versus.documents,
                scope_record.versus.tokens,
                match scope_record.relation {
                    protocol::ScopeRelation::Disjoint => "no document in both".to_owned(),
                    protocol::ScopeRelation::Overlapping => format!(
                        "overlapping: {} documents, {} tokens in both",
                        scope_record.shared.documents, scope_record.shared.tokens
                    ),
                    protocol::ScopeRelation::Identical => "identical scopes".to_owned(),
                }
            ),
        )?;
    }
    for (pattern, (entries, _)) in rankings.iter().enumerate() {
        let name = names[pattern];
        let [first, second] = [&totals[0][pattern], &totals[1][pattern]];
        if jsonl {
            emit(
                &mut writer,
                ContrastEvent::Pattern(ContrastPattern {
                    pattern: name.to_owned(),
                    where_scope: protocol::ScopeCount {
                        count: first.occurrences,
                        works: first.works,
                    },
                    versus: protocol::ScopeCount {
                        count: second.occurrences,
                        works: second.works,
                    },
                    entries: *entries,
                }),
            )?;
        } else {
            line(
                &mut writer,
                format!(
                    "pattern {}: {} entries; {} occurrences --where, {} --versus",
                    sanitize_terminal(name),
                    entries,
                    first.occurrences,
                    second.occurrences
                ),
            )?;
        }
    }
    if !jsonl {
        line(
            &mut writer,
            format!(
                "{:>10} {:>8} {:>10} {:>8} {:>10} {:>6}  pattern  entry",
                "G²", "--where", "per M", "--versus", "per M", "log2"
            ),
        )?;
    }
    let mut emitted = 0u64;
    for (pattern, (_, ranking)) in rankings.into_iter().enumerate() {
        let name = names[pattern];
        for (rank, row) in (1u64..).zip(ranking) {
            if cancellation.is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            let row = row.map_err(table_error)?;
            let group = table.group(row.entry).map_err(table_error)?;
            emitted += 1;
            if jsonl {
                emit(
                    &mut writer,
                    ContrastEvent::Row(protocol::ContrastRow {
                        pattern: name.to_owned(),
                        rank,
                        group: group_data(&group),
                        where_scope: side(&row, 0),
                        versus: side(&row, 1),
                        shared: count(row.shared),
                        g2: row.contrast.log_likelihood,
                        min_expected: row.contrast.min_expected,
                        log2_ratio: row.contrast.log2_ratio,
                        direction: row.contrast.direction.map(|direction| match direction {
                            lingrep_corpus::Direction::First => protocol::Direction::Where,
                            lingrep_corpus::Direction::Second => protocol::Direction::Versus,
                            lingrep_corpus::Direction::Equal => protocol::Direction::Equal,
                        }),
                    }),
                )?;
            } else {
                let figure = |value: Option<f64>, places: usize| {
                    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.places$}"))
                };
                line(
                    &mut writer,
                    format!(
                        "{:>10} {:>8} {:>10} {:>8} {:>10} {:>6}  {}  {}",
                        figure(row.contrast.log_likelihood, 2),
                        row.scopes[0].count,
                        figure(row.contrast.rates_per_million[0], 2),
                        row.scopes[1].count,
                        figure(row.contrast.rates_per_million[1], 2),
                        figure(row.contrast.log2_ratio, 2),
                        sanitize_terminal(name),
                        sanitize_terminal(&group_display(&group)?)
                    ),
                )?;
            }
        }
    }
    let complete = ContrastComplete {
        rows: emitted,
        query_millis: queried.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
    };
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    if jsonl {
        emit(&mut writer, ContrastEvent::Complete(complete))?;
    } else {
        writeln!(
            writer,
            "{} rows in {} ms",
            complete.rows, complete.query_millis
        )
        .map_err(|error| CliError::Execution(execution_output_io(error)))?;
    }
    Ok(CommandOutcome::Success)
}
