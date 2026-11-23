//! Per-attribute presence over the admitted tokens, and the authored
//! exclusions that presence exposes.
//!
//! Shares `tokenize`'s token source and the query scope machinery. It reads
//! the **unprojected** prepared source: projecting
//! `required_attributes` — used by `summarize` — would report every
//! unprojected attribute absent.

pub(crate) mod report;

use std::io::{self, Write};

use lingrep_core::execution::{ExecutionBatch, ExecutionError};

use self::report::{AttributeCoverage, exclusion_record};
use super::CommandOutcome;
use super::query_resolution::{CorpusSourceSpec, OpenedCorpus};
use super::query_scope::parse_scope;
use super::query_support::{
    build_worker_pool, execution_output_error, execution_output_io, random_id,
};
use crate::catalog_input::load_and_compile;
use crate::cli::{AttributesArgs, ExecutionCliConfig};
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::output::{AttributesTsvRow, write_attributes_tsv};
use crate::presentation::{OutputMode, ResultDelivery};
use crate::protocol::attributes::{
    AttributesEnvelope, AttributesEvent, CoverageData, ExclusionData, ProjectionData, ScopeData,
};
use crate::protocol::write_jsonl;
use crate::terminal::{PresentationProgress, TerminalArbiter};

pub fn run(
    args: &AttributesArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
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
    let resolved_corpus = crate::corpus_mode::resolve(
        &args.input,
        &args.cache,
        &args.input.markdown,
        config,
        terminal,
        output,
    )?;
    let opened = OpenedCorpus::open(
        &args.input,
        resolved_corpus.corpus.as_deref(),
        config,
        terminal,
    )?;
    let projection = opened.full_projection()?;
    let decode_pool = (opened.is_prepared() && config.workers > 1)
        .then(|| build_worker_pool(config.workers))
        .transpose()?;
    let mut stream = opened
        .into_source(
            CorpusSourceSpec {
                scope_clause: scope_clause.as_ref(),
                decode_pool,
                attach_cancellation: true,
                ..CorpusSourceSpec::token_stream(projection.clone())
            },
            config,
            terminal,
        )?
        .into_bound_stream(projection);

    let limits = config.limits;
    limits.validate()?;

    let run_id = random_id();
    stream.scope.warn_before_run(
        scope_clause.as_ref(),
        &args.input.markdown.self_label_no_ops(),
        config,
        &run_id,
    )?;
    let presentation_progress = PresentationProgress::stderr(
        &config.stderr,
        config.progress,
        output,
        ResultDelivery::Deferred,
    );
    let post_run = (|| -> Result<CommandOutcome, CliError> {
        let mut coverage = AttributeCoverage::new(&stream.schema);
        let mut admitted_sentences = 0_u64;
        loop {
            if terminal.cancellation().is_cancelled() {
                return Ok(CommandOutcome::Cancelled);
            }
            let Some(batch) = stream.source.next_batch(&limits)? else {
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
            let tokens = coverage.admitted_tokens();
            presentation_progress.snapshot(move || {
                format!("observed {admitted_sentences} sentences, {tokens} tokens")
            });
        }
        presentation_progress.finish();

        // Every count is final before the first record is written: this is an
        // aggregate, and a partially-observed corpus cannot answer "is this
        // attribute absent anywhere?".
        let scope_report =
            stream
                .scope
                .report_after_run(scope_clause.as_ref(), false, config, &run_id)?;
        if !terminal.try_claim_completion() {
            return Ok(CommandOutcome::Cancelled);
        }

        let stdout = io::stdout();
        let mut writer = stdout.lock();
        publish(
            &mut writer,
            output,
            &ScopeData {
                analyzer: stream.run.analyzer,
                token_schema_identity: encode_digest(stream.schema.digest().as_bytes()),
                admitted_tokens: coverage.admitted_tokens(),
                admitted_sentences,
                admitted_documents: stream.source.into_stats().documents_admitted(),
                scope: scope_report
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| CliError::Parse(error.to_string()))?,
                dictionary_identity: stream.run.dictionary_identity,
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
