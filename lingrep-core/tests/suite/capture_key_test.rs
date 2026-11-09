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

use lingrep_core::fsm::{
    CompileLimits, DiagnosticCode, KeyComponent, KeyProjection, PatternSource, ProjectedKeyField,
    compile_catalog, project_key,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn compile(
    text: &str,
) -> Result<lingrep_core::fsm::CompileOutput, lingrep_core::fsm::CompileError> {
    compile_catalog(
        PatternSource::named("test.lx", text),
        CompileLimits::default(),
    )
}

fn compile_err(text: &str, why: &str) -> lingrep_core::fsm::CompileError {
    match compile(text) {
        Ok(_) => panic!("expected compilation to fail: {why}"),
        Err(error) => error,
    }
}

fn assert_code(error: &lingrep_core::fsm::CompileError, expected: DiagnosticCode) {
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == expected),
        "expected {expected:?}, got {:?}",
        error
            .diagnostics
            .iter()
            .map(lingrep_core::fsm::Diagnostic::code)
            .collect::<Vec<_>>()
    );
}

fn token(orth: &str, lemma: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Lemma, lemma.into());
    token
}

fn field(name: &str, value: &str) -> ProjectedKeyField {
    ProjectedKeyField {
        name: name.into(),
        value: value.to_owned(),
    }
}

const KEYED: &str = "key: [$noun.lemma, $verb.lemma]\npattern np := (?<noun> [text=実験]) [text=を] (?<verb> [text=行っ]).\n";

#[test]
fn a_declared_key_extracts_capture_names_and_attributes() {
    let output = compile(KEYED).expect("catalog compiles");
    let policy = output
        .matcher
        .catalog_policy()
        .get("np")
        .expect("np declares policy");
    assert_eq!(
        policy.key,
        vec![
            KeyProjection::capture(
                "noun",
                KeyComponent::Sole,
                TokenAttributeIndex::Lemma.into()
            ),
            KeyProjection::capture(
                "verb",
                KeyComponent::Sole,
                TokenAttributeIndex::Lemma.into()
            ),
        ],
        "the bare form is the `Sole` component, the one the compiler proves \
         covers exactly one token"
    );
}

/// The whole point: two surface realizations of one collocation project to one
/// key. Without this they are two rows, which is what `summarize` does today.
#[test]
fn two_inflections_of_one_collocation_project_to_the_same_key() {
    let output = compile(
        "key: [$noun.lemma, $verb.lemma]\npattern np := (?<noun> [text in {実験, 調査}]) [text=を] (?<verb> [text in {行っ, 行う}]).\n",
    )
    .expect("catalog compiles");
    let matcher = &output.matcher;

    let past = [
        token("実験", "実験"),
        token("を", "を"),
        token("行っ", "行う"),
    ];
    let present = [
        token("実験", "実験"),
        token("を", "を"),
        token("行う", "行う"),
    ];

    let key_of = |tokens: &[TokenInfo]| {
        let matches = matcher.find_matches(tokens);
        let matched = matches.first().expect("one match").clone();
        project_key(matcher.catalog_policy(), &matched, tokens).expect("record is keyed")
    };

    let past_key = key_of(&past);
    assert_eq!(
        past_key,
        vec![field("$noun.lemma", "実験"), field("$verb.lemma", "行う"),]
    );
    assert_eq!(
        past_key,
        key_of(&present),
        "行っ and 行う are one collocation and must key alike"
    );
}

#[test]
fn a_pattern_without_a_declared_key_projects_nothing() {
    let output = compile("pattern np := (?<noun> [text=実験]).\n").expect("catalog compiles");
    let tokens = [token("実験", "実験")];
    let matches = output.matcher.find_matches(&tokens);
    assert_eq!(
        project_key(
            output.matcher.catalog_policy(),
            matches.first().expect("one match"),
            &tokens
        ),
        None
    );
}

#[test]
fn a_missing_projected_attribute_unkeys_the_whole_record() {
    let output = compile("key: [unidic.pos1]\npattern p := [].\n").expect("catalog compiles");
    let tokens = [token("実験", "実験")];
    let matches = output.matcher.find_matches(&tokens);
    assert_eq!(
        project_key(
            output.matcher.catalog_policy(),
            matches.first().expect("one match"),
            &tokens,
        ),
        None
    );
}

#[test]
fn an_unknown_attribute_names_itself_in_the_diagnostic() {
    let error = compile_err(
        "key: [$noun.normm]\npattern np := (?<noun> [text=実験]).\n",
        "normm is not an attribute",
    );
    assert_code(&error, DiagnosticCode::Syntax);
}

#[test]
fn a_key_naming_no_capture_is_an_error() {
    let error = compile_err(
        "key: [$missing.lemma]\npattern np := (?<noun> [text=実験]).\n",
        "there is no capture called missing",
    );
    assert_code(&error, DiagnosticCode::AmbiguousCaptureReference);
}

/// P3 measured this: a quantified capture compiles silently and keeps only the
/// last iteration, leaving no trace in match output. Keying on one would key on
/// an arbitrary occurrence and look like it worked.
#[test]
fn keying_on_a_quantified_capture_is_an_error() {
    let error = compile_err(
        "key: [$noun.lemma]\npattern np := ((?<noun> [text=実験]) [text=と]){1,3} [text=行っ].\n",
        "a quantified capture keeps only its last iteration",
    );
    assert_code(&error, DiagnosticCode::AmbiguousCaptureReference);
}

#[test]
fn keying_on_a_capture_that_can_span_several_tokens_is_an_error() {
    let error = compile_err(
        "key: [$np.lemma]\npattern p := (?<np> [text=実験]{1,3}) [text=を].\n",
        "a multi-token capture has no single attribute value",
    );
    assert_code(&error, DiagnosticCode::MultiTokenCaptureProjection);
}

/// A capture reached through two fragment references expands to two
/// occurrences sharing one authored name, unique only by path.
#[test]
fn keying_on_a_doubly_expanded_capture_is_an_error() {
    let error = compile_err(
        "fragment cap := (?<c> [text=実験]).\n\
         key: [$c.lemma]\npattern p := @cap [text=を] @cap.\n",
        "two expansions share the authored name",
    );
    assert_code(&error, DiagnosticCode::AmbiguousCaptureReference);
}

/// A key on a fragment can never run, the same class as `IneffectivePolicy`
/// for every other publication decision.
#[test]
fn a_key_on_a_fragment_is_reported_as_ineffective() {
    let output = compile(
        "key: [$c.lemma]\nfragment cap := (?<c> [text=実験]).\n\
         pattern p := @cap.\n",
    )
    .expect("an ineffective policy is not fatal");
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::IneffectivePolicy),
        "expected IneffectivePolicy for a key on a fragment"
    );
}

// ---------------------------------------------------------------------------
// Multi-token components: `first`, `last`, `span`.
//
// The bare form above stays sugar for a capture the compiler *proves* covers
// one token. These three waive that proof and say which reading is meant,
// because multi-token captures split into two populations wanting opposite
// rules, so there is no safe default.
// ---------------------------------------------------------------------------

/// The minimal pair the measurement produced, as one test: the same two-token
/// capture shape wants concatenation for a compound nominal and the leftmost
/// token for a suru predicate. Either rule applied to the other case invents a
/// string that is not a word — `細胞` for 細胞間, `減少為る` for 減少し.
#[test]
fn span_and_first_are_the_two_populations_that_want_opposite_rules() {
    let key_of = |declared: &str, left: (&str, &str), right: (&str, &str)| {
        let output = compile(&format!(
            "key: {declared}\npattern p := (?<c> [] []) [text=を].\n"
        ))
        .expect("catalog compiles");
        let tokens = [
            token(left.0, left.1),
            token(right.0, right.1),
            token("を", "を"),
        ];
        let matches = output.matcher.find_matches(&tokens);
        project_key(
            output.matcher.catalog_policy(),
            matches.first().expect("one match"),
            &tokens,
        )
        .expect("record is keyed")
    };

    // Compound nominal: concatenation is right, first-token would give 細胞.
    assert_eq!(
        key_of("[$c.span.lemma]", ("細胞", "細胞"), ("間", "間")),
        vec![field("$c.span.lemma", "細胞間")]
    );
    // Suru compound: the leftmost token is right, concatenation would give the
    // non-word 減少為る.
    assert_eq!(
        key_of("[$c.first.lemma]", ("減少", "減少"), ("し", "為る")),
        vec![field("$c.first.lemma", "減少")]
    );
    // And each rule really does produce the wrong string for the other case,
    // which is why neither can be the default.
    assert_eq!(
        key_of("[$c.first.lemma]", ("細胞", "細胞"), ("間", "間")),
        vec![field("$c.first.lemma", "細胞")]
    );
    assert_eq!(
        key_of("[$c.span.lemma]", ("減少", "減少"), ("し", "為る")),
        vec![field("$c.span.lemma", "減少為る")]
    );
}

/// `last` is distinguishable from `first` only on a capture wider than one
/// token, which is the only place it is legal anyway.
#[test]
fn last_reads_the_rightmost_token_of_the_capture() {
    let output = compile("key: [$c.last.lemma]\npattern p := (?<c> [] []) [text=を].\n")
        .expect("catalog compiles");
    let tokens = [token("研究", "研究"), token("者", "者"), token("を", "を")];
    let matches = output.matcher.find_matches(&tokens);
    assert_eq!(
        project_key(
            output.matcher.catalog_policy(),
            matches.first().expect("one match"),
            &tokens
        )
        .expect("record is keyed"),
        vec![field("$c.last.lemma", "者")]
    );
}

#[test]
fn an_explicit_component_admits_the_capture_the_bare_form_refuses() {
    let wide = "pattern p := (?<c> [text=実験]{1,3}) [text=を].\n";
    for declared in ["[$c.first.lemma]", "[$c.last.lemma]", "[$c.span.lemma]"] {
        compile(&format!("key: {declared}\n{wide}"))
            .unwrap_or_else(|error| panic!("{declared} must compile, got {error:?}"));
    }
    let error = compile_err(
        &format!("key: [$c.lemma]\n{wide}"),
        "the bare form still requires exactly one token",
    );
    assert_code(&error, DiagnosticCode::MultiTokenCaptureProjection);
}

#[test]
fn refusing_a_wide_bare_key_names_the_components_that_would_work() {
    let error = compile_err(
        "key: [$c.lemma]\npattern p := (?<c> [text=実験]{1,3}) [text=を].\n",
        "a multi-token capture has no single attribute value",
    );
    let message = error
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    for component in ["first", "last", "span"] {
        assert!(
            message.contains(component),
            "the diagnostic must name `{component}` as a way out; got: {message}"
        );
    }
}

#[test]
fn an_unbounded_capture_is_admitted_by_span_and_refused_by_the_bare_form() {
    let unbounded = "pattern p := (?<c> [text=実験]+) [text=を].\n";
    compile(&format!("key: [$c.span.lemma]\n{unbounded}"))
        .expect("an unbounded capture has a concrete span at projection time");
    assert_code(
        &compile_err(
            &format!("key: [$c.lemma]\n{unbounded}"),
            "unbounded is not exactly one token",
        ),
        DiagnosticCode::MultiTokenCaptureProjection,
    );
}

/// A zero-width capture has no token to read under any component. Admitting it
/// for `span` would mean keying on the empty string, which is a value, not an
/// absence, and would silently merge unrelated records.
#[test]
fn a_zero_width_capture_is_refused_under_every_component() {
    for declared in [
        "[$c.lemma]",
        "[$c.first.lemma]",
        "[$c.last.lemma]",
        "[$c.span.lemma]",
    ] {
        let error = compile_err(
            &format!(
                "key: {declared}\npattern p := [text=実験] (?<c> followed_by([text=を])) [text=を].\n"
            ),
            "a zero-width capture has no token to project",
        );
        assert_code(&error, DiagnosticCode::MultiTokenCaptureProjection);
    }
}

/// The ambiguity checks are about *which* capture is read, so they hold however
/// it is projected. A quantified capture keeps only its last iteration whether
/// the key asks for one token or all of them.
#[test]
fn a_quantified_capture_stays_an_error_under_every_component() {
    for declared in [
        "[$c.lemma]",
        "[$c.first.lemma]",
        "[$c.last.lemma]",
        "[$c.span.lemma]",
    ] {
        let error = compile_err(
            &format!(
                "key: {declared}\npattern p := ((?<c> [text=実験]) [text=と]){{1,3}} [text=行っ].\n"
            ),
            "a quantified capture keeps only its last iteration",
        );
        assert_code(&error, DiagnosticCode::AmbiguousCaptureReference);
    }
}

#[test]
fn invalid_projection_paths_are_compiler_errors() {
    let error = compile_err(
        "key: [$c.first]\npattern p := (?<c> [text=実験]).\n",
        "a component still requires an attribute",
    );
    assert_code(&error, DiagnosticCode::Syntax);
    compile("key: [$c.middle.lemma]\npattern p := (?<c> [text=実験]).\n")
        .expect("an unreserved first segment begins a provider attribute path");
    let error = compile_err(
        "key: [$c.norm.extra]\npattern p := (?<c> [text=実験]).\n",
        "portable scalar keys cannot grow an extra path segment",
    );
    assert_code(&error, DiagnosticCode::Syntax);
}

#[test]
fn a_dotted_provider_key_is_not_misread_as_a_capture_component() {
    compile("key: [$c.head.lemma]\npattern p := (?<c> [text=実験]).\n")
        .expect("head.lemma is a canonical provider key");
}

/// Key field order is authored tuple order, independent of serde feature
/// unification, because the dedicated CST list never passes through a map.
#[test]
fn key_field_order_preserves_authored_order() {
    let key_of = |declared: &str| {
        let output = compile(&format!(
            "key: {declared}\npattern p := (?<head> [text=行っ]) (?<collocate> [text=実験]).\n"
        ))
        .expect("catalog compiles");
        output
            .matcher
            .catalog_policy()
            .get("p")
            .expect("p declares policy")
            .key
            .iter()
            .map(KeyProjection::name)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        key_of("[$head.lemma, $collocate.lemma]"),
        vec!["$head.lemma", "$collocate.lemma"],
    );
    assert_eq!(
        key_of("[$collocate.lemma, $head.lemma]"),
        vec!["$collocate.lemma", "$head.lemma"],
    );
}

#[test]
fn ordered_keys_mix_match_and_capture_projections_with_canonical_names() {
    let output = compile(
        "key: [unidic.pos1, $head.span.norm, $collocate.norm]\n\
         pattern p := (?<head> (?<collocate> [])).\n",
    )
    .expect("explicit span permits the wide capture");
    let key = &output.matcher.catalog_policy().get("p").unwrap().key;
    assert_eq!(
        key,
        &[
            KeyProjection::match_attribute(
                lingrep_core::token_match::AttributeKey::new("unidic.pos1").unwrap(),
            ),
            KeyProjection::capture("head", KeyComponent::Span, TokenAttributeIndex::Norm.into()),
            KeyProjection::capture(
                "collocate",
                KeyComponent::Sole,
                TokenAttributeIndex::Norm.into(),
            ),
        ]
    );
    assert_eq!(
        key.iter().map(KeyProjection::name).collect::<Vec<_>>(),
        ["unidic.pos1", "$head.span.norm", "$collocate.norm"]
    );
}

#[test]
fn malformed_and_duplicate_ordered_keys_are_refused() {
    for source in [
        "key: []\npattern p := [].\n",
        "key: {$head: \"norm\"}\npattern p := (?<head> []).\n",
    ] {
        assert_code(
            &compile_err(source, "grammar rejects the old/empty shape"),
            DiagnosticCode::Syntax,
        );
    }
    assert_code(
        &compile_err(
            "key: [$head.norm, $head.norm]\npattern p := (?<head> []).\n",
            "a tuple cannot repeat a canonical field",
        ),
        DiagnosticCode::InvalidPolicyValue,
    );
}

#[test]
fn a_match_projection_requires_an_exactly_one_token_definition() {
    compile("key: [unidic.pos1]\npattern one := [].\n").expect("one token is exact");
    let direct = "key: [unidic.pos1]\npattern p := [] [].\n";
    let error = compile_err(direct, "a direct two-token body is too wide");
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::MultiTokenMatchProjection)
        .expect("width diagnostic");
    assert_eq!(&direct[diagnostic.location().byte_range()], "key");

    let error = compile_err(
        "key: [unidic.pos1]\npattern p := ([] | [] []).\n",
        "one alternation branch is wider than one token",
    );
    assert_code(&error, DiagnosticCode::MultiTokenMatchProjection);
}

#[test]
fn nullable_binding_failures_precede_whole_match_width_errors() {
    let error = compile_err(
        "key: [unidic.pos1]\npattern p := []?.\n",
        "binding rejects nullable emitting patterns before width analysis",
    );
    assert_code(&error, DiagnosticCode::NullablePattern);
    assert!(
        error
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code() != DiagnosticCode::MultiTokenMatchProjection)
    );
}

#[test]
fn a_match_projection_checks_width_after_resolving_a_fragment() {
    let source = "fragment pair := [] [].\nkey: [unidic.pos1]\npattern p := @pair.\n";
    let error = compile_err(source, "the referenced fragment consumes two tokens");
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::MultiTokenMatchProjection)
        .expect("width diagnostic");
    assert_eq!(&source[diagnostic.location().byte_range()], "key");
}

#[test]
fn key_defaults_inherit_unset_and_report_the_supplier() {
    compile("defaults\nkey: [unidic.pos1]\nend\npattern one := [].\n")
        .expect("one-token definition inherits the key");
    compile("defaults\nkey: [unidic.pos1]\nend\nkey: none\npattern pair := [] [].\n")
        .expect("a local unset removes the inherited key");

    let source = "defaults\nkey: [unidic.pos1]\nend\npattern pair := [] [].\n";
    let error = compile_err(source, "the inherited key is too wide for pair");
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::MultiTokenMatchProjection)
        .expect("width diagnostic");
    assert_eq!(&source[diagnostic.location().byte_range()], "key");
    assert_eq!(diagnostic.related_locations().len(), 1);
    assert_eq!(diagnostic.related_locations()[0].location().line(), 4);
    assert_eq!(
        diagnostic.related_locations()[0].message(),
        "definition `pair` inherits this key"
    );

    let source = "key: none\npattern one := [].\n";
    let error = compile_err(source, "there is no default to unset");
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::UnsetWithoutDefault)
        .expect("unset diagnostic");
    assert_eq!(&source[diagnostic.location().byte_range()], "key");
}

#[test]
fn binding_failures_precede_independent_whole_match_width_errors() {
    let source = "key: [unidic.pos1]\npattern wide := [] [].\npattern broken := @missing.\n";
    let error = compile_err(source, "binding must complete before width analysis");
    assert_code(&error, DiagnosticCode::UndefinedReference);
    assert!(
        error
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code() != DiagnosticCode::MultiTokenMatchProjection),
        "width analysis must not run on an unbound catalog"
    );
}

// ---------------------------------------------------------------------------
// Tagging is held to the same multiplicity rules as keying.
//
// An ambiguous capture is a compile error under keying and tagging, never a
// silent first- or all-wins choice. Only `key:` was checked, so a
// `tags:` projection over a repeated or multi-token capture did exactly what
// that forbids: it read the first token of the last iteration and published a
// plausible value. Form (b), capture *presence*, is exempt below and says why.
//
// A tag projection stays single-token. `key:`'s `{first|last|span}` spelling is
// not extended here: the measurement that justified components
// was about keys, and a second copy of the vocabulary would have to be kept in
// step with `substitute`'s unreconciled `component:` for no witnessed demand.
// ---------------------------------------------------------------------------

const QUANTIFIED: &str = "pattern p := ((?<c> [text=実験]) [text=と]){1,3} [text=行っ].\n";
const WIDE: &str = "pattern p := (?<c> [text=実験]{1,3}) [text=を].\n";

fn tagged_err(declared: &str, body: &str, why: &str) -> lingrep_core::fsm::CompileError {
    compile_err(&format!("tags: {declared}\n{body}"), why)
}

#[test]
fn a_tag_naming_no_capture_is_an_error() {
    assert_code(
        &tagged_err(
            "{voice: {$missing: \"lemma\"}}",
            "pattern p := (?<c> [text=実験]).\n",
            "there is no capture called missing",
        ),
        DiagnosticCode::AmbiguousCaptureReference,
    );
}

/// The gap this section closes. A quantified capture keeps only its last
/// iteration, so the tag carried a value off one arbitrary occurrence and no
/// output revealed which.
#[test]
fn tagging_a_quantified_capture_is_an_error() {
    assert_code(
        &tagged_err(
            "{voice: {$c: \"lemma\"}}",
            QUANTIFIED,
            "a quantified capture keeps only its last iteration",
        ),
        DiagnosticCode::AmbiguousCaptureReference,
    );
}

/// The other half of the gap: projection read `tokens[found.start]`
/// unconditionally, so a three-token capture silently tagged on its first.
#[test]
fn tagging_a_capture_that_can_span_several_tokens_is_an_error() {
    assert_code(
        &tagged_err(
            "{voice: {$c: \"lemma\"}}",
            WIDE,
            "a multi-token capture has no single attribute value",
        ),
        DiagnosticCode::MultiTokenCaptureProjection,
    );
}

#[test]
fn tagging_a_doubly_expanded_capture_is_an_error() {
    assert_code(
        &compile_err(
            "fragment cap := (?<c> [text=実験]).\n\
             tags: {voice: {$c: \"lemma\"}}\npattern p := @cap [text=を] @cap.\n",
            "two expansions share the authored name",
        ),
        DiagnosticCode::AmbiguousCaptureReference,
    );
}

#[test]
fn a_zero_width_capture_is_refused_as_a_tag_projection() {
    assert_code(
        &tagged_err(
            "{voice: {$c: \"lemma\"}}",
            "pattern p := [text=実験] (?<c> followed_by([text=を])) [text=を].\n",
            "a zero-width capture has no token to project",
        ),
        DiagnosticCode::MultiTokenCaptureProjection,
    );
}

#[test]
fn a_capture_reference_refusal_points_at_the_key_it_was_written_in() {
    const SOURCE: &str = "key: [$missing.lemma]\npattern p := (?<c> [text=実験]).\n";
    let error = compile_err(SOURCE, "there is no capture called missing");
    let range = error.diagnostics[0].location().byte_range();
    assert_eq!(&SOURCE[range.clone()], "key");
    assert_ne!(range, 0..0, "a real span, not the absent one this replaced");

    const TAGGED: &str = "tags: {voice: {$missing: \"lemma\"}}\npattern p := (?<c> [text=実験]).\n";
    let tagged = compile_err(TAGGED, "there is no capture called missing");
    let range = tagged.diagnostics[0].location().byte_range();
    assert_eq!(
        &TAGGED[range], "tags",
        "a `tags:` reference points at `tags:`, not at `key:`"
    );
}

#[test]
fn the_tag_width_refusal_names_tags_and_offers_no_key_components() {
    let message = tagged_err("{voice: {$c: \"lemma\"}}", WIDE, "a wide capture")
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        message.contains("tags:"),
        "must name `tags:`; got: {message}"
    );
    assert!(
        message.contains("voice"),
        "must name the tag field; got: {message}"
    );
    assert!(
        !message.contains("key:"),
        "the author wrote `tags:`; got: {message}"
    );
    for component in ["first", "last", "span"] {
        assert!(
            !message.contains(component),
            "`tags:` has no `{component}` spelling to offer; got: {message}"
        );
    }
}

/// Form (b) is presence, not projection: whichever occurrence participated, the
/// tag carries the value the author wrote for that capture name, so repetition
/// and width change nothing. Refusing it would break the acceptance minimal
/// pair, which is a presence tag over an alternation.
#[test]
fn a_capture_presence_tag_is_unaffected_by_repetition_or_width() {
    for body in [QUANTIFIED, WIDE] {
        compile(&format!("tags: {{voice: {{c: \"repeated\"}}}}\n{body}"))
            .unwrap_or_else(|error| panic!("presence is singular at any width, got {error:?}"));
    }
}

/// Static tags name no capture at all, so nothing here can reach them.
#[test]
fn a_static_tag_is_unaffected_by_repetition_or_width() {
    for body in [QUANTIFIED, WIDE] {
        compile(&format!("tags: {{voice: \"active\"}}\n{body}"))
            .unwrap_or_else(|error| panic!("a static tag reads no token, got {error:?}"));
    }
}

/// A tag projection off a capture the compiler proves covers one token is the
/// form this projection syntax supports, and it still compiles.
#[test]
fn a_single_token_tag_projection_still_compiles() {
    compile("tags: {voice: {$c: \"lemma\"}}\npattern p := (?<c> [text=実験]) [text=を].\n")
        .expect("a one-token capture is exactly what a tag projection reads");
}

/// A record whose key capture did not participate is unkeyed, and the caller
/// must be able to tell that from a record with no key declared at all.
#[test]
fn a_record_whose_key_capture_did_not_participate_is_unkeyed() {
    let output = compile(
        "key: [$noun.lemma]\npattern p := ((?<noun> [text=実験]) | [text=調査]) [text=を].\n",
    )
    .expect("catalog compiles");
    let tokens = [token("調査", "調査"), token("を", "を")];
    let matches = output.matcher.find_matches(&tokens);
    assert_eq!(
        project_key(
            output.matcher.catalog_policy(),
            matches.first().expect("one match"),
            &tokens
        ),
        None,
        "the alternation branch without the capture yields no key"
    );
}

#[hegel::test(test_cases = 60, derandomize = true)]
fn word_graph_keys_project_declared_scalar_values(tc: hegel::TestCase) {
    use hegel::generators as g;
    use lingrep_core::token_match::{BoundTokenSentence, TokenSchema};
    let attribute = tc.draw(g::sampled_from(vec![
        "norm",
        "spacy.reading",
        "unidic.orthbase",
    ]));
    let value = tc.draw(g::text());
    let present = tc.draw(g::booleans());
    let source = format!(
        "key: [$v.{attribute}]\npattern p := (?<n> [upos=NOUN]) where (?<v> [upos=VERB]) -[obj]-> $n."
    );
    let compiled = compile(&source).unwrap().matcher;
    let schema =
        TokenSchema::from_keys(["text", "upos", "dep.head", "dep.rel", attribute]).unwrap();
    let bound = compiled
        .bind_with_projection(&schema, schema.keys().iter().cloned())
        .unwrap();
    let values = (0..2)
        .flat_map(|i| {
            bound.projection().keys().iter().map({
                let value = &value;
                move |key| {
                    if key.as_str() == attribute && !present {
                        return None;
                    }
                    Some(match key.as_str() {
                        "text" => {
                            if i == 0 {
                                "声".into()
                            } else {
                                "云う".into()
                            }
                        }
                        "upos" => {
                            if i == 0 {
                                "NOUN".into()
                            } else {
                                "VERB".into()
                            }
                        }
                        "dep.head" => {
                            if i == 0 {
                                "2".into()
                            } else {
                                "0".into()
                            }
                        }
                        "dep.rel" => {
                            if i == 0 {
                                "obj".into()
                            } else {
                                "root".into()
                            }
                        }
                        _ => value.as_str().into(),
                    })
                }
            })
        })
        .collect();
    let words = BoundTokenSentence::from_projected_values(bound.projection(), 2, values).unwrap();
    let matches = bound.find_matches(&words).unwrap();
    let baseline = compile(&source.replace(&format!("$v.{attribute}"), "$v.text"))
        .unwrap()
        .matcher
        .bind_with_projection(&schema, schema.keys().iter().cloned())
        .unwrap();
    assert_eq!(matches, baseline.find_matches(&words).unwrap());
    assert_eq!(matches.len(), 1);
    assert_eq!(
        project_key(bound.catalog_policy(), &matches[0], &words),
        present.then(|| vec![field(&format!("$v.{attribute}"), &value)])
    );
}

#[test]
fn graph_keys_reject_attributes_not_available_on_empty_nodes() {
    for predicate in [
        "node.kind=empty",
        "upos=VERB",
        "node.kind in {word, empty}",
        "!(node.kind=word)",
    ] {
        let source = format!(
            "key: [$v.norm]\npattern p := (?<n> []) where (?<v> [{predicate}]) -[enhanced obj]-> $n."
        );
        assert_code(
            &compile_err(&source, "the binding can select empty nodes"),
            DiagnosticCode::InvalidPolicyValue,
        );
    }
}

#[test]
fn word_graph_key_eligibility_preserves_branch_and_schema_constraints() {
    use lingrep_core::token_match::{AttributeKind, SchemaAttribute, TokenSchema};
    for predicate in [
        "node.kind=word",
        "node.kind=word & upos=VERB",
        "(node.kind=word & upos=VERB) | (node.kind=word & upos=NOUN)",
    ] {
        let source = format!(
            "key: [$v.norm]\npattern p := (?<n> []) where (?<v> [{predicate}]) -[enhanced obj]-> $n."
        );
        let compiled = compile(&source).unwrap().matcher;
        let schema = TokenSchema::new([
            SchemaAttribute::new("dep.enhanced", AttributeKind::StringSet).unwrap(),
            SchemaAttribute::new("norm", AttributeKind::StringSet).unwrap(),
        ])
        .unwrap();
        let error = compiled.bind(&schema).unwrap_err();
        assert!(error.to_string().contains("norm"), "{error}");
        assert!(error.to_string().contains("scalar"), "{error}");
    }
    for predicate in [
        "node.kind=word | node.kind=empty",
        "(node.kind=word & upos=VERB) | upos=NOUN",
    ] {
        let source = format!(
            "key: [$v.norm]\npattern p := (?<n> []) where (?<v> [{predicate}]) -[enhanced obj]-> $n."
        );
        assert_code(
            &compile_err(&source, "one branch allows empty nodes"),
            DiagnosticCode::InvalidPolicyValue,
        );
    }
}

#[test]
fn accumulated_invalid_key_reports_its_own_source_location() {
    let mut builder = lingrep_core::fsm::FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("first.lx", "first := []."))
        .unwrap();
    let source = format!(
        "description: \"{}\"\nkey: [$missing.lemma]\nsecond := [].",
        "猫".repeat(80)
    );
    builder
        .add_definition(PatternSource::named("second.lx", &source))
        .unwrap();
    let error = match builder.build() {
        Ok(_) => panic!("the key names no capture"),
        Err(error) => error,
    };
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::AmbiguousCaptureReference)
        .unwrap();
    assert_eq!(diagnostic.location().source_name(), "second.lx");
    assert_eq!(diagnostic.location().line(), 2);
    assert_eq!(
        diagnostic.location().byte_range(),
        source.find("key:").unwrap()..source.find("key:").unwrap() + 3
    );
}

#[hegel::test(test_cases = 50, derandomize = true)]
fn accumulated_key_errors_preserve_registered_source_coordinates(tc: hegel::TestCase) {
    use hegel::generators as g;
    let label = format!("later:{}", tc.draw(g::text()));
    let prefix = format!(
        "description: {}\n",
        serde_json::to_string(&tc.draw(g::text())).unwrap()
    );
    let source = format!("{prefix}key: [$missing.lemma]\nsecond := [].");
    let mut builder = lingrep_core::fsm::FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("first.lx", "first := []."))
        .unwrap();
    builder
        .add_definition(PatternSource::named(&label, &source))
        .unwrap();
    let error = match builder.build() {
        Ok(_) => panic!("the key names no capture"),
        Err(error) => error,
    };
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::AmbiguousCaptureReference)
        .unwrap();
    assert_eq!(diagnostic.location().source_name(), label);
    assert_eq!(diagnostic.location().line(), 2);
    assert_eq!(
        diagnostic.location().byte_range(),
        prefix.len()..prefix.len() + 3
    );
}

#[hegel::test(test_cases = 60, derandomize = true)]
fn capture_key_edit_ranges_round_trip_without_changing_recognition(tc: hegel::TestCase) {
    use hegel::generators as g;
    let prefix = serde_json::to_string(&tc.draw(g::text())).unwrap();
    let newline = tc.draw(g::sampled_from(vec!["\n", "\r\n"]));
    let source = format!(
        "description: {prefix}{newline}key: [$word.lemma]{newline}pattern p := (?<word> [text=猫]).{newline}"
    );
    let output = compile(&source).unwrap();
    let [edit] = output.capture_key_edits.as_slice() else {
        panic!("one eligible key");
    };
    assert_eq!(&source[edit.byte_range.clone()], "lemma");
    let tokens = [token("猫", "猫")];
    let expected = output.matcher.find_matches(&tokens);
    for attribute in edit.attributes {
        let mut changed = source.clone();
        changed.replace_range(edit.byte_range.clone(), attribute);
        let changed = compile(&changed).unwrap();
        assert_eq!(changed.matcher.find_matches(&tokens), expected);
        assert_eq!(changed.capture_key_edits[0].attribute, attribute);
        let key = project_key(changed.matcher.catalog_policy(), &expected[0], &tokens);
        if attribute == "norm" {
            assert_eq!(key, None);
        } else {
            assert_eq!(key, Some(vec![field(&format!("$word.{attribute}"), "猫")]));
        }
    }
}

#[test]
fn accumulated_capture_key_edits_name_their_own_source_files() {
    let mut builder = lingrep_core::fsm::FsmBuilder::new();
    for name in ["first", "second"] {
        builder
            .add_definition(PatternSource::named(
                name,
                &format!("key: [$word.lemma]\npattern {name} := (?<word> [])."),
            ))
            .unwrap();
    }
    let output = builder.build().unwrap();
    assert_eq!(
        output
            .capture_key_edits
            .iter()
            .map(|edit| edit.source_name.as_ref())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}
