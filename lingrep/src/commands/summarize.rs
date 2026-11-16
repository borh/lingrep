//! Exact bounded `summarize` orchestration and deferred publication.

use lingrep_core::fsm::metadata_projection::{MetadataPattern, PatternMetadataSelection};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::time::Instant;

use lingrep_core::execution::{
    CancellationFlag, ExecutionError, MatchSink, PolicySelection, Publication, RunOutcome,
    SentenceAdmission, SinkControl, cached_run_attributes, execute_analyzed,
};
use lingrep_core::fsm::{CatalogPolicy, GroupBy, Match, PublishedRecord, RecordView};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use roaring::RoaringTreemap;
use serde::Serialize;
use serde::ser::SerializeMap;

use super::CommandOutcome;
use super::query_completion::FilesystemCompletion;
use super::query_resolution::{QueryProjection, ResolvedMatchingQuery, resolve_matching_query};
use super::query_scope::{document_values, parse_scope};
use super::query_support::{
    execution_output_error, execution_output_io, protocol_execution_limits, publish_query_jsonl,
    random_id, write_vectors_notice, write_warning,
};
use crate::catalog_input::load_and_compile_with_vectors;
use crate::cli::{ExecutionCliConfig, SummarizeArgs};
use crate::direct_source::matched_surface;
use crate::error::{CliError, FailureCode};
use crate::output::{SummaryTsvRow, sanitize_terminal, write_summary_tsv};
use crate::presentation::{OutputMode, ResultDelivery, resolve_summary_top};
use crate::protocol::diagnostic::DiagnosticStage;
use crate::protocol::query::{
    Coverage, DefinitionBody, DefinitionData, DocumentData, InventoryStats, MatchOrder,
    ProtocolKeyField, QueryComplete, QueryEnvelope, QueryEvent, QueryMode, QueryRun,
    ResourceHighWater, RunCounters, SourceSpan, SpanConvention, SummaryData, SummaryGroupData,
    TemporaryStorageLimits, Termination,
};
use crate::protocol::write_jsonl;
use crate::summary::{
    AggregationGroup, AggregationInput, GroupingTally, MetadataValueTally, OwnedAggregationInput,
    SectionTally, SummaryAggregator, SummaryPartition, SummaryRow, SummaryRows, shard_hash,
};
use crate::terminal::PresentationProgress;
use crate::terminal::TerminalArbiter;
use crate::window_summary::WindowTally;

fn summary_pattern<'a>(
    pattern_names: &[&'a str],
    row: &SummaryRow,
) -> Result<&'a str, ExecutionError> {
    pattern_names
        .get(row.partition.pattern_ordinal as usize)
        .copied()
        .ok_or_else(|| ExecutionError::Output {
            kind: io::ErrorKind::InvalidData,
            message: "summary pattern ordinal is out of range".to_owned(),
        })
}

trait SummaryEventWriter {
    fn write_summary(&mut self, pattern: &str, row: &SummaryRow) -> Result<(), ExecutionError>;
    /// Writes `rows` in order and stops at the first row that fails.
    fn write_summaries(
        &mut self,
        pattern_names: &[&str],
        rows: &[SummaryRow],
    ) -> Result<(), ExecutionError> {
        for row in rows {
            self.write_summary(summary_pattern(pattern_names, row)?, row)?;
        }
        Ok(())
    }
    fn write_complete(&mut self, complete: &QueryComplete) -> Result<(), ExecutionError>;
}

struct MachineSummaryEventWriter<'a, W> {
    writer: &'a mut W,
    output: OutputMode,
    pool: &'a rayon::ThreadPool,
}

const SUMMARY_PUBLISH_ROWS: usize = 16 * 1024;
const SUMMARY_SERIALIZE_ROWS: usize = 256;

impl<W: Write> SummaryEventWriter for MachineSummaryEventWriter<'_, W> {
    fn write_summary(&mut self, pattern: &str, row: &SummaryRow) -> Result<(), ExecutionError> {
        match self.output {
            OutputMode::Jsonl => write_jsonl(
                self.writer,
                &QueryEnvelope::new(QueryEvent::Summary(summary_data(pattern, row))),
            )
            .map_err(execution_output_error),
            OutputMode::Tsv => {
                let group = aggregation_group_display(&row.group);
                write_summary_tsv(
                    self.writer,
                    &SummaryTsvRow {
                        pattern: pattern.to_owned(),
                        rank: row.rank,
                        group,
                        count: row.count,
                    },
                )
                .map_err(execution_output_error)
            }
            OutputMode::Human => writeln!(
                self.writer,
                "{:>8}  {}  {}",
                row.count,
                sanitize_terminal(pattern),
                sanitize_terminal(&aggregation_group_display(&row.group))
            )
            .map_err(execution_output_io),
        }
    }

    fn write_summaries(
        &mut self,
        pattern_names: &[&str],
        rows: &[SummaryRow],
    ) -> Result<(), ExecutionError> {
        if self.output != OutputMode::Jsonl {
            for row in rows {
                self.write_summary(summary_pattern(pattern_names, row)?, row)?;
            }
            return Ok(());
        }
        let serialized = self.pool.install(|| {
            rows.par_chunks(SUMMARY_SERIALIZE_ROWS)
                .map(|rows| {
                    let mut lines = Vec::new();
                    for row in rows {
                        let pattern = summary_pattern(pattern_names, row)?;
                        serde_json::to_writer(
                            &mut lines,
                            &QueryEnvelope::new(QueryEvent::Summary(summary_data(pattern, row))),
                        )
                        .map_err(|error| execution_output_error(error.into()))?;
                        lines.push(b'\n');
                    }
                    Ok(lines)
                })
                .collect::<Vec<Result<Vec<u8>, ExecutionError>>>()
        });
        for lines in serialized {
            self.writer
                .write_all(&lines?)
                .map_err(execution_output_io)?;
        }
        self.writer.flush().map_err(execution_output_io)
    }

    fn write_complete(&mut self, complete: &QueryComplete) -> Result<(), ExecutionError> {
        match self.output {
            OutputMode::Jsonl => write_jsonl(
                self.writer,
                &QueryEnvelope::new(QueryEvent::Complete(complete.clone())),
            )
            .map_err(execution_output_error),
            OutputMode::Human => writeln!(
                self.writer,
                "full coverage; {} records; completed in {} ms",
                complete.counters.records_emitted, complete.elapsed_millis
            )
            .map_err(execution_output_io),
            OutputMode::Tsv => Ok(()),
        }
    }
}

fn publish_summary_rows_and_complete(
    finalized: &mut impl SummaryRows,
    top: Option<u64>,
    pattern_names: &[&str],
    mut complete: QueryComplete,
    writer: &mut impl SummaryEventWriter,
) -> Result<u64, CliError> {
    let mut publication_error = None;
    let mut batch = Vec::with_capacity(SUMMARY_PUBLISH_ROWS);
    let mut publish = |batch: &mut Vec<SummaryRow>| {
        let written = writer.write_summaries(pattern_names, batch);
        batch.clear();
        written.map_err(|error| {
            let message = error.to_string();
            publication_error = Some(error);
            io::Error::other(message)
        })
    };
    let visit_result = finalized
        .visit(top, |row| {
            batch.push(row);
            if batch.len() == SUMMARY_PUBLISH_ROWS {
                publish(&mut batch)
            } else {
                Ok(())
            }
        })
        .and_then(|emitted| publish(&mut batch).map(|()| emitted));
    let emitted = match visit_result {
        Ok(emitted) => emitted,
        Err(_) if publication_error.is_some() => {
            return Err(CliError::Execution(
                publication_error.expect("checked summary publication error"),
            ));
        }
        Err(error) => return Err(CliError::Aggregation(error)),
    };
    complete.counters.records_emitted = emitted;
    writer.write_complete(&complete)?;
    Ok(emitted)
}

pub fn run(
    args: &SummarizeArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let top = resolve_summary_top(output, args.top);
    let started = Instant::now();
    let catalog =
        load_and_compile_with_vectors(&args.catalog.catalog_input()?, args.vectors.as_deref())?;
    write_vectors_notice(&catalog, config)?;
    let word_vectors = super::query_support::word_vectors_report(&catalog);
    let descriptors = catalog.authored_definitions()?;
    let catalog_fingerprint = catalog.fingerprint.clone();
    let table_fingerprint = catalog.fingerprint.clone();
    let mut compiled = catalog.output;
    compiled.matcher = compiled.matcher.with_overlaps(args.overlaps.into());
    let projected_attributes = cached_run_attributes(&compiled.matcher);
    let catalog_policy = compiled.matcher.catalog_policy();
    let pattern_ordinals = catalog_policy
        .published_entries(
            descriptors
                .iter()
                .map(|descriptor| (descriptor.name.as_str(), descriptor.catalog_ordinal)),
        )
        .map(|(name, ordinal)| (name.to_owned(), ordinal))
        .collect::<BTreeMap<_, _>>();

    let pattern_metadata_selection = PatternMetadataSelection::parse(
        args.pattern_filter.as_deref(),
        args.group_patterns_by.as_deref(),
    )
    .map_err(CliError::InvalidArgs)?;
    let metadata_patterns = lingrep_core::fsm::metadata_projection::coalesce_metadata(
        descriptors.iter().map(|descriptor| MetadataPattern {
            ordinal: descriptor.catalog_ordinal,
            name: catalog_policy
                .published_name(descriptor.name.as_str())
                .to_owned(),
            strings: descriptor
                .metadata
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                })
                .collect(),
            ambiguous: BTreeSet::new(),
        }),
    );
    let pattern_projection = pattern_metadata_selection
        .project(&metadata_patterns)
        .map_err(|error| CliError::InvalidArgs(error.to_string()))?;

    // Parsed before any input is opened: a clause is authored text, so a bad
    // one is a usage error rather than a run that reads a corpus and fails.
    let scope_clause = parse_scope(args.selection.clause.as_deref())?;
    let sample = super::query_support::resolve_query_sample(
        &args.sample,
        args.cache.cache,
        args.tsv,
        output,
        QueryMode::Summarize,
    )?;

    if args.grouping_coverage && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--grouping-coverage requires --json".to_owned(),
        ));
    }
    if args.by_document && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--by-document requires --json".to_owned(),
        ));
    }
    if args.by_section && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--by-section requires --json".to_owned(),
        ));
    }
    if args.by_metadata.is_some() && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--by-metadata requires --json".to_owned(),
        ));
    }
    if let Some(key) = args
        .by_metadata
        .as_ref()
        .filter(|key| key.strip_prefix("@document.").is_none_or(str::is_empty))
    {
        return Err(CliError::InvalidArgs(format!(
            "summarize --by-metadata accepts only @document.KEY, not {key}"
        )));
    }
    if args.write_table.is_some() && (args.selection.clause.is_some() || sample.is_some()) {
        return Err(CliError::InvalidArgs(
            "--write-table counts the whole corpus; it does not accept --where or --sample"
                .to_owned(),
        ));
    }
    if let Some(key) = args
        .source_metadata
        .iter()
        .find(|key| !key.starts_with("@document."))
    {
        return Err(CliError::InvalidArgs(format!(
            "summarize --source-metadata accepts only @document.KEY, not {key}"
        )));
    }
    let metadata_projection = if args.source_metadata.is_empty() {
        None
    } else {
        Some(
            lingrep_core::corpus::SourceMetadataProjection::new(args.source_metadata.clone())
                .map_err(|error| CliError::InvalidArgs(error.to_string()))?,
        )
    };

    if let Some(path) = &args.write_table {
        if args.input.corpus.is_none() && !args.cache.cache {
            return Err(CliError::InvalidArgs(
                "--write-table requires a prepared corpus: pass --corpus or --cache".to_owned(),
            ));
        }
        lingrep_corpus::CountTableWriter::check_destination(path)
            .map_err(|error| CliError::from(error).at_path(path))?;
    }

    // Before query execution: direct execution is the default; `--cache`
    // observes ROOT, then reuses or creates a compatible artifact beside it.
    let resolved_corpus = crate::corpus_mode::resolve(
        &args.input,
        &args.cache,
        &args.input.markdown,
        config,
        terminal,
        output,
    )?;
    let selected_corpus = resolved_corpus.corpus.as_deref();
    if args.by_section && selected_corpus.is_none() {
        return Err(CliError::InvalidArgs(
            "--by-section requires a prepared corpus: pass --corpus or --cache".to_owned(),
        ));
    }
    if args.write_table.is_some() && selected_corpus.is_none() {
        return Err(CliError::InvalidArgs(
            "--write-table requires a prepared corpus: pass --corpus or --cache".to_owned(),
        ));
    }
    if args.by_metadata.is_some() && selected_corpus.is_none() {
        return Err(CliError::InvalidArgs(
            "--by-metadata requires a prepared corpus: pass --corpus or --cache".to_owned(),
        ));
    }
    let ResolvedMatchingQuery {
        source,
        source_stats,
        lifecycle,
        analysis,
        bound,
        run: run_facts,
        scope,
        provenance,
    } = resolve_matching_query(
        &args.input,
        selected_corpus,
        &compiled,
        QueryProjection {
            selected: &projected_attributes,
            additional: &[],
            all: false,
        },
        scope_clause.as_ref(),
        config,
        terminal,
        QueryMode::Summarize,
        false,
        metadata_projection.clone(),
        sample.filter(|_| args.sample.blocks),
        // A sentence the posting index skips still counts toward its
        // document's admitted tokens.
        sample.is_none()
            && scope_clause.is_none()
            && !args.by_document
            && args.write_table.is_none()
            && !args.by_section
            && args.by_metadata.is_none(),
        args.by_section || args.by_metadata.is_some(),
    )?;
    let provenance = || {
        provenance
            .as_deref()
            .expect("a prepared query opens provenance for --by-section and --by-metadata")
    };
    let sections = args
        .by_section
        .then(|| section_labels(provenance()))
        .transpose()?;
    let metadata_values = args
        .by_metadata
        .as_ref()
        .map(|key| Ok::<_, CliError>((key.clone(), document_values(provenance(), key)?)))
        .transpose()?;
    let run_corpus = lifecycle.run_corpus();
    let run_inventory = lifecycle.run_inventory();
    let run_id = random_id();
    scope.warn_before_run(
        scope_clause.as_ref(),
        &args.input.markdown.self_label_no_ops(),
        config,
        &run_id,
    )?;
    let pattern_request =
        (args.pattern_filter.is_some() || args.group_patterns_by.is_some()).then(|| {
            crate::protocol::query::PatternMetadataRequest {
                filter: args.pattern_filter.clone(),
                group_by: args.group_patterns_by.clone(),
                omitted_patterns: pattern_projection.omitted as u64,
            }
        });
    let run = QueryEnvelope::new(QueryEvent::Run(Box::new(QueryRun {
        pattern_metadata: pattern_request.clone(),
        matcher_semantics_identity: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
        run_id: run_id.clone(),
        mode: QueryMode::Summarize,
        catalog_fingerprint,
        definition_count: descriptors.len() as u64,
        analyzer: run_facts.analyzer,
        corpus: run_corpus,
        dictionary_identity: run_facts.dictionary_identity,
        reader_identity: run_facts.reader_identity,
        token_schema_identity: run_facts.token_schema_identity,
        // `summarize` publishes counts, never token evidence, so the honest
        // declaration is that this run publishes no token attributes.
        published_attributes: Vec::new(),
        published_source_metadata: metadata_projection
            .as_ref()
            .map_or_else(Vec::new, |projection| projection.keys().to_vec()),
        sample: run_facts
            .block_sample_report
            .or_else(|| sample.map(crate::protocol::query::SampleReport::of)),
        overlaps: crate::protocol::query::OverlapsReport::of(args.overlaps),
        word_vectors,
        ordering: MatchOrder::CorpusSentenceStartEndPatternCapturesGraphBindingsTerm,
        span_convention: SpanConvention::HalfOpenToken,
        // Grouped output carries no span, so there is nowhere to put an offset
        // and nothing for a consumer to look for.
        char_offsets: false,
        requested_context: 0,
        requested_limit: None,
        requested_top: top,
        execution_limits: protocol_execution_limits(config),
        temporary_storage_limits: TemporaryStorageLimits {
            max_temp_bytes: config.max_temp_bytes,
        },
        traversal_limits: run_facts.traversal_limits,
        inventory: run_inventory,
        worker_count: config.workers as u64,
    })));

    let post_run = (|| -> Result<CommandOutcome, CliError> {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        if output == OutputMode::Jsonl {
            publish_query_jsonl(&mut writer, &run)?;
            for descriptor in &descriptors {
                publish_query_jsonl(
                    &mut writer,
                    &QueryEnvelope::new(QueryEvent::Definition(DefinitionData {
                        name: descriptor.name.clone(),
                        catalog_ordinal: descriptor.catalog_ordinal,
                        body: match &descriptor.body {
                            crate::catalog_input::AuthoredDefinitionBody::Pattern {
                                expression,
                            } => DefinitionBody::Pattern {
                                expression: expression.clone(),
                            },
                            crate::catalog_input::AuthoredDefinitionBody::Lexicon {
                                by,
                                member_count,
                            } => DefinitionBody::Lexicon {
                                by: by.clone(),
                                member_count: *member_count,
                            },
                        },
                        metadata: descriptor.metadata.clone(),
                        source_file: descriptor.source_file.clone(),
                        source_span: SourceSpan {
                            start: descriptor.source_span.start,
                            end: descriptor.source_span.end,
                        },
                    })),
                )?;
            }
        }

        let pool = ThreadPoolBuilder::new()
            .num_threads(config.workers)
            .build()
            .map_err(|error| CliError::CommandFailure {
                code: FailureCode::Io,
                message: format!("failed to create worker pool: {error}"),
            })?;
        let limits = config.limits;
        let aggregation = SummaryAggregator::new(config.max_temp_bytes, config.workers.clamp(1, 8))
            .map_err(CliError::Aggregation)?;
        let sections = sections
            .map(|labels| SectionTally::new(labels, aggregation.temp_budget()))
            .transpose()
            .map_err(CliError::Aggregation)?;
        let metadata_values = metadata_values
            .map(|(key, documents)| {
                MetadataValueTally::new(
                    key,
                    documents
                        .into_iter()
                        .map(|(_, range, values)| (range, values)),
                    aggregation.temp_budget(),
                )
            })
            .transpose()
            .map_err(CliError::Aggregation)?;
        let grouping_coverage = args
            .grouping_coverage
            .then(|| {
                GroupingTally::new(
                    pattern_projection.selected.iter().copied(),
                    args.by_document,
                    aggregation.temp_budget(),
                )
            })
            .transpose()
            .map_err(CliError::Aggregation)?;
        let windows = if args.grouping_coverage
            || args.by_document
            || args.write_table.is_some()
            || args.trace_policy.is_some()
            || sections.is_some()
            || metadata_values.is_some()
        {
            None
        } else if let Some(windows) = compiled.matcher.token_windows() {
            WindowTally::new(&windows, &pattern_ordinals, aggregation.temp_budget())
                .map_err(CliError::Aggregation)?
        } else {
            None
        };
        let mut sink = SummarySink {
            grouping_coverage,
            windows,
            aggregation: Some(aggregation),
            aggregation_error: None,
            pattern_ordinals: &pattern_ordinals,
            selected_patterns: (args.pattern_filter.is_some() || args.group_patterns_by.is_some())
                .then_some(&pattern_projection.selected),
            policy: compiled.matcher.catalog_policy(),
            records_unkeyed: 0,
            documents: (args.by_document || args.write_table.is_some()).then(BTreeMap::new),
            sentences: args.write_table.is_some(),
            sections,
            metadata_values,
        };
        let presentation_progress = PresentationProgress::stderr(
            &config.stderr,
            config.progress,
            output,
            ResultDelivery::Deferred,
        );
        let progress_writer = presentation_progress.clone();
        let mut progress = move |progress: lingrep_core::execution::ExecutionProgress,
                                 _sink: &SummarySink<'_>| {
            let (sentences, matches) = (progress.sentences_matched, progress.matches_found);
            progress_writer.snapshot(move || {
                format!("matched {sentences} sentences, found {matches} matches")
            });
        };
        let mut max_tokenized_bytes = 0usize;
        let mut observer = |observation: lingrep_core::execution::TokenizedBatchObservation| {
            max_tokenized_bytes = max_tokenized_bytes.max(observation.charged_bytes);
            Ok(())
        };
        // Analysis lanes, not one session: tokenization is the single-threaded
        // stage an uncached run is paced by, and the pool matching already
        // uses is idle while it runs. A prepared source analyzes nothing and
        // builds none of this.
        let binding = analysis.binding(config.workers, &pool);
        let outcome = execute_analyzed(
            lingrep_core::execution::SampledSentences::new(
                source,
                sample.filter(|_| !args.sample.blocks),
            ),
            binding,
            &bound,
            &pool,
            &limits,
            config.limits.max_stage_sentences,
            terminal.cancellation(),
            &mut sink,
            &mut progress,
            &mut observer,
            // `summarize` has no `--raw-matches`: a summary of the raw stream
            // would count records the catalog does not publish.
            match args.trace_policy {
                Some(capacity) if capacity > 0 => {
                    Publication::from(PolicySelection::Apply).with_trace(capacity)
                }
                _ => Publication::from(PolicySelection::Apply),
            },
            scope
                .prepared_scope
                .as_ref()
                .map_or(&SentenceAdmission::All, |resolved| &resolved.admission),
        );
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(error) = sink.aggregation_error.take() {
                    return Err(CliError::Aggregation(error));
                }
                return Err(error.into());
            }
        };
        let report = match outcome {
            RunOutcome::Completed(report) => report,
            RunOutcome::Cancelled(_) => return Ok(CommandOutcome::Cancelled),
        };
        presentation_progress.finish();
        crate::attribute_binding::write_out_of_set_label_notices(
            compiled.matcher.label_declarations(),
            &report.out_of_set_labels,
            config,
            &run_id,
        )?;
        if let Some(observed) = source_stats.observed_division_kinds() {
            crate::attribute_binding::write_unobserved_division_notices(
                &compiled.matcher.lookaround_division_kinds(),
                |kind| observed.contains(kind),
                config,
                Some(&run_id),
            )?;
        }
        if args.trace_policy.is_some() {
            crate::policy_report::write_requested(
                &config.stderr,
                compiled.matcher.catalog_policy(),
                report.policy_stats.captures_dropped,
                report.program_stats.per_operation(),
                report.trace.as_ref(),
            )?;
        }
        let aggregation = sink
            .aggregation
            .take()
            .expect("summary aggregation remains available after successful execution");
        let grouping_coverage = sink.grouping_coverage.take();
        let windows = sink.windows.take();
        let mut records_unkeyed = sink.records_unkeyed;
        let documents = sink.documents.take();
        let sections = sink.sections.take();
        let metadata_values = sink.metadata_values.take();
        drop(sink);

        let (complete_corpus, inventory) = lifecycle.complete(FilesystemCompletion::Full)?;
        let pattern_names = descriptors
            .iter()
            .map(|descriptor| catalog_policy.published_name(descriptor.name.as_str()))
            .collect::<Vec<_>>();
        let mut grouping_coverage = grouping_coverage
            .map(GroupingTally::into_records)
            .transpose()
            .map_err(CliError::Aggregation)?;
        let complete_grouping_coverage = grouping_coverage
            .as_mut()
            .map(|coverage| coverage.take_partition(None, &pattern_names))
            .transpose()
            .map_err(CliError::Aggregation)?;
        let mut finalized_windows = windows
            .map(|windows| windows.finalize(&pool))
            .transpose()
            .map_err(CliError::Aggregation)?;
        if let Some(windows) = &finalized_windows {
            records_unkeyed = windows.records_unkeyed();
        }
        let (mut finalized, mut aggregation) = if args.write_table.is_some() {
            (None, Some(aggregation))
        } else if finalized_windows.is_some() {
            (None, None)
        } else {
            (
                Some(aggregation.finalize().map_err(CliError::Aggregation)?),
                None,
            )
        };
        let published = pattern_projection
            .selected
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let mut sections = sections
            .map(|sections| sections.into_records(&published, &pattern_names))
            .transpose()
            .map_err(CliError::Aggregation)?;
        let mut metadata_values = metadata_values
            .map(|values| values.into_records(&published, &pattern_names))
            .transpose()
            .map_err(CliError::Aggregation)?;
        let mut temp_high_water = match (&finalized, &finalized_windows) {
            (Some(finalized), _) => finalized.peak_temp_bytes(),
            (None, Some(windows)) => windows.peak_temp_bytes(),
            (None, None) => 0,
        };

        let mut documents = documents;
        let mut staged_table = None;
        if let (Some(path), Some(aggregation)) = (args.write_table.as_deref(), aggregation.take()) {
            let crate::protocol::query::CompleteCorpus::Prepared {
                analysis_identity, ..
            } = &complete_corpus
            else {
                return Err(CliError::InvalidArgs(
                    "--write-table requires a prepared corpus".to_owned(),
                ));
            };
            let key = lingrep_corpus::CountTableKey {
                analysis_identity: analysis_identity.clone(),
                catalog_fingerprint: table_fingerprint,
                matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
            };
            match write_count_table(
                path,
                key,
                aggregation,
                documents.take().unwrap_or_default(),
                &pattern_ordinals,
                &pattern_names,
                terminal.cancellation(),
            ) {
                Ok(staged) => staged_table = Some(staged),
                Err(_) if terminal.cancellation().is_cancelled() => {
                    return Ok(CommandOutcome::Cancelled);
                }
                Err(error) => return Err(error),
            }
        }

        if !terminal.try_claim_completion() {
            return Ok(CommandOutcome::Cancelled);
        }
        // Before publication, as in `matches`: a degraded run does not publish
        // a successful completion. A summary is a total, and a total missing a
        // sentence nobody was told about is worse than no total.
        if let Some(summary) = source_stats.oversized_sentence_omissions()? {
            return Err(CliError::OversizedSentencesSkipped {
                count: summary.count,
                first_source_name: summary.first_source_name,
                first_ordinal: summary.first_ordinal,
                first_actual: summary.first_actual,
                limit: summary.limit,
            });
        }
        if let Some(inventory) = inventory
            .as_ref()
            .filter(|stats| stats.alias_documents != 0)
        {
            write_alias_warning(config, &run_id, inventory)?;
        }
        let table_stats = match staged_table {
            Some((staged, peak)) => {
                temp_high_water = peak;
                Some(staged.publish().map_err(|error| match &args.write_table {
                    Some(path) => CliError::from(error).at_path(path),
                    None => CliError::from(error),
                })?)
            }
            None => None,
        };

        // `summarize` publishes no per-match prefix, so it always runs to
        // completion: its basis is the corpus on both paths.
        let scope_report = scope.report_after_run(scope_clause.as_ref(), false, config, &run_id)?;
        let sentences_proven_absent = run_facts.filter_stats.as_ref().map_or(
            0,
            lingrep_corpus::PreparedFilterStats::sentences_proven_absent,
        );
        let complete = QueryComplete {
            grouping_coverage: complete_grouping_coverage,
            retained_matches: None,
            coverage: Coverage::Full,
            corpus: complete_corpus,
            counters: RunCounters {
                documents_admitted: source_stats.documents_admitted(),
                sentences_admitted: report
                    .sentences_admitted
                    .checked_add(sentences_proven_absent)
                    .ok_or_else(|| {
                        CliError::Corpus("sentence admission counter overflow".into())
                    })?,
                sentences_proven_absent,
                sentences_matched: report.sentences_matched,
                tokens_scanned: report.tokens_scanned,
                matches_found: report.matches_found,
                records_emitted: 0,
                records_unkeyed,
                captures_dropped: report.policy_stats.captures_dropped,
                offsets_omitted: 0,
            },
            elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            resources: ResourceHighWater {
                staged_raw_bytes: source_stats
                    .max_staged_raw_bytes()
                    .try_into()
                    .unwrap_or(u64::MAX),
                tokenized_bytes: max_tokenized_bytes.try_into().unwrap_or(u64::MAX),
                temporary_storage_bytes: temp_high_water,
                execution_limits: protocol_execution_limits(config),
                temporary_storage_limits: TemporaryStorageLimits {
                    max_temp_bytes: config.max_temp_bytes,
                },
            },
            committed_prefix: None,
            inventory,
            termination: Termination::Eof,
            scope: scope_report.clone(),
        };
        for (ordinal, tally) in documents.into_iter().flatten() {
            publish_query_jsonl(
                &mut writer,
                &QueryEnvelope::new(QueryEvent::Document(DocumentData {
                    grouping_coverage: grouping_coverage
                        .as_mut()
                        .map(|coverage| coverage.take_partition(Some(ordinal), &pattern_names))
                        .transpose()
                        .map_err(CliError::Aggregation)?,
                    document: ordinal,
                    source_name: tally.source_name.to_string(),
                    sentences_admitted: tally.sentences,
                    tokens_admitted: tally.tokens,
                    source_metadata: tally.source_metadata,
                })),
            )?;
        }
        if let Some(sections) = sections.as_mut() {
            while let Some(mut section) = sections.next_record().map_err(CliError::Aggregation)? {
                if args.pattern_filter.is_some() || args.group_patterns_by.is_some() {
                    let mut groups = Vec::new();
                    if args.group_patterns_by.is_some() {
                        let by_name = section
                            .patterns
                            .iter()
                            .map(|pattern| (pattern.pattern.as_str(), pattern))
                            .collect::<BTreeMap<_, _>>();
                        for row in &pattern_projection.rows {
                            let names = row
                                .members
                                .iter()
                                .map(|ordinal| pattern_names[*ordinal as usize])
                                .collect::<BTreeSet<_>>();
                            let members = names
                                .iter()
                                .filter_map(|name| by_name.get(name))
                                .collect::<Vec<_>>();
                            groups.push(crate::protocol::query::SectionPatternGroup {
                                value: row.name.clone(),
                                patterns: names.into_iter().map(str::to_owned).collect(),
                                matches: members.iter().map(|pattern| pattern.matches).sum(),
                                pattern_sentences: members
                                    .iter()
                                    .map(|pattern| pattern.sentences_matched)
                                    .sum(),
                            });
                        }
                        groups.sort_by(|left, right| {
                            right
                                .pattern_sentences
                                .cmp(&left.pattern_sentences)
                                .then_with(|| left.value.cmp(&right.value))
                        });
                        section.patterns.clear();
                    }
                    section.pattern_metadata =
                        Some(crate::protocol::query::SectionPatternMetadata {
                            selection: pattern_request
                                .clone()
                                .expect("requested metadata selection"),
                            groups,
                        });
                }
                publish_query_jsonl(
                    &mut writer,
                    &QueryEnvelope::new(QueryEvent::Section(section)),
                )?;
            }
        }
        if let Some(values) = metadata_values.as_mut() {
            while let Some(value) = values.next_record().map_err(CliError::Aggregation)? {
                publish_query_jsonl(
                    &mut writer,
                    &QueryEnvelope::new(QueryEvent::MetadataValue(value)),
                )?;
            }
        }
        let mut event_writer = MachineSummaryEventWriter {
            writer: &mut writer,
            output,
            pool: &pool,
        };
        if let Some(finalized) = finalized.as_mut() {
            publish_summary_rows_and_complete(
                finalized,
                top,
                &pattern_names,
                complete,
                &mut event_writer,
            )?;
        } else if let Some(windows) = finalized_windows.as_mut() {
            publish_summary_rows_and_complete(
                windows,
                top,
                &pattern_names,
                complete,
                &mut event_writer,
            )?;
        } else {
            if let (Some(stats), Some(path), OutputMode::Human) =
                (table_stats, args.write_table.as_deref(), output)
            {
                writeln!(
                    event_writer.writer,
                    "wrote count table {}: {} entries, {} document pairs, {} bytes",
                    sanitize_terminal(&path.display().to_string()),
                    stats.entries,
                    stats.pairs,
                    stats.bytes
                )
                .map_err(execution_output_io)?;
            }
            event_writer.write_complete(&complete)?;
        }
        writer.flush().map_err(execution_output_io)?;
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

struct SummarySink<'a> {
    grouping_coverage: Option<GroupingTally>,
    windows: Option<WindowTally>,
    aggregation: Option<SummaryAggregator>,
    aggregation_error: Option<io::Error>,
    pattern_ordinals: &'a BTreeMap<String, u64>,
    selected_patterns: Option<&'a BTreeSet<u64>>,
    /// Consulted per record to decide what it groups by.
    policy: &'a CatalogPolicy,
    /// Records whose pattern declares a `key:` that this match could not
    /// satisfy — an alternation branch that skipped the capture.
    ///
    /// Counted rather than folded in under a partial key, which would collide
    /// with some other pattern's complete one, and counted rather than dropped
    /// silently, because a row that vanished without a number is
    /// indistinguishable from one that never matched.
    records_unkeyed: u64,
    /// Per-document admission under `--by-document`, keyed by document ordinal.
    documents: Option<BTreeMap<u64, DocumentTally>>,
    sentences: bool,
    sections: Option<SectionTally>,
    metadata_values: Option<MetadataValueTally>,
}

fn section_labels(
    provenance: &lingrep_corpus::BlockingProvenance,
) -> Result<Vec<(String, RoaringTreemap)>, CliError> {
    let (coverage, spans) = provenance.block_on(async {
        let view = provenance.view();
        let coverage = view
            .section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
            .await?;
        let names = coverage
            .iter()
            .map(|entry| entry.label.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let spans = view.region_spans_for(&names).await?;
        Ok::<_, lingrep_corpus::CorpusError>((coverage, spans))
    })?;
    if coverage.is_empty() {
        return Err(CliError::InvalidArgs(
            "--by-section requires section labels, but this artifact records none".to_owned(),
        ));
    }
    let mut labels = coverage
        .into_iter()
        .map(|entry| (entry.label, RoaringTreemap::new()))
        .collect::<Vec<_>>();
    let position = labels
        .iter()
        .enumerate()
        .map(|(index, (label, _))| (label.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    for span in spans {
        if let Some(index) = position.get(&span.label) {
            labels[*index]
                .1
                .insert_range(span.first_sentence..span.end_sentence);
        }
    }
    Ok(labels)
}

struct DocumentTally {
    document_id: u64,
    source_name: std::sync::Arc<str>,
    sentences: u64,
    tokens: u64,
    source_metadata: Option<std::sync::Arc<serde_json::Map<String, serde_json::Value>>>,
}

fn write_count_table(
    path: &std::path::Path,
    key: lingrep_corpus::CountTableKey,
    aggregation: SummaryAggregator,
    documents: BTreeMap<u64, DocumentTally>,
    pattern_ordinals: &BTreeMap<String, u64>,
    pattern_names: &[&str],
    cancellation: &lingrep_core::execution::CancellationFlag,
) -> Result<(lingrep_corpus::StagedCountTable, u64), CliError> {
    let table_error = |error| CliError::from(error).at_path(path);
    let mut ordinals = pattern_ordinals.values().copied().collect::<Vec<_>>();
    ordinals.sort_unstable();
    let names = ordinals
        .iter()
        .map(|ordinal| {
            pattern_names
                .get(*ordinal as usize)
                .map(|name| (*name).to_owned())
                .ok_or_else(|| CliError::Corpus("pattern ordinal is out of range".to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let totals = documents
        .values()
        .map(|tally| lingrep_corpus::DocumentTotals {
            document_id: tally.document_id,
            sentences: tally.sentences,
            tokens: tally.tokens,
        })
        .collect::<Vec<_>>();
    let document_index = documents
        .keys()
        .zip(0u32..)
        .map(|(ordinal, index)| (*ordinal, index))
        .collect::<std::collections::HashMap<_, _>>();
    let mut finalized = aggregation
        .finalize_by_group(cancellation)
        .map_err(CliError::Aggregation)?;
    let mut writer = lingrep_corpus::CountTableWriter::create(
        path,
        key,
        names,
        finalized.schemas(),
        finalized.values(),
        &totals,
    )
    .map_err(table_error)?;
    let mut failure = None;
    let mut events = 0u64;
    let visited = finalized.visit(|event| {
        events += 1;
        if events.is_multiple_of(1 << 16) && cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "count table writing cancelled",
            ));
        }
        let written = match event {
            crate::summary::GroupEvent::Entry {
                pattern_ordinal,
                schema,
                words,
            } => match ordinals.binary_search(&pattern_ordinal) {
                Ok(pattern) => writer.begin_entry(pattern, schema, words),
                Err(_) => {
                    return Err(io::Error::other("count for an unpublished pattern"));
                }
            },
            crate::summary::GroupEvent::Sentence {
                document_ordinal,
                sentence_ordinal,
                count,
            } => match document_index.get(&document_ordinal) {
                Some(document) => writer.add_sentence(*document, sentence_ordinal, count),
                None => {
                    return Err(io::Error::other(
                        "count for a document with no admitted sentence",
                    ));
                }
            },
        };
        written.map_err(|error| {
            let message = error.to_string();
            failure = Some(error);
            io::Error::other(message)
        })
    });
    if let Some(error) = failure {
        return Err(table_error(error));
    }
    visited.map_err(CliError::Aggregation)?;
    let peak = finalized.peak_temp_bytes();
    let staged = writer.finish(cancellation).map_err(table_error)?;
    Ok((staged, peak))
}

/// Renders one typed group in the human and TSV surfaces.
///
/// Scalar surface and unit groups retain their bare value. A projected key is
/// a compact JSON object whose entries preserve lingrep patterns source order, retain each
/// canonical field name, and remain one TSV cell regardless of key width.
fn aggregation_group_display(group: &AggregationGroup) -> String {
    match group {
        AggregationGroup::Surface(value)
        | AggregationGroup::Unit(value)
        | AggregationGroup::Term(value) => value.clone(),
        AggregationGroup::Key(fields) => serde_json::to_string(&CompactKeyGroup(fields))
            .expect("typed key fields serialize as an ordered object"),
        AggregationGroup::LexicalKey(fields) => {
            serde_json::to_string(&CompactLexicalKeyGroup(fields))
                .expect("typed lexical key fields serialize as an ordered object")
        }
    }
}

fn summary_data(pattern: &str, row: &SummaryRow) -> SummaryData {
    SummaryData {
        pattern: pattern.to_owned(),
        document: row.partition.document_ordinal,
        group: match &row.group {
            AggregationGroup::Surface(value) => SummaryGroupData::Surface {
                value: value.clone(),
            },
            AggregationGroup::Unit(value) => SummaryGroupData::Unit {
                value: value.clone(),
            },
            AggregationGroup::Term(value) => SummaryGroupData::Term {
                value: value.clone(),
            },
            AggregationGroup::Key(fields) => SummaryGroupData::Key {
                fields: fields
                    .iter()
                    .map(|field| ProtocolKeyField::Scalar {
                        name: field.name.to_string(),
                        value: field.value.clone(),
                    })
                    .collect(),
            },
            AggregationGroup::LexicalKey(fields) => SummaryGroupData::Key {
                fields: fields
                    .iter()
                    .map(|field| match &field.value {
                        lingrep_core::fsm::LexicalProjectedKeyValue::Scalar(value) => {
                            ProtocolKeyField::Scalar {
                                name: field.name.to_string(),
                                value: value.clone(),
                            }
                        }
                        lingrep_core::fsm::LexicalProjectedKeyValue::Segments(values) => {
                            ProtocolKeyField::List {
                                name: field.name.to_string(),
                                values: values.clone(),
                            }
                        }
                    })
                    .collect(),
            },
        },
        count: row.count,
        rank: row.rank,
    }
}

/// The TSV `group` cell is JSON, but does not repeat query/v2's type labels for
/// every field. `SerializeMap` writes entries in this iterator's source order;
/// duplicate canonical names are already a compile error.
struct CompactKeyGroup<'a>(&'a [lingrep_core::fsm::ProjectedKeyField]);

struct CompactLexicalKeyGroup<'a>(&'a [lingrep_core::fsm::LexicalProjectedKeyField]);

impl Serialize for CompactKeyGroup<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for field in self.0 {
            map.serialize_entry(&field.name, &field.value)?;
        }
        map.end()
    }
}

impl Serialize for CompactLexicalKeyGroup<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for field in self.0 {
            match &field.value {
                lingrep_core::fsm::LexicalProjectedKeyValue::Scalar(value) => {
                    map.serialize_entry(&field.name, value)?
                }
                lingrep_core::fsm::LexicalProjectedKeyValue::Segments(values) => {
                    map.serialize_entry(&field.name, values)?
                }
            }
        }
        map.end()
    }
}

impl SummarySink<'_> {
    fn partition(
        &self,
        pattern_ordinal: u64,
        location: &lingrep_core::execution::SentenceLocation,
    ) -> SummaryPartition {
        SummaryPartition {
            pattern_ordinal,
            document_ordinal: self
                .documents
                .is_some()
                .then_some(location.document_ordinal),
            sentence_ordinal: self.sentences.then_some(location.corpus_sentence_ordinal),
        }
    }

    /// Counts one admitted sentence toward its document under `--by-document`.
    fn observe_sentence(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        tokens: usize,
    ) -> Result<(), ExecutionError> {
        let Some(documents) = self.documents.as_mut() else {
            return Ok(());
        };
        let tally = documents
            .entry(location.document_ordinal)
            .or_insert_with(|| DocumentTally {
                document_id: location.document_id.get(),
                source_name: std::sync::Arc::clone(&location.source_name),
                sentences: 0,
                tokens: 0,
                source_metadata: location.source_metadata.clone(),
            });
        tally.sentences = tally
            .sentences
            .checked_add(1)
            .ok_or(ExecutionError::ResourceAccounting)?;
        tally.tokens = tally
            .tokens
            .checked_add(tokens as u64)
            .ok_or(ExecutionError::ResourceAccounting)?;
        Ok(())
    }

    fn grouping<'r>(
        &self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        record: RecordView<'r>,
    ) -> Result<Option<AggregationInput<'r>>, ExecutionError> {
        Ok(match record.group_by(self.policy) {
            None => None,
            Some(GroupBy::Key) => record.key.map(AggregationInput::Key),
            Some(GroupBy::LexicalKey) => record.lexical_key.map(AggregationInput::LexicalKey),
            Some(GroupBy::Unit) => record.unit.map(AggregationInput::Unit),
            Some(GroupBy::Term) => record
                .lexical
                .map(|lexical| AggregationInput::Term(lexical.term.as_str())),
            Some(GroupBy::Surface) => Some(AggregationInput::Surface(matched_surface(
                sentence,
                record.start..record.end,
                location,
            )?)),
        })
    }

    fn grouping_owned(
        &self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        mut record: PublishedRecord,
    ) -> Result<Option<OwnedAggregationInput>, ExecutionError> {
        let group_by = RecordView::published(&record).group_by(self.policy);
        Ok(match group_by {
            None => None,
            Some(GroupBy::Key) => record.key.take().map(OwnedAggregationInput::Key),
            Some(GroupBy::LexicalKey) => record
                .lexical_key
                .take()
                .map(OwnedAggregationInput::LexicalKey),
            Some(GroupBy::Unit) => record.unit.take().map(OwnedAggregationInput::Unit),
            Some(GroupBy::Term) => record
                .raw
                .lexical
                .take()
                .map(|lexical| OwnedAggregationInput::Term(lexical.term)),
            Some(GroupBy::Surface) => Some(OwnedAggregationInput::Surface(matched_surface(
                sentence,
                record.start..record.end,
                location,
            )?)),
        })
    }

    /// Lowers one sentence's records to routed groups: pattern ordinal, the
    /// owned group, and its shard-routing hash, all computed off the scan
    /// thread so the serial reduction only appends and dispatches.
    fn group_owned_records(
        &self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<Vec<(SummaryPartition, Option<(OwnedAggregationInput, u64)>)>, ExecutionError> {
        records
            .into_iter()
            .filter(|record| {
                self.pattern_ordinals
                    .get(record.name.as_str())
                    .is_none_or(|ordinal| {
                        self.selected_patterns
                            .is_none_or(|selected| selected.contains(ordinal))
                    })
            })
            .map(|record| {
                let pattern_ordinal = self
                    .pattern_ordinals
                    .get(record.name.as_str())
                    .copied()
                    .ok_or_else(|| ExecutionError::Sink("unknown summary pattern".into()))?;
                let partition = self.partition(pattern_ordinal, location);
                let grouped = self
                    .grouping_owned(location, sentence, record)?
                    .map(|group| {
                        let hash = shard_hash(partition, &group);
                        (group, hash)
                    });
                Ok((partition, grouped))
            })
            .collect()
    }

    fn observe_slots(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        tokens: usize,
        groups: &[(SummaryPartition, Option<(OwnedAggregationInput, u64)>)],
    ) -> Result<(), ExecutionError> {
        self.observe_slot_patterns(
            location,
            tokens,
            groups
                .iter()
                .map(|(partition, _)| partition.pattern_ordinal),
        )
    }

    fn observe_slot_patterns(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        tokens: usize,
        patterns: impl Iterator<Item = u64>,
    ) -> Result<(), ExecutionError> {
        let observed = match (self.sections.as_mut(), self.metadata_values.as_mut()) {
            (None, None) => return Ok(()),
            (Some(sections), _) => sections.observe(
                location.corpus_sentence_ordinal,
                location.document_ordinal,
                tokens,
                patterns,
            ),
            (None, Some(values)) => {
                values.observe(location.corpus_sentence_ordinal, tokens, patterns)
            }
        };
        if let Err(error) = observed {
            let message = error.to_string();
            self.aggregation_error = Some(error);
            return Err(ExecutionError::Sink(message));
        }
        Ok(())
    }

    fn observe_grouping(
        &mut self,
        partition: SummaryPartition,
        grouped: bool,
    ) -> Result<(), ExecutionError> {
        if let Some(coverage) = self.grouping_coverage.as_mut()
            && let Err(error) = coverage.observe(partition, grouped)
        {
            let message = error.to_string();
            self.aggregation_error = Some(error);
            return Err(ExecutionError::Sink(message));
        }
        Ok(())
    }

    fn add_owned_groups(
        &mut self,
        groups: Vec<(SummaryPartition, Option<(OwnedAggregationInput, u64)>)>,
    ) -> Result<usize, ExecutionError> {
        let records_emitted = groups.len();
        for (partition, grouped) in groups {
            self.observe_grouping(partition, grouped.is_some())?;
            let Some((grouped, hash)) = grouped else {
                self.records_unkeyed += 1;
                continue;
            };
            if let Err(error) = self
                .aggregation
                .as_mut()
                .expect("aggregation exists during execution")
                .add_owned_prehashed(partition, grouped, hash)
            {
                let message = error.to_string();
                self.aggregation_error = Some(error);
                return Err(ExecutionError::Sink(message));
            }
        }
        Ok(records_emitted)
    }

    fn aggregate<'r>(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: impl Iterator<Item = RecordView<'r>>,
    ) -> Result<SinkControl, ExecutionError> {
        self.observe_sentence(location, sentence.len())?;
        let mut records_emitted = 0usize;
        let mut matched = Vec::new();
        for record in records {
            let pattern_ordinal = self
                .pattern_ordinals
                .get(record.pattern)
                .copied()
                .ok_or_else(|| ExecutionError::Sink("unknown summary pattern".into()))?;
            if self
                .selected_patterns
                .is_some_and(|selected| !selected.contains(&pattern_ordinal))
            {
                continue;
            }
            records_emitted += 1;
            if self.sections.is_some() || self.metadata_values.is_some() {
                matched.push(pattern_ordinal);
            }
            let partition = self.partition(pattern_ordinal, location);
            let grouped = self.grouping(location, sentence, record)?;
            self.observe_grouping(partition, grouped.is_some())?;
            let Some(grouped) = grouped else {
                self.records_unkeyed += 1;
                continue;
            };
            if let Err(error) = self
                .aggregation
                .as_mut()
                .expect("aggregation exists during execution")
                .add(partition, grouped)
            {
                let message = error.to_string();
                self.aggregation_error = Some(error);
                return Err(ExecutionError::Sink(message));
            }
        }
        self.observe_slot_patterns(location, sentence.len(), matched.into_iter())?;
        // Every match is folded into the aggregation; nothing is skipped.
        Ok(SinkControl::Continue { records_emitted })
    }
}

impl MatchSink for SummarySink<'_> {
    fn wants_parallel_records_partition(&self) -> bool {
        true
    }

    fn counts_token_windows(&self) -> bool {
        self.windows.is_some()
    }

    fn push_windows<V: lingrep_core::token_match::TokenSequence + Sync>(
        &mut self,
        pool: &rayon::ThreadPool,
        _locations: &[lingrep_core::execution::SentenceLocation],
        sentences: &[V],
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        let windows = self
            .windows
            .as_mut()
            .ok_or_else(|| ExecutionError::Sink("summary does not count token windows".into()))?;
        let mut controls = Vec::with_capacity(sentences.len());
        for sentence in sentences {
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
            match windows.push(pool, sentence) {
                Ok(records_emitted) => controls.push(SinkControl::Continue { records_emitted }),
                Err(error) => {
                    let message = error.to_string();
                    self.aggregation_error = Some(error);
                    return Err(ExecutionError::Sink(message));
                }
            }
        }
        Ok((controls, false))
    }

    fn push(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.aggregate(location, sentence, matches.iter().map(RecordView::raw))
    }

    fn push_records(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        self.observe_sentence(location, sentence.len())?;
        let groups = self.group_owned_records(location, sentence, records)?;
        self.observe_slots(location, sentence.len(), &groups)?;
        let records_emitted = self.add_owned_groups(groups)?;
        Ok(SinkControl::Continue { records_emitted })
    }

    fn push_records_partition<V: lingrep_core::token_match::TokenSequence + Sync>(
        &mut self,
        pool: &rayon::ThreadPool,
        locations: &[lingrep_core::execution::SentenceLocation],
        sentences: &[V],
        records: Vec<Vec<PublishedRecord>>,
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        // Bounded fan-out: at most PARTITION_PUBLICATION_WIDTH concurrent
        // tasks, however wide the shared pool is.
        let chunk = locations
            .len()
            .div_ceil(lingrep_core::execution::PARTITION_PUBLICATION_WIDTH)
            .max(1);
        let grouped = pool.install(|| {
            locations
                .par_chunks(chunk)
                .zip(sentences.par_chunks(chunk))
                .zip(records.into_par_iter().chunks(chunk))
                .map(|((locations, sentences), records)| {
                    locations
                        .iter()
                        .zip(sentences)
                        .zip(records)
                        .map(|((location, sentence), records)| {
                            self.group_owned_records(location, sentence, records)
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });

        // Parallel preparation is speculative. Aggregation, errors, and
        // cancellation remain observable only in corpus order.
        let mut controls = Vec::with_capacity(locations.len());
        for ((groups, location), sentence) in
            grouped.into_iter().flatten().zip(locations).zip(sentences)
        {
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
            self.observe_sentence(location, sentence.len())?;
            let groups = groups?;
            self.observe_slots(location, sentence.len(), &groups)?;
            let records_emitted = self.add_owned_groups(groups)?;
            controls.push(SinkControl::Continue { records_emitted });
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
        }
        Ok((controls, false))
    }
}

fn write_alias_warning(
    config: &ExecutionCliConfig,
    run_id: &str,
    inventory: &InventoryStats,
) -> Result<(), CliError> {
    let message = format!(
        "aliases are counted as separate logical documents: alias_documents={}, \
         max_target_multiplicity={}, documents={}, distinct_targets={}",
        inventory.alias_documents,
        inventory.max_target_multiplicity,
        inventory.documents,
        inventory.distinct_targets
    );
    write_warning(
        &config.stderr,
        config.diagnostic_format,
        DiagnosticStage::Aggregation,
        "aliased_source_documents",
        message,
        Some(run_id),
        Vec::new(),
    )
    .map_err(CliError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Termination as ProcessTermination, exit_code};
    use crate::protocol::query::CompleteCorpus;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Attempt {
        Summary(SummaryData),
        // Boxed only for size: `QueryComplete` carries every run counter and
        // resource high-water mark, so it dwarfs a summary row.
        Complete(Box<QueryComplete>),
    }

    #[derive(Clone, Copy)]
    enum FailAt {
        Summary(usize),
        Complete,
    }

    struct RecordingFailureWriter {
        fail_at: FailAt,
        summary_attempts: usize,
        attempted: Vec<Attempt>,
        committed: Vec<Attempt>,
    }

    impl RecordingFailureWriter {
        fn new(fail_at: FailAt) -> Self {
            Self {
                fail_at,
                summary_attempts: 0,
                attempted: Vec::new(),
                committed: Vec::new(),
            }
        }
    }

    impl SummaryEventWriter for RecordingFailureWriter {
        fn write_summary(&mut self, pattern: &str, row: &SummaryRow) -> Result<(), ExecutionError> {
            self.summary_attempts += 1;
            let attempt = Attempt::Summary(summary_data(pattern, row));
            self.attempted.push(attempt.clone());
            if matches!(self.fail_at, FailAt::Summary(index) if index == self.summary_attempts) {
                return Err(broken_pipe());
            }
            self.committed.push(attempt);
            Ok(())
        }

        fn write_complete(&mut self, complete: &QueryComplete) -> Result<(), ExecutionError> {
            let attempt = Attempt::Complete(Box::new(complete.clone()));
            self.attempted.push(attempt.clone());
            if matches!(self.fail_at, FailAt::Complete) {
                return Err(broken_pipe());
            }
            self.committed.push(attempt);
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_on_a_summary_row_stops_without_later_rows_or_complete() {
        let mut finalized = finalized_fixture();
        let mut writer = RecordingFailureWriter::new(FailAt::Summary(2));

        let error = publish_summary_rows_and_complete(
            &mut finalized,
            None,
            &["authored"],
            complete_fixture(),
            &mut writer,
        )
        .unwrap_err();

        assert_eq!(error.code(), "broken_pipe");
        assert_eq!(error.termination(), ProcessTermination::Failure);
        assert_eq!(exit_code(error.termination()), 1);
        assert_eq!(writer.attempted.len(), 2);
        assert!(matches!(writer.attempted[0], Attempt::Summary(_)));
        assert!(matches!(writer.attempted[1], Attempt::Summary(_)));
        assert_eq!(writer.committed, writer.attempted[..1]);
        assert!(
            writer
                .attempted
                .iter()
                .all(|attempt| !matches!(attempt, Attempt::Complete(_)))
        );
    }

    #[test]
    fn broken_pipe_on_complete_keeps_all_rows_and_no_complete() {
        let mut finalized = finalized_fixture();
        let mut writer = RecordingFailureWriter::new(FailAt::Complete);

        let error = publish_summary_rows_and_complete(
            &mut finalized,
            None,
            &["authored"],
            complete_fixture(),
            &mut writer,
        )
        .unwrap_err();

        assert_eq!(error.code(), "broken_pipe");
        assert_eq!(error.termination(), ProcessTermination::Failure);
        assert_eq!(exit_code(error.termination()), 1);
        assert_eq!(writer.attempted.len(), 4);
        assert!(matches!(writer.attempted[3], Attempt::Complete(_)));
        assert_eq!(writer.committed, writer.attempted[..3]);
        assert!(
            writer
                .committed
                .iter()
                .all(|attempt| !matches!(attempt, Attempt::Complete(_)))
        );
    }

    fn finalized_fixture() -> crate::summary::FinalizedSummary {
        let mut aggregation = SummaryAggregator::with_limits(1, 2, u64::MAX, 2).unwrap();
        for surface in ["b", "b", "a", "c"] {
            aggregation
                .add(
                    crate::summary::SummaryPartition::pattern(0),
                    AggregationInput::Surface(surface.to_owned()),
                )
                .unwrap();
        }
        aggregation.finalize().unwrap()
    }

    fn complete_fixture() -> QueryComplete {
        QueryComplete {
            grouping_coverage: None,
            retained_matches: None,
            coverage: Coverage::Full,
            corpus: CompleteCorpus::Prepared {
                source_identity: "source".into(),
                analysis_identity: "analysis".into(),
            },
            counters: RunCounters::default(),
            elapsed_millis: 0,
            resources: ResourceHighWater::default(),
            committed_prefix: None,
            inventory: None,
            termination: Termination::Eof,
            scope: None,
        }
    }

    fn broken_pipe() -> ExecutionError {
        ExecutionError::Output {
            kind: io::ErrorKind::BrokenPipe,
            message: "injected broken pipe".into(),
        }
    }
}
