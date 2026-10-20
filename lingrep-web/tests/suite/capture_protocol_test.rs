// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use lingrep_core::execution::{
    CancellationFlag, ExecutionError, ExecutionLimits, ExecutionReport, MatchSink, RunOutcome,
    SentenceAdmission, SentenceLocation, SinkControl, TokenizedBatchObservation, execute,
};
use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenInfo, TokenSequence};

use crate::v6_support::{build_artifact, open_artifact};

#[derive(Default)]
struct RecordingMatchSink {
    matches: Vec<Match>,
}

impl MatchSink for RecordingMatchSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.matches.extend_from_slice(matches);
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[test]
fn v6_executor_delivers_authored_capture_values_to_the_sink() {
    let fixture = build_artifact(vec![(
        "captures.md",
        0,
        false,
        vec![vec![TokenInfo::default().with_orth("食べる")]],
    )]);
    let artifact = open_artifact(&fixture);
    let fsm = compile_catalog(
        PatternSource::named("captures.lx", "outer := (?<unit> [text=食べる])."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut sink = RecordingMatchSink::default();

    let mut progress = |_, _: &RecordingMatchSink| {};
    let mut observer = |_: TokenizedBatchObservation| Ok(());
    let outcome = execute(
        artifact.sentence_source().unwrap(),
        |_| unreachable!("V7 artifact sentences are tokenized"),
        &fsm,
        &pool,
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut progress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(_)));
    assert_eq!(sink.matches.len(), 1);
    let captures = &sink.matches[0].captures;
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].name.as_str(), "unit");
    assert_eq!(captures[0].path.as_str(), "outer/unit");
    assert_eq!(captures[0].start..captures[0].end, 0..1);
}

struct CancellingCaptureSink {
    cancel: CancellationFlag,
    matches: Vec<Match>,
    pushes: usize,
}

impl MatchSink for CancellingCaptureSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.matches.extend_from_slice(matches);
        self.pushes += 1;
        if self.pushes == 1 {
            self.cancel.cancel();
        }
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[test]
fn cancellation_is_observed_before_the_next_capture_push() {
    let fixture = build_artifact(vec![(
        "captures.md",
        0,
        false,
        vec![vec![TokenInfo::default().with_orth("食べる")]; 3],
    )]);
    let artifact = open_artifact(&fixture);
    let fsm = compile_catalog(
        PatternSource::named("captures.lx", "outer := (?<unit> [text=食べる])."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let cancel = CancellationFlag::new();
    let mut sink = CancellingCaptureSink {
        cancel: cancel.clone(),
        matches: Vec::new(),
        pushes: 0,
    };

    let mut progress = |_, _: &CancellingCaptureSink| {};
    let mut observer = |_: TokenizedBatchObservation| Ok(());
    let outcome = execute(
        artifact.sentence_source().unwrap(),
        |_| unreachable!("V7 artifact sentences are tokenized"),
        &fsm,
        &pool,
        &ExecutionLimits::default(),
        2,
        &cancel,
        &mut sink,
        &mut progress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Cancelled(ExecutionReport {
            policy_stats: lingrep_core::fsm::PolicyStats::default(),
            program_stats: lingrep_core::fsm::ProgramStats::default(),
            trace: None,
            sentences_admitted: 2,
            sentences_matched: 2,
            tokens_matched: 2,
            matches_found: 2,
            records_emitted: 1,
        })
    );
    assert_eq!(sink.pushes, 1);
    assert_eq!(sink.matches.len(), 1);
    assert!(
        sink.matches
            .iter()
            .all(|matched| matched.captures.len() == 1)
    );
}

#[test]
fn capture_execution_preserves_match_and_capture_counts() {
    const TOKENS: usize = 12_000;
    {
        let sentences = vec![vec![TokenInfo::default().with_orth("食べる")]; TOKENS];
        let fixture = build_artifact(vec![("throughput.md", 0, false, sentences)]);
        let artifact = open_artifact(&fixture);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();

        for (source, expected_captures) in [
            ("plain := [text=食べる].", 0),
            ("outer := (?<unit> [text=食べる]).", 1),
        ] {
            let fsm = compile_catalog(
                PatternSource::named("throughput.lx", source),
                CompileLimits::default(),
            )
            .unwrap()
            .matcher;
            let mut sink = RecordingMatchSink::default();
            let mut progress = |_, _: &RecordingMatchSink| {};
            let mut observer = |_: TokenizedBatchObservation| Ok(());
            let outcome = execute(
                artifact.sentence_source().unwrap(),
                |_| unreachable!("V7 artifact sentences are tokenized"),
                &fsm,
                &pool,
                &ExecutionLimits::default(),
                1_024,
                &CancellationFlag::new(),
                &mut sink,
                &mut progress,
                &mut observer,
                lingrep_core::execution::PolicySelection::Apply,
                &SentenceAdmission::All,
            )
            .unwrap();
            assert!(matches!(outcome, RunOutcome::Completed(_)));
            assert_eq!(sink.matches.len(), TOKENS);
            assert!(
                sink.matches
                    .iter()
                    .all(|matched| matched.captures.len() == expected_captures)
            );
        }
    }
}
