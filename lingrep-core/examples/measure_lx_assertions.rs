#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use compact_str::CompactString;
use lingrep_core::fsm::{
    BoundCatalog, CompileLimits, CompiledCatalog, PatternSource, compile_catalog,
};
use lingrep_core::token_match::{AttributeKind, BoundTokenSentence, SchemaAttribute, TokenSchema};

fn compile(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("assertion-metrics.lx", source),
        CompileLimits::default(),
    )
    .expect("measurement lingrep patterns must compile")
    .matcher
}

fn bind(matcher: &CompiledCatalog) -> (BoundCatalog, lingrep_core::token_match::BoundProjection) {
    let schema = TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar)
        .expect("measurement schema key must be valid")])
    .expect("measurement schema must be valid");
    let bound = matcher
        .bind(&schema)
        .expect("measurement catalog must bind");
    let projection = bound.projection().clone();
    (bound, projection)
}

fn report(name: &str, matcher: &CompiledCatalog, values: &[CompactString]) {
    let (matcher, projection) = bind(matcher);
    let tokens = BoundTokenSentence::from_projected_values(
        &projection,
        values.len(),
        values.iter().cloned().map(Some).collect(),
    )
    .expect("measurement sentence must bind");
    let (matches, metrics) = matcher
        .find_matches_with_assertion_metrics(&tokens)
        .expect("measurement sentence schema must match");
    println!(
        "{name}\ttokens={}\tmatches={}\trow_queries={}\trow_materializations={}\tstate_visits={}",
        tokens.len(),
        matches.len(),
        metrics.row_queries,
        metrics.row_materializations,
        metrics.state_visits,
    );
}

fn main() {
    let tokens = (0..128)
        .map(|index| CompactString::new(if index % 7 == 0 { "b" } else { "a" }))
        .collect::<Vec<_>>();

    for count in [1_usize, 8, 32] {
        let source = (0..count)
            .map(|index| {
                format!("pattern p{index} := [text=a] followed_by(([text=v{index}] [text=a]?)).")
            })
            .collect::<Vec<_>>()
            .join("\n");
        report(
            &format!("distinct-bodies/{count}"),
            &compile(&source),
            &tokens,
        );
    }

    for (name, source) in [
        (
            "capture-consumer",
            "p := (?<unit> [text=a] followed_by(([text=a]?))) [text=b].",
        ),
        (
            "span-equality-consumer",
            "p := (?<unit> [text=a] followed_by(([text=a]?))) same_as($unit).",
        ),
    ] {
        report(name, &compile(source), &tokens);
    }
}
