//! `ling table`: per-pattern counts under `--where`, read from a count table.

use std::io::{self, Write};
use std::sync::Arc;
use std::time::Instant;

use lingrep_core::execution::SentenceAdmission;
use lingrep_core::spill::{SpillPolicy, TempBudget};
use lingrep_corpus::{
    Admission, CorpusArtifact, CountTable, CountTableError, CountTableKey, FieldValue, Group,
    TopRow,
};

use super::CommandOutcome;
use super::query_scope::{parse_scope, resolve_prepared_scope};
use super::query_support::{execution_output_error, execution_output_io};
use crate::catalog_input::load_and_compile;
use crate::cli::TableArgs;
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::output::sanitize_terminal;
use crate::presentation::{OutputMode, resolve_summary_top};
use crate::protocol::query::{ProtocolKeyField, SummaryGroupData};
use crate::protocol::table::{
    TableComplete, TableEnvelope, TableEvent, TablePattern, TableRow, TableRun,
};
use crate::protocol::write_jsonl;

/// Rows held in memory per sorted run when every group is read.
const RESIDENT_ROWS: usize = 1 << 20;
const MERGE_FAN_IN: usize = 16;

fn table_error(error: CountTableError) -> CliError {
    match error {
        CountTableError::Io(error) => CliError::Aggregation(error),
        error => CliError::Corpus(error.to_string()),
    }
}

pub fn run(args: &TableArgs, output: OutputMode) -> Result<CommandOutcome, CliError> {
    let started = Instant::now();
    let catalog = load_and_compile(&args.catalog.catalog_input()?)?;
    let clause = parse_scope(args.clause.as_deref())?;
    if clause
        .as_ref()
        .is_some_and(|clause| clause.names_sections() || clause.requires_sentence_values())
    {
        return Err(CliError::InvalidArgs(
            "table --where accepts only @document conditions: a count table holds counts per \
             document"
                .to_owned(),
        ));
    }
    let top = resolve_summary_top(output, args.top);
    let artifact = CorpusArtifact::open(&args.corpus)?;
    let key = CountTableKey {
        analysis_identity: encode_digest(artifact.analysis_identity().as_bytes()),
        catalog_fingerprint: catalog.fingerprint.clone(),
        matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
    };
    let table =
        CountTable::open(&args.table, &key).map_err(|error| CliError::Corpus(error.to_string()))?;
    let mask = match &clause {
        None => table.mask(|_| true),
        Some(clause) => {
            let reader = artifact.provenance_reader()?;
            let (resolution, _) = resolve_prepared_scope(&reader, clause)?;
            let SentenceAdmission::Documents(admitted) = resolution.admission else {
                return Err(CliError::InvalidArgs(
                    "table --where accepts only @document conditions".to_owned(),
                ));
            };
            table.mask(|id| admitted.contains(id))
        }
    };
    let queried = Instant::now();
    let totals = table.pattern_totals(&mask).map_err(table_error)?;
    // A limit returns each pattern's rows at once; without one, rows stream
    // through sorted runs on disk.
    let (admission, mut rows): (Admission, Box<dyn Iterator<Item = _>>) = if let Some(top) = top {
        let result = table
            .top(&mask, usize::try_from(top).unwrap_or(usize::MAX))
            .map_err(table_error)?;
        let admission = Admission {
            documents_admitted: result.documents_admitted,
            sentences_admitted: result.sentences_admitted,
            tokens_admitted: result.tokens_admitted,
        };
        let rows = result.patterns.into_iter().flat_map(|pattern| {
            pattern
                .rows
                .into_iter()
                .map(move |row| Ok::<(usize, TopRow), CountTableError>((pattern.pattern, row)))
        });
        (admission, Box::new(rows))
    } else {
        let rows = table
            .rows(
                &mask,
                SpillPolicy {
                    max_pending_bytes: usize::MAX,
                    max_pending_keys: RESIDENT_ROWS,
                    merge_fan_in: MERGE_FAN_IN,
                },
                Arc::new(TempBudget::new(args.max_temp_bytes)),
            )
            .map_err(table_error)?;
        (rows.admission(), Box::new(rows))
    };

    let names = table.pattern_names().collect::<Vec<_>>();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut emit = |event: TableEvent| {
        write_jsonl(&mut writer, &TableEnvelope::new(event))
            .map_err(|error| CliError::Execution(execution_output_error(error)))
    };
    let jsonl = output == OutputMode::Jsonl;
    if jsonl {
        emit(TableEvent::Run(TableRun {
            table: args.table.display().to_string(),
            analysis_identity: key.analysis_identity.clone(),
            catalog_fingerprint: key.catalog_fingerprint.clone(),
            matcher_revision: key.matcher_revision.clone(),
            clause: args.clause.clone(),
            requested_top: top,
        }))?;
    }
    let mut human = Vec::new();
    for total in &totals {
        let name = names
            .get(total.pattern)
            .copied()
            .ok_or_else(|| CliError::Corpus("table pattern out of range".to_owned()))?;
        if jsonl {
            emit(TableEvent::Pattern(TablePattern {
                pattern: name.to_owned(),
                occurrences: total.occurrences,
                works: total.works,
            }))?;
        } else {
            human.push(format!(
                "pattern {}: {} occurrences in {} documents",
                sanitize_terminal(name),
                total.occurrences,
                total.works
            ));
        }
    }
    let mut emitted = 0u64;
    let mut previous_pattern = None;
    let mut rank = 0u64;
    for row in &mut rows {
        let (pattern, row) = row.map_err(table_error)?;
        let name = names
            .get(pattern)
            .copied()
            .ok_or_else(|| CliError::Corpus("table pattern out of range".to_owned()))?;
        if previous_pattern != Some(pattern) {
            previous_pattern = Some(pattern);
            rank = 0;
        }
        rank += 1;
        let group = table.group(row.entry).map_err(table_error)?;
        emitted += 1;
        if jsonl {
            emit(TableEvent::Row(TableRow {
                pattern: name.to_owned(),
                rank,
                group: group_data(&group),
                count: row.count,
                works: row.works,
            }))?;
        } else {
            human.push(format!(
                "{:>8} {:>6}  {}  {}",
                row.count,
                row.works,
                sanitize_terminal(name),
                sanitize_terminal(&group_display(&group)?)
            ));
        }
    }
    let query_millis = queried.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    let complete = TableComplete {
        documents_admitted: admission.documents_admitted,
        sentences_admitted: admission.sentences_admitted,
        tokens_admitted: admission.tokens_admitted,
        rows: emitted,
        query_millis,
        elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
    };
    if jsonl {
        emit(TableEvent::Complete(complete))?;
    } else {
        let mut writer = io::stdout().lock();
        for line in human {
            writeln!(writer, "{line}")
                .map_err(|error| CliError::Execution(execution_output_io(error)))?;
        }
        writeln!(
            writer,
            "{} documents, {} tokens admitted; {} rows in {} ms",
            complete.documents_admitted, complete.tokens_admitted, complete.rows, query_millis
        )
        .map_err(|error| CliError::Execution(execution_output_io(error)))?;
    }
    Ok(CommandOutcome::Success)
}

fn group_data(group: &Group<'_>) -> SummaryGroupData {
    match group {
        Group::Surface(value) => SummaryGroupData::Surface {
            value: (*value).to_owned(),
        },
        Group::Unit(value) => SummaryGroupData::Unit {
            value: (*value).to_owned(),
        },
        Group::Term(value) => SummaryGroupData::Term {
            value: (*value).to_owned(),
        },
        Group::Key(fields) => SummaryGroupData::Key {
            fields: fields
                .iter()
                .map(|(name, value)| match value {
                    FieldValue::Scalar(value) => ProtocolKeyField::Scalar {
                        name: (*name).to_owned(),
                        value: (*value).to_owned(),
                    },
                    FieldValue::List(values) => ProtocolKeyField::List {
                        name: (*name).to_owned(),
                        values: values.iter().map(|value| (*value).to_owned()).collect(),
                    },
                })
                .collect(),
        },
    }
}

/// The text of a group as `ling summarize` shows it: a bare value, or a
/// compact JSON object in field order.
fn group_display(group: &Group<'_>) -> Result<String, CliError> {
    Ok(match group {
        Group::Surface(value) | Group::Unit(value) | Group::Term(value) => (*value).to_owned(),
        Group::Key(fields) => {
            let mut text = String::from("{");
            for (index, (name, value)) in fields.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                text.push_str(&serde_json::to_string(name)?);
                text.push(':');
                text.push_str(&match value {
                    FieldValue::Scalar(value) => serde_json::to_string(value)?,
                    FieldValue::List(values) => serde_json::to_string(values)?,
                });
            }
            text.push('}');
            text
        }
    })
}
