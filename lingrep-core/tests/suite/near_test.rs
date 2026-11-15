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
//! Similarity tests: `[attr near WORD >= THRESHOLD]` and `vectors:`.

use std::cell::RefCell;
use std::sync::Arc;

use compact_str::CompactString;
use lingrep_core::fsm::{
    CompileError, CompileLimits, CompiledCatalog, DiagnosticCode, FsmBuilder, NearQuery,
    PatternSource, WordVectors, compile_catalog, compile_catalog_with_vectors,
    report_catalog_drift, report_catalog_drift_with_vectors,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

/// Similarities to `研究` only; every other word has no vector.
struct Table {
    asked: RefCell<Vec<Vec<NearQuery>>>,
}

const SIMILAR_TO_KENKYU: [(&str, u32); 4] = [
    ("研究", 1_000_000),
    ("調査", 700_000),
    ("分析", 600_000),
    ("料理", 200_000),
];

impl Table {
    fn new() -> Self {
        Self {
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl WordVectors for Table {
    fn similar_values(
        &self,
        queries: &[NearQuery],
    ) -> Result<Vec<Option<Arc<[CompactString]>>>, String> {
        self.asked.borrow_mut().push(queries.to_vec());
        Ok(queries
            .iter()
            .map(|query| {
                (query.word == "研究").then(|| {
                    SIMILAR_TO_KENKYU
                        .iter()
                        .filter(|(_, similarity)| *similarity >= query.threshold.millionths())
                        .map(|(value, _)| CompactString::from(*value))
                        .collect()
                })
            })
            .collect())
    }
}

struct Unreadable;

impl WordVectors for Unreadable {
    fn similar_values(&self, _: &[NearQuery]) -> Result<Vec<Option<Arc<[CompactString]>>>, String> {
        Err("cannot read vectors.vec".to_owned())
    }
}

fn with_table(source: &str) -> Result<CompiledCatalog, CompileError> {
    compile_catalog_with_vectors(
        PatternSource::named("near.lx", source),
        CompileLimits::default(),
        &Table::new(),
    )
    .map(|output| output.matcher)
}

fn tokens() -> Vec<TokenInfo> {
    ["研究", "調査", "分析", "料理", "未知"]
        .into_iter()
        .map(|lemma| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Lemma, lemma.into());
            token
        })
        .collect()
}

fn matched_positions(matcher: &CompiledCatalog) -> Vec<usize> {
    matcher
        .find_matches(&tokens())
        .iter()
        .map(|found| found.start)
        .collect()
}

fn code_of(error: &CompileError) -> DiagnosticCode {
    error.diagnostics.first().expect("a diagnostic").code()
}

#[test]
fn near_matches_the_values_at_or_above_the_threshold() {
    let at = |threshold: &str| {
        matched_positions(
            &with_table(&format!("pattern p := [lemma near 研究 >= {threshold}].\n")).unwrap(),
        )
    };
    assert_eq!(at("0.6"), [0, 1, 2]);
    assert_eq!(at("0.65"), [0, 1]);
    assert_eq!(at("1"), [0]);
    assert_eq!(at("0"), [0, 1, 2, 3]);
}

#[test]
fn near_is_the_inline_set_of_its_values() {
    let near =
        with_table("pattern p := [lemma near 研究 >= 0.6] [lemma near 研究 >= 0.6].\n").unwrap();
    let inline = compile_catalog(
        PatternSource::named(
            "near.lx",
            "pattern p := [lemma in {研究, 調査, 分析}] [lemma in {研究, 調査, 分析}].\n",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let found = near.find_matches(&tokens());
    assert!(!found.is_empty());
    assert_eq!(found, inline.find_matches(&tokens()));
}

#[test]
fn not_near_matches_present_values_outside_the_set() {
    let matcher = with_table("pattern p := [lemma not near 研究 >= 0.6].\n").unwrap();
    assert_eq!(matched_positions(&matcher), [3, 4]);
}

#[test]
fn a_table_is_asked_once_for_the_distinct_tests_of_a_catalog() {
    let table = Table::new();
    compile_catalog_with_vectors(
        PatternSource::named(
            "near.lx",
            concat!(
                "test T := lemma near 研究 >= 0.6.\n",
                "pattern p := [@T] [lemma near 研究 >= 0.6 & text not near 研究 >= 0.7].\n",
            ),
        ),
        CompileLimits::default(),
        &table,
    )
    .unwrap();
    let asked = table.asked.borrow();
    let [queries] = asked.as_slice() else {
        panic!("asked {} times", asked.len());
    };
    let thresholds: Vec<String> = queries
        .iter()
        .map(|query| query.threshold.to_string())
        .collect();
    assert_eq!(thresholds, ["0.6", "0.7"]);
}

#[test]
fn near_without_a_table_does_not_compile() {
    let error = compile_catalog(
        PatternSource::named("near.lx", "pattern p := [lemma near 研究 >= 0.6].\n"),
        CompileLimits::default(),
    )
    .err()
    .expect("no table was supplied");
    assert_eq!(code_of(&error), DiagnosticCode::WordVectorUnavailable);
    assert_eq!(error.diagnostics[0].location().column(), 15);
}

#[test]
fn a_word_without_a_vector_does_not_compile() {
    let error = with_table("pattern p := [lemma near 未知 >= 0.6].\n")
        .err()
        .expect("未知 has no vector");
    assert_eq!(code_of(&error), DiagnosticCode::WordVectorUnavailable);
    assert!(error.to_string().contains("未知"), "{error}");
}

#[test]
fn an_unreadable_table_is_reported_at_the_near_test_with_its_message() {
    let error = compile_catalog_with_vectors(
        PatternSource::named("near.lx", "pattern p := [lemma near 研究 >= 0.6].\n"),
        CompileLimits::default(),
        &Unreadable,
    )
    .err()
    .expect("the table cannot be read");
    assert_eq!(code_of(&error), DiagnosticCode::WordVectorTableUnreadable);
    assert_eq!(error.diagnostics[0].location().column(), 15);
    assert!(
        error.to_string().contains("cannot read vectors.vec"),
        "{error}"
    );
}

#[test]
fn a_threshold_outside_zero_to_one_is_a_syntax_error() {
    for threshold in ["1.5", "2", "1.0000001", "0.1234567"] {
        let error = with_table(&format!("pattern p := [lemma near 研究 >= {threshold}].\n"))
            .err()
            .unwrap_or_else(|| panic!("{threshold} is not a cosine with six decimals"));
        assert_eq!(code_of(&error), DiagnosticCode::Syntax, "{threshold}");
    }
}

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn vectors_entries_are_kept_in_source_order() {
    let matcher = with_table(&format!(
        concat!(
            "vectors: {{resource: \"ruri-v3-310m-unidic-cwj\", blake3: \"{0}\"}}\n",
            "vectors: {{blake3: \"{0}\", resource: \"cc-ja-300\"}}\n",
            "pattern p := [lemma near 研究 >= 0.6].\n",
        ),
        DIGEST
    ))
    .unwrap();
    let declared: Vec<(&str, &str)> = matcher
        .vectors()
        .iter()
        .map(|vectors| (vectors.resource.as_str(), vectors.blake3.as_str()))
        .collect();
    assert_eq!(
        declared,
        [("ruri-v3-310m-unidic-cwj", DIGEST), ("cc-ja-300", DIGEST)]
    );
}

#[test]
fn a_vectors_entry_requires_one_resource_and_one_digest_before_the_definitions() {
    let entries = [
        "pattern p := [lemma=x].\nvectors: {resource: \"a\", blake3: \"DIGEST\"}\n".to_owned(),
        "vectors: {resource: \"a\"}\npattern p := [lemma=x].\n".to_owned(),
        "vectors: {blake3: \"DIGEST\"}\npattern p := [lemma=x].\n".to_owned(),
        "vectors: {resource: \"a\", resource: \"b\", blake3: \"DIGEST\"}\npattern p := [lemma=x].\n"
            .to_owned(),
        "vectors: {resource: \"a\", blake3: \"0123\"}\npattern p := [lemma=x].\n".to_owned(),
    ];
    for entry in entries {
        let source = entry.replace("DIGEST", DIGEST);
        let error = with_table(&source)
            .err()
            .unwrap_or_else(|| panic!("compiled: {source}"));
        assert_eq!(code_of(&error), DiagnosticCode::Syntax, "{source}");
    }
}

#[test]
fn the_formatter_keeps_a_near_test_compilable() {
    let source = concat!(
        "vectors:   {resource:\"cc-ja-300\",blake3: \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"}\n",
        "pattern p := [lemma   near 研究   >=   0.6 & text not  near 研究 >= 1].\n",
    );
    let formatted = lingrep_core::fsm::format_pattern_source(
        PatternSource::named("near.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .source;
    assert_eq!(
        formatted,
        concat!(
            "vectors: {resource: \"cc-ja-300\", blake3: \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"}\n",
            "pattern p := [lemma near 研究 >= 0.6 & text not near 研究 >= 1].\n",
        )
    );
    with_table(&formatted).unwrap();
}

#[test]
fn the_values_of_a_near_test_are_charged_to_the_generated_bytes_budget() {
    let source = PatternSource::named("near.lx", "pattern p := [lemma near 研究 >= 0].\n");
    let inline = PatternSource::named("near.lx", "pattern p := [lemma in {研究}].\n");
    let limits = (1..4096)
        .map(|bytes| {
            CompileLimits::default()
                .tightened(|limits| limits.max_generated_owned_bytes = bytes)
                .unwrap()
        })
        .find(|limits| compile_catalog(inline, *limits).is_ok())
        .expect("a one-member list compiles within 4096 bytes");
    let error = compile_catalog_with_vectors(source, limits, &Table::new())
        .err()
        .expect("four members exceed a budget that admits one");
    assert_eq!(code_of(&error), DiagnosticCode::LimitExceeded);
}

#[test]
fn a_compiled_catalog_reports_each_distinct_near_test_with_its_values() {
    let matcher = with_table(
        "pattern p := [lemma near 研究 >= 0.65] [lemma near 研究 >= 0.65 | text near 研究 >= 1].\n",
    )
    .unwrap();
    let reported: Vec<(String, String, Vec<&str>)> = matcher
        .near_tests()
        .iter()
        .map(|test| {
            (
                test.word.to_string(),
                test.threshold.to_string(),
                test.values.iter().map(CompactString::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        reported,
        [
            ("研究".to_owned(), "0.65".to_owned(), vec!["研究", "調査"]),
            ("研究".to_owned(), "1".to_owned(), vec!["研究"]),
        ]
    );
}

#[test]
fn a_builder_compiles_near_tests_only_when_given_a_table() {
    let builder = || {
        let mut builder = FsmBuilder::new();
        builder
            .add_document(PatternSource::named(
                "near.lx",
                "pattern p := [lemma near 研究 >= 0.6].\n",
            ))
            .unwrap();
        builder
    };
    let matcher = builder().build_with_vectors(&Table::new()).unwrap().matcher;
    assert_eq!(matched_positions(&matcher), [0, 1, 2]);
    let error = builder().build().err().expect("no table was supplied");
    assert_eq!(code_of(&error), DiagnosticCode::WordVectorUnavailable);
}

#[test]
fn drift_compares_near_tests_by_the_values_they_resolve_to() {
    let sources = [
        PatternSource::named("a.lx", "pattern p := [lemma near 研究 >= 0.65].\n"),
        PatternSource::named("b.lx", "pattern p := [lemma near 研究 >= 0.7].\n"),
        PatternSource::named("c.lx", "pattern p := [lemma near 研究 >= 0.6].\n"),
    ];
    let table = Table::new();
    let same = report_catalog_drift_with_vectors(&sources[..2], Some(&table)).unwrap();
    assert!(same.entries.is_empty());
    let different = report_catalog_drift_with_vectors(&sources[1..], Some(&table)).unwrap();
    assert_eq!(different.entries.len(), 1);
    let error = report_catalog_drift(&sources).expect_err("no table");
    assert_eq!(code_of(&error), DiagnosticCode::WordVectorUnavailable);
}
