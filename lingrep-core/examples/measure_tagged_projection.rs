#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
use compact_str::CompactString;
#[cfg(target_os = "linux")]
use lingrep_core::token_match::{
    AttributeKind, BoundTokenSentence, ProjectedAttributeValue, SchemaAttribute, TokenSchema,
};
#[cfg(target_os = "linux")]
use serde_json::json;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    const TOKENS: usize = 10_000;
    // Keep the absolute RSS record scoped to a bounded stage. Jemalloc may
    // retain freed pages across iterations, so a long microbenchmark would
    // report allocator high-water rather than the configured live batch.
    const ITERATIONS: usize = 5;
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar)?,
        SchemaAttribute::new("lemma", AttributeKind::Scalar)?,
        SchemaAttribute::new("upos", AttributeKind::Scalar)?,
        SchemaAttribute::new("xpos", AttributeKind::Scalar)?,
        SchemaAttribute::new("feat.Number", AttributeKind::StringSet)?,
        SchemaAttribute::new("spacy.morph.reading", AttributeKind::StringSet)?,
    ])?;
    let projection = schema.bind_projection(schema.keys().iter().cloned())?;
    let values = (0..TOKENS)
        .flat_map(|index| {
            let text = CompactString::new(format!("語{index}"));
            [
                ProjectedAttributeValue::scalar(text.clone()),
                ProjectedAttributeValue::scalar(text),
                ProjectedAttributeValue::scalar("NOUN"),
                ProjectedAttributeValue::scalar("名詞"),
                ProjectedAttributeValue::string_set(["Plur", "Sing"]),
                ProjectedAttributeValue::string_set(["ゴ"]),
            ]
        })
        .collect::<Vec<_>>();

    let first_started = Instant::now();
    let first =
        BoundTokenSentence::from_projected_attribute_values(&projection, TOKENS, values.clone())?;
    let first_result_micros = first_started.elapsed().as_micros();
    let resident_bytes = first.resident_bytes()?;

    let mut cursor = 0_u64;
    let mut surface_ranges = Vec::with_capacity(TOKENS);
    let wire_values = (0..TOKENS)
        .flat_map(|index| {
            let text = format!("語{index}");
            let start = cursor;
            cursor += text.len() as u64;
            surface_ranges.push([start, cursor]);
            [
                json!(text),
                json!(format!("語{index}")),
                json!("NOUN"),
                json!("名詞"),
                json!(["Plur", "Sing"]),
                json!(["ゴ"]),
            ]
        })
        .collect::<Vec<_>>();
    let response_record = json!({
        "record": "sentence",
        "batch_id": 0,
        "document_ordinal": 0,
        "input_id": 0,
        "sentence_ordinal": 0,
        "source_range": [0, cursor],
        "surface_ranges": surface_ranges,
        "alignment": {"kind": "identity"},
        "word_count": TOKENS,
        "values": wire_values,
        "warnings": [],
    });
    let mut wire = serde_json::to_vec(&response_record)?;
    wire.push(b'\n');
    let compressed = zstd::stream::encode_all(wire.as_slice(), 3)?;
    let value_bytes = (0..TOKENS).try_fold(0usize, |total, index| {
        total.checked_add(format!("語{index}").len() * 2 + 4 + 6 + 8 + 3)
    });
    let value_bytes = value_bytes.ok_or("value byte count overflow")?;

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        std::hint::black_box(BoundTokenSentence::from_projected_attribute_values(
            &projection,
            TOKENS,
            values.clone(),
        )?);
    }
    let elapsed = started.elapsed().as_secs_f64();
    let measured_tokens = TOKENS * ITERATIONS;
    println!(
        "{}",
        json!({
            "schema_digest": hex(schema.digest().as_bytes()),
            "tokens_per_sentence": TOKENS,
            "iterations": ITERATIONS,
            "measured_tokens": measured_tokens,
            "elapsed_seconds": elapsed,
            "tokens_per_second": measured_tokens as f64 / elapsed,
            "first_result_micros": first_result_micros,
            "exact_owned_resident_bytes": resident_bytes,
            "resident_bytes_per_token": resident_bytes as f64 / TOKENS as f64,
            "canonical_response_record_bytes": wire.len(),
            "protocol_overhead_bytes": wire.len() - value_bytes,
            "compressed_bytes": compressed.len(),
            "compressed_bytes_per_token": compressed.len() as f64 / TOKENS as f64,
            "peak_host_rss_kib": peak_rss_kib()?,
            "accelerator_memory_bytes": serde_json::Value::Null,
        })
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(target_os = "linux")]
fn peak_rss_kib() -> Result<i64, io::Error> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { usage.assume_init() }.ru_maxrss)
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("measure_tagged_projection is supported only on Linux");
}
