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
    FieldValue, Group, GroupKind, GroupSchema, KeyEquals, TopRow, ValueSlots,
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
use crate::protocol::query::{DocumentData, ProtocolKeyField, SummaryGroupData};
use crate::protocol::table::{
    TableCell, TableComplete, TableDocument, TableEntry, TableEntryCount, TableEnvelope,
    TableEvent, TableKeyEquals, TableMetadataValue, TablePattern, TableRow, TableRun, TableSubset,
};
use crate::protocol::{write_jsonl, write_jsonl_buffered};
use crate::terminal::TerminalArbiter;

const RESIDENT_ROWS: usize = 1 << 20;
const MERGE_FAN_IN: usize = 16;

pub(super) fn table_error(error: CountTableError) -> CliError {
    CliError::from(error)
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
        CountTable::open(path, &key).map_err(|error| CliError::from(error).at_path(path))?;
    table
        .verify()
        .map_err(|error| CliError::from(error).at_path(path))?;
    artifact
        .check_document_ids(table.document_ids())
        .map_err(|error| CliError::from(error).at_path(path))?;
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

enum TableMode<'a> {
    ByDocument {
        metadata: lingrep_core::corpus::SourceMetadataProjection,
    },
    EntryByMetadata {
        raw_entry: &'a str,
        pattern: &'a str,
        entry_key: &'a str,
        by_metadata: &'a str,
    },
    Rows {
        top: Option<u64>,
        select: Option<&'a str>,
        key_equals: KeyEquals,
    },
}

impl<'a> TableMode<'a> {
    fn parse(args: &'a TableArgs, output: OutputMode) -> Result<Self, CliError> {
        if args.by_document && output != OutputMode::Jsonl {
            return Err(CliError::InvalidArgs(
                "table --by-document requires --json".into(),
            ));
        }
        if let Some(key) = args
            .source_metadata
            .iter()
            .find(|key| !key.starts_with("@document."))
        {
            return Err(CliError::InvalidArgs(format!(
                "table --source-metadata accepts only @document.KEY, not {key}"
            )));
        }
        let metadata =
            lingrep_core::corpus::SourceMetadataProjection::new(args.source_metadata.clone())
                .map_err(|error| CliError::InvalidArgs(error.to_string()))?;
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
        let key_equals = KeyEquals::new(
            args.key
                .iter()
                .map(|item| match item.split_once('=') {
                    Some((field, value)) if !field.is_empty() => {
                        Ok((field.to_owned(), value.to_owned()))
                    }
                    _ => Err(CliError::InvalidArgs(format!(
                        "table --key takes FIELD=VALUE, not {item}"
                    ))),
                })
                .collect::<Result<_, _>>()?,
        );
        if args.by_document {
            Ok(Self::ByDocument { metadata })
        } else if let (Some((pattern, entry_key)), Some(by_metadata)) =
            (entry, args.by_metadata.as_deref())
        {
            Ok(Self::EntryByMetadata {
                raw_entry: args.entry.as_deref().expect("checked entry"),
                pattern,
                entry_key,
                by_metadata,
            })
        } else {
            Ok(Self::Rows {
                top: resolve_summary_top(output, args.top),
                select: args.select.as_deref(),
                key_equals,
            })
        }
    }
}

pub fn run(
    args: &TableArgs,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let table_path = args
        .table
        .as_deref()
        .or(args.table_option.as_deref())
        .ok_or_else(|| CliError::InvalidArgs("table requires TABLE or --table TABLE".into()))?;
    let started = Instant::now();
    let mode = TableMode::parse(args, output)?;
    let catalog = load_and_compile(&args.catalog.catalog_input()?)?;
    let clause = document_clause("table --where", args.clause.as_deref())?;
    let artifact = CorpusArtifact::open(&args.corpus)?;
    let (table, key) = open_table(table_path, &artifact, &catalog.fingerprint)?;

    match mode {
        TableMode::ByDocument { metadata } => {
            let reader = artifact.provenance_reader()?;
            let admitted = clause
                .as_ref()
                .map(|clause| document_admission(&reader, clause, "table --where"))
                .transpose()?;
            export_documents(
                table_path,
                args.clause.as_deref(),
                &artifact,
                &table,
                &reader,
                &key,
                &metadata,
                admitted.as_ref(),
                terminal,
                started,
            )
        }
        TableMode::EntryByMetadata {
            raw_entry,
            pattern,
            entry_key,
            by_metadata,
        } => {
            let lookup = Instant::now();
            let (pattern, entry) = find_entry(&table, pattern, entry_key)?;
            let lookup = lookup.elapsed();
            let reader = artifact.provenance_reader()?;
            let admitted = clause
                .as_ref()
                .map(|clause| document_admission(&reader, clause, "table --where"))
                .transpose()?;
            let admits = |id: u64| {
                admitted
                    .as_ref()
                    .is_none_or(|admitted| admitted.contains(id))
            };
            let documents = document_values(&reader, by_metadata)?;
            let queried = Instant::now();
            let records =
                entry_by_metadata(&table, pattern, entry, by_metadata, documents, admits)?;
            let admission = table.top(&table.mask(admits), 0).map_err(table_error)?;
            let query_millis = (lookup + queried.elapsed())
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            if terminal.cancellation().is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            let stdout = io::stdout();
            let mut writer = io::BufWriter::with_capacity(256 * 1024, stdout.lock());
            let mut scratch = Vec::new();
            let emit_immediate = |writer: &mut io::BufWriter<io::StdoutLock<'_>>,
                                  event: TableEvent| {
                write_jsonl(writer, &TableEnvelope::new(event))
                    .map_err(|error| CliError::Execution(execution_output_error(error)))
            };
            let emit_buffered = |writer: &mut io::BufWriter<io::StdoutLock<'_>>,
                                 scratch: &mut Vec<u8>,
                                 event: TableEvent| {
                write_jsonl_buffered(writer, scratch, &TableEnvelope::new(event))
                    .map_err(|error| CliError::Execution(execution_output_error(error)))
            };
            emit_immediate(
                &mut writer,
                TableEvent::Run(TableRun {
                    table: table_path.display().to_string(),
                    analysis_identity: key.analysis_identity.clone(),
                    catalog_fingerprint: key.catalog_fingerprint.clone(),
                    matcher_revision: key.matcher_revision.clone(),
                    clause: args.clause.clone(),
                    requested_top: None,
                    by_document: false,
                    source_identity: None,
                    published_source_metadata: Vec::new(),
                    entry: Some(raw_entry.to_owned()),
                    by_metadata: Some(by_metadata.to_owned()),
                    select: None,
                    key: Vec::new(),
                }),
            )?;
            for record in records {
                emit_buffered(&mut writer, &mut scratch, TableEvent::MetadataValue(record))?;
            }
            if !terminal.try_claim_completion() {
                return Ok(CommandOutcome::Cancelled);
            }
            emit_immediate(
                &mut writer,
                TableEvent::Complete(TableComplete {
                    documents_admitted: admission.documents_admitted,
                    sentences_admitted: admission.sentences_admitted,
                    tokens_admitted: admission.tokens_admitted,
                    rows: 0,
                    entries: None,
                    query_millis,
                    elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                }),
            )?;
            Ok(CommandOutcome::Success)
        }
        TableMode::Rows {
            top,
            select,
            key_equals,
        } => {
            let selected = select
                .map(|name| {
                    table
                        .pattern_names()
                        .position(|candidate| candidate == name)
                        .ok_or_else(|| {
                            CliError::InvalidArgs(format!(
                                "table --select: the table has no pattern {name}"
                            ))
                        })
                })
                .transpose()?;
            if let Some(pattern) = selected
                && let Err(error) = table
                    .check_key_equals(pattern, &key_equals)
                    .map_err(table_error)?
            {
                return Err(CliError::InvalidArgs(format!("table --key: {error}")));
            }
            let admitted = clause
                .as_ref()
                .map(|clause| {
                    let reader = artifact.provenance_reader()?;
                    document_admission(&reader, clause, "table --where")
                })
                .transpose()?;
            let admits = |id: u64| {
                admitted
                    .as_ref()
                    .is_none_or(|admitted| admitted.contains(id))
            };
            let mask = table.mask(admits);
            let queried = Instant::now();
            let totals = table.pattern_totals(&mask).map_err(table_error)?;
            let (admission, mut rows): (Admission, Box<dyn Iterator<Item = _>>) =
                if let Some(pattern) = selected {
                    let admission = table.top(&mask, 0).map_err(table_error)?;
                    let admission = Admission {
                        documents_admitted: admission.documents_admitted,
                        sentences_admitted: admission.sentences_admitted,
                        tokens_admitted: admission.tokens_admitted,
                    };
                    let rows = table
                        .ranked_pattern_rows(
                            pattern,
                            &mask,
                            SpillPolicy {
                                max_pending_bytes: usize::MAX,
                                max_pending_keys: RESIDENT_ROWS,
                                merge_fan_in: MERGE_FAN_IN,
                            },
                            Arc::new(TempBudget::new(args.max_temp_bytes)),
                            terminal.cancellation(),
                        )
                        .map_err(table_error)?
                        .map(move |row| row.map(|row| (pattern, row)));
                    (admission, Box::new(rows))
                } else if let Some(top) = top {
                    let result = table
                        .top(&mask, usize::try_from(top).unwrap_or(usize::MAX))
                        .map_err(table_error)?;
                    let admission = Admission {
                        documents_admitted: result.documents_admitted,
                        sentences_admitted: result.sentences_admitted,
                        tokens_admitted: result.tokens_admitted,
                    };
                    let rows = result.patterns.into_iter().flat_map(|pattern| {
                        pattern.rows.into_iter().map(move |row| {
                            Ok::<(usize, TopRow), CountTableError>((pattern.pattern, row))
                        })
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
            let mut writer = io::BufWriter::with_capacity(256 * 1024, stdout.lock());
            let mut scratch = Vec::new();
            let emit_immediate = |writer: &mut io::BufWriter<io::StdoutLock<'_>>,
                                  event: TableEvent| {
                write_jsonl(writer, &TableEnvelope::new(event))
                    .map_err(|error| CliError::Execution(execution_output_error(error)))
            };
            let emit_buffered = |writer: &mut io::BufWriter<io::StdoutLock<'_>>,
                                 scratch: &mut Vec<u8>,
                                 event: TableEvent| {
                write_jsonl_buffered(writer, scratch, &TableEnvelope::new(event))
                    .map_err(|error| CliError::Execution(execution_output_error(error)))
            };
            let jsonl = output == OutputMode::Jsonl;
            if jsonl {
                emit_immediate(
                    &mut writer,
                    TableEvent::Run(TableRun {
                        table: table_path.display().to_string(),
                        analysis_identity: key.analysis_identity.clone(),
                        catalog_fingerprint: key.catalog_fingerprint.clone(),
                        matcher_revision: key.matcher_revision.clone(),
                        clause: args.clause.clone(),
                        requested_top: top,
                        by_document: false,
                        source_identity: None,
                        published_source_metadata: Vec::new(),
                        entry: None,
                        by_metadata: None,
                        select: select.map(str::to_owned),
                        key: key_fields(&key_equals),
                    }),
                )?;
            }
            for total in totals
                .iter()
                .filter(|total| selected.is_none_or(|pattern| pattern == total.pattern))
            {
                let name = names
                    .get(total.pattern)
                    .copied()
                    .ok_or_else(|| CliError::Corpus("table pattern out of range".to_owned()))?;
                if jsonl {
                    emit_buffered(
                        &mut writer,
                        &mut scratch,
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
            let subset = match selected.filter(|_| !key_equals.is_empty()) {
                Some(pattern) => Some(
                    table
                        .key_subset(pattern, &mask, &key_equals, terminal.cancellation())
                        .map_err(table_error)?
                        .map_err(|error| CliError::InvalidArgs(format!("table --key: {error}")))?,
                ),
                None => None,
            };
            if let (Some(subset), Some(pattern)) = (subset, select) {
                if jsonl {
                    emit_buffered(
                        &mut writer,
                        &mut scratch,
                        TableEvent::Subset(TableSubset {
                            pattern: pattern.to_owned(),
                            key: key_fields(&key_equals),
                            occurrences: subset.occurrences,
                            works: subset.works,
                            entries: subset.entries,
                        }),
                    )?;
                } else {
                    writeln!(
                        writer,
                        "subset {} {}: {} occurrences in {} documents, {} groups",
                        sanitize_terminal(pattern),
                        sanitize_terminal(
                            &key_equals
                                .fields()
                                .iter()
                                .map(|(field, value)| format!("{field}={value}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        ),
                        subset.occurrences,
                        subset.works,
                        subset.entries
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
                if selected.is_some() {
                    if top.is_some_and(|top| emitted >= top) {
                        break;
                    }
                    if !table
                        .key_admits(pattern, row.entry, &key_equals)
                        .map_err(table_error)?
                        .map_err(|error| CliError::InvalidArgs(format!("table --key: {error}")))?
                    {
                        continue;
                    }
                }
                let share = subset.map(|subset| row.count as f64 / subset.occurrences as f64);
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
                    emit_buffered(
                        &mut writer,
                        &mut scratch,
                        TableEvent::Row(TableRow {
                            pattern: name.to_owned(),
                            rank,
                            group: group_data(&group),
                            count: row.count,
                            works: row.works,
                            share,
                        }),
                    )?;
                } else {
                    let share =
                        share.map_or_else(String::new, |share| format!(" {:>6.1}%", share * 100.0));
                    writeln!(
                        writer,
                        "{:>8} {:>6}{share}  {}  {}",
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
                entries: None,
                query_millis,
                elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            };
            if !terminal.try_claim_completion() {
                return Ok(CommandOutcome::Cancelled);
            }
            if jsonl {
                emit_immediate(&mut writer, TableEvent::Complete(complete))?;
            } else {
                writeln!(
                    writer,
                    "{} documents, {} tokens admitted; {} rows in {} ms",
                    complete.documents_admitted,
                    complete.tokens_admitted,
                    complete.rows,
                    query_millis
                )
                .map_err(|error| CliError::Execution(execution_output_io(error)))?;
                writer
                    .flush()
                    .map_err(|error| CliError::Execution(execution_output_io(error)))?;
            }
            Ok(CommandOutcome::Success)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn export_documents(
    table_path: &std::path::Path,
    clause: Option<&str>,
    artifact: &CorpusArtifact,
    table: &CountTable,
    reader: &BlockingProvenance,
    key: &CountTableKey,
    metadata: &lingrep_core::corpus::SourceMetadataProjection,
    admitted: Option<&RoaringTreemap>,
    terminal: &TerminalArbiter,
    started: Instant,
) -> Result<CommandOutcome, CliError> {
    let queried = Instant::now();
    let admits = |id| admitted.is_none_or(|admitted| admitted.contains(id));
    let mut admission = Admission {
        documents_admitted: 0,
        sentences_admitted: 0,
        tokens_admitted: 0,
    };
    for document in table
        .documents()
        .filter(|document| admits(document.document_id))
    {
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
        admission.documents_admitted += 1;
        admission.sentences_admitted = admission
            .sentences_admitted
            .checked_add(document.sentences)
            .ok_or_else(|| CliError::Corpus("table sentence count exceeds u64".into()))?;
        admission.tokens_admitted = admission
            .tokens_admitted
            .checked_add(document.tokens)
            .ok_or_else(|| CliError::Corpus("table token count exceeds u64".into()))?;
    }
    if terminal.cancellation().is_cancelled() {
        return Ok(CommandOutcome::Cancelled);
    }
    let stdout = io::stdout();
    let mut writer = io::BufWriter::with_capacity(256 * 1024, stdout.lock());
    let mut scratch = Vec::new();
    let emit_immediate = |writer: &mut io::BufWriter<io::StdoutLock<'_>>, event| {
        write_jsonl(writer, &TableEnvelope::new(event))
            .map_err(|error| CliError::Execution(execution_output_error(error)))
    };
    let emit_buffered =
        |writer: &mut io::BufWriter<io::StdoutLock<'_>>, scratch: &mut Vec<u8>, event| {
            write_jsonl_buffered(writer, scratch, &TableEnvelope::new(event))
                .map_err(|error| CliError::Execution(execution_output_error(error)))
        };
    emit_immediate(
        &mut writer,
        TableEvent::Run(TableRun {
            table: table_path.display().to_string(),
            analysis_identity: key.analysis_identity.clone(),
            catalog_fingerprint: key.catalog_fingerprint.clone(),
            matcher_revision: key.matcher_revision.clone(),
            source_identity: Some(encode_digest(artifact.source_identity().as_bytes())),
            clause: clause.map(str::to_owned),
            requested_top: None,
            by_document: true,
            published_source_metadata: metadata.keys().to_vec(),
            entry: None,
            by_metadata: None,
            select: None,
            key: Vec::new(),
        }),
    )?;
    let mut documents = 0;
    let mut next = 0;
    loop {
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
        let page = reader.block_on(reader.view().document_metadata_page(next))?;
        if page.is_empty() {
            break;
        }
        for document in page {
            if terminal.cancellation().is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            next = document
                .first_sentence
                .checked_add(document.sentence_count)
                .filter(|end| *end > next)
                .ok_or_else(|| {
                    CliError::Corpus("table document metadata has an invalid sentence range".into())
                })?;
            let id = document.document_id.get();
            if !admits(id) {
                continue;
            }
            let Ok(index) = table.document_ids().binary_search(&id) else {
                continue;
            };
            let totals = table
                .documents()
                .nth(index)
                .ok_or_else(|| CliError::Corpus("table document index is missing".into()))?;
            let source = artifact.document_by_id(document.document_id)?;
            let source_metadata = if metadata.keys().is_empty() {
                None
            } else {
                let values = lingrep_core::corpus::metadata_from_json(
                    serde_json::from_str(document.metadata_json.as_deref().unwrap_or("{}"))?,
                    "table document metadata",
                )
                .map_err(CliError::corpus)?;
                Some(Arc::new(
                    metadata
                        .project(
                            &values,
                            &Default::default(),
                            &lingrep_core::corpus::StructuralContext::default(),
                        )
                        .map_err(CliError::corpus)?,
                ))
            };
            emit_buffered(
                &mut writer,
                &mut scratch,
                TableEvent::Document(TableDocument {
                    document_id: id,
                    data: DocumentData {
                        grouping_coverage: None,
                        document: source.ordinal,
                        source_name: source.logical_name,
                        sentences_admitted: totals.sentences,
                        tokens_admitted: totals.tokens,
                        source_metadata,
                    },
                }),
            )?;
            documents += 1;
        }
    }
    if documents != admission.documents_admitted {
        return Err(CliError::Corpus(
            "count table documents are missing from the corpus metadata".into(),
        ));
    }
    for (pattern, name) in table.pattern_names().enumerate() {
        for entry in table.pattern_range(pattern).map_err(table_error)? {
            if terminal.cancellation().is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            let entry = u32::try_from(entry)
                .map_err(|_| CliError::Corpus("count table entry ID exceeds u32".into()))?;
            emit_buffered(
                &mut writer,
                &mut scratch,
                TableEvent::Entry(TableEntry {
                    entry,
                    pattern: name.to_owned(),
                    group: group_data(&table.group(entry).map_err(table_error)?),
                }),
            )?;
        }
    }
    let mut cells = 0u64;
    for entry in 0..table.entry_count() {
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
        let entry = u32::try_from(entry)
            .map_err(|_| CliError::Corpus("count table entry ID exceeds u32".into()))?;
        let (documents, counts) = table.entry_documents(entry).map_err(table_error)?;
        for (document, count) in documents.iter().zip(counts) {
            if terminal.cancellation().is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            let id = table.document_ids()[*document as usize];
            if admits(id) {
                emit_buffered(
                    &mut writer,
                    &mut scratch,
                    TableEvent::Cell(TableCell {
                        document_id: id,
                        entry,
                        count: u64::from(*count),
                    }),
                )?;
                cells += 1;
            }
        }
    }
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    emit_immediate(
        &mut writer,
        TableEvent::Complete(TableComplete {
            documents_admitted: admission.documents_admitted,
            sentences_admitted: admission.sentences_admitted,
            tokens_admitted: admission.tokens_admitted,
            rows: cells,
            entries: Some(table.entry_count()),
            query_millis: queried.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        }),
    )?;
    Ok(CommandOutcome::Success)
}

fn key_fields(key: &KeyEquals) -> Vec<TableKeyEquals> {
    key.fields()
        .iter()
        .map(|(name, value)| TableKeyEquals {
            name: name.clone(),
            value: value.clone(),
        })
        .collect()
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
        .map_err(CliError::corpus)?;
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
