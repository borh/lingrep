#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, ErrorKind};
use std::time::Instant;

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

fn main() {
    if let Err(error) = run() {
        eprintln!("measure_lx_compile: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let kind = arguments.next().ok_or_else(usage_error)?;
    let label = arguments.next().ok_or_else(usage_error)?;
    let value = arguments.next().ok_or_else(usage_error)?;
    if arguments.next().is_some() {
        return Err(usage_error());
    }

    let (source_kind, source_path, source) = match kind.as_str() {
        "file" => {
            let source = fs::read_to_string(&value)?;
            ("file", value.as_str(), source)
        }
        "nested" => {
            let depth = value.parse::<usize>()?;
            ("generated-nested", "-", generated_nested(depth))
        }
        "references" => {
            let levels = value.parse::<usize>()?;
            ("generated-references", "-", generated_references(levels))
        }
        "payload" => {
            let (literal_bytes, repetitions) = parse_dimensions(&value)?;
            (
                "generated-payload",
                "-",
                generated_payload(literal_bytes, repetitions),
            )
        }
        "metadata" => {
            let (value_bytes, definitions) = parse_dimensions(&value)?;
            (
                "generated-shared-metadata",
                "-",
                generated_shared_metadata(value_bytes, definitions),
            )
        }
        _ => return Err(usage_error()),
    };

    let input_bytes = source.len();
    let started = Instant::now();
    let output = compile_catalog(
        PatternSource::named(&label, &source),
        CompileLimits::default(),
    )?;
    let compile_ns = started.elapsed().as_nanos();
    let stats = output.stats;
    let peak_rss_kib =
        peak_rss_kib().map_or_else(|| "unavailable".to_owned(), |rss| rss.to_string());

    println!(
        "label\tsource_kind\tsource_path\tinput_bytes\tcharged_source_bytes\tdefinitions\texpression_nodes\tmax_depth\tmetadata_bytes\tregex_bytes\treference_edges\texpansion_work\tgenerated_owned_bytes\tnfa_states\ttransitions\tcompile_ns\tpeak_rss_kib"
    );
    println!(
        "{label}\t{source_kind}\t{source_path}\t{input_bytes}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{compile_ns}\t{peak_rss_kib}",
        stats.source_bytes(),
        stats.definitions(),
        stats.expression_nodes(),
        stats.max_depth(),
        stats.metadata_bytes(),
        stats.regex_bytes(),
        stats.reference_edges(),
        stats.expansion_work(),
        stats.generated_owned_bytes(),
        stats.nfa_states(),
        stats.transitions(),
    );
    Ok(())
}

fn usage_error() -> Box<dyn Error> {
    io::Error::new(
        ErrorKind::InvalidInput,
        "usage: measure_lx_compile (file <label> <path> | nested <label> <depth> | references <label> <levels> | payload <label> <literal-bytes>x<repetitions> | metadata <label> <value-bytes>x<definitions>)",
    )
    .into()
}

fn parse_dimensions(value: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let (left, right) = value.split_once('x').ok_or_else(usage_error)?;
    let left = left.parse::<usize>()?;
    let right = right.parse::<usize>()?;
    if left == 0 || right == 0 {
        return Err(usage_error());
    }
    Ok((left, right))
}

fn generated_nested(depth: usize) -> String {
    let mut source = String::with_capacity(24 + depth.saturating_mul(2));
    source.push_str("nested := ");
    source.extend(std::iter::repeat_n('(', depth));
    source.push_str("[text=x]");
    source.extend(std::iter::repeat_n(')', depth));
    source.push_str(".\n");
    source
}

fn generated_references(levels: usize) -> String {
    let mut source = String::from("d0 := [text=x].\n");
    for level in 1..=levels {
        writeln!(source, "d{level} := @d{} @d{}.", level - 1, level - 1)
            .expect("writing to a String cannot fail");
    }
    source
}

fn generated_payload(literal_bytes: usize, repetitions: usize) -> String {
    format!(
        "expanded := [text={}]{{{repetitions}}}.\n",
        "x".repeat(literal_bytes)
    )
}

fn generated_shared_metadata(value_bytes: usize, definitions: usize) -> String {
    // One `defaults` entry, materialized into every definition: the successor
    // to the metadata block's shared header, and the same measurement.
    let mut source = format!("defaults\n  note: \"{}\"\nend\n\n", "m".repeat(value_bytes));
    for index in 0..definitions {
        writeln!(source, "p{index} := [text=x].").expect("writing to a String cannot fail");
    }
    source
}

fn peak_rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?;
    value.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        generated_nested, generated_payload, generated_references, generated_shared_metadata,
    };
    use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

    #[test]
    fn generated_nested_input_reaches_the_requested_depth_and_compiles() {
        let source = generated_nested(32);
        let output = compile_catalog(
            PatternSource::named("generated:nested:32", &source),
            CompileLimits::default(),
        )
        .expect("generated nested lingrep patterns must compile");

        assert!(output.stats.max_depth() >= 32);
        assert_eq!(output.stats.definitions(), 1);
    }

    #[test]
    fn generated_reference_input_is_shared_in_source_and_measured_when_expanded() {
        let source = generated_references(8);
        let output = compile_catalog(
            PatternSource::named("generated:references:8", &source),
            CompileLimits::default(),
        )
        .expect("generated reference-heavy lingrep patterns must compile");

        assert_eq!(output.stats.definitions(), 9);
        assert_eq!(output.stats.reference_edges(), 16);
        assert!(output.stats.expansion_work() > output.stats.expression_nodes());
    }

    #[test]
    fn generated_payload_input_measures_quantifier_fan_out() {
        let source = generated_payload(128, 8);
        let output = compile_catalog(
            PatternSource::named("generated:payload:128x8", &source),
            CompileLimits::default(),
        )
        .expect("generated payload lingrep patterns must compile");

        assert!(output.stats.generated_owned_bytes() >= 128 * 8);
    }

    #[test]
    fn generated_shared_metadata_input_measures_catalog_fan_out() {
        let source = generated_shared_metadata(128, 4);
        let output = compile_catalog(
            PatternSource::named("generated:metadata:128x4", &source),
            CompileLimits::default(),
        )
        .expect("generated shared-metadata lingrep patterns must compile");

        assert!(output.stats.generated_owned_bytes() >= 128 * 4);
    }
}
