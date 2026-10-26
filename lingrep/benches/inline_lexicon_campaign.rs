//! Dev-only full prepared-corpus campaign for inline lexicons.

#![allow(clippy::panic, clippy::unwrap_used)]

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use lingrep_core::execution::{
    CancellationFlag, ExecutionError, ExecutionLimits, MatchSink, PolicySelection,
    SentenceAdmission, SentenceLocation, SinkControl, execute,
};
use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::TokenSequence;
use lingrep_corpus::CorpusArtifact;
use rayon::ThreadPoolBuilder;

#[derive(Default)]
struct CountSink {
    groups: BTreeMap<(String, String, String), u64>,
    lexical_occurrences: u64,
}

impl MatchSink for CountSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        for matched in matches {
            let Some(lexical) = &matched.lexical else {
                continue;
            };
            let projections = lexical
                .matched_by
                .iter()
                .map(lingrep_core::token_match::AttributeKey::pattern_key)
                .collect::<Vec<_>>()
                .join(",");
            *self
                .groups
                .entry((
                    matched.pattern.to_string(),
                    lexical.term.to_string(),
                    projections,
                ))
                .or_default() += 1;
            self.lexical_occurrences += 1;
        }
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

fn argument(name: &str) -> String {
    let args = env::args().collect::<Vec<_>>();
    let index = args
        .iter()
        .position(|value| value == name)
        .unwrap_or_else(|| panic!("missing {name}"));
    args.get(index + 1)
        .unwrap_or_else(|| panic!("missing value for {name}"))
        .clone()
}

fn peak_rss_kib() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

fn main() {
    let patterns = PathBuf::from(argument("--patterns"));
    let corpus = PathBuf::from(argument("--corpus"));
    let workers: usize = argument("--workers").parse().unwrap();
    let source_text = fs::read_to_string(&patterns).unwrap();
    let compiled = compile_catalog(
        PatternSource::named(patterns.to_str().unwrap(), &source_text),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let artifact = CorpusArtifact::open(&corpus).unwrap();
    let source = artifact
        .deferred_sentence_source(&compiled.execution_attributes())
        .unwrap();
    let pool = ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .unwrap();
    let limits = ExecutionLimits::default();
    let mut sink = CountSink::default();
    let started = Instant::now();
    let outcome = execute(
        source,
        |_text| Vec::new(),
        &compiled,
        &pool,
        &limits,
        limits.max_stage_sentences,
        &CancellationFlag::new(),
        &mut sink,
        &mut |_progress, _sink: &CountSink| {},
        &mut |_observation| Ok(()),
        PolicySelection::Apply,
        &SentenceAdmission::All,
    )
    .unwrap();
    let elapsed = started.elapsed();
    let report = match outcome {
        lingrep_core::execution::RunOutcome::Completed(report) => report,
        lingrep_core::execution::RunOutcome::Cancelled(_) => panic!("campaign was cancelled"),
    };
    let mut hasher = blake3::Hasher::new();
    for (key, count) in &sink.groups {
        hasher.update(key.0.as_bytes());
        hasher.update(key.1.as_bytes());
        hasher.update(key.2.as_bytes());
        hasher.update(&count.to_le_bytes());
    }
    let seconds = elapsed.as_secs_f64();
    println!(
        "{}",
        serde_json::json!({
            "tokens": report.tokens_matched,
            "sentences": report.sentences_matched,
            "elapsed_seconds": seconds,
            "tokens_per_second": report.tokens_matched as f64 / seconds,
            "peak_rss_kib": peak_rss_kib(),
            "emitted_lexical_occurrences": sink.lexical_occurrences,
            "distinct_groups": sink.groups.len(),
            "output_digest": hasher.finalize().to_hex().to_string(),
            "projection_bytes": null,
            "automaton_hits": null,
            "largest_scratch_capacities": null,
        })
    );
}
