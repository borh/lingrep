//! Validated ownership for matching-query execution inputs.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use lingrep_core::analysis::{AnalyzerLanes, NativeAnalyzerPlan};
use lingrep_core::corpus::{
    AdmittedCorpus, CorpusReadConfig, ScopeObservation, resolve_input_format,
};
use lingrep_core::execution::{AnalysisBinding, SentenceAdmission};
use lingrep_core::fsm::{BoundCatalog, CompileOutput};
use lingrep_core::scope::ScopeClause;
use lingrep_core::token_match::{AttributeKey, TokenSchema};
use lingrep_corpus::{AnalysisInputs, CorpusArtifact, ReaderIdentity, TokenSchemaIdentity};
use rayon::ThreadPool;

use super::admission_identity;
use super::query_completion::CompletionObservation;
use super::query_scope::resolve_prepared_scope;
use super::query_source::{self, QueryInputStats};
use super::query_support::{
    protocol_inventory, protocol_traversal_limits, protocol_traversal_limits_from_report,
    random_id, write_binding_notice, write_token_schema_notice,
};
use crate::analyzer_selection::resolve_input;
use crate::cli::{AnalyzerCliConfig, ExecutionCliConfig, QueryInputArgs};
use crate::direct_source::{
    FilesystemCorpusEvents, admitted_filesystem_events, encode_digest, scoped, source_from_events,
    source_from_events_skipping_oversized,
};
use crate::error::CliError;
use crate::protocol::query::{AnalyzerRun, QueryMode, TraversalLimits};
use crate::scope_precompute::ScopeResolution;
use crate::section_diagnostics::refuse_unanswerable;
use crate::terminal::TerminalArbiter;

pub(crate) enum ResolvedAnalysis {
    Direct(Box<NativeAnalyzerPlan>),
    Tokenized(TokenSchema),
}

impl ResolvedAnalysis {
    pub(crate) fn binding<'a>(
        &'a self,
        workers: usize,
        pool: &'a ThreadPool,
    ) -> AnalysisBinding<AnalyzerLanes<'a, NativeAnalyzerPlan>> {
        match self {
            Self::Direct(plan) => {
                AnalysisBinding::Session(AnalyzerLanes::new(plan.as_ref(), workers, pool))
            }
            Self::Tokenized(schema) => AnalysisBinding::Prepared(schema.clone()),
        }
    }
}

pub(crate) struct QueryRunFacts {
    pub(crate) analyzer: AnalyzerRun,
    pub(crate) dictionary_identity: String,
    pub(crate) reader_identity: String,
    pub(crate) token_schema_identity: String,
    pub(crate) traversal_limits: TraversalLimits,
    pub(crate) filter_stats: Option<lingrep_corpus::PreparedFilterStats>,
    pub(crate) block_sample_report: Option<crate::protocol::query::SampleReport>,
}

pub(crate) struct ResolvedMatchingQuery {
    pub(crate) source: crate::direct_source::BoxSentenceSource,
    pub(crate) source_stats: QueryInputStats,
    pub(crate) lifecycle: CompletionObservation,
    pub(crate) analysis: ResolvedAnalysis,
    pub(crate) bound: BoundCatalog,
    pub(crate) run: QueryRunFacts,
    pub(crate) scope: QueryScope,
}

#[derive(Default)]
pub(crate) struct QueryScope {
    pub(crate) scope_observation: ScopeObservation,
    pub(crate) prepared_scope: Option<ScopeResolution>,
    pub(crate) section_labels: BTreeSet<String>,
    pub(crate) markdown_diagnostics: Option<lingrep_core::corpus::MarkdownDiagnostics>,
    pub(crate) read_time_policy: Option<lingrep_core::corpus::SectionPolicy>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_matching_query(
    input: &QueryInputArgs,
    selected_corpus: Option<&Path>,
    compiled: &CompileOutput,
    projected_attributes: &[AttributeKey],
    scope_clause: Option<&ScopeClause>,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    mode: QueryMode,
    char_offsets: bool,
    metadata_projection: Option<lingrep_core::corpus::SourceMetadataProjection>,
    block_sample: Option<lingrep_core::execution::CorpusSample>,
    allow_selective: bool,
) -> Result<ResolvedMatchingQuery, CliError> {
    if block_sample.is_some() && selected_corpus.is_none() {
        return Err(CliError::InvalidArgs(
            "--sample-blocks requires --corpus".into(),
        ));
    }
    let root = input.root.as_deref();
    let stdin = selected_corpus.is_none() && root.is_none_or(|path| path == Path::new("-"));
    let mut scope = QueryScope::default();
    Ok(if let Some(path) = selected_corpus {
        let artifact = CorpusArtifact::open(path)?;
        let schema = artifact.token_schema()?;
        let analyzer_name = crate::attribute_binding::analyzer_name(artifact.analyzer());
        let bound = compiled
            .matcher
            .bind_with_projection(&schema, projected_attributes.iter().cloned())
            .map_err(|error| crate::attribute_binding::bind_refusal(&error, &analyzer_name))?;
        crate::attribute_binding::write_unavailable_attribute_notices(
            &bound,
            &compiled.analysis,
            &analyzer_name,
        )?;
        let provenance = if scope_clause.is_some() || metadata_projection.is_some() {
            Some(artifact.provenance_reader()?)
        } else {
            None
        };
        let resolved = scope_clause
            .map(|clause| {
                resolve_prepared_scope(
                    provenance
                        .as_deref()
                        .expect("scoped query opened provenance"),
                    clause,
                )
            })
            .transpose()?;
        scope.prepared_scope = resolved.map(|(resolution, labels)| {
            scope.section_labels = labels;
            resolution
        });
        let admission = scope
            .prepared_scope
            .as_ref()
            .map_or(SentenceAdmission::All, |resolved| {
                resolved.admission.clone()
            });

        let open_source = || {
            let source = match &metadata_projection {
                Some(metadata) => artifact.bound_sentence_source_with_metadata(
                    bound.projection(),
                    metadata.clone(),
                    Arc::clone(
                        provenance
                            .as_ref()
                            .expect("metadata projection opened provenance"),
                    ),
                )?,
                None => match bound.sentence_filter().filter(|_| allow_selective) {
                    Some(filter) => {
                        artifact.filtered_sentence_source(bound.projection(), filter)?
                    }
                    None => artifact.bound_sentence_source(bound.projection())?,
                },
            };
            let source = if char_offsets {
                source
            } else {
                source.without_char_offsets()
            };
            Ok::<_, CliError>(
                source
                    .with_block_sample(block_sample)
                    .with_admission(admission.clone())
                    .with_cancellation(terminal.cancellation().clone()),
            )
        };
        let early_source = match mode {
            QueryMode::Matches => Some(open_source()?),
            QueryMode::Summarize => None,
        };
        let analysis = artifact.analysis_inputs()?;
        write_token_schema_notice(artifact.analyzer(), analysis.token_schema().as_bytes())?;
        let source_identity = encode_digest(artifact.source_identity().as_bytes());
        let analysis_identity = encode_digest(artifact.analysis_identity().as_bytes());
        let inventory = protocol_inventory(*artifact.inventory_stats());
        let traversal_limits = protocol_traversal_limits_from_report(artifact.discovery_report());
        let documents = artifact.document_count();
        let analyzer_run = AnalyzerRun::new(
            artifact.analyzer(),
            artifact.trust_class().address_component(),
            &schema,
        );
        let source = match early_source {
            Some(source) => source,
            None => open_source()?,
        };
        let filter_stats = source.filter_stats();
        let (source, source_stats) = query_source::prepared(source, documents);
        ResolvedMatchingQuery {
            scope,
            source,
            source_stats,
            lifecycle: CompletionObservation::Prepared {
                source_identity,
                analysis_identity,
                inventory,
            },
            analysis: ResolvedAnalysis::Tokenized(schema),
            bound,
            run: QueryRunFacts {
                filter_stats: Some(filter_stats),
                analyzer: analyzer_run,
                dictionary_identity: encode_digest(artifact.dictionary_fingerprint()?.as_bytes()),
                reader_identity: encode_digest(analysis.reader().as_bytes()),
                token_schema_identity: encode_digest(analysis.token_schema().as_bytes()),
                traversal_limits,
                block_sample_report: block_sample.map(|sample| {
                    crate::protocol::query::SampleReport::token_block(
                        sample,
                        artifact
                            .recorded_identity()
                            .components
                            .get("tokens.v1.zst")
                            .expect("validated artifact has a token component")
                            .clone(),
                    )
                }),
            },
        }
    } else {
        let format = resolve_input_format(if stdin { None } else { root }, input.input_format);
        let dict_path = (format != lingrep_core::corpus::CorpusFormat::Conllu
            && matches!(&config.analyzer, AnalyzerCliConfig::Vibrato))
        .then(|| input.direct_dictionary_path())
        .transpose()?;
        let resolved = resolve_input(format, config, dict_path.as_deref())?;
        let schema = resolved.schema().clone();
        let descriptor = resolved.descriptor().clone();
        let dictionary_digest = resolved.resource_identity();
        let analyzer_plan = resolved.into_native_plan();
        if let Some(dictionary_path) = dict_path.as_ref().and_then(|path| path.to_str()) {
            write_binding_notice(&compiled.matcher, dictionary_path)?;
        }
        let analyzer_record = lingrep_corpus::AnalyzerRecord::from_descriptor(&descriptor);
        let analyzer_run = AnalyzerRun::new(&analyzer_record, "locally-executed", &schema);
        let analyzer_name = crate::attribute_binding::analyzer_name(&analyzer_record);
        let bound = compiled
            .matcher
            .bind_with_projection(&schema, projected_attributes.iter().cloned())
            .map_err(|error| crate::attribute_binding::bind_refusal(&error, &analyzer_name))?;
        crate::attribute_binding::write_unavailable_attribute_notices(
            &bound,
            &compiled.analysis,
            &analyzer_name,
        )?;
        crate::cli::validate_format_root(format, root, stdin).map_err(CliError::InvalidArgs)?;

        if let Some(clause) = scope_clause {
            refuse_unanswerable(
                clause,
                format.supports_section_labels(),
                &format!("{format:?} input").to_lowercase(),
            )?;
        }
        let markdown_config = input
            .markdown
            .resolve()
            .map_err(CliError::InvalidArgs)?
            .with_max_frontmatter_bytes(config.limits.max_stage_input_bytes);
        scope.read_time_policy = Some(markdown_config.policy.clone());
        let reader_config = CorpusReadConfig::new(format, markdown_config);
        let normalization = if format == lingrep_core::corpus::CorpusFormat::Conllu {
            lingrep_core::analysis::NormalizationPolicy::None
        } else {
            config.normalization
        };
        let reader_identity = ReaderIdentity::derive(&reader_config, normalization);
        let analysis_inputs = AnalysisInputs::with_token_schema(
            reader_identity,
            admission_identity(),
            &descriptor,
            TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes()),
        )
        .map_err(|error| CliError::Corpus(error.to_string()))?;
        let dictionary_identity = encode_digest(dictionary_digest);
        let reader_identity_text = encode_digest(reader_identity.as_bytes());
        let token_schema_identity = encode_digest(analysis_inputs.token_schema().as_bytes());
        let ((source, source_stats), lifecycle) = if stdin {
            let (observation, events) = crate::direct_source::stdin_events(
                &reader_config,
                config.limits.max_input_record_bytes,
            );
            let stream_id = random_id();
            (
                query_source::direct(Box::new(
                    source_from_events(scoped(
                        AdmittedCorpus::new(events),
                        scope_clause,
                        &scope.scope_observation,
                    ))
                    .with_normalization(config.normalization)
                    .with_segmentation(config.segmentation)
                    .with_projection(bound.projection().clone())
                    .with_source_metadata(metadata_projection.clone()),
                )),
                CompletionObservation::Stdin {
                    stream_id,
                    observation,
                },
            )
        } else {
            let root = root.expect("filesystem root");
            let (events, observation, diagnostics) = FilesystemCorpusEvents::new(
                root,
                reader_config,
                config.traversal.clone(),
                config.limits.max_input_record_bytes,
            )?;
            scope.markdown_diagnostics = Some(diagnostics);
            let admitted = scoped(
                admitted_filesystem_events(events),
                scope_clause,
                &scope.scope_observation,
            );

            let source = match (mode, format) {
                (QueryMode::Matches, lingrep_core::corpus::CorpusFormat::Markdown) => {
                    source_from_events_skipping_oversized(admitted)
                }
                _ => source_from_events(admitted),
            }
            .with_normalization(config.normalization)
            .with_segmentation(config.segmentation)
            .with_projection(bound.projection().clone())
            .with_source_metadata(metadata_projection.clone());
            (
                query_source::direct(Box::new(source)),
                CompletionObservation::Filesystem {
                    observation,
                    analysis: analysis_inputs,
                },
            )
        };
        ResolvedMatchingQuery {
            scope,
            source,
            source_stats,
            lifecycle,
            analysis: analyzer_plan.map_or_else(
                || ResolvedAnalysis::Tokenized(schema.clone()),
                |plan| ResolvedAnalysis::Direct(Box::new(plan)),
            ),
            bound,
            run: QueryRunFacts {
                filter_stats: None,
                block_sample_report: None,
                analyzer: analyzer_run,
                dictionary_identity,
                reader_identity: reader_identity_text,
                token_schema_identity,
                traversal_limits: protocol_traversal_limits(config),
            },
        }
    })
}
