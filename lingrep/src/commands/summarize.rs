//! Exact bounded `summarize` orchestration and deferred publication.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::Instant;

use lingrep_core::execution::{
    CancellationFlag, ExecutionError, MatchSink, PolicySelection, Publication, RunOutcome,
    SentenceAdmission, SinkControl, cached_run_attributes, execute_analyzed,
};
use lingrep_core::fsm::{CatalogPolicy, Match, PublishedRecord, RecordView};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use serde::Serialize;
use serde::ser::SerializeMap;

use super::CommandOutcome;
use super::query_completion::FilesystemCompletion;
use super::query_resolution::{QueryScope, ResolvedMatchingQuery, resolve_matching_query};
use super::query_scope::{build_scope_report, parse_scope};
use super::query_support::{
    execution_output_error, execution_output_io, protocol_execution_limits, publish_query_jsonl,
    random_id,
};
use crate::catalog_input::load_and_compile;
use crate::cli::{DiagnosticFormat, ExecutionCliConfig, ProgressMode, SummarizeArgs};
use crate::direct_source::matched_surface;
use crate::error::{CliError, FailureCode};
use crate::output::{SummaryTsvRow, sanitize_terminal, write_summary_tsv};
use crate::presentation::{OutputMode, resolve_summary_top};
use crate::protocol::diagnostic::{
    DiagnosticData, DiagnosticEnvelope, DiagnosticEvent, DiagnosticSeverity, DiagnosticStage,
};
use crate::protocol::query::{
    Coverage, DefinitionBody, DefinitionData, InventoryStats, MatchOrder, ProtocolKeyField,
    QueryComplete, QueryEnvelope, QueryEvent, QueryMode, QueryRun, ResourceHighWater, RunCounters,
    SourceSpan, SpanConvention, SummaryData, SummaryGroupData, TemporaryStorageLimits, Termination,
};
use crate::protocol::write_jsonl;
use crate::section_diagnostics::{
    warn_read_time_excluded, warn_self_label_no_ops, warn_unused_labels,
};
use crate::summary::{
    AggregationGroup, AggregationInput, FinalizedSummary, OwnedAggregationInput, SummaryAggregator,
    SummaryRow, shard_hash,
};
use crate::terminal::PresentationProgress;
use crate::terminal::TerminalArbiter;

trait SummaryEventWriter {
    fn write_summary(&mut self, pattern: &str, row: &SummaryRow) -> Result<(), ExecutionError>;
    fn write_complete(&mut self, complete: &QueryComplete) -> Result<(), ExecutionError>;
}

struct MachineSummaryEventWriter<'a, W> {
    writer: &'a mut W,
    output: OutputMode,
}

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
    finalized: &mut FinalizedSummary,
    top: Option<u64>,
    pattern_names: &[&str],
    mut complete: QueryComplete,
    writer: &mut impl SummaryEventWriter,
) -> Result<u64, CliError> {
    let mut publication_error = None;
    let visit_result = finalized.visit(top, |row| {
        let pattern = pattern_names
            .get(row.pattern_ordinal as usize)
            .ok_or_else(|| io::Error::other("summary pattern ordinal is out of range"))?;
        match writer.write_summary(pattern, row) {
            Ok(()) => Ok(()),
            Err(error) => {
                let message = error.to_string();
                publication_error = Some(error);
                Err(io::Error::other(message))
            }
        }
    });
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
    let catalog = load_and_compile(&args.catalog.catalog_input()?)?;
    let descriptors = catalog.authored_definitions()?;
    let catalog_fingerprint = catalog.fingerprint.clone();
    let compiled = catalog.output;
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

    // Parsed before any input is opened: a clause is authored text, so a bad
    // one is a usage error rather than a run that reads a corpus and fails.
    let scope_clause = parse_scope(args.selection.clause.as_deref())?;
    // Resolved before anything is read, and refused here rather than after a
    // corpus has been scanned.
    let sample = args.sample.resolve().map_err(CliError::InvalidArgs)?;
    if args.sample.blocks && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--sample-blocks requires --json so the run declares its correlated sample unit".into(),
        ));
    }
    if sample.is_some() && args.cache.cache {
        return Err(CliError::InvalidArgs(
            "--sample cannot be combined with --cache: the artifact would name a \
             fraction of the corpus as the whole of it. Sample a direct run, or \
             build the artifact first and sample the query against it."
                .to_owned(),
        ));
    }
    if sample.is_some() && args.tsv {
        // `summarize --tsv` is a fixed headerless row and carries no `run`
        // record, so a sampled total would travel with nothing saying it is an
        // estimate. Refusing is the honest half of "declare or refuse".
        return Err(CliError::InvalidArgs(
            "--sample cannot be combined with --tsv: `lingrep.summarize-tsv/v1` \
             has no run record, so a sampled count would be published as if it \
             were a census. Use JSONL, whose `run` record declares the sample."
                .to_owned(),
        ));
    }

    // Before query execution: direct execution is the default; `--cache`
    // observes ROOT, then reuses or creates a compatible artifact beside it.
    let resolved_corpus = crate::corpus_mode::resolve(
        &args.input,
        &args.cache,
        &args.input.markdown,
        config,
        terminal,
        &mut io::stderr(),
    )?;
    let selected_corpus = resolved_corpus.corpus.as_deref();
    let ResolvedMatchingQuery {
        source,
        source_stats,
        lifecycle,
        analysis,
        bound,
        run: run_facts,
        scope:
            QueryScope {
                scope_observation,
                prepared_scope,
                section_labels,
                markdown_diagnostics,
                read_time_policy,
            },
    } = resolve_matching_query(
        &args.input,
        selected_corpus,
        &compiled,
        &projected_attributes,
        scope_clause.as_ref(),
        config,
        terminal,
        QueryMode::Summarize,
        false,
        None,
        sample.filter(|_| args.sample.blocks),
        sample.is_none() && scope_clause.is_none(),
    )?;
    let run_corpus = lifecycle.run_corpus();
    let run_inventory = lifecycle.run_inventory();
    let run_id = random_id();
    // Checkable without reading input, so it is reported before the run rather
    // than beside its results.
    warn_self_label_no_ops(
        &args.input.markdown.self_label_no_ops(),
        config.diagnostic_format,
        &run_id,
    )?;
    if let Some(clause) = scope_clause.as_ref() {
        if let Some(policy) = read_time_policy.as_ref() {
            warn_read_time_excluded(clause, policy, config.diagnostic_format, &run_id)?;
        }
        // A prepared artifact's label set is complete here. A Markdown stream's
        // is not — its sink is read after the run, below.
        if markdown_diagnostics.is_none() {
            warn_unused_labels(clause, &section_labels, config.diagnostic_format, &run_id)?;
        }
    }
    let run = QueryEnvelope::new(QueryEvent::Run(Box::new(QueryRun {
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
        published_source_metadata: Vec::new(),
        sample: run_facts
            .block_sample_report
            .or_else(|| sample.map(crate::protocol::query::SampleReport::of)),
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
        let mut sink = SummarySink {
            aggregation: Some(
                SummaryAggregator::new(config.max_temp_bytes, config.workers.clamp(1, 8))
                    .map_err(CliError::Aggregation)?,
            ),
            aggregation_error: None,
            pattern_ordinals: &pattern_ordinals,
            policy: compiled.matcher.catalog_policy(),
            records_unkeyed: 0,
        };
        let progress_mode = config.progress;
        let presentation_progress = PresentationProgress::stderr();
        let progress_writer = presentation_progress.clone();
        let mut progress = move |progress: lingrep_core::execution::ExecutionProgress,
                                 _sink: &SummarySink<'_>| {
            if progress_mode == ProgressMode::Always {
                progress_writer.write_line(&format!(
                    "matched {} sentences, found {} matches",
                    progress.sentences_matched, progress.matches_found
                ));
            }
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
                Some(capacity) => Publication::from(PolicySelection::Apply).with_trace(capacity),
                None => Publication::from(PolicySelection::Apply),
            },
            prepared_scope
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
        // What the policy program did, which a summary otherwise cannot show:
        // its output is a total per pattern, and a total that came out lower
        // than expected has no visible cause in it.
        if progress_mode == ProgressMode::Always {
            crate::policy_report::write_per_statement(
                &presentation_progress,
                compiled.matcher.catalog_policy(),
                report.program_stats.per_operation(),
            );
        }
        if let Some(trace) = report.trace.as_ref() {
            crate::policy_report::write_trace(
                &presentation_progress,
                compiled.matcher.catalog_policy(),
                trace,
            );
        }
        presentation_progress.finish().map_err(CliError::Io)?;
        let aggregation = sink
            .aggregation
            .take()
            .expect("summary aggregation remains available after successful execution");
        let records_unkeyed = sink.records_unkeyed;
        drop(sink);

        let (complete_corpus, inventory) = lifecycle.complete(FilesystemCompletion::Full)?;
        let mut finalized = aggregation.finalize().map_err(CliError::Aggregation)?;
        let temp_high_water = finalized.peak_temp_bytes();

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
            write_alias_warning(config.diagnostic_format, &run_id, inventory)?;
        }

        let pattern_names = descriptors
            .iter()
            .map(|descriptor| catalog_policy.published_name(descriptor.name.as_str()))
            .collect::<Vec<_>>();
        // `summarize` publishes no per-match prefix, so it always runs to
        // completion: its basis is the corpus on both paths.
        // A Markdown stream only knows which labels it saw once it has finished
        // reading, so the unused-label report waits for the sink. On a limited
        // run this reports what the *prefix* observed, which is the honest
        // answer to "did this run see the label you named".
        if let (Some(clause), Some(diagnostics)) =
            (scope_clause.as_ref(), markdown_diagnostics.as_ref())
        {
            warn_unused_labels(
                clause,
                &diagnostics.observed_labels(),
                config.diagnostic_format,
                &run_id,
            )?;
        }
        let scope_report = build_scope_report(
            scope_clause.as_ref(),
            prepared_scope.as_ref(),
            &scope_observation,
            false,
        )?;
        let sentences_proven_absent = run_facts.filter_stats.as_ref().map_or(
            0,
            lingrep_corpus::PreparedFilterStats::sentences_proven_absent,
        );
        let complete = QueryComplete {
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
                tokens_matched: report.tokens_matched,
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
        let mut event_writer = MachineSummaryEventWriter {
            writer: &mut writer,
            output,
        };
        publish_summary_rows_and_complete(
            &mut finalized,
            top,
            &pattern_names,
            complete,
            &mut event_writer,
        )?;
        writer.flush().map_err(execution_output_io)?;
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

struct SummarySink<'a> {
    aggregation: Option<SummaryAggregator>,
    aggregation_error: Option<io::Error>,
    pattern_ordinals: &'a BTreeMap<String, u64>,
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
    /// What one record groups by, or `None` when it declares a key it could not
    /// satisfy.
    ///
    /// Three sources in a fixed order, each narrower than the next: a declared
    /// key, else a rendered unit, else the matched surface. The order is the
    /// point — every fallback is a coarser grouping than the one above it, so a
    /// record must never quietly fall through a declaration the author made.
    fn grouping<'r>(
        &self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        record: RecordView<'r>,
    ) -> Result<Option<AggregationInput<'r>>, ExecutionError> {
        // Keyed by the *definition's* name, not the published one: `output_name`
        // runs last, so every earlier pass looks policy up by the name
        // the researcher wrote.
        let declared_key = self
            .policy
            .get(record.definition)
            .is_some_and(|entry| !entry.key.is_empty());
        if declared_key {
            // Grouping on the surface counts spans, and spans are not
            // collocations.
            return Ok(record
                .key
                .map(AggregationInput::Key)
                .or_else(|| record.lexical_key.map(AggregationInput::LexicalKey)));
        }
        if let Some(unit) = record.unit {
            // A transform exists to construct the unit the researcher wants
            // counted; grouping the span it was built from would count the
            // input instead of the output.
            return Ok(Some(AggregationInput::Unit(unit)));
        }
        if let Some(lexical) = record.lexical {
            return Ok(Some(AggregationInput::Term(lexical.term.as_str())));
        }
        Ok(Some(AggregationInput::Surface(matched_surface(
            sentence,
            record.start..record.end,
            location,
        )?)))
    }

    fn grouping_owned(
        &self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        mut record: PublishedRecord,
    ) -> Result<Option<OwnedAggregationInput>, ExecutionError> {
        let declared_key = self
            .policy
            .get(record.raw.pattern.as_str())
            .is_some_and(|entry| !entry.key.is_empty());
        if declared_key {
            return Ok(record
                .key
                .take()
                .map(OwnedAggregationInput::Key)
                .or_else(|| {
                    record
                        .lexical_key
                        .take()
                        .map(OwnedAggregationInput::LexicalKey)
                }));
        }
        if let Some(unit) = record.unit.take() {
            return Ok(Some(OwnedAggregationInput::Unit(unit)));
        }
        if let Some(lexical) = record.raw.lexical.take() {
            return Ok(Some(OwnedAggregationInput::Term(lexical.term)));
        }
        Ok(Some(OwnedAggregationInput::Surface(matched_surface(
            sentence,
            record.start..record.end,
            location,
        )?)))
    }

    /// Lowers one sentence's records to routed groups: pattern ordinal, the
    /// owned group, and its shard-routing hash, all computed off the scan
    /// thread so the serial reduction only appends and dispatches.
    fn group_owned_records(
        &self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<Vec<(u64, Option<(OwnedAggregationInput, u64)>)>, ExecutionError> {
        records
            .into_iter()
            .map(|record| {
                let pattern_ordinal = self
                    .pattern_ordinals
                    .get(record.name.as_str())
                    .copied()
                    .ok_or_else(|| ExecutionError::Sink("unknown summary pattern".into()))?;
                let grouped = self
                    .grouping_owned(location, sentence, record)?
                    .map(|group| {
                        let hash = shard_hash(pattern_ordinal, &group);
                        (group, hash)
                    });
                Ok((pattern_ordinal, grouped))
            })
            .collect()
    }

    fn add_owned_groups(
        &mut self,
        groups: Vec<(u64, Option<(OwnedAggregationInput, u64)>)>,
    ) -> Result<usize, ExecutionError> {
        let records_emitted = groups.len();
        for (pattern_ordinal, grouped) in groups {
            let Some((grouped, hash)) = grouped else {
                self.records_unkeyed += 1;
                continue;
            };
            if let Err(error) = self
                .aggregation
                .as_mut()
                .expect("aggregation exists during execution")
                .add_owned_prehashed(pattern_ordinal, grouped, hash)
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
        let mut records_emitted = 0usize;
        for record in records {
            records_emitted += 1;
            let Some(grouped) = self.grouping(location, sentence, record)? else {
                self.records_unkeyed += 1;
                continue;
            };
            let pattern_ordinal = self
                .pattern_ordinals
                .get(record.pattern)
                .copied()
                .ok_or_else(|| ExecutionError::Sink("unknown summary pattern".into()))?;
            if let Err(error) = self
                .aggregation
                .as_mut()
                .expect("aggregation exists during execution")
                .add(pattern_ordinal, grouped)
            {
                let message = error.to_string();
                self.aggregation_error = Some(error);
                return Err(ExecutionError::Sink(message));
            }
        }
        // Every match is folded into the aggregation; nothing is skipped.
        Ok(SinkControl::Continue { records_emitted })
    }
}

impl MatchSink for SummarySink<'_> {
    fn wants_parallel_records_partition(&self) -> bool {
        true
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
        let groups = self.group_owned_records(location, sentence, records)?;
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
        for groups in grouped.into_iter().flatten() {
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
            let records_emitted = self.add_owned_groups(groups?)?;
            controls.push(SinkControl::Continue { records_emitted });
            if cancel.is_cancelled() {
                return Ok((controls, true));
            }
        }
        Ok((controls, false))
    }
}

fn write_alias_warning(
    format: DiagnosticFormat,
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
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    match format {
        DiagnosticFormat::Human => writeln!(
            writer,
            "warning[aliased_source_documents]: {}",
            sanitize_terminal(&message)
        )
        .map_err(execution_output_io)
        .map_err(CliError::from),
        DiagnosticFormat::Json => write_jsonl(
            &mut writer,
            &DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(DiagnosticData {
                run_id: Some(run_id.to_owned()),
                observed_input_digest: None,
                stage: DiagnosticStage::Aggregation,
                severity: DiagnosticSeverity::Warning,
                code: "aliased_source_documents".into(),
                message,
                suggestions: Vec::new(),
                usage: None,
                retry: None,
                location: None,
                related_locations: Vec::new(),
            })),
        )
        .map_err(execution_output_error)
        .map_err(CliError::from),
    }
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
                .add(0, AggregationInput::Surface(surface.to_owned()))
                .unwrap();
        }
        aggregation.finalize().unwrap()
    }

    fn complete_fixture() -> QueryComplete {
        QueryComplete {
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
