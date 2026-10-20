use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use lingrep_core::analysis::{
    AnalyzerPlan, AnalyzerSession, InputUnitId, NormalizationPolicy, TextUnit, VibratoPlan,
};
use lingrep_core::dictionary::create_tokenizer_from_path;
use lingrep_core::token_match::AttributeKey;
use serde::Serialize;
use serde_json::Value;

mod analysis_support;
use analysis_support::{allocation_counts, peak_rss_kib, reset_allocator_counts};

#[derive(Debug, Parser)]
#[command(about = "Measure bounded Vibrato analysis into a dense attribute projection")]
struct Args {
    #[arg(long)]
    dict_path: PathBuf,
    #[arg(long)]
    input: PathBuf,
    #[arg(long, value_delimiter = ',')]
    projection: Vec<String>,
}

#[derive(Serialize)]
struct Report {
    sentences: u64,
    tokens: u64,
    projected_values_blake3: String,
    elapsed_nanoseconds: u128,
    first_result_nanoseconds: Option<u128>,
    peak_rss_kib: Option<u64>,
    allocations: u64,
    allocated_bytes: u64,
    deallocations: u64,
    deallocated_bytes: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.projection.is_empty() {
        return Err("--projection requires at least one lingrep patterns attribute key".into());
    }
    let attributes = args
        .projection
        .iter()
        .map(|key| {
            AttributeKey::new(key)
                .map_err(|error| format!("invalid lingrep patterns attribute key {key:?}: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tokenizer = create_tokenizer_from_path(
        args.dict_path
            .to_str()
            .ok_or("dictionary path is not valid UTF-8")?,
    )?;
    let plan = VibratoPlan::new(tokenizer);
    let projection = plan.schema().bind_projection(attributes)?;
    let mut session = plan.create_session();
    let input = BufReader::new(File::open(&args.input)?);

    reset_allocator_counts();
    let started = Instant::now();
    let mut hasher = blake3::Hasher::new();
    let mut sentences = 0_u64;
    let mut tokens = 0_u64;
    let mut first_result_nanoseconds = None;
    for (line_index, line) in input.lines().enumerate() {
        let line = line?;
        let document: Value = serde_json::from_str(&line)
            .map_err(|error| format!("JSONL line {}: {error}", line_index + 1))?;
        let values = document
            .get("sentences")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("JSONL line {} has no sentences array", line_index + 1))?;
        for value in values {
            let text = value.as_str().ok_or_else(|| {
                format!(
                    "JSONL line {} contains a non-string sentence",
                    line_index + 1
                )
            })?;
            let input =
                TextUnit::sentence(InputUnitId::new(sentences), text, NormalizationPolicy::None)?;
            let mut analyzed = session
                .analyze_bounded_batch(vec![input], &projection)?
                .into_inputs()
                .into_vec();
            let (_, analyzed_sentences) = analyzed
                .pop()
                .ok_or("Vibrato returned no input for sentence input")?
                .into_parts();
            let sentence = analyzed_sentences
                .into_vec()
                .pop()
                .ok_or("Vibrato returned no sentence for sentence input")?
                .into_words();
            if first_result_nanoseconds.is_none() {
                first_result_nanoseconds = Some(started.elapsed().as_nanos());
            }
            sentences = sentences.checked_add(1).ok_or("sentence count overflow")?;
            tokens = tokens
                .checked_add(u64::try_from(sentence.len())?)
                .ok_or("token count overflow")?;
            hasher.update(&u64::try_from(sentence.len())?.to_le_bytes());
            for token_index in 0..sentence.len() {
                let token = sentence.token(token_index).ok_or("invalid token index")?;
                for ordinal in 0..projection.len() {
                    match token.get(lingrep_core::token_match::AttributeOrdinal::new(
                        u8::try_from(ordinal)?,
                    )) {
                        Some(value) => {
                            hasher.update(&[1]);
                            hasher.update(&u64::try_from(value.len())?.to_le_bytes());
                            hasher.update(value.as_bytes());
                        }
                        None => {
                            hasher.update(&[0]);
                        }
                    }
                }
            }
        }
    }
    let elapsed_nanoseconds = started.elapsed().as_nanos();
    let counts = allocation_counts();
    let report = Report {
        sentences,
        tokens,
        projected_values_blake3: hasher.finalize().to_hex().to_string(),
        elapsed_nanoseconds,
        first_result_nanoseconds,
        peak_rss_kib: peak_rss_kib(),
        allocations: counts.allocations,
        allocated_bytes: counts.allocated_bytes,
        deallocations: counts.deallocations,
        deallocated_bytes: counts.deallocated_bytes,
    };
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
