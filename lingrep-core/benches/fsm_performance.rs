// Exempt benchmark code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use lingrep_core::fsm::{BoundCatalog, CompileLimits, PatternSource, compile_catalog};
use lingrep_core::token_match::{BoundTokenSentence, ProjectedAttributeValue, TokenSchema};

type Row = Vec<(String, String)>;

fn compile(source: &str) -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("fsm-performance.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

fn row(values: &[(&str, &str)]) -> Row {
    values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn bind_input(
    matcher: &lingrep_core::fsm::CompiledCatalog,
    rows: &[Row],
) -> (BoundCatalog, BoundTokenSentence) {
    let schema = TokenSchema::unidic_morphology();
    let bound = matcher.bind(&schema).unwrap();
    let values = rows
        .iter()
        .flat_map(|row| {
            bound.projection().keys().iter().map(move |key| {
                row.iter()
                    .find(|(candidate, _)| candidate == key.as_str())
                    .map_or_else(ProjectedAttributeValue::missing_scalar, |(_, value)| {
                        ProjectedAttributeValue::scalar(value)
                    })
            })
        })
        .collect();
    let tokens =
        BoundTokenSentence::from_projected_attribute_values(bound.projection(), rows.len(), values)
            .unwrap();
    (bound, tokens)
}

fn bench_case(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: impl Into<String>,
    matcher: lingrep_core::fsm::CompiledCatalog,
    rows: &[Row],
) {
    let (bound, tokens) = bind_input(&matcher, rows);
    group.bench_function(name.into(), |b| {
        b.iter(|| black_box(bound.find_matches(black_box(&tokens)).unwrap()))
    });
}

fn cutover_reference(c: &mut Criterion) {
    let matcher = compile("verb := [upos=VERB].");
    let rows = (0..1_000)
        .map(|index| row(&[("upos", if index % 2 == 0 { "VERB" } else { "NOUN" })]))
        .collect::<Vec<_>>();
    let (bound, tokens) = bind_input(&matcher, &rows);
    c.bench_function("cutover_reference", |b| {
        b.iter(|| black_box(bound.find_matches(black_box(&tokens)).unwrap()))
    });
}

fn regular_foundations(c: &mut Criterion) {
    let noun = (0..1_000)
        .map(|_| row(&[("upos", "NOUN")]))
        .collect::<Vec<_>>();
    let assertion = (0..1_000)
        .map(|index| {
            if index % 2 == 0 {
                row(&[("text", "する"), ("unidic.cform", "終止形-一般")])
            } else {
                row(&[("text", "そう")])
            }
        })
        .collect::<Vec<_>>();
    let finite = (0..1_000)
        .map(|index| row(&[("text", if index % 2 == 0 { "ある" } else { "ない" })]))
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("pattern_regular_foundations");
    group.throughput(Throughput::Elements(1_000));
    bench_case(
        &mut group,
        "anchored",
        compile("p := ^ [upos=NOUN]+ $."),
        &noun,
    );
    bench_case(
        &mut group,
        "preceded_by",
        compile("p := preceded_by([unidic.cform != 連用形-一般]) [text=そう]."),
        &assertion,
    );
    let set = "p := [text in {ある, 有る, あり, 有り, あっ, 有っ, あら, 有ら}].";
    bench_case(&mut group, "finite_set", compile(set), &finite);
    bench_case(
        &mut group,
        "literal_regex",
        compile("p := [text~/^(?:ある|有る|あり|有り|あっ|有っ|あら|有ら)$/]."),
        &finite,
    );
    group.finish();
}

fn capture_and_equality(c: &mut Criterion) {
    let mut rows = (0..999).map(|_| row(&[("text", "a")])).collect::<Vec<_>>();
    rows.push(row(&[("text", "z")]));
    let mut group = c.benchmark_group("pattern_capture_and_equality");
    group.throughput(Throughput::Elements(1_000));
    bench_case(
        &mut group,
        "plain",
        compile("p := [text=a]+ [text=z]."),
        &rows,
    );
    bench_case(
        &mut group,
        "named",
        compile("p := (?<unit> [text=a]+) [text=z]."),
        &rows,
    );
    for width in [1_usize, 4, 8] {
        let source = format!("p := (?<unit> [text=a]{{{width}}}) same_as($unit).");
        bench_case(
            &mut group,
            format!("same-as/w{width}"),
            compile(&source),
            &rows,
        );
    }
    group.finish();
}

fn lookaround(c: &mut Criterion) {
    let short = (0..128)
        .map(|index| row(&[("text", if index % 7 == 0 { "b" } else { "a" })]))
        .collect::<Vec<_>>();
    let long = (0..4_096)
        .map(|index| row(&[("text", if index % 31 == 0 { "b" } else { "a" })]))
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("pattern_regular_lookaround");
    for (name, source) in [
        (
            "preceded",
            "p := preceded_by(([text=a] [text=b]?)) [text=a].",
        ),
        (
            "not-preceded",
            "p := not_preceded_by(([text=a]+)) [text=b].",
        ),
        (
            "followed",
            "p := [text=a] followed_by(([text=a] [text=b]?)).",
        ),
        (
            "not-followed",
            "p := [text=b] not_followed_by(([text=a]+)).",
        ),
        (
            "nested",
            "p := [text=a] followed_by(([text=a] preceded_by(([text=a]+)))).",
        ),
    ] {
        group.throughput(Throughput::Elements(short.len() as u64));
        bench_case(&mut group, name, compile(source), &short);
    }
    group.throughput(Throughput::Elements(long.len() as u64));
    bench_case(
        &mut group,
        "long-adversarial",
        compile("p := [text=a] followed_by(([text=a]+ [text=b]?))."),
        &long,
    );
    group.finish();
}

fn roadmap(c: &mut Criterion) {
    let mut group = c.benchmark_group("pattern_matcher_roadmap");
    for (name, source, modulus) in [
        ("no-match", "p := [text=z].".to_owned(), 1),
        ("dense-match", "p := [text=a].".to_owned(), 1),
        (
            "predicate-512",
            (0..512)
                .map(|index| format!("p{index} := [text=v{index}]."))
                .collect::<Vec<_>>()
                .join("\n"),
            512,
        ),
    ] {
        let rows = (0..1_000)
            .map(|index| {
                let value = if name == "dense-match" {
                    "a".to_owned()
                } else {
                    format!("v{}", index % modulus)
                };
                row(&[("text", &value)])
            })
            .collect::<Vec<_>>();
        group.throughput(Throughput::Elements(rows.len() as u64));
        bench_case(&mut group, name, compile(&source), &rows);
    }
    group.finish();
}

criterion_group!(
    benches,
    cutover_reference,
    regular_foundations,
    capture_and_equality,
    lookaround,
    roadmap
);
criterion_main!(benches);
