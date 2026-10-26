//! Criterion smoke coverage for bounded recursive corpus discovery.
//!
//! Fixture creation occurs before Criterion starts timing. Discovery itself only
//! reads directory metadata and retains no traversal spill files.

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::fs;

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use lingrep_core::corpus::{CorpusDiscovery, TraversalLimits};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
}

impl Fixture {
    fn flat() -> Self {
        let directory = tempfile::tempdir().expect("create flat discovery fixture");
        for index in 0..10_000 {
            fs::write(directory.path().join(format!("{index:05}.md")), "x\n")
                .expect("write flat fixture document");
        }
        Self {
            _directory: directory,
        }
    }

    fn sharded() -> Self {
        let directory = tempfile::tempdir().expect("create sharded discovery fixture");
        for shard in 0..100 {
            let shard_path = directory.path().join(format!("{shard:03}"));
            fs::create_dir(&shard_path).expect("create shard");
            for document in 0..100 {
                fs::write(shard_path.join(format!("{document:03}.md")), "x\n")
                    .expect("write sharded fixture document");
            }
        }
        Self {
            _directory: directory,
        }
    }

    fn root(&self) -> &std::path::Path {
        self._directory.path()
    }
}

fn discover_all(root: &std::path::Path) -> usize {
    let mut discovery =
        CorpusDiscovery::new(root, TraversalLimits::default()).expect("open discovery");
    let mut documents = 0;
    while discovery
        .next_document()
        .expect("discover document")
        .is_some()
    {
        documents += 1;
    }
    let report = discovery.finish().expect("finish discovery");
    assert_eq!(documents, 10_000);
    // This is the traversal-index high-water measurement. There is no spill
    // file API or write operation in the timed traversal.
    black_box(report.observed.index_bytes)
}

fn first_document(root: &std::path::Path) -> usize {
    let mut discovery =
        CorpusDiscovery::new(root, TraversalLimits::default()).expect("open discovery");
    let first = discovery
        .next_document()
        .expect("discover first document")
        .expect("fixture is not empty");
    black_box(first.logical_name.len())
}

fn corpus_discovery(c: &mut Criterion) {
    let flat = Fixture::flat();
    let sharded = Fixture::sharded();
    let mut group = c.benchmark_group("corpus_discovery");
    for (name, fixture) in [("flat_10000", &flat), ("sharded_100x100", &sharded)] {
        let peak_retained_bytes = discover_all(fixture.root());
        eprintln!("{name}: peak traversal-index bytes = {peak_retained_bytes}");
        group.throughput(Throughput::Elements(10_000));
        group.bench_function(
            format!("{name}/throughput_and_peak_retained_bytes"),
            |bench| {
                bench.iter(|| discover_all(fixture.root()));
            },
        );
        group.bench_function(format!("{name}/first_document_latency"), |bench| {
            bench.iter(|| first_document(fixture.root()));
        });
    }
    group.finish();
}

criterion_group!(benches, corpus_discovery);
criterion_main!(benches);
