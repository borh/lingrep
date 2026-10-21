//! Direct bounded `matches` orchestration and publication.

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::time::Instant;

use lingrep_core::execution::{
    ExecutionError, MatchSink, PolicySelection, Publication, RunOutcome, SentenceAdmission,
    SinkControl, cached_run_attributes, execute_analyzed,
};
use lingrep_core::fsm::{Match, PublishedRecord, RecordView};
use rayon::ThreadPoolBuilder;

use super::CommandOutcome;
use super::query_completion::FilesystemCompletion;
use super::query_projection::token_evidence;
use super::query_resolution::{QueryScope, ResolvedMatchingQuery, resolve_matching_query};
use super::query_scope::{build_scope_report, parse_scope};
use super::query_support::{
    execution_output_error, execution_output_io, protocol_execution_limits, publish_query_jsonl,
    random_id,
};
use crate::catalog_input::load_and_compile;
use crate::cli::{ExecutionCliConfig, MatchFormat, MatchesArgs, ProgressMode};
use crate::direct_source::OversizedSentenceOmissionSummary;
use crate::direct_source::{matched_orth_tokens, matched_surface};
use crate::error::{CliError, FailureCode};
use crate::output::{
    ColorPolicy, ContextColorPalette, HumanKwicRow, HumanMatchLayout, HumanRenderer,
    MIN_HUMAN_MATCH_WIDTH, MatchTsvRow, TerminalColorCapability, write_match_tsv,
};
use crate::presentation::{ColorMode, OutputMode, resolve_match_context, resolve_match_limit};
use crate::protocol::query::{
    Capture, CharSpan as ProtocolCharSpan, CommittedPrefix, Coverage, DefinitionBody,
    DefinitionData, GraphBinding as ProtocolGraphBinding,
    GraphNodeAttribute as ProtocolGraphNodeAttribute, GraphNodeId as ProtocolGraphNodeId,
    MatchData, MatchOrder, ProtocolKeyField, PublishedProjection, QueryComplete, QueryEnvelope,
    QueryEvent, QueryMode, QueryRun, ResourceHighWater, RunCounters, SourceSpan, SpanConvention,
    TagField, TemporaryStorageLimits, Termination,
};
use crate::section_diagnostics::{
    warn_read_time_excluded, warn_self_label_no_ops, warn_unused_labels,
};
use crate::terminal::{PresentationProgress, TerminalArbiter};

#[derive(Debug, PartialEq, Eq)]
enum ValidatedCompletion<T> {
    Cancelled,
    Publish(T),
}

fn protocol_graph_node(node: lingrep_core::corpus::conllu::EnhancedNodeId) -> ProtocolGraphNodeId {
    match node {
        lingrep_core::corpus::conllu::EnhancedNodeId::Word { token_index } => {
            ProtocolGraphNodeId::Word {
                token_index: token_index as u64,
            }
        }
        lingrep_core::corpus::conllu::EnhancedNodeId::Empty { boundary, subindex } => {
            ProtocolGraphNodeId::Empty {
                boundary: boundary as u64,
                subindex: subindex as u64,
            }
        }
    }
}

fn protocol_graph_binding(binding: &lingrep_core::fsm::GraphBinding) -> ProtocolGraphBinding {
    match binding {
        lingrep_core::fsm::GraphBinding::Node {
            name,
            node,
            attributes,
        } => ProtocolGraphBinding::Node {
            name: name.to_string(),
            node: protocol_graph_node(*node),
            attributes: attributes
                .iter()
                .map(|attribute| ProtocolGraphNodeAttribute {
                    name: attribute.name.pattern_key().to_owned(),
                    value: attribute.value.to_string(),
                })
                .collect(),
        },
        lingrep_core::fsm::GraphBinding::Edge {
            name,
            head,
            dependent,
            relation,
        } => ProtocolGraphBinding::Edge {
            name: name.to_string(),
            head: protocol_graph_node(*head),
            dependent: protocol_graph_node(*dependent),
            relation: relation.to_string(),
        },
    }
}

fn resolve_matches_completion<T>(
    terminal: &TerminalArbiter,
    validate: impl FnOnce() -> Result<T, CliError>,
    observe_omissions: impl FnOnce() -> Result<Option<OversizedSentenceOmissionSummary>, ExecutionError>,
) -> Result<ValidatedCompletion<T>, CliError> {
    let validated = validate()?;
    if !terminal.try_claim_completion() {
        return Ok(ValidatedCompletion::Cancelled);
    }
    if let Some(summary) = observe_omissions()? {
        return Err(CliError::OversizedSentencesSkipped {
            count: summary.count,
            first_source_name: summary.first_source_name,
            first_ordinal: summary.first_ordinal,
            first_actual: summary.first_actual,
            limit: summary.limit,
        });
    }
    Ok(ValidatedCompletion::Publish(validated))
}

pub fn run(
    args: &MatchesArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
    color_mode: ColorMode,
) -> Result<CommandOutcome, CliError> {
    let format = match output {
        OutputMode::Human => MatchFormat::Human,
        OutputMode::Jsonl => MatchFormat::Jsonl,
        OutputMode::Tsv => MatchFormat::Tsv,
    };
    if args.retain.is_some() && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs("--retain requires --json".into()));
    }
    let context = resolve_match_context(output, args.context);
    let limit = resolve_match_limit(output, args.limit);
    let sink_output = match format {
        MatchFormat::Jsonl => MatchSinkOutput::Jsonl,
        MatchFormat::Tsv => MatchSinkOutput::Tsv,
        MatchFormat::Human => {
            let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
            let color = ColorPolicy::resolve(color_mode, no_color, io::stdout().is_terminal());
            let colorfgbg = std::env::var("COLORFGBG").ok();
            let colorterm = std::env::var("COLORTERM").ok();
            let term = std::env::var("TERM").ok();
            let theme = args.context_color_theme.resolve(colorfgbg.as_deref());
            let capability =
                TerminalColorCapability::resolve(colorterm.as_deref(), term.as_deref());
            let layout = if args.align_hits {
                HumanMatchLayout::AlignHits
            } else {
                HumanMatchLayout::Flowing
            };
            let width = terminal_size::terminal_size()
                .map_or(120, |(width, _)| usize::from(width.0))
                .max(MIN_HUMAN_MATCH_WIDTH);
            let palette = ContextColorPalette::new(args.context_color_depth, theme, capability);
            MatchSinkOutput::Human(HumanRenderer::new(
                palette,
                color,
                width,
                layout,
                args.morpheme_separator.clone(),
            ))
        }
    };
    if !args.source_metadata.is_empty() && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--source-metadata requires --json matches output".to_owned(),
        ));
    }
    let metadata_projection = if args.source_metadata.is_empty() {
        None
    } else {
        Some(
            lingrep_core::corpus::SourceMetadataProjection::new(args.source_metadata.clone())
                .map_err(|error| CliError::InvalidArgs(error.to_string()))?,
        )
    };
    let started = Instant::now();
    let catalog = load_and_compile(&args.catalog.catalog_input()?)?;
    if output == OutputMode::Tsv && !catalog.output.matcher.lexicons().is_empty() {
        return Err(CliError::InvalidArgs(
            "matches --tsv does not accept lexicon definitions; use --json or human output".into(),
        ));
    }
    let descriptors = catalog.authored_definitions()?;
    let catalog_fingerprint = catalog.fingerprint.clone();
    let compiled = catalog.output;

    // Parsed before any input is opened: a clause is authored text, so a bad
    // one is a usage error rather than a run that reads a corpus and then
    // fails.
    let scope_clause = parse_scope(args.selection.clause.as_deref())?;
    // Resolved before anything is read: an unusable rate must not surface
    // after a corpus has been scanned.
    let sample = args.sample.resolve().map_err(CliError::InvalidArgs)?;
    if args.sample.blocks && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--sample-blocks requires --json so the run declares its correlated sample unit".into(),
        ));
    }
    if sample.is_some() && args.cache.cache {
        // `--cache` publishes an artifact, and an artifact built from a sample
        // is a corpus of a fraction of the sentences wearing the identity of
        // the whole. Every later run against it would be silently sampled with
        // no record saying so.
        return Err(CliError::InvalidArgs(
            "--sample cannot be combined with --cache: the artifact would name a \
             fraction of the corpus as the whole of it. Sample a direct run, or \
             build the artifact first and sample the query against it."
                .to_owned(),
        ));
    }
    if sample.is_some() && args.tsv {
        // `lingrep.matches-tsv/v1` is headerless rows and nothing else, so a
        // sampled run would publish a fraction of the matches with nowhere to
        // say the fraction — and a consumer counting rows would read an
        // estimate as a census. The same argument as `summarize --tsv`.
        return Err(CliError::InvalidArgs(
            "--sample cannot be combined with --tsv: `lingrep.matches-tsv/v1` \
             has no run record, so a sampled run would publish a fraction of \
             the matches as if it were all of them. Use JSONL, whose `run` \
             record declares the sample."
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
    let char_offsets_available = output == OutputMode::Jsonl;
    // Built here rather than at its use site so a prepared source can borrow it
    // for deferred completion. Matching a partition finishes before that
    // partition's completion begins, both on the one consumer thread, so the
    // pool is idle exactly when completion runs — the sharing costs no threads
    // and cannot contend with the decode pool the producer is using.
    let pool = Arc::new(
        ThreadPoolBuilder::new()
            .num_threads(config.workers)
            .build()
            .map_err(|error| CliError::CommandFailure {
                code: FailureCode::Io,
                message: format!("failed to create worker pool: {error}"),
            })?,
    );
    // The one attribute set this run publishes. It is what a prepared source
    // decodes, what the `run` record declares, and what publication filters
    // every token by. One value rather than three, so a declaration cannot
    // drift from the records it describes — and so a direct run and a prepared
    // run over the same catalog publish *equal* evidence, which
    // `assert_jsonl_evidence_parity` pins. A prepared token carries only the
    // projection, so the filter is a no-op there; a direct token carries the
    // whole schema, so the filter is what narrows it to the same set.
    // Context rendering reads the same portable `text` values that
    // `cached_run_attributes` always includes. Requiring a UniDic-specific
    // raw-text field here would make an otherwise portable catalog impossible
    // to bind to Sudachi or an external analyzer.
    let projected_attributes = cached_run_attributes(&compiled.matcher);
    let published_projection = PublishedProjection::new(&projected_attributes);
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
        QueryMode::Matches,
        char_offsets_available,
        metadata_projection.clone(),
        sample.filter(|_| args.sample.blocks),
        limit.is_none() && sample.is_none() && scope_clause.is_none(),
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
        mode: QueryMode::Matches,
        catalog_fingerprint,
        definition_count: descriptors.len() as u64,
        analyzer: run_facts.analyzer,
        corpus: run_corpus,
        dictionary_identity: run_facts.dictionary_identity,
        reader_identity: run_facts.reader_identity,
        token_schema_identity: run_facts.token_schema_identity,
        published_attributes: published_projection.names().to_vec(),
        published_source_metadata: metadata_projection
            .as_ref()
            .map_or_else(Vec::new, |projection| projection.keys().to_vec()),
        sample: run_facts
            .block_sample_report
            .or_else(|| sample.map(crate::protocol::query::SampleReport::of)),
        ordering: MatchOrder::CorpusSentenceStartEndPatternCapturesGraphBindingsTerm,
        span_convention: SpanConvention::HalfOpenToken,
        char_offsets: char_offsets_available,
        requested_context: context as u64,
        requested_limit: limit,
        requested_top: None,
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
        // Block-buffered rather than the lock's own line buffering. A record
        // stream is written one record at a time and `StdoutLock` turns every
        // one of those into its own `write(2)`; the durability boundary that
        // matters is the logical batch seam, and `finish_batch` flushes there.
        // Human output re-flushes per row so it still arrives as it is drawn.
        let mut writer = io::BufWriter::with_capacity(256 * 1024, stdout.lock());
        if format == MatchFormat::Jsonl {
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

        let limits = config.limits;
        let direct = DirectMatchSink {
            writer: &mut writer,
            output: sink_output,
            requested_context: context,
            limit,
            projection: published_projection.clone(),
            char_offsets: char_offsets_available,
            offsets_omitted: 0,
            sequence: 0,
            emitted: 0,
            last_committed: None,
            scratch: Vec::new(),
            retention: args
                .retain
                .map(|capacity| MatchRetention::new(capacity, args.retain_seed)),
        };
        // The seam itself is `execute`'s; this surface only chooses.
        let selection = if args.raw_matches {
            PolicySelection::Raw
        } else {
            PolicySelection::Apply
        };
        // The trace request rides in with the selection rather than as another
        // argument to `execute`, so every other surface still passes a bare
        // `PolicySelection`.
        let publication = match args.trace_policy {
            Some(capacity) => Publication::from(selection).with_trace(capacity),
            None => Publication::from(selection),
        };
        let mut sink = direct;
        let progress_mode = config.progress;
        let presentation_progress = PresentationProgress::stderr();
        // The run record's declaration makes a saved file diagnosable. This
        // makes a live run *loud*, at the one moment a researcher can still
        // act on it — a `jq` script reading an attribute this catalog never
        // names gets nothing, silently.
        if progress_mode == ProgressMode::Always {
            presentation_progress.write_line(&format!(
                "publishing token attributes: {}",
                published_projection.names().join(", ")
            ));
        }
        let progress_writer = presentation_progress.clone();
        let mut progress =
            move |progress: lingrep_core::execution::ExecutionProgress,
                  _sink: &DirectMatchSink<'_, io::BufWriter<io::StdoutLock<'_>>>| {
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
            publication,
            prepared_scope
                .as_ref()
                .map_or(&SentenceAdmission::All, |resolved| &resolved.admission),
        )?;
        let report = match outcome {
            RunOutcome::Completed(report) => report,
            RunOutcome::Cancelled(_) => return Ok(CommandOutcome::Cancelled),
        };
        // What policy removed is a run fact and uses the same reporting
        // channel as other run facts. Reported only when policy actually
        // applied and actually removed something — a line of zeroes on every
        // policy-free run is noise that trains the reader to skip it.
        //
        // One figure for the run; the per-*statement* counts follow below.
        let policy_stats = report.policy_stats;
        let reporting_policy =
            selection == PolicySelection::Apply && progress_mode == ProgressMode::Always;
        if reporting_policy && policy_stats != lingrep_core::fsm::PolicyStats::default() {
            let captures = policy_stats.captures_dropped;
            presentation_progress.write_line(&format!(
                "policy dropped {captures} capture{}",
                if captures == 1 { "" } else { "s" }
            ));
        }
        // Per statement, in authored order, named by the text the researcher
        // wrote. Shared with `summarize` and `sentence-final`, which publish
        // through the same block and had no way to say what it did.
        if reporting_policy {
            crate::policy_report::write_per_statement(
                &presentation_progress,
                compiled.matcher.catalog_policy(),
                report.program_stats.per_operation(),
            );
        }
        // The per-record explanation, when `--trace-policy` asked for one.
        //
        // Printed whatever `--progress` says, unlike everything above it. Those
        // are unrequested run facts and stay behind `--progress always`; this
        // one was typed out loud, and a flag that answered nothing unless a
        // second flag agreed would be a flag that looks broken.
        if let Some(trace) = report.trace.as_ref() {
            crate::policy_report::write_trace(
                &presentation_progress,
                compiled.matcher.catalog_policy(),
                trace,
            );
        }
        presentation_progress.finish().map_err(CliError::Io)?;
        let limited = limit.is_some_and(|limit| sink.emitted == limit);
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
            limited,
        )?;
        let coverage = if limited {
            Coverage::Prefix
        } else {
            Coverage::Full
        };
        let termination = if limited {
            Termination::ResultLimit
        } else {
            Termination::Eof
        };
        let completion = resolve_matches_completion(
            terminal,
            || {
                lifecycle.complete(if limited {
                    FilesystemCompletion::PrefixUnavailable
                } else {
                    FilesystemCompletion::Full
                })
            },
            || source_stats.oversized_sentence_omissions(),
        )?;
        let (complete_corpus, inventory) = match completion {
            ValidatedCompletion::Cancelled => return Ok(CommandOutcome::Cancelled),
            ValidatedCompletion::Publish(completed) => completed,
        };
        if format == MatchFormat::Jsonl {
            let sentences_proven_absent = run_facts.filter_stats.as_ref().map_or(
                0,
                lingrep_corpus::PreparedFilterStats::sentences_proven_absent,
            );
            let retained_matches = sink.publish_retained()?;
            publish_query_jsonl(
                sink.writer,
                &QueryEnvelope::new(QueryEvent::Complete(QueryComplete {
                    retained_matches,
                    coverage,
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
                        records_emitted: sink.emitted,
                        // `matches` drops nothing, but it does publish the
                        // `key` field, so a record whose declared key could not
                        // be projected is published without one. That is the
                        // same unkeyable-record disposition `summarize` counts, and
                        // reporting a hardcoded zero here would have made the
                        // absent field look like "no key was declared".
                        records_unkeyed: report.policy_stats.records_unkeyed,
                        captures_dropped: report.policy_stats.captures_dropped,
                        // Only meaningful on a run that declares offsets: the
                        // sink leaves it zero otherwise, so an absent field
                        // never has to be read as "every record lost them".
                        offsets_omitted: sink.offsets_omitted,
                    },
                    elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    resources: ResourceHighWater {
                        staged_raw_bytes: source_stats
                            .max_staged_raw_bytes()
                            .try_into()
                            .unwrap_or(u64::MAX),
                        tokenized_bytes: max_tokenized_bytes.try_into().unwrap_or(u64::MAX),
                        temporary_storage_bytes: 0,
                        execution_limits: protocol_execution_limits(config),
                        temporary_storage_limits: TemporaryStorageLimits {
                            max_temp_bytes: config.max_temp_bytes,
                        },
                    },
                    committed_prefix: (coverage == Coverage::Prefix)
                        .then(|| sink.last_committed.clone())
                        .flatten(),
                    inventory,
                    termination,
                    scope: scope_report.clone(),
                })),
            )?;
        }
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

enum MatchSinkOutput {
    Jsonl,
    Tsv,
    Human(HumanRenderer),
}

/// A token span's code-point range, or `None` when the sentence cannot place
/// either end.
///
/// All-or-nothing on purpose. A span whose first token has an offset and whose
/// last does not would otherwise publish a range ending where the match does
/// not, and a half-known position is the one answer worse than none — the same
/// rule that drops a capture leaving a rewritten span rather than clamping it.
fn span_char_range(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: std::ops::Range<usize>,
) -> Option<ProtocolCharSpan> {
    if span.start >= span.end {
        return None;
    }
    let first = sentence.char_span(span.start)?;
    let last = sentence.char_span(span.end.checked_sub(1)?)?;
    Some(first.join(last).into())
}

struct DirectMatchSink<'a, W> {
    writer: &'a mut W,
    output: MatchSinkOutput,
    requested_context: usize,
    limit: Option<u64>,
    projection: PublishedProjection,
    sequence: u64,
    emitted: u64,
    /// Whether this run's source derives character offsets at all.
    ///
    /// Held so the sink can tell a record that lost its offsets from a run that
    /// never had any: only the first is worth counting, and counting the second
    /// would report every record of a Markdown run as an omission.
    char_offsets: bool,
    offsets_omitted: u64,
    last_committed: Option<CommittedPrefix>,
    /// Reused serialization scratch for the JSONL record stream.
    ///
    /// One buffer for the whole run rather than one allocation per record;
    /// see [`crate::protocol::write_jsonl_buffered`].
    scratch: Vec<u8>,
    retention: Option<MatchRetention>,
}

struct MatchRetention {
    capacity: u32,
    seed: u64,
    seen: u64,
    bytes: usize,
    records: Vec<(u64, Vec<u8>, bool)>,
}

impl MatchRetention {
    const MAX_BYTES: usize = 64 * 1024 * 1024;

    fn new(capacity: u32, seed: u64) -> Self {
        Self {
            capacity,
            seed,
            seen: 0,
            bytes: 0,
            records: Vec::new(),
        }
    }

    fn push(
        &mut self,
        sequence: u64,
        event: &QueryEnvelope,
        offsets_missing: bool,
    ) -> Result<(), ExecutionError> {
        self.seen = self
            .seen
            .checked_add(1)
            .ok_or(ExecutionError::ResourceAccounting)?;
        let draw = lingrep_core::execution::reservoir_draw(self.seed, 0, self.seen) % self.seen;
        let slot = if self.records.len() < self.capacity as usize {
            self.records.len()
        } else if draw < u64::from(self.capacity) {
            draw as usize
        } else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(event).map_err(|error| ExecutionError::InvalidLimit {
            field: "retain",
            message: error.to_string(),
        })?;
        let old = self
            .records
            .get(slot)
            .map_or(0, |(_, bytes, _)| bytes.len());
        let total = self
            .bytes
            .checked_sub(old)
            .and_then(|n| n.checked_add(bytes.len()))
            .ok_or(ExecutionError::ResourceAccounting)?;
        if total > Self::MAX_BYTES {
            return Err(ExecutionError::InvalidLimit {
                field: "retain",
                message: "retained matches exceed 64 MiB; reduce --retain or --context".into(),
            });
        }
        self.bytes = total;
        if slot == self.records.len() {
            self.records.push((sequence, bytes, offsets_missing));
        } else {
            self.records[slot] = (sequence, bytes, offsets_missing);
        }
        Ok(())
    }
}

impl<W: Write> DirectMatchSink<'_, W> {
    fn publish_retained(
        &mut self,
    ) -> Result<Option<crate::protocol::query::RetainedMatches>, CliError> {
        let Some(retention) = self.retention.as_mut() else {
            return Ok(None);
        };
        retention
            .records
            .sort_unstable_by_key(|(sequence, _, _)| *sequence);
        for (_, bytes, _) in &retention.records {
            self.writer.write_all(bytes)?;
            self.writer.write_all(b"\n")?;
        }
        self.emitted = retention.records.len() as u64;
        self.offsets_omitted = retention
            .records
            .iter()
            .filter(|(_, _, missing)| *missing)
            .count() as u64;
        Ok(Some(crate::protocol::query::RetainedMatches {
            algorithm: "algorithm_r_splitmix64_v1".into(),
            unit: "published_match".into(),
            seed: retention.seed,
            capacity: retention.capacity,
            max_bytes: MatchRetention::MAX_BYTES as u64,
            seen: retention.seen,
            retained: self.emitted,
        }))
    }

    /// The one emission path both publication modes drive.
    ///
    /// Taking [`RecordView`] rather than `&Match` or `&PublishedRecord` is what
    /// keeps it one path: the modes differ only in whether `unit`, `key`, and
    /// `tags` exist to write, and every other decision here — format, context,
    /// limit, sequence accounting — is identical. Two copies of this loop would
    /// be two places for the limit to be checked differently.
    fn publish<'r>(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: impl Iterator<Item = RecordView<'r>>,
    ) -> Result<SinkControl, ExecutionError> {
        let mut records_emitted = 0usize;
        for record in records {
            if self.limit.is_some_and(|limit| self.emitted == limit) {
                return Ok(SinkControl::StopAfter { records_emitted });
            }
            let start = record.start.min(sentence.len());
            let end = record.end.min(sentence.len()).max(start);
            let left = start.saturating_sub(self.requested_context)..start;
            let right = end..end
                .saturating_add(self.requested_context)
                .min(sentence.len());
            match &self.output {
                MatchSinkOutput::Jsonl => {
                    let surface = matched_surface(sentence, record.start..record.end, location)
                        .map_err(ExecutionError::from)?;
                    let char_span = span_char_range(sentence, record.start..record.end);
                    let offsets_missing = self.char_offsets && char_span.is_none();
                    if offsets_missing {
                        self.offsets_omitted = self.offsets_omitted.saturating_add(1);
                    }
                    let event = QueryEnvelope::new(QueryEvent::Match(MatchData {
                        sequence: self.sequence,
                        pattern: record.pattern.to_owned(),
                        span: SourceSpan {
                            start: record.start as u64,
                            end: record.end as u64,
                        },
                        char_span,
                        captures: record
                            .captures
                            .iter()
                            .map(|capture| Capture {
                                name: capture.name.to_string(),
                                hygienic_path: capture.path.to_string(),
                                span: SourceSpan {
                                    start: capture.start as u64,
                                    end: capture.end as u64,
                                },
                                char_span: span_char_range(sentence, capture.start..capture.end),
                            })
                            .collect(),
                        graph_bindings: record
                            .graph_bindings
                            .iter()
                            .map(protocol_graph_binding)
                            .collect(),
                        corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                        source_name: location.source_name.to_string(),
                        source_metadata: location.source_metadata.clone(),
                        document_ordinal: location.document_ordinal,
                        document_sentence_ordinal: location.document_sentence_ordinal,
                        surface,
                        tokens: token_evidence(sentence, start..end, &self.projection),
                        left_context: (self.requested_context != 0)
                            .then(|| token_evidence(sentence, left.clone(), &self.projection)),
                        right_context: (self.requested_context != 0)
                            .then(|| token_evidence(sentence, right.clone(), &self.projection)),
                        unit: record.unit.map(ToOwned::to_owned),
                        key: record
                            .key
                            .map(|fields| {
                                fields
                                    .iter()
                                    .map(|field| ProtocolKeyField::Scalar {
                                        name: field.name.to_string(),
                                        value: field.value.clone(),
                                    })
                                    .collect()
                            })
                            .or_else(|| {
                                record.lexical_key.map(|fields| {
                                    fields
                                        .iter()
                                        .map(|field| {
                                            match &field.value {
                                            lingrep_core::fsm::LexicalProjectedKeyValue::Scalar(
                                                value,
                                            ) => ProtocolKeyField::Scalar {
                                                name: field.name.to_string(),
                                                value: value.clone(),
                                            },
                                            lingrep_core::fsm::LexicalProjectedKeyValue::Segments(
                                                values,
                                            ) => ProtocolKeyField::List {
                                                name: field.name.to_string(),
                                                values: values.clone(),
                                            },
                                        }
                                        })
                                        .collect()
                                })
                            }),
                        lexical: record.lexical.map(|lexical| {
                            crate::protocol::query::LexicalMatchData {
                                term: lexical.term.to_string(),
                                matched_by: lexical
                                    .matched_by
                                    .iter()
                                    .map(|attribute| attribute.pattern_key().to_owned())
                                    .collect(),
                            }
                        }),
                        tags: record
                            .tags
                            .iter()
                            .map(|(name, value)| TagField {
                                name: name.to_string(),
                                value: value.clone(),
                            })
                            .collect(),
                    }));
                    if let Some(retention) = self.retention.as_mut() {
                        retention.push(self.sequence, &event, offsets_missing)?;
                    } else {
                        crate::protocol::write_jsonl_buffered(
                            self.writer,
                            &mut self.scratch,
                            &event,
                        )
                        .map_err(execution_output_error)?;
                    }
                }
                MatchSinkOutput::Tsv => {
                    let surface = matched_surface(sentence, record.start..record.end, location)
                        .map_err(ExecutionError::from)?;
                    write_match_tsv(
                        self.writer,
                        &MatchTsvRow {
                            sequence: self.sequence,
                            pattern: record.pattern.to_owned(),
                            source_name: location.source_name.to_string(),
                            document_ordinal: location.document_ordinal,
                            document_sentence_ordinal: location.document_sentence_ordinal,
                            corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                            match_start: record.start as u64,
                            match_end: record.end as u64,
                            left_surface: (self.requested_context != 0)
                                .then(|| matched_surface(sentence, left.clone(), location))
                                .transpose()
                                .map_err(ExecutionError::from)?,
                            surface,
                            right_surface: (self.requested_context != 0)
                                .then(|| matched_surface(sentence, right.clone(), location))
                                .transpose()
                                .map_err(ExecutionError::from)?,
                        },
                    )
                    .map_err(execution_output_error)?;
                }
                MatchSinkOutput::Human(renderer) => {
                    if let Some(lexical) = record.lexical {
                        writeln!(
                            self.writer,
                            "term={} matched_by={}",
                            lexical.term,
                            lexical
                                .matched_by
                                .iter()
                                .map(lingrep_core::token_match::AttributeKey::pattern_key)
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                        .map_err(execution_output_io)?;
                    }
                    let row = HumanKwicRow {
                        left: matched_orth_tokens(sentence, left.clone(), location)
                            .map_err(ExecutionError::from)?,
                        keyword: matched_orth_tokens(sentence, record.start..record.end, location)
                            .map_err(ExecutionError::from)?,
                        right: matched_orth_tokens(sentence, right.clone(), location)
                            .map_err(ExecutionError::from)?,
                    };
                    renderer
                        .match_row(&mut *self.writer, &row)
                        .map_err(execution_output_io)?;
                    // Human output is watched as it arrives, so it flushes per
                    // row even though this sink writes through a `BufWriter`.
                    // The machine formats above flush at `finish_batch` boundaries instead.
                    self.writer.flush().map_err(execution_output_io)?;
                }
            }
            self.last_committed = Some(CommittedPrefix {
                corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                sequence: self.sequence,
            });
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
            self.emitted = self
                .emitted
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
            records_emitted = records_emitted
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
            if self.limit.is_some_and(|limit| self.emitted == limit) {
                return Ok(SinkControl::StopAfter { records_emitted });
            }
        }
        Ok(SinkControl::Continue { records_emitted })
    }
}

impl<W: Write> MatchSink for DirectMatchSink<'_, W> {
    fn push(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.publish(location, sentence, matches.iter().map(RecordView::raw))
    }

    fn push_records(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        self.publish(
            location,
            sentence,
            records.iter().map(RecordView::published),
        )
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.writer.flush().map_err(execution_output_io)
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::query_support::token_schema_mismatch;
    use lingrep_core::corpus::CorpusReadConfig;
    use lingrep_corpus::ReaderIdentity;
    use std::cell::Cell;

    use super::*;
    use lingrep_core::token_match::TokenInfo;

    /// A counting writer: what reached the destination, and how often the
    /// sink asked for it to be made durable.
    #[derive(Default)]
    struct CountingWriter {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn sink_test_location() -> lingrep_core::execution::SentenceLocation {
        lingrep_core::execution::SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: 3,
            document_id: lingrep_core::corpus::DocumentId::new(7),
            document_ordinal: 11,
            document_sentence_ordinal: 1,
            source_name: Arc::from("doc.md"),
        }
    }

    fn sink_test_sentence() -> Vec<TokenInfo> {
        ["形態", "素", "解析"]
            .into_iter()
            .map(|orth| {
                let mut token = TokenInfo::default();
                token.set(
                    lingrep_core::token_match::TokenAttributeIndex::Orth,
                    orth.into(),
                );
                token
            })
            .collect()
    }

    fn sink_test_matches(count: usize) -> Vec<Match> {
        (0..count)
            .map(|index| Match {
                pattern: "p".into(),
                start: index % 3,
                end: index % 3 + 1,
                captures: Box::new([]),
                graph_bindings: Box::default(),
                lexical: None,
            })
            .collect()
    }

    fn sink_over<'a, W: Write>(
        writer: &'a mut W,
        output: MatchSinkOutput,
    ) -> DirectMatchSink<'a, W> {
        DirectMatchSink {
            writer,
            output,
            requested_context: 0,
            limit: None,
            projection: PublishedProjection::everything(),
            char_offsets: false,
            offsets_omitted: 0,
            sequence: 0,
            emitted: 0,
            last_committed: None,
            scratch: Vec::new(),
            retention: None,
        }
    }

    /// The record stream owes durability at the **logical batch seam**, not at
    /// each record, and that is where the publication win came from: a
    /// per-record flush turns every record into its own `write(2)` and defeats
    /// the writer's buffer. Measured on `patterns/distributions.lx` over the
    /// tracked corpus, per-record flushing cost 5.6s of the run's 6.0s of
    /// system time and 38% of its wall clock.
    ///
    /// Byte-for-byte, the stream is the one `write_jsonl` produced; only the
    /// flush cadence differs. `matches_jsonl_bytes_are_unchanged_by_buffering`
    /// below pins the bytes.
    #[test]
    fn matches_jsonl_records_flush_only_at_the_batch_seam() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(8);
        let mut writer = CountingWriter::default();
        {
            let mut sink = sink_over(&mut writer, MatchSinkOutput::Jsonl);
            sink.push(&location, &sentence, &matches)
                .expect("published");
            assert_eq!(
                sink.writer.flushes, 0,
                "eight records must not ask for eight flushes"
            );
            sink.finish_batch().expect("batch finished");
        }
        assert_eq!(writer.flushes, 1, "the batch seam is the durability point");
        assert_eq!(
            writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
            8,
            "one line per record still reaches the destination"
        );
    }

    /// Human output is watched as it is drawn, so it flushes per row, which a
    /// `BufWriter` does not. Machine formats flush at batch boundaries instead.
    #[test]
    fn matches_human_rows_still_flush_per_row() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(5);
        let mut writer = CountingWriter::default();
        let renderer = HumanRenderer::new(
            ContextColorPalette::new(
                5,
                crate::output::ResolvedContextColorTheme::Dark,
                TerminalColorCapability::Ansi16,
            ),
            ColorPolicy::resolve(ColorMode::Never, true, false),
            80,
            HumanMatchLayout::Flowing,
            "".parse().expect("empty separator is terminal-safe"),
        );
        let mut sink = sink_over(&mut writer, MatchSinkOutput::Human(renderer));
        sink.push(&location, &sentence, &matches)
            .expect("published");
        assert_eq!(sink.writer.flushes, 5, "one flush per drawn row");
    }

    /// The buffered record path and the flushing one produce the same bytes.
    /// Two spellings of the same wire format is exactly how a format silently
    /// drifts, so this compares them directly rather than trusting review.
    #[test]
    fn matches_jsonl_bytes_are_unchanged_by_buffering() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(4);

        let mut buffered = CountingWriter::default();
        {
            let mut sink = sink_over(&mut buffered, MatchSinkOutput::Jsonl);
            sink.push(&location, &sentence, &matches)
                .expect("published");
        }

        let mut per_record = CountingWriter::default();
        {
            let mut sink = sink_over(&mut per_record, MatchSinkOutput::Jsonl);
            for one in &matches {
                sink.publish(&location, &sentence, std::iter::once(RecordView::raw(one)))
                    .expect("published");
            }
        }
        assert_eq!(
            String::from_utf8(buffered.bytes).expect("utf-8"),
            String::from_utf8(per_record.bytes).expect("utf-8"),
        );
    }

    #[test]
    fn retained_offset_omissions_count_only_emitted_records() {
        let mut writer = CountingWriter::default();
        let mut sink = sink_over(&mut writer, MatchSinkOutput::Jsonl);
        sink.char_offsets = true;
        sink.retention = Some(MatchRetention::new(5, 7));
        sink.push(
            &sink_test_location(),
            &sink_test_sentence(),
            &sink_test_matches(20),
        )
        .unwrap();
        let summary = sink.publish_retained().unwrap().unwrap();
        assert_eq!(summary.seen, 20);
        assert_eq!(sink.emitted, 5);
        assert_eq!(sink.offsets_omitted, 5);
    }

    #[test]
    fn retention_byte_budget_fails_without_publishing_partial_evidence() {
        let location = sink_test_location();
        let mut token = TokenInfo::default();
        token.set(
            lingrep_core::token_match::TokenAttributeIndex::Orth,
            "a".repeat(1024 * 1024).into(),
        );
        let sentence = vec![token];
        let matches = vec![
            Match {
                pattern: "p".into(),
                start: 0,
                end: 1,
                captures: Box::new([]),
                graph_bindings: Box::default(),
                lexical: None
            };
            64
        ];
        let mut writer = CountingWriter::default();
        let mut sink = sink_over(&mut writer, MatchSinkOutput::Jsonl);
        sink.retention = Some(MatchRetention::new(100_000, 7));
        let error = sink.push(&location, &sentence, &matches).unwrap_err();
        assert!(matches!(
            error,
            ExecutionError::InvalidLimit {
                field: "retain",
                ..
            }
        ));
        assert!(sink.writer.bytes.is_empty());
        assert!(sink.retention.as_ref().unwrap().bytes <= MatchRetention::MAX_BYTES);
    }

    #[test]
    fn integrity_failure_prevents_cancellation_and_omission_resolution() {
        let terminal = TerminalArbiter::new();
        let omission_read = Cell::new(false);

        let error = resolve_matches_completion(
            &terminal,
            || Err::<(), CliError>(CliError::Execution(ExecutionError::InventoryMismatch)),
            || {
                omission_read.set(true);
                Ok(None)
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::Execution(ExecutionError::InventoryMismatch)
        ));
        assert!(!omission_read.get());
    }

    #[test]
    fn cancellation_prevents_the_omission_read() {
        let terminal = TerminalArbiter::new();
        assert!(terminal.try_cancel(1));

        let outcome = resolve_matches_completion(
            &terminal,
            || Ok("validated"),
            || panic!("cancelled completion must not read omission stats"),
        )
        .unwrap();

        assert_eq!(outcome, ValidatedCompletion::Cancelled);
    }

    #[test]
    fn omission_prevents_publication_after_validation_and_completion_claim() {
        let terminal = TerminalArbiter::new();
        let error = resolve_matches_completion(
            &terminal,
            || Ok("validated"),
            || {
                Ok(Some(OversizedSentenceOmissionSummary {
                    count: 2,
                    first_source_name: "00027.md".into(),
                    first_ordinal: 7,
                    first_actual: 187_965,
                    limit: 65_536,
                }))
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::OversizedSentencesSkipped { count: 2, .. }
        ));
    }

    #[test]
    fn omission_observation_failure_prevents_publication() {
        let terminal = TerminalArbiter::new();
        let error = resolve_matches_completion(
            &terminal,
            || Ok("validated"),
            || Err(ExecutionError::ResourceAccounting),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::Execution(ExecutionError::ResourceAccounting)
        ));
    }

    fn analyzer_record(family: &str, profile: &str) -> lingrep_corpus::AnalyzerRecord {
        let mut record = lingrep_corpus::AnalyzerRecord::vibrato_fixture();
        record.family = family.to_owned();
        record.profile = profile.to_owned();
        record
    }

    fn declared_digest(family: &str, profile: &str) -> [u8; 32] {
        *analyzer_record(family, profile)
            .declared_token_schema()
            .expect("a native family declares a schema")
            .digest()
            .as_bytes()
    }

    /// The round trip: an artifact this build could have produced is silent.
    ///
    /// Without this the notice could be unconditional and the mismatch test
    /// below would still pass, which would make every prepared run noisy.
    #[test]
    fn an_artifact_built_under_this_token_schema_is_not_reported() {
        let record = analyzer_record("vibrato", "morphology");
        assert_eq!(
            token_schema_mismatch(&record, &declared_digest("vibrato", "morphology")),
            None
        );
    }

    /// The defect this comparison was carrying: a Sudachi artifact was
    /// measured against the *Vibrato* schema, so every one of them was
    /// reported as stale and told to rebuild — which would have produced the
    /// identical artifact and left the notice firing.
    #[test]
    fn a_sudachi_artifact_is_measured_against_the_sudachi_schema() {
        let sudachi = declared_digest("sudachi", "morphology");
        assert_ne!(
            sudachi,
            declared_digest("vibrato", "morphology"),
            "the two schemas must differ, or this test proves nothing"
        );
        assert_eq!(
            token_schema_mismatch(&analyzer_record("sudachi", "morphology"), &sudachi),
            None
        );
    }

    /// An adapter-hosted analyzer declares its schema at handshake time, so
    /// this build has nothing to compare and must claim nothing.
    #[test]
    fn an_adapter_hosted_analyzer_draws_no_claim() {
        assert_eq!(
            token_schema_mismatch(&analyzer_record("external", "morphology"), &[0; 32]),
            None
        );
    }

    /// A differing schema is reported, and the message names the rebuild.
    ///
    /// The recovery is the whole value of the notice: knowing the identities
    /// differ is useless to a researcher who is not told that `corpus build`
    /// is what reconciles them.
    #[test]
    fn an_artifact_built_under_another_token_schema_names_the_rebuild() {
        let message = token_schema_mismatch(&analyzer_record("vibrato", "morphology"), &[0; 32])
            .expect("a differing token schema must be reported");
        assert!(
            message.contains("corpus build"),
            "the notice must name the rebuild, got {message}"
        );
    }

    #[test]
    fn bounded_markdown_identity_does_not_alias_the_historical_direct_or_prepared_fixture() {
        let bounded = ReaderIdentity::derive(
            &CorpusReadConfig::Markdown(lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            )),
            lingrep_core::analysis::NormalizationPolicy::Nfc,
        );
        let mut historical = blake3::Hasher::new();
        historical.update(b"lingrep/corpus-reader/v1");
        historical.update(&0_u64.to_le_bytes());
        historical.update(&[3]);
        let historical =
            lingrep_corpus::ReaderIdentity::from_bytes(*historical.finalize().as_bytes());

        assert_ne!(bounded, historical);
    }
}
