use std::path::PathBuf;
use std::time::Instant;

use lingrep_core::execution::CancellationFlag;
use lingrep_corpus::{CountTableKey, CountTableWriter, DocumentTotals, GroupKind, GroupSchema};

const USAGE: &str = "count_table_transpose [--output NEW_DIRECTORY] [--entries N] [--documents N] [--fanout N]\nDefaults: 10000 entries, 64 documents, 8 pairs per entry. Capacity runs require --output on a disk-backed filesystem.";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut output, mut entries, mut documents, mut fanout) = (None, 10_000u32, 64u32, 8u32);
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--bench" | "--test" => {}
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--output" => output = Some(PathBuf::from(arguments.next().ok_or(USAGE)?)),
            "--entries" => entries = arguments.next().ok_or(USAGE)?.parse()?,
            "--documents" => documents = arguments.next().ok_or(USAGE)?.parse()?,
            "--fanout" => fanout = arguments.next().ok_or(USAGE)?.parse()?,
            _ => return Err(format!("unknown argument {argument:?}\n{USAGE}").into()),
        }
    }
    if entries == 0 || documents == 0 || fanout == 0 || fanout > documents {
        return Err("entries and documents must be positive; fanout must be between 1 and the document count".into());
    }
    let pairs = u64::from(entries) * u64::from(fanout);
    if output.is_none() && pairs > 1_000_000 {
        return Err("use --output on a disk-backed filesystem for a capacity run".into());
    }
    let temporary = tempfile::tempdir()?;
    let destination = output.unwrap_or_else(|| temporary.path().join("table.lingtable"));
    let width = (f64::from(entries).sqrt().ceil() as u32).max(1);
    let values = (0..width)
        .map(|value| format!("v{value:08}"))
        .collect::<Vec<_>>();
    let docs = (0..documents)
        .map(|document| DocumentTotals {
            document_id: u64::from(document),
            sentences: 1,
            tokens: 10,
        })
        .collect::<Vec<_>>();
    let generated = Instant::now();
    let mut writer = CountTableWriter::create(
        &destination,
        CountTableKey {
            analysis_identity: "probe".into(),
            catalog_fingerprint: "probe".into(),
            matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.into(),
        },
        vec!["probe".into()],
        vec![GroupSchema {
            kind: GroupKind::Key,
            names: vec!["left".into(), "right".into()],
        }],
        &values,
        &docs,
    )?;
    let stride = (documents / fanout).max(1);
    for entry in 0..entries {
        writer.begin_entry(0, 0, &[entry / width, entry % width])?;
        let phase = entry % documents;
        let wrap = (documents - phase).div_ceil(stride).min(fanout);
        for offset in (wrap..fanout).chain(0..wrap) {
            let document = ((u64::from(phase) + u64::from(offset) * u64::from(stride))
                % u64::from(documents)) as u32;
            writer.add_sentence(
                document,
                u64::from(document),
                (u64::from(entry) + u64::from(document)) % 3 + 1,
            )?;
        }
    }
    let generated_seconds = generated.elapsed().as_secs_f64();
    let started = Instant::now();
    let stats = writer.finish(&CancellationFlag::new())?.publish()?;
    println!(
        "{}",
        serde_json::json!({
            "entries": entries, "documents": documents, "pairs": pairs,
            "generation_seconds": generated_seconds,
            "finish_seconds": started.elapsed().as_secs_f64(),
            "bytes": stats.bytes, "output": destination,
        })
    );
    Ok(())
}
