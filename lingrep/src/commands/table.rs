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
use super::query_scope::{document_values, parse_scope, resolve_prepared_scope};
use super::query_support::{execution_output_error, execution_output_io};
use crate::catalog_input::load_and_compile;
use crate::cli::TableArgs;
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::output::sanitize_terminal;
use crate::presentation::{OutputMode, resolve_summary_top};
use crate::protocol::query::{ProtocolKeyField, SummaryGroupData};
use crate::protocol::table::{
    TableComplete, TableEntryCount, TableEnvelope, TableEvent, TableMetadataValue, TablePattern,
    TableRow, TableRun,
};
use crate::protocol::write_jsonl;

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
    if args.by_metadata.is_some() && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "table --by-metadata requires --json".to_owned(),
        ));
    }
    if let Some(key) = args
        .by_metadata
        .as_ref()
        .filter(|key| key.strip_prefix("@document.").is_none_or(str::is_empty))
    {
        return Err(CliError::InvalidArgs(format!(
            "table --by-metadata accepts only @document.NAME, not {key}"
        )));
    }
    let entry = args
        .entry
        .as_deref()
        .map(|entry| {
            entry.split_once('=').ok_or_else(|| {
                CliError::InvalidArgs(format!("table --entry takes PATTERN=KEY, not {entry}"))
            })
        })
        .transpose()?;
    let top = resolve_summary_top(output, args.top);
    let artifact = CorpusArtifact::open(&args.corpus)?;
    let key = CountTableKey {
        analysis_identity: encode_digest(artifact.analysis_identity().as_bytes()),
        catalog_fingerprint: catalog.fingerprint.clone(),
        matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
    };
    let table =
        CountTable::open(&args.table, &key).map_err(|error| CliError::Corpus(error.to_string()))?;
    let lookup = Instant::now();
    let entry = entry
        .map(|(pattern, entry_key)| find_entry(&table, pattern, entry_key))
        .transpose()?;
    let lookup = lookup.elapsed();
    let provenance = (clause.is_some() || entry.is_some())
        .then(|| artifact.provenance_reader())
        .transpose()?;
    let admitted = match (&clause, &provenance) {
        (Some(clause), Some(reader)) => {
            let (resolution, _) = resolve_prepared_scope(reader, clause)?;
            let SentenceAdmission::Documents(admitted) = resolution.admission else {
                return Err(CliError::InvalidArgs(
                    "table --where accepts only @document conditions".to_owned(),
                ));
            };
            Some(admitted)
        }
        _ => None,
    };
    let admits = |id: u64| {
        admitted
            .as_ref()
            .is_none_or(|admitted| admitted.contains(id))
    };
    if let (Some((pattern, entry)), Some(by), Some(reader)) =
        (entry, args.by_metadata.as_deref(), &provenance)
    {
        let documents = document_values(reader, by)?;
        let queried = Instant::now();
        let records = entry_by_metadata(&table, pattern, entry, by, documents, admits)?;
        let admission = table.top(&table.mask(admits), 0).map_err(table_error)?;
        let query_millis = (lookup + queried.elapsed())
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        let mut emit = |event: TableEvent| {
            write_jsonl(&mut writer, &TableEnvelope::new(event))
                .map_err(|error| CliError::Execution(execution_output_error(error)))
        };
        emit(TableEvent::Run(TableRun {
            table: args.table.display().to_string(),
            analysis_identity: key.analysis_identity.clone(),
            catalog_fingerprint: key.catalog_fingerprint.clone(),
            matcher_revision: key.matcher_revision.clone(),
            clause: args.clause.clone(),
            requested_top: None,
            entry: args.entry.clone(),
            by_metadata: args.by_metadata.clone(),
        }))?;
        for record in records {
            emit(TableEvent::MetadataValue(record))?;
        }
        emit(TableEvent::Complete(TableComplete {
            documents_admitted: admission.documents_admitted,
            sentences_admitted: admission.sentences_admitted,
            tokens_admitted: admission.tokens_admitted,
            rows: 0,
            query_millis,
            elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        }))?;
        return Ok(CommandOutcome::Success);
    }
    let mask = table.mask(admits);
    let queried = Instant::now();
    let totals = table.pattern_totals(&mask).map_err(table_error)?;
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
    let emit = |writer: &mut io::StdoutLock<'_>, event: TableEvent| {
        write_jsonl(writer, &TableEnvelope::new(event))
            .map_err(|error| CliError::Execution(execution_output_error(error)))
    };
    let jsonl = output == OutputMode::Jsonl;
    if jsonl {
        emit(
            &mut writer,
            TableEvent::Run(TableRun {
                table: args.table.display().to_string(),
                analysis_identity: key.analysis_identity.clone(),
                catalog_fingerprint: key.catalog_fingerprint.clone(),
                matcher_revision: key.matcher_revision.clone(),
                clause: args.clause.clone(),
                requested_top: top,
                entry: None,
                by_metadata: None,
            }),
        )?;
    }
    for total in &totals {
        let name = names
            .get(total.pattern)
            .copied()
            .ok_or_else(|| CliError::Corpus("table pattern out of range".to_owned()))?;
        if jsonl {
            emit(
                &mut writer,
                TableEvent::Pattern(TablePattern {
                    pattern: name.to_owned(),
                    occurrences: total.occurrences,
                    works: total.works,
                }),
            )?;
        } else {
            writeln!(
                writer,
                "pattern {}: {} occurrences in {} documents",
                sanitize_terminal(name),
                total.occurrences,
                total.works
            )
            .map_err(|error| CliError::Execution(execution_output_io(error)))?;
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
            emit(
                &mut writer,
                TableEvent::Row(TableRow {
                    pattern: name.to_owned(),
                    rank,
                    group: group_data(&group),
                    count: row.count,
                    works: row.works,
                }),
            )?;
        } else {
            writeln!(
                writer,
                "{:>8} {:>6}  {}  {}",
                row.count,
                row.works,
                sanitize_terminal(name),
                sanitize_terminal(&group_display(&group)?)
            )
            .map_err(|error| CliError::Execution(execution_output_io(error)))?;
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
        emit(&mut writer, TableEvent::Complete(complete))?;
    } else {
        writeln!(
            writer,
            "{} documents, {} tokens admitted; {} rows in {} ms",
            complete.documents_admitted, complete.tokens_admitted, complete.rows, query_millis
        )
        .map_err(|error| CliError::Execution(execution_output_io(error)))?;
    }
    Ok(CommandOutcome::Success)
}

enum EntryKey {
    Group(SummaryGroupData),
    Printed {
        text: String,
        fields: Option<serde_json::Map<String, serde_json::Value>>,
    },
}

impl EntryKey {
    fn parse(text: &str) -> Result<Self, CliError> {
        match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text) {
            Ok(object) if object.contains_key("kind") => {
                serde_json::from_value(serde_json::Value::Object(object))
                    .map(Self::Group)
                    .map_err(|error| {
                        CliError::InvalidArgs(format!(
                            "table --entry: {text} is not the group of a `ling table` row: {error}"
                        ))
                    })
            }
            fields => Ok(Self::Printed {
                text: text.to_owned(),
                fields: fields.ok(),
            }),
        }
    }

    fn selects(&self, group: &Group<'_>) -> bool {
        match (self, group) {
            (Self::Group(expected), group) => group_data(group) == *expected,
            (Self::Printed { fields, .. }, Group::Key(entry)) => {
                fields.as_ref().is_some_and(|fields| {
                    fields.len() == entry.len()
                        && entry.iter().all(|(name, value)| {
                            fields.get(*name).is_some_and(|given| match value {
                                FieldValue::Scalar(value) => given.as_str() == Some(*value),
                                FieldValue::List(values) => given.as_array().is_some_and(|given| {
                                    given.len() == values.len()
                                        && given
                                            .iter()
                                            .zip(values)
                                            .all(|(given, value)| given.as_str() == Some(*value))
                                }),
                            })
                        })
                })
            }
            (
                Self::Printed { text, .. },
                Group::Surface(value) | Group::Unit(value) | Group::Term(value),
            ) => text == value,
        }
    }
}

fn find_entry<'a>(
    table: &CountTable,
    pattern_name: &'a str,
    entry_key: &str,
) -> Result<(&'a str, u32), CliError> {
    let pattern = table
        .pattern_names()
        .position(|name| name == pattern_name)
        .ok_or_else(|| {
            CliError::InvalidArgs(format!(
                "table --entry: the table has no pattern {pattern_name}"
            ))
        })?;
    let range = table.pattern_range(pattern).map_err(table_error)?;
    let key = EntryKey::parse(entry_key)?;
    let mut found = None;
    for entry in range {
        let entry = u32::try_from(entry)
            .map_err(|_| CliError::Corpus("table entry out of range".to_owned()))?;
        if key.selects(&table.group(entry).map_err(table_error)?) {
            if found.is_some() {
                return Err(CliError::InvalidArgs(format!(
                    "table --entry: pattern {pattern_name} has more than one entry printed as \
                     {entry_key}; give the `group` of its JSON row instead"
                )));
            }
            found = Some(entry);
        }
    }
    let entry = found.ok_or_else(|| {
        CliError::InvalidArgs(format!(
            "table --entry: pattern {pattern_name} has no entry {entry_key}; give the key as \
             the rows of `ling table` print it, or the `group` of its JSON row"
        ))
    })?;
    Ok((pattern_name, entry))
}

fn entry_by_metadata(
    table: &CountTable,
    pattern_name: &str,
    entry: u32,
    by: &str,
    documents: Vec<(u64, std::ops::Range<u64>, Vec<String>)>,
    admits: impl Fn(u64) -> bool,
) -> Result<Vec<TableMetadataValue>, CliError> {
    let document_ids = table.document_ids();
    let mut slot_of = std::collections::HashMap::<String, usize>::new();
    let mut values = Vec::<String>::new();
    let mut document_slots = vec![Vec::new(); document_ids.len()];
    for (id, _, document_values) in documents {
        let mut slots = document_values
            .into_iter()
            .map(|value| {
                let next = values.len();
                *slot_of.entry(value).or_insert_with_key(|value| {
                    values.push(value.clone());
                    next
                })
            })
            .collect::<Vec<_>>();
        if let Ok(document) = document_ids.binary_search(&id) {
            slots.sort_unstable();
            slots.dedup();
            document_slots[document] = slots;
        }
    }
    let no_value = values.len();
    for slots in &mut document_slots {
        if slots.is_empty() {
            slots.push(no_value);
        }
    }

    let admitted = document_ids
        .iter()
        .map(|id| admits(*id))
        .collect::<Vec<_>>();
    let mut totals = vec![
        Admission {
            documents_admitted: 0,
            sentences_admitted: 0,
            tokens_admitted: 0,
        };
        no_value + 1
    ];
    for (document, totals_of) in table.documents().enumerate() {
        if !admitted[document] {
            continue;
        }
        for slot in &document_slots[document] {
            let target = &mut totals[*slot];
            target.documents_admitted += 1;
            target.sentences_admitted += totals_of.sentences;
            target.tokens_admitted += totals_of.tokens;
        }
    }
    let mut counts = vec![(0u64, 0u64); no_value + 1];
    let (entry_documents, entry_counts) = table.entry_documents(entry).map_err(table_error)?;
    for (document, count) in entry_documents.iter().zip(entry_counts) {
        let document = *document as usize;
        if !admitted.get(document).copied().unwrap_or(false) {
            continue;
        }
        for slot in &document_slots[document] {
            counts[*slot].0 += u64::from(*count);
            counts[*slot].1 += 1;
        }
    }

    let group = group_data(&table.group(entry).map_err(table_error)?);
    let records = totals
        .into_iter()
        .zip(counts)
        .enumerate()
        .filter(|(_, (totals, _))| totals.documents_admitted > 0)
        .map(|(slot, (totals, (count, works)))| TableMetadataValue {
            key: by.to_owned(),
            value: values.get(slot).cloned(),
            documents_admitted: totals.documents_admitted,
            sentences_admitted: totals.sentences_admitted,
            tokens_admitted: totals.tokens_admitted,
            entry: TableEntryCount {
                pattern: pattern_name.to_owned(),
                group: group.clone(),
                count,
                works,
            },
        })
        .collect();
    Ok(records)
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
