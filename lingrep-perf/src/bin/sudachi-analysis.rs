use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use lingrep_core::analysis::{
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, InputUnitId, NormalizationPolicy,
    SegmentationMode, SudachiConfig, SudachiEdition, SudachiPlan, SudachiResources, SudachiSession,
    SudachiSplit, TextUnit, TextUnitKind,
};
use lingrep_core::token_match::{AttributeKey, AttributeOrdinal, BoundProjection};
use serde::Serialize;
use serde_json::Value;

mod analysis_support;
use analysis_support::{allocation_counts, peak_rss_kib, reset_allocator_counts};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EditionArg {
    Small,
    Core,
    Full,
}

impl From<EditionArg> for SudachiEdition {
    fn from(value: EditionArg) -> Self {
        match value {
            EditionArg::Small => Self::Small,
            EditionArg::Core => Self::Core,
            EditionArg::Full => Self::Full,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SplitArg {
    A,
    B,
    C,
}

impl From<SplitArg> for SudachiSplit {
    fn from(value: SplitArg) -> Self {
        match value {
            SplitArg::A => Self::A,
            SplitArg::B => Self::B,
            SplitArg::C => Self::C,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileArg {
    Tokenize,
    Morphology,
}

impl From<ProfileArg> for ExtractionProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::Tokenize => Self::Tokenize,
            ProfileArg::Morphology => Self::Morphology,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SegmentationArg {
    Input,
    Analyzer,
}

impl From<SegmentationArg> for SegmentationMode {
    fn from(value: SegmentationArg) -> Self {
        match value {
            SegmentationArg::Input => Self::Input,
            SegmentationArg::Analyzer => Self::Analyzer,
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Measure bounded native Sudachi analysis into a dense projection")]
struct Args {
    #[arg(long)]
    dictionary: PathBuf,
    #[arg(long)]
    resources: PathBuf,
    #[arg(long, value_enum)]
    edition: EditionArg,
    #[arg(long, value_enum)]
    split: SplitArg,
    #[arg(long, value_enum)]
    profile: ProfileArg,
    #[arg(long, value_enum)]
    segmentation: SegmentationArg,
    #[arg(long)]
    input: PathBuf,
    #[arg(long, value_delimiter = ',')]
    projection: Vec<String>,
}

#[derive(Serialize)]
struct Report {
    edition: String,
    split: String,
    profile: String,
    segmentation: String,
    dictionary_blake3: String,
    resource_blake3: String,
    schema_blake3: String,
    projection_keys_blake3: String,
    canonical_output_blake3: String,
    startup_nanoseconds: u128,
    analysis_nanoseconds: u128,
    first_result_nanoseconds: Option<u128>,
    input_units: u64,
    analyzed_sentences: u64,
    source_bytes: u64,
    tokens: u64,
    peak_rss_kib: Option<u64>,
    allocations: u64,
    allocated_bytes: u64,
    deallocations: u64,
    deallocated_bytes: u64,
}

#[derive(Default)]
struct Totals {
    input_units: u64,
    analyzed_sentences: u64,
    source_bytes: u64,
    tokens: u64,
    first_result: Option<Duration>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.projection.is_empty() {
        return Err("--projection requires at least one analyzer attribute key".into());
    }

    let startup = Instant::now();
    let (dictionary_digest, resource_digest) = SudachiConfig::content_digests(
        &args.dictionary,
        &SudachiResources::directory(&args.resources),
    )?;
    let profile = ExtractionProfile::from(args.profile);
    let segmentation = SegmentationMode::from(args.segmentation);
    let plan = SudachiPlan::load(SudachiConfig::new(
        &args.dictionary,
        SudachiResources::directory(&args.resources),
        dictionary_digest,
        resource_digest,
        SudachiEdition::from(args.edition),
        SudachiSplit::from(args.split),
        profile,
        segmentation,
        NormalizationPolicy::None,
    )?)?;
    let keys = args
        .projection
        .iter()
        .map(|key| AttributeKey::new(key).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let projection = plan.schema().bind_projection(keys)?;
    let startup_nanoseconds = startup.elapsed().as_nanos();

    let mut projection_hasher = blake3::Hasher::new();
    projection_hasher.update(b"lingrep.sudachi-capacity-projection/v1");
    for key in projection.keys() {
        projection_hasher.update(&(key.as_bytes().len() as u64).to_le_bytes());
        projection_hasher.update(key.as_bytes());
    }

    let mut session = plan.create_session();
    let input = BufReader::new(File::open(&args.input)?);
    let mut output_hasher = blake3::Hasher::new();
    output_hasher.update(b"lingrep.sudachi-capacity-output/v1");
    let mut totals = Totals::default();
    let mut next_input_id = 0_u64;
    reset_allocator_counts();
    let analysis_started = Instant::now();
    for (line_index, line) in input.lines().enumerate() {
        let line = line?;
        let document: Value = serde_json::from_str(&line)
            .map_err(|error| format!("JSONL line {}: {error}", line_index + 1))?;
        let sentences = document
            .get("sentences")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("JSONL line {} has no sentences array", line_index + 1))?;
        match segmentation {
            SegmentationMode::Input => {
                for sentence in sentences {
                    let text = sentence.as_str().ok_or_else(|| {
                        format!(
                            "JSONL line {} contains a non-string sentence",
                            line_index + 1
                        )
                    })?;
                    consume(
                        TextUnit::sentence(
                            InputUnitId::new(next_input_id),
                            text,
                            NormalizationPolicy::None,
                        )?,
                        &mut session,
                        &projection,
                        analysis_started,
                        &mut output_hasher,
                        &mut totals,
                    )?;
                    next_input_id = next_input_id.checked_add(1).ok_or("input ID overflow")?;
                }
            }
            SegmentationMode::Analyzer => {
                let mut text = String::new();
                for sentence in sentences {
                    text.push_str(sentence.as_str().ok_or_else(|| {
                        format!(
                            "JSONL line {} contains a non-string sentence",
                            line_index + 1
                        )
                    })?);
                }
                consume(
                    TextUnit::admit(
                        InputUnitId::new(next_input_id),
                        TextUnitKind::Segment,
                        text,
                        NormalizationPolicy::None,
                    )?,
                    &mut session,
                    &projection,
                    analysis_started,
                    &mut output_hasher,
                    &mut totals,
                )?;
                next_input_id = next_input_id.checked_add(1).ok_or("input ID overflow")?;
            }
        }
    }
    let analysis_nanoseconds = analysis_started.elapsed().as_nanos();
    let counts = allocation_counts();
    let report = Report {
        edition: format!("{:?}", args.edition).to_lowercase(),
        split: format!("{:?}", args.split),
        profile: format!("{:?}", args.profile).to_lowercase(),
        segmentation: format!("{:?}", args.segmentation).to_lowercase(),
        dictionary_blake3: blake3::Hash::from_bytes(dictionary_digest)
            .to_hex()
            .to_string(),
        resource_blake3: blake3::Hash::from_bytes(resource_digest)
            .to_hex()
            .to_string(),
        schema_blake3: blake3::Hash::from_bytes(*plan.schema().digest().as_bytes())
            .to_hex()
            .to_string(),
        projection_keys_blake3: projection_hasher.finalize().to_hex().to_string(),
        canonical_output_blake3: output_hasher.finalize().to_hex().to_string(),
        startup_nanoseconds,
        analysis_nanoseconds,
        first_result_nanoseconds: totals.first_result.map(|duration| duration.as_nanos()),
        input_units: totals.input_units,
        analyzed_sentences: totals.analyzed_sentences,
        source_bytes: totals.source_bytes,
        tokens: totals.tokens,
        peak_rss_kib: peak_rss_kib(),
        allocations: counts.allocations,
        allocated_bytes: counts.allocated_bytes,
        deallocations: counts.deallocations,
        deallocated_bytes: counts.deallocated_bytes,
    };
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn consume(
    input: TextUnit,
    session: &mut SudachiSession,
    projection: &BoundProjection,
    analysis_started: Instant,
    output: &mut blake3::Hasher,
    totals: &mut Totals,
) -> Result<(), Box<dyn std::error::Error>> {
    totals.input_units = totals
        .input_units
        .checked_add(1)
        .ok_or("input count overflow")?;
    totals.source_bytes = totals
        .source_bytes
        .checked_add(u64::try_from(input.len())?)
        .ok_or("source byte count overflow")?;
    let batch = session.analyze_bounded_batch(vec![input], projection)?;
    for analyzed_input in batch.inputs() {
        output.update(&analyzed_input.input().id().get().to_le_bytes());
        for sentence in analyzed_input.sentences() {
            if totals.first_result.is_none() {
                totals.first_result = Some(analysis_started.elapsed());
            }
            totals.analyzed_sentences = totals
                .analyzed_sentences
                .checked_add(1)
                .ok_or("sentence count overflow")?;
            totals.tokens = totals
                .tokens
                .checked_add(u64::try_from(sentence.words().len())?)
                .ok_or("token count overflow")?;
            output.update(&sentence.analyzer_sentence_ordinal().to_le_bytes());
            output.update(&sentence.source_range().start().to_le_bytes());
            output.update(&sentence.source_range().end().to_le_bytes());
            for surface in sentence.surface_tokens() {
                output.update(&surface.source_range().start().to_le_bytes());
                output.update(&surface.source_range().end().to_le_bytes());
                output.update(&surface.word_range().start.to_le_bytes());
                output.update(&surface.word_range().end.to_le_bytes());
            }
            for token_index in 0..sentence.words().len() {
                let token = sentence
                    .words()
                    .token(token_index)
                    .ok_or("invalid token index")?;
                for ordinal in 0..projection.len() {
                    match token.get(AttributeOrdinal::new(u8::try_from(ordinal)?)) {
                        Some(value) => {
                            output.update(&[1]);
                            output.update(&(value.len() as u64).to_le_bytes());
                            output.update(value.as_bytes());
                        }
                        None => {
                            output.update(&[0]);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
