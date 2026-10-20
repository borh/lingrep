//! Explicit bounded direct/stdin tokenization and `tokenize/v2` publication.

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use lingrep_core::analysis::AnalyzerPlan;
use lingrep_core::corpus::{AdmittedCorpus, CorpusReadConfig, resolve_input_format};
use lingrep_core::execution::{
    ExecutionBatch, ExecutionError, ExecutionLimits as MatcherLimits, SentenceBatchSource,
};
use lingrep_corpus::{AnalysisInputs, CorpusArtifact, ReaderIdentity, TokenSchemaIdentity};

use super::query_completion::{CompletionObservation, FilesystemCompletion};
use super::query_projection::token_evidence;
use super::query_support::{
    execution_output_error, execution_output_io, protocol_execution_limits, protocol_inventory,
    protocol_traversal_limits, protocol_traversal_limits_from_report, random_id,
    write_token_schema_notice,
};
use super::{CommandOutcome, admission_identity};
use crate::analyzer_selection::resolve_native;
use crate::cli::{ExecutionCliConfig, ProgressMode, TokenizeArgs};
use crate::direct_source::{
    FilesystemCorpusEvents, admitted_filesystem_events, boxed_direct_source, encode_digest,
    source_from_events,
};
use crate::error::CliError;
use crate::output::{TokenizeTsvRow, write_tokenize_tsv};
use crate::presentation::OutputMode;
use crate::protocol::query::{
    AnalyzerRun, CompleteCorpus, Coverage, PublishedProjection, ResourceHighWater, RunCorpus,
    TemporaryStorageLimits, Termination,
};
use crate::protocol::tokenize::{
    SentenceData, TokenizeComplete, TokenizeEnvelope, TokenizeEvent, TokenizeRun,
};
use crate::protocol::write_jsonl;
use crate::terminal::{PresentationProgress, TerminalArbiter};

#[derive(Debug)]
struct TokenizeCounts {
    sentences_emitted: u64,
    tokens_emitted: u64,
    max_tokenized_bytes: usize,
}

struct SentencePublisher<'a, W> {
    writer: &'a mut W,
    output: OutputMode,
    projection: &'a PublishedProjection,
    stream_id: Option<String>,
}

impl<W: Write> SentencePublisher<'_, W> {
    fn stream(
        &mut self,
        source: &mut impl SentenceBatchSource,
        limits: &MatcherLimits,
        terminal: &TerminalArbiter,
        mut progress: impl FnMut(u64, u64),
        invalid_batch_message: &str,
    ) -> Result<Option<TokenizeCounts>, ExecutionError> {
        let mut counts = TokenizeCounts {
            sentences_emitted: 0,
            tokens_emitted: 0,
            max_tokenized_bytes: 0,
        };
        loop {
            if terminal.cancellation().is_cancelled() {
                return Ok(None);
            }
            let batch = source.next_batch(limits);
            if terminal.cancellation().is_cancelled() {
                return Ok(None);
            }
            let Some(batch) = batch? else { break };
            let ExecutionBatch::BoundTokenized { sentences, charges } = batch else {
                return Err(ExecutionError::Source {
                    ordinal: counts.sentences_emitted,
                    message: invalid_batch_message.into(),
                });
            };
            for (sentence, tokenized_bytes) in sentences.into_iter().zip(charges) {
                if terminal.cancellation().is_cancelled() {
                    return Ok(None);
                }
                let token_count = u64::try_from(sentence.value.words().len())
                    .map_err(|_| ExecutionError::ResourceAccounting)?;
                publish_projection(
                    self.writer,
                    &TokenizeEnvelope::new(TokenizeEvent::Sentence(SentenceData {
                        corpus_sentence_ordinal: sentence.location.corpus_sentence_ordinal,
                        source_name: sentence.location.source_name.to_string(),
                        stream_id: self.stream_id.clone(),
                        document_ordinal: sentence.location.document_ordinal,
                        document_sentence_ordinal: sentence.location.document_sentence_ordinal,
                        tokens: token_evidence(
                            sentence.value.words(),
                            0..sentence.value.words().len(),
                            self.projection,
                        ),
                    })),
                    self.output,
                )?;
                counts.max_tokenized_bytes = counts.max_tokenized_bytes.max(tokenized_bytes);
                counts.sentences_emitted = counts
                    .sentences_emitted
                    .checked_add(1)
                    .ok_or(ExecutionError::ResourceAccounting)?;
                counts.tokens_emitted = counts
                    .tokens_emitted
                    .checked_add(token_count)
                    .ok_or(ExecutionError::ResourceAccounting)?;
                if terminal.cancellation().is_cancelled() {
                    return Ok(None);
                }
            }
            self.writer.flush().map_err(execution_output_io)?;
            progress(counts.sentences_emitted, counts.tokens_emitted);
        }
        Ok(Some(counts))
    }
}

pub fn run(
    args: &TokenizeArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let resolved_corpus = crate::corpus_mode::resolve(
        &args.input,
        &args.cache,
        &args.input.markdown,
        config,
        terminal,
        &mut io::stderr(),
    )?;
    if let Some(artifact) = resolved_corpus.corpus.as_deref() {
        return run_prepared(artifact, config, terminal, output);
    }

    let started = Instant::now();
    let presentation_progress = PresentationProgress::stderr();
    let dict_path = matches!(&config.analyzer, crate::cli::AnalyzerCliConfig::Vibrato)
        .then(|| args.input.direct_dictionary_path())
        .transpose()?;
    let resolved = resolve_native(config, dict_path.as_deref())?;
    let dictionary_digest = resolved.resource_identity;
    let analyzer_plan = resolved.plan;
    let analyzer_record =
        lingrep_corpus::AnalyzerRecord::from_descriptor(analyzer_plan.descriptor());
    let analyzer_schema = analyzer_plan.schema().clone();
    let bound_projection = analyzer_schema
        .bind_projection(analyzer_schema.keys().iter().cloned())
        .map_err(|error| CliError::Parse(error.to_string()))?;
    let analyzer_run = AnalyzerRun::new(&analyzer_record, "locally-executed", &analyzer_schema);
    let analyzer_segmentation = analyzer_plan.descriptor().segmentation();
    let analyzer_session = analyzer_plan.create_session();

    let root = args.input.root.as_deref();
    let stdin = root.is_none_or(|path| path == Path::new("-"));
    let format = resolve_input_format(if stdin { None } else { root }, args.input.input_format);
    crate::cli::validate_format_root(format, root, stdin).map_err(CliError::InvalidArgs)?;
    let markdown_config = args
        .input
        .markdown
        .resolve()
        .map_err(CliError::InvalidArgs)?
        .with_max_frontmatter_bytes(config.limits.max_stage_input_bytes);
    let reader_config = CorpusReadConfig::new(format, markdown_config);
    let reader_identity = ReaderIdentity::derive(&reader_config, config.normalization);
    let analysis_inputs = AnalysisInputs::with_token_schema(
        reader_identity,
        admission_identity(),
        analyzer_plan.descriptor(),
        TokenSchemaIdentity::from_bytes(*analyzer_schema.digest().as_bytes()),
    )
    .map_err(|error| CliError::Corpus(error.to_string()))?;
    let dictionary_identity = encode_digest(dictionary_digest);
    let reader_identity_text = encode_digest(reader_identity.as_bytes());
    let token_schema_identity = Arc::new(encode_digest(analysis_inputs.token_schema().as_bytes()));
    // `tokenize` publishes the tokenizer's own output, so every attribute is
    // in scope by definition. Built once for the selected schema.
    let full_projection = PublishedProjection::new(analyzer_schema.keys());

    let ((source, source_stats), completion_observation) = if stdin {
        let (observation, events) = crate::direct_source::stdin_events(
            &reader_config,
            config.limits.max_input_record_bytes,
        );
        let stream_id = random_id();
        (
            boxed_direct_source(Box::new(
                source_from_events(AdmittedCorpus::new(events))
                    .with_normalization(config.normalization)
                    .with_segmentation(config.segmentation),
            )),
            CompletionObservation::Stdin {
                stream_id,
                observation,
            },
        )
    } else {
        let root = root.expect("filesystem root");
        let (events, observation, _diagnostics) = FilesystemCorpusEvents::new(
            root,
            reader_config,
            config.traversal.clone(),
            config.limits.max_input_record_bytes,
        )?;
        (
            boxed_direct_source(Box::new(
                source_from_events(admitted_filesystem_events(events))
                    .with_normalization(config.normalization)
                    .with_segmentation(config.segmentation),
            )),
            CompletionObservation::Filesystem {
                observation,
                analysis: analysis_inputs.clone(),
            },
        )
    };
    let run_corpus = completion_observation.run_corpus();
    let mut source = lingrep_core::execution::NativeAnalysisSource::new(
        source,
        analyzer_session,
        bound_projection,
        analyzer_segmentation,
    );
    let limits = config.limits;
    limits.validate()?;
    let sentence_stream_id = match &run_corpus {
        RunCorpus::Stdin { stream_id } => Some(stream_id.clone()),
        RunCorpus::Prepared { .. } | RunCorpus::Direct { .. } => None,
    };

    let run_id = random_id();
    let run = TokenizeEnvelope::new(TokenizeEvent::Run(Box::new(TokenizeRun {
        run_id: run_id.clone(),
        analyzer: analyzer_run,
        corpus: run_corpus,
        dictionary_identity,
        reader_identity: reader_identity_text,
        token_schema_identity: token_schema_identity.as_ref().clone(),
        execution_limits: protocol_execution_limits(config),
        temporary_storage_limits: TemporaryStorageLimits {
            max_temp_bytes: config.max_temp_bytes,
        },
        traversal_limits: protocol_traversal_limits(config),
        // Direct tokenization is deliberately ordered and sentence-at-a-time
        // so its resident-token bound is exact. Report that effective
        // execution width rather than the shared CLI's requested pool size.
        worker_count: 1,
    })));

    let post_run = (|| -> Result<CommandOutcome, CliError> {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        publish_projection(&mut writer, &run, output)?;
        writer.flush().map_err(execution_output_io)?;

        let Some(TokenizeCounts {
            sentences_emitted,
            tokens_emitted,
            max_tokenized_bytes,
        }) = (SentencePublisher {
            writer: &mut writer,
            output,
            projection: &full_projection,
            stream_id: sentence_stream_id,
        })
        .stream(
            &mut source,
            &limits,
            terminal,
            |sentences_emitted, tokens_emitted| {
                if config.progress == ProgressMode::Always {
                    presentation_progress.write_line(&format!(
                        "tokenized {sentences_emitted} sentences, emitted {tokens_emitted} tokens"
                    ));
                }
            },
            "native tokenization source returned non-bound data",
        )?
        else {
            return Ok(CommandOutcome::Cancelled);
        };
        presentation_progress.finish().map_err(CliError::Io)?;

        let (complete_corpus, inventory) =
            completion_observation.complete(FilesystemCompletion::Full)?;

        if !terminal.try_claim_completion() {
            return Ok(CommandOutcome::Cancelled);
        }
        publish_projection(
            &mut writer,
            &TokenizeEnvelope::new(TokenizeEvent::Complete(TokenizeComplete {
                coverage: Coverage::Full,
                corpus: complete_corpus,
                sentences_emitted,
                tokens_emitted,
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
                inventory,
                termination: Termination::Eof,
            })),
            output,
        )?;
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

fn run_prepared(
    path: &Path,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let started = Instant::now();
    let presentation_progress = PresentationProgress::stderr();
    let artifact = CorpusArtifact::open(path)?;
    let analysis = artifact.analysis_inputs()?;
    write_token_schema_notice(artifact.analyzer(), analysis.token_schema().as_bytes())?;
    let source_identity = encode_digest(artifact.source_identity().as_bytes());
    let analysis_identity = encode_digest(artifact.analysis_identity().as_bytes());
    let inventory = protocol_inventory(*artifact.inventory_stats());
    let schema = artifact.token_schema()?;
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .map_err(|error| CliError::Parse(error.to_string()))?;
    let mut source = artifact.bound_sentence_source(&projection)?;
    let limits = MatcherLimits {
        // Prepared tokenization publishes one sentence at a time, matching the
        // direct path's exact resident-token boundary.
        max_stage_sentences: 1,
        ..config.limits
    };
    limits.validate()?;
    let token_schema_identity = Arc::new(encode_digest(analysis.token_schema().as_bytes()));
    // `tokenize` publishes the tokenizer's own output, so every attribute is
    // in scope by definition. Built once for the selected schema.
    let full_projection = PublishedProjection::new(schema.keys());
    let run_id = random_id();
    let run = TokenizeEnvelope::new(TokenizeEvent::Run(Box::new(TokenizeRun {
        run_id: run_id.clone(),
        analyzer: AnalyzerRun::new(
            artifact.analyzer(),
            artifact.trust_class().address_component(),
            &schema,
        ),
        corpus: RunCorpus::Prepared {
            source_identity: source_identity.clone(),
            analysis_identity: analysis_identity.clone(),
        },
        dictionary_identity: encode_digest(artifact.dictionary_fingerprint()?.as_bytes()),
        reader_identity: encode_digest(analysis.reader().as_bytes()),
        token_schema_identity: token_schema_identity.as_ref().clone(),
        execution_limits: crate::protocol::query::ExecutionLimits {
            max_sentence_bytes: limits.max_sentence_bytes as u64,
            max_stage_sentences: 1,
            max_stage_input_bytes: limits.max_stage_input_bytes as u64,
            max_tokenized_bytes: limits.max_tokenized_bytes as u64,
        },
        temporary_storage_limits: TemporaryStorageLimits {
            max_temp_bytes: config.max_temp_bytes,
        },
        traversal_limits: protocol_traversal_limits_from_report(artifact.discovery_report()),
        worker_count: 1,
    })));

    let post_run = (|| -> Result<CommandOutcome, CliError> {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        publish_projection(&mut writer, &run, output)?;
        writer.flush().map_err(execution_output_io)?;

        let Some(TokenizeCounts { sentences_emitted, tokens_emitted, max_tokenized_bytes }) =
            (SentencePublisher {
                writer: &mut writer,
                output,
                projection: &full_projection,
                stream_id: None,
            }).stream(&mut source, &limits, terminal, |sentences_emitted, tokens_emitted| {
                if config.progress == ProgressMode::Always {
                    presentation_progress.write_line(&format!(
                        "tokenized {sentences_emitted} prepared sentences, emitted {tokens_emitted} tokens"
                    ));
                }
            }, "prepared tokenization source returned raw text")?
        else { return Ok(CommandOutcome::Cancelled); };
        presentation_progress.finish().map_err(CliError::Io)?;
        if !terminal.try_claim_completion() {
            return Ok(CommandOutcome::Cancelled);
        }
        publish_projection(
            &mut writer,
            &TokenizeEnvelope::new(TokenizeEvent::Complete(TokenizeComplete {
                coverage: Coverage::Full,
                corpus: CompleteCorpus::Prepared {
                    source_identity,
                    analysis_identity,
                },
                sentences_emitted,
                tokens_emitted,
                elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                resources: ResourceHighWater {
                    staged_raw_bytes: 0,
                    tokenized_bytes: max_tokenized_bytes.try_into().unwrap_or(u64::MAX),
                    temporary_storage_bytes: 0,
                    execution_limits: crate::protocol::query::ExecutionLimits {
                        max_sentence_bytes: limits.max_sentence_bytes as u64,
                        max_stage_sentences: 1,
                        max_stage_input_bytes: limits.max_stage_input_bytes as u64,
                        max_tokenized_bytes: limits.max_tokenized_bytes as u64,
                    },
                    temporary_storage_limits: TemporaryStorageLimits {
                        max_temp_bytes: config.max_temp_bytes,
                    },
                },
                inventory: Some(inventory),
                termination: Termination::Eof,
            })),
            output,
        )?;
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

fn publish_jsonl<W: Write>(writer: &mut W, event: &TokenizeEnvelope) -> Result<(), ExecutionError> {
    write_jsonl(writer, event).map_err(execution_output_error)
}

fn publish_projection<W: Write>(
    writer: &mut W,
    envelope: &TokenizeEnvelope,
    output: OutputMode,
) -> Result<(), ExecutionError> {
    match output {
        OutputMode::Jsonl => publish_jsonl(writer, envelope),
        OutputMode::Human => match &envelope.event {
            TokenizeEvent::Run(_) => Ok(()),
            TokenizeEvent::Sentence(sentence) => {
                write!(writer, "sentence {}:", sentence.corpus_sentence_ordinal)
                    .map_err(execution_output_io)?;
                for token in &sentence.tokens {
                    // The portable surface key, not `orth`. `orth` is UniDic's
                    // name for this field and is in no published namespace, so
                    // the lookup missed on every token of every run and the
                    // fallback rendered whole corpora as `?`. Nothing caught
                    // it because no test read a rendered token.
                    //
                    // The sentinel is the one TSV already uses for an absent
                    // context surface, and it is lossy for the same reason:
                    // corpus text can contain that string. JSONL stays
                    // authoritative.
                    let surface = token.attributes.get("text").unwrap_or("⟨missing⟩");
                    write!(writer, " {}", crate::output::sanitize_terminal(surface))
                        .map_err(execution_output_io)?;
                }
                writeln!(writer).map_err(execution_output_io)
            }
            TokenizeEvent::Complete(complete) => writeln!(
                writer,
                "full coverage; {} sentences, {} tokens; completed in {} ms",
                complete.sentences_emitted, complete.tokens_emitted, complete.elapsed_millis
            )
            .map_err(execution_output_io),
        },
        OutputMode::Tsv => match &envelope.event {
            TokenizeEvent::Sentence(sentence) => {
                for token in &sentence.tokens {
                    let attribute =
                        |name| token.attributes.get(name).unwrap_or_default().to_owned();
                    write_tokenize_tsv(
                        writer,
                        &TokenizeTsvRow {
                            corpus_sentence_ordinal: sentence.corpus_sentence_ordinal,
                            source_name: sentence.source_name.clone(),
                            document_ordinal: sentence.document_ordinal,
                            document_sentence_ordinal: sentence.document_sentence_ordinal,
                            token_index: token.index,
                            // Published keys, not the UniDic/lingrep patterns-era spellings.
                            // `orth`, `pos` and `cform` are in no published
                            // namespace, so three of the four content columns
                            // were empty on every row of every run — the
                            // `unwrap_or_default()` turned a total miss into a
                            // well-formed record instead of a failure.
                            orth: attribute("text"),
                            lemma: attribute("lemma"),
                            pos: attribute("upos"),
                            cform: attribute("unidic.cform"),
                        },
                    )
                    .map_err(execution_output_error)?;
                }
                Ok(())
            }
            TokenizeEvent::Run(_) | TokenizeEvent::Complete(_) => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lingrep_core::corpus::DocumentId;
    use lingrep_core::execution::{
        BoundSentenceInput, EnhancedGraphInput, ExecutionSentence, SentenceLocation,
    };
    use lingrep_core::token_match::{BoundTokenSentence, TokenSchema};

    use super::*;
    use crate::error::{exit_code, termination_for_signal};

    struct TestSource<F>(F);

    impl<F> SentenceBatchSource for TestSource<F>
    where
        F: FnMut() -> Result<Option<ExecutionBatch>, ExecutionError>,
    {
        fn next_batch(
            &mut self,
            _: &MatcherLimits,
        ) -> Result<Option<ExecutionBatch>, ExecutionError> {
            (self.0)()
        }
        fn wants_lookahead(&self) -> bool {
            false
        }
    }

    fn batch() -> ExecutionBatch {
        let schema = TokenSchema::from_keys(["text"]).unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let sentences: Vec<_> = (0..2)
            .map(|ordinal| ExecutionSentence {
                location: SentenceLocation {
                    source_metadata: None,
                    corpus_sentence_ordinal: ordinal,
                    document_id: DocumentId::new(0),
                    document_ordinal: 0,
                    document_sentence_ordinal: ordinal,
                    source_name: Arc::from("fixture.txt"),
                },
                value: BoundSentenceInput::new(
                    BoundTokenSentence::from_projected_values(
                        &projection,
                        1,
                        vec![Some("猫".into())],
                    )
                    .unwrap(),
                    EnhancedGraphInput::NotRequested,
                ),
            })
            .collect();
        let charges = sentences
            .iter()
            .map(|sentence| sentence.value.resident_bytes().unwrap())
            .collect();
        ExecutionBatch::bound_tokenized_with_charges(sentences, charges)
    }

    fn publish(
        source: &mut impl SentenceBatchSource,
        terminal: &TerminalArbiter,
        writer: &mut impl Write,
    ) -> Result<Option<TokenizeCounts>, ExecutionError> {
        let schema = TokenSchema::from_keys(["text"]).unwrap();
        SentencePublisher {
            writer,
            output: OutputMode::Jsonl,
            projection: &PublishedProjection::new(schema.keys()),
            stream_id: Some("test-stream".into()),
        }
        .stream(
            source,
            &MatcherLimits::default(),
            terminal,
            |_, _| {},
            "invalid test batch",
        )
    }

    fn winning_exit_code(terminal: &TerminalArbiter) -> i32 {
        exit_code(termination_for_signal(terminal.winning_signal().unwrap()).unwrap())
    }

    #[test]
    fn cancellation_before_publication_does_not_pull_or_write() {
        let terminal = TerminalArbiter::new();
        assert!(terminal.try_cancel(signal_hook::consts::SIGTERM));
        let mut source = TestSource(|| panic!("cancelled execution must not pull input"));
        let mut output = Vec::new();
        assert!(
            publish(&mut source, &terminal, &mut output)
                .unwrap()
                .is_none()
        );
        assert!(output.is_empty());
        assert_eq!(winning_exit_code(&terminal), 143);
    }

    #[test]
    fn cancellation_after_analysis_discards_the_unpublished_batch_for_both_signals() {
        for (signal, expected_exit) in [
            (signal_hook::consts::SIGINT, 130),
            (signal_hook::consts::SIGTERM, 143),
        ] {
            let terminal = TerminalArbiter::new();
            let mut source = TestSource(|| {
                assert!(terminal.try_cancel(signal));
                Ok(Some(batch()))
            });
            let mut output = Vec::new();
            assert!(
                publish(&mut source, &terminal, &mut output)
                    .unwrap()
                    .is_none()
            );
            assert!(output.is_empty());
            assert_eq!(winning_exit_code(&terminal), expected_exit);
        }
    }

    struct CancellingWriter<'a> {
        terminal: &'a TerminalArbiter,
        output: Vec<u8>,
        fail: bool,
        signal: i32,
    }

    impl Write for CancellingWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.terminal.try_cancel(self.signal);
            if self.fail {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn cancellation_during_publication_commits_one_complete_record_and_skips_the_next() {
        let terminal = TerminalArbiter::new();
        let mut source = TestSource(|| Ok(Some(batch())));
        let mut writer = CancellingWriter {
            terminal: &terminal,
            output: Vec::new(),
            fail: false,
            signal: signal_hook::consts::SIGINT,
        };
        assert!(
            publish(&mut source, &terminal, &mut writer)
                .unwrap()
                .is_none()
        );
        assert_eq!(writer.output.last(), Some(&b'\n'));
        let envelope: TokenizeEnvelope = serde_json::from_slice(&writer.output).unwrap();
        let TokenizeEvent::Sentence(sentence) = envelope.event else {
            panic!("sentence record")
        };
        assert_eq!(sentence.corpus_sentence_ordinal, 0);
        assert_eq!(sentence.stream_id.as_deref(), Some("test-stream"));
        assert_eq!(sentence.tokens.len(), 1);
        assert_eq!(winning_exit_code(&terminal), 130);
    }

    #[test]
    fn output_failure_during_started_publication_wins_over_cancellation() {
        let terminal = TerminalArbiter::new();
        let mut source = TestSource(|| Ok(Some(batch())));
        let mut writer = CancellingWriter {
            terminal: &terminal,
            output: Vec::new(),
            fail: true,
            signal: signal_hook::consts::SIGTERM,
        };
        let error = publish(&mut source, &terminal, &mut writer).unwrap_err();
        assert!(matches!(
            error,
            ExecutionError::Output {
                kind: io::ErrorKind::BrokenPipe,
                ..
            }
        ));
        assert!(writer.output.is_empty());
        assert_eq!(exit_code(CliError::from(error).termination()), 1);
        assert_eq!(winning_exit_code(&terminal), 143);
    }

    #[test]
    fn cancellation_during_a_failed_source_pull_wins_over_the_source_error() {
        for (signal, expected_exit) in [
            (signal_hook::consts::SIGINT, 130),
            (signal_hook::consts::SIGTERM, 143),
        ] {
            let terminal = TerminalArbiter::new();
            let mut source = TestSource(|| {
                assert!(terminal.try_cancel(signal));
                Err(ExecutionError::Source {
                    ordinal: 7,
                    message: "simultaneous source failure".into(),
                })
            });
            let mut output = Vec::new();
            assert!(
                publish(&mut source, &terminal, &mut output)
                    .unwrap()
                    .is_none()
            );
            assert!(output.is_empty());
            assert_eq!(winning_exit_code(&terminal), expected_exit);
        }
    }
}
