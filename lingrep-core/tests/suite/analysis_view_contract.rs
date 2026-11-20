//! The Vibrato session analyzes the fold view of each sentence and publishes
//! the exact source slice as `surface` and its match key as `text`.

#![allow(clippy::unwrap_used)]

use std::{collections::BTreeMap, sync::LazyLock};

use lingrep_core::analysis::{
    AnalyzerPlan, AnalyzerSession, InputUnitId, NormalizationPolicy, TextUnit, VibratoPlan,
};
use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};

static PLAN: LazyLock<VibratoPlan> = LazyLock::new(|| {
    VibratoPlan::new(
        lingrep_core::testing::test_tokenizer().clone(),
        std::path::Path::new(&lingrep_core::testing::dictionary_path()),
    )
    .expect("the test dictionary lexicon enumerates")
});

/// One analyzed token: its source byte range and every schema attribute.
#[derive(Debug, PartialEq, Eq)]
struct Token {
    source: String,
    attributes: BTreeMap<String, Option<String>>,
}

impl Token {
    fn get(&self, key: &str) -> Option<&str> {
        self.attributes[key].as_deref()
    }

    /// The attributes other than `surface`, which is the only one that keeps
    /// the source spelling.
    fn analysis(&self) -> BTreeMap<String, Option<String>> {
        let mut attributes = self.attributes.clone();
        attributes.remove("surface");
        attributes
    }
}

fn unit(text: &str, normalization: NormalizationPolicy) -> TextUnit {
    TextUnit::sentence(InputUnitId::new(0), text, normalization).unwrap()
}

fn analyze_with(text: &str, normalization: NormalizationPolicy) -> Vec<Token> {
    let schema = PLAN.schema();
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .unwrap();
    let input = unit(text, normalization);
    let batch = PLAN
        .create_session()
        .analyze_bounded_batch(vec![input], &projection)
        .unwrap();
    let analyzed = &batch.inputs()[0];
    let sentence = &analyzed.sentences()[0];
    sentence
        .surface_tokens()
        .iter()
        .enumerate()
        .map(|(index, surface)| {
            let token = sentence.words().token(index).unwrap();
            Token {
                source: analyzed
                    .input()
                    .slice(surface.source_range())
                    .unwrap()
                    .into(),
                attributes: schema
                    .keys()
                    .iter()
                    .map(|key| {
                        let ordinal = projection.ordinal_for_key(key).unwrap();
                        (
                            key.as_str().to_owned(),
                            token.get(ordinal).map(str::to_owned),
                        )
                    })
                    .collect(),
            }
        })
        .collect()
}

fn analyze(text: &str) -> Vec<Token> {
    analyze_with(text, NormalizationPolicy::None)
}

fn surfaces(tokens: &[Token]) -> Vec<&str> {
    tokens
        .iter()
        .map(|token| token.get("surface").unwrap())
        .collect()
}

/// Every token's `surface` is the source slice its range covers, and the
/// surfaces tile the sentence.
fn assert_surfaces_tile(text: &str, tokens: &[Token]) {
    for token in tokens {
        assert_eq!(token.get("surface"), Some(token.source.as_str()));
    }
    assert_eq!(surfaces(tokens).concat(), text);
}

/// A variant spelling analyzes exactly as its plain spelling does: the same
/// tokens with the same attributes, apart from `surface`, which keeps the
/// variant.
#[yare::parameterized(
    ideographic_variation_sequence = { "葛\u{E0100}城市に住む", "葛城市に住む" },
    half_width_katakana = { "ｶﾞﾗｽの靴", "ガラスの靴" },
    fullwidth_tilde_between_kana = { "長～い", "長〜い" },
    decomposed_kana = { "か\u{3099}まんする", "がまんする" },
    compatibility_ideograph_outside_its_surfaces = { "\u{FA5C}い匂い", "臭い匂い" },
)]
fn variant_spelling_analyzes_as_its_plain_spelling(variant: &str, plain: &str) {
    let variant_tokens = analyze(variant);
    let plain_tokens = analyze(plain);

    assert_surfaces_tile(variant, &variant_tokens);
    assert_eq!(
        variant_tokens
            .iter()
            .map(Token::analysis)
            .collect::<Vec<_>>(),
        plain_tokens.iter().map(Token::analysis).collect::<Vec<_>>()
    );
}

#[test]
fn ivs_stays_inside_one_token_surface_and_leaves_the_text_key() {
    let tokens = analyze("葛\u{E0100}城市に住む");

    assert_eq!(tokens[0].get("surface"), Some("葛\u{E0100}城"));
    assert_eq!(tokens[0].get("text"), Some("葛城"));
    assert_eq!(tokens[0].get("lemma"), analyze("葛城")[0].get("lemma"));
}

#[test]
fn wave_dash_spellings_form_one_adjective() {
    let tokens = analyze("長～い");

    assert_eq!(surfaces(&tokens), ["長～い"]);
    assert_eq!(tokens[0].get("text"), Some("長〜い"));
    assert_eq!(tokens[0].get("upos"), Some("ADJ"));
}

/// UniDic lists 臭氣 only with U+FA5C, so the fold keeps that ideograph
/// inside the surface; the same ideograph before い is unified to U+81ED.
#[test]
fn compatibility_ideograph_is_kept_only_inside_a_dictionary_surface() {
    let protected = analyze("\u{FA5C}氣");
    assert_eq!(surfaces(&protected), ["\u{FA5C}氣"]);
    assert_eq!(protected[0].get("text"), Some("\u{81ED}氣"));
    assert_eq!(protected[0].get("unidic.orthbase"), Some("\u{FA5C}氣"));

    let unified = analyze("\u{FA5C}い");
    assert_eq!(unified[0].get("surface"), Some("\u{FA5C}い"));
    assert_eq!(unified[0].get("text"), Some("\u{81ED}い"));
    assert_eq!(unified[0].get("unidic.orthbase"), Some("\u{81ED}い"));
}

/// UniDic's only entry spelled U+FA15 alone is the name ヒロシ, and U+51DE
/// occurs in compounds such as 康凞, so U+FA15 is unified and the compound
/// entries are reached.
#[yare::parameterized(
    emperor = { "康\u{FA15}帝", "康\u{51DE}", "コウキ" },
    given_name = { "忠\u{FA15}", "忠\u{51DE}", "タダヒロ" },
    reduplication = { "\u{FA15}\u{FA15}", "\u{51DE}\u{51DE}", "キキ" },
)]
fn single_compatibility_entry_is_unified_where_its_unified_form_is_in_a_surface(
    text: &str,
    first_text: &str,
    first_lform: &str,
) {
    let tokens = analyze(text);
    assert_surfaces_tile(text, &tokens);
    assert_eq!(tokens[0].get("text"), Some(first_text));
    assert_eq!(tokens[0].get("unidic.lform"), Some(first_lform));
}

/// U+212B ANGSTROM SIGN is a UniDic surface and U+00C5 occurs in none, so the
/// sign stays protected.
#[test]
fn angstrom_sign_stays_protected() {
    let tokens = analyze("\u{212B}");
    assert_eq!(surfaces(&tokens), ["\u{212B}"]);
    assert_eq!(tokens[0].get("unidic.orthbase"), Some("\u{212B}"));
}

/// Samples tokenized through the plan keep the dictionary's protected
/// surfaces, as its sessions do.
#[test]
fn plan_tokenize_protects_the_surfaces_its_sessions_protect() {
    let tokens = PLAN.tokenize("\u{FA5C}氣");
    let surfaces = tokens
        .iter()
        .map(|token| token.surface().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(surfaces, ["\u{FA5C}氣"]);
    assert_eq!(tokens[0].orth(), Some("\u{81ED}氣"));
    assert_eq!(tokens[0].orth_base(), Some("\u{FA5C}氣"));
}

/// Full-width ASCII reaches the analyzer unchanged and is folded only in the
/// `text` key.
#[test]
fn full_width_ascii_is_folded_in_text_only() {
    let tokens = analyze("ＮＨＫを見る");

    assert_eq!(tokens[0].get("surface"), Some("ＮＨＫ"));
    assert_eq!(tokens[0].get("text"), Some("NHK"));
    assert_eq!(tokens[0].get("unidic.orthbase"), Some("ＮＨＫ"));
    assert_eq!(tokens[0].get("upos"), Some("PROPN"));
}

/// Text the fold leaves unchanged has `text` equal to `surface` on every
/// token.
#[yare::parameterized(
    kana_and_kanji = { "猫が走る。" },
    ascii = { "lingrep matches tokens." },
    mixed = { "東京都で2024年にAIを研究した。" },
)]
fn unfolded_text_has_text_equal_to_surface(text: &str) {
    let tokens = analyze(text);

    for token in &tokens {
        assert_eq!(token.get("text"), token.get("surface"));
    }
    assert_eq!(
        surfaces(&tokens).concat(),
        text.replace(' ', ""),
        "only spaces fall between tokens"
    );
}

/// Under `--normalization nfc` the admitted text is already composed, so
/// `surface` is a slice of the NFC text rather than of the raw input.
#[test]
fn nfc_admission_composes_before_the_fold() {
    let tokens = analyze_with("か\u{3099}まんする", NormalizationPolicy::Nfc);

    assert_eq!(tokens[0].get("surface"), Some("がまん"));
}

fn find_matches(catalog: &str, text: &str) -> Vec<Match> {
    let compiled = compile_catalog(
        PatternSource::named("view.lx", catalog),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = compiled.bind(PLAN.schema()).unwrap();
    let input = unit(text, NormalizationPolicy::None);
    let batch = PLAN
        .create_session()
        .analyze_bounded_batch(vec![input], bound.projection())
        .unwrap();
    bound
        .find_matches(batch.inputs()[0].sentences()[0].words())
        .unwrap()
}

/// `text` matches the variant and the plain spelling alike, with the same
/// captures; `surface` distinguishes them.
#[test]
fn text_matches_both_spellings_and_surface_only_the_source() {
    let catalog = "pattern city := (?<name> [text=葛城]) [text=市].\n\
                   pattern exact := [surface=葛\u{E0100}城].\n";

    let variant = find_matches(catalog, "葛\u{E0100}城市に住む");
    let plain = find_matches(catalog, "葛城市に住む");

    assert_eq!(
        variant
            .iter()
            .filter(|m| m.pattern == "city")
            .collect::<Vec<_>>(),
        plain.iter().collect::<Vec<_>>()
    );
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].captures.len(), 1);
    assert_eq!(
        variant
            .iter()
            .filter(|m| m.pattern == "exact")
            .map(|m| (m.start, m.end))
            .collect::<Vec<_>>(),
        [(0, 1)]
    );
}
