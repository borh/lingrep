//! What a section clause adds to a prepared run's scope resolution.
//!
//! This measures [`resolve_scope`] directly so full-run variance and unrelated
//! decoding work do not obscure the section-resolution cost. Performance bars
//! and measurements live in `docs/PERFORMANCE_REQUIREMENTS.md`.
//!
//! # The fixture
//!
//! Shaped from the acceptance artifact — 1,534 documents, 355,824 sentences,
//! 8,377 region spans over 16 labels, 236 bytes of frontmatter JSON per
//! document, 0.99 labels per sentence. The generator reproduces those
//! **aggregates**, not the corpus: sentence counts are scaled from a uniform
//! draw, so the real 12..1,477 tail is absent. Resolution is linear in the
//! document and sentence totals and asks one bitmap question per sentence, so
//! the tail is not what the cost depends on.
//!
//! Only the labels a clause *names* are fetched by `resolve_prepared_scope`,
//! so each case is given exactly the spans its clause would have received.

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

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lingrep::scope_precompute::resolve_scope;
use lingrep_core::corpus::DocumentId;
use lingrep_core::scope::ScopeClause;
use lingrep_corpus::{DocumentMetadataRow, RegionSpanRow};

/// Documents in the acceptance artifact.
const DOCUMENTS: u64 = 1_534;
/// Sentences in the acceptance artifact.
const SENTENCES: u64 = 355_824;

/// One label's measured span count and sentence coverage.
const METHODS: (&str, u64, u64) = ("methods", 1_162, 59_482);
const RESULTS: (&str, u64, u64) = ("results", 1_569, 149_117);

/// A deterministic draw. `rand` is a dependency here, but a benchmark that
/// reseeds per run cannot be compared against its own history.
fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

/// Sentence counts summing to exactly [`SENTENCES`] over [`DOCUMENTS`].
fn sentence_counts() -> Vec<u64> {
    let mut state = 0x5eed_1234_5678_9abc_u64;
    let mut counts: Vec<u64> = (0..DOCUMENTS)
        .map(|_| 12 + next(&mut state) % 1_466)
        .collect();
    let raw: u64 = counts.iter().sum();
    for count in &mut counts {
        *count = (*count * SENTENCES / raw).max(12);
    }
    // Correct the rounding residual one sentence at a time rather than dumping
    // it on one document, which would give that document an unrepresentative
    // share of the per-sentence walk.
    let mut residual = SENTENCES - counts.iter().sum::<u64>();
    let documents = counts.len();
    let mut index = 0;
    while residual > 0 {
        counts[index % documents] += 1;
        residual -= 1;
        index += 1;
    }
    counts
}

/// One document's frontmatter, in the shape the corpus records it: ten keys,
/// one of them a list, ~236 bytes. `year` splits the corpus roughly in half so
/// the metadata-only baseline exercises both verdicts.
fn metadata_json(index: u64) -> String {
    let year = 1_995 + index % 30;
    format!(
        r#"{{"author":"著者{index}","basename":"{index:05}","corpus":"人文社会学論文","journal_name":"村落社会研究ジャーナル","keywords":["農学・食品科学","人類学・史学・地理学","経済学・経営学","社会学"],"number":"2","permission":false,"subject_area":"社会学","title":"標題","volume":"19","year":"{year}"}}"#
    )
}

fn rows() -> Vec<DocumentMetadataRow> {
    let mut first_sentence = 0;
    sentence_counts()
        .into_iter()
        .enumerate()
        .map(|(index, sentence_count)| {
            let index = index as u64;
            let row = DocumentMetadataRow {
                document_id: DocumentId::new(index),
                first_sentence,
                sentence_count,
                metadata_json: Some(metadata_json(index)),
            };
            first_sentence += sentence_count;
            row
        })
        .collect()
}

/// `count` non-overlapping spans totalling `coverage` sentences, spread evenly
/// over the ordinal space — the shape a section label has in a corpus where
/// most but not all documents carry one.
fn spans((label, count, coverage): (&str, u64, u64)) -> Vec<RegionSpanRow> {
    let stride = SENTENCES / count;
    let length = (coverage / count).clamp(1, stride);
    (0..count)
        .map(|index| {
            let first_sentence = index * stride;
            RegionSpanRow {
                label: label.to_owned(),
                first_sentence,
                end_sentence: first_sentence + length,
            }
        })
        .collect()
}

fn bench(criterion: &mut Criterion) {
    let rows = rows();
    let methods = spans(METHODS);
    let mut union = spans(METHODS);
    union.extend(spans(RESULTS));

    let mut group = criterion.benchmark_group("scope_resolution");
    // Resolution runs once per run, so a sample that reports per-iteration
    // milliseconds directly is what the bar is stated in.
    group.sample_size(20);

    // Each case states the fraction of the corpus its clause admits, and the
    // harness checks it before timing anything. A fixture whose section spans
    // missed the sentences they were meant to cover would still produce a
    // plausible number, and the number would be of the wrong thing.
    let mut case = |name: &str, clause: &str, spans: &[RegionSpanRow], admitted: u64| {
        let parsed = ScopeClause::parse(clause).unwrap();
        let resolved = resolve_scope(
            &parsed,
            rows.iter().cloned().map(Ok),
            spans,
            std::iter::empty(),
        )
        .unwrap();
        assert_eq!(
            resolved.sentences.admitted, admitted,
            "{name}: the fixture admits {} sentences, not the {admitted} this case is about",
            resolved.sentences.admitted
        );
        assert_eq!(resolved.sentences.total().unwrap(), SENTENCES);
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                resolve_scope(
                    black_box(&parsed),
                    black_box(&rows).iter().cloned().map(Ok),
                    black_box(spans),
                    std::iter::empty(),
                )
            });
        });
    };

    // The baseline the bar is measured against: decided once per document,
    // and — as `resolve_prepared_scope` does — given no spans at all.
    case("document_grain", "@document.year >= 2010", &[], 179_996);
    // The per-sentence walk with nothing to find, which is what the CLI
    // comparison measured. Its clause names a label the corpus does not
    // record, so it too receives no spans.
    case("section_absent_label", "@section = absent_label", &[], 0);
    // The realistic single-label case: 1,162 spans, 16.7% of the corpus.
    case("section_methods", "@section = methods", &methods, 59_262);
    // The acceptance clause: two labels and a metadata predicate together. It
    // admits 26% here against 54.1% on the real artifact, because the fixture
    // splits `year` evenly while the corpus is mostly recent. That changes how
    // many sentences are *admitted*, not how many are walked — every sentence
    // of an in-scope document is evaluated either way.
    case(
        "section_union_and_metadata",
        "@section in {methods, results} & @document.year >= 2010",
        &union,
        92_590,
    );

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
