//! V1 removes the ambiguous per-definition `dictionary:` annotation.

use lingrep_core::fsm::{CompileLimits, DiagnosticCode, PatternSource, compile_catalog};

#[test]
fn every_dictionary_annotation_is_a_hard_compile_error() {
    for source in [
        "dictionary: \"unidic-cwj-202512\"\npattern p := [text=猫].\n",
        "dictionary: 7\npattern p := [text=猫].\n",
        "dictionary: [\"unidic-cwj-202512\", \"unidic-csj-202512\"]\npattern p := [text=猫].\n",
    ] {
        let error = match compile_catalog(
            PatternSource::named("test.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("retired dictionary metadata must not compile"),
            Err(error) => error,
        };
        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::Syntax);
        assert!(
            error.to_string().contains("retired `dictionary:`"),
            "{error}"
        );
    }
}

#[test]
fn an_unconstrained_catalog_is_silent() {
    let output = compile_catalog(
        PatternSource::named("test.lx", "pattern p := [text=猫].\n"),
        CompileLimits::default(),
    )
    .unwrap();
    assert!(output.matcher.analyzer_constraints().is_empty());
}

#[test]
fn similarly_spelled_open_metadata_does_not_recreate_the_retired_contract() {
    let output = compile_catalog(
        PatternSource::named(
            "test.lx",
            "dictionry: \"unidic-cwj-202512\"\npattern p := [text=猫].\n",
        ),
        CompileLimits::default(),
    )
    .unwrap();
    assert!(output.matcher.analyzer_constraints().is_empty());
}
