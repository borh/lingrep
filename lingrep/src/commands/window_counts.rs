use std::io::{self, BufWriter, Write};
use std::num::NonZeroU64;

use lingrep_core::execution::{
    ExecutionError, ExecutionProgress, MatchSink, PolicySelection, Publication, RunOutcome,
    SentenceAdmission, SentenceLocation, SinkControl, execute_analyzed,
};
use lingrep_core::fsm::{CompileOutput, Match, PublishedRecord};
use lingrep_core::token_match::TokenSequence;

use super::CommandOutcome;
use super::query_resolution::{QueryProjection, ResolvedMatchingQuery, resolve_matching_query};
use super::query_support::{execution_output_error, execution_output_io, random_id};
use crate::cli::{ExecutionCliConfig, SummarizeArgs};
use crate::error::{CliError, FailureCode};
use crate::presentation::{OutputMode, ResultDelivery};
use crate::protocol::query::{QueryMode, RunCorpus};
use crate::protocol::window_counts::{
    WindowCountsComplete, WindowCountsData, WindowCountsEnvelope, WindowCountsEvent,
    WindowCountsRun,
};
use crate::protocol::write_jsonl;
use crate::terminal::{PresentationProgress, TerminalArbiter};

pub(super) fn run(
    args: &SummarizeArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    size: NonZeroU64,
    compiled: &CompileOutput,
    catalog_fingerprint: String,
    patterns: Vec<String>,
) -> Result<CommandOutcome, CliError> {
    if args.input.corpus.is_none() && !args.cache.cache {
        return Err(CliError::InvalidArgs(
            "--by-window requires a prepared corpus: pass --corpus or --cache".into(),
        ));
    }
    let corpus = crate::corpus_mode::resolve(
        &args.input,
        &args.cache,
        &args.input.markdown,
        config,
        terminal,
        OutputMode::Jsonl,
    )?;
    let Some(path) = corpus.corpus.as_deref() else {
        return Err(CliError::InvalidArgs(
            "--by-window requires a prepared corpus".into(),
        ));
    };
    let pool = std::sync::Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(config.workers)
            .build()
            .map_err(|error| CliError::CommandFailure {
                code: FailureCode::Io,
                message: format!("failed to create worker pool: {error}"),
            })?,
    );
    let ResolvedMatchingQuery {
        source,
        source_stats,
        lifecycle,
        analysis,
        bound,
        ..
    } = resolve_matching_query(
        &args.input,
        Some(path),
        compiled,
        QueryProjection {
            selected: &[],
            additional: &[],
            all: false,
        },
        None,
        config,
        terminal,
        QueryMode::Summarize,
        false,
        None,
        None,
        false,
        false,
        std::sync::Arc::clone(&pool),
    )?;
    let RunCorpus::Prepared {
        source_identity,
        analysis_identity,
    } = lifecycle.run_corpus()
    else {
        return Err(CliError::InvalidArgs(
            "--by-window requires a prepared corpus".into(),
        ));
    };
    let run_id = random_id();
    let mut sink = WindowSink {
        writer: BufWriter::new(io::stdout().lock()),
        size,
        current: None,
        document_tokens: 0,
        windows_emitted: 0,
    };
    sink.write(WindowCountsEvent::Run(WindowCountsRun {
        run_id: run_id.clone(),
        source_identity,
        analysis_identity,
        catalog_fingerprint,
        window_size: size.get(),
        patterns,
    }))?;
    let presentation = PresentationProgress::stderr(
        &config.stderr,
        config.progress,
        OutputMode::Jsonl,
        ResultDelivery::Streaming,
    );
    let mut progress = |progress: ExecutionProgress, _sink: &WindowSink<_>| {
        let sentences = progress.sentences_matched;
        presentation.snapshot(move || format!("matched {sentences} sentences"));
    };
    let outcome = execute_analyzed(
        source,
        analysis.binding(config.workers, &pool),
        &bound,
        &pool,
        &config.limits,
        config.limits.max_stage_sentences,
        terminal.cancellation(),
        &mut sink,
        &mut progress,
        &mut |_| Ok(()),
        Publication::from(PolicySelection::Apply),
        &SentenceAdmission::All,
    )?;
    presentation.finish();
    let RunOutcome::Completed(report) = outcome else {
        return Ok(CommandOutcome::Cancelled);
    };
    if source_stats.oversized_sentence_omissions()?.is_some() {
        return Err(ExecutionError::Sink(
            "window counts require every prepared sentence to fit the execution limits".into(),
        )
        .into());
    }
    crate::attribute_binding::write_out_of_set_label_notices(
        compiled.matcher.label_declarations(),
        &report.out_of_set_labels,
        config,
        &run_id,
    )?;
    crate::attribute_binding::write_unobserved_span_kind_notices(
        &compiled.matcher,
        &report,
        None,
        config,
        &run_id,
    )?;
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    sink.publish_window()?;
    sink.write(WindowCountsEvent::Complete(WindowCountsComplete {
        windows: sink.windows_emitted,
    }))?;
    sink.writer.flush().map_err(execution_output_io)?;
    Ok(CommandOutcome::Success)
}

struct WindowSink<W> {
    writer: W,
    size: NonZeroU64,
    current: Option<WindowCountsData>,
    document_tokens: u64,
    windows_emitted: u64,
}

impl<W: Write> WindowSink<W> {
    fn write(&mut self, event: WindowCountsEvent) -> Result<(), ExecutionError> {
        write_jsonl(&mut self.writer, &WindowCountsEnvelope::new(event))
            .map_err(execution_output_error)
    }

    fn publish_window(&mut self) -> Result<(), ExecutionError> {
        if let Some(window) = self.current.take() {
            self.write(WindowCountsEvent::Window(window))?;
            self.windows_emitted = add(self.windows_emitted, 1)?;
        }
        Ok(())
    }

    fn count<'a>(
        &mut self,
        document_ordinal: u64,
        words: usize,
        names: impl ExactSizeIterator<Item = &'a str>,
    ) -> Result<SinkControl, ExecutionError> {
        if self
            .current
            .as_ref()
            .is_some_and(|window| window.document_ordinal != document_ordinal)
        {
            self.publish_window()?;
            self.document_tokens = 0;
        }
        let window_ordinal = self.document_tokens / self.size.get();
        if self
            .current
            .as_ref()
            .is_some_and(|window| window.window_ordinal != window_ordinal)
        {
            self.publish_window()?;
        }
        let window = self.current.get_or_insert_with(|| WindowCountsData {
            document_ordinal,
            window_ordinal,
            ..WindowCountsData::default()
        });
        let words = u64::try_from(words).map_err(|_| ExecutionError::ResourceAccounting)?;
        window.tokens = add(window.tokens, words)?;
        window.sentences = add(window.sentences, 1)?;
        self.document_tokens = add(self.document_tokens, words)?;
        let records_emitted = names.len();
        for name in names {
            if let Some(count) = window.counts.get_mut(name) {
                *count = add(*count, 1)?;
            } else {
                window.counts.insert(name.to_owned(), 1);
            }
        }
        Ok(SinkControl::Continue { records_emitted })
    }
}

impl<W: Write> MatchSink for WindowSink<W> {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.count(
            location.document_ordinal,
            sentence.len(),
            matches.iter().map(|m| m.pattern.as_str()),
        )
    }

    fn push_records(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        self.count(
            location.document_ordinal,
            sentence.len(),
            records.iter().map(|record| record.name.as_str()),
        )
    }
}

fn add(left: u64, right: u64) -> Result<u64, ExecutionError> {
    left.checked_add(right)
        .ok_or(ExecutionError::ResourceAccounting)
}
