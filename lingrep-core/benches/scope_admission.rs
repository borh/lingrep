//! Cost of scope conditioning at direct-input sentence admission.
//!
//! This isolates the direct-ingestion path, where metadata-only scope is
//! evaluated per document and section scope per sentence. Performance bars and
//! measurements live in `docs/PERFORMANCE_REQUIREMENTS.md`.
//!
//! The metadata-only bar is the load-bearing one. A clause naming no section
//! takes the same branch it took before section scope existed, so a regression
//! there means the two-pass split is wrong rather than slow — and there is
//! deliberately no pre-committed remedy for it, because there is nothing to
//! optimise.
//!
//! The three original cases isolate the reader: they drive `AdmittedCorpus`
//! directly and count admitted sentences, so tokenization is not in the
//! measurement.
//!

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use criterion::{Criterion, criterion_group, criterion_main};
use lingrep_core::corpus::{
    AdmittedCorpus, AdmittedCorpusEvent, CorpusEvent, ScopeObservation, metadata_from_json,
};
use lingrep_core::scope::ScopeClause;
use std::hint::black_box;

const DOCUMENTS: u64 = 100;
const SENTENCES_PER_DOCUMENT: u64 = 100;

/// 10,000 sentences over 100 documents, half of them from 2013 and half from
/// 2005, so a `year >= 2010` clause admits exactly half.
///
/// Sentences alternate between the `methods` and `results` labels, so
/// `@section in {methods, results}` admits *every* sentence. That isolates the
/// per-sentence evaluation cost from the skip's saving, which would otherwise
/// flatter the number.
fn corpus_events(labelled: bool) -> Vec<std::io::Result<CorpusEvent>> {
    let mut events = Vec::new();
    for document in 0..DOCUMENTS {
        let year = if document % 2 == 0 { "2013" } else { "2005" };
        events.push(Ok(CorpusEvent::DocumentStart {
            source_name: "bench".to_owned(),
            metadata: metadata_from_json(
                serde_json::json!({"year": year, "subject_area": "社会学"}),
                "bench",
            )
            .expect("bench metadata types"),
        }));
        events.push(Ok(CorpusEvent::ParagraphStart {
            metadata: Box::new([]),
        }));
        for sentence in 0..SENTENCES_PER_DOCUMENT {
            let active_labels: Box<[String]> = if labelled {
                let label = if sentence % 2 == 0 {
                    "methods"
                } else {
                    "results"
                };
                Box::new([label.to_owned()])
            } else {
                Box::new([])
            };
            events.push(Ok(CorpusEvent::Sentence {
                text: format!("これは文{sentence}である"),
                active_labels,
                metadata: Box::new([]),
                words: None,
                enhanced_graph: None,
            }));
        }
        events.push(Ok(CorpusEvent::ParagraphEnd));
        events.push(Ok(CorpusEvent::DocumentEnd));
    }
    events
}

fn drain(corpus: impl Iterator<Item = std::io::Result<AdmittedCorpusEvent>>) -> usize {
    corpus
        .filter(|event| {
            event
                .as_ref()
                .is_ok_and(|event| matches!(event, AdmittedCorpusEvent::Sentence(_)))
        })
        .count()
}

fn admission(criterion: &mut Criterion) {
    let events = corpus_events(true);
    let unlabelled = corpus_events(false);
    let mut group = criterion.benchmark_group("scope_admission");

    // Keep an unlabelled comparison point: otherwise label construction becomes
    // fixture cost in the unscoped measurement.
    group.bench_function("unscoped_unlabelled", |bencher| {
        bencher.iter(|| {
            let corpus = AdmittedCorpus::new(unlabelled.iter().map(clone_event));
            black_box(drain(corpus))
        });
    });

    group.bench_function("unscoped", |bencher| {
        bencher.iter(|| {
            let corpus = AdmittedCorpus::new(events.iter().map(clone_event));
            black_box(drain(corpus))
        });
    });

    group.bench_function("scoped_admits_all", |bencher| {
        let clause = ScopeClause::parse("subject_area = 社会学").expect("clause parses");
        bencher.iter(|| {
            let corpus = AdmittedCorpus::new(events.iter().map(clone_event))
                .with_scope(clause.clone(), ScopeObservation::default());
            black_box(drain(corpus))
        });
    });

    group.bench_function("scoped_admits_half", |bencher| {
        let clause = ScopeClause::parse("year >= 2010").expect("clause parses");
        bencher.iter(|| {
            let corpus = AdmittedCorpus::new(events.iter().map(clone_event))
                .with_scope(clause.clone(), ScopeObservation::default());
            black_box(drain(corpus))
        });
    });

    group.bench_function("scoped_by_section", |bencher| {
        let clause = ScopeClause::parse("@section in {methods, results}").expect("clause parses");
        bencher.iter(|| {
            let corpus = AdmittedCorpus::new(events.iter().map(clone_event))
                .with_scope(clause.clone(), ScopeObservation::default());
            black_box(drain(corpus))
        });
    });

    group.finish();
}

/// The reader consumes owned events, and the corpus is built once outside the
/// timed region, so each iteration clones. The clone is identical across all
/// three cases and so cancels out of the comparison.
fn clone_event(event: &std::io::Result<CorpusEvent>) -> std::io::Result<CorpusEvent> {
    match event {
        Ok(event) => Ok(event.clone()),
        Err(error) => Err(std::io::Error::new(error.kind(), error.to_string())),
    }
}

criterion_group!(benches, admission);
criterion_main!(benches);
