//! Bounded sentence-final workflow over shared direct/prepared execution.

use std::io::{self, Write};
use std::sync::Arc;
use std::time::Instant;

use lingrep_core::execution::{
    ExecutionError, MatchSink, PolicySelection, Publication, RunOutcome, SentenceLocation,
    SinkControl,
};
use lingrep_core::fsm::Match;
use lingrep_core::spill::{SortedRunCounts, TempBudget};
use lingrep_core::token_match::TokenAttributeIndex;
#[cfg(test)]
use lingrep_core::token_match::TokenInfo;
use serde::{Deserialize, Serialize};

use super::CommandOutcome;
use crate::catalog_input::load_and_compile;
use crate::cli::{ExecutionCliConfig, ProgressMode, SentenceFinalArgs};
use crate::direct_source::matched_surface;
use crate::error::CliError;
use crate::interactive_execution::InteractiveExecution;
use crate::output::{SentenceFinalTsvRow, sanitize_terminal, write_sentence_final_tsv};
use crate::presentation::OutputMode;
use crate::protocol::sentence_final::{
    CompleteData, RowData, RunData, SentenceFinalEnvelope, SentenceFinalEvent,
};
use crate::protocol::write_jsonl;
use crate::terminal::{PresentationProgress, TerminalArbiter};

const MAX_RESIDENT_KEYS: usize = 4_096;
const MERGE_FAN_IN: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct SentenceFinalKey {
    pattern: String,
    surface: String,
}

struct SentenceFinalSink {
    counts: SortedRunCounts<SentenceFinalKey>,
}

impl SentenceFinalSink {
    fn new(max_temp_bytes: u64) -> io::Result<Self> {
        Ok(Self {
            counts: SortedRunCounts::new_budgeted(
                MAX_RESIDENT_KEYS,
                MERGE_FAN_IN,
                Arc::new(TempBudget::new(max_temp_bytes)),
            )?,
        })
    }
}

impl MatchSink for SentenceFinalSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        // Both the sentence-final predicate and the empty-surface skip discard
        // matches outright, so this sink routinely publishes far fewer records
        // than it is handed.
        let mut records_emitted = 0usize;
        for matched in matches
            .iter()
            .filter(|matched| is_sentence_final(sentence, matched.end))
        {
            let surface = matched_surface(sentence, matched.start..matched.end, location)
                .map_err(ExecutionError::from)?;
            if surface.is_empty() {
                continue;
            }
            self.counts
                .add(
                    SentenceFinalKey {
                        pattern: matched.pattern.to_string(),
                        surface,
                    },
                    1,
                )
                .map_err(|error| ExecutionError::Sink(error.to_string()))?;
            records_emitted += 1;
        }
        Ok(SinkControl::Continue { records_emitted })
    }
}

pub fn run(
    args: &SentenceFinalArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let started = Instant::now();
    let catalog = load_and_compile(&args.query.catalog.catalog_input()?)?;
    if !catalog.output.matcher.lexicons().is_empty() {
        return Err(CliError::InvalidArgs(
            "sentence-final does not accept lexicon definitions; use matches or summarize".into(),
        ));
    }
    let compiled = catalog.output;
    let execution = InteractiveExecution::new(&args.query.input, config)?;
    let mut sink = SentenceFinalSink::new(config.max_temp_bytes).map_err(CliError::Aggregation)?;
    let progress_mode = config.progress;
    let presentation_progress = PresentationProgress::stderr();
    let progress_writer = presentation_progress.clone();
    let outcome = execution.execute_publishing_with_attributes(
        &compiled.matcher,
        [lingrep_core::token_match::AttributeKey::from(
            TokenAttributeIndex::Orth,
        )],
        terminal.cancellation(),
        &mut sink,
        &mut |progress, _sink| {
            if progress_mode == ProgressMode::Always {
                progress_writer.write_line(&format!(
                    "sentence-final: sentences={} tokens={}",
                    progress.sentences_matched, progress.tokens_matched
                ));
            }
        },
        match args.trace_policy {
            Some(capacity) => Publication::from(PolicySelection::Apply).with_trace(capacity),
            None => Publication::from(PolicySelection::Apply),
        },
    )?;
    let report = match &outcome {
        RunOutcome::Completed(report) => report,
        RunOutcome::Cancelled(_) => return Ok(CommandOutcome::Cancelled),
    };
    // What the policy program did. This command drops further records of its
    // own — the sentence-final predicate, and the empty-surface skip — so a
    // record the trace ends with `kept` may still be absent from the table
    // below. `policy_report`'s module documentation is where that is stated;
    // it is a property of every aggregating caller, not of this one.
    if progress_mode == ProgressMode::Always {
        crate::policy_report::write_per_statement(
            &presentation_progress,
            compiled.matcher.catalog_policy(),
            report.program_stats.per_operation(),
        );
    }
    if let Some(trace) = report.trace.as_ref() {
        crate::policy_report::write_trace(
            &presentation_progress,
            compiled.matcher.catalog_policy(),
            trace,
        );
    }
    presentation_progress.finish().map_err(CliError::Io)?;
    sink.counts.finalize().map_err(CliError::Aggregation)?;
    if !terminal.try_claim_completion() {
        return Ok(CommandOutcome::Cancelled);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let run_id = format!("{:032x}", rand::random::<u128>());
    if output == OutputMode::Jsonl {
        write_jsonl(
            &mut writer,
            &SentenceFinalEnvelope::new(SentenceFinalEvent::Run(RunData {
                run_id,
                dictionary_identity: "resolved-at-execution".into(),
                catalog_fingerprint: catalog.fingerprint,
                input_kind: if args.query.input.corpus.is_some() {
                    "prepared"
                } else {
                    "direct"
                }
                .into(),
                limits: serde_json::json!({ "max_temp_bytes": config.max_temp_bytes }),
            })),
        )
        .map_err(|error| CliError::Io(io::Error::other(error)))?;
    }
    let mut rows = 0_u64;
    let mut matches_found = 0_u64;
    sink.counts
        .visit_sorted(|key, count| {
            let count = u64::try_from(count).map_err(io::Error::other)?;
            rows = rows.saturating_add(1);
            matches_found = matches_found.saturating_add(count);
            write_row(&mut writer, output, key, count)
        })
        .map_err(CliError::Aggregation)?;
    match output {
        OutputMode::Jsonl => write_jsonl(
            &mut writer,
            &SentenceFinalEnvelope::new(SentenceFinalEvent::Complete(CompleteData {
                coverage: "full".into(),
                rows,
                matches_found,
                elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                resources: serde_json::json!({ "max_temp_bytes": config.max_temp_bytes }),
            })),
        )
        .map_err(|error| CliError::Io(io::Error::other(error)))?,
        OutputMode::Human => writeln!(
            writer,
            "full coverage; {rows} rows, {matches_found} matches; completed in {} ms",
            started.elapsed().as_millis()
        )
        .map_err(CliError::Io)?,
        OutputMode::Tsv => {}
    }
    writer.flush().map_err(CliError::Io)?;
    Ok(CommandOutcome::Success)
}

fn write_row<W: Write>(
    writer: &mut W,
    output: OutputMode,
    key: &SentenceFinalKey,
    count: u64,
) -> io::Result<()> {
    match output {
        OutputMode::Jsonl => write_jsonl(
            writer,
            &SentenceFinalEnvelope::new(SentenceFinalEvent::Row(RowData {
                pattern: key.pattern.clone(),
                surface: key.surface.clone(),
                count,
            })),
        )
        .map_err(io::Error::other),
        OutputMode::Tsv => write_sentence_final_tsv(
            writer,
            &SentenceFinalTsvRow {
                pattern: key.pattern.clone(),
                surface: key.surface.clone(),
                count,
            },
        )
        .map_err(io::Error::other),
        OutputMode::Human => writeln!(
            writer,
            "{:>8}  {}  {}",
            count,
            sanitize_terminal(&key.pattern),
            sanitize_terminal(&key.surface)
        ),
    }
}

fn is_sentence_final(
    tokens: &dyn lingrep_core::token_match::TokenSequence,
    match_end: usize,
) -> bool {
    (match_end..tokens.len()).all(|index| {
        tokens
            .get_key(
                index,
                &lingrep_core::token_match::AttributeKey::new("upos")
                    .expect("upos is a canonical attribute key"),
            )
            .is_some_and(|pos| pos == "PUNCT")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::token_match::TokenAttributeIndex;

    fn token(orth: &str, pos: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token.set(TokenAttributeIndex::Pos, pos.into());
        token
    }

    #[test]
    fn punctuation_only_remainder_is_sentence_final() {
        let tokens = [token("研究", "NOUN"), token("。", "PUNCT")];
        assert!(is_sentence_final(&tokens, 1));
        assert!(!is_sentence_final(&tokens, 0));
    }
}
