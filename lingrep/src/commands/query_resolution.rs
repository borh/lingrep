//! Unified direct/prepared corpus resolution and matching-query execution session.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lingrep_core::analysis::AnalyzerSession;
use lingrep_core::corpus::{
    AdmittedCorpus, CorpusFormat, CorpusReadConfig, ScopeCounts, SectionPolicy,
    SourceMetadataProjection, resolve_input_format,
};
use lingrep_core::execution::{
    AnalysisBinding, CorpusSample, ExecutionError, ExecutionProgress, ExecutionReport, MatchSink,
    NativeAnalysisSource, Publication, RunOutcome, SampledSentences, SentenceAdmission,
    TokenizedAdmission, execute_analyzed,
};
use lingrep_core::fsm::{BoundCatalog, CompileOutput, CompiledCatalog, SentenceFilter};
use lingrep_core::scope::ScopeClause;
use lingrep_core::token_match::{AttributeKey, BoundProjection, TokenSchema};
use lingrep_corpus::{
    AnalysisInputs, AnalyzerRecord, BlockingProvenance, CorpusArtifact, ReaderIdentity,
    ScopeResolution, TokenSchemaIdentity,
};
use rayon::ThreadPool;

use super::admission_identity;
use super::query_completion::CompletionObservation;
use super::query_scope::resolve_prepared_scope;
use super::query_source::{BoxQuerySource, QueryInputStats, QuerySentenceSource};
use super::query_support::{
    build_worker_pool, oversized_omissions_error, protocol_execution_limits, protocol_inventory,
    protocol_traversal_limits, protocol_traversal_limits_from_report, random_id,
    write_binding_notice, write_token_schema_notice,
};
use crate::analyzer_selection::{CliAnalyzerPlan, ResolvedInputAnalyzer, resolve_input};
use crate::cli::{AnalyzerCliConfig, ExecutionCliConfig, QueryInputArgs};
use crate::direct_source::{
    FilesystemCorpusEvents, admitted_filesystem_events, encode_digest, scoped, source_from_events,
    source_from_events_skipping_oversized,
};
use crate::error::CliError;
use crate::protocol::query::{
    AnalyzerRun, QueryMode, ResourceHighWater, RunCounters, SampleReport, TemporaryStorageLimits,
    TraversalLimits,
};
use crate::section_diagnostics::refuse_unanswerable;
use crate::terminal::TerminalArbiter;

pub(crate) enum ResolvedAnalysis {
    Direct(Box<CliAnalyzerPlan>),
    Tokenized(TokenSchema),
}

impl ResolvedAnalysis {
    pub(crate) fn binding<'a>(
        &'a self,
        workers: usize,
        pool: &'a ThreadPool,
    ) -> AnalysisBinding<Box<dyn AnalyzerSession + 'a>> {
        match self {
            Self::Direct(plan) => AnalysisBinding::Session(plan.create_lanes(workers, pool)),
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
    pub(crate) block_sample_report: Option<SampleReport>,
}

#[derive(Default)]
pub(crate) struct QueryScope {
    pub(crate) prepared_scope: Option<ScopeResolution>,
    pub(crate) section_labels: BTreeSet<String>,
    pub(crate) markdown_diagnostics: Option<lingrep_core::corpus::MarkdownDiagnostics>,
    pub(crate) read_time_policy: Option<SectionPolicy>,
}

impl QueryScope {
    pub(crate) fn warn_before_run(
        &self,
        clause: Option<&ScopeClause>,
        self_labels: &[String],
        config: &ExecutionCliConfig,
        run_id: &str,
    ) -> Result<(), CliError> {
        use crate::section_diagnostics::{
            warn_read_time_excluded, warn_self_label_no_ops, warn_unused_labels,
        };
        warn_self_label_no_ops(self_labels, config, run_id)?;
        if let Some(clause) = clause {
            if let Some(policy) = &self.read_time_policy {
                warn_read_time_excluded(clause, policy, config, run_id)?;
            }
            if self.markdown_diagnostics.is_none() {
                warn_unused_labels(clause, &self.section_labels, config, run_id)?;
            }
        }
        Ok(())
    }

    pub(crate) fn report_after_run(
        &self,
        clause: Option<&ScopeClause>,
        counts: ScopeCounts,
        limited: bool,
        config: &ExecutionCliConfig,
        run_id: &str,
    ) -> Result<Option<crate::protocol::query::ScopeReport>, CliError> {
        if let (Some(clause), Some(diagnostics)) = (clause, &self.markdown_diagnostics) {
            crate::section_diagnostics::warn_unused_labels(
                clause,
                &diagnostics.observed_labels(),
                config,
                run_id,
            )?;
        }
        Ok(super::query_scope::build_scope_report(
            clause,
            self.prepared_scope.as_ref(),
            counts,
            limited,
        ))
    }
}

pub(crate) struct QueryProjection<'a> {
    pub(crate) selected: &'a [AttributeKey],
    pub(crate) additional: &'a [AttributeKey],
    pub(crate) all: bool,
}

impl QueryProjection<'_> {
    fn bind(
        &self,
        compiled: &CompileOutput,
        schema: &TokenSchema,
        analyzer: &str,
        config: &ExecutionCliConfig,
    ) -> Result<BoundCatalog, CliError> {
        let catalog_attributes = compiled.matcher.execution_attributes();
        let mut attributes = self
            .selected
            .iter()
            .filter(|key| catalog_attributes.contains(key) || schema.keys().contains(key))
            .cloned()
            .collect::<Vec<_>>();
        if self.all {
            attributes.extend_from_slice(schema.keys());
        }
        for key in self.additional.iter().collect::<BTreeSet<_>>() {
            if schema.keys().contains(key) {
                attributes.push(key.clone());
            } else if !catalog_attributes.contains(key) {
                super::query_support::write_warning(
                    &config.stderr,
                    config.diagnostic_format,
                    crate::protocol::diagnostic::DiagnosticStage::Catalog,
                    "attribute_unavailable",
                    format!(
                        "--publish-attribute {key}: this corpus's token schema ({analyzer}) \
                         does not carry `{key}`; it will be omitted from exported tokens"
                    ),
                    None,
                    Vec::new(),
                )
                .map_err(CliError::Io)?;
            }
        }
        let bound = compiled
            .matcher
            .bind_with_projection(schema, attributes)
            .map_err(|error| crate::attribute_binding::bind_refusal(&error, analyzer))?;
        crate::attribute_binding::write_unavailable_attribute_notices(
            &bound,
            &compiled.analysis,
            analyzer,
            config,
        )?;
        crate::attribute_binding::write_unfolded_text_notice(
            schema,
            &compiled.analysis,
            analyzer,
            config,
        )?;
        crate::attribute_binding::write_normalization_notices(compiled, config)?;
        Ok(bound)
    }
}

/// Validates a direct input root against `format` and builds its `CorpusReadConfig`.
///
/// Shared by direct query commands (`matches`, `summarize`, `tokenize`,
/// `attributes`) and `InteractiveExecution` so every direct surface enforces
/// the same format-root rules and Markdown frontmatter limit.
pub(crate) fn build_direct_reader_config(
    format: CorpusFormat,
    root: Option<&Path>,
    stdin: bool,
    markdown: &lingrep_cli_common::MarkdownReaderArgs,
    config: &ExecutionCliConfig,
) -> Result<(CorpusReadConfig, SectionPolicy), CliError> {
    crate::cli::validate_format_root(format, root, stdin).map_err(CliError::InvalidArgs)?;
    let markdown_config = markdown
        .resolve()
        .map_err(CliError::InvalidArgs)?
        .with_max_frontmatter_bytes(config.limits.max_stage_input_bytes);
    let policy = markdown_config.policy.clone();
    Ok((CorpusReadConfig::new(format, markdown_config), policy))
}

pub(crate) struct OpenedPreparedCorpus {
    artifact: CorpusArtifact,
    schema: TokenSchema,
    analyzer_name: String,
}

pub(crate) struct OpenedDirectCorpus<'a> {
    input: &'a QueryInputArgs,
    root: Option<&'a Path>,
    stdin: bool,
    format: CorpusFormat,
    dict_path: Option<PathBuf>,
    resolved: ResolvedInputAnalyzer,
    analyzer_record: AnalyzerRecord,
    analyzer_name: String,
}

/// A corpus opened far enough to know its analyzer and token schema, before
/// binding a projection or opening its sentence stream.
pub(crate) enum OpenedCorpus<'a> {
    Prepared(Box<OpenedPreparedCorpus>),
    Direct(Box<OpenedDirectCorpus<'a>>),
}

/// Options governing how an [`OpenedCorpus`] opens its sentence stream.
pub(crate) struct CorpusSourceSpec<'a> {
    pub(crate) projection: BoundProjection,
    pub(crate) scope_clause: Option<&'a ScopeClause>,
    pub(crate) metadata_projection: Option<SourceMetadataProjection>,
    pub(crate) lookaround_divisions: Vec<compact_str::CompactString>,
    pub(crate) char_offsets: bool,
    pub(crate) block_sample: Option<CorpusSample>,
    pub(crate) sentence_filter: Option<&'a SentenceFilter>,
    pub(crate) needs_provenance: bool,
    pub(crate) decode_pool: Option<Arc<ThreadPool>>,
    pub(crate) omit_oversized_prepared: bool,
    pub(crate) skip_oversized_direct_markdown: bool,
    pub(crate) early_open_prepared_source: bool,
    pub(crate) attach_cancellation: bool,
}

impl CorpusSourceSpec<'_> {
    pub(crate) fn token_stream(projection: BoundProjection) -> Self {
        Self {
            projection,
            scope_clause: None,
            metadata_projection: None,
            lookaround_divisions: Vec::new(),
            char_offsets: true,
            block_sample: None,
            sentence_filter: None,
            needs_provenance: false,
            decode_pool: None,
            omit_oversized_prepared: false,
            skip_oversized_direct_markdown: false,
            early_open_prepared_source: false,
            attach_cancellation: false,
        }
    }
}

/// A fully opened direct or prepared sentence source and its run metadata.
pub(crate) struct ResolvedCorpusSource {
    pub(crate) source: BoxQuerySource,
    pub(crate) lifecycle: CompletionObservation,
    pub(crate) analysis: ResolvedAnalysis,
    pub(crate) schema: TokenSchema,
    pub(crate) run: QueryRunFacts,
    pub(crate) scope: QueryScope,
    pub(crate) provenance: Option<Arc<BlockingProvenance>>,
    pub(crate) is_prepared: bool,
    pub(crate) pretokenized_conllu: bool,
}

/// A schema-bound token stream for non-matching corpus commands (`tokenize`,
/// `attributes`).
pub(crate) struct BoundTokenStream {
    pub(crate) source: Box<dyn QuerySentenceSource>,
    pub(crate) lifecycle: CompletionObservation,
    pub(crate) schema: TokenSchema,
    pub(crate) run: QueryRunFacts,
    pub(crate) scope: QueryScope,
    pub(crate) is_prepared: bool,
    pub(crate) pretokenized_conllu: bool,
}

impl ResolvedCorpusSource {
    /// Wraps a direct native analyzer in [`NativeAnalysisSource`] so callers
    /// that read tokens directly (`tokenize`, `attributes`) always consume
    /// `ExecutionBatch::BoundTokenized`.
    pub(crate) fn into_bound_stream(self, projection: BoundProjection) -> BoundTokenStream {
        let source: Box<dyn QuerySentenceSource> = match self.analysis {
            ResolvedAnalysis::Direct(plan) => {
                let segmentation = plan.descriptor().segmentation();
                Box::new(NativeAnalysisSource::new(
                    self.source,
                    plan.create_session(),
                    projection,
                    segmentation,
                ))
            }
            ResolvedAnalysis::Tokenized(_) => self.source,
        };
        BoundTokenStream {
            source,
            lifecycle: self.lifecycle,
            schema: self.schema,
            run: self.run,
            scope: self.scope,
            is_prepared: self.is_prepared,
            pretokenized_conllu: self.pretokenized_conllu,
        }
    }
}

impl<'a> OpenedCorpus<'a> {
    pub(crate) fn open(
        input: &'a QueryInputArgs,
        selected_corpus: Option<&Path>,
        config: &ExecutionCliConfig,
        terminal: &TerminalArbiter,
    ) -> Result<Self, CliError> {
        if let Some(path) = selected_corpus {
            let artifact = CorpusArtifact::open(path)?;
            let schema = artifact.token_schema()?;
            let analyzer_name = crate::attribute_binding::analyzer_name(artifact.analyzer());
            Ok(Self::Prepared(Box::new(OpenedPreparedCorpus {
                artifact,
                schema,
                analyzer_name,
            })))
        } else {
            let root = input.root.as_deref();
            let stdin = root.is_none_or(|path| path == Path::new("-"));
            let format = resolve_input_format(if stdin { None } else { root }, input.input_format);
            let dict_path = (format != CorpusFormat::Conllu
                && matches!(&config.analyzer, AnalyzerCliConfig::Vibrato))
            .then(|| input.direct_dictionary_path())
            .transpose()?;
            let resolved = resolve_input(
                format,
                config,
                dict_path.as_deref(),
                terminal.cancellation(),
            )?;
            let analyzer_record = AnalyzerRecord::from_descriptor(resolved.descriptor());
            let analyzer_name = crate::attribute_binding::analyzer_name(&analyzer_record);
            Ok(Self::Direct(Box::new(OpenedDirectCorpus {
                input,
                root,
                stdin,
                format,
                dict_path,
                resolved,
                analyzer_record,
                analyzer_name,
            })))
        }
    }

    pub(crate) fn schema(&self) -> &TokenSchema {
        match self {
            Self::Prepared(prepared) => &prepared.schema,
            Self::Direct(direct) => direct.resolved.schema(),
        }
    }

    pub(crate) fn analyzer_name(&self) -> &str {
        match self {
            Self::Prepared(prepared) => &prepared.analyzer_name,
            Self::Direct(direct) => &direct.analyzer_name,
        }
    }

    pub(crate) fn is_prepared(&self) -> bool {
        matches!(self, Self::Prepared(_))
    }

    pub(crate) fn full_projection(&self) -> Result<BoundProjection, CliError> {
        self.schema()
            .bind_projection(self.schema().keys().iter().cloned())
            .map_err(|error| CliError::Parse(error.to_string()))
    }

    pub(crate) fn write_direct_binding_notice(
        &self,
        matcher: &CompiledCatalog,
        config: &ExecutionCliConfig,
    ) -> Result<(), CliError> {
        if let Self::Direct(direct) = self
            && let Some(dictionary_path) = direct.dict_path.as_ref().and_then(|path| path.to_str())
        {
            write_binding_notice(matcher, dictionary_path, config)?;
        }
        Ok(())
    }

    pub(crate) fn into_source(
        self,
        spec: CorpusSourceSpec<'_>,
        config: &ExecutionCliConfig,
        terminal: &TerminalArbiter,
    ) -> Result<ResolvedCorpusSource, CliError> {
        let mut scope = QueryScope::default();
        match self {
            Self::Prepared(prepared) => {
                let OpenedPreparedCorpus {
                    artifact, schema, ..
                } = *prepared;
                let provenance = if spec.needs_provenance
                    || spec.scope_clause.is_some()
                    || spec.metadata_projection.is_some()
                    || !spec.lookaround_divisions.is_empty()
                {
                    Some(artifact.provenance_reader()?)
                } else {
                    None
                };
                if let Some(reader) = provenance.as_deref()
                    && !spec.lookaround_divisions.is_empty()
                {
                    let observed: BTreeSet<String> = reader
                        .block_on(reader.view().division_observation_counts())?
                        .into_iter()
                        .filter(|(_, documents)| *documents > 0)
                        .map(|(kind, _)| kind)
                        .collect();
                    crate::attribute_binding::write_unobserved_division_notices(
                        &spec.lookaround_divisions,
                        |kind| observed.contains(kind),
                        config,
                        None,
                    )?;
                }
                let resolved_scope = spec
                    .scope_clause
                    .map(|clause| {
                        resolve_prepared_scope(
                            provenance
                                .as_deref()
                                .expect("scoped query opened provenance"),
                            clause,
                        )
                    })
                    .transpose()?;
                scope.prepared_scope = resolved_scope.map(|(resolution, labels)| {
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
                    let source = match &spec.metadata_projection {
                        Some(metadata) => artifact.bound_sentence_source_with_metadata(
                            &spec.projection,
                            metadata.clone(),
                            Arc::clone(
                                provenance
                                    .as_ref()
                                    .expect("metadata projection opened provenance"),
                            ),
                        )?,
                        None => match spec.sentence_filter {
                            Some(filter) => {
                                artifact.filtered_sentence_source(&spec.projection, filter)?
                            }
                            None => artifact.bound_sentence_source(&spec.projection)?,
                        },
                    };
                    let source = if spec.lookaround_divisions.is_empty() {
                        source
                    } else {
                        artifact.with_lookaround_divisions(
                            source,
                            spec.lookaround_divisions.clone(),
                            Arc::clone(
                                provenance
                                    .as_ref()
                                    .expect("lookaround divisions opened provenance"),
                            ),
                        )?
                    };
                    let source = if spec.char_offsets {
                        source
                    } else {
                        source.without_char_offsets()
                    };
                    let source = match &spec.decode_pool {
                        Some(pool) => source.with_decode_pool(Arc::clone(pool)),
                        None => source,
                    };
                    let source = source
                        .with_block_sample(spec.block_sample)
                        .with_admission(admission.clone());
                    let source = if spec.attach_cancellation {
                        source.with_cancellation(terminal.cancellation().clone())
                    } else {
                        source
                    };
                    Ok::<_, CliError>(source)
                };
                let early_source = if spec.early_open_prepared_source {
                    Some(open_source()?)
                } else {
                    None
                };
                let analysis = artifact.analysis_inputs()?;
                write_token_schema_notice(artifact.analyzer(), &analysis, config)?;
                let source_identity = encode_digest(artifact.source_identity().as_bytes());
                let analysis_identity = encode_digest(artifact.analysis_identity().as_bytes());
                let inventory = protocol_inventory(*artifact.inventory_stats());
                let traversal_limits =
                    protocol_traversal_limits_from_report(artifact.discovery_report());
                let analyzer_run = AnalyzerRun::new(
                    artifact.analyzer(),
                    artifact.trust_class().address_component(),
                    &schema,
                );
                let source = match early_source {
                    Some(source) => source,
                    None => open_source()?,
                };
                let source = if spec.omit_oversized_prepared {
                    source.with_tokenized_admission(TokenizedAdmission::Omit)
                } else {
                    source
                };
                let pretokenized_conllu = schema == lingrep_core::corpus::conllu::token_schema();
                Ok(ResolvedCorpusSource {
                    scope,
                    provenance,
                    source: Box::new(source),
                    lifecycle: CompletionObservation::Prepared {
                        source_identity,
                        analysis_identity,
                        inventory,
                    },
                    analysis: ResolvedAnalysis::Tokenized(schema.clone()),
                    schema,
                    run: QueryRunFacts {
                        analyzer: analyzer_run,
                        dictionary_identity: encode_digest(
                            artifact.dictionary_fingerprint()?.as_bytes(),
                        ),
                        reader_identity: encode_digest(analysis.reader().as_bytes()),
                        token_schema_identity: encode_digest(analysis.token_schema().as_bytes()),
                        traversal_limits,
                        block_sample_report: spec.block_sample.map(|sample| {
                            SampleReport::token_block(
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
                    is_prepared: true,
                    pretokenized_conllu,
                })
            }
            Self::Direct(direct) => {
                let OpenedDirectCorpus {
                    input,
                    root,
                    stdin,
                    format,
                    resolved,
                    analyzer_record,
                    ..
                } = *direct;
                let schema = resolved.schema().clone();
                let descriptor = resolved.descriptor().clone();
                let dictionary_digest = resolved.resource_identity();
                let analyzer_plan = resolved.into_native_plan();
                let analyzer_run = AnalyzerRun::new(&analyzer_record, "locally-executed", &schema);

                let (reader_config, policy) =
                    build_direct_reader_config(format, root, stdin, &input.markdown, config)?;
                if let Some(clause) = spec.scope_clause {
                    refuse_unanswerable(
                        clause,
                        format.supports_section_labels(),
                        &format!("{format:?} input").to_lowercase(),
                    )?;
                }
                scope.read_time_policy = Some(policy);
                let reader_identity =
                    ReaderIdentity::derive(&reader_config, config.reader_normalization(format));
                let analysis_inputs = AnalysisInputs::with_token_schema(
                    reader_identity,
                    admission_identity(),
                    &descriptor,
                    TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes()),
                )
                .map_err(CliError::corpus)?;
                let dictionary_identity = encode_digest(dictionary_digest);
                let reader_identity_text = encode_digest(reader_identity.as_bytes());
                let token_schema_identity =
                    encode_digest(analysis_inputs.token_schema().as_bytes());
                let (source, lifecycle): (BoxQuerySource, _) = if stdin {
                    let (observation, events) = crate::direct_source::stdin_events(
                        &reader_config,
                        config.limits.max_input_record_bytes,
                    );
                    let stream_id = random_id();
                    (
                        Box::new(
                            source_from_events(scoped(
                                AdmittedCorpus::new(events),
                                spec.scope_clause,
                            ))
                            .with_normalization(config.normalization)
                            .with_segmentation(config.segmentation)
                            .with_projection(spec.projection)
                            .with_source_metadata(spec.metadata_projection)
                            .with_lookaround_divisions(spec.lookaround_divisions),
                        ),
                        CompletionObservation::Stdin {
                            stream_id,
                            observation,
                        },
                    )
                } else {
                    let root = root.expect("filesystem root");
                    let (events, diagnostics) = FilesystemCorpusEvents::new(
                        root,
                        reader_config,
                        config.traversal.clone(),
                        config.limits.max_input_record_bytes,
                    )?;
                    scope.markdown_diagnostics = Some(diagnostics);
                    let admitted = scoped(admitted_filesystem_events(events), spec.scope_clause);
                    let source = if spec.skip_oversized_direct_markdown
                        && format == CorpusFormat::Markdown
                    {
                        source_from_events_skipping_oversized(admitted)
                    } else {
                        source_from_events(admitted)
                    }
                    .with_normalization(config.normalization)
                    .with_segmentation(config.segmentation)
                    .with_projection(spec.projection)
                    .with_source_metadata(spec.metadata_projection)
                    .with_lookaround_divisions(spec.lookaround_divisions);
                    (
                        Box::new(source),
                        CompletionObservation::Filesystem {
                            analysis: analysis_inputs,
                        },
                    )
                };
                Ok(ResolvedCorpusSource {
                    scope,
                    provenance: None,
                    source,
                    lifecycle,
                    analysis: analyzer_plan.map_or_else(
                        || ResolvedAnalysis::Tokenized(schema.clone()),
                        |plan| ResolvedAnalysis::Direct(Box::new(plan)),
                    ),
                    schema,
                    run: QueryRunFacts {
                        block_sample_report: None,
                        analyzer: analyzer_run,
                        dictionary_identity,
                        reader_identity: reader_identity_text,
                        token_schema_identity,
                        traversal_limits: protocol_traversal_limits(config),
                    },
                    is_prepared: false,
                    pretokenized_conllu: format == CorpusFormat::Conllu,
                })
            }
        }
    }
}

/// Typed specification for opening a matching-query session (`matches`,
/// `summarize`, `window_counts`).
pub(crate) struct MatchingQuerySpec<'a> {
    pub(crate) input: &'a QueryInputArgs,
    pub(crate) selected_corpus: Option<&'a Path>,
    pub(crate) compiled: &'a CompileOutput,
    pub(crate) projection: QueryProjection<'a>,
    pub(crate) scope_clause: Option<&'a ScopeClause>,
    pub(crate) mode: QueryMode,
    pub(crate) char_offsets: bool,
    pub(crate) metadata_projection: Option<SourceMetadataProjection>,
    pub(crate) block_sample: Option<CorpusSample>,
    pub(crate) allow_selective: bool,
    pub(crate) needs_provenance: bool,
}

/// Shared execution session for matching queries (`matches`, `summarize`,
/// `window_counts`), owning the caller-owned Rayon pool, bound catalog,
/// resolved corpus source, and run identity.
pub(crate) struct MatchingQuerySession {
    pub(crate) pool: Arc<ThreadPool>,
    pub(crate) run_id: String,
    pub(crate) source: BoxQuerySource,
    pub(crate) lifecycle: CompletionObservation,
    pub(crate) analysis: ResolvedAnalysis,
    pub(crate) bound: BoundCatalog,
    pub(crate) run: QueryRunFacts,
    pub(crate) scope: QueryScope,
    pub(crate) provenance: Option<Arc<BlockingProvenance>>,
}

pub(crate) enum MatchingRunOutcome {
    Cancelled,
    Completed(Box<CompletedMatchingQuery>),
}

pub(crate) struct CompletedMatchingQuery {
    pub(crate) lifecycle: CompletionObservation,
    pub(crate) metrics: QueryCompletionMetrics,
}

pub(crate) struct QueryCompletionMetrics {
    pub(crate) report: ExecutionReport,
    pub(crate) max_tokenized_bytes: usize,
    pub(crate) source_stats: QueryInputStats,
    pub(crate) scope: QueryScope,
    pub(crate) run_id: String,
}

impl MatchingQuerySession {
    pub(crate) fn open(
        spec: MatchingQuerySpec<'_>,
        config: &ExecutionCliConfig,
        terminal: &TerminalArbiter,
    ) -> Result<Self, CliError> {
        if spec.block_sample.is_some() && spec.selected_corpus.is_none() {
            return Err(CliError::InvalidArgs(
                "--sample-blocks requires --corpus".into(),
            ));
        }
        // Built before opening a prepared source so multi-block decode shares
        // the caller-owned pool instead of spawning its own.
        let pool = build_worker_pool(config.workers)?;
        let opened = OpenedCorpus::open(spec.input, spec.selected_corpus, config, terminal)?;
        opened.write_direct_binding_notice(&spec.compiled.matcher, config)?;
        let bound = spec.projection.bind(
            spec.compiled,
            opened.schema(),
            opened.analyzer_name(),
            config,
        )?;
        let division_kinds = spec.compiled.matcher.lookaround_division_kinds();
        let resolved = opened.into_source(
            CorpusSourceSpec {
                projection: bound.projection().clone(),
                scope_clause: spec.scope_clause,
                metadata_projection: spec.metadata_projection,
                lookaround_divisions: division_kinds,
                char_offsets: spec.char_offsets,
                block_sample: spec.block_sample,
                sentence_filter: bound.sentence_filter().filter(|_| spec.allow_selective),
                needs_provenance: spec.needs_provenance,
                decode_pool: Some(Arc::clone(&pool)),
                omit_oversized_prepared: true,
                skip_oversized_direct_markdown: spec.mode == QueryMode::Matches,
                early_open_prepared_source: spec.mode == QueryMode::Matches,
                attach_cancellation: true,
            },
            config,
            terminal,
        )?;
        Ok(Self {
            pool,
            run_id: random_id(),
            source: resolved.source,
            lifecycle: resolved.lifecycle,
            analysis: resolved.analysis,
            bound,
            run: resolved.run,
            scope: resolved.scope,
            provenance: resolved.provenance,
        })
    }

    /// Runs `execute_analyzed` over the session's bound catalog and corpus,
    /// tracking tokenized-batch high-water bytes and honoring scope admission.
    pub(crate) fn execute<S>(
        self,
        sentence_sample: Option<CorpusSample>,
        publication: impl Into<Publication>,
        config: &ExecutionCliConfig,
        terminal: &TerminalArbiter,
        sink: &mut S,
        progress: &mut impl FnMut(ExecutionProgress, &S),
    ) -> Result<MatchingRunOutcome, ExecutionError>
    where
        S: MatchSink,
    {
        let mut max_tokenized_bytes = 0usize;
        let mut observer = |observation: lingrep_core::execution::TokenizedBatchObservation| {
            max_tokenized_bytes = max_tokenized_bytes.max(observation.charged_bytes);
            Ok(())
        };
        // Analysis lanes, not one session: tokenization is the single-threaded
        // stage an uncached run is paced by, and the pool matching already
        // uses is idle while it runs. A prepared source analyzes nothing and
        // builds none of this.
        let binding = self.analysis.binding(config.workers, &self.pool);
        let admission = self
            .scope
            .prepared_scope
            .as_ref()
            .map_or(&SentenceAdmission::All, |resolved| &resolved.admission);
        let mut source = self.source;
        let outcome = execute_analyzed(
            SampledSentences::new(&mut source, sentence_sample),
            binding,
            &self.bound,
            &self.pool,
            &config.limits,
            config.limits.max_stage_sentences,
            terminal.cancellation(),
            sink,
            progress,
            &mut observer,
            publication,
            admission,
        )?;
        match outcome {
            RunOutcome::Cancelled(_) => Ok(MatchingRunOutcome::Cancelled),
            RunOutcome::Completed(report) => Ok(MatchingRunOutcome::Completed(Box::new(
                CompletedMatchingQuery {
                    lifecycle: self.lifecycle,
                    metrics: QueryCompletionMetrics {
                        report,
                        max_tokenized_bytes,
                        source_stats: source.into_stats(),
                        scope: self.scope,
                        run_id: self.run_id,
                    },
                },
            ))),
        }
    }
}

impl QueryCompletionMetrics {
    pub(crate) fn write_post_run_notices(
        &self,
        compiled: &CompileOutput,
        config: &ExecutionCliConfig,
        limited: bool,
        observed_span_kind: Option<(&str, bool)>,
        trace_policy: bool,
    ) -> Result<(), CliError> {
        crate::attribute_binding::write_out_of_set_label_notices(
            compiled.matcher.label_declarations(),
            &self.report.out_of_set_labels,
            config,
            &self.run_id,
        )?;
        if !limited {
            crate::attribute_binding::write_unobserved_span_kind_notices(
                &compiled.matcher,
                &self.report,
                observed_span_kind,
                config,
                &self.run_id,
            )?;
        }
        if !limited && let Some(observed) = self.source_stats.observed_division_kinds() {
            crate::attribute_binding::write_unobserved_division_notices(
                &compiled.matcher.lookaround_division_kinds(),
                |kind| observed.contains(kind),
                config,
                Some(&self.run_id),
            )?;
        }
        if trace_policy {
            crate::policy_report::write_requested(
                &config.stderr,
                compiled.matcher.catalog_policy(),
                self.report.policy_stats.captures_dropped,
                self.report.program_stats.per_operation(),
                self.report.trace.as_ref(),
            )?;
        }
        Ok(())
    }

    pub(crate) fn check_oversized_omissions(&self) -> Result<(), CliError> {
        if let Some(summary) = self.source_stats.oversized_sentence_omissions() {
            return Err(oversized_omissions_error(summary));
        }
        Ok(())
    }

    pub(crate) fn run_counters(
        &self,
        records_emitted: u64,
        records_unkeyed: u64,
        offsets_omitted: u64,
    ) -> Result<RunCounters, CliError> {
        let sentences_proven_absent = self.source_stats.sentences_proven_absent();
        let sentences_admitted = self
            .report
            .sentences_admitted
            .checked_add(sentences_proven_absent)
            .ok_or_else(|| CliError::Corpus("sentence admission counter overflow".into()))?;
        Ok(RunCounters {
            documents_admitted: self.source_stats.documents_admitted(),
            sentences_admitted,
            sentences_proven_absent,
            sentences_matched: self.report.sentences_matched,
            tokens_scanned: self.report.tokens_scanned,
            matches_found: self.report.matches_found,
            records_emitted,
            records_unkeyed,
            captures_dropped: self.report.policy_stats.captures_dropped,
            offsets_omitted,
        })
    }

    pub(crate) fn resource_high_water(
        &self,
        temporary_storage_bytes: u64,
        config: &ExecutionCliConfig,
    ) -> ResourceHighWater {
        ResourceHighWater {
            staged_raw_bytes: self
                .source_stats
                .max_staged_raw_bytes()
                .try_into()
                .unwrap_or(u64::MAX),
            tokenized_bytes: self.max_tokenized_bytes.try_into().unwrap_or(u64::MAX),
            temporary_storage_bytes,
            execution_limits: protocol_execution_limits(config),
            temporary_storage_limits: TemporaryStorageLimits {
                max_temp_bytes: config.max_temp_bytes,
            },
        }
    }
}
