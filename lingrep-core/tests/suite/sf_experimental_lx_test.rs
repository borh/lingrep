use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

const RESEARCH_DEFINITION_COUNT: usize = 86;
const APPROXIMATED_DEFINITIONS: &[&str] = &[];

fn compile_fixture() -> lingrep_core::fsm::CompileOutput {
    compile_catalog(
        PatternSource::named(
            "patterns/sf-experimental.lx",
            include_str!("../../../patterns/sf-experimental.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("experimental SF lingrep patterns must compile strictly")
}

#[test]
fn finite_predicates_and_negative_context_restore_source_semantics() {
    use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

    fn token(values: &[(TokenAttributeIndex, &str)]) -> TokenInfo {
        let mut token = TokenInfo::default();
        for (attribute, value) in values {
            token.set(*attribute, (*value).into());
        }
        token
    }

    let matcher = compile_fixture().matcher;
    let matched = |name: &str, tokens: &[TokenInfo]| {
        matcher
            .find_matches(tokens)
            .iter()
            .any(|matched| matched.pattern == name)
    };

    assert!(matched(
        "ある",
        &[token(&[
            (TokenAttributeIndex::Lemma, "ある"),
            (TokenAttributeIndex::Orth, "あん"),
        ])]
    ));
    assert!(!matched(
        "ある",
        &[token(&[
            (TokenAttributeIndex::Lemma, "ある"),
            (TokenAttributeIndex::Orth, "あろう"),
        ])]
    ));
    assert!(!matched(
        "なる",
        &[token(&[
            (TokenAttributeIndex::Lemma, "なる"),
            (TokenAttributeIndex::Orth, "なろう"),
        ])]
    ));

    let sou_da = |cform: Option<&str>| {
        let mut previous = vec![
            (TokenAttributeIndex::Orth, "する"),
            (TokenAttributeIndex::Lemma, "為る"),
        ];
        if let Some(cform) = cform {
            previous.push((TokenAttributeIndex::CForm, cform));
        }
        vec![
            token(&previous),
            token(&[(TokenAttributeIndex::Orth, "そう")]),
            token(&[
                (TokenAttributeIndex::Orth, "だ"),
                (TokenAttributeIndex::Lemma, "だ"),
            ]),
        ]
    };
    assert!(matched("そうだ", &sou_da(Some("終止形-一般"))));
    assert!(!matched("そうだ", &sou_da(Some("連用形-一般"))));
    assert!(matched("そうだ", &sou_da(None)));
}

#[test]
fn sou_da_matches_the_pinned_negative_previous_token_semantics() {
    use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

    fn token(values: &[(TokenAttributeIndex, &str)]) -> TokenInfo {
        let mut token = TokenInfo::default();
        for (attribute, value) in values {
            token.set(*attribute, (*value).into());
        }
        token
    }

    let matcher = compile_fixture().matcher;
    let matched = |tokens: &[TokenInfo]| {
        matcher
            .find_matches(tokens)
            .iter()
            .any(|matched| matched.pattern == "そうだ")
    };
    let suffix = || {
        [
            token(&[(TokenAttributeIndex::Orth, "そう")]),
            token(&[
                (TokenAttributeIndex::Orth, "だ"),
                (TokenAttributeIndex::Lemma, "だ"),
            ]),
        ]
    };
    let with_previous = |cform: Option<&str>| {
        let mut previous = token(&[(TokenAttributeIndex::Orth, "する")]);
        if let Some(cform) = cform {
            previous.set(TokenAttributeIndex::CForm, cform.into());
        }
        let [sou, da] = suffix();
        vec![previous, sou, da]
    };

    assert!(matched(&suffix()));
    assert!(matched(&with_previous(None)));
    assert!(matched(&with_previous(Some("終止形-一般"))));
    assert!(!matched(&with_previous(Some("連用形-一般"))));
}

#[test]
fn catalog_contains_only_research_patterns() {
    let output = compile_fixture();
    assert_eq!(output.matcher.patterns().len(), RESEARCH_DEFINITION_COUNT);
    assert!(
        output
            .matcher
            .patterns()
            .keys()
            .all(|name| !name.starts_with('_'))
    );
}

#[test]
fn the_catalog_schema_refuses_what_the_rust_assertions_refused() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../patterns/sf-experimental.lx"),
    )
    .expect("catalog is readable");

    let mutations = [
        // `status:` is a defaults entry, so one edit reaches every definition.
        (
            "status: \"experimental\"",
            "status: \"draft\"",
            "not one of",
        ),
        // A pattern's `role:` is declared `research`; `helper` is the fragment
        // spelling, so this also proves the visibility scoping is load-bearing.
        ("role: \"research\"", "role: \"helper\"", "not one of"),
        // `layer:` is declared `string`; a number is the wrong type.
        ("layer: \"S2\"", "layer: 2", "declared `string`"),
        // `examples:` is declared `list`. Mutating the *declaration* rather
        // than a value, because every spelling of a non-list value here is a
        // syntax error before it can be a type error — and this proves the
        // declared type is read, which is the same claim.
        (
            "pattern examples: list required",
            "pattern examples: string required",
            "declared `string`",
        ),
        (
            "pattern research_classes: list required",
            "pattern research_classes: string required",
            "declared `string`",
        ),
    ];

    for (from, to, expected) in mutations {
        let mutated = source.replacen(from, to, 1);
        assert_ne!(mutated, source, "the `{from}` mutation applied");
        let error = match compile_catalog(
            PatternSource::named("sf-experimental.lx", &mutated),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("the schema must refuse `{to}`"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(expected),
            "`{to}` must be refused with `{expected}`; got {error}"
        );
    }

    let without_category = source.replacen("category: \"S1a 相\"\n", "", 1);
    assert_ne!(without_category, source, "the deletion applied");
    let error = match compile_catalog(
        PatternSource::named("sf-experimental.lx", &without_category),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("a required key that is absent is refused"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("requires `category`"), "{error}");

    let without_research_classes = source.replacen("research_classes: [\"S3A\"]\n", "", 1);
    assert_ne!(
        without_research_classes, source,
        "the research-class deletion applied"
    );
    let error = match compile_catalog(
        PatternSource::named("sf-experimental.lx", &without_research_classes),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("a research pattern without research_classes must be refused"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("requires `research_classes`"),
        "{error}"
    );
}

#[test]
fn research_metadata_and_approximations_are_explicit() {
    use std::collections::BTreeSet;

    let output = compile_fixture();
    let mut approximated = BTreeSet::new();
    for (name, pattern) in output.matcher.patterns() {
        // `approximation:` is declared `string` but its *membership* — which
        // definitions carry it — is not something a schema can state, so it
        // stays here.
        if pattern.metadata.contains_key("approximation") {
            approximated.insert(name.as_str());
        }
    }

    assert_eq!(
        approximated,
        APPROXIMATED_DEFINITIONS.iter().copied().collect()
    );
    for name in [
        "と思われる",
        "ずにはおかない",
        "というのである",
        "と言われる",
    ] {
        assert_eq!(output.matcher.patterns()[name].metadata["layer"], "S2");
    }
    assert_eq!(
        output.matcher.patterns()["に他ならない"].metadata["layer"],
        "S1"
    );
}

#[test]
fn research_classes_cover_the_sentence_final_taxonomy() {
    let output = compile_fixture();

    for (name, pattern) in output.matcher.patterns() {
        let classes = pattern
            .metadata
            .get("research_classes")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{name} must declare a non-empty research_classes list"));
        assert!(
            !classes.is_empty(),
            "{name} must declare at least one sentence-final research class"
        );
    }

    let expected = [
        ("に違いない", &["S2A"][..]),
        ("と信じる", &["S2B"][..]),
        ("と考える", &["S2B"][..]),
        ("意図する", &["S2B"][..]),
        ("と判断できる", &["S2B"][..]),
        ("ことがわかる", &["S2B"][..]),
        ("だろう", &["S2C"][..]),
        ("かもしれない", &["S2D"][..]),
        ("なければならない", &["S2E", "S3C"][..]),
        ("のだ", &["S2F", "S3E"][..]),
        ("そうだ", &["S2G"][..]),
        ("たい", &["S2I"][..]),
        ("ことが多い", &["S1C"][..]),
        ("指す", &["S1D"][..]),
        ("ている", &["S3A"][..]),
        ("される", &["S3B"][..]),
        ("重要だ", &["S3C"][..]),
        ("ことになる", &["S3D"][..]),
        ("のである", &["S2F", "S3E"][..]),
        ("ということだ", &["S3F"][..]),
    ];
    for (name, classes) in expected {
        assert_eq!(
            output.matcher.patterns()[name].metadata["research_classes"],
            serde_json::json!(classes),
            "{name} must retain its agreed sentence-final classification"
        );
    }
}

#[test]
fn mono_dearu_excludes_the_conjectural_aro_u_form() {
    let matcher = compile_fixture().matcher;
    let matched = |tokens: &[lingrep_core::token_match::TokenInfo]| {
        matcher
            .find_matches(tokens)
            .iter()
            .any(|found| found.pattern == "ものである")
    };

    let declarative = [
        attributed("もの", "物"),
        attributed("で", "だ"),
        attributed("ある", "有る"),
    ];
    assert!(
        matched(&declarative),
        "the declarative form must remain covered"
    );

    let conjectural = [
        attributed("もの", "物"),
        attributed("で", "だ"),
        attributed("あろう", "有る"),
    ];
    assert!(
        !matched(&conjectural),
        "ものであろう belongs to the separate S2C であろう expression"
    );
}

// `exhaustive_examples_match_their_own_definition` lived here, checking this
// catalog and no other. `catalog_examples.rs` now checks every tracked
// catalog through `verify_examples`; two implementations of one check drift.

// --- Policy statement semantics -----------------------------------------------
//
// Each of these pins one policy statement against a hand-built sentence. Tokens
// are hand-built rather than tokenized because the assertion is about the
// policy program, not about UniDic: a dictionary revision must not be able to
// turn one of these red.

/// A token carrying the attributes these tests read.
fn attributed(orth: &str, lemma: &str) -> lingrep_core::token_match::TokenInfo {
    use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Lemma, lemma.into());
    token
}

/// The names this catalog publishes for one sentence, after policy.
fn published(tokens: &[lingrep_core::token_match::TokenInfo]) -> Vec<String> {
    compile_fixture()
        .matcher
        .find_published_matches(tokens)
        .expect("the declared anchor resolves")
        .iter()
        .map(|found| found.pattern.to_string())
        .collect()
}

#[test]
fn a_negated_teiru_is_not_published_but_a_bare_one_is() {
    // 行っ-て-い-ない: `ている` spans `て` and `い`, and `ない` follows it.
    let negated = [
        attributed("行っ", "行く"),
        attributed("て", "て"),
        attributed("い", "居る"),
        attributed("ない", "ない"),
    ];
    assert!(
        !published(&negated).iter().any(|name| name == "ている"),
        "a negated progressive must not publish as 相: {:?}",
        published(&negated)
    );

    // The same sentence without the negation. The statement tests the token
    // after the match, so removing it is the whole difference.
    let bare = [
        attributed("行っ", "行く"),
        attributed("て", "て"),
        attributed("い", "居る"),
    ];
    assert!(
        published(&bare).iter().any(|name| name == "ている"),
        "a bare progressive still publishes: {:?}",
        published(&bare)
    );
}

/// The negation operand is its own fragment, and it reads `lemma`.
///
/// `NAI` is a *wider* set (`ない 無い ず ぬ なかっ 無かっ ねえ`), read on `orth`,
/// with an optional trailing `た`. Reusing it would also drop a `ている` before
/// the colloquial `ねえ`. The operand reads `lemma` because `orth` would miss
/// the inflected `なかっ`.
///
/// Both halves are asserted because they fail to different edits: the first to
/// spelling the operand on `orth`, the second to pointing it at `NAI`.
#[test]
fn the_negation_operand_reads_lemma_and_is_not_nai() {
    let inflected = [
        attributed("行っ", "行く"),
        attributed("て", "て"),
        attributed("い", "居る"),
        // orth なかっ, lemma ない: only a lemma test sees this one.
        attributed("なかっ", "ない"),
    ];
    assert!(
        !published(&inflected).iter().any(|name| name == "ている"),
        "an inflected negation is still a negation: {:?}",
        published(&inflected)
    );

    // `ねえ` is in `NAI` and not in `NEG`, so a catalog that reused `NAI`
    // would drop this record.
    let outside = [
        attributed("行っ", "行く"),
        attributed("て", "て"),
        attributed("い", "居る"),
        attributed("ねえ", "ねえ"),
    ];
    assert!(
        published(&outside).iter().any(|name| name == "ている"),
        "`ねえ` is outside the `NEG` set: {:?}",
        published(&outside)
    );
}

/// The passive is dropped and the aspect that follows it is kept.
///
/// The subject is `@される` and the operand `%aspect` with `relation: after`.
/// The opposite assignment, `%aspect` before the passive, would drop the
/// aspect record.
///
/// Asserting only that "one record disappeared" would pass under either
/// reading, so this asserts which — by comparing the complete published set,
/// not by probing for one name.
#[test]
fn a_passive_before_an_aspect_is_dropped_and_the_aspect_is_kept() {
    // さ-れ-て-い: `される` spans さ-れ, and `ている` starts exactly where it ends.
    let consumed = [
        attributed("さ", "為る"),
        attributed("れ", "れる"),
        attributed("て", "て"),
        attributed("い", "居る"),
    ];
    let names = published(&consumed);
    assert!(
        names.iter().any(|name| name == "ている"),
        "the aspect survives: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "される"),
        "and the passive it consumed does not: {names:?}"
    );

    // The same passive with no aspect after it is untouched, which is what
    // makes the statement conditional rather than a blanket suppression.
    let alone = [attributed("さ", "為る"), attributed("れ", "れる")];
    assert!(
        published(&alone).iter().any(|name| name == "される"),
        "an unconsumed passive still publishes: {:?}",
        published(&alone)
    );
}

/// A bare-S0 sentence publishes only the S0 patterns exempt from the
/// upper-layer requirement.
///
/// Every S0 record is kept when the sentence carries any non-S0 record;
/// otherwise only the exempt ones are. It is a sentence-wide presence test,
/// not an overlap contest, which is why `resolve` cannot express it and
/// `require … relation: in_sentence` can.
///
/// All three halves, because each pair alone is satisfied by a mistake. Keeping
/// `ある` where an upper layer is present, and dropping it where none is, is
/// consistent with a statement over the wrong subject — until the third half
/// shows `だ` surviving the very sentence shape that dropped `ある`. That
/// asymmetry is the whole declaration: `だ` carries no key saying "always keep",
/// it is simply absent from `%s0_conditional`.
#[test]
fn bare_s0_survives_only_where_it_opted_out() {
    // 問題-が-ある. One S0 record, nothing above it.
    let bare = [
        attributed("問題", "問題"),
        attributed("が", "が"),
        attributed("ある", "ある"),
    ];
    assert!(
        !published(&bare).iter().any(|name| name == "ある"),
        "a conditional S0 alone in its sentence is not published: {:?}",
        published(&bare)
    );

    // 問題-が-ある-よう-だ. The same `ある`, now with an S2 record after it.
    // `ようだ` publishes as `よう` and suppresses the overlapping `だ`; what
    // matters here is only that it is in `%upper_layer` and survives to be
    // seen, since `evidence: current` reads the surviving set.
    let supported = [
        attributed("問題", "問題"),
        attributed("が", "が"),
        attributed("ある", "ある"),
        attributed("よう", "よう"),
        attributed("だ", "だ"),
    ];
    let names = published(&supported);
    assert!(
        names.iter().any(|name| name == "よう"),
        "the upper-layer record the S0 depends on: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "ある"),
        "and the S0 it licenses: {names:?}"
    );

    // 問題-だ. Bare S0 again, and `だ` publishes anyway.
    let always = [attributed("問題", "問題"), attributed("だ", "だ")];
    assert!(
        published(&always).iter().any(|name| name == "だ"),
        "`だ` is outside `%s0_conditional` and is never selected: {:?}",
        published(&always)
    );
}

// --- The 言う family reads `orthbase`, not `lemma` -----------------------------

/// A token carrying `orthbase` as well, for the 言う family.
///
/// UniDic's `lemma` for the potential 言える is 言う — the same lemma the plain
/// 言う carries — so a predicate over `lemma` cannot tell the two apart.
/// `orthbase` can: 言える has 言える, 言う has 言う. Every value here is taken
/// from a real UniDic analysis of the sentence in the test that reads it.
fn based(orth: &str, lemma: &str, orthbase: &str) -> lingrep_core::token_match::TokenInfo {
    use lingrep_core::token_match::TokenAttributeIndex;
    let mut token = attributed(orth, lemma);
    token.set(TokenAttributeIndex::OrthBase, orthbase.into());
    token
}

/// The potential 言える and the plain 言う are different patterns.
///
/// UniDic's lemma for 言える is 言う, so a `lemma` test cannot tell them apart:
/// written on `lemma`, every 「という」 matched と言える as well, on the same two
/// tokens, and every 「と言える」 matched という. The patterns therefore test
/// `orthbase`.
///
/// Asserted as the complete published set in both directions, because a probe
/// for one name passes while the *other* pattern is still over-matching, which
/// is the half of the defect a probe for the other name misses.
#[test]
fn the_potential_and_the_plain_verb_are_told_apart_by_orthbase() {
    // 努力-の-結果-と-言える. UniDic: orth 言える, lemma 言う, orthbase 言える.
    let potential = [
        based("努力", "努力", "努力"),
        based("の", "の", "の"),
        based("結果", "結果", "結果"),
        based("と", "と", "と"),
        based("言える", "言う", "言える"),
    ];
    assert_eq!(
        published(&potential),
        vec!["と言える"],
        "the potential publishes as と言える and not as という"
    );

    // 特徴-で-ある-と-も-いえる. The kana spelling, which `orthbase` keeps
    // distinct from いう.
    let kana = [
        based("特徴", "特徴", "特徴"),
        based("で", "だ", "だ"),
        based("ある", "有る", "ある"),
        based("と", "と", "と"),
        based("も", "も", "も"),
        based("いえる", "言う", "いえる"),
    ];
    assert!(
        published(&kana).iter().any(|name| name == "ともいえる"),
        "the kana potential publishes as ともいえる: {:?}",
        published(&kana)
    );

    // そう-だ-と-いう. The plain verb, whose orthbase is いう.
    let plain = [
        based("そう", "そう", "そう"),
        based("だ", "だ", "だ"),
        based("と", "と", "と"),
        based("いう", "言う", "いう"),
    ];
    let names = published(&plain);
    assert!(
        names.iter().any(|name| name == "という"),
        "the plain verb still publishes as という: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "と言える"),
        "and never as the potential: {names:?}"
    );
}

/// No 言う pattern admits the potential in the 言う slot, and every one still
/// admits both spellings.
///
/// The two-token `という` is not the only pattern that read `lemma`. Four
/// others put the same verb in the same slot with a longer tail, and a
/// potential fits three of those tails in ordinary Japanese —
/// 「…と言えるのである」 and 「…と言えることだ」 are not contrived. They were
/// published as `というのである` and `ということだ`.
///
/// The second half is the guard against the plausible wrong fix. `orthbase`
/// is `いう` for a kana 「いい」 and `言う` only for the kanji, so the test must
/// admit both. A single-valued `orthbase=言う` would pass every negative
/// assertion here and stop matching 「十分であるとはいいがたい」, the second
/// example of `とは言い難い`.
#[test]
fn no_iu_pattern_admits_the_potential_and_all_admit_both_spellings() {
    // 圧倒-さ-れ-て-しまう-と-言える-の-で-ある.
    let no_de_aru = [
        based("圧倒", "圧倒", "圧倒"),
        based("さ", "為る", "する"),
        based("れ", "れる", "れる"),
        based("て", "て", "て"),
        based("しまう", "仕舞う", "しまう"),
        based("と", "と", "と"),
        based("言える", "言う", "言える"),
        based("の", "の", "の"),
        based("で", "だ", "だ"),
        based("ある", "有る", "ある"),
    ];
    let names = published(&no_de_aru);
    assert!(
        !names.iter().any(|name| name == "というのである"),
        "a potential before のである is not というのである: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "と言える"),
        "it is と言える: {names:?}"
    );

    // 重要-だ-と-言える-こと-だ.
    let koto_da = [
        based("重要", "重要", "重要"),
        based("だ", "だ", "だ"),
        based("と", "と", "と"),
        based("言える", "言う", "言える"),
        based("こと", "事", "こと"),
        based("だ", "だ", "だ"),
    ];
    assert!(
        !published(&koto_da)
            .iter()
            .any(|name| name == "ということだ"),
        "a potential before ことだ is not ということだ: {:?}",
        published(&koto_da)
    );

    // 十分-で-ある-と-は-いい-がたい. Both halves in kana; `orthbase` is いう.
    let kana_gatai = [
        based("十分", "十分", "十分"),
        based("で", "だ", "だ"),
        based("ある", "有る", "ある"),
        based("と", "と", "と"),
        based("は", "は", "は"),
        based("いい", "言う", "いう"),
        based("がたい", "難い", "がたい"),
    ];
    assert!(
        published(&kana_gatai)
            .iter()
            .any(|name| name == "とは言い難い"),
        "the kana spelling is a catalog example and must still match: {:?}",
        published(&kana_gatai)
    );

    // 攻撃-目標-と-なっ-た-と-言わ-れる. The passive's 言わ has orthbase 言う,
    // so the same two-element set covers an inflection that is not the
    // dictionary form.
    let iwareru = [
        based("攻撃", "攻撃", "攻撃"),
        based("目標", "目標", "目標"),
        based("と", "と", "と"),
        based("なっ", "成る", "なる"),
        based("た", "た", "た"),
        based("と", "と", "と"),
        based("言わ", "言う", "言う"),
        based("れる", "れる", "れる"),
    ];
    assert!(
        published(&iwareru).iter().any(|name| name == "と言われる"),
        "an inflected 言わ still matches と言われる: {:?}",
        published(&iwareru)
    );
}
