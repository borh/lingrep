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
use std::path::{Path, PathBuf};

fn rust_sources_below(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                sources.push(path);
            }
        }
    }
    sources
}

#[test]
fn retired_alternate_executor_facade_cannot_reenter_the_source_tree() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/matcher.rs",
        "src/sampling.rs",
        "src/streaming.rs",
        "src/surface_counts.rs",
        "src/output/jsonl.rs",
        "tests/ooc_characterization.rs",
    ] {
        assert!(
            !manifest.join(relative).exists(),
            "retired alternate-executor file returned: {relative}"
        );
    }

    let retired_identifiers = [
        "CatalogMatcher",
        "SampledTokenSource",
        "SentenceLineReader",
        "write_surface_matches",
        "write_kwic_matches",
        "DetailLevel",
        "format_aligned_header",
        "write_kwic_aligned",
        "write_surface_jsonl",
        "write_kwic_jsonl",
        "write_surface_tsv",
        "write_kwic_tsv",
    ];
    let retired_declarations = ["pub enum Format"];
    for source_path in rust_sources_below(&manifest.join("src")) {
        let source = std::fs::read_to_string(&source_path).unwrap();
        for identifier in retired_identifiers {
            assert!(
                !source.contains(identifier),
                "retired public identifier `{identifier}` returned in {}",
                source_path.display()
            );
        }
        for declaration in retired_declarations {
            assert!(
                !source.contains(declaration),
                "retired public declaration `{declaration}` returned in {}",
                source_path.display()
            );
        }
    }
}
