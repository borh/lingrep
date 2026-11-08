#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lingrep::output::{
    ColorPolicy, ContextColorPalette, HumanKwicRow, HumanMatchLayout, HumanRenderer,
    ResolvedContextColorTheme, TerminalColorCapability,
};
use lingrep::presentation::ColorMode;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn bench(criterion: &mut Criterion) {
    let row = HumanKwicRow {
        left: ["日本", "語", "の", "形態", "素"].map(String::from).into(),
        keyword: vec!["解析".into()],
        right: ["を", "比較", "する", "研究", "。"]
            .map(String::from)
            .into(),
    };
    let mut group = criterion.benchmark_group("human_output");
    group.throughput(Throughput::Elements(1));
    for (name, capability, mode) in [
        (
            "plain",
            TerminalColorCapability::Truecolor,
            ColorMode::Never,
        ),
        (
            "truecolor",
            TerminalColorCapability::Truecolor,
            ColorMode::Always,
        ),
        (
            "ansi256",
            TerminalColorCapability::Ansi256,
            ColorMode::Always,
        ),
        ("ansi16", TerminalColorCapability::Ansi16, ColorMode::Always),
    ] {
        let renderer = HumanRenderer::new(
            ContextColorPalette::new(5, ResolvedContextColorTheme::Dark, capability),
            ColorPolicy::resolve(mode, false, true),
            80,
            HumanMatchLayout::Flowing,
            "".parse().unwrap(),
        );
        let mut output = Vec::with_capacity(1024);
        group.bench_with_input(BenchmarkId::new("row", name), &row, |bencher, row| {
            bencher.iter(|| {
                output.clear();
                renderer.match_row(&mut output, black_box(row)).unwrap();
                black_box(&output);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
