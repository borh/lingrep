//! Per-attribute presence over the admitted tokens, and the authored
//! exclusions that presence exposes.
//!
//! Shares `tokenize`'s token source and the query scope machinery. It reads
//! the **unprojected** prepared source deliberately: projecting
//! `required_attributes` — right for `summarize` — would report every
//! unprojected attribute absent, which is the wrong answer in the alarming
//! direction from a command whose whole subject is absence.

pub(crate) mod report;

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;

use lingrep_core::corpus::{
    AdmittedCorpus, CorpusReadConfig, ScopeObservation, resolve_input_format,
};
use lingrep_core::execution::{
    ExecutionBatch, ExecutionError, SentenceAdmission, SentenceBatchSource,
};
use lingrep_corpus::CorpusArtifact;

use self::report::{AttributeCoverage, exclusion_record};
use super::CommandOutcome;
use super::query_scope::{build_scope_report, parse_scope, resolve_prepared_scope};
use super::query_support::{
    execution_output_error, execution_output_io, random_id, write_token_schema_notice,
};
use crate::analyzer_selection::resolve_input;
use crate::catalog_input::load_and_compile;
use crate::cli::{AnalyzerCliConfig, AttributesArgs, ExecutionCliConfig, ProgressMode};
use crate::direct_source::{
    BoxSentenceSource, FilesystemCorpusEvents, admitted_filesystem_events, boxed_direct_source,
    encode_digest, scoped, source_from_events,
};
use crate::error::CliError;
use crate::output::{AttributesTsvRow, write_attributes_tsv};
use crate::presentation::OutputMode;
use crate::protocol::attributes::{
    AttributesEnvelope, AttributesEvent, CoverageData, ExclusionData, ProjectionData, ScopeData,
};
use crate::protocol::write_jsonl;
use crate::scope_precompute::ScopeResolution;
use crate::section_diagnostics::refuse_unanswerable;
use crate::terminal::{PresentationProgress, TerminalArbiter};

pub fn run(
    args: &AttributesArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    // Refused here rather than by clap, which would name the two flags and
    // not the recovery. Two record shapes in one headerless fixed-column
    // stream cannot be consumed, and silently dropping the exclusion table
    // would be worse than refusing the combination.
    if args.tsv && args.catalog.catalog_input().is_some() {
        return Err(CliError::InvalidArgs(
            "--tsv cannot be combined with a catalog; use --json for both tables".into(),
        ));
    }

    // Compiled before any input is opened: a broken catalog is a usage error,
    // not a run that reads a corpus and then fails.
    let catalog = args
        .catalog
        .catalog_input()
        .map(|input| load_and_compile(&input))
        .transpose()?;
    let exclusions: &[lingrep_core::fsm::ExclusionSite] = catalog.as_ref().map_or(&[], |catalog| {
        catalog.output.analysis.absence_sensitive_exclusions()
    });
    let projections: &[lingrep_core::fsm::LexiconProjectionSite] = catalog
        .as_ref()
        .map_or(&[], |catalog| catalog.output.analysis.lexicon_projections());
    let catalog_fingerprint = catalog.as_ref().map(|catalog| catalog.fingerprint.clone());

    let scope_clause = parse_scope(args.selection.clause.as_deref())?;
    let scope_observation = ScopeObservation::default();
    let mut prepared_scope: Option<ScopeResolution> = None;
    let mut section_labels = BTreeSet::new();

    let root = args.input.root.as_deref();
    let resolved_corpus = crate::corpus_mode::resolve(
        &args.input,
        &args.cache,
        &args.input.markdown,
        config,
        terminal,
        &mut io::stderr(),
    )?;
    let selected_corpus = resolved_corpus.corpus.as_deref();
    let stdin = selected_corpus.is_none() && root.is_none_or(|path| path == Path::new("-"));
    let ((source, source_documents), analyzer_plan, analyzer_run, schema, dictionary_identity) =
        if let Some(path) = selected_corpus {
            let artifact = CorpusArtifact::open(path)?;
            let schema = artifact.token_schema()?;
            let projection = schema
                .bind_projection(schema.keys().iter().cloned())
                .map_err(|error| CliError::Parse(error.to_string()))?;
            let resolved = scope_clause
                .as_ref()
                .map(|clause| {
                    let reader = artifact.provenance_reader()?;
                    resolve_prepared_scope(&reader, clause)
                })
                .transpose()?;
            prepared_scope = resolved.map(|(resolution, labels)| {
                section_labels = labels;
                resolution
            });
            let inputs = artifact.analysis_inputs()?;
            write_token_schema_notice(artifact.analyzer(), &inputs)?;
            let documents = artifact.document_count();
            let admission = prepared_scope
                .as_ref()
                .map_or(SentenceAdmission::All, |resolved| {
                    resolved.admission.clone()
                });
            (
                // `sentence_source`, never `sentence_source_with_attributes`:
                // this command's answer is *which columns are missing*, so a
                // projection would manufacture absences it then reports.
                (
                    Box::new(
                        artifact
                            .bound_sentence_source(&projection)?
                            .with_admission(admission)
                            .with_cancellation(terminal.cancellation().clone()),
                    ) as BoxSentenceSource,
                    documents,
                ),
                None,
                crate::protocol::query::AnalyzerRun::new(
                    artifact.analyzer(),
                    artifact.trust_class().address_component(),
                    &schema,
                ),
                schema,
                encode_digest(artifact.dictionary_fingerprint()?.as_bytes()),
            )
        } else {
            let format =
                resolve_input_format(if stdin { None } else { root }, args.input.input_format);
            let dict_path = (format != lingrep_core::corpus::CorpusFormat::Conllu
                && matches!(&config.analyzer, AnalyzerCliConfig::Vibrato))
            .then(|| args.input.direct_dictionary_path())
            .transpose()?;
            let resolved = resolve_input(
                format,
                config,
                dict_path.as_deref(),
                terminal.cancellation(),
            )?;
            let schema = resolved.schema().clone();
            let descriptor = resolved.descriptor().clone();
            let dictionary_digest = resolved.resource_identity();
            let analyzer_plan = resolved.into_native_plan();
            let projection = schema
                .bind_projection(schema.keys().iter().cloned())
                .map_err(|error| CliError::Parse(error.to_string()))?;
            let analyzer_record = lingrep_corpus::AnalyzerRecord::from_descriptor(&descriptor);
            let analyzer_run = crate::protocol::query::AnalyzerRun::new(
                &analyzer_record,
                "locally-executed",
                &schema,
            );
            crate::cli::validate_format_root(format, root, stdin).map_err(CliError::InvalidArgs)?;
            // Markdown and BCCWJ record section labels (BCCWJ's paragraph
            // tags ride `active_labels`, spec D2); the other direct formats
            // record none.
            if let Some(clause) = scope_clause.as_ref() {
                refuse_unanswerable(
                    clause,
                    format.supports_section_labels(),
                    &format!("{format:?} input").to_lowercase(),
                )?;
            }
            let markdown_config = args
                .input
                .markdown
                .resolve()
                .map_err(CliError::InvalidArgs)?
                .with_max_frontmatter_bytes(config.limits.max_stage_input_bytes);
            let reader_config = CorpusReadConfig::new(format, markdown_config);
            let (source, stats) = if stdin {
                // No completion observation: a stream digest is not a corpus
                // identity, and `attributes` reports none.
                let (_, events) = crate::direct_source::stdin_events(
                    &reader_config,
                    config.limits.max_input_record_bytes,
                );
                boxed_direct_source(Box::new(
                    source_from_events(scoped(
                        AdmittedCorpus::new(events),
                        scope_clause.as_ref(),
                        &scope_observation,
                    ))
                    .with_normalization(config.normalization)
                    .with_segmentation(config.segmentation)
                    .with_projection(projection.clone()),
                ))
            } else {
                let root = root.expect("filesystem root");
                let (events, _observation, _diagnostics) = FilesystemCorpusEvents::new(
                    root,
                    reader_config,
                    config.traversal.clone(),
                    config.limits.max_input_record_bytes,
                )?;
                boxed_direct_source(Box::new(
                    source_from_events(scoped(
                        admitted_filesystem_events(events),
                        scope_clause.as_ref(),
                        &scope_observation,
                    ))
                    .with_normalization(config.normalization)
                    .with_segmentation(config.segmentation)
                    .with_projection(projection.clone()),
                ))
            };
            (
                (source, stats.documents_admitted()),
                analyzer_plan,
                analyzer_run,
                schema,
                encode_digest(dictionary_digest),
            )
        };

    let mut source: Box<dyn SentenceBatchSource> = source;
    if let Some(plan) = analyzer_plan {
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .map_err(|error| CliError::Parse(error.to_string()))?;
        let segmentation = plan.descriptor().segmentation();
        source = Box::new(lingrep_core::execution::NativeAnalysisSource::new(
            source,
            plan.create_session(),
            projection,
            segmentation,
        ));
    }

    let limits = config.limits;
    limits.validate()?;

    let run_id = random_id();
    let presentation_progress = PresentationProgress::stderr();
    let post_run = (|| -> Result<CommandOutcome, CliError> {
        let mut coverage = AttributeCoverage::new(&schema);
        let mut admitted_sentences = 0_u64;
        loop {
            if terminal.cancellation().is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            let Some(batch) = source.next_batch(&limits)? else {
                break;
            };
            if terminal.cancellation().is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            let ExecutionBatch::BoundTokenized { sentences, .. } = batch else {
                return Err(CliError::from(ExecutionError::Source {
                    ordinal: admitted_sentences,
                    message: "attributes requires schema-bound analysis".into(),
                }));
            };
            for sentence in sentences {
                coverage.observe(sentence.value.words())?;
                admitted_sentences = admitted_sentences
                    .checked_add(1)
                    .ok_or(ExecutionError::ResourceAccounting)?;
            }
            if config.progress == ProgressMode::Always {
                presentation_progress.write_line(&format!(
                    "observed {admitted_sentences} sentences, {} tokens",
                    coverage.admitted_tokens()
                ));
            }
        }
        presentation_progress
            .finish()
            .map_err(execution_output_io)?;

        // Every count is final before the first record is written: this is an
        // aggregate, and a partially-observed corpus cannot answer "is this
        // attribute absent anywhere?".
        let scope_report = build_scope_report(
            scope_clause.as_ref(),
            prepared_scope.as_ref(),
            &scope_observation,
            false,
        )?;
        if !terminal.try_claim_completion() {
            return Ok(CommandOutcome::Cancelled);
        }

        let stdout = io::stdout();
        let mut writer = stdout.lock();
        publish(
            &mut writer,
            output,
            &ScopeData {
                analyzer: analyzer_run,
                token_schema_identity: encode_digest(schema.digest().as_bytes()),
                admitted_tokens: coverage.admitted_tokens(),
                admitted_sentences,
                admitted_documents: source_documents,
                scope: scope_report
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| CliError::Parse(error.to_string()))?,
                dictionary_identity,
                catalog_fingerprint,
            },
            &coverage,
            exclusions,
            projections,
        )?;
        writer.flush().map_err(execution_output_io)?;
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

fn publish<W: Write>(
    writer: &mut W,
    output: OutputMode,
    scope: &ScopeData,
    coverage: &AttributeCoverage,
    exclusions: &[lingrep_core::fsm::ExclusionSite],
    projections: &[lingrep_core::fsm::LexiconProjectionSite],
) -> Result<(), ExecutionError> {
    let records = exclusions
        .iter()
        .map(|site| exclusion_record(site, coverage));
    match output {
        OutputMode::Jsonl => {
            emit(writer, AttributesEvent::Scope(Box::new(scope.clone())))?;
            for attribute in coverage.attributes() {
                let buckets = coverage.buckets(attribute);
                emit(
                    writer,
                    AttributesEvent::Coverage(CoverageData {
                        attribute: attribute.as_str().to_owned(),
                        present: buckets.present,
                        unspecified: buckets.unspecified,
                        absent: buckets.absent,
                    }),
                )?;
            }
            for record in records {
                emit(
                    writer,
                    AttributesEvent::Exclusion(ExclusionData {
                        definition: record.definition,
                        attribute: record.attribute,
                        source_name: record.source_name,
                        declaration_line: record.declaration_line,
                        site_count: record.site_count,
                        reachable: record.reachable,
                        absent_in_corpus: record.absent_in_corpus,
                        status: record.status.as_str().to_owned(),
                    }),
                )?;
            }
            for site in projections {
                let buckets = coverage.buckets(&site.attribute);
                let status = if !site.reachable {
                    "inert"
                } else if coverage.admitted_tokens() == 0 {
                    "unknown"
                } else if buckets.absent + buckets.empty == 0 {
                    "clean"
                } else {
                    "exposed"
                };
                emit(
                    writer,
                    AttributesEvent::Projection(ProjectionData {
                        definition: site.definition.to_string(),
                        attribute: site.attribute.pattern_key().to_owned(),
                        source_name: site.source_name.to_string(),
                        declaration_line: site.declaration_line,
                        reachable: site.reachable,
                        missing_in_corpus: buckets.absent,
                        empty_in_corpus: buckets.empty,
                        status: status.to_owned(),
                    }),
                )?;
            }
        }
        OutputMode::Tsv => {
            for attribute in coverage.attributes() {
                let buckets = coverage.buckets(attribute);
                write_attributes_tsv(
                    writer,
                    &AttributesTsvRow {
                        attribute: attribute.as_str().to_owned(),
                        present: buckets.present,
                        unspecified: buckets.unspecified,
                        absent: buckets.absent,
                        admitted_tokens: coverage.admitted_tokens(),
                    },
                )
                .map_err(execution_output_error)?;
            }
        }
        OutputMode::Human => {
            writeln!(
                writer,
                "{} tokens over {} sentences in {} documents",
                scope.admitted_tokens, scope.admitted_sentences, scope.admitted_documents
            )
            .map_err(execution_output_io)?;
            for attribute in coverage.attributes() {
                let buckets = coverage.buckets(attribute);
                writeln!(
                    writer,
                    "  {:<10} present {:>10}  unspecified {:>10}  absent {:>10}",
                    attribute.as_str(),
                    buckets.present,
                    buckets.unspecified,
                    buckets.absent
                )
                .map_err(execution_output_io)?;
            }
            let records: Vec<_> = records.collect();
            if !records.is_empty() {
                writeln!(writer, "exclusions (exposure, not lost matches)")
                    .map_err(execution_output_io)?;
                for record in records {
                    writeln!(
                        writer,
                        "  {}:{} {} [{}] {} absent_in_corpus {}",
                        record.source_name,
                        record.declaration_line,
                        record.definition,
                        record.attribute,
                        record.status.as_str(),
                        record.absent_in_corpus
                    )
                    .map_err(execution_output_io)?;
                }
            }
        }
    }
    Ok(())
}

fn emit<W: Write>(writer: &mut W, event: AttributesEvent) -> Result<(), ExecutionError> {
    write_jsonl(writer, &AttributesEnvelope::new(event)).map_err(execution_output_error)
}
