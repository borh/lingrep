use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

#[test]
fn an_unconstrained_catalog_is_silent() {
    let output = compile_catalog(
        PatternSource::named("test.lx", "pattern p := [text=猫].\n"),
        CompileLimits::default(),
    )
    .unwrap();
    assert!(output.matcher.analyzer_constraints().is_empty());
}
