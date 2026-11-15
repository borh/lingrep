//! `neighbors`: the values nearest one word in a word-vector file.

use std::io::{self, Write};

use compact_str::CompactString;
use lingrep_corpus::word_vectors::{Millionths, VectorTable, pattern_literal as literal};

use crate::cli::NeighborsArgs;
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::neighbors::{
    Neighbor, NeighborsComplete, NeighborsEnvelope, NeighborsEvent, NeighborsRun,
};
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run(args: &NeighborsArgs, mode: OutputMode) -> Result<CommandOutcome, CliError> {
    if mode == OutputMode::Tsv {
        return Err(CliError::UnsupportedOutput {
            command: "neighbors",
            output: "tsv",
        });
    }
    if args.cut.min_similarity.is_none() && args.cut.top.is_none() {
        return Err(CliError::InvalidArgs(
            "neighbors requires --min-similarity, --top, or both".into(),
        ));
    }
    let table = VectorTable::open(&args.vectors)?;
    let Some(query) = table.unit_vectors(&[&args.word])?.pop().flatten() else {
        return Err(CliError::InvalidArgs(format!(
            "`{}` has no vector in {}",
            args.word,
            args.vectors.display()
        )));
    };
    #[allow(clippy::cast_possible_truncation)]
    let minimum = args
        .cut
        .min_similarity
        .map(|minimum| (f64::from(minimum) * 1e6).round() as Millionths);
    let mut scan = table.scan(&[&query], |_, similarity| {
        minimum.is_none_or(|minimum| similarity >= minimum)
    })?;
    let mut neighbors = scan.hits.pop().unwrap_or_default();
    neighbors.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    neighbors.truncate(args.cut.top.unwrap_or(usize::MAX));
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if mode == OutputMode::Jsonl {
        let mut emit = |event| {
            write_jsonl(&mut writer, &NeighborsEnvelope::new(event)).map_err(CliError::from)
        };
        emit(NeighborsEvent::Run(NeighborsRun {
            word: args.word.clone(),
            vectors: args.vectors.display().to_string(),
            dimensions: query.len() as u64,
            min_similarity: args.cut.min_similarity,
            top: args.cut.top.map(|top| top as u64),
        }))?;
        let count = neighbors.len() as u64;
        for (index, (value, similarity)) in neighbors.into_iter().enumerate() {
            emit(NeighborsEvent::Neighbor(Neighbor {
                rank: index as u64 + 1,
                value: value.into(),
                #[allow(clippy::cast_possible_truncation)]
                similarity: (f64::from(similarity) / 1e6) as f32,
            }))?;
        }
        emit(NeighborsEvent::Complete(NeighborsComplete {
            neighbors: count,
            values: scan.values,
            values_without_vector: scan.values_without_vector,
        }))?;
    } else {
        write_set(&mut writer, args, &neighbors)?;
    }
    Ok(CommandOutcome::Success)
}

fn write_set<W: Write>(
    writer: &mut W,
    args: &NeighborsArgs,
    neighbors: &[(CompactString, Millionths)],
) -> Result<(), CliError> {
    let members = neighbors
        .iter()
        .filter_map(|(value, _)| literal(value))
        .collect::<Vec<_>>();
    let omitted = neighbors.len() - members.len();
    if members.is_empty() {
        return Err(CliError::Parse(
            "none of the selected values can be written as a set member".into(),
        ));
    }
    let mut cut = Vec::new();
    if let Some(minimum) = args.cut.min_similarity {
        cut.push(format!("cosine >= {minimum}"));
    }
    if let Some(top) = args.cut.top {
        cut.push(format!("top {top}"));
    }
    writeln!(
        writer,
        "# {} values near {} ({})",
        members.len(),
        literal(&args.word).unwrap_or_default(),
        cut.join(", ")
    )
    .map_err(CliError::stdout)?;
    writeln!(writer, "set {} := {{{}}}.", args.name, members.join(", "))
        .map_err(CliError::stdout)?;
    if omitted > 0 {
        writeln!(
            io::stderr().lock(),
            "note: {omitted} values contain a character that a set member cannot spell and were left out"
        )?;
    }
    Ok(())
}
