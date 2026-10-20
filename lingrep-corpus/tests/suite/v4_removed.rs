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

fn rust_sources(workspace: &Path, roots: [&str; 4]) -> Vec<PathBuf> {
    fn visit(directory: &Path, sources: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            let path = entry.path();
            if file_type.is_dir() {
                visit(&path, sources);
            } else if file_type.is_file()
                && path.extension().is_some_and(|extension| extension == "rs")
            {
                sources.push(path);
            }
        }
    }

    let mut sources = Vec::new();
    for root in roots {
        visit(&workspace.join(root), &mut sources);
    }
    sources.sort();
    sources
}

#[test]
fn runtime_tree_contains_no_v4_producer_or_consumer() {
    let forbidden = [
        "StreamingLazyCache",
        "StreamingLazyWriter",
        "ParallelCacheBatchDecoder",
        "open_supported_lazy_cache",
        "V4_MAGIC",
        "cached_executor",
        "LazyColumnarCache",
        "ColumnarCache",
        "CacheFileHeader",
        "CacheFormatVersion",
        "CacheDataSection",
        "CACHE_MAGIC",
        "columnar_builder",
        "columnar_integration",
        "ArchivedSentenceView",
    ];
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for path in rust_sources(
        workspace,
        [
            "lingrep-core/src",
            "lingrep-corpus/src",
            "lingrep-web/src",
            "lingrep/src",
        ],
    ) {
        let source = std::fs::read_to_string(&path).unwrap();
        for name in forbidden {
            assert!(
                !source.contains(name),
                "{name} survived in {}",
                path.display()
            );
        }
    }
}
