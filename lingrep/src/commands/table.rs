//! `ling table`: per-pattern counts under `--where`, read from a count table.

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use lingrep_core::execution::SentenceAdmission;
use lingrep_core::scope::ScopeClause;
use lingrep_core::spill::{SpillPolicy, TempBudget};
use lingrep_corpus::{
    Admission, BlockingProvenance, CorpusArtifact, CountTable, CountTableError, CountTableKey,
    FieldValue, Group, GroupKind, GroupSchema, TopRow, ValueSlots,
};

use roaring::RoaringTreemap;

use super::CommandOutcome;
use super::query_scope::{document_values, parse_scope_flag, resolve_prepared_scope};
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
use crate::terminal::TerminalArbiter;

const RESIDENT_ROWS: usize = 1 << 20;
const MERGE_FAN_IN: usize = 16;

pub(super) fn table_error(error: CountTableError) -> CliError {
    match error {
        CountTableError::Io(error) => CliError::Aggregation(error),
        error => CliError::Corpus(error.to_string()),
    }
}

pub(super) fn open_table(
    path: &Path,
    artifact: &CorpusArtifact,
    catalog_fingerprint: &str,
) -> Result<(CountTable, CountTableKey), CliError> {
    let key = CountTableKey {
        analysis_identity: encode_digest(artifact.analysis_identity().as_bytes()),
        catalog_fingerprint: catalog_fingerprint.to_owned(),
        matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
    };
    let table =
        CountTable::open(path, &key).map_err(|error| CliError::Corpus(error.to_string()))?;
    Ok((table, key))
}

fn only_documents(option: &str) -> CliError {
    CliError::InvalidArgs(format!(
        "{option} accepts only @document conditions: a count table holds counts per document"
    ))
}

/// Rejects clauses that reference sections, sentence metadata or divisions.
pub(super) fn document_clause(
    option: &str,
    clause: Option<&str>,
) -> Result<Option<ScopeClause>, CliError> {
    let flag = option.rsplit(' ').next().unwrap_or(option);
    let clause = parse_scope_flag(flag, clause)?;
    if clause
        .as_ref()
        .is_some_and(|clause| clause.names_sections() || clause.requires_sentence_values())
    {
        return Err(only_documents(option));
    }
    Ok(clause)
}

pub(super) fn document_admission(
    reader: &BlockingProvenance,
    clause: &ScopeClause,
    option: &str,
) -> Result<RoaringTreemap, CliError> {
    let (resolution, _) = resolve_prepared_scope(reader, clause)?;
    let SentenceAdmission::Documents(admitted) = resolution.admission else {
        return Err(only_documents(option));
    };
    Ok(admitted)
}

pub fn run(
    args: &TableArgs,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let started = Instant::now();
    let catalog = load_and_compile(&args.catalog.catalog_input()?)?;
    let clause = document_clause("table --where", args.clause.as_deref())?;
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
    let (table, key) = open_table(&args.table, &artifact, &catalog.fingerprint)?;
    let lookup = Instant::now();
    let entry = entry
        .map(|(pattern, entry_key)| find_entry(&table, pattern, entry_key))
        .transpose()?;
    let lookup = lookup.elapsed();
    let provenance = (clause.is_some() || entry.is_some())
        .then(|| artifact.provenance_reader())
        .transpose()?;
    let admitted = match (&clause, &provenance) {
        (Some(clause), Some(reader)) => Some(document_admission(reader, clause, "table --where")?),
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
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
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
        if !terminal.try_claim_completion() {
            return Ok(CommandOutcome::Cancelled);
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

    if terminal.cancellation().is_cancelled() {
        return Ok(CommandOutcome::Cancelled);
    }
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
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
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
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
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

    fn candidates<'k>(&'k self, schemas: &'k [GroupSchema]) -> Vec<Group<'k>> {
        match self {
            Self::Group(SummaryGroupData::Surface { value }) => vec![Group::Surface(value)],
            Self::Group(SummaryGroupData::Unit { value }) => vec![Group::Unit(value)],
            Self::Group(SummaryGroupData::Term { value }) => vec![Group::Term(value)],
            Self::Group(SummaryGroupData::Key { fields }) => vec![Group::Key(
                fields
                    .iter()
                    .map(|field| match field {
                        ProtocolKeyField::Scalar { name, value } => {
                            (name.as_str(), FieldValue::Scalar(value))
                        }
                        ProtocolKeyField::List { name, values } => (
                            name.as_str(),
                            FieldValue::List(values.iter().map(String::as_str).collect()),
                        ),
                    })
                    .collect(),
            )],
            Self::Printed { text, fields } => {
                let mut groups = vec![Group::Surface(text), Group::Unit(text), Group::Term(text)];
                let Some(fields) = fields else {
                    return groups;
                };
                for schema in schemas {
                    if !matches!(schema.kind, GroupKind::Key | GroupKind::LexicalKey)
                        || schema.names.len() != fields.len()
                    {
                        continue;
                    }
                    let key = schema
                        .names
                        .iter()
                        .map(|name| {
                            Some(match fields.get(name)? {
                                serde_json::Value::String(value) => {
                                    (name.as_str(), FieldValue::Scalar(value))
                                }
                                serde_json::Value::Array(values) => (
                                    name.as_str(),
                                    FieldValue::List(
                                        values
                                            .iter()
                                            .map(serde_json::Value::as_str)
                                            .collect::<Option<_>>()?,
                                    ),
                                ),
                                _ => return None,
                            })
                        })
                        .collect::<Option<Vec<_>>>();
                    if let Some(key) = key {
                        groups.push(Group::Key(key));
                    }
                }
                groups
            }
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
    let key = EntryKey::parse(entry_key)?;
    let mut found = None;
    for group in key.candidates(table.schemas()) {
        let Some(entry) = table.find_entry(pattern, &group).map_err(table_error)? else {
            continue;
        };
        if found.is_some_and(|found| found != entry) {
            return Err(CliError::InvalidArgs(format!(
                "table --entry: pattern {pattern_name} has more than one entry printed as \
                 {entry_key}; give the `group` of its JSON row instead"
            )));
        }
        found = Some(entry);
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
    let corpus_index = documents
        .iter()
        .enumerate()
        .map(|(index, (id, _, _))| (*id, index))
        .collect::<std::collections::HashMap<_, _>>();
    let slots = ValueSlots::number(documents.into_iter().map(|(_, _, values)| values))
        .map_err(|error| CliError::Corpus(error.to_string()))?;
    let slots = slots.select(
        table
            .document_ids()
            .iter()
            .map(|id| corpus_index.get(id).copied()),
    );
    let (entry_documents, entry_counts) = table.entry_documents(entry).map_err(table_error)?;
    let counts = table
        .value_counts(
            &slots,
            &table.mask(admits),
            entry_documents
                .iter()
                .zip(entry_counts)
                .map(|(document, count)| (*document as usize, u64::from(*count))),
        )
        .map_err(table_error)?;

    let group = group_data(&table.group(entry).map_err(table_error)?);
    let records = counts
        .into_iter()
        .enumerate()
        .filter(|(_, sums)| sums.admission.documents_admitted > 0)
        .map(|(slot, sums)| TableMetadataValue {
            key: by.to_owned(),
            value: slots.value(slot).map(str::to_owned),
            documents_admitted: sums.admission.documents_admitted,
            sentences_admitted: sums.admission.sentences_admitted,
            tokens_admitted: sums.admission.tokens_admitted,
            entry: TableEntryCount {
                pattern: pattern_name.to_owned(),
                group: group.clone(),
                count: sums.count,
                works: sums.works,
            },
        })
        .collect();
    Ok(records)
}

pub(super) fn group_data(group: &Group<'_>) -> SummaryGroupData {
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

pub(super) fn group_display(group: &Group<'_>) -> Result<String, CliError> {
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
