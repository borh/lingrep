//! The web surface seeds its per-pattern descriptors from PUBLISHED names.
//!
//! `SnapshotSink` and `ExportSink` both reject a record naming a pattern they
//! were not seeded with, by design — an unrecognized name means the catalog and
//! the sink disagree, and silently dropping the record would understate every
//! count. Once result policy is read, a record relabeled by `output_name`
//! carries a name `CompiledCatalog::patterns()` does not contain, so seeding from
//! `patterns()` would turn every rename into a failed run.
//!
//! That is the whole hazard this file pins: it is invisible until a catalog
//! declares a rename, and then it fails the run rather than the test.

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_web::pattern_catalog::pattern_metadata_pairs;

fn compile(text: &str) -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("policy.lx", text),
        CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher
}

#[test]
fn descriptors_are_keyed_by_the_name_records_will_carry() {
    let matcher = compile(
        r#"
output_name: "published"
authored := [text=a].
"#,
    );
    let names: Vec<String> = pattern_metadata_pairs(&matcher)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        vec!["published".to_string()],
        "the descriptor must carry the published name, not the authored one"
    );
}

#[test]
fn definitions_sharing_a_published_name_collapse_to_one_descriptor() {
    // The catalog is deliberately merging surface variants under one label, so
    // two descriptors under one name would be two rows nothing can tell apart
    // — and would collide in the sink's name-to-index map.
    let matcher = compile(
        r#"
output_name: "わけ"
わけだ := [text=わけ] [text=だ].

output_name: "わけ"
わけである := [text=わけ] [text=で] [text=ある].
"#,
    );
    let names: Vec<String> = pattern_metadata_pairs(&matcher)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, vec!["わけ".to_string()]);
}

#[test]
fn a_policy_free_catalog_keys_descriptors_by_its_own_names() {
    let matcher = compile("alpha := [text=a].\nbeta := [text=b].\n");
    let names: Vec<String> = pattern_metadata_pairs(&matcher)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
}

#[test]
fn synthesized_emitters_never_become_descriptors() {
    // They are absent from `patterns()` by construction; this pins that the
    // web projection cannot reintroduce them.
    let matcher = compile(
        r#"
fragment COPULA := [lemma=だ].

guarded := [text=a].

policy
  exclude @guarded with: @COPULA, relation: within, evidence: raw.
end
"#,
    );
    assert!(
        pattern_metadata_pairs(&matcher)
            .iter()
            .all(|(name, _)| !name.starts_with("__policy__")),
        "a synthesized emitter must never reach the pattern browser"
    );
}

#[test]
fn ordinary_patterns_precede_lexicons_and_supply_shared_scalar_metadata() {
    use lingrep_web::snapshot::PatternMetadataValue::{Bool, Number, String as Text};
    let matcher = compile(
        r#"
output_name: "shared"
label: "lexicon"
lexicon early by [text] := {"a"}.

output_name: "shared"
label: "pattern"
flag: true
score: 1.25
list_value: ["hidden"]
map_value: {nested: "hidden"}
pattern later := [text=a].

pattern second := [text=b].
lexicon last by [text] := {"c"}.
"#,
    );
    let pairs = pattern_metadata_pairs(&matcher);
    assert_eq!(
        pairs
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["shared", "second", "last"]
    );
    assert_eq!(
        pairs[0].1.as_ref(),
        &[
            ("flag".into(), Bool(true)),
            ("label".into(), Text("pattern".into())),
            ("output_name".into(), Text("shared".into())),
            ("score".into(), Number("1.25".into())),
        ]
    );
}
