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
use lingrep_core::execution::{BoundSentenceInput, EnhancedGraphInput};
use lingrep_core::fsm::policy_program::BoundOp;
use lingrep_core::fsm::{
    COMPILER_SCHEMA_IDENTITY, CompileLimits, CompileOutput, DiagnosticCode, FsmBuilder,
    GRAMMAR_SEMANTICS_REVISION, Match, POLICY_SEMANTICS_IDENTITY, PatternSource, PredicateDigest,
    compile_catalog, report_catalog_drift,
};
use lingrep_core::testing::catalogs::tracked_catalog_sources;
use lingrep_core::token_match::{
    AttributeKey, AttributeValueRef, BoundProjection, BoundTokenSentence, ProjectedAttributeValue,
    TokenAttributeIndex, TokenInfo, TokenInfoTrait, TokenSequence,
};

use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Debug, PartialEq, Eq)]
struct CompilerStatsFingerprint {
    definitions: usize,
    expression_nodes: usize,
    max_depth: usize,
    reference_edges: usize,
    nfa_states: usize,
    transitions: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct CatalogFingerprint {
    source: String,
    definitions: usize,
    catalog: Vec<(String, Vec<(String, String)>)>,
    /// The ordered policy program, one rendered statement per authored
    /// operation. Empty for a catalog declaring no `policy` block.
    policy: Vec<String>,
    required_attributes: Vec<String>,
    /// Every compiled token predicate, in interning order.
    ///
    /// The counts below are blind to what a predicate tests and to the order
    /// its attributes are tested in; this is not.
    predicates: PredicateDigest,
    compiler_stats: CompilerStatsFingerprint,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct MatchFingerprint {
    pattern: String,
    start: usize,
    end: usize,
    captures: Vec<(String, String, usize, usize)>,
}

fn token_with_attrs(values: &[(TokenAttributeIndex, &str)]) -> TokenInfo {
    let mut token = TokenInfo::default();
    for (attribute, value) in values {
        token.set(*attribute, (*value).into());
    }
    token
}

fn double_wo_token(orth_base: &str, pos1: &str, pos2: &str) -> TokenInfo {
    double_wo_token_with_orth(orth_base, orth_base, pos1, pos2)
}

/// Build a double-wo token whose surface and base form differ.
///
/// The catalog's fragment boundary reads `orth` and its gap tests read
/// `orthBase`; a helper that forces them equal makes that difference
/// untestable. Every test that does not care passes the same value twice
/// through `double_wo_token`.
fn double_wo_token_with_orth(orth: &str, orth_base: &str, pos1: &str, pos2: &str) -> TokenInfo {
    token_with_attrs(&[
        (TokenAttributeIndex::Orth, orth),
        (TokenAttributeIndex::OrthBase, orth_base),
        (TokenAttributeIndex::Pos1, pos1),
        (TokenAttributeIndex::Pos2, pos2),
    ])
}

fn double_wo_span_multiset(matches: &[Match]) -> Vec<(usize, usize)> {
    let mut spans = matches
        .iter()
        .map(|matched| (matched.start, matched.end))
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans
}

fn match_fingerprints(matches: &[Match]) -> Vec<MatchFingerprint> {
    matches
        .iter()
        .map(|matched| MatchFingerprint {
            pattern: matched.pattern.to_string(),
            start: matched.start,
            end: matched.end,
            captures: matched
                .captures
                .iter()
                .map(|capture| {
                    (
                        capture.name.to_string(),
                        capture.path.to_string(),
                        capture.start,
                        capture.end,
                    )
                })
                .collect(),
        })
        .collect()
}

#[test]
fn public_pattern_identity_constants_are_pinned() {
    assert_eq!(
        GRAMMAR_SEMANTICS_REVISION,
        "b7ac30d8055e0e4e22ce4e4a34711f27f07712fc"
    );
    assert_eq!(COMPILER_SCHEMA_IDENTITY, "lingrep-core-compiler/v1");
    // Pinned for the same reason as its sibling: these constants are the only
    // thing distinguishing two runs over identical source that publish
    // different records, so a change to one has to be a change someone wrote
    // down rather than a change someone noticed later.
    assert_eq!(POLICY_SEMANTICS_IDENTITY, "lingrep-core-policy/v1");
}

#[test]
fn repository_catalog_match_fingerprints_pin_pre_lookaround_semantics() {
    let cases = [
        (
            "sfe",
            include_str!("../../../patterns/sfe.lx"),
            vec![
                token_with_attrs(&[
                    (TokenAttributeIndex::Lemma, "必要"),
                    (TokenAttributeIndex::Orth, "必要"),
                ]),
                token_with_attrs(&[(TokenAttributeIndex::Orth, "が")]),
                token_with_attrs(&[
                    (TokenAttributeIndex::Lemma, "ある"),
                    (TokenAttributeIndex::Orth, "ある"),
                ]),
            ],
        ),
        (
            "sf-experimental",
            include_str!("../../../patterns/sf-experimental.lx"),
            vec![
                token_with_attrs(&[
                    (TokenAttributeIndex::Orth, "する"),
                    (TokenAttributeIndex::Lemma, "為る"),
                    (TokenAttributeIndex::CForm, "終止形-一般"),
                ]),
                token_with_attrs(&[(TokenAttributeIndex::Orth, "そう")]),
                token_with_attrs(&[
                    (TokenAttributeIndex::Orth, "だ"),
                    (TokenAttributeIndex::Lemma, "だ"),
                ]),
            ],
        ),
    ];

    let fingerprints = cases
        .into_iter()
        .map(|(name, source, tokens)| {
            let matcher =
                compile_catalog(PatternSource::named(name, source), CompileLimits::default())
                    .expect("repository catalog must compile")
                    .matcher;
            let matches = matcher.find_matches(&tokens);
            assert!(!matches.is_empty(), "{name} fixture must be non-vacuous");
            (name, match_fingerprints(&matches))
        })
        .collect::<Vec<_>>();

    insta::assert_debug_snapshot!(fingerprints);
}

/// `patterns/sfe.lx`'s `ARU` covers the polite and past tail it names.
///
/// The catalog's own `counterexamples:` pin the other half of that fragment —
/// that `あろう` and `あれ` are excluded — but a counterexample is checked
/// match/no-match, so nothing there can see a *span*. This is the half that
/// needs a span: `まし? た?` exists so `必要があった` reports the whole form
/// rather than stopping at `必要があっ` and dropping the tense marker off the
/// end of the record.
///
/// Synthetic tokens for the same reason the fingerprint fixture above uses
/// them: the claim is about which tokens the fragment consumes, and routing it
/// through a dictionary would make a segmentation change look like a
/// regression here.
#[test]
fn sfe_aru_consumes_the_past_tense_tail() {
    let matcher = compile_catalog(
        PatternSource::named("patterns/sfe.lx", include_str!("../../../patterns/sfe.lx")),
        CompileLimits::default(),
    )
    .expect("repository catalog must compile")
    .matcher;

    let tokens = [
        token_with_attrs(&[(TokenAttributeIndex::Lemma, "必要")]),
        token_with_attrs(&[(TokenAttributeIndex::Orth, "が")]),
        token_with_attrs(&[
            (TokenAttributeIndex::Lemma, "ある"),
            (TokenAttributeIndex::Orth, "あっ"),
        ]),
        token_with_attrs(&[(TokenAttributeIndex::Orth, "た")]),
    ];

    let spans: Vec<(usize, usize)> = matcher
        .find_matches(&tokens)
        .iter()
        .filter(|found| found.pattern == "必要がある")
        .map(|found| (found.start, found.end))
        .collect();
    assert_eq!(
        spans,
        vec![(0, 4)],
        "the record covers 必要があった, not the 必要があっ prefix"
    );
}

/// The unfiltered double-を language, written independently of
/// `patterns/double-wo.lx` so the catalog can be checked against it. Shared by
/// every test that needs the raw reference; two copies could drift apart
/// silently.
const RAW_DOUBLE_WO: &str = r#"
fragment wo := [unidic.orthbase=を & unidic.pos1=助詞 & unidic.pos2=格助詞].
fragment anchor := [unidic.pos1!=動詞 & unidic.pos1!=助動詞 & unidic.pos1!=補助記号].
fragment gap :=
  [unidic.orthbase!=を & unidic.pos1!=動詞 & unidic.pos1!=助動詞 & unidic.pos1!=補助記号].
pattern raw := @wo @anchor (@gap){0,8} @wo @anchor.
"#;

#[test]
fn double_wo_catalog_partitions_exact_raw_matches() {
    let wo = || double_wo_token("を", "助詞", "格助詞");
    let noun = |orth| double_wo_token(orth, "名詞", "普通名詞-一般");
    let particle = |orth| double_wo_token(orth, "助詞", "係助詞");
    let cases = [
        (
            "adverbial",
            vec![
                wo(),
                noun("対象"),
                particle("に"),
                noun("調査"),
                wo(),
                noun("実施"),
            ],
            "double_wo_exclude_adverbial_anchor",
        ),
        (
            "ideographic-space-wins-over-adverbial",
            vec![
                wo(),
                noun("対象"),
                double_wo_token("\u{3000}", "空白", "*"),
                noun("調査"),
                wo(),
                noun("実施"),
            ],
            "double_wo_exclude_fragment_boundary",
        ),
        (
            "ascii-space-is-not-a-fragment-boundary",
            vec![
                wo(),
                noun("住所"),
                double_wo_token(" ", "空白", "*"),
                noun("欄"),
                wo(),
                noun("備考"),
            ],
            "double_wo_candidate",
        ),
        (
            "focus",
            vec![wo(), particle("も"), noun("世"), wo(), noun("見る")],
            "double_wo_exclude_focus_marker",
        ),
        (
            "lexicalized",
            vec![wo(), noun("首"), noun("長く"), wo(), noun("待つ")],
            "double_wo_exclude_lexicalized_anchor",
        ),
        (
            "directional-frame",
            vec![
                wo(),
                noun("上"),
                particle("に"),
                noun("左手"),
                wo(),
                noun("下"),
            ],
            "double_wo_exclude_directional_frame",
        ),
        (
            "adjective-after-anchor-stays-candidate",
            vec![
                wo(),
                noun("いずれ"),
                {
                    let mut token = double_wo_token("近い", "形容詞", "一般");
                    token.set(TokenAttributeIndex::CForm, "連体形-一般".into());
                    token
                },
                noun("うち"),
                wo(),
                noun("確認"),
            ],
            "double_wo_candidate",
        ),
        (
            "malformed",
            vec![wo(), noun("り"), noun("列"), wo(), noun("項目")],
            "double_wo_exclude_malformed_first_anchor",
        ),
        (
            "candidate",
            vec![wo(), noun("住所"), noun("欄"), wo(), noun("備考")],
            "double_wo_candidate",
        ),
        (
            "second-anchor-evidence-stays-candidate",
            vec![wo(), noun("住所"), noun("欄"), wo(), noun("首")],
            "double_wo_candidate",
        ),
        (
            "three-wo-greedy-endpoint",
            vec![
                wo(),
                noun("住所"),
                wo(),
                noun("対象"),
                noun("欄"),
                wo(),
                noun("備考"),
            ],
            "double_wo_candidate",
        ),
    ];

    let raw_matcher = compile_catalog(
        PatternSource::named("raw-double-wo.lx", RAW_DOUBLE_WO),
        CompileLimits::default(),
    )
    .expect("raw double-wo reference must compile")
    .matcher;
    let catalog = compile_catalog(
        PatternSource::named(
            "patterns/double-wo.lx",
            include_str!("../../../patterns/double-wo.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("double-wo catalog must compile")
    .matcher;

    for (label, tokens, expected_at_zero) in cases {
        let raw = raw_matcher.find_matches(&tokens);
        let classified = catalog
            .find_matches(&tokens)
            .into_iter()
            .filter(|matched| matched.pattern.as_str().starts_with("double_wo_"))
            .collect::<Vec<_>>();

        assert_eq!(
            double_wo_span_multiset(&classified),
            double_wo_span_multiset(&raw),
            "{label}: classified spans must equal raw spans"
        );

        let mut by_span = BTreeMap::<(usize, usize), Vec<&str>>::new();
        for matched in &classified {
            by_span
                .entry((matched.start, matched.end))
                .or_default()
                .push(matched.pattern.as_str());
        }
        for ((start, end), names) in by_span {
            assert_eq!(
                names.len(),
                1,
                "{label}: ({start}, {end}) must have exactly one classification: {names:?}"
            );
        }

        let at_zero = classified
            .iter()
            .filter(|matched| matched.start == 0)
            .collect::<Vec<_>>();
        assert_eq!(
            at_zero.len(),
            1,
            "{label}: start zero must have one classification"
        );
        assert_eq!(at_zero[0].pattern.as_str(), expected_at_zero, "{label}");
    }
}

/// Class S: a U+3000 that the dictionary did NOT tag 空白. It is a
/// `double_wo_gap` token, so `RAW_DOUBLE_WO` matches across it, but it is
/// excluded from `double_wo_non_boundary_gap` (which now rejects it via
/// `!@double_wo_boundary`, a complement over `orth`, where it once spelled a
/// second independent `orthBase` exclusion) and from
/// `double_wo_fragment_boundary` (which requires unidic.pos1=空白).
///
/// Characterization only: this records observed behaviour at a token class the
/// partition cases do not cover. It is deliberately separate from
/// `double_wo_catalog_partitions_exact_raw_matches` so that whatever it finds
/// cannot weaken the established partition contract.
#[test]
fn double_wo_classification_at_ideographic_space_not_tagged_blank() {
    let tokens = vec![
        double_wo_token("を", "助詞", "格助詞"),
        double_wo_token("氏名", "名詞", "普通名詞-一般"),
        double_wo_token("\u{3000}", "名詞", "普通名詞-一般"),
        double_wo_token("欄", "名詞", "普通名詞-一般"),
        double_wo_token("を", "助詞", "格助詞"),
        double_wo_token("住所", "名詞", "普通名詞-一般"),
    ];

    let raw_matcher = compile_catalog(
        PatternSource::named("raw-double-wo", RAW_DOUBLE_WO),
        CompileLimits::default(),
    )
    .expect("raw double-wo reference must compile")
    .matcher;
    let catalog = compile_catalog(
        PatternSource::named(
            "patterns/double-wo.lx",
            include_str!("../../../patterns/double-wo.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("double-wo catalog must compile")
    .matcher;

    let raw = raw_matcher.find_matches(&tokens);
    let classified = catalog
        .find_matches(&tokens)
        .into_iter()
        .filter(|matched| matched.pattern.as_str().starts_with("double_wo_"))
        .collect::<Vec<_>>();

    insta::assert_debug_snapshot!((
        double_wo_span_multiset(&raw),
        double_wo_span_multiset(&classified),
        classified
            .iter()
            .map(|matched| (matched.pattern.to_string(), matched.start, matched.end))
            .collect::<Vec<_>>(),
    ));
}

/// The fragment boundary reads `text`, not `unidic.orthbase`, and the
/// difference is observable. `text` is the token surface; `unidic.orthbase` is
/// a dictionary base form, and on a token built without the surface fallback
/// in `token_match::featurize` it can be absent entirely — a missing attribute
/// satisfies neither polarity, making `[orthBase=…]` match nothing. The token
/// built below keeps that case reachable as a unit, which is what pins the
/// catalog's choice of attribute rather than merely its current behaviour.
#[test]
fn double_wo_fragment_boundary_reads_orth_not_orth_base() {
    let wo = || double_wo_token("を", "助詞", "格助詞");
    let noun = |orth: &str| double_wo_token(orth, "名詞", "普通名詞-一般");
    // No four-argument helper can express a *missing* attribute, so this one
    // is built directly.
    let space_without_orth_base = token_with_attrs(&[
        (TokenAttributeIndex::Orth, "\u{3000}"),
        (TokenAttributeIndex::Pos1, "空白"),
    ]);

    let tokens = vec![
        wo(),
        noun("予防"),
        space_without_orth_base,
        noun("高血圧"),
        wo(),
        noun("抑制"),
    ];

    let catalog = compile_catalog(
        PatternSource::named(
            "patterns/double-wo.lx",
            include_str!("../../../patterns/double-wo.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("double-wo catalog must compile")
    .matcher;

    let classified = catalog
        .find_matches(&tokens)
        .into_iter()
        .filter(|matched| matched.pattern.as_str().starts_with("double_wo_"))
        .map(|matched| matched.pattern.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        classified,
        vec!["double_wo_exclude_fragment_boundary".to_string()],
        "a U+3000 with no orthBase must still be a fragment boundary"
    );
}

/// The witness for the divergence that deriving the gap from the boundary
/// closed: a token whose `orth` is U+3000 while its `orthBase` is present and
/// something else.
///
/// Before the derivation, the boundary tested `orth` and the gap excluded
/// `orthBase`, so this token satisfied *both*. The catalog then reported the
/// same span twice, under `double_wo_exclude_fragment_boundary` **and**
/// `double_wo_candidate` — a violation of the documented partition at the
/// level of what the catalog *specifies*.
/// `double_wo_classification_at_ideographic_space_not_tagged_blank` guards the
/// same property but cannot see this: its tokens come from `double_wo_token`,
/// which forces `orth` and `orthBase` equal.
///
/// The token is constructible but was **not** shown to be tokenizer-reachable,
/// and the evidence says the ordinary case is not: with the pinned UniDic-CWJ,
/// U+3000 tokenizes to `orth == orthBase == U+3000`, `unidic.pos1=空白`,
/// `unidic.lextype=system`. So this pins a specification defect rather than a
/// reproduced misclassification — which is the honest reading, and also the
/// reason the fix is worth having: the two definitions had already drifted onto
/// different attributes with nothing to report it, because their relationship
/// was stated only in the catalog's prose. `!@double_wo_boundary` states it in
/// the language instead.
///
/// This test pins the consequence rather than the spelling: it would fail for
/// any future edit that reintroduced an independent exclusion, however that
/// exclusion were written.
///
/// Real-token U+3000 coverage lives elsewhere and is not replaced by this:
/// `catalog_examples.rs` tokenizes the `examples:` string on this catalog's
/// `double_wo_exclude_fragment_boundary` — `を予防`, U+3000, `高血圧を抑制`
/// — with the real dictionary, and requires that pattern to match it. That
/// one string is the sole place a raw U+3000 is correct in this repository,
/// because it is corpus text the tokenizer has to see; everywhere else,
/// including here, it is spelled.
#[test]
fn double_wo_ideographic_space_with_a_differing_orth_base_is_classified_once() {
    let wo = || double_wo_token("を", "助詞", "格助詞");
    let noun = |orth: &str| double_wo_token(orth, "名詞", "普通名詞-一般");
    let divergent = double_wo_token_with_orth("\u{3000}", "空", "名詞", "普通名詞-一般");

    let tokens = vec![
        wo(),
        noun("氏名"),
        divergent,
        noun("欄"),
        wo(),
        noun("住所"),
    ];

    let catalog = compile_catalog(
        PatternSource::named(
            "patterns/double-wo.lx",
            include_str!("../../../patterns/double-wo.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("double-wo catalog must compile")
    .matcher;

    let classified = catalog
        .find_matches(&tokens)
        .into_iter()
        .filter(|matched| matched.pattern.as_str().starts_with("double_wo_"))
        .map(|matched| (matched.pattern.to_string(), matched.start, matched.end))
        .collect::<Vec<_>>();

    assert_eq!(
        classified,
        vec![("double_wo_exclude_fragment_boundary".to_string(), 0, 6)],
        "a U+3000 whose orthBase differs must reach exactly one bucket; \
         before the gap was derived from the boundary it reached two"
    );
}

/// The shared double-wo token helper must be able to build a token whose
/// `orth` and `orthBase` differ. While it could not, every double-wo test was
/// blind to the distinction the catalog actually depends on — which is how the
/// fragment boundary came to read the wrong attribute.
#[test]
fn the_double_wo_token_helper_can_distinguish_orth_from_orth_base() {
    let token = double_wo_token_with_orth("を", "ヲ", "助詞", "格助詞");

    assert_eq!(
        token
            .get(TokenAttributeIndex::Orth)
            .map(compact_str::CompactString::as_str),
        Some("を")
    );
    assert_eq!(
        token
            .get(TokenAttributeIndex::OrthBase)
            .map(compact_str::CompactString::as_str),
        Some("ヲ")
    );
}

/// A compiled fingerprint of every tracked catalog, in sorted path order.
///
/// The set is discovered rather than listed. The hand-written list this
/// replaced omitted `patterns/sfe.lx` — the one catalog that had already sat
/// broken in the tree — so the snapshot recorded six catalogs while claiming
/// to fingerprint the repository's.
///
/// The snapshot is what pins the per-catalog data here, so discovery costs
/// nothing in strictness: a new catalog changes the snapshot and must be
/// reviewed before it is accepted.
/// A metadata value with every nested map re-keyed into a fixed order.
///
/// `serde_json::Map` is a `BTreeMap` normally and an `IndexMap` when
/// `preserve_order` is on. No crate here requests that feature, but
/// `thirtyfour` — a lingrep-web dev-dependency — does, and Cargo unifies features
/// across the graph. Serializing a nested map directly would therefore snapshot
/// a different string under `cargo test -p lingrep-core` than under
/// `cargo test --workspace`, and the snapshot would record whichever
/// configuration last regenerated it.
///
/// The outer metadata pairs were already sorted for this reason; nested maps
/// were not, and went unnoticed until `patterns/sketch-grammar.lx` became the
/// first tracked catalog to declare one (`key:`). Rebuilding through a
/// `BTreeMap` gives sorted iteration under both map types.
fn canonical_metadata(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), canonical_metadata(value)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_metadata).collect())
        }
        scalar => scalar.clone(),
    }
}

/// One statement, rendered semantically.
///
/// **`PolicySpan` is deliberately absent.** It is a byte range into whatever
/// document happened to carry the block, so rendering it would make a
/// reformatted catalog, an added comment, or a different assembly order change
/// a *semantic* fingerprint — and this is the authority a reader consults to
/// ask whether two catalogs publish alike.
///
/// Each arm names its fields rather than deriving the rendering from `Debug` on
/// the whole operation, which is what keeps the span out by construction rather
/// than by remembering to strip it.
fn render_operation(op: &BoundOp) -> String {
    match op {
        // The partition map is rendered whole: it is compile-time semantics —
        // which records contest which — and two catalogs whose maps differ
        // publish differently, so a rendering that elided it would make a
        // partitioned and an unpartitioned `resolve all` interchangeable here.
        BoundOp::Resolve {
            subject,
            rank,
            partition,
            ..
        } => match partition {
            Some(classes) => format!("resolve {subject:?} partition={classes:?} rank={rank:?}"),
            None => format!("resolve {subject:?} rank={rank:?}"),
        },
        BoundOp::Associate {
            effect,
            subject,
            operand,
            relation,
            evidence,
            ..
        } => format!(
            "{effect:?} {subject:?} with={operand:?} relation={relation:?} evidence={evidence:?}"
        ),
        BoundOp::Admit {
            subject,
            terminal,
            anchor,
            ..
        } => format!("admit {subject:?} terminal={terminal:?} anchor={anchor}"),
    }
}

fn catalog_fingerprint(source: String, output: &CompileOutput) -> CatalogFingerprint {
    let policy = output.matcher.catalog_policy();
    CatalogFingerprint {
        source,
        definitions: output.stats.definitions(),
        catalog: output
            .matcher
            .patterns()
            .iter()
            .map(|(name, pattern)| {
                let mut metadata = pattern
                    .metadata
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.to_string(),
                            serde_saphyr::to_string(&canonical_metadata(value))
                                .expect("metadata value serializes"),
                        )
                    })
                    .collect::<Vec<_>>();
                if let Some(entry) = policy.get(name)
                    && !entry.key.is_empty()
                {
                    metadata.push((
                        "key".to_owned(),
                        entry
                            .key
                            .iter()
                            .map(lingrep_core::fsm::KeyProjection::name)
                            .collect::<Vec<_>>()
                            .join(","),
                    ));
                }
                metadata.sort();
                (name.to_string(), metadata)
            })
            .collect(),
        policy: output
            .matcher
            .catalog_policy()
            .program()
            .ops
            .iter()
            .map(render_operation)
            .collect(),
        required_attributes: output
            .matcher
            .required_attributes()
            .into_iter()
            .map(|attribute| format!("{attribute:?}"))
            .collect(),
        predicates: output.matcher.predicate_digest(),
        compiler_stats: CompilerStatsFingerprint {
            definitions: output.stats.definitions(),
            expression_nodes: output.stats.expression_nodes(),
            max_depth: output.stats.max_depth(),
            reference_edges: output.stats.reference_edges(),
            nfa_states: output.stats.nfa_states(),
            transitions: output.stats.transitions(),
        },
    }
}

/// The fingerprint of one catalog compiled from one document.
fn fingerprint_of(source: &str, text: &str) -> CatalogFingerprint {
    let output = compile_catalog(PatternSource::named(source, text), CompileLimits::default())
        .unwrap_or_else(|error| panic!("{source} must compile: {error}"));
    catalog_fingerprint(source.to_owned(), &output)
}

fn repository_catalog_fingerprints() -> Vec<CatalogFingerprint> {
    tracked_catalog_sources()
        .into_iter()
        .map(|(source, text)| {
            let output = compile_catalog(
                PatternSource::named(&source, &text),
                CompileLimits::default(),
            )
            .unwrap_or_else(|error| panic!("{source} must compile: {error}"));
            catalog_fingerprint(source, &output)
        })
        .collect()
}

#[test]
fn repository_catalog_fingerprints_are_stable_across_cst_normalization() {
    insta::assert_debug_snapshot!(repository_catalog_fingerprints());
}

/// The fingerprint guards the compiled program, so a change that reorders
/// predicates without changing behavior must still move it.
///
/// Attribute conjunction is commutative: these two catalogs match exactly the
/// same tokens, and they agree on every count the fingerprint carried before
/// the predicate digest — definitions, expression nodes, depth, reference
/// edges, states, transitions. That is why a forced canonicalization which
/// reordered every predicate in every tracked catalog passed all four snapshot
/// tests. Semantically inert is not structurally inert, and this guard exists
/// for the structure.
#[test]
fn two_catalogs_differing_only_in_predicate_order_do_not_share_a_fingerprint() {
    assert_ne!(
        fingerprint_of("t.lx", "a := [text=猫 & unidic.pos1=名詞].\n"),
        fingerprint_of("t.lx", "a := [unidic.pos1=名詞 & text=猫].\n"),
    );
}

/// The control for the test above: the digest is over the compiled program,
/// not the text that produced it.
///
/// A digest that moved with whitespace would pass that test while catching
/// nothing, because every real edit changes the text too.
#[test]
fn formatting_and_comments_do_not_change_a_predicate_digest() {
    assert_eq!(
        fingerprint_of("t.lx", "a := [text=猫 & unidic.pos1=名詞].\n").predicates,
        fingerprint_of(
            "t.lx",
            "# a cat noun\na    :=   [text=猫   &   unidic.pos1=名詞]  .\n"
        )
        .predicates,
    );
}

const FINGERPRINT_DEFS: &str = "a := [text=猫].\nb := [text=猫] [text=犬].\n";

/// The block is load-bearing for output, so it enters catalog identity.
///
/// Without this, a definitions-identical, policy-contradictory pair would
/// fingerprint identically. The programs differ only in the first rank criterion,
/// and they publish opposite records.
#[test]
fn two_catalogs_differing_only_in_policy_do_not_share_a_fingerprint() {
    let with_rank = |first: &str| {
        format!(
            "{FINGERPRINT_DEFS}policy\n               resolve all rank: [{first}, start, end, pattern].\nend\n"
        )
    };
    assert_ne!(
        fingerprint_of("t.lx", &with_rank("longest")).policy,
        fingerprint_of("t.lx", &with_rank("shortest")).policy,
    );
}

/// The rendering is canonical and *semantic*: it says what the program does.
///
/// Whitespace, a comment, and the position of the block in the document all
/// change every `PolicySpan` in it. None of them changes what it publishes, so
/// none may change this.
#[test]
fn formatting_and_comments_do_not_change_a_policy_fingerprint() {
    let terse = format!(
        "{FINGERPRINT_DEFS}policy\n           resolve all rank: [longest, start, end, pattern].\nend\n"
    );
    let padded = format!(
        "\n\n# a leading comment, and a definition-order-preserving reflow\n\n\
         {FINGERPRINT_DEFS}\n\npolicy\n\n           # what this contest is for\n           resolve   all   rank:  [longest, start, end, pattern].\n\nend\n"
    );
    assert_eq!(
        fingerprint_of("t.lx", &terse).policy,
        fingerprint_of("t.lx", &padded).policy,
    );
}

/// One document, versus its definitions plus a policy-only document.
///
/// Spans differ between the two — the accumulated block's are indices into a
/// second source — and the fingerprint must not. This is the assembly a
/// researcher gets from `--patterns a.lx --patterns b.lx`, and it has to mean
/// the same thing as the concatenation.
#[test]
fn whole_document_and_accumulated_assembly_fingerprint_alike() {
    let program = "policy\n  resolve all rank: [longest, start, end, pattern].\nend\n";
    let whole = fingerprint_of("t.lx", &format!("{FINGERPRINT_DEFS}{program}"));

    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named("defs.lx", FINGERPRINT_DEFS))
        .expect("the definitions compile");
    builder
        .add_document(PatternSource::named("policy.lx", program))
        .expect("a policy-only document is a document");
    let assembled = catalog_fingerprint(
        "t.lx".to_owned(),
        &builder.build().expect("the assembly compiles"),
    );

    assert_eq!(whole.policy, assembled.policy);
    assert_eq!(whole, assembled, "and nothing else moved either");
}

fn count_named_nodes(node: tree_sitter::Node<'_>, kind: &str) -> usize {
    let mut count = usize::from(node.kind() == kind);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        count += count_named_nodes(child, kind);
    }
    count
}

/// `pattern_reference` nodes that lie in the definition graph, not in a
/// `policy_block`.
///
/// The grammar spells a policy subject and a policy operand with the same
/// `pattern_reference` node it uses inside an expression, so a plain scan
/// counts them. The compiler does not, and should not: `reference_edges` is a
/// preflight counter over the expression graph (`bind.rs`'s `enter_reference`)
/// whose job is to bound NFA expansion. A policy operand names a definition
/// and expands nothing.
fn count_definition_graph_references(node: tree_sitter::Node<'_>) -> usize {
    if node.kind() == "policy_block" {
        return 0;
    }
    let mut count = usize::from(node.kind() == "pattern_reference");
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        count += count_definition_graph_references(child);
    }
    count
}

fn parse_with_pinned_grammar(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_lingrep::LANGUAGE.into())
        .expect("pinned lingrep patterns grammar must load");
    parser
        .parse(source, None)
        .expect("pinned lingrep patterns grammar returned no tree")
}

/// How many `pattern_definition` nodes the pinned grammar must find in each
/// tracked catalog.
///
/// The *set* is discovered; only the numbers are written here. A new catalog
/// therefore fails this test with "no pinned definition count" until someone
/// states its number, rather than being skipped by not appearing in a list.
///
/// These are grammar-level CST node counts, not the compiler's definition
/// stats, and the two legitimately differ — `patterns/sfe.lx` parses 27 and
/// compiles to 27, but the numbers answer different questions and a future
/// catalog may separate them.
const PINNED_DEFINITION_COUNTS: &[(&str, usize)] = &[
    // An executable fixture for the anchor nesting claim, not a research
    // catalog — but it is a catalog, and this list exists so none goes
    // unverified.
    (
        "lingrep-core/tests/fixtures/catalogs/apparatus-depth2.lx",
        9,
    ),
    // 9 emitting patterns (three alternation families) and 6 predicate
    // fragments; the derived `group` declaration is not a definition and does
    // not parse as one, the same way `counting.lx`'s `set` does not.
    ("patterns/alternation.lx", 15),
    // 13 emitting constructions and 6 predicate fragments; the one `test` is a
    // value, not a definition.
    ("patterns/collexeme.lx", 19),
    // 12 emitting patterns (six 呼応 constructions, two spellings each) and 4
    // predicate fragments.
    ("patterns/concord.lx", 16),
    // 9 unrolled counting cost probes and 3 predicate fragments; the one
    // `set` is a value, not a definition, and does not parse as one.
    ("patterns/counting.lx", 12),
    // Two exact-width-one any-token distributions; their key projections do
    // not add definition CST nodes.
    ("patterns/distributions.lx", 2),
    ("patterns/double-wo.lx", 25),
    // One emitting head-sharing pattern and the shared skeleton fragment;
    // the `test` and the `set` are values, not definitions. Eight emitting
    // patterns until the 2026-08-29 re-validation collapsed the observed
    // label pairs into two per-edge label sets.
    ("patterns/double-wo-dep.lx", 2),
    // Five emitting fixed-width patterns; keys and captures add no definitions.
    // 13 emitting patterns and no fragments: the register layers, plus the
    // `token` denominator. Keys and captures add no definitions.
    ("patterns/comment-register.lx", 13),
    ("patterns/ngrams.lx", 5),
    ("patterns/npv-collocation.lx", 2),
    // Two emitting surface orders over one passive-agent relation.
    ("patterns/passive-agent-dep.lx", 2),
    // Three mutually exclusive evidence arms, each split into a
    // candidate-present and candidate-absent population. Sets and tests are
    // values rather than definition CST nodes.
    ("patterns/passive-oblique-realization.lx", 6),
    // 2 emitting patterns, same_as's first tracked user; no fragments.
    ("patterns/reduplication.lx", 2),
    // Two portable content-word patterns and no provider-specific fragments.
    ("patterns/portable-content-words.lx", 2),
    // 49 emitting sentence-final patterns and 12 shared fragments; the set is
    // a value rather than a definition.
    ("patterns/sentence-final.lx", 61),
    // 86 emitting patterns and 18 fragments, including the `NEG` policy operand.
    ("patterns/sf-experimental.lx", 104),
    ("patterns/sfe.lx", 27),
    ("patterns/sketch-grammar.lx", 22),
    ("patterns/test-simple.lx", 1),
    // Five emitting where-clause patterns; constraints add no definitions, and
    // the one `set` is a value, not a definition, whether a relation bracket
    // or an `in` test is what references it.
    ("patterns/ud-dependency.lx", 5),
    // Two emitting graph-assignment patterns; enhanced node and edge binders
    // are observable bindings, not definition CST nodes.
    ("patterns/ud-enhanced-argument.lx", 2),
    // Four consistency instruments; derived endpoints, graph bindings, and
    // edge bindings are not definition CST nodes.
    ("patterns/ud-enhanced-consistency.lx", 4),
    // Seven fixed enhanced-graph motifs; named graph nodes and edges are
    // observable assignments rather than definitions.
    ("patterns/ud-enhanced-propagation.lx", 7),
    // Four dependency-direction combinations for crossing intervals.
    ("patterns/ud-nonprojectivity.lx", 4),
    // Five one-token head classifiers; anonymous edge targets and their
    // token predicates add no definition CST node.
    ("patterns/ud-object-realization.lx", 5),
    // Three fork linearizations and one order-independent chain definition.
    ("patterns/ud-subtree-profile.lx", 4),
    // Seven positive frames, including two transitive surface orders and the
    // observed relative- and adverbial-clause argument chains.
    ("patterns/ud-valency.lx", 7),
    // Four linear-rule fragments and the two arms of the ヲ格 comparison; the
    // two sets add no definition CST node.
    ("patterns/wo-predicate-dep.lx", 6),
    ("scripts/test/test_metadata.lx", 1),
    ("tests/fixtures/selective-patterns.lx", 10),
    ("lingrep/tests/fixtures/one.lx", 1),
    ("lingrep/tests/fixtures/lexicon-research-mix.lx", 6),
    ("lingrep/tests/fixtures/lexicon-semantic.lx", 2),
    ("lingrep/tests/fixtures/pron-complement.lx", 1),
    ("lingrep/tests/fixtures/pron-exclusion.lx", 1),
    ("lingrep/tests/fixtures/sentence-final.lx", 1),
];

#[test]
fn pinned_tree_sitter_parses_repository_catalogs() {
    let mut quantified_patterns = 0;
    let mut checked = 0;
    for (path, source) in tracked_catalog_sources() {
        let expected_definitions = PINNED_DEFINITION_COUNTS
            .iter()
            .find(|(name, _)| *name == path)
            .map_or_else(
                || {
                    panic!(
                        "{path} has no pinned definition count — add one to \
                     PINNED_DEFINITION_COUNTS so the new catalog is verified \
                     rather than silently skipped"
                    )
                },
                |(_, count)| *count,
            );
        checked += 1;
        let tree = parse_with_pinned_grammar(&source);
        let root = tree.root_node();
        assert!(
            !root.has_error(),
            "pinned lingrep patterns grammar recovered while parsing {path}: {root}"
        );
        assert_eq!(
            count_named_nodes(root, "pattern_definition")
                + count_named_nodes(root, "lexicon_definition"),
            expected_definitions,
            "pinned lingrep patterns grammar parsed an incomplete definition catalog for {path}"
        );
        quantified_patterns += count_named_nodes(root, "quantified_pattern");
    }
    // Catches the stale half: an entry left behind for a catalog that was
    // deleted or renamed. Without this the map only ever grows, and a name
    // that no longer exists reads as coverage.
    assert_eq!(
        checked,
        PINNED_DEFINITION_COUNTS.len(),
        "PINNED_DEFINITION_COUNTS names a catalog the repository no longer has"
    );
    assert!(
        quantified_patterns > 0,
        "repository catalogs must exercise normalized quantifiers"
    );
}

#[test]
fn pinned_grammar_parses_match_mode_operators() {
    // Grammar acceptance only. Lowering is asserted separately; this test
    // pins the operators the pinned revision must be able to parse at all,
    // including the tight forms a `~`-excluding value token would break.
    for source in [
        "p := [text ~ ない].",
        "p := [text !~ ない].",
        "p := [unidic.cform ~ /終止形/].",
        "p := [unidic.cform !~ /終止形/].",
        "p := [text~ない].",
        "p := [text!~ない].",
        "p := [upos=VERB & unidic.cform ~ 終止形].",
    ] {
        let tree = parse_with_pinned_grammar(source);
        assert!(
            !tree.root_node().has_error(),
            "pinned grammar must parse {source} without recovery nodes"
        );
    }
}

#[test]
fn pinned_tree_sitter_exposes_span_equality_cst() {
    let source = "pattern p := (?<unit> [text=a]{1,3}) same_as($unit, by=lemma).";
    let tree = parse_with_pinned_grammar(source);
    let root = tree.root_node();

    assert!(
        !root.has_error(),
        "new lingrep patterns syntax must parse: {root}"
    );
    assert_eq!(count_named_nodes(root, "same_as"), 1);
    assert_eq!(count_named_nodes(root, "capture_reference"), 1);
    assert_eq!(count_named_nodes(root, "capture_projection"), 1);
}

#[test]
fn lookaround_keywords_are_reserved_but_remain_plain_attribute_values() {
    for keyword in [
        "preceded_by",
        "not_preceded_by",
        "followed_by",
        "not_followed_by",
    ] {
        let bare = format!("pattern p := {keyword}.");
        let error = match compile_catalog(
            PatternSource::named("reserved-lookaround.lx", &bare),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("bare lookaround keyword must be reserved: {keyword}"),
            Err(error) => error,
        };
        assert_eq!(
            error.diagnostics[0].code(),
            DiagnosticCode::Syntax,
            "{keyword}"
        );

        let bracketed = format!("pattern p := [text={keyword}].");
        compile_catalog(
            PatternSource::named("lookaround-value.lx", &bracketed),
            CompileLimits::default(),
        )
        .expect("lookaround spelling remains valid as a bracketed value");
    }
}

/// Parses one CoNLL-U sentence and projects it for a bound catalog.
fn conllu_sentence(
    source: &str,
    bound: &lingrep_core::fsm::BoundCatalog,
) -> lingrep_core::token_match::BoundTokenSentence {
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "test",
    );
    let sentence = reader
        .next()
        .expect("fixture holds one sentence")
        .expect("fixture parses");
    assert!(
        reader.next().is_none(),
        "fixture holds exactly one sentence"
    );
    sentence
        .project(bound.projection())
        .expect("fixture projects")
}

fn replace_projected_scalar(
    sentence: &BoundTokenSentence,
    projection: &BoundProjection,
    token_index: usize,
    key: &str,
    replacement: Option<&str>,
) -> BoundTokenSentence {
    let values = (0..sentence.len())
        .flat_map(|token| {
            projection.keys().iter().map(move |attribute| {
                if token == token_index && attribute.as_str() == key {
                    return replacement.map_or_else(
                        ProjectedAttributeValue::missing_scalar,
                        ProjectedAttributeValue::scalar,
                    );
                }
                match sentence.value_key(token, attribute) {
                    Some(AttributeValueRef::Scalar(value)) => {
                        ProjectedAttributeValue::scalar(value)
                    }
                    Some(AttributeValueRef::StringSet(values)) => {
                        ProjectedAttributeValue::string_set(values.iter().cloned())
                    }
                    None => ProjectedAttributeValue::missing_scalar(),
                }
            })
        })
        .collect();
    BoundTokenSentence::from_projected_attribute_values(projection, sentence.len(), values)
        .expect("replacement preserves the bound projection's row shape")
}

#[test]
fn where_clause_filters_candidate_matches_by_dependency_edges() {
    // 猫が魚を食べる: nsubj(食べる, 猫), obj(食べる, 魚) — the SOV witness
    // shape from the 2026-08-27 experiment, in miniature.
    let sov = "1\t猫\t猫\tNOUN\t_\t_\t4\tnsubj\t_\t_\n\
               2\tが\tが\tADP\t_\t_\t1\tcase\t_\t_\n\
               3\t魚\t魚\tNOUN\t_\t_\t4\tobj\t_\t_\n\
               4\t食べる\t食べる\tVERB\t_\t_\t0\troot\t_\t_\n";
    // Same linear shape, but the obj token depends on the first noun rather
    // than the verb: the linear expression still matches, the edge does not.
    let broken = "1\t猫\t猫\tNOUN\t_\t_\t4\tnsubj\t_\t_\n\
                  2\tが\tが\tADP\t_\t_\t1\tcase\t_\t_\n\
                  3\t魚\t魚\tNOUN\t_\t_\t1\tobj\t_\t_\n\
                  4\t食べる\t食べる\tVERB\t_\t_\t0\troot\t_\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "where-clause.lx",
            "pattern sov := (?<s> [dep.rel=nsubj]) []* (?<o> [dep.rel=obj]) []* \
             (?<v> [upos=VERB]) where $v -[nsubj]-> $s, $v -[obj]-> $o.",
        ),
        CompileLimits::default(),
    )
    .expect("a relation-bearing catalog compiles")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the CoNLL-U schema carries dep.head and dep.rel");

    let matches = bound
        .find_matches(&conllu_sentence(sov, &bound))
        .expect("schema matches");
    assert_eq!(matches.len(), 1);
    let matched = &matches[0];
    assert_eq!((matched.start, matched.end), (0, 4));
    assert_eq!(
        matched
            .captures
            .iter()
            .map(|capture| (capture.name.as_str(), capture.start, capture.end))
            .collect::<Vec<_>>(),
        vec![("s", 0, 1), ("o", 2, 3), ("v", 3, 4)],
    );

    assert!(
        bound
            .find_matches(&conllu_sentence(broken, &bound))
            .expect("schema matches")
            .is_empty(),
        "an edge miss must drop the candidate even though the linear \
         expression matches"
    );
}

#[test]
fn enhanced_where_clause_reaches_empty_nodes_from_a_surface_anchor() {
    let source = "# text = I saw\n1\tI\tI\tPRON\t_\t_\t2\tnsubj\t2:nsubj\t_\n2\tsaw\tsee\tVERB\t_\t_\t0\troot\t0:root\t_\n2.1\tpro\tpro\tPRON\t_\t_\t_\t_\t2:obj\t_\n2.2\tthere\tthere\tADV\t_\t_\t_\t_\t2.1:dep\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "enhanced-where.lx",
            "pattern implicit := (?<v> [upos=VERB]) where $v -[enhanced obj as $object]-> (?<pro> [upos=PRON]), $pro -[enhanced dep as $link]-> (?<site> [upos=ADV]).",
        ),
        CompileLimits::default(),
    )
    .expect("an anchored enhanced graph query compiles")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the CoNLL-U schema advertises enhanced graph capability");
    assert!(bound.requires_enhanced_graph());

    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "enhanced-test",
    );
    let sentence = reader.next().unwrap().unwrap();
    let words = sentence.project(bound.projection()).unwrap();
    let input = BoundSentenceInput::new(
        words,
        EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
    );
    let matches = bound.find_matches_input(&input).unwrap();
    assert_eq!(
        matches
            .iter()
            .map(|matched| (matched.pattern.as_str(), matched.start, matched.end))
            .collect::<Vec<_>>(),
        vec![("implicit", 1, 2)]
    );
    assert_eq!(matches[0].graph_bindings.len(), 4);
    assert!(matches[0].graph_bindings.iter().any(|binding| matches!(
        binding,
        lingrep_core::fsm::GraphBinding::Node {
            name,
            node: lingrep_core::corpus::conllu::EnhancedNodeId::Empty {
                boundary: 2,
                subindex: 1
            },
            ..
        } if name == "pro"
    )));
    assert!(matches[0].graph_bindings.iter().any(|binding| matches!(
        binding,
        lingrep_core::fsm::GraphBinding::Edge { name, relation, .. }
            if name == "object" && relation == "obj"
    )));

    let unavailable = BoundSentenceInput::new(
        input.words().clone(),
        EnhancedGraphInput::Observation(
            lingrep_core::corpus::conllu::EnhancedGraphObservation::Unavailable,
        ),
    );
    assert!(bound.find_matches_input(&unavailable).unwrap().is_empty());
}

#[test]
fn enhanced_where_clause_refuses_ambiguous_or_unanchored_bindings() {
    let cases = [
        (
            "pattern p := (?<v> [upos=VERB]) where $v -[enhanced obj]-> (?<v> [upos=NOUN]).",
            DiagnosticCode::EnhancedGraphBindingDuplicate,
        ),
        (
            "pattern p := (?<v> [upos=VERB]) where $v -[enhanced obj]-> $missing.",
            DiagnosticCode::EnhancedGraphBindingUndefined,
        ),
        (
            "pattern p := [upos=VERB] where (?<a> [upos=NOUN]) -[enhanced dep]-> (?<b> [upos=NOUN]).",
            DiagnosticCode::EnhancedGraphComponentUnanchored,
        ),
        (
            "pattern p := (?<v> [upos=VERB]+) where $v -[enhanced obj]-> (?<a> [upos=NOUN]).",
            DiagnosticCode::WhereCaptureNotSingleToken,
        ),
        (
            "pattern p := (?<v> [upos=VERB]) where $later -[enhanced dep]-> (?<first> []), $v -[enhanced dep]-> (?<later> []).",
            DiagnosticCode::EnhancedGraphBindingUndefined,
        ),
        (
            "pattern p := (?<v> [upos=VERB]) where $v -[enhanced obj as $v]-> (?<a> [upos=NOUN]).",
            DiagnosticCode::EnhancedGraphBindingDuplicate,
        ),
    ];
    for (source, expected) in cases {
        let error = match compile_catalog(
            PatternSource::named("enhanced-refusal.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("invalid enhanced binding must be refused"),
            Err(error) => error,
        };
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == expected),
            "expected {expected:?} for {source}: {:?}",
            error.diagnostics
        );
    }
}

#[test]
fn enhanced_where_clause_preserves_named_parallel_edges_and_projects_graph_keys() {
    let source = "1\truns\trun\tVERB\t_\t_\t0\troot\t0:root\t_\n\
                  1.1\tpro\tpro\tPRON\t_\t_\t_\t_\t1:obj|1:obj:agent\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "enhanced-assignments.lx",
            "key: [$edge.rel, $arg.node.id, $arg.lemma]\n\
             pattern named := (?<v> [upos=VERB]) where \
               $v -[enhanced obj|obj:* as $edge]-> (?<arg> [node.kind=empty & upos=PRON]).\n\
             pattern existential := (?<v> [upos=VERB]) where \
               $v -[enhanced obj|obj:*]-> (?<arg> [node.kind=empty & upos=PRON]).\n\
             policy\n\
               resolve @named rank: [longest, start, end, pattern].\n\
             end",
        ),
        CompileLimits::default(),
    )
    .expect("named enhanced edges and graph-node key projections compile")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "enhanced-assignments",
    );
    let sentence = reader.next().unwrap().unwrap();
    let input = BoundSentenceInput::new(
        sentence.project(bound.projection()).unwrap(),
        EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
    );

    let matches = bound.find_matches_input(&input).unwrap();
    let named = matches
        .iter()
        .filter(|matched| matched.pattern == "named")
        .collect::<Vec<_>>();
    assert_eq!(
        named.len(),
        2,
        "a named parallel edge is assignment identity"
    );
    assert_eq!(
        named
            .iter()
            .map(|matched| {
                lingrep_core::fsm::project_key(bound.catalog_policy(), matched, input.words())
                    .unwrap()
                    .into_iter()
                    .map(|field| field.value)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        vec![
            vec!["obj".to_owned(), "1.1".to_owned(), "pro".to_owned()],
            vec!["obj:agent".to_owned(), "1.1".to_owned(), "pro".to_owned()],
        ],
    );
    assert_eq!(
        matches
            .iter()
            .filter(|matched| matched.pattern == "existential")
            .count(),
        1,
        "unnamed parallel edges remain existential",
    );
    let mut stats = lingrep_core::fsm::PolicyStats::default();
    let published = lingrep_core::fsm::apply_policy(
        bound.catalog_policy(),
        &matches,
        input.words(),
        &compiled.anchor_positions(input.words()),
        &mut stats,
    )
    .unwrap();
    assert_eq!(
        published
            .iter()
            .filter(|matched| matched.pattern == "named")
            .count(),
        2,
        "resolve keeps every graph assignment in the winning rank cohort",
    );
}

#[test]
fn enhanced_constraint_order_preserves_complete_graph_binding_order() {
    let source = "1\tsaw\tsee\tVERB\t_\t_\t0\troot\t0:root\t_\n\
                  1.1\tpro\tpro\tPRON\t_\t_\t_\t_\t1:obj\t_\n\
                  1.2\tthere\tthere\tADV\t_\t_\t_\t_\t1:obl\t_\n";
    let patterns = [
        "pattern p := (?<v> [upos=VERB]) where \
           $v -[enhanced obj as $edge_a]-> (?<a> [node.kind=empty]), \
           $v -[enhanced obl as $edge_b]-> (?<b> [node.kind=empty]).",
        "pattern p := (?<v> [upos=VERB]) where \
           $v -[enhanced obl as $edge_b]-> (?<b> [node.kind=empty]), \
           $v -[enhanced obj as $edge_a]-> (?<a> [node.kind=empty]).",
    ];

    let mut records = Vec::new();
    for pattern in patterns {
        let compiled = compile_catalog(
            PatternSource::named("enhanced-order.lx", pattern),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let bound = compiled
            .bind(&lingrep_core::corpus::conllu::token_schema())
            .unwrap();
        let mut reader = lingrep_core::corpus::conllu::Reader::new(
            std::io::BufReader::new(source.as_bytes()),
            "enhanced-order",
        );
        let sentence = reader.next().unwrap().unwrap();
        let input = BoundSentenceInput::new(
            sentence.project(bound.projection()).unwrap(),
            EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
        );
        records.push(bound.find_matches_input(&input).unwrap());
    }
    assert_eq!(records[0], records[1]);
    assert_eq!(
        records[0][0]
            .graph_bindings
            .iter()
            .map(|binding| match binding {
                lingrep_core::fsm::GraphBinding::Node { name, .. }
                | lingrep_core::fsm::GraphBinding::Edge { name, .. } => name.as_str(),
            })
            .collect::<Vec<_>>(),
        vec!["a", "b", "edge_a", "edge_b"],
    );
}

#[test]
fn negative_enhanced_edge_filters_only_the_bound_endpoint_pair() {
    let source = "1\tfirst\tfirst\tVERB\t_\t_\t0\troot\t0:root\t_\n\
                  2\tsecond\tsecond\tVERB\t_\t_\t1\tconj\t1:conj\t_\n\
                  3\tshared\tshared\tNOUN\t_\t_\t1\tnsubj\t1:nsubj|2:nsubj\t_\n\
                  4\tprivate\tprivate\tNOUN\t_\t_\t1\tobj\t1:obj|2:advmod\t_\n\
                  5\tother\tother\tNOUN\t_\t_\t2\tobj\t2:obj\t_\n";
    let patterns = [
        "pattern p := (?<first> [upos=VERB]) (?<second> [upos=VERB]) where \
           $first -[enhanced obj]-> (?<private> [lemma=private]), \
           not $second -[enhanced obj|obl:*]-> $private.",
        "pattern p := (?<first> [upos=VERB]) (?<second> [upos=VERB]) where \
           not $second -[enhanced obj|obl:*]-> $private, \
           $first -[enhanced obj]-> (?<private> [lemma=private]).",
    ];

    let mut records = Vec::new();
    for pattern in patterns {
        let compiled = compile_catalog(
            PatternSource::named("negative-enhanced.lx", pattern),
            CompileLimits::default(),
        )
        .expect("an exact negative enhanced edge compiles in either clause order")
        .matcher;
        let bound = compiled
            .bind(&lingrep_core::corpus::conllu::token_schema())
            .unwrap();
        let mut reader = lingrep_core::corpus::conllu::Reader::new(
            std::io::BufReader::new(source.as_bytes()),
            "negative-enhanced",
        );
        let sentence = reader.next().unwrap().unwrap();
        let input = BoundSentenceInput::new(
            sentence.project(bound.projection()).unwrap(),
            EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
        );
        let matches = bound.find_matches_input(&input).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].graph_bindings.iter().any(|binding| matches!(
            binding,
            lingrep_core::fsm::GraphBinding::Node { name, .. } if name == "private"
        )));
        records.push(matches);
    }
    assert_eq!(records[0], records[1], "clause order is not semantic");

    let blocked = source.replace("1:obj|2:advmod", "1:obj|2:advmod|2:obj");
    let compiled = compile_catalog(
        PatternSource::named("negative-enhanced.lx", patterns[0]),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(blocked.as_bytes()),
        "negative-enhanced-blocked",
    );
    let sentence = reader.next().unwrap().unwrap();
    let words = sentence.project(bound.projection()).unwrap();
    assert!(
        bound
            .find_matches_input(&BoundSentenceInput::new(
                words.clone(),
                EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
            ))
            .unwrap()
            .is_empty(),
        "one matching parallel edge is enough to falsify the anti-edge"
    );
    for observation in [
        lingrep_core::corpus::conllu::EnhancedGraphObservation::Unavailable,
        lingrep_core::corpus::conllu::EnhancedGraphObservation::Incomplete(
            sentence.enhanced_graph().graph().unwrap().clone(),
        ),
    ] {
        assert!(
            bound
                .find_matches_input(&BoundSentenceInput::new(
                    words.clone(),
                    EnhancedGraphInput::Observation(observation),
                ))
                .unwrap()
                .is_empty(),
            "incomplete evidence proves neither presence nor absence"
        );
    }

    let absence = compile_catalog(
        PatternSource::named(
            "negative-only-enhanced.lx",
            "pattern absence := (?<first> [lemma=first]) (?<second> [lemma=second]) \
             where not $second -[enhanced obj]-> $first.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let absence = absence
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "negative-only-enhanced",
    );
    let sentence = reader.next().unwrap().unwrap();
    let words = sentence.project(absence.projection()).unwrap();
    assert_eq!(
        absence
            .find_matches_input(&BoundSentenceInput::new(
                words.clone(),
                EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
            ))
            .unwrap()
            .len(),
        1,
        "a complete graph with no exact edge proves absence"
    );
    for observation in [
        lingrep_core::corpus::conllu::EnhancedGraphObservation::Unavailable,
        lingrep_core::corpus::conllu::EnhancedGraphObservation::Incomplete(
            sentence.enhanced_graph().graph().unwrap().clone(),
        ),
    ] {
        assert!(
            absence
                .find_matches_input(&BoundSentenceInput::new(
                    words.clone(),
                    EnhancedGraphInput::Observation(observation),
                ))
                .unwrap()
                .is_empty(),
            "an incomplete graph cannot prove an otherwise true anti-edge"
        );
    }
}

#[test]
fn basic_head_endpoint_compares_the_derived_word_without_publishing_a_binding() {
    let source = "1\thead\thead\tVERB\t_\t_\t0\troot\t0:root\t_\n\
                  2\tpresent\tpresent\tNOUN\t_\t_\t1\tobj\t1:obj\t_\n\
                  3\tabsent\tabsent\tNOUN\t_\t_\t1\tobl\t2:nsubj\t_\n\
                  4\troot\troot\tVERB\t_\t_\t0\troot\t0:root\t_\n\
                  5\tmissing\tmissing\tNOUN\t_\t_\t_\tdep\t1:dep\t_\n\
                  6\tout-of-range\tout-of-range\tNOUN\t_\t_\t1\tdep\t2:dep\t_\n\
                  7\tparallel\tparallel\tNOUN\t_\t_\t1\tdep\t1:nsubj\t_\n";
    let pattern = "pattern present := (?<d> [lemma=present]) where \
                     basic_head($d) -[enhanced *]-> $d.\n\
                   pattern absent := (?<d> [lemma=absent]) where \
                     not basic_head($d) -[enhanced *]-> $d.\n\
                   pattern irrelevant_parallel := (?<d> [lemma=parallel]) where \
                     not basic_head($d) -[enhanced obj]-> $d.\n\
                   pattern blocked_parallel := (?<d> [lemma=parallel]) where \
                     not basic_head($d) -[enhanced nsubj]-> $d.\n\
                   pattern root := (?<d> [lemma=root]) where \
                     not basic_head($d) -[enhanced *]-> $d.\n\
                   pattern missing := (?<d> [lemma=missing]) where \
                     not basic_head($d) -[enhanced *]-> $d.\n\
                   pattern out_of_range := (?<d> [lemma=out-of-range]) where \
                     not basic_head($d) -[enhanced *]-> $d.";
    let compiled = compile_catalog(
        PatternSource::named("basic-head.lx", pattern),
        CompileLimits::default(),
    )
    .expect("positive and negative derived basic-head endpoints compile")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the required projection carries basic and enhanced edges");
    let required = bound
        .projection()
        .keys()
        .iter()
        .map(AttributeKey::as_str)
        .collect::<Vec<_>>();
    assert!(required.contains(&"dep.head"));
    assert!(required.contains(&"dep.enhanced"));

    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "basic-head",
    );
    let sentence = reader.next().unwrap().unwrap();
    let words = sentence.project(bound.projection()).unwrap();
    let input = BoundSentenceInput::new(
        words.clone(),
        EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
    );
    let matches = bound.find_matches_input(&input).unwrap();
    assert_eq!(
        matches
            .iter()
            .map(|matched| matched.pattern.as_str())
            .collect::<Vec<_>>(),
        vec!["present", "absent", "out_of_range", "irrelevant_parallel"]
    );
    assert!(
        matches
            .iter()
            .all(|matched| matched.graph_bindings.is_empty()),
        "a derived endpoint is not observable assignment identity"
    );

    let malformed = replace_projected_scalar(
        &words,
        bound.projection(),
        1,
        "dep.head",
        Some("not-a-head"),
    );
    assert!(
        bound
            .find_matches_input(&BoundSentenceInput::new(
                malformed,
                EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
            ))
            .unwrap()
            .iter()
            .all(|matched| matched.pattern != "present"),
        "a malformed basic HEAD does not invent a graph endpoint"
    );

    let out_of_range =
        replace_projected_scalar(&words, bound.projection(), 5, "dep.head", Some("99"));
    assert!(
        bound
            .find_matches_input(&BoundSentenceInput::new(
                out_of_range,
                EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
            ))
            .unwrap()
            .iter()
            .all(|matched| matched.pattern != "out_of_range"),
        "an out-of-range basic HEAD does not invent a graph endpoint"
    );

    for observation in [
        lingrep_core::corpus::conllu::EnhancedGraphObservation::Unavailable,
        lingrep_core::corpus::conllu::EnhancedGraphObservation::Incomplete(
            sentence.enhanced_graph().graph().unwrap().clone(),
        ),
    ] {
        assert!(
            bound
                .find_matches_input(&BoundSentenceInput::new(
                    words.clone(),
                    EnhancedGraphInput::Observation(observation),
                ))
                .unwrap()
                .is_empty(),
            "incomplete DEPS proves neither derived-edge presence nor absence"
        );
    }
}

#[test]
fn basic_head_endpoint_anchors_graph_bindings_on_either_relation_side() {
    let source = "1\thead\thead\tVERB\t_\t_\t0\troot\t0:root|3:dep\t_\n\
                  2\tdependent\tdependent\tNOUN\t_\t_\t1\tobj\t1:obj\t_\n\
                  3\tsource\tsource\tNOUN\t_\t_\t1\tobl\t1:obl\t_\n";
    let pattern = "pattern head_side := (?<d> [lemma=dependent]) where \
                     basic_head($d) -[enhanced obj]-> (?<g> [lemma=dependent]).\n\
                   pattern dependent_side := (?<d> [lemma=dependent]) where \
                     (?<g> [lemma=source]) -[enhanced dep]-> basic_head($d).";
    let compiled = compile_catalog(
        PatternSource::named("basic-head-anchor.lx", pattern),
        CompileLimits::default(),
    )
    .expect("a derived endpoint anchors a graph binding from either relation side")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "basic-head-anchor",
    );
    let sentence = reader.next().unwrap().unwrap();
    let input = BoundSentenceInput::new(
        sentence.project(bound.projection()).unwrap(),
        EnhancedGraphInput::Observation(sentence.enhanced_graph().clone()),
    );
    let matches = bound.find_matches_input(&input).unwrap();

    assert_eq!(
        matches
            .iter()
            .map(|matched| matched.pattern.as_str())
            .collect::<Vec<_>>(),
        ["dependent_side", "head_side"]
    );
    assert!(matches.iter().all(|matched| {
        matched
            .graph_bindings
            .iter()
            .any(|binding| matches!(binding, lingrep_core::fsm::GraphBinding::Node { name, .. } if name == "g"))
    }));
}

#[test]
fn basic_head_endpoint_resolves_one_exact_surface_capture() {
    let cases = [
        (
            "pattern p := (?<d> []) where not basic_head($missing) -[enhanced *]-> $d.",
            DiagnosticCode::WhereCaptureUndefined,
        ),
        (
            "pattern p := (?<d> []+) where not basic_head($d) -[enhanced *]-> $d.",
            DiagnosticCode::WhereCaptureNotSingleToken,
        ),
        (
            "fragment f := (?<d> []). pattern p := @f @f where \
             not basic_head($d) -[enhanced *]-> $d.",
            DiagnosticCode::WhereCaptureUndefined,
        ),
        (
            "pattern p := ((?<d> []) | (?<other> [])) where \
             not basic_head($d) -[enhanced *]-> $other.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
    ];
    for (source, expected) in cases {
        let error = match compile_catalog(
            PatternSource::named("basic-head-refusal.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("an invalid derived endpoint must be refused"),
            Err(error) => error,
        };
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == expected),
            "expected {expected:?} for {source}: {:?}",
            error.diagnostics
        );
    }
}

#[test]
fn basic_head_endpoint_constraint_order_has_one_drift_identity() {
    let report = report_catalog_drift(&[
        PatternSource::named(
            "first.lx",
            "pattern p := (?<d> [lemma=x]) where \
             not basic_head($d) -[enhanced obj]-> $d, \
             not basic_head($d) -[enhanced obl]-> $d.",
        ),
        PatternSource::named(
            "second.lx",
            "pattern p := (?<d> [lemma=x]) where \
             not basic_head($d) -[enhanced obl]-> $d, \
             not basic_head($d) -[enhanced obj]-> $d.",
        ),
    ])
    .expect("both catalogs compile");
    assert!(
        report.entries.is_empty(),
        "a `where` clause is a conjunction"
    );
}

#[test]
fn basic_existence_contradiction_requires_positive_label_containment() {
    compile_catalog(
        PatternSource::named(
            "satisfiable-existence.lx",
            "pattern p := (?<a> [text=x]) where \
             $a -[obj|obl]-> _, not $a -[obj]-> _.",
        ),
        CompileLimits::default(),
    )
    .expect("an `obl` witness satisfies the positive side outside the `obj` exclusion");
    compile_catalog(
        PatternSource::named(
            "compatible-negatives.lx",
            "pattern p := (?<a> [text=x]) where \
             not $a -[obj]-> [upos=NOUN], not $a -[obj]-> _.",
        ),
        CompileLimits::default(),
    )
    .expect("two negative constraints are compatible when no object exists");

    let error = match compile_catalog(
        PatternSource::named(
            "contradictory-existence.lx",
            "pattern p := (?<a> [text=x]) where \
             $a -[obl:tmod]-> _, not $a -[obl:*]-> _.",
        ),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("the wildcard exclusion covers the exact positive subtype"),
        Err(error) => error,
    };
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code() == DiagnosticCode::WhereContradictoryExistence })
    );
}

#[test]
fn enhanced_observation_state_filters_only_graph_definitions_in_a_mixed_catalog() {
    let source = "1\truns\trun\tVERB\t_\t_\t0\troot\t0:root\t_\n\
                  1.1\tpro\tpro\tPRON\t_\t_\t_\t_\t1:obj\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "mixed-enhanced.lx",
            "pattern ordinary := [upos=VERB].\n\
             pattern graph := (?<v> [upos=VERB]) where \
               $v -[enhanced obj]-> (?<arg> [upos=PRON]).",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let morphology_only = lingrep_core::token_match::TokenSchema::new([
        lingrep_core::token_match::SchemaAttribute::new(
            "upos",
            lingrep_core::token_match::AttributeKind::Scalar,
        )
        .unwrap(),
    ])
    .unwrap();
    assert!(matches!(
        compiled.bind(&morphology_only),
        Err(lingrep_core::fsm::SchemaBindError::MissingKeys(keys))
            if keys.iter().any(|key| key == "dep.enhanced")
    ));
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "mixed-enhanced",
    );
    let sentence = reader.next().unwrap().unwrap();
    let words = sentence.project(bound.projection()).unwrap();

    let incomplete = lingrep_core::corpus::conllu::EnhancedGraphObservation::Incomplete(
        sentence.enhanced_graph().graph().unwrap().clone(),
    );
    let input = BoundSentenceInput::new(
        words.clone(),
        EnhancedGraphInput::Observation(incomplete.clone()),
    );
    assert_eq!(
        bound
            .find_matches_input(&input)
            .unwrap()
            .into_iter()
            .map(|matched| matched.pattern)
            .collect::<Vec<_>>(),
        vec![compact_str::CompactString::from("ordinary")],
    );
    assert_eq!(
        compiled
            .find_matches_with_enhanced_graph(&words, &incomplete)
            .unwrap()
            .into_iter()
            .map(|matched| matched.pattern)
            .collect::<Vec<_>>(),
        vec![compact_str::CompactString::from("ordinary")],
    );
    assert!(matches!(
        bound.find_matches_input(&BoundSentenceInput::new(
            words,
            EnhancedGraphInput::NotRequested,
        )),
        Err(lingrep_core::fsm::BoundExecutionError::EnhancedGraphNotRequested)
    ));
}

#[test]
fn anonymous_dependency_edges_classify_heads_once() {
    // The first verb has two direct objects.  An anonymous existential edge
    // must still publish one record for the head, not one per witness.  The
    // second verb has no direct object and witnesses the complemented query.
    let source = "1\tA\tA\tNOUN\t_\t_\t3\tobj\t_\t_\n\
                  2\tB\tB\tNOUN\t_\t_\t3\tobj:agent\t_\t_\n\
                  3\tdoes\tdo\tVERB\t_\t_\t0\troot\t_\t_\n\
                  4\twaits\twait\tVERB\t_\t_\t3\tconj\t_\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "anonymous-edge.lx",
            "set OBJECT := {obj, obj:*}.\n\
             pattern present := (?<v> [upos=VERB]) \
               where $v -[@OBJECT]-> _.\n\
             pattern absent := (?<v> [upos=VERB]) \
               where not $v -[@OBJECT]-> _.",
        ),
        CompileLimits::default(),
    )
    .expect("anonymous positive and negative edges compile")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the CoNLL-U schema carries dependency attributes");

    assert_eq!(
        bound
            .find_matches(&conllu_sentence(source, &bound))
            .expect("complete basic dependencies classify both heads"),
        vec![
            with_capture(full_match("present", 2, 3), "v", 2, 3),
            with_capture(full_match("absent", 3, 4), "v", 3, 4),
        ],
    );
}

#[test]
fn negative_captured_edges_filter_only_the_bound_pair_and_labels() {
    for (head, relation, labels, accepted) in [
        ("1", "obj", "obj", false),
        ("3", "obj", "obj", true),
        ("1", "obl", "obj", true),
        ("1", "obl:tmod", "obj|obl:*", false),
        ("1", "obl", "obj|obl:*", true),
        ("1", "obl", "*", false),
        ("3", "obl", "*", true),
    ] {
        let pattern = format!(
            "pattern p := ^ (?<h> []) (?<d> []) [] $ \
             where not $h -[{labels}]-> $d."
        );
        let matcher = compile_catalog(
            PatternSource::named("negative-captured.lx", &pattern),
            CompileLimits::default(),
        )
        .expect("a negative edge between captured endpoints compiles")
        .matcher;
        let bound = matcher
            .bind(&lingrep_core::corpus::conllu::token_schema())
            .unwrap();
        let source = format!(
            "1\th\th\tVERB\t_\t_\t0\troot\t_\t_\n\
             2\td\td\tNOUN\t_\t_\t{head}\t{relation}\t_\t_\n\
             3\tother\tother\tNOUN\t_\t_\t1\tobj\t_\t_\n"
        );
        let expected = if accepted {
            vec![with_captures(
                full_match("p", 0, 3),
                &[("h", 0, 1), ("d", 1, 2)],
            )]
        } else {
            vec![]
        };
        assert_eq!(
            bound
                .find_matches(&conllu_sentence(&source, &bound))
                .unwrap(),
            expected,
            "head={head}, relation={relation}, labels={labels}"
        );
    }
}

#[test]
fn negative_captured_edges_require_complete_basic_observations() {
    let matcher = compile_catalog(
        PatternSource::named(
            "negative-captured-observation.lx",
            "pattern p := ^ (?<h> []) (?<d> []) [] $ where not $h -[*]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let sentence = conllu_sentence(
        "1\th\th\tVERB\t_\t_\t0\troot\t_\t_\n\
         2\td\td\tNOUN\t_\t_\t3\tobj\t_\t_\n\
         3\tother\tother\tNOUN\t_\t_\t1\tobj\t_\t_\n",
        &bound,
    );
    assert_eq!(
        bound.find_matches(&sentence).unwrap(),
        vec![with_captures(
            full_match("p", 0, 3),
            &[("h", 0, 1), ("d", 1, 2)]
        )],
    );
    for index in 0..3 {
        for (key, value) in [
            ("dep.head", None),
            ("dep.head", Some("_")),
            ("dep.head", Some("bad")),
            ("dep.head", Some("4")),
            ("dep.rel", None),
            ("dep.rel", Some("_")),
            ("dep.rel", Some("")),
        ] {
            let incomplete =
                replace_projected_scalar(&sentence, bound.projection(), index, key, value);
            assert!(
                bound.find_matches(&incomplete).unwrap().is_empty(),
                "index={index}, key={key}, value={value:?}"
            );
        }
    }
}

#[test]
fn negative_captured_edges_filter_before_longest_selection_in_either_clause_order() {
    for clause in [
        "not $h -[obj]-> $d, $h -[other]-> _",
        "$h -[other]-> _, not $h -[obj]-> $d",
    ] {
        let source =
            format!("pattern p := ^ (?<h> [text=h]) []* (?<d> [upos=NOUN]) where {clause}.");
        let matcher = compile_catalog(
            PatternSource::named("negative-before-longest.lx", &source),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let bound = matcher
            .bind(&lingrep_core::corpus::conllu::token_schema())
            .unwrap();
        let sentence = conllu_sentence(
            "1\th\th\tVERB\t_\t_\t0\troot\t_\t_\n\
             2\tfirst\tfirst\tNOUN\t_\t_\t3\tobj\t_\t_\n\
             3\tmiddle\tmiddle\tADV\t_\t_\t1\tother\t_\t_\n\
             4\tsecond\tsecond\tNOUN\t_\t_\t1\tobj\t_\t_\n",
            &bound,
        );
        assert_eq!(
            bound.find_matches(&sentence).unwrap(),
            vec![with_captures(
                full_match("p", 0, 2),
                &[("h", 0, 1), ("d", 1, 2)]
            )],
        );
    }
}

#[hegel::test(test_cases = 300, derandomize = true)]
fn negative_captured_edges_match_an_independent_edge_model(tc: hegel::TestCase) {
    use hegel::generators as g;
    let raw_heads = tc.draw(g::vecs(g::integers::<u8>()).max_size(12));
    let count = raw_heads.len();
    let query = tc.draw(g::sampled_from(vec![
        "obj",
        "obl:*",
        "obj|obl:*",
        "@ROLE",
        "*",
    ]));
    let padded = tc.draw(g::booleans());
    let observations = raw_heads
        .iter()
        .map(|raw| {
            let head = usize::from(*raw) % (count + 1);
            let label = if tc.draw(g::booleans()) {
                tc.draw(g::sampled_from(vec![
                    "obj",
                    "obj:agent",
                    "obl",
                    "obl:tmod",
                    "xobj",
                    "root",
                    "_",
                ]))
                .to_owned()
            } else {
                tc.draw(g::text().max_size(24))
            };
            (
                Some(if padded {
                    format!("{head:03}")
                } else {
                    head.to_string()
                }),
                Some(label),
            )
        })
        .collect::<Vec<_>>();
    let pattern = format!(
        "set ROLE := {{obj, obl:tmod}}. pattern p := ^ (?<h> []) (?<d> []) []* $ \
         where not $h -[{query}]-> $d."
    );
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named("generated-negative.lx", &pattern))
        .unwrap();
    let matcher = builder.build().unwrap().matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let words = projected_dependency_observations(&bound, &observations);
    let usable = observations.iter().all(|(_, label)| {
        label
            .as_deref()
            .is_some_and(|label| !label.is_empty() && label != "_")
    });
    let forbidden = count >= 2 && usize::from(raw_heads[1]) % (count + 1) == 1 && {
        let label = observations[1].1.as_deref().unwrap();
        match query {
            "obj" => label == "obj",
            "obl:*" => label.starts_with("obl:") && label.len() > 4,
            "obj|obl:*" => label == "obj" || (label.starts_with("obl:") && label.len() > 4),
            "@ROLE" => label == "obj" || label == "obl:tmod",
            "*" => true,
            _ => unreachable!(),
        }
    };
    let expected = if count >= 2 && usable && !forbidden {
        vec![with_captures(
            full_match("p", 0, count),
            &[("h", 0, 1), ("d", 1, 2)],
        )]
    } else {
        vec![]
    };
    assert_eq!(matcher.find_matches(&words), expected);
    assert_eq!(bound.find_matches(&words).unwrap(), expected);
    let input = BoundSentenceInput::new(words, EnhancedGraphInput::NotRequested);
    assert_eq!(bound.find_matches_input(&input).unwrap(), expected);
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn unusable_dependency_values_anywhere_prevent_captured_absence(tc: hegel::TestCase) {
    use hegel::generators as g;
    let count = tc
        .draw(g::vecs(g::booleans()).min_size(2).max_size(12))
        .len();
    let index = tc.draw(g::integers::<usize>().max_value(count - 1));
    let (key, replacement) = tc.draw(g::sampled_from(vec![
        ("dep.head", None),
        ("dep.head", Some("")),
        ("dep.head", Some("_")),
        ("dep.head", Some("-1")),
        ("dep.head", Some("1.0")),
        ("dep.head", Some(" 1")),
        ("dep.head", Some("340282366920938463463374607431768211455")),
        ("dep.rel", None),
        ("dep.rel", Some("")),
        ("dep.rel", Some("_")),
    ]));
    let matcher = compile_catalog(
        PatternSource::named(
            "generated-unknown.lx",
            "pattern p := ^ (?<h> []) (?<d> []) where not $h -[*]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let observations = vec![(Some("0".to_owned()), Some("root".to_owned())); count];
    let words = projected_dependency_observations(&bound, &observations);
    let words = if tc.draw(g::booleans()) {
        replace_projected_scalar(&words, bound.projection(), index, key, replacement)
    } else {
        replace_projected_scalar(
            &words,
            bound.projection(),
            index,
            "dep.head",
            Some(&(count + 1).to_string()),
        )
    };
    assert_eq!(bound.find_matches(&words).unwrap(), Vec::<Match>::new());
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn captured_absence_uses_the_final_repeated_assignment(tc: hegel::TestCase) {
    use hegel::generators as g;
    let linked = tc.draw(g::vecs(g::booleans()).min_size(1).max_size(7));
    let matcher = compile_catalog(
        PatternSource::named(
            "generated-final-assignment.lx",
            "pattern p := ^ (?<h> []) (?<d> [])+ $ where not $h -[obj]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let mut observations = vec![(Some("0".to_owned()), Some("root".to_owned()))];
    observations.extend(linked.iter().map(|linked| {
        (
            Some(if *linked { "1" } else { "0" }.to_owned()),
            Some("obj".to_owned()),
        )
    }));
    let words = projected_dependency_observations(&bound, &observations);
    let count = observations.len();
    let expected = if *linked.last().unwrap() {
        vec![]
    } else {
        vec![with_captures(
            full_match("p", 0, count),
            &[("h", 0, 1), ("d", count - 1, count)],
        )]
    };
    assert_eq!(bound.find_matches(&words).unwrap(), expected);
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn captured_absence_retains_a_passing_assignment_at_the_same_span(tc: hegel::TestCase) {
    use hegel::generators as g;
    let linked = tc.draw(g::vecs(g::booleans()).min_size(1).max_size(7));
    let matcher = compile_catalog(
        PatternSource::named(
            "generated-competing-assignments.lx",
            "pattern p := ^ (?<h> []) ((?<d> []) | [])+ $ where not $h -[obj]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let mut observations = vec![(Some("0".to_owned()), Some("root".to_owned()))];
    observations.extend(linked.iter().map(|linked| {
        (
            Some(if *linked { "1" } else { "0" }.to_owned()),
            Some("obj".to_owned()),
        )
    }));
    let words = projected_dependency_observations(&bound, &observations);
    let expected = linked
        .iter()
        .rposition(|linked| !linked)
        .map_or_else(Vec::new, |last| {
            vec![with_captures(
                full_match("p", 0, observations.len()),
                &[("h", 0, 1), ("d", last + 1, last + 2)],
            )]
        });
    assert_eq!(bound.find_matches(&words).unwrap(), expected);
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn captured_absence_requires_both_optional_operands_to_bind(tc: hegel::TestCase) {
    use hegel::generators as g;
    let linked = tc.draw(g::vecs(g::booleans()).max_size(5));
    let matcher = compile_catalog(
        PatternSource::named(
            "generated-optional-operands.lx",
            "pattern p := ^ (?<h> [])? (?<d> [])? [] $ where not $h -[obj]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let observations = linked
        .iter()
        .map(|linked| {
            (
                Some(if *linked { "1" } else { "0" }.to_owned()),
                Some("obj".to_owned()),
            )
        })
        .collect::<Vec<_>>();
    let words = projected_dependency_observations(&bound, &observations);
    let expected = if linked.len() == 3 && !linked[1] {
        vec![with_captures(
            full_match("p", 0, 3),
            &[("h", 0, 1), ("d", 1, 2)],
        )]
    } else {
        vec![]
    };
    assert_eq!(bound.find_matches(&words).unwrap(), expected);
}

#[hegel::test(test_cases = 60, derandomize = true)]
fn bound_sentence_batches_preserve_captured_absence_and_sentence_order(tc: hegel::TestCase) {
    use hegel::generators as g;
    let linked = tc.draw(g::vecs(g::booleans()).max_size(16));
    let threads = tc.draw(g::integers::<usize>().min_value(1).max_value(4));
    let matcher = compile_catalog(
        PatternSource::named(
            "negative-batch.lx",
            "pattern p := ^ (?<h> []) (?<d> []) $ where not $h -[obj]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let inputs = linked
        .iter()
        .map(|linked| {
            let values = [
                (Some("0".to_owned()), Some("root".to_owned())),
                (
                    Some(if *linked { "1" } else { "0" }.to_owned()),
                    Some("obj".to_owned()),
                ),
            ];
            BoundSentenceInput::new(
                projected_dependency_observations(&bound, &values),
                EnhancedGraphInput::NotRequested,
            )
        })
        .collect::<Vec<_>>();
    let expected = linked
        .iter()
        .map(|linked| {
            if *linked {
                vec![]
            } else {
                vec![with_captures(
                    full_match("p", 0, 2),
                    &[("h", 0, 1), ("d", 1, 2)],
                )]
            }
        })
        .collect::<Vec<_>>();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    assert_eq!(
        lingrep_core::fsm::BoundSentenceMatcher::new(&bound)
            .match_all(&pool, &inputs)
            .unwrap(),
        expected
    );
}

#[test]
fn captured_negative_labels_do_not_exclude_other_positive_dependents() {
    let pattern = "pattern p := ^ (?<h> []) (?<d> []) (?<other> []) $ where \
        $h -[obj]-> $d, not $h -[obl]-> $d, not $h -[obj]-> $other.";
    let matcher = compile_catalog(
        PatternSource::named("negative-pair-scope.lx", pattern),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let words = conllu_sentence(
        "1\th\th\tVERB\t_\t_\t0\troot\t_\t_\n\
        2\td\td\tNOUN\t_\t_\t1\tobj\t_\t_\n\
        3\tother\tother\tVERB\t_\t_\t0\troot\t_\t_\n",
        &bound,
    );
    assert_eq!(
        bound.find_matches(&words).unwrap(),
        vec![with_captures(
            full_match("p", 0, 3),
            &[("h", 0, 1), ("d", 1, 2), ("other", 2, 3)]
        )]
    );
}

#[test]
fn captured_negative_polarity_participates_in_drift_but_clause_order_does_not() {
    let first = "pattern p := (?<h> []) (?<d> []) where not $h -[obj]-> $d, not $h -[obl]-> $d.";
    let reordered =
        "pattern p := (?<h> []) (?<d> []) where not $h -[obl]-> $d, not $h -[obj]-> $d.";
    let same = report_catalog_drift(&[
        PatternSource::named("first.lx", first),
        PatternSource::named("second.lx", reordered),
    ])
    .unwrap();
    assert!(same.entries.is_empty());
    let different = report_catalog_drift(&[
        PatternSource::named(
            "positive.lx",
            "pattern p := (?<h> []) (?<d> []) where $h -[obj]-> $d.",
        ),
        PatternSource::named(
            "negative.lx",
            "pattern p := (?<h> []) (?<d> []) where not $h -[obj]-> $d.",
        ),
    ])
    .unwrap();
    assert_eq!(different.entries.len(), 1);
}

#[test]
fn captured_absence_validation_scales_linearly_with_sentence_length() {
    struct CountedWords<'a> {
        words: &'a BoundTokenSentence,
        reads: std::cell::Cell<usize>,
    }
    impl TokenSequence for CountedWords<'_> {
        fn len(&self) -> usize {
            self.words.len()
        }
        fn get(
            &self,
            token: usize,
            attribute: TokenAttributeIndex,
        ) -> Option<&compact_str::CompactString> {
            TokenSequence::get(self.words, token, attribute)
        }
        fn value_key(&self, token: usize, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
            self.reads.set(self.reads.get() + 1);
            self.words.value_key(token, key)
        }
    }
    let matcher = compile_catalog(
        PatternSource::named(
            "negative-scale.lx",
            "pattern p := (?<h> []) (?<d> []) where not $h -[*]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    let measure = |count: usize| {
        let values = vec![(Some("0".to_owned()), Some("root".to_owned())); count];
        let words = projected_dependency_observations(&bound, &values);
        let counted = CountedWords {
            words: &words,
            reads: std::cell::Cell::new(0),
        };
        let expected = (0..count - 1)
            .map(|start| {
                with_captures(
                    full_match("p", start, start + 2),
                    &[("h", start, start + 1), ("d", start + 1, start + 2)],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(matcher.find_matches(&counted), expected);
        counted.reads.get()
    };
    let small = measure(128);
    let large = measure(256);
    assert!(
        small > 0 && large <= small * 3,
        "doubling the sentence must not quadruple dependency reads: {small} -> {large}"
    );
    measure(100_000);
}

fn projected_dependency_observations(
    bound: &lingrep_core::fsm::BoundCatalog,
    observations: &[(Option<String>, Option<String>)],
) -> BoundTokenSentence {
    let values = observations
        .iter()
        .flat_map(|(head, label)| {
            bound.projection().keys().iter().map(move |key| {
                match key.as_str() {
                    "dep.head" => head.as_deref(),
                    "dep.rel" => label.as_deref(),
                    key => panic!("unexpected projection key {key}"),
                }
                .map(compact_str::CompactString::from)
            })
        })
        .collect();
    BoundTokenSentence::from_projected_values(bound.projection(), observations.len(), values)
        .unwrap()
}

#[test]
fn anonymous_dependent_predicates_classify_heads_without_assignments() {
    // One verbal head has one pronominal and two nominal direct-object
    // witnesses. Each predicate target must publish the one-token head once,
    // not one assignment per witness or a span joining head and dependent.
    // The missing-UPOS dependent has an ordinary false token test and does
    // not make the complete basic dependency observation unusable.
    let source = "1\tit\tit\tPRON\t_\t_\t5\tobj\t_\t_\n\
                  2\ta\ta\tNOUN\t_\t_\t5\tobj\t_\t_\n\
                  3\tB\tB\tPROPN\t_\t_\t5\tobj:agent\t_\t_\n\
                  4\tunknown\tunknown\t_\t_\t_\t5\tobj\t_\t_\n\
                  5\tdoes\tdo\tVERB\t_\t_\t0\troot\t_\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "anonymous-dependent-predicate.lx",
            "set NOMINAL := {NOUN, PROPN}.\n\
             test PRONOMINAL := upos=PRON.\n\
             pattern pronominal := (?<v> [lemma=do]) \
               where $v -[obj|obj:*]-> [@PRONOMINAL].\n\
             pattern nominal := (?<v> [lemma=do]) \
               where $v -[obj|obj:*]-> [upos in @NOMINAL].",
        ),
        CompileLimits::default(),
    )
    .expect("anonymous dependent predicates compile")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("target predicate attributes join schema projection");

    assert_eq!(
        bound
            .find_matches(&conllu_sentence(source, &bound))
            .expect("complete dependencies and predicate values match"),
        vec![
            with_capture(full_match("nominal", 4, 5), "v", 4, 5),
            with_capture(full_match("pronominal", 4, 5), "v", 4, 5),
        ],
    );
}

#[test]
fn negative_dependent_predicates_require_complete_compatible_observations() {
    // `does` has no pronominal object and an unrelated dependent with missing
    // UPOS; only edge-compatible dependents participate in completeness.
    // `makes` has an object whose UPOS is missing, so neither polarity may
    // classify it. `sees` has a positive witness. `takes` supplies UPOS but
    // lacks the second attribute referenced through a compound named test.
    let source = "1\ta\ta\tNOUN\t_\t_\t4\tobj\t_\t_\n\
                  2\tunknown\tunknown\t_\t_\t_\t4\tnsubj\t_\t_\n\
                  3\tquickly\tquickly\tADV\t_\t_\t4\tadvmod\t_\t_\n\
                  4\tdoes\tdo\tVERB\t_\t_\t0\troot\t_\t_\n\
                  5\tunknown\tunknown\t_\t_\t_\t6\tobj\t_\t_\n\
                  6\tmakes\tmake\tVERB\t_\t_\t4\tconj\t_\t_\n\
                  7\tit\tit\tPRON\t_\t_\t8\tobj\t_\t_\n\
                  8\tsees\tsee\tVERB\t_\t_\t4\tconj\t_\t_\n\
                  9\tthis\t_\tDET\t_\t_\t10\tobj\t_\t_\n\
                  10\ttakes\ttake\tVERB\t_\t_\t4\tconj\t_\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "negative-dependent-predicate.lx",
            "test PRONOMINAL := upos=PRON | (upos=DET & lemma=this).\n\
             pattern present := (?<v> [upos=VERB]) \
               where $v -[obj|obj:*]-> [@PRONOMINAL].\n\
             pattern absent := (?<v> [upos=VERB]) \
               where not $v -[obj|obj:*]-> [@PRONOMINAL].",
        ),
        CompileLimits::default(),
    )
    .expect("positive and negative dependent predicates compile")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("target predicate attributes join schema projection");

    assert_eq!(
        bound
            .find_matches(&conllu_sentence(source, &bound))
            .expect("complete compatible observations classify heads"),
        vec![
            with_capture(full_match("absent", 3, 4), "v", 3, 4),
            with_capture(full_match("present", 7, 8), "v", 7, 8),
        ],
    );
}

#[test]
fn anonymous_dependency_absence_requires_a_usable_basic_tree() {
    let compiled = compile_catalog(
        PatternSource::named(
            "anonymous-edge-admission.lx",
            "pattern present := (?<v> [upos=VERB]) where $v -[obj]-> _.\n\
             pattern absent := (?<v> [upos=VERB]) where not $v -[obj]-> _.",
        ),
        CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("schema binds");

    let missing_relation =
        "1\tX\tX\tNOUN\t_\t_\t2\t_\t_\t_\n2\tdoes\tdo\tVERB\t_\t_\t0\troot\t_\t_\n";
    assert!(
        bound
            .find_matches(&conllu_sentence(missing_relation, &bound))
            .expect("matching itself succeeds")
            .is_empty(),
        "neither polarity may turn a missing relation into a result"
    );

    // The CoNLL-U reader rejects malformed trees at admission. Exercise the
    // matcher boundary independently as well, because another schema-bearing
    // caller can construct projected sentences directly.
    for head in ["bad", "3"] {
        let observations = [("NOUN", head, "obj"), ("VERB", "0", "root")];
        let values: Vec<Option<compact_str::CompactString>> = observations
            .iter()
            .flat_map(|(upos, dep_head, dep_rel)| {
                bound.projection().keys().iter().map(move |key| {
                    Some(
                        match key.as_str() {
                            "upos" => *upos,
                            "dep.head" => *dep_head,
                            "dep.rel" => *dep_rel,
                            other => panic!("unexpected projection key {other}"),
                        }
                        .into(),
                    )
                })
            })
            .collect();
        let sentence = lingrep_core::token_match::BoundTokenSentence::from_projected_values(
            bound.projection(),
            observations.len(),
            values,
        )
        .expect("the deliberately malformed values still project");
        assert!(
            bound
                .find_matches(&sentence)
                .expect("matching succeeds")
                .is_empty(),
            "neither polarity may classify malformed or out-of-range head {head}"
        );
    }
}

#[test]
fn anonymous_edge_spelling_is_contextual() {
    compile_catalog(
        PatternSource::named(
            "anonymous-edge-context.lx",
            "not := [text=not].\n_ := [text=_].\n\
             pattern p := @not | @_ | [text=not] | [text=_].",
        ),
        CompileLimits::default(),
    )
    .expect("not and underscore remain ordinary names outside an anonymous edge");
}

#[test]
fn where_clause_accepts_more_than_eight_assignments_per_sentence_token() {
    let mut sentence = String::new();
    for index in 1..=20 {
        let (upos, head, relation) = if index == 20 {
            ("VERB", 0, "root")
        } else {
            ("NOUN", 20, "dep")
        };
        writeln!(
            sentence,
            "{index}\tt{index}\tt{index}\t{upos}\t_\t_\t{head}\t{relation}\t_\t_"
        )
        .unwrap();
    }

    let bound = compile_catalog(
        PatternSource::named(
            "many-assignments.lx",
            "pattern triples := (?<a> []) []* (?<b> []) []* (?<c> []) \
             where $c -[dep]-> $a, $c -[dep]-> $b.",
        ),
        CompileLimits::default(),
    )
    .expect("the relation-bearing catalog compiles")
    .matcher
    .bind(&lingrep_core::corpus::conllu::token_schema())
    .expect("the CoNLL-U schema carries dependency attributes");

    let matches = bound
        .find_matches(&conllu_sentence(&sentence, &bound))
        .expect("a legal candidate set is not subject to an arbitrary linear bound");
    assert_eq!(matches.len(), 18);
}

#[test]
fn where_clause_filters_before_longest_candidate_selection() {
    // From start 0 the linear expression accepts two candidates: ending at
    // the first verb (whose nsubj edge holds) and at the second verb (whose
    // edge does not). Selecting the longest before filtering would discard
    // the passing candidate invisibly; the shorter passing candidate must
    // survive the longer failing one.
    let source = "1\t猫\t猫\tNOUN\t_\t_\t2\tnsubj\t_\t_\n\
                  2\t走る\t走る\tVERB\t_\t_\t0\troot\t_\t_\n\
                  3\tと\tと\tSCONJ\t_\t_\t2\tmark\t_\t_\n\
                  4\t鳴く\t鳴く\tVERB\t_\t_\t2\tconj\t_\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "where-shadow.lx",
            "pattern p := (?<s> [dep.rel=nsubj]) []* (?<v> [upos=VERB]) \
             where $v -[nsubj]-> $s.",
        ),
        CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("schema binds");
    let matches = bound
        .find_matches(&conllu_sentence(source, &bound))
        .expect("schema matches");
    assert_eq!(
        matches
            .iter()
            .map(|matched| (matched.start, matched.end))
            .collect::<Vec<_>>(),
        vec![(0, 2)],
        "the shorter passing candidate must survive the longer failing one"
    );
}

#[test]
fn double_wo_dep_candidate_matches_shared_heads_and_rejects_frames() {
    // The dependency successor to double-wo's candidate pattern
    // (patterns/double-wo-dep.lx). Both witnesses follow recorded parses: the
    // genuine
    // double-を reading of 氏名を住所を備考に入力した — both nouns obj of
    // 入力, the analysis GiNZA got wrong and a correct parse surfaces as
    // obj+obj — and the をXに frame 生徒を対象にアンケート調査を実施した,
    // whose first noun attaches to 対象 rather than the main predicate: the
    // structural fact that replaces the linear catalog's adverbial-anchor
    // word list.
    let double_wo = "1\t氏名\t氏名\tNOUN\t_\t_\t7\tobj\t_\t_\n\
                     2\tを\tを\tADP\t_\t_\t1\tcase\t_\t_\n\
                     3\t住所\t住所\tNOUN\t_\t_\t7\tobj\t_\t_\n\
                     4\tを\tを\tADP\t_\t_\t3\tcase\t_\t_\n\
                     5\t備考\t備考\tNOUN\t_\t_\t7\tobl\t_\t_\n\
                     6\tに\tに\tADP\t_\t_\t5\tcase\t_\t_\n\
                     7\t入力\t入力\tVERB\t_\t_\t0\troot\t_\t_\n\
                     8\tし\tする\tAUX\t_\t_\t7\taux\t_\t_\n\
                     9\tた\tた\tAUX\t_\t_\t7\taux\t_\t_\n";
    let frame = "1\t生徒\t生徒\tNOUN\t_\t_\t3\tobj\t_\t_\n\
                 2\tを\tを\tADP\t_\t_\t1\tcase\t_\t_\n\
                 3\t対象\t対象\tNOUN\t_\t_\t8\tobl\t_\t_\n\
                 4\tに\tに\tADP\t_\t_\t3\tcase\t_\t_\n\
                 5\tアンケート\tアンケート\tNOUN\t_\t_\t6\tcompound\t_\t_\n\
                 6\t調査\t調査\tNOUN\t_\t_\t8\tobj\t_\t_\n\
                 7\tを\tを\tADP\t_\t_\t6\tcase\t_\t_\n\
                 8\t実施\t実施\tVERB\t_\t_\t0\troot\t_\t_\n\
                 9\tし\tする\tAUX\t_\t_\t8\taux\t_\t_\n\
                 10\tた\tた\tAUX\t_\t_\t8\taux\t_\t_\n";
    let (_, source) = lingrep_core::testing::catalogs::tracked_catalog_sources()
        .into_iter()
        .find(|(name, _)| name == "patterns/double-wo-dep.lx")
        .expect("the successor catalog is tracked");
    let compiled = compile_catalog(
        PatternSource::named("patterns/double-wo-dep.lx", &source),
        CompileLimits::default(),
    )
    .expect("the successor catalog compiles")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the CoNLL-U schema carries dep.head and dep.rel");

    let matches = bound
        .find_matches(&conllu_sentence(double_wo, &bound))
        .expect("schema matches");
    assert_eq!(
        matches.len(),
        1,
        "exactly one label pair can hold per triple, so the label set that \
         replaced the eight-pattern enumeration publishes one record too"
    );
    let matched = &matches[0];
    assert_eq!(matched.pattern.as_str(), "double_wo_dep");
    assert_eq!((matched.start, matched.end), (0, 7));
    assert_eq!(
        matched
            .captures
            .iter()
            .map(|capture| (capture.name.as_str(), capture.start, capture.end))
            .collect::<Vec<_>>(),
        vec![
            ("n1", 0, 1),
            ("w1", 1, 2),
            ("n2", 2, 3),
            ("w2", 3, 4),
            ("v", 6, 7),
        ],
        "the head capture is pinned by the edges, not by the linear shape"
    );

    assert!(
        bound
            .find_matches(&conllu_sentence(frame, &bound))
            .expect("schema matches")
            .is_empty(),
        "a をXに frame's first noun attaches to the frame nominal, so no \
         pattern's shared-head clause can hold"
    );
}

/// A clause must reach an assignment its own skeleton admits.
///
/// Six tokens, reduced from sentence 329 of the re-validation's parse. The
/// `[!@WO]{0,5}` gap before `$w2` may consume 理想 or nothing, so the skeleton
/// admits `$n2` = 社会的 or 理想 from one start. Only 社会的 satisfies
/// `$n2 -[case]-> $w2` — を's `dep.head` is 3 — and 理想 is the one the
/// evaluator reaches first, from the shorter gap.
///
/// Both assignments reach position 5, so `deduplicate` must key on the whole
/// slot vector: a position-only key keeps the first and the clause never sees
/// the other.
#[test]
fn a_where_clause_reaches_a_satisfying_assignment_a_failing_one_shadowed() {
    let sentence = "1\tX\tX\tNOUN\t_\t_\t6\tobj\t_\t_\n\
                    2\tを\tを\tADP\t_\t_\t1\tcase\t_\t_\n\
                    3\t社会的\t社会的\tNOUN\t_\t_\t6\tobj\t_\t_\n\
                    4\t理想\t理想\tNOUN\t_\t_\t3\tcompound\t_\t_\n\
                    5\tを\tを\tADP\t_\t_\t3\tcase\t_\t_\n\
                    6\tV\tV\tVERB\t_\t_\t0\troot\t_\t_\n";
    let source = "test WO := text=を.\n\
                  pattern amb :=\n\
                    (?<n1> []) [!@WO]{0,5} (?<w1> [text=を & upos=ADP])\n\
                    [!@WO]{0,8}\n\
                    (?<n2> []) [!@WO]{0,5} (?<w2> [text=を & upos=ADP])\n\
                    []*\n\
                    (?<v> [])\n\
                    where $n1 -[case]-> $w1, $n2 -[case]-> $w2,\n\
                          $v -[obj]-> $n1, $v -[obj]-> $n2.\n";
    let bound = compile_catalog(
        PatternSource::named("assignment.lx", source),
        CompileLimits::default(),
    )
    .expect("the catalog compiles")
    .matcher
    .bind(&lingrep_core::corpus::conllu::token_schema())
    .expect("the CoNLL-U schema carries dep.head and dep.rel");

    let matches = bound
        .find_matches(&conllu_sentence(sentence, &bound))
        .expect("schema matches");
    assert_eq!(
        matches.len(),
        1,
        "the satisfying assignment must survive the failing one that reaches \
         the same position first"
    );
    let bindings: Vec<(&str, usize)> = matches[0]
        .captures
        .iter()
        .map(|capture| (capture.name.as_str(), capture.start))
        .collect();
    assert_eq!(
        bindings,
        vec![("n1", 0), ("w1", 1), ("n2", 2), ("w2", 4), ("v", 5)],
        "n2 must bind 社会的 at index 2, not 理想 at index 3"
    );
}

#[test]
fn where_clause_static_refusals_each_carry_their_own_code() {
    for (source, expected) in [
        (
            "fragment f := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[acl]-> $b.",
            DiagnosticCode::WhereOnFragment,
        ),
        (
            "pattern p := (?<a> [text=x]) [text=y] where $a -[acl]-> $b.",
            DiagnosticCode::WhereCaptureUndefined,
        ),
        (
            "pattern p := (?<a> [text=x]+) (?<b> [text=y]) \
             where $a -[acl]-> $b.",
            DiagnosticCode::WhereCaptureNotSingleToken,
        ),
        (
            "pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[acl]-> $a.",
            DiagnosticCode::WhereSelfEdge,
        ),
        (
            "pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[acl|acl]-> $b.",
            DiagnosticCode::DuplicateRelationLabel,
        ),
        (
            "set S := {acl}.\n\
             pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[@S|acl]-> $b.",
            DiagnosticCode::DuplicateRelationLabel,
        ),
        (
            "set S := {動詞}.\n\
             pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[@S]-> $b.",
            DiagnosticCode::InvalidRelationLabel,
        ),
        (
            "test T := text=x.\n\
             pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[@T]-> $b.",
            DiagnosticCode::UndefinedReference,
        ),
        (
            "pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[@NOPE]-> $b.",
            DiagnosticCode::UndefinedReference,
        ),
        (
            "pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[obj]-> $b, $a -[obl]-> $b.",
            DiagnosticCode::WhereContradictoryLabels,
        ),
        (
            "pattern p := [text=x] where $missing -[obj]-> _.",
            DiagnosticCode::WhereCaptureUndefined,
        ),
        (
            "pattern p := (?<a> [text=x]+) where $a -[obj]-> _.",
            DiagnosticCode::WhereCaptureNotSingleToken,
        ),
        (
            "pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where not $a -[obj]-> $missing.",
            DiagnosticCode::WhereCaptureUndefined,
        ),
        (
            "pattern p := (?<a> []) (?<b> []) where not $missing -[obj]-> $b.",
            DiagnosticCode::WhereCaptureUndefined,
        ),
        (
            "pattern p := (?<a> []) where not $a -[obj]-> $a.",
            DiagnosticCode::WhereSelfEdge,
        ),
        (
            "pattern p := (?<a> []) (?<b> []+) where not $a -[obj]-> $b.",
            DiagnosticCode::WhereCaptureNotSingleToken,
        ),
        (
            "pattern p := ((?<a> []) | (?<b> [])) where not $a -[obj]-> $b.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
        (
            "fragment f := (?<d> []). pattern p := (?<h> []) @f @f where not $h -[obj]-> $d.",
            DiagnosticCode::WhereCaptureUndefined,
        ),
        (
            "pattern p := (?<a> []) (?<b> []) where $a -[obj|obl]-> $b, not $a -[*]-> $b.",
            DiagnosticCode::WhereContradictoryExistence,
        ),
        (
            "pattern p := (?<a> []) (?<b> []) where not $a -[*]-> $b, $a -[obj|obl]-> $b.",
            DiagnosticCode::WhereContradictoryExistence,
        ),
        (
            "pattern p := (?<a> [text=x]) (?<b> [text=y]) \
             where $a -[enhanced obj]-> $b, not $a -[enhanced obj|obl]-> $b.",
            DiagnosticCode::WhereContradictoryExistence,
        ),
        (
            "pattern p := (?<a> [text=x]) (?<b> [text=y]) (?<c> [text=z]) \
             where $a -[obj]-> $c, $b -[obl]-> $c.",
            DiagnosticCode::WhereContradictoryLabels,
        ),
        // Both operands of one constraint, in exclusive branches.
        (
            "pattern p := ((?<a> [text=x]) | (?<b> [text=y])) \
             where $a -[obj]-> $b.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
        (
            "pattern p := ((?<a> [text=x]) | (?<b> [text=y])) \
             where not $a -[enhanced obj]-> $b.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
        (
            "pattern p := ((?<a> [text=x]) | (?<b> [text=y])) (?<c> [text=z]) \
             where $c -[obj]-> $a, not $c -[enhanced obl]-> $b.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
        // Across two constraints, which is the same defect: the clause is a
        // conjunction, so every operand it names must bind in one match.
        (
            "pattern p := ((?<a> [text=x]) | (?<b> [text=y])) (?<c> [text=z]) \
             where $c -[obj]-> $a, $c -[obl]-> $b.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
        // Nested one alternation deeper, so the guard is not a root-level fact.
        (
            "pattern p := [text=w] ((?<a> [text=x]) | ([text=q] (?<b> [text=y]))) \
             (?<c> [text=z]) where $c -[obj]-> $a, $c -[obl]-> $b.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
        // `{0,1}` enters its body once, so a branch chosen inside one is still
        // chosen once and the guard still holds. This is the boundary of the
        // repetition exception below, not an instance of it.
        (
            "pattern p := ((?<a> [text=x]) | (?<b> [text=y]))? (?<c> [text=z]) \
             where $c -[obj]-> $a, $c -[obl]-> $b.",
            DiagnosticCode::WhereExclusiveCaptures,
        ),
    ] {
        let error = match compile_catalog(
            PatternSource::named("where-refusal.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("must be refused: {source}"),
            Err(error) => error,
        };
        assert_eq!(error.diagnostics[0].code(), expected, "{source}");
    }
}

/// What branch exclusivity must **not** refuse.
///
/// The refusal is sound because a single-entry alternation takes one branch,
/// and deliberately incomplete everywhere else. Each shape here has a live
/// precedent, so a rule that over-refused would delete a legal query rather
/// than merely annoy someone.
#[test]
fn branch_exclusivity_admits_every_shape_that_can_bind_both() {
    for (source, why) in [
        (
            // An optional operand: the clause filters when the branch is not
            // taken, which is the existing three-valued behaviour.
            "pattern p := ((?<a> [text=x]) | [text=q]) (?<c> [text=z]) \
             where $c -[obj]-> $a.",
            "one operand in a branch and one outside it share no alternation",
        ),
        (
            // Nested captures bind one token twice, and the label-sets round
            // kept this legal on purpose.
            "pattern p := (?<a> (?<b> [text=x])) (?<c> [text=z]) \
             where $c -[obj]-> $a, $c -[obj]-> $b.",
            "an ancestor capture and its child are never exclusive",
        ),
        (
            // The same branches, in the same alternation, under a repetition
            // that can run twice.
            "pattern p := ((?<a> [text=x]) | (?<b> [text=y]))+ (?<c> [text=z]) \
             where $c -[obj]-> $a, $a -[case]-> $b.",
            "two iterations legally bind two branches",
        ),
        (
            "pattern p := ((?<a> [text=x]) | (?<b> [text=y])){2,4} (?<c> [text=z]) \
             where $c -[obj]-> $a, $a -[case]-> $b.",
            "a bounded repetition above one iteration is re-entrant too",
        ),
        (
            // Two captures in the *same* branch of an alternation.
            "pattern p := (((?<a> [text=x]) (?<b> [text=y])) | [text=q]) \
             where $a -[obj]-> $b.",
            "same branch, so the alternation constrains nothing about the pair",
        ),
    ] {
        compile_catalog(
            PatternSource::named("branch-exclusivity.lx", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "must compile ({why}): {source}\n{:?}",
                error
                    .diagnostics
                    .iter()
                    .map(lingrep_core::fsm::Diagnostic::code)
                    .collect::<Vec<_>>()
            )
        });
    }
}

/// The re-entrant case is legal *and matches*, which compilation alone cannot
/// show.
///
/// A rule keyed on branch membership alone would refuse this at compile time;
/// one that admitted it but broke the runtime would pass the test above. Both
/// captures bind here, in different iterations of one repetition, and the
/// clause holds over them — the counterexample the design round was built on.
#[test]
fn a_repetition_over_an_alternation_binds_both_branches_in_one_match() {
    let bound = compile_catalog(
        PatternSource::named(
            "reentrant.lx",
            "pattern repeated := \
             ((?<r1> [upos=NOUN]) | (?<r2> [upos=ADP]))+ (?<r3> [upos=VERB]) \
             where $r3 -[nsubj]-> $r1, $r1 -[case]-> $r2.",
        ),
        CompileLimits::default(),
    )
    .expect("the re-entrant catalog compiles")
    .matcher
    .bind(&lingrep_core::corpus::conllu::token_schema())
    .expect("the CoNLL-U schema carries dep.head and dep.rel");

    // 魚(obj) を(case->魚) 猫(nsubj->食べる) が(case->猫) 食べる。 — the noun
    // branch binds 猫 and the adposition branch binds が, in two iterations of
    // one repetition.
    let sentence = "1\t魚\t魚\tNOUN\t_\t_\t5\tobj\t_\t_\n\
                    2\tを\tを\tADP\t_\t_\t1\tcase\t_\t_\n\
                    3\t猫\t猫\tNOUN\t_\t_\t5\tnsubj\t_\t_\n\
                    4\tが\tが\tADP\t_\t_\t3\tcase\t_\t_\n\
                    5\t食べる\t食べる\tVERB\t_\t_\t0\troot\t_\t_\n";
    let matches = bound
        .find_matches(&conllu_sentence(sentence, &bound))
        .expect("schema matches");
    assert!(
        !matches.is_empty(),
        "a repetition must be able to bind both branches"
    );
    let bindings: Vec<(&str, usize)> = matches[0]
        .captures
        .iter()
        .map(|capture| (capture.name.as_str(), capture.start))
        .collect();
    assert_eq!(
        bindings,
        vec![("r1", 2), ("r2", 3), ("r3", 4)],
        "both branch captures are bound in one match"
    );
}

#[test]
fn where_clause_requires_dependency_attributes_at_binding() {
    let compiled = compile_catalog(
        PatternSource::named(
            "where-admission.lx",
            "pattern p := (?<s> [text=猫]) (?<v> [text=走る]) \
             where $v -[nsubj]-> $s.",
        ),
        CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher;
    let error = compiled
        .bind(&lingrep_core::token_match::TokenSchema::unidic_morphology())
        .expect_err("a dependency-free schema must be refused before matching");
    let missing = error.missing_keys();
    assert!(
        missing.iter().any(|key| key == "dep.head") && missing.iter().any(|key| key == "dep.rel"),
        "the refusal names both edge attributes: {missing:?}"
    );
}

/// The contradictory-labels refusal is sound but deliberately incomplete: it
/// keys on disjoint label *sets*, not on a repeated dependent, because nested
/// captures legally bind one token and two constraints over one dependent can
/// then be two spellings of one satisfiable edge.
#[test]
fn a_shared_dependent_with_intersecting_labels_stays_legal() {
    for source in [
        "pattern p := (?<a> (?<b> [text=x])) (?<c> [text=y]) \
         where $a -[obj]-> $c, $b -[obj]-> $c.",
        "pattern p := (?<a> [text=x]) (?<b> [text=y]) (?<c> [text=z]) \
         where $a -[obj|obl]-> $c, $b -[obl|acl]-> $c.",
    ] {
        compile_catalog(
            PatternSource::named("shared-dependent.lx", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("must compile: {source}: {error:?}"));
    }
}

/// Two witnesses that differ only in whether their edges carry UD subtypes:
/// `subtyped` uses `nsubj:pass`/`obl:tmod` where `plain` uses `nsubj`/`obl`.
///
/// The pair is what makes the wildcard testable at all — `obl:*` and `obl`
/// select one witness each, and neither selects both.
const SUBTYPED_WITNESS: &str = "1\tit\tit\tPRON\t_\t_\t3\tnsubj:pass\t_\t_\n\
                                2\twas\tbe\tAUX\t_\t_\t3\taux:pass\t_\t_\n\
                                3\tdone\tdo\tVERB\t_\t_\t0\troot\t_\t_\n\
                                4\tyesterday\tyesterday\tNOUN\t_\t_\t3\tobl:tmod\t_\t_\n";
const PLAIN_WITNESS: &str = "1\tthey\tthey\tPRON\t_\t_\t2\tnsubj\t_\t_\n\
                             2\twalked\twalk\tVERB\t_\t_\t0\troot\t_\t_\n\
                             3\thome\thome\tNOUN\t_\t_\t2\tobl\t_\t_\n";

/// The witnesses one `where` catalog selects, by name.
fn selected_witnesses(source: &str) -> Vec<&'static str> {
    let compiled = compile_catalog(
        PatternSource::named("label-set.lx", source),
        CompileLimits::default(),
    )
    .expect("catalog compiles")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the CoNLL-U schema carries the edge attributes");
    [("subtyped", SUBTYPED_WITNESS), ("plain", PLAIN_WITNESS)]
        .into_iter()
        .filter(|(_, text)| {
            !bound
                .find_matches(&conllu_sentence(text, &bound))
                .expect("schema matches")
                .is_empty()
        })
        .map(|(name, _)| name)
        .collect()
}

/// A label set is a disjunction over the one `dep.rel` a token carries, so
/// `-[a|b]->` selects exactly the union of the two single-label catalogs and
/// never a candidate neither would have selected.
#[test]
fn a_label_set_selects_the_union_of_its_members() {
    let subject = |labels: &str| {
        selected_witnesses(&format!(
            "pattern p := (?<d> [upos=PRON]) []* (?<h> [upos=VERB]) \
             where $h -[{labels}]-> $d."
        ))
    };

    assert_eq!(subject("nsubj"), vec!["plain"]);
    assert_eq!(subject("nsubj:*"), vec!["subtyped"]);
    assert_eq!(
        subject("nsubj|nsubj:*"),
        vec!["subtyped", "plain"],
        "the set is exactly the union of its members"
    );
}

#[test]
fn an_any_relation_constraint_selects_every_present_label() {
    assert_eq!(
        selected_witnesses(
            "pattern p := (?<d> [upos=PRON]) []* (?<h> [upos=VERB]) \
             where $h -[*]-> $d.",
        ),
        vec!["subtyped", "plain"]
    );

    let missing = "1\tthey\tthey\tPRON\t_\t_\t2\t_\t_\t_\n\
                   2\twalked\twalk\tVERB\t_\t_\t0\troot\t_\t_\n";
    let compiled = compile_catalog(
        PatternSource::named(
            "any-relation.lx",
            "pattern p := (?<d> [upos=PRON]) (?<h> [upos=VERB]) \
             where $h -[*]-> $d.",
        ),
        CompileLimits::default(),
    )
    .expect("the any-relation catalog compiles")
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the CoNLL-U schema carries the edge attributes");
    assert!(
        bound
            .find_matches(&conllu_sentence(missing, &bound))
            .expect("schema matches")
            .is_empty(),
        "`*` means any present relation, not a missing value"
    );
}

#[test]
fn any_relation_intersects_every_listed_relation_on_one_dependent() {
    compile_catalog(
        PatternSource::named(
            "any-relation-intersection.lx",
            "pattern p := (?<h> [upos=VERB]) (?<d> [upos=NOUN]) \
             where $h -[*]-> $d, $h -[obj]-> $d.",
        ),
        CompileLimits::default(),
    )
    .expect("an any-relation constraint cannot contradict a listed label");
}

#[test]
fn any_relation_cannot_be_mixed_with_redundant_label_terms() {
    let error = match compile_catalog(
        PatternSource::named(
            "mixed-any-relation.lx",
            "pattern p := (?<h> []) (?<d> []) where $h -[*|obj]-> $d.",
        ),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("`*` already includes every present label, so a union is redundant"),
        Err(error) => error,
    };
    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::Syntax);
}

/// A `@set` term means what the inline spelling means. The two are one
/// catalog after bind, which is also why they do not drift against each other.
#[test]
fn a_named_set_term_matches_what_the_inline_spelling_matches() {
    let inline = selected_witnesses(
        "pattern p := (?<d> [upos=PRON]) []* (?<h> [upos=VERB]) \
         where $h -[nsubj|nsubj:*]-> $d.",
    );
    let named = selected_witnesses(
        "set SUBJ := {nsubj, nsubj:*}.\n\
         pattern p := (?<d> [upos=PRON]) []* (?<h> [upos=VERB]) \
         where $h -[@SUBJ]-> $d.",
    );
    assert_eq!(inline, named);
    assert_eq!(inline, vec!["subtyped", "plain"]);
}

/// `obl:*` is a family matcher over the subtype and deliberately excludes the
/// unsubtyped label: `obl` and `obl:tmod` are two labels, and a wildcard that
/// quietly included the bare form would make `obl|obl:*` mean `obl:*`.
#[test]
fn a_subtype_wildcard_excludes_the_unsubtyped_label() {
    let object = |labels: &str| {
        selected_witnesses(&format!(
            "pattern p := (?<h> [upos=VERB]) []* (?<d> [upos=NOUN]) \
             where $h -[{labels}]-> $d."
        ))
    };

    assert_eq!(object("obl"), vec!["plain"]);
    assert_eq!(object("obl:*"), vec!["subtyped"]);
    assert_eq!(
        object("obl|obl:*"),
        vec!["subtyped", "plain"],
        "the union spelling is the one that takes both"
    );
}

#[test]
fn where_remains_a_legal_name_and_bracketed_value() {
    compile_catalog(
        PatternSource::named("where-name.lx", "where := [].\npattern p := [text=where]."),
        CompileLimits::default(),
    )
    .expect("`where` remains a legal bare definition name and bracketed value");
}

#[test]
fn lookaround_bodies_reject_observable_capture_and_span_equality_state() {
    for (source, expected) in [
        (
            "pattern p := followed_by((?<unit> [text=a])) [text=x].",
            DiagnosticCode::CaptureInsideLookaround,
        ),
        (
            concat!(
                "fragment captured := (?<unit> [text=a]).\n",
                "pattern p := followed_by(@captured) [text=x].",
            ),
            DiagnosticCode::CaptureInsideLookaround,
        ),
        (
            "pattern p := (?<unit> [text=a]) followed_by(same_as($unit)) [text=x].",
            DiagnosticCode::SpanEqualityInsideLookaround,
        ),
    ] {
        let error = match compile_catalog(
            PatternSource::named("invalid-lookaround.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("stateful lookaround body must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.diagnostics[0].code(), expected, "{source}");
    }
}

#[test]
fn assertion_only_attributes_are_part_of_the_cache_projection_contract() {
    let matcher = compile_catalog(
        PatternSource::named(
            "lookaround-attributes.lx",
            "pattern p := followed_by([lemma=target]) [text=x].",
        ),
        CompileLimits::default(),
    )
    .expect("lookaround attribute fixture must compile")
    .matcher;
    let required = matcher.required_attributes();
    assert!(required.contains(&TokenAttributeIndex::Orth.into()));
    assert!(required.contains(&TokenAttributeIndex::Lemma.into()));
}

#[test]
fn strict_lookaround_catalog_rejects_the_global_spawn_condition_cliff() {
    let source = |count: usize| {
        (0..count)
            .map(|index| format!("pattern p{index} := not_preceded_by([text=z{index}]) [text=x]."))
            .collect::<Vec<_>>()
            .join("\n")
    };

    compile_catalog(
        PatternSource::named("64-lookarounds.lx", &source(64)),
        CompileLimits::default(),
    )
    .expect("64 spawn conditions are supported");

    let error = match compile_catalog(
        PatternSource::named("65-lookarounds.lx", &source(65)),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("65 spawn conditions must not silently select the slow fallback"),
        Err(error) => error,
    };
    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
    assert!(error.diagnostics[0].message.contains("spawn conditions"));
    assert!(error.diagnostics[0].message.contains("requested 65"));
}

#[test]
fn span_equality_is_compiled_by_its_dedicated_execution_plan() {
    let source = "pattern p := (?<unit> a) same_as($unit).";
    let matcher = compile_catalog(
        PatternSource::named("span-equality.lx", source),
        CompileLimits::default(),
    )
    .expect("bounded span equality must compile")
    .matcher;
    assert_eq!(matcher.patterns().len(), 1);
}

#[test]
fn capture_binding_enforces_source_order_and_definite_assignment() {
    for (source, code) in [
        (
            "pattern p := (?<x> a) (?<x> b).",
            DiagnosticCode::DuplicateCapture,
        ),
        (
            "pattern p := same_as($x).",
            DiagnosticCode::UndefinedCapture,
        ),
        (
            "pattern p := same_as($x) (?<x> a).",
            DiagnosticCode::ForwardCaptureReference,
        ),
        (
            "pattern p := ((?<x> a))? same_as($x).",
            DiagnosticCode::CaptureNotDefinitelyAssigned,
        ),
        (
            "pattern p := ((?<x> a) | b) same_as($x).",
            DiagnosticCode::CaptureNotDefinitelyAssigned,
        ),
        (
            "fragment f := (?<x> a).\npattern p := @f same_as($x).",
            DiagnosticCode::CaptureOutOfScope,
        ),
        (
            concat!(
                "pattern p := (?<a> x) same_as($a) ",
                "(?<b> y) same_as($b).",
            ),
            DiagnosticCode::MultipleBackreferencedCaptures,
        ),
    ] {
        let error = match compile_catalog(
            PatternSource::named("capture-binding.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("capture binding fixture must fail: {source}"),
            Err(error) => error,
        };
        assert_eq!(error.diagnostics[0].code(), code, "source: {source}");
    }

    compile_catalog(
        PatternSource::named(
            "capture-binding.lx",
            "pattern p := ((?<x> a)){1,2} same_as($x).",
        ),
        CompileLimits::default(),
    )
    .expect("minimum-one repetition definitely assigns its capture");
}

#[test]
fn repository_catalogs_use_no_regex_operand_with_equals() {
    // Deliberately the same expression as `scripts/check-lx-catalogs.sh`, and
    // it must stay that way: two guards over the same property that disagree
    // about what they forbid are worse than one, because whichever is looser
    // makes the stricter one look like a false alarm.
    //
    // `\s*` matters — a `contains("=/")` check would wave through the spaced
    // spelling, which the pinned grammar accepts and the compiler rejects.
    //
    // The `\[[^\]]*` anchor matters for the opposite reason: without it the
    // expression also flags ordinary prose such as a URL-encoded
    // `dict_path=/tmp/x` in a comment. Only an operand inside a `[...]` node
    // pattern is the thing the compiler rejects.
    //
    // This overlaps the shell guard rather than replacing it, and the gap is
    // measured: a catalog written into `patterns/` but not yet `git add`ed and
    // carrying the rejected spelling fails here and passes the shell guard
    // green. The spelling in question, quoted so both guards can see it:
    //   [unidic.cform = /終止形/]   (legacy-lx-syntax-ok — quoting it is the point)
    // That one reads `git ls-files` and so also covers `.rs` sources, but
    // skips an untracked catalog; this one discovers from the filesystem and
    // so covers a catalog
    // that has not been added yet. Neither subsumes the other, and this half
    // runs under a bare `cargo test`, without git and without ci.sh.
    let pattern = regex::Regex::new(r"\[[^\]]*[a-z0-9_]+\s*!?=\s*/[^/]*/").expect("valid regex");
    for (path, source) in tracked_catalog_sources() {
        for (index, line) in source.lines().enumerate() {
            // Same exemption marker as scripts/check-lx-catalogs.sh. It covers
            // teaching comments that show the rejected spelling on purpose.
            if line.contains("legacy-lx-syntax-ok") {
                continue;
            }
            assert!(
                !pattern.is_match(line),
                "{path}:{} still uses a regex operand on `=`/`!=`: {line}",
                index + 1,
            );
        }
    }
}

#[test]
fn tilde_is_substring_search_and_not_tilde_is_its_complement() {
    let matcher = compile_catalog(
        PatternSource::named(
            "match-mode.lx",
            concat!(
                "contains := [text ~ ない].\n",
                "not_contains := [text !~ ない].\n",
                "search := [text ~ /ない$/].\n",
                "not_search := [text !~ /ない$/].\n",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("match-mode catalog compiles")
    .matcher;

    let values = &["じゃない", "ないよ", "ます"];
    // `~ ない` is containment, so it matches medially as well as finally.
    assert_pattern_spans(&matcher, "contains", values, &[(0, 1), (1, 2)]);
    assert_pattern_spans(&matcher, "not_contains", values, &[(2, 3)]);
    // `~ /ない$/` keeps `is_match` semantics: an anchor still means what it says.
    assert_pattern_spans(&matcher, "search", values, &[(0, 1)]);
    assert_pattern_spans(&matcher, "not_search", values, &[(1, 2), (2, 3)]);
}

#[test]
fn match_mode_operators_leave_a_missing_attribute_unsatisfied_in_both_polarities() {
    let matcher = compile_catalog(
        PatternSource::named(
            "missing-attribute.lx",
            concat!(
                "contains := [unidic.cform ~ 終止形].\n",
                "not_contains := [unidic.cform !~ 終止形].\n",
                "not_search := [unidic.cform !~ /終止形/].\n",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("match-mode catalog compiles")
    .matcher;

    // A token lacking the attribute satisfies NEITHER polarity. `!~` must not
    // become a way to select tokens that simply have no such field.
    let tokens = [token_with_cform("走る", None)];
    assert_eq!(
        matcher.find_matches(&tokens),
        Vec::new(),
        "a token without cform must satisfy neither `~` nor `!~`"
    );
}

#[test]
fn negated_regex_search_is_charged_the_same_regex_bytes_as_the_positive_form() {
    // Guards the regex-byte accounting sites. They are `if let` matches, so
    // omitting the negated variant is a silent under-count rather than a
    // compile error, and a deterministic compile limit stops being truthful.
    let positive = compile_catalog(
        PatternSource::named("positive.lx", "p := [unidic.cform ~ /終止形/]."),
        CompileLimits::default(),
    )
    .expect("positive search compiles");
    let negative = compile_catalog(
        PatternSource::named("negative.lx", "p := [unidic.cform !~ /終止形/]."),
        CompileLimits::default(),
    )
    .expect("negated search compiles");

    assert!(
        positive.stats.regex_bytes() > 0,
        "a regex operand must charge regex bytes at all"
    );
    assert_eq!(
        positive.stats.regex_bytes(),
        negative.stats.regex_bytes(),
        "`!~` must charge the same regex bytes as `~`"
    );
}

fn first_diagnostic_message(source: &str) -> String {
    let error = match compile_catalog(
        PatternSource::named("rejected.lx", source),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("source must be rejected: {source}"),
        Err(error) => error,
    };
    assert_eq!(
        error.diagnostics[0].code(),
        DiagnosticCode::Syntax,
        "{source}"
    );
    error.diagnostics[0].message.clone()
}

#[test]
fn equals_rejects_a_regex_operand_and_names_both_fixes() {
    // Every spacing the grammar accepts must be rejected, not just the tight
    // one the catalogs happened to use.
    for source in [
        "p := [unidic.cform = /終止形/].",   // legacy-lx-syntax-ok
        "p := [unidic.cform=/終止形/].",     // legacy-lx-syntax-ok
        "p := [unidic.cform  =  /終止形/].", // legacy-lx-syntax-ok
    ] {
        let message = first_diagnostic_message(source);
        assert!(message.contains('~'), "must name the `~` fix: {message}");
        assert!(
            message.contains(r"\A"),
            "must name the whole-value anchoring fix: {message}"
        );
    }
}

#[test]
fn not_equals_regex_rejection_points_at_the_negated_operator() {
    let message = first_diagnostic_message("p := [unidic.cform != /終止形/]."); // legacy-lx-syntax-ok
    assert!(
        message.contains("!~"),
        "a negated context must recommend `!~`: {message}"
    );
}

#[test]
fn positive_membership_regex_rejection_points_at_the_positive_operator() {
    // `in {/re/}` is positive, so recommending `!~` here would be wrong.
    let message = first_diagnostic_message("p := [unidic.cform in {/終止形/}].");
    assert!(message.contains('~'), "must recommend `~`: {message}");
    assert!(
        !message.contains("!~"),
        "must not recommend the negated operator: {message}"
    );
}

#[test]
fn equals_still_accepts_a_literal_and_stays_exact() {
    let matcher = compile_catalog(
        PatternSource::named("exact.lx", "p := [text = ない]."),
        CompileLimits::default(),
    )
    .expect("a literal operand still compiles")
    .matcher;
    assert_pattern_spans(&matcher, "p", &["ない", "じゃない"], &[(0, 1)]);
}

#[test]
fn tilde_rejects_a_set_operand() {
    let error = match compile_catalog(
        PatternSource::named("set.lx", "p := [text ~ {ない, 無い}]."),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("`~` must not accept a set operand"),
        Err(error) => error,
    };
    assert!(!error.diagnostics.is_empty());
}

/// The metadata surface added seven top-level keywords, and five of them cost
/// nothing: `required`, `string`, `number`, `boolean` and `list` are only valid
/// inside a `schema` block, so at top level they are absent from the parser's
/// valid-token set and lex as ordinary identifiers. `end` is safe for the same
/// reason.
///
/// One probe per name, because a single `ERROR` cascades and makes an
/// unaffected name downstream of it look broken.
#[test]
fn a_definition_may_be_named_after_a_block_scoped_metadata_keyword() {
    for name in ["required", "string", "number", "boolean", "list", "end"] {
        let source = format!("{name} := a.\n");
        compile_catalog(
            PatternSource::named("t.lx", &source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("`{name}` must remain a legal definition name: {error}"));
    }
}

/// The two that do cost something, pinned as the deliberate price.
///
/// `defaults` and `schema` are valid at top level, so the lexer commits them to
/// the keyword before the parser is consulted. Neither `word: $ => $.identifier`
/// nor a `conflicts:` entry avoids it — the generator calls those conflicts
/// *unnecessary*, because the grammar is unambiguous with one token of
/// lookahead and the collision is purely lexical. `pattern` and `fragment`
/// behave identically and predate this work; `metadata` was reserved the same
/// way and is freed by it.
#[test]
fn the_top_level_reserved_names_are_exactly_the_four_measured() {
    for name in ["defaults", "schema", "pattern", "fragment"] {
        let source = format!("{name} := a.\n");
        assert!(
            compile_catalog(
                PatternSource::named("t.lx", &source),
                CompileLimits::default()
            )
            .is_err(),
            "`{name}` is reserved at top level; if this now compiles, the reserved \
             set shrank and the results doc and AGENTS.md must say so"
        );
    }
    // Freed by this change: `metadata` was a top-level keyword and is not one now.
    compile_catalog(
        PatternSource::named("t.lx", "metadata := a.\n"),
        CompileLimits::default(),
    )
    .expect("`metadata` is no longer a keyword, so it is a legal definition name");
}

/// The CST's `pattern_reference` count and the compiler's `reference_edges`
/// answer the same question, and can finally be compared.
///
/// They could not be before: `metadata_line` was opaque, so a `@name` written
/// inside metadata was invisible to the CST scan while the compiler never saw
/// it either. Now every `@name` in a definition is a parsed node, and the two
/// counts must agree exactly. A disagreement means one side is counting
/// something the other is not — which is the defect, not a reason to adjust
/// either number.
///
/// Scoped to the definition graph, because a `policy_block` is the one place
/// where the two questions genuinely differ rather than disagree: it spells its
/// subjects and operands with the same `pattern_reference` node, and those name
/// a definition without adding an edge to expand. See
/// [`count_definition_graph_references`]. Widening the compiler's counter to
/// match would put a stat that bounds expansion work in charge of syntax that
/// costs none.
#[test]
fn the_cst_reference_count_and_the_compilers_agree_for_every_catalog() {
    for (name, source) in lingrep_core::testing::catalogs::tracked_catalog_sources() {
        let tree = parse_with_pinned_grammar(&source);
        let in_cst = count_definition_graph_references(tree.root_node());
        let output = compile_catalog(
            PatternSource::named(&name, &source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{name} must compile: {error}"));
        assert_eq!(
            output.stats.reference_edges(),
            in_cst,
            "{name}: the compiler counted {} reference edges and the CST holds {in_cst} \
             `pattern_reference` nodes",
            output.stats.reference_edges()
        );
    }
}

#[test]
fn removed_legacy_spellings_are_rejected() {
    for source in [
        "bad := [unidic.cform: 意志推量形].",
        "bad := [upos=VERB; unidic.cform=終止形].",
        "bad := [upos=VERB unidic.cform=終止形].",
        "[upos=VERB]",
        "metadata\n  category: x\nbegin\np := [text=a].\nend",
    ] {
        let tree = parse_with_pinned_grammar(source);
        assert!(
            tree.root_node().has_error(),
            "removed source must contain ERROR or MISSING recovery: {source}"
        );

        let error = match compile_catalog(
            PatternSource::named("legacy.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("removed syntax must fail: {source}"),
            Err(error) => error,
        };
        assert!(!error.diagnostics.is_empty(), "{source}");
        assert!(
            error
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.location().source_name() == "legacy.lx"),
            "{source}"
        );
    }
}

fn token(orth: &str, pos1: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Pos1, pos1.into());
    token
}

fn token_with_cform(orth: &str, cform: Option<&str>) -> TokenInfo {
    let mut value = token(orth, "X");
    if let Some(cform) = cform {
        value.set(TokenAttributeIndex::CForm, cform.into());
    }
    value
}

fn assert_pattern_spans(
    matcher: &lingrep_core::fsm::CompiledCatalog,
    pattern: &str,
    values: &[&str],
    expected: &[(usize, usize)],
) {
    let tokens = values
        .iter()
        .map(|value| token(value, "X"))
        .collect::<Vec<_>>();
    let actual = matcher
        .find_matches(&tokens)
        .into_iter()
        .filter(|matched| matched.pattern == pattern)
        .map(|matched| (matched.start, matched.end))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "pattern: {pattern}, values: {values:?}");
}

#[test]
fn normalized_grammar_preserves_precedence_and_complete_matches() {
    let matcher = compile_catalog(
        PatternSource::named(
            "precedence.lx",
            concat!(
                "left := a b | c.\n",
                "right := a | b c.\n",
                "grouped := (a | b) c.\n",
                "repeated := (a b)+.\n",
                "mixed := a? b+ | c{2,4}.\n",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("precedence fixture must compile")
    .matcher;

    assert_pattern_spans(&matcher, "left", &["a", "b"], &[(0, 2)]);
    assert_pattern_spans(&matcher, "left", &["c"], &[(0, 1)]);
    assert_pattern_spans(&matcher, "right", &["b", "c"], &[(0, 2)]);
    assert_pattern_spans(&matcher, "grouped", &["a", "c"], &[(0, 2)]);
    assert_pattern_spans(
        &matcher,
        "repeated",
        &["a", "b", "a", "b"],
        &[(0, 4), (2, 4)],
    );

    let contextual = compile_catalog(
        PatternSource::named("context.lx", "p := a b."),
        CompileLimits::default(),
    )
    .expect("context fixture must compile")
    .matcher
    .find_matches(&[
        token("x", "X"),
        token("a", "X"),
        token("b", "X"),
        token("y", "X"),
    ]);
    assert_eq!(
        contextual,
        [Match {
            pattern: "p".into(),
            start: 1,
            end: 3,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        }]
    );
}

#[test]
fn document_compilation_never_drops_definitions_or_metadata() {
    let source = r#"category: "test"
p := [text=a].
q := @p [text=b]."#;

    let output = compile_catalog(
        PatternSource::named("fixture.lx", source),
        CompileLimits::default(),
    )
    .unwrap();

    assert_eq!(
        output
            .matcher
            .patterns()
            .keys()
            .map(compact_str::CompactString::as_str)
            .collect::<Vec<_>>(),
        ["p", "q"]
    );
    assert_eq!(output.matcher.patterns()["p"].metadata["category"], "test");
}

#[test]
fn definition_source_cannot_compile_to_zero_patterns() {
    let output = compile_catalog(
        PatternSource::named("one.lx", "p := [text=a]."),
        CompileLimits::default(),
    )
    .unwrap();

    assert_eq!(output.matcher.patterns().len(), 1);
}

#[test]
fn new_list_nai_matches_exact_zu_but_not_words_containing_zu() {
    let matcher = compile_catalog(
        PatternSource::named(
            "nai.lx",
            concat!(
                "fragment NAI := [text in {ない, 無い, ず, ぬ, なかっ, 無かっ, ねえ}].\n",
                "pattern probe := @NAI."
            ),
        ),
        CompileLimits::default(),
    )
    .expect("NAI fragment must compile")
    .matcher;
    let tokens = [token("必ず", "X"), token("ず", "X"), token("まず", "X")];
    let nai_spans = matcher
        .find_matches(&tokens)
        .into_iter()
        .filter(|matched| matched.pattern == "probe")
        .map(|matched| (matched.start, matched.end))
        .collect::<Vec<_>>();

    assert_eq!(nai_spans, [(1, 2)]);
}

#[test]
fn sfe_exposes_the_retained_pattern_probes() {
    let matcher = compile_catalog(
        PatternSource::named("patterns/sfe.lx", include_str!("../../../patterns/sfe.lx")),
        CompileLimits::default(),
    )
    .expect("patterns/sfe.lx must compile")
    .matcher;

    assert_eq!(matcher.patterns().len(), 27);
    for helper in [
        "動詞終止形",
        "形容詞終止形",
        "助動詞終止形",
        "用言終止形",
        "動詞連用形",
        "形容詞語幹",
        "形状詞",
        "ARU",
        "KAKO",
        "NAI",
    ] {
        assert!(matcher.patterns().contains_key(helper), "{helper}");
    }
}

#[test]
fn finite_predicates_are_typed_and_missing_attributes_do_not_satisfy_negation() {
    let matcher = compile_catalog(
        PatternSource::named(
            "finite.lx",
            concat!(
                "allowed := [text in {ある, 有る}].\n",
                "not_kanarazu := [text != 必ず].\n",
                "not_blocked := [text not in {必ず, 決して}].",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("finite literal predicates must compile")
    .matcher;
    let tokens = [
        token("ある", "X"),
        token("有る", "X"),
        token("必ず", "X"),
        token("いかず", "X"),
        TokenInfo::default(),
    ];

    let matches = matcher.find_matches(&tokens);
    assert!(
        matches
            .iter()
            .any(|m| m.pattern == "allowed" && m.start == 0)
    );
    assert!(
        matches
            .iter()
            .any(|m| m.pattern == "allowed" && m.start == 1)
    );
    assert!(
        !matches
            .iter()
            .any(|m| m.pattern == "not_kanarazu" && m.start == 2)
    );
    assert!(
        matches
            .iter()
            .any(|m| m.pattern == "not_kanarazu" && m.start == 3)
    );
    assert!(
        !matches
            .iter()
            .any(|m| m.pattern == "not_kanarazu" && m.start == 4)
    );
    assert!(
        !matches
            .iter()
            .any(|m| m.pattern == "not_blocked" && m.start == 2)
    );
    assert!(
        matches
            .iter()
            .any(|m| m.pattern == "not_blocked" && m.start == 3)
    );
    assert!(
        !matches
            .iter()
            .any(|m| m.pattern == "not_blocked" && m.start == 4)
    );
}

#[test]
fn finite_predicate_shape_and_regex_errors_are_rejected() {
    for source in [
        "p := [text in {}].",
        "p := [text in value].",
        "p := [text != {value}].",
        "p := [text in {a, a}].",
        // Asserting that `!=` rejects a regex operand is the point of this
        // case, so the rejected spelling must survive here.
        "p := [text != /^a$/].", // legacy-lx-syntax-ok
        "p := [text in {/a/}].",
        "p := [text not in {/a/}].",
    ] {
        assert!(
            compile_catalog(
                PatternSource::named("invalid-finite.lx", source),
                CompileLimits::default()
            )
            .is_err(),
            "invalid finite predicate compiled: {source}"
        );
    }
}

#[test]
fn fragments_are_referenceable_but_never_emit_or_enter_the_runtime_catalog() {
    let matcher = compile_catalog(
        PatternSource::named(
            "fragment.lx",
            "fragment HELPER := [text=a].\npattern RESULT := @HELPER [text=b].",
        ),
        CompileLimits::default(),
    )
    .expect("a pattern may reference a fragment")
    .matcher;

    assert_eq!(
        matcher
            .patterns()
            .keys()
            .map(compact_str::CompactString::as_str)
            .collect::<Vec<_>>(),
        ["RESULT"]
    );
    assert_eq!(
        matcher
            .find_matches(&[token("a", "X"), token("b", "X")])
            .into_iter()
            .map(|matched| matched.pattern)
            .collect::<Vec<_>>(),
        ["RESULT"]
    );
}

#[test]
fn nullable_fragments_are_usable_but_nullable_patterns_and_repeats_are_rejected() {
    let matcher = compile_catalog(
        PatternSource::named(
            "nullable-fragment.lx",
            "fragment MAYBE := [text=a]?.\npattern P := @MAYBE [text=b].",
        ),
        CompileLimits::default(),
    )
    .expect("a nullable fragment is valid when its use still consumes")
    .matcher;
    assert_eq!(matcher.patterns().len(), 1);

    for (source, expected) in [
        (
            "pattern EMPTY := [text=a]?.",
            DiagnosticCode::NullablePattern,
        ),
        (
            "fragment MAYBE := [text=a]?.\npattern P := @MAYBE? [text=b].",
            DiagnosticCode::NullableRepeat,
        ),
        (
            "fragment MAYBE := [text=a]?.\npattern P := @MAYBE+ [text=b].",
            DiagnosticCode::NullableUnboundedRepeat,
        ),
    ] {
        let error = compile_catalog(
            PatternSource::named("invalid-nullability.lx", source),
            CompileLimits::default(),
        )
        .err()
        .expect("invalid nullability must be rejected");
        assert_eq!(error.diagnostics[0].code(), expected, "{source}");
    }
}

#[test]
fn anchors_and_previous_token_assertions_are_zero_width() {
    let matcher = compile_catalog(
        PatternSource::named(
            "assertions.lx",
            concat!(
                "pattern WHOLE := ^ [text=a] [text=b] $.\n",
                "pattern HEARSAY := preceded_by([unidic.cform != 連用形-一般]) [text=そう].",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("regular position assertions must compile")
    .matcher;

    let whole = [token("a", "X"), token("b", "X")];
    assert!(
        matcher
            .find_matches(&whole)
            .iter()
            .any(|matched| matched.pattern == "WHOLE" && (matched.start, matched.end) == (0, 2))
    );

    let cases = [
        (
            vec![
                token_with_cform("する", Some("終止形-一般")),
                token("そう", "X"),
            ],
            true,
        ),
        (
            vec![
                token_with_cform("し", Some("連用形-一般")),
                token("そう", "X"),
            ],
            false,
        ),
        (vec![token("そう", "X")], false),
        (
            vec![token_with_cform("前", None), token("そう", "X")],
            false,
        ),
    ];
    for (tokens, expected) in cases {
        assert_eq!(
            matcher
                .find_matches(&tokens)
                .iter()
                .any(|matched| matched.pattern == "HEARSAY"),
            expected
        );
    }
}

#[test]
fn metadata_values_containing_begin_are_not_truncated() {
    let source = r#"category: "beginning"
p := [text=a]."#;

    let output = compile_catalog(
        PatternSource::named("metadata-value.lx", source),
        CompileLimits::default(),
    )
    .unwrap();

    assert_eq!(
        output.matcher.patterns()["p"].metadata["category"],
        "beginning"
    );
}

#[test]
fn whole_document_and_multi_call_builder_are_equivalent() {
    let document = r#"category: "shared"
p := [text=a].
q := @p [text=b]."#;
    let whole = compile_catalog(
        PatternSource::named("whole.lx", document),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;

    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named(
            "p.lx",
            "category: \"shared\"\np := [text=a].",
        ))
        .unwrap();
    builder
        .add_definition(PatternSource::named("q.lx", "q := @p [text=b]."))
        .unwrap();
    let incremental = builder.build().unwrap().matcher;

    let catalog = |matcher: &lingrep_core::fsm::CompiledCatalog| {
        matcher
            .patterns()
            .iter()
            .map(|(name, pattern)| (name.clone(), pattern.metadata.clone()))
            .collect::<Vec<_>>()
    };
    let tokens = [token("a", "X"), token("b", "X")];

    assert_eq!(catalog(&incremental), catalog(&whole));
    assert_eq!(
        incremental.find_matches(&tokens),
        whole.find_matches(&tokens)
    );
}

#[test]
fn builder_resolves_forward_references_across_registration_calls() {
    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("use.lx", "pair := @atom [text=b]."))
        .unwrap();
    builder
        .add_definition(PatternSource::named("atom.lx", "atom := [text=a]."))
        .unwrap();

    let matcher = builder.build().unwrap().matcher;
    let matches = matcher.find_matches(&[token("a", "X"), token("b", "X")]);

    assert!(
        matches
            .iter()
            .any(|matched| matched.pattern == "pair" && matched.start == 0 && matched.end == 2)
    );
}

#[test]
fn compiled_catalog_contains_every_definition_and_its_metadata() {
    let source = r#"category: "shared"
status: "checked"
p := [text=a].
category: "shared"
status: "checked"
q := [text=b].
r := [text=c]."#;

    let output = compile_catalog(
        PatternSource::named("catalog.lx", source),
        CompileLimits::default(),
    )
    .unwrap();
    let catalog = output.matcher.patterns();

    assert_eq!(
        catalog
            .keys()
            .map(compact_str::CompactString::as_str)
            .collect::<Vec<_>>(),
        ["p", "q", "r"]
    );
    assert_eq!(catalog["p"].metadata["category"], "shared");
    assert_eq!(catalog["q"].metadata["status"], "checked");
    assert!(catalog["r"].metadata.is_empty());
}

#[test]
fn duplicate_builder_registrations_fail_when_the_complete_program_is_bound() {
    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("first.lx", "p := [text=a]."))
        .unwrap();
    builder
        .add_definition(PatternSource::named("second.lx", "p := [text=b]."))
        .unwrap();

    let error = match builder.build() {
        Ok(_) => panic!("duplicate definitions must not produce a matcher"),
        Err(error) => error,
    };

    assert_eq!(
        error.diagnostics[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert!(
        error.diagnostics[0]
            .message
            .contains("duplicate definition p")
    );
}

#[test]
fn failed_builder_build_is_atomic_and_does_not_publish_a_partial_matcher() {
    let mut invalid = FsmBuilder::new();
    invalid
        .add_definition(PatternSource::named("valid.lx", "valid := [text=a]."))
        .unwrap();
    invalid
        .add_definition(PatternSource::named("invalid.lx", "invalid := @missing."))
        .unwrap();
    assert!(invalid.build().is_err());

    let mut valid = FsmBuilder::new();
    valid
        .add_definition(PatternSource::named("valid.lx", "valid := [text=a]."))
        .unwrap();
    let matcher = valid.build().unwrap().matcher;

    assert_eq!(matcher.patterns().len(), 1);
    assert_eq!(matcher.find_matches(&[token("a", "X")]).len(), 1);
}

#[test]
fn binding_diagnostics_preserve_caller_source_identities() {
    let whole_error = compile_catalog(
        PatternSource::named("whole-error.lx", "p := @missing."),
        CompileLimits::default(),
    )
    .err()
    .expect("undefined whole-document reference must fail");
    assert_eq!(
        whole_error.diagnostics[0].location().source_name(),
        "whole-error.lx"
    );

    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("first.lx", "p := [text=a]."))
        .unwrap();
    builder
        .add_definition(PatternSource::named("second.lx", "p := [text=b]."))
        .unwrap();
    let error = match builder.build() {
        Ok(_) => panic!("duplicate definitions must not produce a matcher"),
        Err(error) => error,
    };

    assert_eq!(error.diagnostics[0].location().source_name(), "second.lx");
}

#[test]
fn invalid_regex_diagnostics_never_echo_submitted_source() {
    const SECRET: &str = "PATTERN_REVIEW_SECRET_7F92C1";
    let source = format!("秘密 := [text~/({SECRET}/].");
    let error = compile_catalog(
        PatternSource::named("safe-regex.lx", &source),
        CompileLimits::default(),
    )
    .err()
    .expect("an unclosed regex group must be rejected");
    let diagnostic = &error.diagnostics[0];

    assert_eq!(diagnostic.code(), DiagnosticCode::InvalidRegex);
    assert_eq!(diagnostic.location().source_name(), "safe-regex.lx");
    assert_eq!(diagnostic.location().line(), 1);
    assert!(diagnostic.location().column() > 1);
    assert_eq!(diagnostic.message, "invalid Rust regex");
    assert!(!diagnostic.message.contains(SECRET));
    assert!(!error.to_string().contains(SECRET));
    assert!(!error.to_string().contains(&source));
}

#[test]
fn multiline_unicode_undefined_reference_points_to_the_reference_span() {
    let source = "先頭 := 猫.\n対象 :=\n  猫 @未定.";
    let reference_start = source.find("@未定").expect("fixture has a reference");
    let error = compile_catalog(
        PatternSource::named("undefined-location.lx", source),
        CompileLimits::default(),
    )
    .err()
    .expect("undefined reference must fail binding");
    let diagnostic = &error.diagnostics[0];

    assert_eq!(diagnostic.code(), DiagnosticCode::UndefinedReference);
    assert_eq!(
        diagnostic.location().byte_range(),
        reference_start..reference_start + "@未定".len()
    );
    assert_eq!(diagnostic.location().line(), 3);
    assert_eq!(diagnostic.location().column(), 7);
}

#[test]
fn multiline_duplicate_locations_include_the_original_definition() {
    let source = "日本 := 猫.\n\n日本 := 犬.";
    let second_start = source.rfind("日本").expect("fixture has a duplicate");
    let error = compile_catalog(
        PatternSource::named("duplicate-location.lx", source),
        CompileLimits::default(),
    )
    .err()
    .expect("duplicate definition must fail binding");
    let diagnostic = &error.diagnostics[0];

    assert_eq!(diagnostic.code(), DiagnosticCode::DuplicateDefinition);
    assert_eq!(diagnostic.location().byte_range().start, second_start);
    assert_eq!(diagnostic.location().line(), 3);
    assert_eq!(diagnostic.location().column(), 1);
    assert_eq!(diagnostic.related_locations().len(), 1);
    assert_eq!(diagnostic.related_locations()[0].location().line(), 1);
    assert_eq!(diagnostic.related_locations()[0].location().column(), 1);
}

#[test]
fn multiline_cycle_points_to_the_unicode_prefixed_closing_reference() {
    let source = "甲 := @乙.\n乙 :=\n  猫 @甲.";
    let reference_start = source.rfind("@甲").expect("fixture closes a cycle");
    let error = compile_catalog(
        PatternSource::named("cycle-location.lx", source),
        CompileLimits::default(),
    )
    .err()
    .expect("reference cycle must fail binding");
    let diagnostic = &error.diagnostics[0];

    assert_eq!(diagnostic.code(), DiagnosticCode::ReferenceCycle);
    assert_eq!(
        diagnostic.location().byte_range(),
        reference_start..reference_start + "@甲".len()
    );
    assert_eq!(diagnostic.location().line(), 3);
    assert_eq!(diagnostic.location().column(), 7);
}

#[test]
fn whole_document_source_budget_rejects_huge_label_before_echoing_it() {
    let source_name = "秘密ラベル".repeat(200_000);
    let error = compile_catalog(
        PatternSource::named(&source_name, "p := [text=a]."),
        CompileLimits::default(),
    )
    .err()
    .expect("an oversized diagnostic label must exhaust the source byte budget");
    let diagnostic = &error.diagnostics[0];

    assert_eq!(diagnostic.code(), DiagnosticCode::LimitExceeded);
    assert_eq!(diagnostic.location().source_name(), "<pattern>");
    assert!(diagnostic.message.contains("source byte limit"));
    assert!(!diagnostic.message.contains(&source_name));
}

#[test]
fn builder_source_budget_accumulates_text_and_utf8_labels() {
    let first_name = "第一.lx";
    let first_text = "first := [text=a].";
    let second_name = "第二.lx";
    let second_text = "second := [text=b].";
    let total_bytes = first_name.len() + first_text.len() + second_name.len() + second_text.len();
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_source_bytes = total_bytes - 1)
        .unwrap();
    let mut builder = FsmBuilder::with_limits(limits);

    builder
        .add_definition(PatternSource::named(first_name, first_text))
        .expect("the first source must fit the cumulative budget");
    let error = builder
        .add_definition(PatternSource::named(second_name, second_text))
        .expect_err("the second UTF-8 label must exhaust the cumulative source budget");

    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
    assert_eq!(error.diagnostics[0].location().source_name(), second_name);
    assert!(error.diagnostics[0].message.contains("source bytes"));

    let exact_limits = CompileLimits::default()
        .tightened(|limits| limits.max_source_bytes = total_bytes)
        .unwrap();
    let mut exact_builder = FsmBuilder::with_limits(exact_limits);
    exact_builder
        .add_definition(PatternSource::named(first_name, first_text))
        .unwrap();
    exact_builder
        .add_definition(PatternSource::named(second_name, second_text))
        .unwrap();
    let output = exact_builder.build().unwrap();

    assert_eq!(output.stats.source_bytes(), total_bytes);
}

#[test]
fn many_definitions_charge_one_shared_source_label() {
    let source_name = "many-definitions.lx";
    let mut source = String::new();
    for index in 0..128 {
        writeln!(source, "p{index} := [text=a].").unwrap();
    }

    let output = compile_catalog(
        PatternSource::named(source_name, &source),
        CompileLimits::default(),
    )
    .unwrap();

    assert_eq!(output.stats.definitions(), 128);
    assert_eq!(
        output.stats.source_bytes(),
        source.len() + source_name.len()
    );
}

#[test]
fn duplicate_diagnostics_share_one_source_label_allocation() {
    let source_name = "shared-diagnostics.lx";
    let source = "duplicate := [text=a].\n".repeat(64);
    let error = compile_catalog(
        PatternSource::named(source_name, &source),
        CompileLimits::default(),
    )
    .err()
    .expect("duplicate definitions must produce diagnostics");
    let shared_name = error.diagnostics[0].location().source_name();

    assert_eq!(error.diagnostics.len(), 63);
    for diagnostic in &error.diagnostics {
        assert!(std::ptr::eq(
            diagnostic.location().source_name(),
            shared_name
        ));
        assert!(std::ptr::eq(
            diagnostic.related_locations()[0].location().source_name(),
            shared_name
        ));
    }
}

#[test]
fn compile_output_reports_phase_and_nfa_measurements() {
    let source = "atom := [text=a].\npair := @atom [unidic.pos1=X].";
    let output = compile_catalog(
        PatternSource::named("stats.lx", source),
        CompileLimits::default(),
    )
    .unwrap();

    assert_eq!(output.stats.source_bytes(), source.len() + "stats.lx".len());
    assert_eq!(output.stats.definitions(), 2);
    assert_eq!(output.stats.reference_edges(), 1);
    assert!(output.stats.expression_nodes() >= 4);
    assert_eq!(output.stats.expansion_work(), 5);
    assert_eq!(output.stats.nfa_states(), 5);
    assert_eq!(output.stats.transitions(), 5);
    assert!(output.stats.generated_owned_bytes() > 0);
}

#[test]
fn generated_owned_byte_limit_is_tightening_only() {
    let error = CompileLimits::default()
        .tightened(|limits| limits.max_generated_owned_bytes = usize::MAX)
        .expect_err("generated owned bytes must not be raisable");

    assert_eq!(error.budget(), "generated owned bytes");
}

#[test]
fn large_literal_quantifier_exhausts_generated_owned_bytes_atomically() {
    let literal = "x".repeat(8_192);
    let source = format!("expanded := [text={literal}]{{64}}.");
    let measured = compile_catalog(
        PatternSource::named("large-literal-quantifier.lx", &source),
        CompileLimits::default(),
    )
    .expect("the generated-payload probe must fit supported defaults")
    .stats;

    assert!(
        measured.generated_owned_bytes() >= literal.len() * 64,
        "transition fan-out must charge every owned literal payload"
    );

    let limits = CompileLimits::default()
        .tightened(|limits| {
            limits.max_generated_owned_bytes = measured.generated_owned_bytes() - 1;
        })
        .expect("one below the measured generated bytes is a valid tightening");
    let attempt = std::panic::catch_unwind(|| {
        compile_catalog(
            PatternSource::named("large-literal-quantifier.lx", &source),
            limits,
        )
    });
    let error = attempt
        .expect("generated owned byte exhaustion must not panic")
        .err()
        .expect("generated owned byte exhaustion must not publish a matcher");

    assert_eq!(error.diagnostics.len(), 1);
    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
    assert!(
        error.diagnostics[0]
            .message
            .contains("generated owned bytes")
    );
}

#[test]
fn shared_metadata_fan_out_exhausts_generated_owned_bytes_atomically() {
    const DEFINITIONS: usize = 32;
    let metadata_value = "m".repeat(8_192);
    let mut source = format!("defaults\n  note: \"{metadata_value}\"\nend\n\n");
    for index in 0..DEFINITIONS {
        writeln!(source, "p{index} := [text=x].").unwrap();
    }

    let measured = compile_catalog(
        PatternSource::named("shared-metadata-fan-out.lx", &source),
        CompileLimits::default(),
    )
    .expect("the shared-metadata probe must fit supported defaults")
    .stats;

    assert!(
        measured.generated_owned_bytes() >= metadata_value.len() * DEFINITIONS,
        "every final catalog metadata clone must be charged"
    );

    let limits = CompileLimits::default()
        .tightened(|limits| {
            limits.max_generated_owned_bytes = measured.generated_owned_bytes() - 1;
        })
        .expect("one below the measured generated bytes is a valid tightening");
    let attempt = std::panic::catch_unwind(|| {
        compile_catalog(
            PatternSource::named("shared-metadata-fan-out.lx", &source),
            limits,
        )
    });
    let error = attempt
        .expect("generated owned byte exhaustion must not panic")
        .err()
        .expect("generated owned byte exhaustion must not publish a matcher");

    assert_eq!(error.diagnostics.len(), 1);
    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
    assert!(
        error.diagnostics[0]
            .message
            .contains("generated owned bytes")
    );
}

#[test]
fn every_compile_budget_rejects_one_below_atomically_without_panicking() {
    let source_name = "all-budgets.lx";
    let source = r#"category: "measured"
atom := [text~/a+/].
category: "measured"
pair := ((@atom [unidic.pos1=X]) | [text=b])."#;
    let measured = compile_catalog(
        PatternSource::named(source_name, source),
        CompileLimits::default(),
    )
    .expect("the adversarial fixture must compile under supported defaults")
    .stats;

    let cases = [
        (
            "source bytes",
            "source byte",
            CompileLimits::default().tightened(|limits| {
                limits.max_source_bytes = measured.source_bytes() - 1;
            }),
        ),
        (
            "definitions",
            "definition",
            CompileLimits::default().tightened(|limits| {
                limits.max_definitions = measured.definitions() - 1;
            }),
        ),
        (
            "expression nodes",
            "expression-node",
            CompileLimits::default().tightened(|limits| {
                limits.max_expression_nodes = measured.expression_nodes() - 1;
            }),
        ),
        (
            "nesting depth",
            "nesting-depth",
            CompileLimits::default().tightened(|limits| {
                limits.max_nesting_depth = measured.max_depth() - 1;
            }),
        ),
        (
            "metadata bytes",
            "metadata byte",
            CompileLimits::default().tightened(|limits| {
                limits.max_metadata_bytes = measured.metadata_bytes() - 1;
            }),
        ),
        (
            "regex bytes",
            "regex byte",
            CompileLimits::default().tightened(|limits| {
                limits.max_regex_bytes = measured.regex_bytes() - 1;
            }),
        ),
        (
            "reference edges",
            "reference-edge",
            CompileLimits::default().tightened(|limits| {
                limits.max_reference_edges = measured.reference_edges() - 1;
            }),
        ),
        (
            "expansion work",
            "expansion work",
            CompileLimits::default().tightened(|limits| {
                limits.max_expansion_work = measured.expansion_work() - 1;
            }),
        ),
        (
            "generated owned bytes",
            "generated owned bytes",
            CompileLimits::default().tightened(|limits| {
                limits.max_generated_owned_bytes = measured.generated_owned_bytes() - 1;
            }),
        ),
        (
            "NFA states",
            "NFA states",
            CompileLimits::default().tightened(|limits| {
                limits.max_nfa_states = measured.nfa_states() - 1;
            }),
        ),
        (
            "transitions",
            "transitions",
            CompileLimits::default().tightened(|limits| {
                limits.max_transitions = measured.transitions() - 1;
            }),
        ),
    ];

    for (budget, diagnostic_fragment, limits) in cases {
        let limits = limits.expect("one-below limits must be valid tightening");
        let attempt = std::panic::catch_unwind(|| {
            compile_catalog(PatternSource::named(source_name, source), limits)
        });
        let error = attempt
            .unwrap_or_else(|_| panic!("{budget} exhaustion must not panic"))
            .err()
            .unwrap_or_else(|| panic!("{budget} exhaustion must not publish a matcher"));

        assert_eq!(error.diagnostics.len(), 1, "budget: {budget}");
        assert_eq!(
            error.diagnostics[0].code(),
            DiagnosticCode::LimitExceeded,
            "budget: {budget}"
        );
        assert!(
            error.diagnostics[0].message.contains(diagnostic_fragment),
            "diagnostic for {budget} did not name its budget: {}",
            error.diagnostics[0].message
        );
    }
}

#[test]
fn compiler_stats_report_metadata_source_bytes() {
    let metadata_line = "  category: \"shared\"";
    let source = format!("{metadata_line}\np := [text=a].");
    let output = compile_catalog(
        PatternSource::named("metadata-stats.lx", &source),
        CompileLimits::default(),
    )
    .unwrap();

    // Leading indentation is `extras`, outside the `metadata_entry` node, so
    // it is not charged.
    assert_eq!(
        output.stats.metadata_bytes(),
        metadata_line.trim_start().len()
    );
}

#[test]
fn compiler_stats_charge_the_entry_not_the_terminator_for_crlf() {
    let entry = "category: \"shared\"";
    let source = format!("  {entry}\r\np := [text=a].\r\n");
    let exact_limits = CompileLimits::default()
        .tightened(|limits| limits.max_metadata_bytes = entry.len())
        .unwrap();
    let output = compile_catalog(
        PatternSource::named("metadata-crlf.lx", &source),
        exact_limits,
    )
    .unwrap();

    // A `metadata_entry` ends at its value, so it can never contain a line
    // terminator. The CR that the removed `metadata_line` had to account for
    // is outside the node, and so is the indentation: neither is charged.
    assert_eq!(output.stats.metadata_bytes(), entry.len());

    let one_below = CompileLimits::default()
        .tightened(|limits| limits.max_metadata_bytes = entry.len() - 1)
        .unwrap();
    let error = compile_catalog(PatternSource::named("metadata-crlf.lx", &source), one_below)
        .err()
        .expect("the authored entry must count toward the whole-document limit");
    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
}

#[test]
fn builder_accumulates_strict_lower_crlf_metadata_bytes() {
    let entry = "k: \"a\"";
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_metadata_bytes = entry.len() * 2 - 1)
        .unwrap();
    let first = format!("  {entry}\r\na := [text=a].\r\n");
    let second = format!("  {entry}\r\nb := [text=b].\r\n");
    let mut builder = FsmBuilder::with_limits(limits);

    builder
        .add_definition(PatternSource::named("first-crlf.lx", &first))
        .unwrap();
    let error = builder
        .add_definition(PatternSource::named("second-crlf.lx", &second))
        .expect_err("the accumulated strict-lower metadata budget must be enforced");

    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
    assert_eq!(
        error.diagnostics[0].location().source_name(),
        "second-crlf.lx"
    );
    assert!(error.diagnostics[0].message.contains("metadata bytes"));
}

#[test]
fn zero_state_doubling_dag_hits_expansion_work_limit() {
    let mut source = String::from("fragment d0 := [text=x]{0}.\n");
    for level in 1..=16 {
        writeln!(
            source,
            "fragment d{level} := @d{} @d{}.",
            level - 1,
            level - 1
        )
        .unwrap();
    }
    source.push_str("pattern root := @d16 [text=x].\n");
    let error = compile_catalog(
        PatternSource::named("zero-state-dag.lx", &source),
        CompileLimits::default(),
    )
    .err()
    .expect("expanded work must be bounded independently of generated NFA size");

    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
    assert!(error.diagnostics[0].message.contains("expansion work"));
}

#[test]
fn nfa_state_and_transition_limits_reject_compilation_atomically() {
    for (budget, limits) in [
        (
            "NFA states",
            CompileLimits::default()
                .tightened(|limits| limits.max_nfa_states = 0)
                .unwrap(),
        ),
        (
            "transitions",
            CompileLimits::default()
                .tightened(|limits| limits.max_transitions = 0)
                .unwrap(),
        ),
    ] {
        let error = compile_catalog(PatternSource::named("limit.lx", "p := [text=a]."), limits)
            .err()
            .expect("an exhausted NFA budget must fail compilation");

        assert_eq!(error.diagnostics.len(), 1, "budget: {budget}");
        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
        assert!(error.diagnostics[0].message.contains(budget));
    }
}

#[test]
fn nfa_limits_reject_exactly_one_below_generated_totals() {
    let source = "p := [text=a] | [text=b].";
    let measured = compile_catalog(
        PatternSource::named("one-below.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .stats;

    let exact_limits = CompileLimits::default()
        .tightened(|limits| {
            limits.max_nfa_states = measured.nfa_states();
            limits.max_transitions = measured.transitions();
        })
        .unwrap();
    compile_catalog(PatternSource::named("exact.lx", source), exact_limits)
        .expect("the exact generated totals must be accepted");

    for (budget, limits) in [
        (
            "NFA states",
            CompileLimits::default()
                .tightened(|limits| limits.max_nfa_states = measured.nfa_states() - 1)
                .unwrap(),
        ),
        (
            "transitions",
            CompileLimits::default()
                .tightened(|limits| limits.max_transitions = measured.transitions() - 1)
                .unwrap(),
        ),
    ] {
        let error = compile_catalog(PatternSource::named("one-below.lx", source), limits)
            .err()
            .expect("one below the generated total must be rejected");
        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
        assert!(error.diagnostics[0].message.contains(budget));
    }
}

#[test]
fn lookaround_automata_are_charged_to_existing_compile_limits() {
    let source = "p := preceded_by(([text=a] | [text=b])) [text=c].";
    let outer_only = compile_catalog(
        PatternSource::named("outer-only.lx", "p := ^ [text=c]."),
        CompileLimits::default(),
    )
    .unwrap()
    .stats;
    let measured = compile_catalog(
        PatternSource::named("lookaround-limits.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .stats;

    // Six states: two per branch atom, and two for the union that reads them.
    // A top-level alternation is interned as one program per branch plus a
    // union over their rows, so the state count is what the single alternation
    // automaton cost before — but four transitions replace six, because the
    // alternation's four epsilon edges into and out of its branches become two
    // zero-width `Fact` edges, one per member.
    assert_eq!(measured.nfa_states(), outer_only.nfa_states() + 6);
    assert_eq!(measured.transitions(), outer_only.transitions() + 4);
    assert!(measured.expansion_work() >= outer_only.expansion_work() + 3);
    assert!(
        measured.generated_owned_bytes() > outer_only.generated_owned_bytes(),
        "the assertion catalog and its cloned predicates must consume owned-byte budget"
    );

    let exact = CompileLimits::default()
        .tightened(|limits| {
            limits.max_expansion_work = measured.expansion_work();
            limits.max_generated_owned_bytes = measured.generated_owned_bytes();
            limits.max_nfa_states = measured.nfa_states();
            limits.max_transitions = measured.transitions();
        })
        .unwrap();
    compile_catalog(PatternSource::named("lookaround-exact.lx", source), exact)
        .expect("the exact combined NFA and assertion totals must compile");

    for (budget, limits) in [
        (
            "expansion work",
            CompileLimits::default().tightened(|limits| {
                limits.max_expansion_work = measured.expansion_work() - 1;
            }),
        ),
        (
            "generated owned bytes",
            CompileLimits::default().tightened(|limits| {
                limits.max_generated_owned_bytes = measured.generated_owned_bytes() - 1;
            }),
        ),
        (
            "NFA states",
            CompileLimits::default().tightened(|limits| {
                limits.max_nfa_states = measured.nfa_states() - 1;
            }),
        ),
        (
            "transitions",
            CompileLimits::default().tightened(|limits| {
                limits.max_transitions = measured.transitions() - 1;
            }),
        ),
    ] {
        let error = compile_catalog(
            PatternSource::named("lookaround-one-below.lx", source),
            limits.unwrap(),
        )
        .err()
        .expect("one below a combined generated total must fail");
        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
        assert!(
            error.diagnostics[0].message.contains(budget),
            "diagnostic did not name {budget}: {}",
            error.diagnostics[0].message
        );
    }
}

#[test]
fn typed_compilation_preserves_required_attributes() {
    let output = compile_catalog(
        PatternSource::named(
            "attrs.lx",
            "p := [text=a & unidic.pos1=X] followed_by([unidic.cform=終止形-一般]).",
        ),
        CompileLimits::default(),
    )
    .unwrap();

    assert_eq!(
        output.matcher.required_attributes(),
        [
            AttributeKey::new("text").unwrap(),
            AttributeKey::new("unidic.cform").unwrap(),
            AttributeKey::new("unidic.pos1").unwrap(),
        ]
    );
}

#[test]
fn typed_compilation_keeps_full_match_output_and_canonical_order() {
    let source = "b := [text=a].\na := [text=a].\npair := [text=a] [unidic.pos1=X].";
    let typed = compile_catalog(
        PatternSource::named("parity.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;

    let tokens = [token("a", "X"), token("z", "Y")];

    let expected = vec![
        Match {
            pattern: "a".into(),
            start: 0,
            end: 1,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        },
        Match {
            pattern: "b".into(),
            start: 0,
            end: 1,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        },
    ];

    assert_eq!(typed.find_matches(&tokens), expected);
}

#[test]
fn final_tree_lowering_has_golden_full_match_coverage() {
    struct GoldenCase {
        name: &'static str,
        source: &'static str,
        tokens: &'static [&'static str],
        expected: Vec<Match>,
    }

    let cases = [
        GoldenCase {
            name: "alternation",
            source: "choice := [text=a] | [text=b].",
            tokens: &["z", "a", "b", "z"],
            expected: vec![full_match("choice", 1, 2), full_match("choice", 2, 3)],
        },
        GoldenCase {
            name: "quantified reference",
            source: "atom := [text=a].\ndouble := @atom{2}.",
            tokens: &["z", "a", "a", "z"],
            expected: vec![
                full_match("atom", 1, 2),
                full_match("double", 1, 3),
                full_match("atom", 2, 3),
            ],
        },
        GoldenCase {
            name: "nested sequence",
            source: "nested := ([text=a] ([text=b] [text=c])) [text=d].",
            tokens: &["z", "a", "b", "c", "d", "z"],
            expected: vec![full_match("nested", 1, 5)],
        },
        GoldenCase {
            name: "named and unnamed group erasure",
            source: concat!(
                "plain := [text=a] [text=b].\n",
                "unnamed := ([text=a] [text=b]).\n",
                "named := (?<pair> [text=a] [text=b]).\n",
                "nested_group := ((?<outer> ([text=a] [text=b]))).",
            ),
            tokens: &["z", "a", "b", "z"],
            expected: vec![
                with_capture(full_match("named", 1, 3), "pair", 1, 3),
                with_capture(full_match("nested_group", 1, 3), "outer", 1, 3),
                full_match("plain", 1, 3),
                full_match("unnamed", 1, 3),
            ],
        },
    ];

    for case in cases {
        let matcher = compile_catalog(
            PatternSource::named(case.name, case.source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{} must compile: {error}", case.name))
        .matcher;
        let tokens = case
            .tokens
            .iter()
            .map(|orth| token(orth, "X"))
            .collect::<Vec<_>>();

        assert_eq!(
            matcher.find_matches(&tokens),
            case.expected,
            "{}",
            case.name
        );
    }
}

fn full_match(pattern: &str, start: usize, end: usize) -> Match {
    Match {
        pattern: pattern.into(),
        start,
        end,
        captures: Box::default(),
        graph_bindings: Box::default(),
        lexical: None,
    }
}

fn with_capture(mut matched: Match, name: &str, start: usize, end: usize) -> Match {
    matched.captures = vec![lingrep_core::fsm::Capture {
        name: name.into(),
        path: format!("{}/{}", matched.pattern, name).into(),
        start,
        end,
    }]
    .into_boxed_slice();
    matched
}

fn with_captures(mut matched: Match, captures: &[(&str, usize, usize)]) -> Match {
    matched.captures = captures
        .iter()
        .map(|&(name, start, end)| lingrep_core::fsm::Capture {
            name: name.into(),
            path: format!("{}/{}", matched.pattern, name).into(),
            start,
            end,
        })
        .collect();
    matched
}

#[test]
fn pinned_grammar_parses_the_any_token_predicate() {
    // Grammar acceptance only; matching semantics are asserted separately.
    for source in [
        "p := [].",
        "p := [] [upos=NOUN].",
        "p := []{2,5}.",
        "p := [upos=NOUN] [] [upos=VERB].",
    ] {
        let tree = parse_with_pinned_grammar(source);
        assert!(
            !tree.root_node().has_error(),
            "pinned grammar must parse {source} without recovery nodes"
        );
    }
}

#[test]
fn any_token_predicate_matches_every_token_including_attribute_less_ones() {
    let matcher = compile_catalog(
        PatternSource::named(
            "any-token.lx",
            concat!(
                "any := [].\n",
                "noun_then_any := [upos=NOUN] [].\n",
                "any_pair := []{2}.\n",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("any-token catalog compiles")
    .matcher;

    // `[]` constrains nothing, so every one-token window matches.
    assert_pattern_spans(&matcher, "any", &["a", "b", "c"], &[(0, 1), (1, 2), (2, 3)]);
    assert_pattern_spans(&matcher, "any_pair", &["a", "b", "c"], &[(0, 2), (1, 3)]);

    // `[]` in a SEQUENCE, after a constrained term. `assert_pattern_spans` builds
    // its tokens with `token(orth, pos1)`, which sets `pos1` and leaves `pos`
    // UNSET -- so `[upos=NOUN]` would be unsatisfiable there under the
    // missing-attribute rule and the sequence would be vacuously empty. Build
    // the tokens explicitly, setting the attribute `[upos=...]` actually reads
    // (`attribute_index("pos")` returns `TokenAttributeIndex::Pos`, not `Pos1`).
    let tokens = [
        token_with_attrs(&[(TokenAttributeIndex::Pos, "NOUN")]),
        token_with_attrs(&[(TokenAttributeIndex::Pos, "ADP")]),
    ];
    let spans = matcher
        .find_matches(&tokens)
        .into_iter()
        .filter(|matched| matched.pattern == "noun_then_any")
        .map(|matched| (matched.start, matched.end))
        .collect::<Vec<_>>();
    assert_eq!(spans, vec![(0, 2)], "`[]` must consume exactly one token");

    // A token carrying NO attributes at all still matches `[]`. This is the one
    // place the missing-attribute rule does not bite: there is no attribute to
    // be missing. `TokenInfo::default()` has every attribute unset.
    let bare = [TokenInfo::default()];
    let matches = matcher
        .find_matches(&bare)
        .into_iter()
        .filter(|matched| matched.pattern == "any")
        .count();
    assert_eq!(matches, 1, "`[]` must match a token with no attributes set");
}

/// A `preceded_by` with a single-token body is compiled as a specialized
/// `PositionAssertion::PreviousToken` rather than a general assertion program,
/// and `compile_assertions::collect` therefore registers no occurrence for it.
/// That specialization is only valid at the top level: inside another
/// lookaround's body, `AutomatonBuilder::compile` needs an occurrence to build
/// its `Fact` edge, and must not `.expect()` one into existence, which would
/// abort the process with exit 101 instead of returning a diagnostic.
///
/// `pattern-language-co-development.md` states that nested lookaround is valid,
/// so these must compile.
#[test]
fn a_specialized_preceded_by_nested_in_a_lookaround_body_compiles() {
    for source in [
        "p := preceded_by(preceded_by([upos=NOUN])) [text=x].",
        "p := preceded_by(preceded_by([upos=NOUN]) [text=z]) [text=x].",
        "p := preceded_by([text=z] preceded_by([upos=NOUN])) [text=x].",
        "p := followed_by(preceded_by([upos=NOUN]) [text=z]) [text=x].",
        "p := not_followed_by(preceded_by([upos=NOUN])) [text=x].",
    ] {
        let result = compile_catalog(
            PatternSource::named("nested.lx", source),
            CompileLimits::default(),
        );
        assert!(
            result.is_ok(),
            "nested lookaround must compile, got {:?} for {source}",
            result.err().map(|error| error.to_string())
        );
    }
}

/// The forms that already worked, kept alongside the regression so a future fix
/// cannot restore the panic for one nesting shape while fixing another. Only the
/// positive single-token backward form was ever specialized, so only it failed.
#[test]
fn the_other_nested_lookaround_forms_still_compile() {
    for source in [
        "p := preceded_by(not_preceded_by([upos=NOUN]) [text=z]) [text=x].",
        "p := followed_by(followed_by([upos=NOUN]) [text=z]) [text=x].",
        "p := preceded_by(preceded_by([upos=NOUN] [upos=VERB]) [text=z]) [text=x].",
        "p := preceded_by(preceded_by([upos=NOUN]{2}) [text=z]) [text=x].",
    ] {
        assert!(
            compile_catalog(
                PatternSource::named("nested-ok.lx", source),
                CompileLimits::default()
            )
            .is_ok(),
            "{source}"
        );
    }
}

/// The specialization must survive the fix. A top-level single-token
/// `preceded_by` is the common idiom (`sfe.lx` and `sf-experimental.lx`
/// both use it); compiling it as a general assertion program instead would add
/// an automaton and its owned bytes to every such pattern.
#[test]
fn a_top_level_preceded_by_is_still_specialized_and_builds_no_automaton() {
    let specialized = compile_catalog(
        PatternSource::named("specialized.lx", "p := preceded_by([text=a]) [text=c]."),
        CompileLimits::default(),
    )
    .expect("a top-level single-token preceded_by compiles")
    .stats;
    let general = compile_catalog(
        PatternSource::named(
            "general.lx",
            "p := preceded_by([text=a] [text=b]) [text=c].",
        ),
        CompileLimits::default(),
    )
    .expect("a multi-token preceded_by compiles")
    .stats;

    assert!(
        specialized.generated_owned_bytes() < general.generated_owned_bytes(),
        "the specialized form must not build an assertion program: \
         specialized={} general={}",
        specialized.generated_owned_bytes(),
        general.generated_owned_bytes()
    );
}

/// The first non-fatal diagnostic the compiler emits. Before this,
/// `CompileOutput::diagnostics` was constructed empty and sorted, so no caller
/// could ever observe a warning and `Diagnostic::warning` had zero callers.
#[test]
fn an_unreferenced_fragment_warns_without_failing_the_compile() {
    let output = compile_catalog(
        PatternSource::named(
            "frag.lx",
            "fragment unused := [upos=NOUN].\nfragment used := [upos=VERB].\np := @used.\n",
        ),
        CompileLimits::default(),
    )
    .expect("an unreferenced fragment is not a compile error");

    let codes: Vec<_> = output
        .diagnostics
        .iter()
        .map(|d| (d.code(), d.severity(), d.message.clone()))
        .collect();
    assert_eq!(codes.len(), 1, "exactly one warning expected: {codes:?}");
    assert_eq!(codes[0].0, DiagnosticCode::UnusedFragment);
    assert_eq!(codes[0].1, lingrep_core::fsm::DiagnosticSeverity::Warning);
    assert!(codes[0].2.contains("unused"), "{:?}", codes[0].2);
    assert!(
        !codes[0].2.contains("`used`"),
        "a referenced fragment must not warn: {:?}",
        codes[0].2
    );
}

/// A `pattern` is never reported, however unreferenced: patterns are the
/// emitting declarations, so nothing referencing them is the normal case.
#[test]
fn an_unreferenced_pattern_is_not_a_warning() {
    let output = compile_catalog(
        PatternSource::named("pat.lx", "a := [upos=NOUN].\nb := [upos=VERB].\n"),
        CompileLimits::default(),
    )
    .expect("compiles");
    assert!(
        output.diagnostics.is_empty(),
        "{:?}",
        output
            .diagnostics
            .iter()
            .map(lingrep_core::fsm::Diagnostic::code)
            .collect::<Vec<_>>()
    );
}

/// The warning renders through the ordinary renderer, with the `warning` label
/// the severity fix made possible.
#[test]
fn an_unused_fragment_warning_renders_as_a_warning() {
    let source = "fragment unused := [upos=NOUN].\np := [upos=VERB].\n";
    let output = compile_catalog(
        PatternSource::named("frag.lx", source),
        CompileLimits::default(),
    )
    .expect("compiles");

    let rendered = lingrep_core::fsm::render_diagnostics(Some(source), &output.diagnostics);
    assert!(
        rendered.contains("warning[UnusedFragment]"),
        "must not claim to be an error:\n{rendered}"
    );
    assert!(rendered.contains("--> frag.lx:1:1"), "{rendered}");
}
