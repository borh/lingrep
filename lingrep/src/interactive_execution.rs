//! Bounded direct/prepared execution shared by retained human workflows.

use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use lingrep_core::analysis::{AnalyzerLanes, VibratoPlan};
use lingrep_core::corpus::{
    AdmittedCorpus, CorpusReadConfig, resolve_input_format, stream_input_events,
};
use lingrep_core::dictionary::create_tokenizer_from_path;
#[cfg(test)]
use lingrep_core::dictionary::{create_tokenizer, load_dictionary_data};
#[cfg(test)]
use lingrep_core::execution::ExecutionLimits;
use lingrep_core::execution::{
    AnalysisBinding, CancellationFlag, ExecutionProgress, MatchSink, PolicySelection, RunOutcome,
    SentenceAdmission, execute_analyzed,
};
use lingrep_core::fsm::CompiledCatalog;
use lingrep_core::spill::TempBudget;
use lingrep_core::token_match::{TokenInfo, tokenize};
use lingrep_corpus::CorpusArtifact;
use rayon::ThreadPoolBuilder;
use vibrato_rkyv::Tokenizer;

use crate::cli::{ExecutionCliConfig, QueryInputArgs};
use crate::direct_source::{
    BoundedRecordReader, BoxSentenceSource, FilesystemCorpusEvents, admitted_filesystem_events,
    source_from_events,
};
use crate::error::{CliError, FailureCode};

enum InputSpec {
    Direct {
        root: Option<PathBuf>,
        tokenizer: Arc<Tokenizer>,
        reader_config: CorpusReadConfig,
    },
    Prepared {
        artifact: PathBuf,
    },
}

/// Reopenable execution inputs and immutable resource bounds for TUI and
/// TUI workers. Stdin is single-use.
pub(crate) struct InteractiveExecution {
    input: InputSpec,
    config: ExecutionCliConfig,
    temp_budget: Arc<TempBudget>,
    stdin_opened: AtomicBool,
}

impl InteractiveExecution {
    pub(crate) fn new(
        args: &QueryInputArgs,
        config: &ExecutionCliConfig,
    ) -> Result<Self, CliError> {
        if let Some(artifact) = &args.corpus {
            return Ok(Self::from_input(
                InputSpec::Prepared {
                    artifact: artifact.clone(),
                },
                config,
            ));
        }

        let dict_path = args.direct_dictionary_path()?;
        let dictionary_path = dict_path
            .to_str()
            .ok_or_else(|| CliError::Dictionary("dictionary path is not UTF-8".into()))?;
        let tokenizer = create_tokenizer_from_path(dictionary_path)
            .map_err(|error| CliError::Dictionary(error.to_string()))?;
        Self::direct_with_tokenizer(args, config, Arc::new(tokenizer))
    }

    fn direct_with_tokenizer(
        args: &QueryInputArgs,
        config: &ExecutionCliConfig,
        tokenizer: Arc<Tokenizer>,
    ) -> Result<Self, CliError> {
        let declared_stdin = args
            .root
            .as_ref()
            .is_some_and(|path| path.as_path() == Path::new("-"));
        let root = args
            .root
            .as_ref()
            .filter(|path| path.as_path() != Path::new("-"))
            .cloned();
        let format = resolve_input_format(root.as_deref(), args.input_format);
        crate::cli::validate_format_root(format, root.as_deref(), declared_stdin)
            .map_err(CliError::InvalidArgs)?;
        let markdown = args
            .markdown
            .resolve()
            .map_err(CliError::InvalidArgs)?
            .with_max_frontmatter_bytes(config.limits.max_stage_input_bytes);
        Ok(Self::from_input(
            InputSpec::Direct {
                root,
                tokenizer,
                reader_config: CorpusReadConfig::new(format, markdown),
            },
            config,
        ))
    }

    fn from_input(input: InputSpec, config: &ExecutionCliConfig) -> Self {
        Self {
            input,
            config: config.clone(),
            temp_budget: Arc::new(TempBudget::new(config.max_temp_bytes)),
            stdin_opened: AtomicBool::new(false),
        }
    }

    pub(crate) fn temp_budget(&self) -> Arc<TempBudget> {
        Arc::clone(&self.temp_budget)
    }

    pub(crate) fn is_reopenable(&self) -> bool {
        !matches!(self.input, InputSpec::Direct { root: None, .. })
    }

    /// Validate the selected source before an interactive caller mutates the
    /// terminal. Detail rescans still reopen it so each scan observes a fresh,
    /// canonical source.
    pub(crate) fn preflight(&self) -> Result<(), CliError> {
        if !self.is_reopenable() {
            return Err(CliError::InvalidArgs(
                "interactive execution requires a reopenable source".into(),
            ));
        }
        let _validated_source = self.open_source()?;
        Ok(())
    }

    pub(crate) fn tokenize_sample(&self, text: &str) -> Result<Vec<TokenInfo>, CliError> {
        match &self.input {
            InputSpec::Direct { tokenizer, .. } => Ok(tokenize(text, tokenizer)),
            InputSpec::Prepared { .. } => Err(CliError::InvalidArgs(
                "playground sample execution is unavailable: prepared artifacts do not retain a tokenizer"
                    .into(),
            )),
        }
    }

    pub(crate) fn execute<S>(
        &self,
        matcher: &CompiledCatalog,
        cancellation: &CancellationFlag,
        sink: &mut S,
        progress: &mut impl FnMut(ExecutionProgress, &S),
    ) -> Result<RunOutcome, CliError>
    where
        S: MatchSink,
    {
        let (source, tokenizer) = self.open_source()?;
        let pool = ThreadPoolBuilder::new()
            .num_threads(self.config.workers)
            .build()
            .map_err(|error| CliError::CommandFailure {
                code: FailureCode::Io,
                message: format!("failed to create worker pool: {error}"),
            })?;
        let limits = self.config.limits;
        // Named once and used twice: it binds the catalog, and — when there is
        // no tokenizer, which is how this surface says "prepared" — it is also
        // the schema the executor must project those stored tokens through.
        // The interactive surfaces analyze only with Vibrato over a packaged
        // UniDic dictionary, so one schema is a constant here and binds both
        // the catalog and prepared-token projection.
        let schema = lingrep_core::token_match::TokenSchema::unidic_morphology();
        // No unavailable-attribute notices here: the TUI owns the terminal,
        // and this surface's schema is that one constant anyway. A refusal
        // (dependency `where` clauses on a morphology schema) still reports.
        let bound = matcher
            .bind_with_projection(&schema, matcher.execution_attributes())
            .map_err(|error| {
                crate::attribute_binding::bind_refusal(&error, "vibrato morphology")
            })?;
        // Lanes rather than one session, for the same reason `matches` uses
        // them: this pool is idle while tokenization runs, and an interactive
        // surface is the one that feels it.
        let plan = tokenizer
            .as_ref()
            .map(|tokenizer| VibratoPlan::new(tokenizer.as_ref().clone()));
        let binding = plan.as_ref().map_or_else(
            || AnalysisBinding::Prepared(schema),
            |plan| AnalysisBinding::Session(AnalyzerLanes::new(plan, self.config.workers, &pool)),
        );
        execute_analyzed(
            source,
            binding,
            &bound,
            &pool,
            &limits,
            self.config.limits.max_stage_sentences,
            cancellation,
            sink,
            progress,
            &mut |_observation: lingrep_core::execution::TokenizedBatchObservation| Ok(()),
            PolicySelection::Apply,
            &SentenceAdmission::All,
        )
        .map_err(CliError::from)
    }

    fn open_source(&self) -> Result<(BoxSentenceSource, Option<Arc<Tokenizer>>), CliError> {
        match &self.input {
            InputSpec::Prepared { artifact } => {
                let artifact = CorpusArtifact::open(artifact)?;
                Ok((Box::new(artifact.sentence_source()?), None))
            }
            InputSpec::Direct {
                root: Some(root),
                tokenizer,
                reader_config,
            } => {
                let (events, _, _) = FilesystemCorpusEvents::new(
                    root,
                    reader_config.clone(),
                    self.config.traversal.clone(),
                    self.config.limits.max_input_record_bytes,
                )?;
                Ok((
                    Box::new(
                        source_from_events(admitted_filesystem_events(events))
                            .with_normalization(self.config.normalization),
                    ),
                    Some(Arc::clone(tokenizer)),
                ))
            }
            InputSpec::Direct {
                root: None,
                tokenizer,
                reader_config,
            } => {
                if self.stdin_opened.swap(true, Ordering::AcqRel) {
                    return Err(CliError::InvalidArgs(
                        "stdin cannot be reopened for another interactive scan".into(),
                    ));
                }
                let (reader, _) = BoundedRecordReader::new(
                    BufReader::new(io::stdin()),
                    self.config.limits.max_input_record_bytes,
                    "<stdin>",
                );
                let events: Box<
                    dyn Iterator<Item = io::Result<lingrep_core::corpus::CorpusEvent>> + Send,
                > = Box::new(stream_input_events(
                    crate::direct_source::input_format(reader_config),
                    reader,
                    "<stdin>".into(),
                    None,
                ));
                Ok((
                    Box::new(
                        source_from_events(AdmittedCorpus::new(events))
                            .with_normalization(self.config.normalization),
                    ),
                    Some(Arc::clone(tokenizer)),
                ))
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn direct_test_execution(text: &str) -> (tempfile::TempDir, Arc<InteractiveExecution>) {
    let directory = tempfile::tempdir().expect("temporary direct corpus");
    std::fs::write(directory.path().join("fixture.md"), text).expect("write direct corpus");
    let input = QueryInputArgs {
        dict_path: None,
        corpus: None,
        root: Some(directory.path().to_path_buf()),
        markdown: lingrep_cli_common::MarkdownReaderArgs::default(),
        input_format: None,
    };
    let config = ExecutionCliConfig {
        traversal: Default::default(),
        normalization: lingrep_core::analysis::NormalizationPolicy::Nfc,
        analyzer: crate::cli::AnalyzerCliConfig::Vibrato,
        profile: lingrep_core::analysis::ExtractionProfile::Morphology,
        segmentation: lingrep_core::analysis::SegmentationMode::Input,
        limits: ExecutionLimits::default(),
        max_temp_bytes: 1024 * 1024,
        workers: 1,
        progress: crate::cli::ProgressMode::Never,
        diagnostic_format: crate::cli::DiagnosticFormat::Human,
    };
    static TOKENIZER: OnceLock<Arc<Tokenizer>> = OnceLock::new();
    let tokenizer = Arc::clone(TOKENIZER.get_or_init(|| {
        let mini = lingrep_core::testing::MiniDictionary::new();
        let dictionary_path = mini
            .path()
            .to_str()
            .expect("miniature dictionary path must be UTF-8");
        let dictionary =
            load_dictionary_data(dictionary_path).expect("load miniature test dictionary");
        Arc::new(create_tokenizer(&dictionary).expect("create miniature test tokenizer"))
    }));
    let execution =
        InteractiveExecution::direct_with_tokenizer(&input, &config, tokenizer).unwrap();
    (directory, Arc::new(execution))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_test_fixtures_reuse_the_immutable_tokenizer() {
        let (_first_directory, first) = direct_test_execution("猫。\n");
        let (_second_directory, second) = direct_test_execution("犬。\n");

        let InputSpec::Direct {
            tokenizer: first_tokenizer,
            ..
        } = &first.input
        else {
            panic!("direct fixture must retain a tokenizer");
        };
        let InputSpec::Direct {
            tokenizer: second_tokenizer,
            ..
        } = &second.input
        else {
            panic!("direct fixture must retain a tokenizer");
        };

        assert!(Arc::ptr_eq(first_tokenizer, second_tokenizer));
    }

    #[test]
    fn playground_samples_use_direct_tokenizer_and_reject_prepared_mode() {
        let (_directory, execution) = direct_test_execution("猫。\n");
        let tokens = execution
            .tokenize_sample("猫。")
            .expect("direct sample tokenization");
        assert_eq!(tokens[0].orth(), Some("猫"));

        let mut prepared = Arc::try_unwrap(execution)
            .ok()
            .expect("sole execution owner");
        prepared.input = InputSpec::Prepared {
            artifact: PathBuf::from("unused.lingcorpus"),
        };
        let error = prepared
            .tokenize_sample("猫。")
            .expect_err("prepared sample tokenization must be unavailable");
        assert!(
            error
                .to_string()
                .contains("prepared artifacts do not retain a tokenizer"),
            "{error}"
        );
    }
}
