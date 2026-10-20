//! Live and stored corpus inventory commands.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use lingrep_cli_common::SafeText;
use lingrep_core::analysis::{
    AnalyzedInput, AnalyzerLanes, AnalyzerPlan, AnalyzerSession, InputUnitId, TextUnit,
};
use lingrep_core::corpus::{
    AdmittedCorpus, AdmittedCorpusEvent, CorpusDiscovery, CorpusFormat, CorpusReadConfig,
    DiscoveryError, DiscoveryReport, MarkdownReaderConfig, SourceDocument, resolve_input_format,
};
use lingrep_core::execution::{CancellationFlag, ExecutionError};
use lingrep_core::spill::TempBudget;
use lingrep_core::token_match::BoundProjection;
use lingrep_corpus::{
    AnalysisInputs, BuildConfig, BuildOutcome, CorpusArtifact, CorpusBuilder,
    DictionaryFingerprint, ObservedSources, ReaderIdentity, Realization, RealizationRequest,
    SiblingScan, SourceObservation, TokenSchemaIdentity, TrustClass, canonical_artifact_path,
    derive_analysis_identity,
};

use super::query_support::{
    execution_output_error, protocol_inventory, protocol_traversal_limits, random_id,
};
use super::{CommandOutcome, admission_identity};
use crate::analyzer_selection::resolve_input;
use crate::cli::{CorpusBuildArgs, CorpusInspectArgs, CorpusSourcesArgs, ExecutionCliConfig};
use crate::direct_source::{encode_digest, execution_discovery_error};
use crate::error::CliError;
use crate::output::{SourceTsvRow, write_source_tsv};
use crate::prepared_source::{PreparedBuildEvents, corpus_format_for_validated_root};
use crate::presentation::OutputMode;
use crate::protocol::query::{Coverage, Termination, TraversalLimits};
use crate::protocol::sources::{
    AggregateState, ArtifactVerification, ComponentVerification, SourceData, SourcesComplete,
    SourcesEnvelope, SourcesEvent, SourcesKind, SourcesRun, TraversalHighWater,
};
use crate::protocol::write_jsonl;
use crate::terminal::TerminalArbiter;

/// Stream the bounded ordered inventory admitted from a live filesystem root.
pub fn run_sources(
    args: &CorpusSourcesArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let stdout = io::stdout();
    run_sources_to(args, config, terminal, output, &mut stdout.lock())
}

/// Stream one admitted source into all immutable prepared artifact projections.
pub fn run_build(
    args: &CorpusBuildArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    if args.root == Path::new("-") {
        return Err(CliError::InvalidArgs(
            "corpus build requires a filesystem ROOT; stdin is unsupported".into(),
        ));
    }
    let markdown = args.markdown.resolve().map_err(CliError::InvalidArgs)?;
    let request = BuildRequest {
        root: args.root.clone(),
        output: args.output.clone(),
        dict_path: args.dict_path.clone(),
        markdown,
        input_format: args.input_format,
    };
    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;
    runtime.block_on(async {
        let mut progress = io::stderr();
        match ensure_artifact(
            &request,
            config,
            terminal,
            true,
            PublicationRole::RunCompletion,
            &mut progress,
        )
        .await?
        {
            None => Ok(CommandOutcome::Cancelled),
            Some(artifact) => report_artifact(artifact.into_path(), output).await,
        }
    })
}

/// Whether publishing the artifact is the run's own terminating act.
///
/// [`TerminalArbiter::try_claim_completion`](crate::terminal::TerminalArbiter::try_claim_completion)
/// is a one-shot latch: it exists to make completion and a signal mutually
/// exclusive, so exactly one of them decides the exit status. A run therefore
/// has one completion to claim, and it belongs to whatever the run's last act
/// is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationRole {
    /// `corpus build`: publishing the artifact *is* the run's completion.
    RunCompletion,
    /// A query preparing its own corpus: publishing is an intermediate commit
    /// and the run still owes an answer. Claiming the terminal here would
    /// spend the run's one completion on the artifact, leaving the answer with
    /// nothing to claim — which reports the query as cancelled, prints
    /// nothing, and exits non-zero.
    IntermediateCommit,
}

/// What one prepared-artifact build needs, independent of which command asked
/// for it.
///
/// `corpus build` and a query resolving its own corpus want the same artifact
/// under the same name, so they state the same request and reach the same
/// identity derivation. A second deriver is exactly the divergence that made
/// `lingrep-web` and `ling` name two artifacts for one Markdown tree.
pub(crate) struct BuildRequest {
    pub root: PathBuf,
    pub output: Option<PathBuf>,
    pub dict_path: Option<PathBuf>,
    pub markdown: MarkdownReaderConfig,
    /// The `--input-format` override. A directory defaults to Markdown unless
    /// this selects CoNLL-U or the BCCWJ distribution root, so it is part of
    /// the request's identity.
    pub input_format: Option<CorpusFormat>,
}

/// Resolves the prepared artifact for one request, building it when asked.
///
/// Returns the artifact and whether it was reused or published by this call.
/// `Ok(None)` means either the run was cancelled, or `create` was false and no
/// matching artifact exists — which the caller distinguishes by asking the
/// terminal, because only one of the two is a reason to stop.
pub(crate) async fn ensure_artifact(
    request: &BuildRequest,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    create: bool,
    role: PublicationRole,
    progress_sink: &mut impl io::Write,
) -> Result<Option<EnsuredArtifact>, CliError> {
    let args = request;
    let root = request.root.as_path();
    let validated = CorpusDiscovery::validate_root(root, config.traversal.clone())
        .map_err(|error| execution_discovery_error(&error))?;
    let format =
        corpus_format_for_validated_root(root, validated.is_directory(), args.input_format);
    crate::cli::validate_format_root(format, Some(root), false).map_err(CliError::InvalidArgs)?;
    let markdown = request
        .markdown
        .clone()
        .with_max_frontmatter_bytes(config.limits.max_stage_input_bytes);
    let reader_config = CorpusReadConfig::new(format, markdown);
    let reader = ReaderIdentity::derive(&reader_config, config.normalization);

    let vibrato_dictionary = request.dict_path.clone().or_else(|| {
        std::env::var_os("LINGREP_DICTIONARY_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let resolved_analyzer = resolve_input(format, config, vibrato_dictionary.as_deref())?;
    let schema = resolved_analyzer.schema().clone();
    let descriptor = resolved_analyzer.descriptor().clone();
    let dictionary_digest = resolved_analyzer.resource_identity();
    // The dictionary fingerprint is a recorded inspection fact, not an
    // identity input: the descriptor below already covers every resource that
    // reaches output, and keying on the fingerprint alone is what let two
    // Sudachi split modes address one artifact.
    let analysis = AnalysisInputs::with_token_schema(
        reader,
        admission_identity(),
        &descriptor,
        TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes()),
    )
    .map_err(|error| CliError::Corpus(error.to_string()))?;
    let analyzer_record = lingrep_corpus::AnalyzerRecord::from_descriptor(&descriptor);
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .map_err(|error| CliError::Corpus(error.to_string()))?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(config.workers)
        .build()
        .map_err(|error| CliError::Corpus(format!("failed to create worker pool: {error}")))?;
    let analyzer = resolved_analyzer.into_native_plan();
    // The caller's limit is a resource guard it chose; the analyzer's is a
    // property of the tokenizer and is not negotiable. Truncating to the
    // caller's alone leaves a window where a sentence passes it and then fails
    // inside the analyzer, and Sudachi's cap is *below* this workspace's
    // default, so that window is reachable rather than theoretical. Vibrato
    // declares none, as MeCab did not, and the minimum is then the caller's.
    let sentence_limit = analyzer
        .as_ref()
        .and_then(AnalyzerPlan::max_input_bytes)
        .map_or(config.limits.max_sentence_bytes, |analyzer_limit| {
            analyzer_limit.min(config.limits.max_sentence_bytes)
        });
    let mut session = analyzer
        .as_ref()
        .map(|analyzer| AnalyzerLanes::new(analyzer, config.workers, &pool));
    let resolved = resolve_destination(
        sentence_limit,
        args.output.as_deref(),
        root,
        config,
        &reader_config,
        terminal,
        &analysis,
        &analyzer_record,
        progress_sink,
    )?;
    let destination = match resolved {
        None => return Ok(None),
        // Nothing to build: an artifact of this exact identity already sits
        // beside the source.
        Some(Destination::Reuse(path)) => return Ok(Some(EnsuredArtifact::Reused(path))),
        Some(Destination::Fresh(path)) => path,
    };
    if !create {
        return Ok(None);
    }
    let mut builder = CorpusBuilder::begin_with_schema(
        BuildConfig {
            analysis,
            analyzer: analyzer_record,
            dictionary: DictionaryFingerprint::from_bytes(dictionary_digest),
            trust: TrustClass::LocallyExecuted,
            temp_budget: Arc::new(TempBudget::new(config.max_temp_bytes)),
            sentence_metadata_keys: reader_config
                .format()
                .sentence_metadata_keys()
                .iter()
                .map(|key| (*key).to_owned())
                .collect(),
        },
        &destination,
        schema,
    )
    .await?;
    let observation = builder.source_observation();
    let (events, documents, report) = PreparedBuildEvents::new(
        validated,
        reader_config,
        config.limits.max_input_record_bytes,
        observation,
        &config.traversal,
    )
    .map_err(|error| execution_discovery_error(&error))?;
    let mut admitted = AdmittedCorpus::new(events);
    let mut active_physical = None;

    // Read a bounded window of events, analyze the whole window across lanes,
    // then replay it into the builder in exactly the order it was read.
    //
    // The two halves are separated because only one of them parallelizes:
    // analysis is per-sentence and independent, while every builder call is
    // ordered — a document must register before its sentences, and each
    // sentence's stored location is assigned as it is pushed. Buffering the
    // reader's *interleaving* rather than just its events is what keeps the
    // replay faithful: a drained source registers exactly where it did before.
    //
    // Bound metadata and structural events as well as text, including a run
    // of empty divisions. Shared active division metadata remains bounded by
    // admission independently of this window.
    let mut window: Vec<(Vec<SourceDocument>, Option<AdmittedCorpusEvent>)> = Vec::new();
    let mut truncated: Vec<TruncatedSentence> = Vec::new();
    let mut reached_end = false;
    while !reached_end {
        if terminal.cancellation().is_cancelled() {
            return Ok(None);
        }
        window.clear();
        let mut window_bytes = 0_usize;
        let mut window_sentences = 0_usize;
        while window_sentences < config.limits.max_stage_sentences
            && window.len() < config.limits.max_stage_sentences
            && window_bytes < config.limits.max_stage_input_bytes
        {
            let event = admitted.next().transpose().map_err(build_input_error)?;
            let drained = documents.drain()?;
            let metadata_bytes = match event.as_ref() {
                Some(AdmittedCorpusEvent::DocumentStart { metadata, .. }) => {
                    lingrep_core::corpus::divisions::validate_metadata(metadata)
                        .map_err(build_input_error)?
                }
                Some(AdmittedCorpusEvent::DivisionStart(division)) => {
                    lingrep_core::corpus::divisions::validate_metadata(&division.metadata)
                        .map_err(build_input_error)?
                }
                Some(AdmittedCorpusEvent::Sentence(sentence)) => {
                    lingrep_core::corpus::divisions::validate_metadata(&sentence.metadata)
                        .map_err(build_input_error)?
                        + sentence
                            .active_labels
                            .iter()
                            .map(|label| label.len() + std::mem::size_of::<String>())
                            .sum::<usize>()
                        + serde_json::to_vec(&sentence.structure.observations)
                            .map_err(|error| CliError::Parse(error.to_string()))?
                            .len()
                }
                _ => 0,
            };
            window_bytes = window_bytes
                .checked_add(metadata_bytes)
                .ok_or_else(|| CliError::Parse("build metadata byte count overflow".into()))?;
            if let Some(event) = event.as_ref().and_then(AdmittedCorpusEvent::sentence) {
                window_sentences += 1;
                window_bytes += event.text.len();
            }
            reached_end = event.is_none();
            window.push((drained, event));
            if reached_end {
                break;
            }
        }

        // Before analysis *and* before the replay, so both see one text. The
        // artifact stores the event's bytes and the tokens come from the
        // analyzed unit, and the provenance sidecar cross-checks that the two
        // agree — truncating only the analysis input fails that check, which is
        // the sidecar doing its job.
        let window_truncations = truncate_window_sentences(
            &mut window,
            active_physical.as_ref(),
            config,
            sentence_limit,
        )?;
        // Recorded before the pushes that carry the shortened text, so a build
        // interrupted mid-window cannot publish truncated sentences under a
        // manifest that claims none. The builder accumulates across windows.
        builder.record_sentence_truncation(window_truncations.len() as u64, sentence_limit as u64);
        truncated.extend(window_truncations);

        let analyzed = match session.as_mut() {
            Some(session) => analyze_window(
                &window,
                active_physical.as_ref(),
                session,
                &projection,
                config,
                sentence_limit,
            )?,
            None => Vec::new(),
        };
        let mut analyzed = analyzed.into_iter();
        for (drained, event) in window.drain(..) {
            for document in drained {
                builder.register_discovered_source(&document)?;
                active_physical = Some(document);
            }
            let Some(event) = event else {
                break;
            };
            if let AdmittedCorpusEvent::DocumentStart {
                id, source_name, ..
            } = &event
            {
                let physical = active_physical.as_ref().ok_or_else(|| {
                    CliError::Corpus("admitted document has no physical source".into())
                })?;
                builder.register_admitted_document_from_source(id.get(), source_name, physical)?;
            }
            let sentence = event.sentence();
            let analyzed_sentence = sentence
                .filter(|sentence| sentence.words.is_none())
                .and_then(|_| analyzed.next());
            let bound_words = sentence.and_then(|sentence| sentence.words.clone());
            let location = builder.push_corpus_event(&event).await?;
            if let (Some(location), Some(analyzed)) = (location.as_ref(), analyzed_sentence) {
                builder
                    .push_analyzed_sentence(location, analyzed, &projection)
                    .await?;
            }
            if let (Some(location), Some(words)) = (location.as_ref(), bound_words) {
                builder
                    .push_bound_sentence(location, words, &projection)
                    .await?;
            }
        }
    }
    if terminal.cancellation().is_cancelled() {
        return Ok(None);
    }
    // Before publication, and on stderr regardless of `--progress`: this is not
    // progress, it is a statement that the artifact about to be written is not
    // a faithful copy of its source.
    warn_truncated_sentences(&truncated, sentence_limit, config.limits.max_sentence_bytes)?;
    let discovery = report
        .lock()
        .map_err(|_| CliError::Corpus("prepared discovery report lock poisoned".into()))?
        .clone()
        .ok_or_else(|| CliError::Corpus("prepared discovery did not reach clean EOF".into()))?;
    let artifact = builder
        .finish_guarded(
            discovery,
            || !terminal.cancellation().is_cancelled(),
            || match role {
                PublicationRole::RunCompletion => terminal.try_claim_completion(),
                // Still the last cooperative cancellation checkpoint — the
                // no-replace rename must not begin on a cancelled run — but it
                // does not latch the terminal, because the answer has not been
                // delivered yet.
                PublicationRole::IntermediateCommit => !terminal.cancellation().is_cancelled(),
            },
        )
        .await?;
    if artifact.is_none() {
        return Ok(None);
    }
    Ok(Some(EnsuredArtifact::Prepared(destination)))
}

/// Whether automatic artifact resolution reused an existing artifact or
/// published a new one.
pub(crate) enum EnsuredArtifact {
    Reused(PathBuf),
    Prepared(PathBuf),
}

impl EnsuredArtifact {
    pub(crate) fn into_path(self) -> PathBuf {
        match self {
            Self::Reused(path) | Self::Prepared(path) => path,
        }
    }
}

/// Where one `corpus build` run will publish, decided before any tokenizing
/// work begins.
enum Destination {
    /// The run builds and publishes there.
    Fresh(PathBuf),
    /// The canonical path already holds an artifact of this exact identity,
    /// so the run has nothing left to do but report it.
    Reuse(PathBuf),
}

/// Resolves the publication destination, reusing a matching canonical artifact.
///
/// An explicit `--output` is taken as spelled and always [`Destination::Fresh`];
/// the commit-time no-replace rename remains the sole guarantee that an existing
/// artifact is never overwritten, and this function neither relaxes nor relies
/// on anything but it. An omitted `--output` names the canonical
/// content-addressed destination, which is only knowable once the source has
/// been observed — and, when that destination already exists, is answered
/// without a build.
///
/// `Ok(None)` means observation was cancelled before a canonical name could be
/// derived, as it does for [`observe_root`].
#[allow(clippy::too_many_arguments)]
fn resolve_destination(
    sentence_limit: usize,
    output: Option<&Path>,
    root: &Path,
    config: &ExecutionCliConfig,
    reader_config: &CorpusReadConfig,
    terminal: &TerminalArbiter,
    analysis: &AnalysisInputs,
    analyzer: &lingrep_corpus::AnalyzerRecord,
    progress: &mut impl std::io::Write,
) -> Result<Option<Destination>, CliError> {
    if let Some(path) = output {
        return Ok(Some(Destination::Fresh(path.to_path_buf())));
    }
    let Some(observed) = observe_root(root, config, reader_config, terminal.cancellation())? else {
        return Ok(None);
    };
    let analysis_identity = derive_analysis_identity(&observed, analysis);
    // `.`, `..` and `/` have no file name, and `canonical_artifact_path`
    // would nest the artifact inside the tree being scanned rather than
    // beside it. Resolve only those: a named root is passed through exactly
    // as spelled, which is also how lingrep-web names one, so wherever the two
    // agree on an artifact at all they agree on where it sits. (They agree
    // for a JSONL or plaintext root; for Markdown they do not, because
    // lingrep-web reads the dialect with conversion-artifact recovery and this
    // command does not, and that flag is inside `reader_tag`. Spelling the
    // path the same way is what keeps the sharing that does exist, and is
    // independent of the reader question.) A relative named root needs no
    // resolution either — `build corpus/` from `/data` names
    // `corpus.g5.….lingcorpus`, which is the very path `build /data/corpus`
    // names.
    let named = if root.file_name().is_some() {
        root.to_path_buf()
    } else {
        std::fs::canonicalize(root).map_err(CliError::Io)?
    };
    let destination =
        canonical_artifact_path(&named, &analysis_identity, TrustClass::LocallyExecuted);

    // Path existence is no longer the question. Re-keying analysis identity
    // re-addressed every artifact that already exists, so a corpus prepared
    // before the cutover — unchanged source, unchanged analyzer — answers to a
    // name this build would never derive. Asking "does the canonical path
    // exist" would rebuild it as a duplicate sibling. So the directory beside
    // the source is scanned once, and each candidate's own recorded identity
    // decides.
    let directory = lingrep_corpus::containing_directory(&destination);
    let report = SiblingScan::scan(directory).map_err(|error| {
        CliError::Corpus(format!("cannot scan for prepared realizations: {error}"))
    })?;
    for (path, error) in &report.diagnostics {
        // A damaged sibling is reported and stepped over. It must not decide
        // this build, and it must not hide the realizations beside it.
        writeln!(
            progress,
            "warning: ignoring unreadable artifact at {}: {error}",
            path.display()
        )
        .map_err(CliError::Io)?;
    }
    let request = RealizationRequest {
        source_identity: *observed.source_identity(),
        analysis,
        analyzer,
        trust: TrustClass::LocallyExecuted,
        destination: &destination,
    };
    match Realization::classify(&request, &report) {
        BuildOutcome::ExactReuse(path) => {
            // Identity says these are the same corpus, and for every artifact
            // that truncated nothing they are. A truncating build is the one
            // case where a resource limit changed the content, so the recorded
            // limit has to agree too — otherwise a run with a larger
            // `--max-sentence-bytes` silently receives the shorter corpus a
            // smaller one produced. Rebuilding is the conservative answer and
            // costs one build.
            if let Some(recorded) = report
                .realizations
                .iter()
                .find(|realization| realization.path == path)
                .and_then(|realization| realization.sentence_truncation)
                && recorded.limit != sentence_limit as u64
            {
                // Rebuilding is not available: identity does not carry the
                // limit, so the rebuilt corpus would want this very path, and
                // publication never overwrites. Refusing names the one thing
                // the operator has to decide — this artifact is a *different*
                // corpus wearing the right name, and only they can say which
                // one should keep it.
                return Err(CliError::Corpus(format!(
                    "the artifact at {} truncated {} sentence(s) at --max-sentence-bytes {}, \
                     and this run asks for {}, so it is a different corpus under the same \
                     identity; rerun with --max-sentence-bytes {}, or remove that artifact, \
                     or pass an explicit --output",
                    path.display(),
                    recorded.count,
                    recorded.limit,
                    sentence_limit,
                    recorded.limit
                )));
            }
            Ok(Some(Destination::Reuse(path)))
        }
        BuildOutcome::SiblingOf(siblings) => {
            for sibling in &siblings {
                writeln!(
                    progress,
                    "note: {} realizes the same source under a different analysis ({})",
                    sibling.path.display(),
                    describe_difference(sibling, analysis, analyzer)
                )
                .map_err(CliError::Io)?;
            }
            Ok(Some(Destination::Fresh(destination)))
        }
        BuildOutcome::Fresh => Ok(Some(Destination::Fresh(destination))),
        BuildOutcome::ContradictoryDestination(path) => {
            // The canonical name is a digest over everything below, so
            // whatever disagrees, the analysis identity disagrees. Naming the
            // specific field as well is what turns "some identity is wrong"
            // into something an operator can act on.
            let detail = report
                .realizations
                .iter()
                .find(|realization| realization.path == path)
                .map_or_else(
                    || "unreadable".to_owned(),
                    |realization| {
                        if realization.source_identity == *observed.source_identity() {
                            describe_difference(realization, analysis, analyzer)
                        } else {
                            "source content".to_owned()
                        }
                    },
                );
            Err(CliError::Corpus(format!(
                "the artifact at {} records a different analysis identity ({detail}); \
                 remove it or pass an explicit --output",
                path.display()
            )))
        }
    }
}

/// Names, deterministically, what distinguishes a sibling from this request.
///
/// Ordered so the report is reproducible over unchanged bytes, and phrased so
/// an identity-scheme difference is never reported as an analyzer difference:
/// the two artifacts may be the same analysis, addressed two ways.
fn describe_difference(
    sibling: &lingrep_corpus::SiblingRealization,
    analysis: &AnalysisInputs,
    analyzer: &lingrep_corpus::AnalyzerRecord,
) -> String {
    let mut differences = Vec::new();
    if sibling.reader != analysis.reader() {
        differences.push("reader");
    }
    if sibling.admission != analysis.admission() {
        differences.push("admission policy");
    }
    if sibling.analyzer.semantic_identity != analyzer.semantic_identity {
        differences.push("analyzer semantics");
    }
    if sibling.token_schema != analysis.token_schema() {
        differences.push("token schema");
    }
    if differences.is_empty() {
        // Everything semantic agrees, so the only thing left is how the
        // address was derived.
        return format!("identity derivation only: {:?}", sibling.scheme);
    }
    differences.join(", ")
}

/// Reports a published or reused artifact as `corpus inspect` reports it.
async fn report_artifact(
    artifact: PathBuf,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    inspect_artifact(
        &CorpusInspectArgs {
            verify: false,
            documents: false,
            metadata: false,
            artifact,
            plan: false,
        },
        &TerminalArbiter::new(),
        output,
    )
    .await
}

/// Analyzes every sentence in one read window, in window order.
///
/// Batch-shaped rather than sentence-shaped so a lane-parallel session has a
/// batch to spread; a window carrying no sentence analyzes to nothing and
/// dispatches nothing.
///
/// The two size limits are enforced exactly where they were, and in window
/// order, so the first oversized sentence is still the one reported. What did
/// move is *when*: a window's whole analysis now precedes any of its builder
/// pushes, so a failing sentence aborts the build with fewer of its
/// predecessors pushed. Nothing observes that difference — a failed build
/// publishes no artifact.
///
/// An **oversized sentence is truncated rather than refused**, and reported in
/// [`truncate_window_sentences`]. A build is the one place where refusing is
/// the worst of the three options: a corpus is bad data's natural habitat, one
/// runaway line in one document of a thousand should not cost the other 999,
/// and unlike a query there is no partial answer to fall back on. Truncating
/// rather than skipping keeps the sentence *present* — a prepared artifact
/// addresses sentences by ordinal, so dropping one silently renumbers
/// everything after it.
///
/// This is a real loss and is treated as one: the run warns, and the artifact
/// records the count and the limit so a later build cannot reuse a shortened
/// corpus under a larger limit. `matches` still skips-and-degrades and
/// `summarize` still refuses to publish a total, each for reasons recorded at
/// those call sites; this is a third answer to a third question, not a change
/// to either.
fn analyze_window(
    window: &[(Vec<SourceDocument>, Option<AdmittedCorpusEvent>)],
    active: Option<&SourceDocument>,
    session: &mut impl AnalyzerSession,
    projection: &BoundProjection,
    config: &ExecutionCliConfig,
    limit: usize,
) -> Result<Vec<AnalyzedInput>, CliError> {
    let mut inputs = Vec::new();
    // A corpus-global sentence ordinal locates nothing in a thousand-document
    // tree, and a truncation notice carries the document for exactly this.
    // The window is walked in order rather than filtered so the name is the
    // document the sentence actually came from; `active` seeds it, because a
    // window can open in the middle of a document it never saw begin.
    let mut source = active.map(|document| document.logical_name.clone());
    for (drained, event) in window {
        if let Some(document) = drained.last() {
            source = Some(document.logical_name.clone());
        }
        let Some(sentence) = event.as_ref().and_then(AdmittedCorpusEvent::sentence) else {
            continue;
        };
        let input = TextUnit::sentence(
            InputUnitId::new(sentence.id.get()),
            &sentence.text,
            config.normalization,
        )
        .map_err(|error| CliError::Corpus(error.to_string()))?;
        let actual = input.len();
        if actual > limit {
            // Unreachable: `truncate_window_sentences` ran over this window
            // first and shortened anything that did not fit. Kept as a refusal
            // rather than an assertion because the alternative to noticing here
            // is an artifact whose stored tokens exceed what it claims to bound.
            return Err(ExecutionError::SentenceTooLarge {
                ordinal: sentence.id.get(),
                actual,
                limit,
                context: source
                    .as_deref()
                    .map_or_else(String::new, |name| format!(" in {name}")),
            }
            .into());
        }
        inputs.push(input);
    }
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let analyzed = session
        .analyze_bounded_batch(inputs, projection)
        .map_err(|error| CliError::Corpus(error.to_string()))?
        .into_inputs()
        .into_vec();
    for analyzed in &analyzed {
        let sentence = analyzed
            .sentences()
            .first()
            .ok_or_else(|| CliError::Corpus("analyzer returned no sentence".into()))?;
        let actual = sentence
            .words()
            .resident_bytes()
            .map_err(|_| ExecutionError::ResourceAccounting)?;
        if actual > config.limits.max_tokenized_bytes {
            return Err(ExecutionError::TokenizedSentenceTooLarge {
                ordinal: analyzed.input().id().get(),
                actual,
                limit: config.limits.max_tokenized_bytes,
            }
            .into());
        }
    }
    Ok(analyzed)
}

/// A sentence that did not fit `--max-sentence-bytes` and was shortened.
struct TruncatedSentence {
    ordinal: u64,
    /// The document it came from, absent only when a window opened before any
    /// document had been drained — which the seeding in `analyze_window` makes
    /// unreachable in practice, and which is reported as unknown rather than
    /// guessed if it ever happens.
    source: Option<String>,
    original: usize,
    retained: usize,
}

/// Shortens every sentence in `window` that exceeds `max_sentence_bytes`,
/// returning what it shortened.
///
/// Truncation is measured on the **admitted** text — what `TextUnit` produces
/// after normalization — because that is what the limit bounds and what the
/// artifact stores, but it is *applied* to the event's raw text, because the
/// event is what the builder pushes. Normalization can lengthen a string, so
/// cutting to the limit does not always reach it; the loop shrinks the budget
/// by the overshoot until it does, and terminates because the budget strictly
/// decreases and zero always fits.
fn truncate_window_sentences(
    window: &mut [(Vec<SourceDocument>, Option<AdmittedCorpusEvent>)],
    active: Option<&SourceDocument>,
    config: &ExecutionCliConfig,
    limit: usize,
) -> Result<Vec<TruncatedSentence>, CliError> {
    let mut truncated = Vec::new();
    let mut source = active.map(|document| document.logical_name.clone());
    for (drained, event) in window.iter_mut() {
        if let Some(document) = drained.last() {
            source = Some(document.logical_name.clone());
        }
        let Some(AdmittedCorpusEvent::Sentence(sentence)) = event.as_mut() else {
            continue;
        };
        let id = InputUnitId::new(sentence.id.get());
        let admitted_len = |text: &str| -> Result<usize, CliError> {
            TextUnit::sentence(id, text, config.normalization)
                .map(|unit| unit.len())
                .map_err(|error| CliError::Corpus(error.to_string()))
        };
        let original = admitted_len(&sentence.text)?;
        if original <= limit {
            continue;
        }
        let mut budget = limit;
        let retained = loop {
            let keep = floor_char_boundary(&sentence.text, budget);
            // `get` rather than a slice expression: the workspace denies
            // `clippy::string_slice`, and the denial is the point — a
            // byte-indexed cut through a three-byte character panics, which is
            // exactly the failure this whole function exists to avoid.
            let prefix = sentence
                .text
                .get(..keep)
                .ok_or_else(|| CliError::Corpus("truncation split a character".into()))?;
            let candidate = admitted_len(prefix)?;
            if candidate <= limit || keep == 0 {
                sentence.text.truncate(keep);
                break candidate;
            }
            budget = budget.saturating_sub((candidate - limit).max(1));
        };
        truncated.push(TruncatedSentence {
            ordinal: sentence.id.get(),
            source: source.clone(),
            original,
            retained,
        });
    }
    Ok(truncated)
}

/// Reports every truncated sentence, or nothing at all when none was.
///
/// Every one is named rather than the first with a count, because each is a
/// document a researcher may want to look at, and a corpus with hundreds of
/// them is a corpus whose conversion needs re-running — which a wall of
/// warnings says better than a number does.
fn warn_truncated_sentences(
    truncated: &[TruncatedSentence],
    limit: usize,
    configured: usize,
) -> Result<(), CliError> {
    if truncated.is_empty() {
        return Ok(());
    }
    // Naming *which* limit bound the run matters: an operator who raises
    // `--max-sentence-bytes` and sees the same truncations would otherwise have
    // no way to learn that the analyzer, not the flag, is what is deciding.
    let bound = if limit < configured {
        " (the analyzer's own maximum, below the configured --max-sentence-bytes)"
    } else {
        " (--max-sentence-bytes)"
    };
    let stderr = std::io::stderr();
    let mut writer = stderr.lock();
    writeln!(
        writer,
        "warning[sentence_truncated]: {} sentence(s) exceeded {limit} bytes{bound} \
         and were shortened to fit; the artifact does not reproduce its source for these",
        truncated.len()
    )
    .map_err(CliError::Io)?;
    for sentence in truncated {
        writeln!(
            writer,
            "  sentence {} in {}: {} bytes retained of {}",
            sentence.ordinal,
            sentence
                .source
                .as_deref()
                .map_or("an unnamed document", |name| name),
            sentence.retained,
            sentence.original
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}

/// The largest index `<= limit` that splits `text` between characters.
///
/// `str::floor_char_boundary` is still unstable, and a truncation that lands
/// mid-codepoint would panic rather than shorten.
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    if limit >= text.len() {
        return text.len();
    }
    let mut index = limit;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Inspect a stored artifact without reopening any source root.
pub fn run_inspect(
    args: &CorpusInspectArgs,
    _config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;
    runtime.block_on(inspect_artifact(args, terminal, output))
}

async fn inspect_artifact(
    args: &CorpusInspectArgs,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let started = Instant::now();
    let artifact = CorpusArtifact::open(&args.artifact)?;
    if args.metadata && output == OutputMode::Tsv {
        return Err(CliError::InvalidArgs(
            "corpus inspect --metadata requires human or JSONL output".into(),
        ));
    }
    let verification = if args.verify {
        let report = artifact.verify_integrity().await?;
        Some(ArtifactVerification {
            components: report
                .components
                .into_iter()
                .map(|component| ComponentVerification {
                    name: component.name,
                    length_matches: component.length_matches,
                    blake3_matches: component.blake3_matches,
                })
                .collect(),
            compared_location_rows: report.compared_location_rows,
        })
    } else {
        None
    };
    let view = artifact.provenance_view().await?;
    let metadata = if args.metadata {
        Some(super::corpus_metadata::inspect_metadata(&view, artifact.document_count()).await?)
    } else {
        None
    };
    if terminal.cancellation().is_cancelled() {
        return Ok(CommandOutcome::Cancelled);
    }

    let analysis = artifact.analysis_inputs()?;
    let report = artifact.discovery_report().clone();
    let run_id = random_id();
    let post_run = (|| -> Result<CommandOutcome, CliError> {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        publish_projected(
            &mut writer,
            &SourcesEnvelope::new(SourcesEvent::Run(SourcesRun {
                run_id: run_id.clone(),
                source_kind: SourcesKind::Artifact,
                reader_identity: encode_digest(analysis.reader().as_bytes()),
                admission_policy_identity: encode_digest(analysis.admission().as_bytes()),
                traversal_limits: traversal_limits_from_report(&report),
                aggregate_state: AggregateState::Known {
                    inventory: protocol_inventory(report.inventory),
                },
            })),
            output,
        )?;
        if args.documents {
            for document in artifact.documents() {
                if terminal.cancellation().is_cancelled() {
                    return Ok(CommandOutcome::Cancelled);
                }
                let document = document?;
                publish_projected(
                    &mut writer,
                    &SourcesEnvelope::new(SourcesEvent::Source(SourceData {
                        ordinal: document.ordinal,
                        logical_name: document.logical_name,
                        target_group_ordinal: document.target_group_ordinal,
                        external_target: document.external_target,
                        target_multiplicity: Some(document.target_multiplicity),
                    })),
                    output,
                )?;
            }
        }
        if !terminal.try_claim_completion() {
            return Ok(CommandOutcome::Cancelled);
        }
        publish_projected(
            &mut writer,
            &SourcesEnvelope::new(SourcesEvent::Complete(SourcesComplete {
                coverage: Coverage::Full,
                inventory: protocol_inventory(report.inventory),
                traversal_high_water: TraversalHighWater {
                    max_depth: report.observed.max_depth as u64,
                    examined_entries: report.observed.examined_entries,
                    max_directory_entries: report.observed.max_directory_entries as u64,
                    index_bytes: report.observed.index_bytes as u64,
                },
                ordered_inventory_digest: encode_digest(report.ordered_inventory_digest),
                elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                termination: Termination::Eof,
                verification,
                metadata,
            })),
            output,
        )?;
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

fn traversal_limits_from_report(report: &DiscoveryReport) -> TraversalLimits {
    TraversalLimits {
        max_depth: report.configured_limits.max_depth as u64,
        max_source_entries: report.configured_limits.max_source_entries,
        max_directory_entries: report.configured_limits.max_directory_entries as u64,
        max_documents: report.configured_limits.max_documents,
        max_index_bytes: report.configured_limits.max_index_bytes as u64,
    }
}

/// Preflight source observation used to name a canonical artifact before the
/// build: consumes the bounded event stream once, tokenizing nothing. The
/// analysis identity — and therefore the destination — is only knowable after
/// the source has been observed, so this pass is the price of a
/// content-addressed default. `Ok(None)` means the run was cancelled.
///
/// The preflight registers nothing, but it must still drain the discovered-
/// document queue: `PreparedBuildEvents::next` pushes one entry per document
/// onto an unbounded queue, so leaving it unread grows it across the whole
/// traversal. The drain-then-break ordering matches `build_artifact`'s own
/// loop: a trailing document that yields no events is enqueued by the same
/// `next()` call that reports end of stream.
fn observe_root(
    root: &Path,
    config: &ExecutionCliConfig,
    reader_config: &CorpusReadConfig,
    cancellation: &CancellationFlag,
) -> Result<Option<ObservedSources>, CliError> {
    let validated = CorpusDiscovery::validate_root(root, config.traversal.clone())
        .map_err(|error| execution_discovery_error(&error))?;
    let observation = SourceObservation::new();
    let (mut events, documents, report) = PreparedBuildEvents::new(
        validated,
        reader_config.clone(),
        config.limits.max_input_record_bytes,
        observation.clone(),
        &config.traversal,
    )
    .map_err(|error| execution_discovery_error(&error))?;

    loop {
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        let event = events.next().transpose().map_err(build_input_error)?;
        documents.drain()?;
        if event.is_none() {
            break;
        }
    }

    let discovery = report
        .lock()
        .map_err(|_| CliError::Corpus("prepared discovery report lock poisoned".into()))?
        .clone()
        .ok_or_else(|| CliError::Corpus("preflight discovery did not reach clean EOF".into()))?;
    let observed = observation
        .finish()
        .map_err(|error| CliError::Corpus(error.to_string()))?;
    if observed.document_count() != discovery.inventory.documents {
        return Err(CliError::Corpus(
            "observed document count disagrees with the discovery inventory".into(),
        ));
    }
    Ok(Some(observed))
}

fn build_input_error(error: io::Error) -> CliError {
    if let Some(discovery) = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<DiscoveryError>())
    {
        return execution_discovery_error(discovery).into();
    }
    CliError::Corpus(error.to_string())
}

fn run_sources_to<W: Write>(
    args: &CorpusSourcesArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
    writer: &mut W,
) -> Result<CommandOutcome, CliError> {
    let validated = CorpusDiscovery::validate_root(&args.root, config.traversal.clone())
        .map_err(|error| execution_discovery_error(&error))?;
    let format = if validated.is_directory() {
        // A directory defaults to Markdown; an explicit override selects a
        // supported CoNLL-U tree or the BCCWJ distribution root.
        args.input_format.unwrap_or(CorpusFormat::Markdown)
    } else {
        resolve_input_format(Some(&args.root), args.input_format)
    };
    crate::cli::validate_format_root(format, Some(&args.root), false)
        .map_err(CliError::InvalidArgs)?;
    let reader_config = CorpusReadConfig::new(
        format,
        args.markdown.resolve().map_err(CliError::InvalidArgs)?,
    );
    let run_id = random_id();
    let run = SourcesEnvelope::new(SourcesEvent::Run(SourcesRun {
        run_id: run_id.clone(),
        source_kind: SourcesKind::Live,
        reader_identity: encode_digest(
            ReaderIdentity::derive(&reader_config, config.normalization).as_bytes(),
        ),
        admission_policy_identity: encode_digest(admission_identity().as_bytes()),
        traversal_limits: protocol_traversal_limits(config),
        aggregate_state: AggregateState::Pending,
    }));
    let started = Instant::now();

    let result = (|| {
        publish_projected(writer, &run, output)?;
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }

        if matches!(reader_config, CorpusReadConfig::Bccwj) {
            let discovery = BccwjSourcesDiscovery::new(&args.root, &config.traversal)
                .map_err(|error| execution_discovery_error(&error))?;
            return stream_discovery(discovery, terminal, writer, started, output);
        }
        let file_kind = reader_config
            .directory_file_kind()
            .unwrap_or(lingrep_core::corpus::CorpusFileKind::Markdown);
        let discovery = CorpusDiscovery::from_validated_root_for_kind(validated, file_kind)
            .map_err(|error| execution_discovery_error(&error))?;
        stream_discovery(
            GenericSourcesDiscovery(discovery),
            terminal,
            writer,
            started,
            output,
        )
    })();

    result.map_err(|error: CliError| error.with_run_id(run_id, None))
}

trait SourceDiscovery {
    fn next_document(&mut self) -> Result<Option<SourceDocument>, DiscoveryError>;
    fn finish(self) -> Result<DiscoveryReport, DiscoveryError>;
}

struct GenericSourcesDiscovery(CorpusDiscovery);

impl SourceDiscovery for GenericSourcesDiscovery {
    fn next_document(&mut self) -> Result<Option<SourceDocument>, DiscoveryError> {
        CorpusDiscovery::next_document(&mut self.0)
    }

    fn finish(self) -> Result<DiscoveryReport, DiscoveryError> {
        CorpusDiscovery::finish(self.0)
    }
}

/// The BCCWJ `corpus inventory` cursor: one `SourceData` per inventory
/// **zip**, byte-sorted. Context files are not sources — they are identity
/// inputs, reported nowhere here.
///
/// The unit is the zip because that is the unit this command's own
/// completion record counts, and the unit every other format uses:
/// `sources` lists what was discovered, not what was admitted from it, the
/// same way it lists a JSONL file rather than its records. Streaming one row
/// per zip *entry* while reporting `documents = <zip count>` made the
/// listing and the completion line disagree on a real distribution by four
/// orders of magnitude.
struct BccwjSourcesDiscovery(crate::bccwj_seam::BccwjCursor);

impl BccwjSourcesDiscovery {
    fn new(
        root: &std::path::Path,
        limits: &lingrep_core::corpus::TraversalLimits,
    ) -> Result<Self, DiscoveryError> {
        let inventory = lingrep_core::corpus::bccwj::BccwjInventory::discover(root, limits)
            .map_err(|source| DiscoveryError::IoAt {
                logical_path: root.to_path_buf(),
                source,
            })?;
        Ok(Self(crate::bccwj_seam::BccwjCursor::new(inventory.zips)))
    }
}

impl SourceDiscovery for BccwjSourcesDiscovery {
    fn next_document(&mut self) -> Result<Option<SourceDocument>, DiscoveryError> {
        Ok(self.0.next_document())
    }

    fn finish(self) -> Result<DiscoveryReport, DiscoveryError> {
        Ok(self.0.report())
    }
}

fn stream_discovery<W: Write, D: SourceDiscovery>(
    mut discovery: D,
    terminal: &TerminalArbiter,
    writer: &mut W,
    started: Instant,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    loop {
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
        let next = discovery.next_document();
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
        let next = next.map_err(|error| execution_discovery_error(&error))?;
        let Some(document) = next else {
            break;
        };
        if terminal.cancellation().is_cancelled() {
            return Ok(CommandOutcome::Cancelled);
        }
        publish_projected(
            writer,
            &SourcesEnvelope::new(SourcesEvent::Source(protocol_source(document))),
            output,
        )?;
    }
    let report = discovery
        .finish()
        .map_err(|error| execution_discovery_error(&error))?;
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    publish_projected(
        writer,
        &SourcesEnvelope::new(SourcesEvent::Complete(protocol_complete(
            report,
            started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        ))),
        output,
    )?;
    Ok(CommandOutcome::Success)
}

fn protocol_source(document: SourceDocument) -> SourceData {
    SourceData {
        ordinal: document.ordinal,
        logical_name: document.logical_name,
        target_group_ordinal: document.target_group_ordinal,
        external_target: document.external_target,
        target_multiplicity: None,
    }
}

fn protocol_complete(report: DiscoveryReport, elapsed_millis: u64) -> SourcesComplete {
    SourcesComplete {
        coverage: Coverage::Full,
        inventory: protocol_inventory(report.inventory),
        traversal_high_water: TraversalHighWater {
            max_depth: report.observed.max_depth as u64,
            examined_entries: report.observed.examined_entries,
            max_directory_entries: report.observed.max_directory_entries as u64,
            index_bytes: report.observed.index_bytes as u64,
        },
        ordered_inventory_digest: encode_digest(report.ordered_inventory_digest),
        elapsed_millis,
        termination: Termination::Eof,
        verification: None,
        metadata: None,
    }
}

fn publish<W: Write>(writer: &mut W, event: &SourcesEnvelope) -> Result<(), CliError> {
    write_jsonl(writer, event)
        .map_err(execution_output_error)
        .map_err(CliError::from)
}

fn publish_projected<W: Write>(
    writer: &mut W,
    event: &SourcesEnvelope,
    output: OutputMode,
) -> Result<(), CliError> {
    match output {
        OutputMode::Jsonl => publish(writer, event),
        OutputMode::Tsv => match &event.event {
            SourcesEvent::Source(source) => write_source_tsv(
                writer,
                &SourceTsvRow {
                    ordinal: source.ordinal,
                    logical_name: source.logical_name.clone(),
                    target_group_ordinal: source.target_group_ordinal,
                    external_target: source.external_target,
                    target_multiplicity: source.target_multiplicity,
                },
            )
            .map_err(execution_output_error)
            .map_err(CliError::from),
            SourcesEvent::Run(_) | SourcesEvent::Complete(_) => Ok(()),
        },
        OutputMode::Human => match &event.event {
            SourcesEvent::Run(run) => writeln!(
                writer,
                "corpus inventory ({})",
                match run.source_kind {
                    SourcesKind::Live => "live",
                    SourcesKind::Artifact => "recorded",
                }
            ),
            SourcesEvent::Source(source) => writeln!(
                writer,
                "{:>8}  {}{}",
                source.ordinal,
                SafeText::from_untrusted(&source.logical_name),
                if source.external_target {
                    "  [external]"
                } else {
                    ""
                }
            ),
            SourcesEvent::Complete(complete) => {
                if let Some(metadata) = &complete.metadata {
                    super::corpus_metadata::write_metadata(writer, metadata)?;
                }
                writeln!(
                    writer,
                    "complete: {} document(s), {} distinct target(s), {} ms",
                    complete.inventory.documents,
                    complete.inventory.distinct_targets,
                    complete.elapsed_millis
                )
            }
        }
        .map_err(CliError::Io),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::cli::Cli;

    struct CancelsDuringDiscovery {
        terminal: TerminalArbiter,
        signal: i32,
        result: Option<Result<Option<SourceDocument>, DiscoveryError>>,
    }

    impl SourceDiscovery for CancelsDuringDiscovery {
        #[expect(
            clippy::panic_in_result_fn,
            reason = "fixture: cancellation must already be latched before the next pull"
        )]
        fn next_document(&mut self) -> Result<Option<SourceDocument>, DiscoveryError> {
            assert!(self.terminal.try_cancel(self.signal));
            self.result.take().unwrap()
        }

        fn finish(self) -> Result<DiscoveryReport, DiscoveryError> {
            #[expect(
                clippy::unreachable,
                reason = "cancellation must stop before discovery finish"
            )]
            {
                unreachable!("cancellation must stop before discovery finish")
            }
        }
    }

    #[test]
    fn preclaimed_cancellation_stops_after_the_run_boundary() {
        let directory = TempDir::new().unwrap();
        std::fs::write(directory.path().join("a.md"), "a").unwrap();
        let cli = Cli::try_parse_from([
            "ling",
            "corpus",
            "inventory",
            directory.path().to_str().unwrap(),
        ])
        .unwrap();
        let config = cli.execution_config().unwrap();
        let args = cli.corpus_sources().unwrap();
        let terminal = TerminalArbiter::new();
        assert!(terminal.try_cancel(signal_hook::consts::SIGINT));
        let mut output = Vec::new();

        let outcome =
            run_sources_to(args, &config, &terminal, OutputMode::Jsonl, &mut output).unwrap();

        assert_eq!(outcome, CommandOutcome::Cancelled);
        let events = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "run");
    }

    #[test]
    fn cancellation_claimed_during_discovery_does_not_publish_the_candidate_source() {
        for (signal, expected_exit) in [
            (signal_hook::consts::SIGINT, 130),
            (signal_hook::consts::SIGTERM, 143),
        ] {
            let terminal = TerminalArbiter::new();
            let discovery = CancelsDuringDiscovery {
                terminal: terminal.clone(),
                signal,
                result: Some(Ok(Some(SourceDocument {
                    ordinal: 1,
                    logical_name: "candidate.md".into(),
                    path: PathBuf::from("candidate.md"),
                    target_group_ordinal: 1,
                    external_target: false,
                }))),
            };
            let mut output = Vec::new();
            publish(
                &mut output,
                &SourcesEnvelope::new(SourcesEvent::Source(SourceData {
                    ordinal: 0,
                    logical_name: "committed.md".into(),
                    target_group_ordinal: 0,
                    external_target: false,
                    target_multiplicity: None,
                })),
            )
            .unwrap();

            let outcome = stream_discovery(
                discovery,
                &terminal,
                &mut output,
                Instant::now(),
                OutputMode::Jsonl,
            )
            .unwrap();

            assert_eq!(outcome, CommandOutcome::Cancelled);
            assert_eq!(terminal.winning_signal(), Some(signal));
            assert_eq!(
                crate::error::exit_code(crate::error::termination_for_signal(signal).unwrap()),
                expected_exit
            );
            let events = output
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["data"]["logical_name"], "committed.md");
        }
    }

    #[test]
    fn cancellation_claimed_during_failing_discovery_wins_over_the_error() {
        let terminal = TerminalArbiter::new();
        let discovery = CancelsDuringDiscovery {
            terminal: terminal.clone(),
            signal: signal_hook::consts::SIGTERM,
            result: Some(Err(DiscoveryError::IoAt {
                logical_path: PathBuf::from("candidate.md"),
                source: io::Error::other("simultaneous discovery failure"),
            })),
        };
        let mut output = Vec::new();

        let outcome = stream_discovery(
            discovery,
            &terminal,
            &mut output,
            Instant::now(),
            OutputMode::Jsonl,
        )
        .unwrap();

        assert_eq!(outcome, CommandOutcome::Cancelled);
        assert_eq!(
            terminal.winning_signal(),
            Some(signal_hook::consts::SIGTERM)
        );
        assert_eq!(
            crate::error::exit_code(
                crate::error::termination_for_signal(signal_hook::consts::SIGTERM).unwrap()
            ),
            143
        );
        assert!(output.is_empty());
    }
}
