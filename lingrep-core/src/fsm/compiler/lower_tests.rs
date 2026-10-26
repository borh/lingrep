// Every span assertion below slices the fixture by a byte range the
// compiler reported. Test code is exempt from the panic deny-floor
// (docs/lint-policy.md), and a panic here is the test failing, which is
// what it is for.
#![allow(clippy::string_slice)]

use super::{lower_definition, lower_document, lower_document_with_stats, resolved_attr};
use crate::fsm::{AttrValue, AttrValueType};
use crate::token_match::AttributeKey;
use crate::token_match::TokenAttributeIndex;
use serde_json::json;
use std::collections::BTreeSet;

use super::super::PatternSource;
use super::super::diagnostic::{CompileError, DiagnosticCode};
use super::super::limits::CompileLimits;
use super::super::model::{
    CaptureProjection, LookDirection, LookPolarity, LookaroundExpr, ParsedAttrValue,
    ParsedDefinition, ParsedDefinitionBody, ParsedExpr, ParsedProgram, ParsedTest, SourceId,
    SourceSpan,
};

fn lower(source: &str) -> ParsedProgram {
    lower_document(
        &PatternSource::named("fixture.lx", source),
        &CompileLimits::default(),
    )
    .expect("fixture must lower")
}

/// The source text a span covers.
///
/// Every span assertion goes through this, so none of them can silently
/// degrade into "some two spans differ" — a value span, a whole-line span,
/// and the right span all satisfy `assert_ne!`; this helper asserts the
/// authored text itself.
fn spanned(source: &str, span: SourceSpan) -> &str {
    let range = span.byte_range();
    &source[range.start as usize..range.end as usize]
}

/// The byte range an error's first diagnostic points at.
fn error_range(error: &CompileError) -> std::ops::Range<usize> {
    error
        .diagnostics
        .first()
        .expect("an error carries at least one diagnostic")
        .location()
        .byte_range()
}

fn refuse(source: &str) -> CompileError {
    lower_document(
        &PatternSource::named("fixture.lx", source),
        &CompileLimits::default(),
    )
    .expect_err("source is refused")
}

#[test]
fn annotation_run_binds_to_the_following_definition_with_key_spans() {
    let source = "layer: \"S2\"\nexamples: [\"a\"]\nteiru := x.\n";
    let program = lower(source);
    assert_eq!(program.definitions.len(), 1);
    let metadata = &program.definitions[0].metadata;
    assert_eq!(metadata["layer"].value, json!("S2"));
    assert_eq!(metadata["examples"].value, json!(["a"]));
    assert_eq!(spanned(source, metadata["layer"].span), "layer");
    assert_eq!(spanned(source, metadata["examples"].span), "examples");
}

#[test]
fn a_dangling_annotation_run_reports_its_first_key() {
    let source = "layer: \"S2\"\ncategory: \"c\"\n";
    let error = refuse(source);
    assert!(
        error.to_string().contains("annotates no definition"),
        "message names the defect: {error}"
    );
    assert_eq!(
        &source[error_range(&error)],
        "layer",
        "the run's first key is where the author starts reading"
    );
}

#[test]
fn a_duplicate_key_in_one_run_reports_the_later_key() {
    let source = "layer: \"S1\"\nlayer: \"S2\"\np := x.\n";
    let error = refuse(source);
    assert!(
        error.to_string().contains("duplicate metadata key"),
        "{error}"
    );
    let range = error_range(&error);
    assert_eq!(&source[range.clone()], "layer");
    assert_eq!(
        range.start,
        source.find("layer: \"S2\"").expect("second entry present"),
        "the later of the two keys is reported, not the first"
    );
}

#[test]
fn a_second_defaults_block_is_refused() {
    let source = "defaults\n  a: \"1\"\nend\ndefaults\n  b: \"2\"\nend\np := x.\n";
    let error = refuse(source);
    assert!(
        error.to_string().contains("at most one `defaults` block"),
        "{error}"
    );
    assert_eq!(
        error_range(&error).start,
        source.find("defaults\n  b").expect("second block present"),
        "the second block is where the author has to look"
    );
}

#[test]
fn a_second_schema_block_is_refused() {
    let source = "schema\n  layer: string\nend\nschema\n  layer: string\nend\np := x.\n";
    let error = refuse(source);
    assert!(
        error.to_string().contains("at most one `schema` block"),
        "{error}"
    );
    assert_eq!(
        error_range(&error).start,
        source.rfind("schema").expect("two schema blocks"),
        "the second block is reported"
    );
}

#[test]
fn a_second_policy_block_is_refused() {
    const BLOCK: &str = "policy\n  resolve all rank: [longest, start, end, pattern].\nend\n";
    let source = format!("{BLOCK}{BLOCK}p := x.\n");
    let error = refuse(&source);
    assert!(
        error.to_string().contains("at most one `policy` block"),
        "{error}"
    );
    assert_eq!(error_range(&error).start, BLOCK.len(), "the second block");
}

#[test]
fn defaults_after_the_first_annotation_is_refused() {
    let source = "layer: \"S1\"\ndefaults\n  a: \"1\"\nend\np := x.\n";
    let error = refuse(source);
    assert!(
        error
            .to_string()
            .contains("a `defaults` block must precede the first annotation or definition"),
        "{error}"
    );
    assert_eq!(
        error_range(&error).start,
        source.find("defaults").expect("block present")
    );
}

#[test]
fn defaults_after_the_first_definition_is_refused() {
    let source = "p := x.\ndefaults\n  a: \"1\"\nend\n";
    let error = refuse(source);
    assert!(
        error
            .to_string()
            .contains("a `defaults` block must precede the first annotation or definition"),
        "{error}"
    );
}

#[test]
fn schema_after_the_first_definition_is_refused() {
    let source = "p := x.\nschema\n  layer: string\nend\n";
    let error = refuse(source);
    assert!(
        error
            .to_string()
            .contains("a `schema` block must precede the first annotation or definition"),
        "{error}"
    );
}

#[test]
fn an_analyzer_declaration_after_an_annotation_is_refused() {
    let source = "layer: \"S1\"\n\
                      analyzer: { family: \"vibrato\", resource: \"unidic-cwj\" }\n\
                      p := x.\n";
    let error = refuse(source);
    assert!(
        error
            .to_string()
            .contains("analyzer declarations must precede annotations and definitions"),
        "{error}"
    );
}

#[test]
fn an_annotation_run_before_a_non_pattern_declaration_is_refused() {
    // Every catalog-value declaration shares one refusal: a run can only
    // annotate the pattern or lexicon that follows it, so a declaration
    // that takes no metadata must not swallow the run.
    for declaration in [
        "set S := {a}.",
        "test t := text=a.",
        "group g := layer in {\"S1\"}.",
    ] {
        let source = format!("layer: \"S1\"\n{declaration}\np := x.\n");
        let error = refuse(&source);
        assert!(
            error
                .to_string()
                .contains("metadata annotates no definition"),
            "{declaration}: {error}"
        );
        assert_eq!(
            error_range(&error).start,
            0,
            "{declaration}: the run's first key is reported, not the declaration"
        );
    }
}

#[test]
fn a_string_value_must_be_quoted() {
    refuse("status: experimental\np := x.\n");
}

#[test]
fn there_is_no_null_metadata_value() {
    refuse("source: null\np := x.\n");
}

#[test]
fn comments_and_blank_lines_do_not_break_an_annotation_run() {
    let source = "layer: \"S2\"\n# why this layer\n\ncategory: \"S2d\"\np := x.\n";
    let program = lower(source);
    assert_eq!(program.definitions.len(), 1);
    let metadata = &program.definitions[0].metadata;
    assert_eq!(metadata["layer"].value, json!("S2"));
    assert_eq!(
        metadata["category"].value,
        json!("S2d"),
        "a comment between two annotations does not start a new run"
    );
}

#[test]
fn typed_values_round_trip_through_the_walker() {
    let source = "depth: 2\nratio: -1.5e2\nkeep: true\ndrop: false\n\
                      tags: [\"a\", \"b\"]\nanchor: {name: \"end\", nested: {depth: 0}}\np := x.\n";
    let program = lower(source);
    let metadata = &program.definitions[0].metadata;
    assert_eq!(metadata["depth"].value, json!(2));
    assert_eq!(metadata["ratio"].value, json!(-150.0));
    assert_eq!(metadata["keep"].value, json!(true));
    assert_eq!(metadata["drop"].value, json!(false));
    assert_eq!(metadata["tags"].value, json!(["a", "b"]));
    assert_eq!(
        metadata["anchor"].value,
        json!({"name": "end", "nested": {"depth": 0}})
    );
}

#[test]
fn a_string_value_resolves_json_escapes() {
    let program = lower("note: \"a\\\"b\\nc\\u3042\"\np := x.\n");
    assert_eq!(
        program.definitions[0].metadata["note"].value,
        json!("a\"b\nc\u{3042}"),
        "escape handling is serde_json's, not a second implementation of it"
    );
}

#[test]
fn a_default_is_materialized_into_every_definition() {
    let source = "defaults\n  status: \"experimental\"\nend\n\np := x.\nq := y.\n";
    let program = lower(source);
    for definition in &program.definitions {
        assert_eq!(
            definition.metadata["status"].value,
            json!("experimental"),
            "{} carries the materialized default",
            definition.name
        );
    }
}

#[test]
fn a_definition_annotation_overrides_a_default() {
    let source = "defaults\n  role: \"research\"\nend\n\nrole: \"helper\"\np := x.\nq := y.\n";
    let program = lower(source);
    let by_name = |name: &str| {
        program
            .definitions
            .iter()
            .find(|definition| definition.name == name)
            .expect("definition present")
    };
    assert_eq!(by_name("p").metadata["role"].value, json!("helper"));
    assert_eq!(
        by_name("q").metadata["role"].value,
        json!("research"),
        "an override binds to one definition, not to the rest of the file"
    );
}

#[test]
fn a_defaulted_key_carries_the_defaults_entry_span() {
    let source = "defaults\n  status: \"experimental\"\nend\n\np := x.\n";
    let program = lower(source);
    let span = program.definitions[0].metadata["status"].span;
    assert_eq!(spanned(source, span), "status");
    assert!(
        (span.start() as usize) < source.find("p :=").expect("definition present"),
        "a defaulted key points at the default, where the author would fix it"
    );
}

#[test]
fn a_second_defaults_block_is_an_error() {
    let error = refuse("defaults\n  a: \"1\"\nend\ndefaults\n  b: \"2\"\nend\np := x.\n");
    assert!(
        error.to_string().contains("one `defaults` block"),
        "{error}"
    );
}

#[test]
fn a_defaults_block_after_a_definition_is_an_error() {
    let error = refuse("p := x.\ndefaults\n  a: \"1\"\nend\n");
    assert!(
        error.to_string().contains("must precede"),
        "message says where it belongs: {error}"
    );
}

#[test]
fn a_duplicate_key_within_defaults_reports_the_later_key() {
    let source = "defaults\n  alpha: \"1\"\n  alpha: \"2\"\nend\np := x.\n";
    let error = refuse(source);
    let range = error_range(&error);
    assert_eq!(&source[range.clone()], "alpha");
    assert_eq!(
        range.start,
        source.find("alpha: \"2\"").expect("second entry present"),
        "the later of the two keys is reported"
    );
}

#[test]
fn charges_a_defaults_entry_source_bytes_once() {
    const DEFINITION_COUNT: usize = 128;
    let payload = "x".repeat(8 * 1024);
    let entry = format!("  payload: \"{payload}\"");
    let mut source = format!("defaults\n{entry}\nend\n\n");
    for index in 0..DEFINITION_COUNT {
        source.push_str(&format!("p{index:03} := a.\n"));
    }
    // Assert the exact charge. A test that only survives a tight limit also
    // passes when the accounting is missing entirely, because charging zero
    // survives every limit.
    let lowered = lower_document_with_stats(
        &PatternSource::named("t.lx", &source),
        &CompileLimits::default(),
    )
    .expect("document lowers");
    assert_eq!(
        lowered.metadata_bytes,
        entry.trim_start().len(),
        "one authored entry is charged once — not once per materialized copy, \
             and not zero; leading indentation is `extras` and is not charged"
    );
}

#[test]
fn refuses_a_defaults_entry_one_byte_over_the_metadata_budget() {
    let payload = "x".repeat(8 * 1024);
    let entry = format!("  payload: \"{payload}\"");
    let source = format!("defaults\n{entry}\nend\n\np := a.\n");
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_metadata_bytes = entry.trim_start().len() - 1)
        .expect("metadata limit is tightened below the authored entry");
    lower_document(&PatternSource::named("t.lx", &source), &limits)
        .expect_err("the budget binds the authored entry");
}

#[test]
fn materialized_defaults_are_charged_as_owned_memory_per_pattern() {
    const DEFINITION_COUNT: usize = 64;
    let payload = "x".repeat(512);
    let mut with_default = format!("defaults\n  note: \"{payload}\"\nend\n\n");
    let mut without_default = String::new();
    for index in 0..DEFINITION_COUNT {
        with_default.push_str(&format!("p{index:03} := a.\n"));
        without_default.push_str(&format!("p{index:03} := a.\n"));
    }
    let charged = |source: &str| {
        super::super::compile_catalog(
            PatternSource::named("t.lx", source),
            CompileLimits::default(),
        )
        .expect("document compiles")
        .stats
        .generated_owned_bytes()
    };
    let extra = charged(&with_default) - charged(&without_default);
    assert!(
        extra >= payload.len() * DEFINITION_COUNT,
        "each materialized copy is real owned memory and must be counted; \
             charging it once would make the budget lie about the artifact"
    );
}

#[test]
fn the_public_pattern_metadata_carries_values_not_entries() {
    let output = super::super::compile_catalog(
        PatternSource::named("t.lx", "layer: \"S2\"\np := a.\n"),
        CompileLimits::default(),
    )
    .expect("document compiles");
    // The public contract is HashMap<CompactString, serde_json::Value>;
    // spans stay behind the compiler boundary.
    assert_eq!(
        output.matcher.patterns()["p"].metadata["layer"],
        json!("S2")
    );
}

#[test]
fn missing_definition_terminator_names_the_dot_exactly() {
    let error = lower_document(
        &PatternSource::named("fixture.lx", "inline := [text=を]"),
        &CompileLimits::default(),
    )
    .expect_err("missing terminator must fail");
    let d = &error.diagnostics[0];
    assert_eq!(d.code(), DiagnosticCode::Syntax);
    assert_eq!(d.location().byte_range(), 20..20);
    assert_eq!(d.location().line(), 1);
    assert_eq!(d.location().column(), 21);
    assert_eq!(d.message, "missing `.`");
}

#[test]
fn error_node_recovery_uses_generic_message_not_expected_error() {
    let error = lower_document(
        &PatternSource::named("fixture.lx", "inline := [text=]"),
        &CompileLimits::default(),
    )
    .expect_err("empty attribute value must fail");
    let d = &error.diagnostics[0];
    assert_eq!(d.code(), DiagnosticCode::Syntax);
    assert!(
        d.message == "invalid lingrep patterns syntax",
        "must not be 'expected ERROR'; got: {}",
        d.message
    );
    assert!(!d.message.contains("ERROR"), "got: {}", d.message);
}

#[test]
fn bare_cr_compilation_behavior_is_unchanged_by_parser_view_extraction() {
    let source = PatternSource::named("bare-cr.lx", "x := [text~/a\rb/].");
    assert!(lower_document(&source, &CompileLimits::default()).is_ok());
}

fn only_definition(source: &str) -> ParsedDefinition {
    let mut definitions = lower(source).definitions;
    assert_eq!(definitions.len(), 1, "fixture must define one pattern");
    definitions.remove(0)
}

fn only_expr(source: &str) -> ParsedExpr {
    match only_definition(source).body {
        ParsedDefinitionBody::Pattern { expr, .. } => expr,
        ParsedDefinitionBody::Lexicon { .. } => panic!("fixture must define a pattern"),
    }
}

fn token(text: &str) -> ParsedExpr {
    ParsedExpr::Token(ParsedTest::Attr(resolved_attr(AttrValue {
        attr: TokenAttributeIndex::Orth,
        value: AttrValueType::Single(text.into()),
        complement: false,
    })))
}

/// The parsed test a bracketed conjunction of `attributes` lowers to.
///
/// One attribute is [`ParsedTest::Attr`] and several are
/// [`ParsedTest::And`], because that is what the grammar produces: a lone
/// attribute is an `attribute` node, and `&` is what introduces an
/// `attribute_conjunction`. Written out here so a fixture below does not
/// have to remember which.
fn conjunction(attributes: Vec<super::super::model::ParsedAttr>) -> ParsedTest {
    match <[_; 1]>::try_from(attributes) {
        Ok([only]) => ParsedTest::Attr(only),
        Err(several) => ParsedTest::And(several.into_iter().map(ParsedTest::Attr).collect()),
    }
}

fn assert_diagnostic(
    source: &str,
    limits: &CompileLimits,
    code: DiagnosticCode,
    byte_range: std::ops::Range<usize>,
) {
    let error = lower_document(&PatternSource::named("fixture.lx", source), limits)
        .expect_err("fixture must be rejected");
    assert_eq!(error.diagnostics.len(), 1);
    assert_eq!(error.diagnostics[0].code(), code);
    assert_eq!(error.diagnostics[0].location().byte_range(), byte_range);
}

#[test]
fn lowers_each_core_expression_shape_without_fallbacks() {
    let regex = regex::Regex::new(r"^(食べる|見る)$").expect("test regex is valid");
    let cases = [
        ("literal", "p := 猫.", token("猫")),
        (
            "three-way attribute conjunction",
            "p := [upos=VERB & unidic.cform=終止形 & lemma~/^(食べる|見る)$/].",
            ParsedExpr::Token(conjunction(vec![
                resolved_attr(AttrValue {
                    attr: TokenAttributeIndex::Pos,
                    value: AttrValueType::Single("VERB".into()),
                    complement: false,
                }),
                resolved_attr(AttrValue {
                    attr: TokenAttributeIndex::CForm,
                    value: AttrValueType::Single("終止形".into()),
                    complement: false,
                }),
                resolved_attr(AttrValue {
                    attr: TokenAttributeIndex::Lemma,
                    value: AttrValueType::Regex(regex),
                    complement: false,
                }),
            ])),
        ),
        (
            "sequence",
            "p := 猫 が いる.",
            ParsedExpr::Sequence(vec![token("猫"), token("が"), token("いる")]),
        ),
        (
            "alternation precedence",
            "p := a b | c d.",
            ParsedExpr::Alternation(vec![
                ParsedExpr::Sequence(vec![token("a"), token("b")]),
                ParsedExpr::Sequence(vec![token("c"), token("d")]),
            ]),
        ),
    ];

    for (label, source, expected) in cases {
        assert_eq!(only_expr(source), expected, "case: {label}");
    }
}

#[test]
fn classifies_only_complete_slash_delimited_leaf_values_as_regex() {
    assert_eq!(
        only_expr("p := [text~/a+/]."),
        ParsedExpr::Token(conjunction(vec![resolved_attr(AttrValue {
            attr: TokenAttributeIndex::Orth,
            value: AttrValueType::Regex(regex::Regex::new("a+").expect("test regex is valid"),),
            complement: false,
        })]))
    );
    assert_eq!(
        only_expr("p := [text=and/or]."),
        ParsedExpr::Token(conjunction(vec![resolved_attr(AttrValue {
            attr: TokenAttributeIndex::Orth,
            value: AttrValueType::Single("and/or".into()),
            complement: false,
        })]))
    );
}

#[test]
fn lowers_every_pinned_quantifier_form_to_checked_bounds() {
    let actual = only_expr("p := a? b* c+ d{2} e{3,} f{4,5}.");
    let repeat = |text, min, max| ParsedExpr::Repeat {
        expr: Box::new(token(text)),
        min,
        max,
    };

    assert_eq!(
        actual,
        ParsedExpr::Sequence(vec![
            repeat("a", 0, Some(1)),
            repeat("b", 0, None),
            repeat("c", 1, None),
            repeat("d", 2, Some(2)),
            repeat("e", 3, None),
            repeat("f", 4, Some(5)),
        ])
    );
}

#[test]
fn preserves_references_parentheses_named_groups_and_unicode_names() {
    let source = "日本語 := (?<命題> (@参照 | 猫)).";
    let reference_start = source.find("@参照").expect("fixture has a reference");
    let capture_start = source.find("(?<命題>").expect("fixture has a capture");
    let definition = only_definition(source);

    assert_eq!(definition.name.as_str(), "日本語");
    assert_eq!(definition.span.byte_range(), 0..41);
    assert_eq!(
        definition.expr().expect("pattern"),
        &ParsedExpr::NamedGroup {
            name: "命題".into(),
            expr: Box::new(ParsedExpr::Group(Box::new(ParsedExpr::Alternation(vec![
                ParsedExpr::Reference {
                    name: "参照".into(),
                    span: SourceSpan::new(
                        SourceId::new(0),
                        reference_start..reference_start + "@参照".len(),
                    )
                    .unwrap(),
                },
                token("猫")
            ],)))),
            span: SourceSpan::new(
                SourceId::new(0),
                capture_start..capture_start + "(?<命題> (@参照 | 猫))".len(),
            )
            .unwrap(),
        }
    );
}

#[test]
fn lowers_capture_spans_and_span_equality_projection() {
    let source = "pattern p := (?<unit> [text=a]{1,3}) same_as($unit, by=lemma).";
    let ParsedExpr::Sequence(expressions) = only_expr(source) else {
        panic!("fixture must lower to a sequence");
    };
    assert_eq!(expressions.len(), 2);

    let group_start = source.find("(?<unit>").expect("fixture has capture");
    match &expressions[0] {
        ParsedExpr::NamedGroup { name, span, .. } => {
            assert_eq!(name, "unit");
            assert_eq!(
                span.byte_range(),
                u32::try_from(group_start).unwrap()
                    ..u32::try_from(group_start + "(?<unit> [text=a]{1,3})".len()).unwrap()
            );
        }
        other => panic!("expected named group, got {other:?}"),
    }

    let same_as_start = source.find("same_as").expect("fixture has same_as");
    assert_eq!(
        expressions[1],
        ParsedExpr::SameAs {
            name: "unit".into(),
            projection: CaptureProjection(TokenAttributeIndex::Lemma.into()),
            span: SourceSpan::new(
                SourceId::new(0),
                same_as_start..same_as_start + "same_as($unit, by=lemma)".len(),
            )
            .expect("fixture span is valid"),
        }
    );
}

#[test]
fn same_as_remains_available_as_an_attribute_value() {
    assert_eq!(only_expr("pattern p := [text=same_as]."), token("same_as"));
}

#[test]
fn lowers_all_regular_lookaround_forms_with_direction_polarity_body_and_span() {
    let cases = [
        (
            "preceded_by",
            LookDirection::PrecededBy,
            LookPolarity::Positive,
        ),
        (
            "not_preceded_by",
            LookDirection::PrecededBy,
            LookPolarity::Negative,
        ),
        (
            "followed_by",
            LookDirection::FollowedBy,
            LookPolarity::Positive,
        ),
        (
            "not_followed_by",
            LookDirection::FollowedBy,
            LookPolarity::Negative,
        ),
    ];

    for (construct, direction, polarity) in cases {
        let source = format!("pattern p := {construct}([text=a] [text=b]*) [text=x].");
        let start = source.find(construct).expect("fixture contains construct");
        let end = source
            .find(") [text=x]")
            .expect("fixture contains closing paren")
            + 1;
        let ParsedExpr::Sequence(mut expressions) = only_expr(&source) else {
            panic!("{construct} fixture must lower to a sequence");
        };
        let lookaround = expressions.remove(0);

        assert_eq!(
            lookaround,
            ParsedExpr::Lookaround(LookaroundExpr {
                direction,
                polarity,
                body: Box::new(ParsedExpr::Sequence(vec![
                    token("a"),
                    ParsedExpr::Repeat {
                        expr: Box::new(token("b")),
                        min: 0,
                        max: None,
                    },
                ])),
                span: SourceSpan::new(SourceId::new(0), start..end).expect("fixture span is valid"),
            }),
            "construct: {construct}",
        );
    }
}

#[test]
fn rejects_unknown_capture_projection_at_the_projection_name() {
    let source = "pattern p := (?<unit> a) same_as($unit, by=surface).";
    let start = source.find("surface").expect("fixture has projection");
    assert_diagnostic(
        source,
        &CompileLimits::default(),
        DiagnosticCode::Syntax,
        start - 5..start + "surface".len(),
    );
}

#[test]
fn preserves_typed_metadata_for_every_annotated_definition() {
    let source = r#"# leading comment
examples: ["a", "考える"]
status: "FIXME"
count: 2
enabled: true
一 := 猫.
examples: ["a", "考える"]
status: "FIXME"
count: 2
enabled: true
二 := 犬. # trailing comment
外 := 鳥."#;
    let program = lower(source);

    assert_eq!(
        program
            .definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>(),
        ["一", "二", "外"]
    );
    let expected = [
        ("examples", json!(["a", "考える"])),
        ("status", json!("FIXME")),
        ("count", json!(2)),
        ("enabled", json!(true)),
    ];
    for definition in &program.definitions[..2] {
        for (key, value) in &expected {
            assert_eq!(&definition.metadata[*key].value, value);
        }
    }
    assert!(program.definitions[2].metadata.is_empty());
}

#[test]
fn comment_only_source_may_describe_definition_syntax() {
    let program = lower("# Example syntax: name := token.");

    assert!(program.definitions.is_empty());
}

#[test]
fn rejects_recovered_cst_unknown_nodes_attributes_and_non_rust_regex() {
    let cases = [
        // The unterminated bracket is a MISSING `]` rather than an ERROR
        // subtree, and the span is the zero-width position the token
        // belongs at. That is a property of the grammar's `value` token,
        // which excludes `.` — so `a` closes the value, `.` closes the
        // definition, and the only thing absent is the bracket. It read as
        // one ERROR spanning the whole definition while `.` was a legal
        // value character.
        (
            "MISSING bracket",
            "p := [text=a.",
            DiagnosticCode::Syntax,
            12..12,
        ),
        (
            "MISSING node",
            "p := [text=].",
            DiagnosticCode::Syntax,
            11..11,
        ),
        (
            "standalone expression",
            "[text=a]",
            DiagnosticCode::Syntax,
            0..8,
        ),
        (
            "unknown token attribute",
            "p := [unknown=a].",
            DiagnosticCode::Syntax,
            6..15,
        ),
        (
            "unsupported negative lookahead",
            "p := [text~/^(?!a)$/].",
            DiagnosticCode::InvalidRegex,
            11..20,
        ),
    ];

    for (label, source, code, byte_range) in cases {
        let error = lower_document(
            &PatternSource::named("fixture.lx", source),
            &CompileLimits::default(),
        )
        .expect_err(label);
        assert_eq!(error.diagnostics.len(), 1, "case: {label}");
        assert_eq!(error.diagnostics[0].code(), code, "case: {label}");
        assert_eq!(
            error.diagnostics[0].location().byte_range(),
            byte_range,
            "case: {label}"
        );
    }
}

#[test]
fn malformed_attribute_conjunction_is_syntax_at_recovered_token() {
    assert_diagnostic(
        "p := [text=a & & lemma=b].",
        &CompileLimits::default(),
        DiagnosticCode::Syntax,
        13..14,
    );
}

#[test]
fn rejects_invalid_or_overflowing_quantifier_bounds() {
    for source in ["p := a{5,2}.", "p := a{999999999999999999999999999999}."] {
        let quantifier_start = source.find('{').expect("fixture has a quantifier");
        assert_diagnostic(
            source,
            &CompileLimits::default(),
            DiagnosticCode::InvalidQuantifier,
            quantifier_start..source.find('}').expect("fixture closes") + 1,
        );
    }
}

#[test]
fn enforces_all_limits_owned_by_cst_lowering() {
    let source = "p := a.";
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_source_bytes = source.len() - 1)
        .expect("source limit is tightened");
    assert_diagnostic(
        source,
        &limits,
        DiagnosticCode::LimitExceeded,
        0..source.len(),
    );

    let source = "p := a.\nq := b.";
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_definitions = 1)
        .expect("definition limit is tightened");
    let second = source
        .find("q :=")
        .expect("fixture has a second definition");
    assert_diagnostic(
        source,
        &limits,
        DiagnosticCode::LimitExceeded,
        second..source.len(),
    );

    let source = "p := a b.";
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_expression_nodes = 2)
        .expect("expression limit is tightened");
    let b = source.find("b.").expect("fixture has a second token");
    assert_diagnostic(source, &limits, DiagnosticCode::LimitExceeded, b..b + 1);

    let source = "p := ((a)).";
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_nesting_depth = 2)
        .expect("depth limit is tightened");
    let a = source.find('a').expect("fixture has a nested token");
    assert_diagnostic(source, &limits, DiagnosticCode::LimitExceeded, a..a + 1);

    let source = "category: \"test\"\np := a.\n";
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_metadata_bytes = 0)
        .expect("metadata limit is tightened");
    let line_start = source.find("category").expect("fixture has metadata");
    // `line_start` is a byte offset from `find` of an ASCII substring, so
    // it is a char boundary.
    #[expect(
        clippy::string_slice,
        reason = "`line_start` is an ASCII-substring `find` offset, a char boundary"
    )]
    let line_end = source[line_start..].find('\n').expect("line ends") + line_start;
    assert_diagnostic(
        source,
        &limits,
        DiagnosticCode::LimitExceeded,
        line_start..line_end,
    );

    let source = "p := [text~/a+/].";
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_regex_bytes = 1)
        .expect("regex limit is tightened");
    let regex_start = source.find('/').expect("fixture has regex");
    let regex_end = source.rfind('/').expect("fixture closes regex") + 1;
    assert_diagnostic(
        source,
        &limits,
        DiagnosticCode::LimitExceeded,
        regex_start..regex_end,
    );
}

#[test]
fn deep_malformed_nesting_hits_budgets_before_recursive_recovery() {
    let nesting = 128;
    let source = format!(
        "p := {}[text=]{}.",
        "(".repeat(nesting),
        ")".repeat(nesting)
    );
    let limits = [
        CompileLimits::default()
            .tightened(|limits| limits.max_nesting_depth = 8)
            .expect("depth limit is tightened"),
        CompileLimits::default()
            .tightened(|limits| limits.max_expression_nodes = 8)
            .expect("node limit is tightened"),
    ];

    for limits in limits {
        let result = std::panic::catch_unwind(|| {
            lower_document(&PatternSource::named("deep.lx", &source), &limits)
        });
        let error = result
            .expect("deep malformed lowering must not panic")
            .expect_err("deep malformed lowering must be rejected");

        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::LimitExceeded);
    }
}

#[test]
fn lower_definition_accepts_exactly_one_definition() {
    let source = PatternSource::named("one.lx", "p := a.");
    assert_eq!(
        lower_definition(&source, &CompileLimits::default())
            .expect("one definition is accepted")
            .name
            .as_str(),
        "p"
    );

    let source = PatternSource::named("two.lx", "p := a.\nq := b.");
    let error = lower_definition(&source, &CompileLimits::default())
        .expect_err("multiple definitions are rejected");
    assert_eq!(error.diagnostics[0].code(), DiagnosticCode::Syntax);
}

#[test]
fn lowers_real_document_to_exact_definition_catalog_and_metadata() {
    let source = include_str!("../../../../patterns/sfe.lx");
    let program = lower_document(
        &PatternSource::named("patterns/sfe.lx", source),
        &CompileLimits::default(),
    )
    .expect("the coordinated real document must lower completely");

    assert_eq!(
        program
            .definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>(),
        [
            "動詞終止形",
            "形容詞終止形",
            "助動詞終止形",
            "用言終止形",
            "終助詞",
            "動詞連用形",
            "形容詞連用形",
            "形容詞語幹",
            "用言連用形",
            "形状詞",
            "形容動詞",
            "ARU",
            "KAKO",
            "NAI",
            "だ",
            "である",
            "たい",
            "なさい",
            "のに",
            "必要がある",
            "恐れがある",
            "だろう",
            "であろう",
            "う",
            "よう",
            "ようだ",
            "かもしれない",
        ]
    );

    let definition = |name: &str| {
        program
            .definitions
            .iter()
            .find(|definition| definition.name == name)
            .expect("asserted definition exists")
    };
    // The retained catalog puts its executable claims on an emitting
    // pattern, rather than on a helper fragment.
    assert_eq!(
        definition("必要がある").metadata["examples"].value,
        json!(["必要があった", "必要がある"])
    );
    assert_eq!(
        definition("必要がある").metadata["counterexamples"].value,
        json!(["必要があろう", "必要があれば"])
    );
    assert!(!definition("用言連用形").metadata.contains_key("examples"));
    assert_eq!(
        definition("恐れがある").metadata["category"].value,
        "がある"
    );
    assert_eq!(definition("だろう").metadata["subcategory"].value, "う");
    assert_eq!(definition("よう").metadata["subcategory"].value, "よう");
    assert_eq!(
        definition("かもしれない").metadata["category"].value,
        "かもしれない"
    );
    assert_eq!(
        definition("う").expr().expect("pattern"),
        &ParsedExpr::Token(conjunction(vec![
            resolved_attr(AttrValue {
                attr: TokenAttributeIndex::CForm,
                value: AttrValueType::Single("意志推量形".into()),
                complement: false,
            }),
            resolved_attr(AttrValue {
                attr: TokenAttributeIndex::Orth,
                value: AttrValueType::Single("う".into()),
                complement: false,
            }),
        ]))
    );

    let mut attributes = BTreeSet::new();
    for definition in &program.definitions {
        collect_attributes(definition.expr().expect("pattern"), &mut attributes);
    }
    assert_eq!(
        attributes,
        [
            TokenAttributeIndex::CForm,
            TokenAttributeIndex::Lemma,
            TokenAttributeIndex::Orth,
            TokenAttributeIndex::Pos,
            TokenAttributeIndex::Pos1,
            TokenAttributeIndex::Pos2,
            TokenAttributeIndex::Pos3,
        ]
        .into_iter()
        .map(AttributeKey::from)
        .collect()
    );
}

fn collect_attributes(expr: &ParsedExpr, attributes: &mut BTreeSet<AttributeKey>) {
    match expr {
        ParsedExpr::Token(test) => collect_test_attributes(test, attributes),
        ParsedExpr::Sequence(exprs) | ParsedExpr::Alternation(exprs) => {
            for expr in exprs {
                collect_attributes(expr, attributes);
            }
        }
        ParsedExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_attributes(operand, attributes);
            }
        }
        ParsedExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_attributes(operand, attributes);
            }
        }
        ParsedExpr::Repeat { expr, .. }
        | ParsedExpr::Group(expr)
        | ParsedExpr::NamedGroup { expr, .. } => collect_attributes(expr, attributes),
        ParsedExpr::Lookaround(lookaround) => collect_attributes(&lookaround.body, attributes),
        ParsedExpr::Assertion(assertion) => {
            if let crate::fsm::PositionAssertion::PreviousToken(predicate) = assertion {
                attributes.extend(predicate.iter().map(|attribute| attribute.attr.clone()));
            }
        }
        ParsedExpr::SameAs { .. } | ParsedExpr::Reference { .. } => {}
    }
}

/// Compile `source`, expect failure, return the first diagnostic's
/// (message, byte_range, line, column).
fn first_recovery_diagnostic(source: &str) -> (String, std::ops::Range<usize>, usize, usize) {
    let error = lower_document(
        &PatternSource::named("fixture.lx", source),
        &CompileLimits::default(),
    )
    .expect_err("fixture must fail to lower");
    let d = &error.diagnostics[0];
    assert_eq!(d.code(), DiagnosticCode::Syntax);
    (
        d.message.clone(),
        d.location().byte_range(),
        d.location().line(),
        d.location().column(),
    )
}

#[test]
fn unterminated_definition_points_at_the_dot_not_the_next_definition() {
    // Byte 13 is the end of `[text=x]`. The `:=` tree-sitter actually errors
    // on is on line 2; before this fix the diagnostic pointed there and said
    // "invalid lingrep patterns syntax".
    assert_eq!(
        first_recovery_diagnostic("a := [text=x]\nb := [text=y].\n"),
        ("missing `.`".to_string(), 13..13, 1, 14)
    );
}

#[test]
fn unterminated_definition_keeps_a_trailing_reference_as_the_final_term() {
    // `ref` is a legitimate pattern reference ending definition `a`. Only
    // `b` is the absorbed header, so the terminator belongs after `ref`.
    assert_eq!(
        first_recovery_diagnostic("a := ref\nb := [text=y].\n"),
        ("missing `.`".to_string(), 8..8, 1, 9)
    );
}

#[test]
fn unterminated_definition_consumes_a_fragment_or_pattern_keyword_header() {
    // Both kind keywords, since the recognizer matches on the literal text.
    // `pattern a := ` is 13 bytes and `fragment a := ` is 14, so `[text=x]`
    // ends at 21 and 22 respectively.
    for (keyword, offset) in [("pattern", 21_usize), ("fragment", 22_usize)] {
        let source = format!("{keyword} a := [text=x]\n{keyword} b := [text=y].\n");
        assert_eq!(
            first_recovery_diagnostic(&source),
            ("missing `.`".to_string(), offset..offset, 1, offset + 1),
            "keyword: {keyword}"
        );
    }
}

#[test]
fn unterminated_definition_places_the_dot_before_a_trailing_comment() {
    // `#...` runs to end of line, so a `.` suggested after the comment would
    // be swallowed by it.
    assert_eq!(
        first_recovery_diagnostic("a := [text=x] # note\nb := [text=y].\n"),
        ("missing `.`".to_string(), 13..13, 1, 14)
    );
}

#[test]
fn a_nested_group_never_reports_a_missing_terminator() {
    // Same local shape, but inside a parenthesized_pattern. A definition
    // cannot begin there, so `missing `.`` would be a confident lie.
    let (message, ..) = first_recovery_diagnostic("a := ([text=x] b := [text=y]).\n");
    assert_eq!(message, "invalid lingrep patterns syntax");
}

#[test]
fn an_alternation_branch_never_reports_a_missing_terminator() {
    let (message, ..) = first_recovery_diagnostic("a := [text=x] | [text=z]\nb := [text=y].\n");
    assert_eq!(message, "invalid lingrep patterns syntax");
}

#[test]
fn unterminated_definition_falls_back_when_no_term_precedes_the_reference() {
    // `a := b := [text=x].` is all on one line and has no term before the
    // absorbed `b`. There is nowhere honest to put the caret.
    let (message, ..) = first_recovery_diagnostic("a := b := [text=x].\n");
    assert_eq!(message, "invalid lingrep patterns syntax");
}

#[test]
fn a_genuine_reference_before_an_unrelated_error_is_not_called_a_missing_dot() {
    // The ERROR here comes from the empty attribute value, not from a `:=`.
    let (message, ..) = first_recovery_diagnostic("a := ref [text=].\n");
    assert_eq!(message, "invalid lingrep patterns syntax");
}

/// Every attribute one token test names, wherever `&`, `|` and `!` put it.
///
/// A `Reference` contributes nothing: the test it names is a declaration of
/// its own, and this walker is over one definition's expression.
fn collect_test_attributes(test: &ParsedTest, attributes: &mut BTreeSet<AttributeKey>) {
    match test {
        ParsedTest::Attr(attribute) => {
            attributes.insert(attribute.attr.clone());
        }
        ParsedTest::Reference { .. } => {}
        ParsedTest::And(operands) | ParsedTest::Or(operands) => {
            for operand in operands {
                collect_test_attributes(operand, attributes);
            }
        }
        ParsedTest::Not(inner) => collect_test_attributes(inner, attributes),
    }
}

/// The single literal value of a one-token, one-attribute definition.
///
/// Panics rather than returning an Option: every fixture below is written
/// to have exactly this shape, so any other shape is a broken test, not a
/// case to handle.
fn single_literal(source: &str) -> String {
    let program = lower(source);
    let [definition] = &program.definitions[..] else {
        panic!("fixture must have exactly one definition");
    };
    let ParsedExpr::Token(ParsedTest::Attr(attribute)) = definition.expr().expect("pattern") else {
        panic!("fixture must lower to a single token predicate");
    };
    let ParsedAttrValue::Resolved(AttrValueType::Single(value)) = &attribute.value else {
        panic!("fixture must have a single literal value");
    };
    value.to_string()
}

fn lower_error_code(source: &str) -> DiagnosticCode {
    lower_document(
        &PatternSource::named("fixture.lx", source),
        &CompileLimits::default(),
    )
    .expect_err("fixture must fail to lower")
    .diagnostics[0]
        .code()
}

#[test]
fn a_unicode_escape_denotes_the_character_it_names() {
    // U+3000 is invisible in a diff, in a review and in a terminal. A
    // literal that depends on one is a literal nobody can proofread, and one
    // was in fact silently dropped during an edit of this very catalog. The
    // escape gives the same value a spelling that survives being looked at.
    assert_eq!(single_literal("p := [text=\\u3000]."), "\u{3000}");
    assert_eq!(single_literal("p := [text=\\u3042]."), "あ");
    // Mixed with ordinary text, and more than once.
    assert_eq!(
        single_literal("p := [text=a\\u3000b\\u3000c]."),
        "a\u{3000}b\u{3000}c"
    );
}

#[test]
fn a_backslash_escape_denotes_exactly_one_backslash() {
    assert_eq!(single_literal("p := [text=\\\\]."), "\\");
    assert_eq!(single_literal("p := [text=a\\\\b]."), "a\\b");
}

#[test]
fn a_literal_without_a_backslash_is_unchanged() {
    // The fast path. Every existing catalog goes through it, so if this
    // moves, every fingerprint in the repository moves with it.
    assert_eq!(single_literal("p := [text=を]."), "を");
    assert_eq!(single_literal("p := [text=\u{3000}]."), "\u{3000}");
}

#[test]
fn exactly_four_hex_digits_are_consumed_and_the_rest_stays_literal() {
    // `　0` is U+3000 followed by the character `0`, not U+30000. Four
    // digits is the whole rule; there is no longest-match and no brace
    // form. Pinned because the alternative reading is plausible enough
    // that someone will eventually assume it.
    assert_eq!(single_literal("p := [text=\\u30000]."), "\u{3000}0");
}

#[test]
fn a_malformed_unicode_escape_is_an_error() {
    for (label, source) in [
        ("three digits then the end", "p := [text=\\u300]."),
        ("two digits then the end", "p := [text=\\u30]."),
        ("no digits at all", "p := [text=\\u]."),
        ("non-hexadecimal digits", "p := [text=\\uZZZZ]."),
        ("three hex digits then a non-digit", "p := [text=\\u30ag]."),
    ] {
        assert_eq!(
            lower_error_code(source),
            DiagnosticCode::Syntax,
            "case: {label}"
        );
    }
}

#[test]
fn an_escape_naming_a_surrogate_is_an_error() {
    // D800-DFFF are not Unicode scalar values; `char::from_u32` rejects
    // them and so must this, rather than silently substituting U+FFFD.
    assert_eq!(
        lower_error_code("p := [text=\\uD800]."),
        DiagnosticCode::Syntax
    );
    assert_eq!(
        lower_error_code("p := [text=\\uDFFF]."),
        DiagnosticCode::Syntax
    );
}

#[test]
fn an_unrecognised_escape_is_an_error_not_a_literal_backslash() {
    // Rejecting the unknown case keeps every other `\x` spelling free for a
    // later addition. Treating it as a literal backslash would make those
    // spellings permanently ambiguous.
    for source in [
        "p := [text=\\q].",
        "p := [text=\\n].",
        "p := [text=\\U0001F600].",
    ] {
        assert_eq!(lower_error_code(source), DiagnosticCode::Syntax, "{source}");
    }
}

#[test]
fn a_trailing_backslash_is_an_error() {
    assert_eq!(lower_error_code("p := [text=a\\]."), DiagnosticCode::Syntax);
}

#[test]
fn an_escape_inside_a_regex_value_is_left_to_the_regex_engine() {
    // `\d` is regex syntax and must reach the regex compiler untouched. If
    // the decoder ran here it would reject `\d` as an unknown escape.
    lower("p := [text~/\\d+/].");
    lower("p := [text~/\\u3000/].");
}
