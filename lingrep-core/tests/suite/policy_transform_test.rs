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
//! The record transform executor and the keying
//! and tagging that run after it.
//!
//! Every test drives the **whole chain** — lingrep patterns source with a `metadata` block,
//! compiled, matched, then published — rather than hand-building a
//! `PatternPolicy`. The transform vocabulary is authored as metadata and the
//! guards are compiled by the lingrep patterns compiler itself, so a test that skipped
//! extraction would leave the two halves free to disagree about what
//! `{op: drop_edge, edge: trailing}` means.
//!
//! The six ground-truth rows are real UniDic-CWJ tokenizations, transcribed
//! here as tokens rather than re-tokenized so the suite needs no dictionary.

use compact_str::CompactString;
use lingrep_core::fsm::{
    CatalogPolicy, CompileLimits, PatternSource, PolicyStats, ProgramStats, ProjectedKeyField,
    PublishedRecord, apply_policy_records, compile_catalog,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfoTrait};

/// A token carrying exactly the attributes it was given.
///
/// Absence has to be expressible: a token *lacking* an attribute is what
/// decides three of the rules under test — a guard admits neither polarity of
/// it, a substitution leaves it alone, and a key projection over it unkeys the
/// whole record.
#[derive(Clone)]
struct Token(Vec<(TokenAttributeIndex, CompactString)>);

impl TokenInfoTrait for Token {
    fn get(&self, index: TokenAttributeIndex) -> Option<&CompactString> {
        self.0
            .iter()
            .find(|(candidate, _)| *candidate == index)
            .map(|(_, value)| value)
    }
}

/// One token as `surface/lemma/pos/cform`, the shape P1's appendix records.
fn token(surface: &str, lemma: &str, pos: &str, cform: &str) -> Token {
    Token(vec![
        (TokenAttributeIndex::Orth, CompactString::new(surface)),
        (TokenAttributeIndex::Lemma, CompactString::new(lemma)),
        (TokenAttributeIndex::Pos, CompactString::new(pos)),
        (TokenAttributeIndex::CForm, CompactString::new(cform)),
    ])
}

/// Compiles `source` and publishes every record it produces over `tokens`.
fn publish(source: &str, tokens: &[Token]) -> (Vec<PublishedRecord>, PolicyStats) {
    let output = compile_catalog(
        PatternSource::named("<test>", source),
        CompileLimits::default(),
    )
    .expect("the catalog compiles");
    let policy: &CatalogPolicy = output.matcher.catalog_policy();
    let matches = output.matcher.find_matches(tokens);
    let mut stats = PolicyStats::default();
    let anchors = output.matcher.anchor_positions(tokens);
    let mut program = ProgramStats::for_program(policy.program());
    let records = apply_policy_records(
        policy,
        &matches,
        tokens,
        &anchors,
        &mut stats,
        &mut program,
        None,
    )
    .expect("the positions come from this catalog's own compilation");
    (records, stats)
}

/// A catalog whose single pattern covers the supplied sentence exactly, so the
/// transform under test sees the whole span and produces one record.
fn whole_sentence(policy_lines: &str) -> String {
    format!("{policy_lines}pattern unit := ^ []+ $.\n")
}

/// The unit the sole published record rendered.
fn unit(source: &str, tokens: &[Token]) -> Option<String> {
    let (records, _) = publish(source, tokens);
    let [record] = records.as_slice() else {
        panic!(
            "expected exactly one published record, got {}",
            records.len()
        );
    };
    record.unit.clone()
}

/// The guard vocabulary P1 declared before inspecting any tokenization, quoted
/// verbatim so the rows below cannot drift into guards fitted to their answers.
const DROP_TRAILING: &str = r#""[upos in {AUX, ADP}]""#;
const DROP_LEADING: &str = r#""[upos in {CCONJ, ADP, SCONJ, PART}]""#;
const KEEP_VOICE_OR_SURU: &str = r#""[lemma in {れる, られる, 為る}]""#;
const STOP_AT_SCONJ: &str = r#""[upos=SCONJ]""#;
/// natsume's `simple_lemma` mapping.
const SURU_TABLE: &str = r#"{為る: "する"}"#;

#[test]
fn p1_row1_drops_the_trailing_auxiliary() {
    let tokens = [
        token("飛び立つ", "飛び立つ", "VERB", "終止形-一般"),
        token("でしょう", "です", "AUX", "意志推量形"),
    ];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING}}}, {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("飛び立つ"));
}

#[test]
fn p1_row2_keeps_the_voice_auxiliary_the_drop_rule_would_have_taken() {
    // The keep guard is the whole row: without it the scan would walk past られ
    // and publish 考え, losing the passive that natsume preserves.
    let tokens = [
        token("考え", "考える", "VERB", "未然形-一般"),
        token("られ", "られる", "AUX", "連用形-一般"),
        token("ませ", "ます", "AUX", "未然形-一般"),
        token("ん", "ず", "AUX", "終止形-撥音便"),
        token("でし", "です", "AUX", "連用形-一般"),
        token("た", "た", "AUX", "終止形-一般"),
    ];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING}, keep: \
         {KEEP_VOICE_OR_SURU}}}, {{op: \"substitute\", component: \"last\", attr: \"lemma\", table: \
         {SURU_TABLE}}}, {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("考えられる"));
}

#[test]
fn p1_row3_drops_a_trailing_particle_and_copula() {
    let tokens = [
        token("扱う", "扱う", "VERB", "終止形-一般"),
        token("か", "か", "ADP", "*"),
        token("です", "です", "AUX", "終止形-一般"),
    ];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING}}}, {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("扱う"));
}

#[test]
fn p1_row4_substitutes_the_light_verb_rather_than_inserting_one() {
    // This row is why `insert` ships with no witness: UniDic-CWJ keeps し as
    // its own token with lemma 為る, so the サ変 repair natsume performs by
    // appending する is a substitution here.
    let tokens = [
        token("突入", "突入", "VERB", "*"),
        token("し", "為る", "AUX", "連用形-一般"),
        token("ちゃう", "ちゃう", "AUX", "終止形-一般"),
    ];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"drop_edge\", edge: \"trailing\", drop: {DROP_TRAILING}, keep: \
         {KEEP_VOICE_OR_SURU}}}, {{op: \"substitute\", component: \"last\", attr: \"lemma\", table: \
         {SURU_TABLE}}}, {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("突入する"));
}

#[test]
fn p1_row5_runs_all_three_span_operations_in_declaration_order() {
    let tokens = [
        token("で", "で", "CCONJ", "*"),
        token("囲ん", "囲む", "VERB", "連用形-撥音便"),
        token("で", "て", "SCONJ", "*"),
        token("ある", "有る", "VERB", "終止形-一般"),
    ];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"drop_edge\", edge: \"leading\", drop: {DROP_LEADING}}}, {{op: \
         \"truncate_before\", stop: {STOP_AT_SCONJ}}}, {{op: \"substitute\", component: \"last\", attr: \
         \"lemma\", table: {SURU_TABLE}}}, {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("囲む"));
}

#[test]
fn p1_row6_truncates_without_any_edge_drop() {
    let tokens = [
        token("たら", "たり-断定", "AUX", "未然形-一般"),
        token("しめ", "しめる", "AUX", "連用形-一般"),
        token("て", "て", "SCONJ", "*"),
        token("いる", "居る", "VERB", "終止形-一般"),
    ];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"truncate_before\", stop: {STOP_AT_SCONJ}}}, {{op: \"substitute\", \
         component: \"last\", attr: \"lemma\", table: {SURU_TABLE}}}, {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("たらしめる"));
}

#[test]
fn a_guard_sees_the_span_earlier_operations_left_not_the_raw_one() {
    // Guards read current transformed state. Both operations name `pos=AUX`,
    // and the AUX sits at the **start**: the leading drop removes it, so the
    // truncate that follows finds no stop token and leaves the span alone.
    //
    // The leading edge is what makes this discriminating. With the AUX trailing,
    // a truncate evaluated against the raw span would cut to exactly where the
    // drop had already put the end, and the two readings would agree — which is
    // what the first version of this test did, and it passed under a deliberate
    // mutation that read the raw span. Here the raw reading cuts to zero and
    // renders an empty unit.
    let tokens = [
        token("です", "です", "AUX", "終止形-一般"),
        token("読む", "読む", "VERB", "終止形-一般"),
        token("本", "本", "NOUN", "*"),
    ];
    let source = whole_sentence(
        "  transform: [{op: \"drop_edge\", edge: \"leading\", drop: \"[upos=AUX]\"}, {op: \
         \"truncate_before\", stop: \"[upos=AUX]\"}, {op: \"join\"}]\n",
    );
    assert_eq!(unit(&source, &tokens).as_deref(), Some("読む本"));
}

#[test]
fn the_same_operation_may_appear_twice_and_each_entry_runs_once() {
    // Each declared operation runs once, which is about list
    // entries. Two drops of one token each are not the same as one greedy drop:
    // this list removes exactly two trailing tokens even though a third would
    // also satisfy the guard.
    let tokens = [
        token("読む", "読む", "VERB", "終止形-一般"),
        token("か", "か", "ADP", "*"),
        token("です", "です", "AUX", "終止形-一般"),
        token("ね", "ね", "ADP", "*"),
    ];
    let single = r#""[upos=ADP]""#;
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"drop_edge\", edge: \"trailing\", drop: {single}}}, {{op: \"drop_edge\", edge: \
         \"trailing\", drop: {single}}}, {{op: \"join\"}}]\n"
    ));
    // First drop takes ね and stops at です; second finds です is not ADP and
    // stops immediately. The middle token survives, which a fixpoint would not
    // have allowed.
    assert_eq!(unit(&source, &tokens).as_deref(), Some("読むかです"));
}

#[test]
fn a_span_with_no_stop_token_is_left_alone_by_truncate() {
    let tokens = [
        token("読む", "読む", "VERB", "終止形-一般"),
        token("本", "本", "NOUN", "*"),
    ];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"truncate_before\", stop: {STOP_AT_SCONJ}}}, {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("読む本"));
}

#[test]
fn a_substitution_over_an_absent_attribute_leaves_the_token_written_as_it_is() {
    // Absence is normal, not exceptional. Blanking the surface would silently
    // shorten the unit, which is the failure mode no output could be used to
    // detect.
    let tokens = [Token(vec![
        (TokenAttributeIndex::Orth, CompactString::new("々")),
        (TokenAttributeIndex::Pos, CompactString::new("NOUN")),
    ])];
    let source = whole_sentence(
        "  transform: [{op: \"substitute\", component: \"last\", attr: \"lemma\"}, {op: \"join\"}]\n",
    );
    assert_eq!(unit(&source, &tokens).as_deref(), Some("々"));
}

#[test]
fn a_value_absent_from_the_table_passes_through_as_the_attribute_read_it() {
    let tokens = [token("囲ん", "囲む", "VERB", "連用形-撥音便")];
    let source = whole_sentence(&format!(
        "  transform: [{{op: \"substitute\", component: \"first\", attr: \"lemma\", table: {SURU_TABLE}}}, \
         {{op: \"join\"}}]\n"
    ));
    assert_eq!(unit(&source, &tokens).as_deref(), Some("囲む"));
}

#[test]
fn an_override_whose_token_a_later_drop_removed_does_not_survive_into_the_unit() {
    // `[substitute, drop_edge]` is authorable, so an orphaned override is
    // reachable. Overrides are sentence-absolute indices in the same coordinate
    // space as spans, and are dropped by the same rule that drops a capture.
    let tokens = [
        token("読む", "読む", "VERB", "終止形-一般"),
        token("です", "です", "AUX", "終止形-一般"),
    ];
    let source = whole_sentence(
        "  transform: [{op: \"substitute\", component: \"last\", attr: \"lemma\"}, {op: \"drop_edge\", edge: \
         \"trailing\", drop: \"[upos=AUX]\"}, {op: \"join\"}]\n",
    );
    let (records, _) = publish(&source, &tokens);
    let [record] = records.as_slice() else {
        panic!("one record");
    };
    assert_eq!(record.unit.as_deref(), Some("読む"));
    assert!(
        record.overrides.is_empty(),
        "the override on the dropped token should be gone, not retargeted: {:?}",
        record.overrides
    );
}

#[test]
fn a_transform_leaves_the_raw_match_untouched_so_ordering_cannot_move() {
    // The published span narrows; `raw` is what emission order and
    // `--raw-matches` diffing key on, and it must still describe the canonical
    // match.
    let tokens = [
        token("読む", "読む", "VERB", "終止形-一般"),
        token("です", "です", "AUX", "終止形-一般"),
    ];
    let source = whole_sentence(
        "  transform: [{op: \"drop_edge\", edge: \"trailing\", drop: \"[upos=AUX]\"}, {op: \"join\"}]\n",
    );
    let (records, _) = publish(&source, &tokens);
    let [record] = records.as_slice() else {
        panic!("one record");
    };
    assert_eq!((record.raw.start, record.raw.end), (0, 2));
    assert_eq!((record.start, record.end), (0, 1));
    assert_eq!(record.raw.pattern.as_str(), "unit");
}

#[test]
fn a_key_projects_off_the_captures_that_survived_the_span_rewrite() {
    let tokens = [
        token("実験", "実験", "NOUN", "*"),
        token("を", "を", "ADP", "*"),
        token("行っ", "行う", "VERB", "連用形-促音便"),
        token("た", "た", "AUX", "終止形-一般"),
    ];
    let source = "key: [$noun.lemma, $verb.lemma]\ntransform: [{op: \"drop_edge\", edge: \"trailing\", drop: \"[upos=AUX]\"}, {op: \"join\"}]\npattern collocation := ^ (?<noun> [upos=NOUN]) [upos=ADP] (?<verb> [upos=VERB]) [upos=AUX] $.\n";
    let (records, stats) = publish(source, &tokens);
    let [record] = records.as_slice() else {
        panic!("one record");
    };
    assert_eq!(
        record.key,
        Some(vec![
            ProjectedKeyField {
                name: "$noun.lemma".into(),
                value: "実験".to_owned(),
            },
            ProjectedKeyField {
                name: "$verb.lemma".into(),
                value: "行う".to_owned(),
            },
        ])
    );
    assert_eq!(record.unit.as_deref(), Some("実験を行っ"));
    assert_eq!(stats.records_unkeyed, 0);
}

#[test]
fn a_key_naming_a_capture_the_span_rewrite_dropped_unkeys_the_record() {
    // The three unkeyed causes have one disposition and one counter. This is
    // the span-rewrite one: `$tail` matched, and then left the published span.
    let tokens = [
        token("読む", "読む", "VERB", "終止形-一般"),
        token("です", "です", "AUX", "終止形-一般"),
    ];
    let source = "key: [$tail.lemma]\ntransform: [{op: \"drop_edge\", edge: \"trailing\", drop: \"[upos=AUX]\"}]\npattern p := ^ [upos=VERB] (?<tail> [upos=AUX]) $.\n";
    let (records, stats) = publish(source, &tokens);
    let [record] = records.as_slice() else {
        panic!("one record");
    };
    assert_eq!(record.key, None);
    assert!(record.components.is_empty());
    assert_eq!(stats.records_unkeyed, 1);
    assert_eq!(stats.captures_dropped, 1);
}

#[test]
fn a_key_projecting_an_attribute_the_token_does_not_carry_unkeys_the_record() {
    // The third cause, and the one an implementation is most likely to get
    // wrong: `unwrap_or_default()` here would produce an empty field that
    // groups unrelated records together in `summarize`.
    let tokens = [Token(vec![
        (TokenAttributeIndex::Orth, CompactString::new("々")),
        (TokenAttributeIndex::Pos, CompactString::new("NOUN")),
    ])];
    let source = "key: [$only.lemma]\npattern p := ^ (?<only> [upos=NOUN]) $.\n";
    let (records, stats) = publish(source, &tokens);
    let [record] = records.as_slice() else {
        panic!("one record");
    };
    assert_eq!(record.key, None);
    assert_eq!(stats.records_unkeyed, 1);
}

#[test]
fn tag_form_a_puts_the_same_value_on_every_record() {
    let tokens = [token("読む", "読む", "VERB", "終止形-一般")];
    let source = whole_sentence("  tags: {source: \"natsume\"}\n");
    let (records, _) = publish(&source, &tokens);
    assert_eq!(
        records[0].tags,
        vec![(CompactString::new("source"), "natsume".to_owned())]
    );
}

#[test]
fn tag_form_b_reports_which_capture_participated() {
    let tokens = [
        token("読ま", "読む", "VERB", "未然形-一般"),
        token("れる", "れる", "AUX", "終止形-一般"),
    ];
    let source = "tags: {voice: {passive: \"passive\", plain: \"active\"}}\npattern p := ^ [upos=VERB] ((?<passive> [lemma=れる]) | (?<plain> [lemma=た])) $.\n";
    let (records, _) = publish(source, &tokens);
    assert_eq!(
        records[0].tags,
        vec![(CompactString::new("voice"), "passive".to_owned())]
    );
}

#[test]
fn tag_form_b_omits_the_tag_when_no_named_capture_participated() {
    // An absent source is an absent tag, not an empty one: an empty string is a
    // value, and would compare equal across unrelated records.
    let tokens = [token("読む", "読む", "VERB", "終止形-一般")];
    let source = "tags: {voice: {passive: \"passive\"}}\npattern p := ^ [upos=VERB] $.\n";
    let (records, _) = publish(source, &tokens);
    assert!(records[0].tags.is_empty());
}

#[test]
fn tag_form_b_omits_the_tag_when_several_named_captures_participated() {
    // Zero and two-or-more get the same disposition, deliberately. Resolving an
    // ambiguity by declaration order would invent a winner out of what is really
    // an authoring mistake — and under a map-valued metadata key, "declaration
    // order" is not even stable across builds (see `PatternPolicy::key`).
    let tokens = [
        token("読ま", "読む", "VERB", "未然形-一般"),
        token("れる", "れる", "AUX", "終止形-一般"),
    ];
    let source = "tags: {voice: {verb: \"verbal\", aux: \"auxiliary\"}}\npattern p := ^ (?<verb> [upos=VERB]) (?<aux> [upos=AUX]) $.\n";
    let (records, _) = publish(source, &tokens);
    assert!(
        records[0].tags.is_empty(),
        "two participating captures is ambiguous, not a vote: {:?}",
        records[0].tags
    );
}

#[test]
fn a_second_substitution_on_one_token_replaces_the_first_rather_than_stacking() {
    // Repeated ops are legal, so two substitutions targeting the same edge are
    // authorable. Each writes the token's rendered surface, so the later one
    // wins; appending would render both and produce a unit that is a word
    // followed by another word.
    let tokens = [token("囲ん", "囲む", "VERB", "連用形-撥音便")];
    let source = whole_sentence(
        "  transform: [{op: \"substitute\", component: \"last\", attr: \"lemma\"}, {op: \"substitute\", \
         component: \"last\", attr: \"text\"}, {op: \"join\"}]\n",
    );
    assert_eq!(unit(&source, &tokens).as_deref(), Some("囲ん"));
}

#[test]
fn tag_form_c_projects_an_attribute_off_a_named_capture() {
    let tokens = [
        token("実験", "実験", "NOUN", "*"),
        token("を", "を", "ADP", "*"),
    ];
    let source = "tags: {head_lemma: {$head: \"lemma\"}}\npattern p := ^ (?<head> [upos=NOUN]) [upos=ADP] $.\n";
    let (records, _) = publish(source, &tokens);
    assert_eq!(
        records[0].tags,
        vec![(CompactString::new("head_lemma"), "実験".to_owned())]
    );
}

#[test]
fn tag_form_c_omits_the_tag_when_the_token_lacks_the_attribute() {
    // The tag analogue of the third unkeyed cause, and deliberately a
    // *different* disposition: a key is an identity, so a missing field
    // invalidates the record; a tag is an annotation, so a missing source is
    // simply no annotation.
    let tokens = [Token(vec![
        (TokenAttributeIndex::Orth, CompactString::new("々")),
        (TokenAttributeIndex::Pos, CompactString::new("NOUN")),
    ])];
    let source =
        "tags: {head_lemma: {$head: \"lemma\"}}\npattern p := ^ (?<head> [upos=NOUN]) $.\n";
    let (records, stats) = publish(source, &tokens);
    assert!(records[0].tags.is_empty());
    // No record was unkeyed: nothing declared a key.
    assert_eq!(stats.records_unkeyed, 0);
}

#[test]
fn transforms_run_before_relabeling_so_policy_is_looked_up_by_the_own_name() {
    // `output_name` is applied last, so
    // a definition publishing under another name still runs its own transform.
    let tokens = [
        token("読む", "読む", "VERB", "終止形-一般"),
        token("です", "です", "AUX", "終止形-一般"),
    ];
    let source = "output_name: \"predicate\"\ntransform: [{op: \"drop_edge\", edge: \"trailing\", drop: \"[upos=AUX]\"}, {op: \"join\"}]\npattern verb_phrase := ^ []+ $.\n";
    let (records, _) = publish(source, &tokens);
    assert_eq!(records[0].name.as_str(), "predicate");
    assert_eq!(records[0].unit.as_deref(), Some("読む"));
}

// ---------------------------------------------------------------------------
// Capture-scoped transforms.
//
// The witness is natsume-simple's NPV extraction, which wants three fields
// from one record: the noun, the case particle, and a *normalized* verb. The
// verb needs the transform vocabulary and the other two must survive it.
// Measured over 946 NPV records on a 600-sentence GiNZA CoNLL-U corpus before
// scoping existed: 384 unkeyed, 40.6%, `captures_dropped: 384` — one dropped
// capture per unkeyed record. The two tests below are that measurement in
// miniature, with the control beside it.
// ---------------------------------------------------------------------------

/// 規則 に 基づい て いる 。 — a real UniDic-CWJ tokenization, transcribed.
fn kisoku_ni_motozuku() -> Vec<Token> {
    vec![
        token("規則", "規則", "NOUN", "*"),
        token("に", "に", "ADP", "*"),
        token("基づい", "基づく", "VERB", "連用形-イ音便"),
        token("て", "て", "SCONJ", "*"),
        token("いる", "居る", "AUX", "終止形-一般"),
        token("。", "。", "PUNCT", "*"),
    ]
}

/// 問題 を 検討 し まし た 。 — the サ変 case, where the normalized verb is
/// two tokens and the last one substitutes.
fn mondai_wo_kentou_suru() -> Vec<Token> {
    vec![
        token("問題", "問題", "NOUN", "*"),
        token("を", "を", "ADP", "*"),
        token("検討", "検討", "VERB", "*"),
        token("し", "為る", "AUX", "連用形-一般"),
        token("まし", "ます", "AUX", "連用形-一般"),
        token("た", "た", "AUX", "終止形-一般"),
        token("。", "。", "PUNCT", "*"),
    ]
}

/// The NPV catalog, with every transform op confined to the verb capture.
const NPV_SCOPED: &str = r#"key: [$n.lemma, $p.lemma, $vb.span.text]
transform: [
  {op: "truncate_before", scope: "$vb", stop: "[upos=SCONJ]"},
  {op: "drop_edge", scope: "$vb", edge: "trailing", drop: "[upos in {AUX, ADP, PUNCT}]", keep: "[lemma in {れる, られる, 為る}]"},
  {op: "substitute", scope: "$vb", component: "last", attr: "lemma", table: {為る: "する"}},
  {op: "join", scope: "$vb"}
]
pattern npv := ^ (?<n> [upos in {NOUN, PROPN, PRON, NUM}]) (?<p> [upos=ADP]) (?<vb> []+) $.
"#;

/// The same catalog with the scopes removed — the shape that was expressible
/// before, and the defect.
const NPV_UNSCOPED: &str = r#"key: [$n.lemma, $p.lemma, $vb.span.text]
transform: [
  {op: "truncate_before", stop: "[upos=SCONJ]"},
  {op: "drop_edge", edge: "trailing", drop: "[upos in {AUX, ADP, PUNCT}]", keep: "[lemma in {れる, られる, 為る}]"},
  {op: "substitute", component: "last", attr: "lemma", table: {為る: "する"}},
  {op: "join"}
]
pattern npv := ^ (?<n> [upos in {NOUN, PROPN, PRON, NUM}]) (?<p> [upos=ADP]) (?<vb> []+) $.
"#;

fn key_values(record: &PublishedRecord) -> Vec<String> {
    record
        .key
        .as_ref()
        .expect("the record is keyed")
        .iter()
        .map(|field| field.value.clone())
        .collect()
}

#[test]
fn a_scoped_transform_rewrites_its_capture_and_leaves_the_key_projectable() {
    for (tokens, expected) in [
        (kisoku_ni_motozuku(), ["規則", "に", "基づく"]),
        (mondai_wo_kentou_suru(), ["問題", "を", "検討する"]),
    ] {
        let (records, stats) = publish(NPV_SCOPED, &tokens);
        let [record] = records.as_slice() else {
            panic!("one record, got {records:?}");
        };
        // The record still spans the whole sentence: a scoped step rewrites
        // its capture, not the record.
        assert_eq!((record.start, record.end), (0, tokens.len()));
        assert_eq!(record.components.len(), 3);
        assert_eq!(key_values(record), expected);
        // The unit is the verb the scoped `join` rendered, not the sentence.
        assert_eq!(record.unit.as_deref(), Some(expected[2]));
        assert_eq!(stats.records_unkeyed, 0);
        assert_eq!(stats.captures_dropped, 0);
    }
}

/// The control, and the reason the extension exists rather than a wider `key:`
/// rule: unscoped, the same op list narrows the *record*, `narrow_to_span`
/// discards the two captures that fell outside it, and the key cannot project.
#[test]
fn the_unscoped_form_drops_the_captures_its_own_key_needs() {
    let tokens = kisoku_ni_motozuku();
    let (records, stats) = publish(NPV_UNSCOPED, &tokens);
    let [record] = records.as_slice() else {
        panic!("one record, got {records:?}");
    };
    assert_eq!(record.key, None);
    assert_eq!(stats.records_unkeyed, 1);
    // `$vb` is the capture the transform was written for, and it is the one
    // the narrowing removes: `truncate_before` cut the record at the SCONJ,
    // and `$vb` reached past it.
    assert_eq!(stats.captures_dropped, 1);
    assert!(
        !record.components.iter().any(|capture| capture.name == "vb"),
        "{:?}",
        record.components
    );
    // And the second half of the defect: `join` renders the record span, so
    // the unit is the noun phrase and the verb together rather than the verb
    // the author asked for.
    assert_eq!(record.unit.as_deref(), Some("規則に基づく"));
}

/// The isolating control the diagnosis rested on: the same key over the same
/// tokens projects fine when nothing is transformed, so multi-token key
/// projection was never the defect.
#[test]
fn the_same_key_projects_without_a_transform() {
    let tokens = kisoku_ni_motozuku();
    let source = "key: [$n.lemma, $p.lemma, $vb.span.text]\npattern npv := ^ (?<n> [upos in {NOUN, PROPN, PRON, NUM}]) (?<p> [upos=ADP]) (?<vb> []+) $.\n";
    let (records, stats) = publish(source, &tokens);
    assert_eq!(key_values(&records[0]), ["規則", "に", "基づいている。"]);
    assert_eq!(stats.records_unkeyed, 0);
}

/// A substitution is a rewrite of the written form, so the key reads it — and
/// reads it only there. `$vb.span.lemma` still reports what the tokens carry.
#[test]
fn a_key_reads_substitutions_for_the_written_form_and_not_for_other_attributes() {
    let tokens = mondai_wo_kentou_suru();
    let scoped_ops = r#"transform: [
  {op: "drop_edge", scope: "$vb", edge: "trailing", drop: "[upos in {AUX, ADP, PUNCT}]", keep: "[lemma in {れる, られる, 為る}]"},
  {op: "substitute", scope: "$vb", component: "last", attr: "lemma", table: {為る: "する"}},
  {op: "join", scope: "$vb"}
]
pattern npv := ^ (?<n> [upos in {NOUN, PROPN, PRON, NUM}]) (?<p> [upos=ADP]) (?<vb> []+) $.
"#;
    let written = format!("key: [$vb.span.text]\n{scoped_ops}");
    let (records, _) = publish(&written, &tokens);
    assert_eq!(key_values(&records[0]), ["検討する"]);

    let lemmas = format!("key: [$vb.span.lemma]\n{scoped_ops}");
    let (records, _) = publish(&lemmas, &tokens);
    assert_eq!(key_values(&records[0]), ["検討為る"]);
}

/// A scope naming a capture this match did not bind does nothing, rather than
/// silently falling back to the record. An alternation branch that did not
/// fire is the ordinary way to reach this.
#[test]
fn a_scope_whose_capture_did_not_bind_leaves_the_record_alone() {
    let tokens = [
        token("読む", "読む", "VERB", "終止形-一般"),
        token("です", "です", "AUX", "終止形-一般"),
    ];
    let source = "transform: [{op: \"drop_edge\", scope: \"$absent\", edge: \"trailing\", drop: \"[upos=AUX]\"}, {op: \"join\"}]\npattern p := ^ ((?<absent> [upos=NOUN]) | [upos=VERB]) [upos=AUX] $.\n";
    let (records, _) = publish(source, &tokens);
    let [record] = records.as_slice() else {
        panic!("one record");
    };
    assert_eq!((record.start, record.end), (0, 2));
    assert_eq!(record.unit.as_deref(), Some("読むです"));
}

/// A `substitute` with no `join` is not provably inert, because a `key:`
/// projecting the written form reads the overrides too. The refusal narrows to
/// the case where nothing reads them at all.
#[test]
fn a_substitute_is_observable_through_a_key_as_well_as_a_join() {
    let tokens = [token("し", "為る", "AUX", "連用形-一般")];
    let keyed = "key: [$only.text]\ntransform: [{op: \"substitute\", scope: \"$only\", component: \"last\", attr: \"lemma\", table: {為る: \"する\"}}]\npattern p := ^ (?<only> []) $.\n";
    let (records, stats) = publish(keyed, &tokens);
    assert_eq!(key_values(&records[0]), ["する"]);
    assert_eq!(stats.records_unkeyed, 0);

    let inert = "transform: [{op: \"substitute\", component: \"last\", attr: \"lemma\", table: {為る: \"する\"}}]\npattern p := ^ [] $.\n";
    let Err(error) = compile_catalog(
        PatternSource::named("<test>", inert),
        CompileLimits::default(),
    ) else {
        panic!("a substitution nothing reads is still refused");
    };
    assert!(
        error.to_string().contains("would change nothing"),
        "{error}"
    );
}

/// Shape and reference are checked in two different passes, so both refusals
/// have to be pinned: a scope that is not a `$` reference at all, and one that
/// is but names no capture.
#[test]
fn a_malformed_or_undefined_scope_is_refused() {
    for (source, expected) in [
        (
            "transform: [{op: \"join\", scope: \"vb\"}]\npattern p := ^ (?<vb> []) $.\n",
            "not a capture reference",
        ),
        (
            "transform: [{op: \"join\", scope: \"$nope\"}]\npattern p := ^ (?<vb> []) $.\n",
            "not a capture in this pattern",
        ),
    ] {
        let Err(error) = compile_catalog(
            PatternSource::named("<test>", source),
            CompileLimits::default(),
        ) else {
            panic!("the scope must be refused: {source}");
        };
        assert!(error.to_string().contains(expected), "{error}");
    }
}
