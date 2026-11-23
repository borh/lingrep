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
    AdmittedCorpus, CorpusFormat, CorpusReadConfig, conllu, resolve_input_format,
    stream_input_events,
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
use lingrep_core::fsm::{BoundCatalog, CompiledCatalog};
use lingrep_core::spill::TempBudget;
use lingrep_core::token_match::{
    BoundProjection, TokenInfo, TokenSchema, display_surface_attributes,
};
use lingrep_corpus::CorpusArtifact;

use crate::cli::{ExecutionCliConfig, QueryInputArgs};
use crate::commands::query_resolution::build_direct_reader_config;
use crate::commands::query_support::build_worker_pool;
use crate::direct_source::{
    BoundedRecordReader, BoxSentenceSource, FilesystemCorpusEvents, admitted_filesystem_events,
    source_from_events,
};
use crate::error::CliError;

/// The format of a direct root, with stdin (`-`) treated as no path.
fn direct_format(args: &QueryInputArgs) -> CorpusFormat {
    let root = args.root.as_deref().filter(|path| *path != Path::new("-"));
    resolve_input_format(root, args.input_format)
}

enum InputSpec {
    Direct {
        root: Option<PathBuf>,
        analysis: DirectAnalysis,
        reader_config: CorpusReadConfig,
    },
    Prepared {
        artifact: PathBuf,
    },
}

enum DirectAnalysis {
    /// Built once, so every execution and sample shares one
    /// protected-surface table.
    Vibrato(Arc<VibratoPlan>),
    /// The source supplies its own tokens in this schema, as CoNLL-U does.
    Pretokenized(TokenSchema),
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

        if direct_format(args) == CorpusFormat::Conllu {
            return Self::direct(
                args,
                config,
                DirectAnalysis::Pretokenized(conllu::token_schema()),
            );
        }
        let dict_path = args.direct_dictionary_path()?;
        let dictionary_path = dict_path
            .to_str()
            .ok_or_else(|| CliError::Dictionary("dictionary path is not UTF-8".into()))?;
        let tokenizer = create_tokenizer_from_path(dictionary_path)
            .map_err(|error| CliError::Dictionary(error.to_string()))?;
        let plan = VibratoPlan::new(tokenizer, &dict_path)
            .map_err(|error| CliError::Dictionary(format!("{error}: {}", error.source)))?;
        Self::direct(args, config, DirectAnalysis::Vibrato(Arc::new(plan)))
    }

    fn direct(
        args: &QueryInputArgs,
        config: &ExecutionCliConfig,
        analysis: DirectAnalysis,
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
        let (reader_config, _) = build_direct_reader_config(
            direct_format(args),
            root.as_deref(),
            declared_stdin,
            &args.markdown,
            config,
        )?;
        Ok(Self::from_input(
            InputSpec::Direct {
                root,
                analysis,
                reader_config,
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
        match &self.input {
            InputSpec::Prepared { artifact } => {
                self.open_prepared(artifact, Vec::new(), None)?;
            }
            InputSpec::Direct { .. } => {
                self.open_direct(Vec::new(), None)?;
            }
        }
        Ok(())
    }

    pub(crate) fn tokenize_sample(&self, text: &str) -> Result<Vec<TokenInfo>, CliError> {
        match &self.input {
            InputSpec::Direct {
                analysis: DirectAnalysis::Vibrato(plan),
                ..
            } => Ok(plan.tokenize(text)),
            InputSpec::Direct {
                analysis: DirectAnalysis::Pretokenized(_),
                ..
            } => Err(CliError::InvalidArgs(
                "playground sample execution is unavailable: CoNLL-U input is pretokenized and supplies no tokenizer"
                    .into(),
            )),
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
        let pool = build_worker_pool(self.config.workers)?;
        let division_kinds = matcher.lookaround_division_kinds();
        // A prepared artifact is opened first because the display attributes
        // depend on its stored schema: one written before `surface` existed
        // does not carry it. A direct source is opened second because its
        // pretokenized rows are projected through the bound catalog.
        let (source, schema, bound, plan) = match &self.input {
            InputSpec::Prepared { artifact } => {
                let (source, stored_schema) =
                    self.open_prepared(artifact, division_kinds, Some(Arc::clone(&pool)))?;
                // Stored tokens are bound as UniDic morphology whichever
                // analyzer wrote the artifact.
                let schema = TokenSchema::unidic_morphology();
                let bound = bind(matcher, &schema, &stored_schema, "vibrato morphology")?;
                (source, schema, bound, None)
            }
            InputSpec::Direct { analysis, .. } => {
                let (schema, name, plan) = match analysis {
                    DirectAnalysis::Vibrato(plan) => (
                        TokenSchema::unidic_morphology(),
                        "vibrato morphology".to_owned(),
                        Some(plan.as_ref()),
                    ),
                    DirectAnalysis::Pretokenized(schema) => (
                        schema.clone(),
                        crate::attribute_binding::analyzer_name(
                            &lingrep_corpus::AnalyzerRecord::from_descriptor(
                                &conllu::analyzer_descriptor(),
                            ),
                        ),
                        None,
                    ),
                };
                let bound = bind(matcher, &schema, &schema, &name)?;
                let source = self.open_direct(division_kinds, Some(bound.projection().clone()))?;
                (source, schema, bound, plan)
            }
        };
        let limits = self.config.limits;
        // Lanes rather than one session, for the same reason `matches` uses
        // them: this pool is idle while tokenization runs, and an interactive
        // surface is the one that feels it.
        let binding = plan.map_or_else(
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

    fn open_prepared(
        &self,
        artifact: &Path,
        division_kinds: Vec<compact_str::CompactString>,
        decode_pool: Option<Arc<rayon::ThreadPool>>,
    ) -> Result<(BoxSentenceSource, TokenSchema), CliError> {
        let artifact = CorpusArtifact::open(artifact)?;
        let stored_schema = artifact.token_schema()?;
        let source = artifact.sentence_source()?;
        let source = match decode_pool {
            Some(pool) => source.with_decode_pool(pool),
            None => source,
        };
        let source = if division_kinds.is_empty() {
            source
        } else {
            artifact.with_lookaround_divisions(
                source,
                division_kinds,
                artifact.provenance_reader()?,
            )?
        };
        Ok((Box::new(source), stored_schema))
    }

    /// Opens the direct source. `projection` selects the attributes kept from
    /// pretokenized rows; reading such a row without one is an error.
    fn open_direct(
        &self,
        division_kinds: Vec<compact_str::CompactString>,
        projection: Option<BoundProjection>,
    ) -> Result<BoxSentenceSource, CliError> {
        let InputSpec::Direct {
            root,
            reader_config,
            ..
        } = &self.input
        else {
            return Err(CliError::InvalidArgs(
                "a prepared corpus has no direct source".into(),
            ));
        };
        let Some(root) = root else {
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
            let source = source_from_events(AdmittedCorpus::new(events))
                .with_normalization(self.config.normalization)
                .with_lookaround_divisions(division_kinds);
            return Ok(match projection {
                Some(projection) => Box::new(source.with_projection(projection)),
                None => Box::new(source),
            });
        };
        let (events, _, _) = FilesystemCorpusEvents::new(
            root,
            reader_config.clone(),
            self.config.traversal.clone(),
            self.config.limits.max_input_record_bytes,
        )?;
        let source = source_from_events(admitted_filesystem_events(events))
            .with_normalization(self.config.normalization)
            .with_lookaround_divisions(division_kinds);
        Ok(match projection {
            Some(projection) => Box::new(source.with_projection(projection)),
            None => Box::new(source),
        })
    }
}

/// Binds `matcher` to `schema`, projecting the attributes it executes on and
/// the surface attributes `display_schema` carries for the rendered views.
fn bind(
    matcher: &CompiledCatalog,
    schema: &TokenSchema,
    display_schema: &TokenSchema,
    schema_name: &str,
) -> Result<BoundCatalog, CliError> {
    matcher
        .bind_with_projection(
            schema,
            matcher
                .execution_attributes()
                .into_iter()
                .chain(display_surface_attributes(display_schema)),
        )
        .map_err(|error| crate::attribute_binding::bind_refusal(&error, schema_name))
}

#[cfg(test)]
fn test_config() -> ExecutionCliConfig {
    ExecutionCliConfig {
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
        stderr: crate::terminal::RunStderr::new(std::io::stderr()),
    }
}

/// An execution over the tracked CoNLL-U fixture, opened the way `explore`
/// opens a root named on the command line.
#[cfg(test)]
pub(crate) fn conllu_test_execution() -> Arc<InteractiveExecution> {
    let input = QueryInputArgs {
        dict_path: None,
        corpus: None,
        root: Some(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/psycholinguistics.conllu"
        ))),
        markdown: lingrep_cli_common::MarkdownReaderArgs::default(),
        input_format: None,
    };
    Arc::new(InteractiveExecution::new(&input, &test_config()).expect("open the CoNLL-U fixture"))
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
    let config = test_config();
    static PLAN: OnceLock<Arc<VibratoPlan>> = OnceLock::new();
    let plan = Arc::clone(PLAN.get_or_init(|| {
        let mini = lingrep_core::testing::MiniDictionary::new();
        let dictionary_path = mini
            .path()
            .to_str()
            .expect("miniature dictionary path must be UTF-8");
        let dictionary =
            load_dictionary_data(dictionary_path).expect("load miniature test dictionary");
        Arc::new(
            VibratoPlan::new(
                create_tokenizer(&dictionary).expect("create miniature test tokenizer"),
                mini.path(),
            )
            .expect("the miniature dictionary lexicon enumerates"),
        )
    }));
    let execution =
        InteractiveExecution::direct(&input, &config, DirectAnalysis::Vibrato(plan)).unwrap();
    (directory, Arc::new(execution))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_test_fixtures_reuse_the_immutable_plan() {
        let (_first_directory, first) = direct_test_execution("猫。\n");
        let (_second_directory, second) = direct_test_execution("犬。\n");

        let InputSpec::Direct {
            analysis: DirectAnalysis::Vibrato(first_plan),
            ..
        } = &first.input
        else {
            panic!("direct fixture must retain a plan");
        };
        let InputSpec::Direct {
            analysis: DirectAnalysis::Vibrato(second_plan),
            ..
        } = &second.input
        else {
            panic!("direct fixture must retain a plan");
        };

        assert!(Arc::ptr_eq(first_plan, second_plan));
    }

    #[derive(Default)]
    struct Recorded(Vec<(String, u64)>);

    impl MatchSink for Recorded {
        fn push(
            &mut self,
            location: &lingrep_core::execution::SentenceLocation,
            _sentence: &dyn lingrep_core::token_match::TokenSequence,
            matches: &[lingrep_core::fsm::Match],
        ) -> Result<lingrep_core::execution::SinkControl, lingrep_core::execution::ExecutionError>
        {
            self.0.extend(matches.iter().map(|matched| {
                (
                    matched.pattern.to_string(),
                    location.document_sentence_ordinal,
                )
            }));
            Ok(lingrep_core::execution::SinkControl::Continue {
                records_emitted: matches.len(),
            })
        }

        fn finish_batch(&mut self) -> Result<(), lingrep_core::execution::ExecutionError> {
            Ok(())
        }
    }

    #[test]
    fn direct_scans_read_division_lookaround_windows() {
        let (_directory, execution) = direct_test_execution(
            "# First\n\n猫が鳥を見る。\n犬も見る。\n\n鳥が猫を見る。\n\n# Second\n\n犬が来る。\n",
        );
        let matcher = crate::tui::compile_catalog_source(
            "scoped.lx",
            "pattern section := [] $ followed_by([], within=section).\n\
             pattern paragraph := [] $ followed_by([], within=paragraph).",
        )
        .unwrap();
        let mut sink = Recorded::default();
        execution
            .execute(
                &matcher,
                &CancellationFlag::new(),
                &mut sink,
                &mut |_, _| {},
            )
            .unwrap();
        sink.0.sort();
        assert_eq!(
            sink.0,
            [
                ("paragraph".to_owned(), 0),
                ("section".to_owned(), 0),
                ("section".to_owned(), 1),
            ]
        );
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
    #[test]
    fn playground_samples_reject_pretokenized_input_by_name() {
        let error = conllu_test_execution()
            .tokenize_sample("猫。")
            .expect_err("CoNLL-U input supplies no tokenizer");
        assert!(
            error.to_string().contains("CoNLL-U input is pretokenized"),
            "{error}"
        );
    }
}
