// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! The cascade-vs-resolve differential verdict for `patterns/double-wo.lx`
//! (Task A3, 2026-08-14): the `not_followed_by` cascade is **not** expressible
//! as a publication-seam `resolve`, and this module pins why with the minimal
//! witnesses the corpus differential surfaced.
//!
//! The hypothesis was plausible because the catalog's buckets are
//! endpoint-aligned *at one start position*: each bucket fixes its evidence
//! before the variable gap and keeps the same raw suffix language, so at one
//! start every bucket that matches matches the same span, and the cascade's
//! suppression looks like a same-span priority contest — exactly what
//! `resolve … rank: [longest, priority_order, …]` adjudicates.
//!
//! The two are nonetheless different operations over different scopes:
//!
//! - the cascade's `not_followed_by(higher bodies)` fires at the **start
//!   position** — a lower bucket is suppressed only where a higher body
//!   matches at the *same start*, and matches at different starts never see
//!   each other;
//! - `resolve` is a greedy **overlap contest** over the whole
//!   sentence — two records at *different starts* whose spans overlap
//!   contest, and exactly one survives.
//!
//! On corpus-2000 the resolve form dropped 7 of the cascade's 626 records; on
//! the science artifact, 35 of 2,542; on BCCWJ (read as direct plaintext), 41
//! of 3,203. Every dropped record was a cross-start
//! overlap loss and none was a same-start case — see
//! `docs/results/2026-08-14-double-wo-resolve-migration/README.md`. The
//! catalog therefore keeps its cascade, and the resolve form is retained
//! below as a string constant the way `intersection_test.rs` keeps the wrong
//! rewrite beside the right one: so the non-equivalence stays checkable
//! in-tree, and so a future attempt to migrate starts from the witnesses that
//! refused this one.

use lingrep_core::fsm::{CompileLimits, CompiledCatalog, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

/// The tracked catalog: match-time classification via the quadratic
/// `not_followed_by` cascade. No policy block, so its published stream is its
/// canonical stream.
const DOUBLE_WO_CASCADE: &str = include_str!("../../../patterns/double-wo.lx");

/// The refuted experiment variant (`patterns/double-wo-resolve.lx` while it
/// existed): every cascade prelude removed, bucket priority restated as
/// `priority_order` metadata, and one `resolve` at the publication seam.
/// Same fragments, same raw languages, different publication semantics —
/// as of 2026-08-14, when this constant was frozen from the live catalog.
/// `both_forms_declare_the_same_buckets` is the structural tie that keeps
/// that parity claim honest as the catalog evolves.
const DOUBLE_WO_RESOLVE: &str = r#"
defaults
  source: "research query, 2重を格 exclusion classification"
  status: "experimental"
end

# Double-を-like exclusion classification.
#
# The classified value is the complete canonical (start, end) match. Each
# public pattern below adds evidence fixed before the variable gap while
# retaining the raw suffix language. This keeps independently greedy lingrep patterns
# patterns aligned on the same endpoint.
#
# These names record evidence for automatic exclusion from the primary
# candidate set. They are not final dependency or construction analyses.
# Evidence tied only to the second anchor or the variable tail remains in
# `double_wo_candidate`, because constraining it can change the greedy endpoint.
#
# Tagset note (unidic-cwj / UniDic-CWJ):
#   `pos1=助詞 & unidic.pos2=格助詞` identifies the two を particles. Verbs,
#   auxiliaries, and punctuation use pos1=動詞/助動詞/補助記号.
#
# BCCWJ classified baseline before the fragment-boundary category
# (2026-07-30; UniDic-CWJ 202512):
#   exclude_adverbial_anchor: 1,080
#   exclude_focus_marker: 34
#   exclude_lexicalized_anchor: 24
#   exclude_directional_frame: 29
#   exclude_malformed_first_anchor: 18
#   candidate: 2,001
#   total: 3,186 (former filtered baseline: 2,646)
# These are classifier-output counts, not confirmed linguistic constructions.

# Everything the gap and anchor predicates exclude. Nine sites spelled this
# list before it had a name.
set DOUBLE_WO_NON_CONTENT := {動詞, 助動詞, 補助記号}.

# Characterized BCCWJ anchor counts from the former broad result:
#   対象 116; 理由 90; はじめ 78; 基 44; 機 36; 目的 35;
#   めど/メド/目途 30; 参考 27; 前提 26; 背景 21; 基準 15;
#   頼り 12; 軸 11; 目標 10; 舞台 7.
set DOUBLE_WO_ADVERBIAL_ANCHORS := {
  対象, 理由, 基, 機, 機会, 目的, 前提, 参考, 背景, 基準, 目標,
  目途, めど, メド, 目安, 舞台, 境, 皮切り, 先頭, 合言葉,
  名目, 担保, 拠点, 武器, キーワード, 条件, ベース, バック,
  根拠, 口実, 手, 軸, 頼り, 材料, 原料, テコ, 梃子, ネタ,
  ヒント, モデル, 主体, 中心, テーマ, きっかけ, 契機, もと,
  巡, 通じ, 巡り
}.

role: "helper"
fragment double_wo_particle :=
  [unidic.orthbase=を & unidic.pos1=助詞 & unidic.pos2=格助詞].

role: "helper"
fragment double_wo_anchor :=
  [unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_gap :=
  [unidic.orthbase!=を & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

# The boundary is named once and used twice: as the positive test, and
# complemented as the gap's exclusion. Before `test` existed these were two
# independent definitions that had to agree, and they had already stopped
# agreeing — the boundary tested `orth` while its supposed complement
# excluded `orthBase`. Nothing reported the drift, because the relationship
# between the two was stated only in the prose below. Deriving one from the
# other is what makes that class of drift unrepresentable rather than merely
# unobserved.
test double_wo_boundary := text=\u3000.

role: "helper"
fragment double_wo_non_boundary_gap :=
  [unidic.orthbase!=を & !@double_wo_boundary
   & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_fragment_boundary :=
  [@double_wo_boundary].

role: "helper"
fragment double_wo_non_boundary_suffix :=
  (@double_wo_non_boundary_gap){0,8}
  @double_wo_particle
  @double_wo_anchor.

# A preserved ideographic space (U+3000) inside the variable gap is strong
# evidence that the two を phrases belong to separate list items, headlines,
# or sentence fragments. The body locates the first U+3000 while preserving
# the raw body's eight-token maximum gap and canonical endpoint.
#
# It says that in one line, using `&`. Until the operator existed it was
# eight alternatives, one per possible count of leading non-boundary gaps,
# because a bounded repetition does not distribute over a concatenation of
# variable-length parts: the eight-token limit had to be multiplied through
# the shape by hand. The branch count was C(n+v-1, v-1) for v variable-length
# segments and a bound of n, so v=2 and n=7 gave exactly the eight that were
# written here. One more starred segment would have needed thirty-six.
#
# `&` factors the two apart: the left operand says what the shape is, the
# right says how long it may be, and neither has to know about the other.
# The rewrite is exactly equivalent rather than equivalent-in-practice, and
# that distinction is not pedantry — the first attempt at it was *wrong*,
# admitting a U+3000 tagged as something other than 空白 before the boundary
# where the eight branches required a non-boundary gap. The witness is two
# tokens long and would very likely never have appeared in a corpus test.
# `intersection_test.rs` checks both languages exhaustively over the three
# classes these predicates distinguish, and keeps the wrong rewrite beside
# the right one so the check cannot quietly stop discriminating.
#
# The boundary tests `orth` alone, for two reasons.
#   * It is a surface fact. The evidence is that a U+3000 survived in the
#     text, which is what `orth` records. `orthBase` is a dictionary base
#     form; featurization now populates it for every token, but for an
#     unknown word its value is the surface synthesised from the token
#     rather than a dictionary entry — which is not what this test asks
#     about.
#   * Dropping the former `& unidic.pos1=空白` conjunct is what makes the
#     classification a partition again. While the boundary was a
#     *conjunction*, its negation was a disjunction, which lingrep patterns could not
#     then express inside `[…]`; a U+3000 tagged as anything but 空白
#     matched the raw language but no bucket. lingrep patterns can express it now:
#     `!@double_wo_boundary` complements the whole named test whatever
#     shape it has, so the partition no longer depends on the boundary
#     staying a single atom. It depends instead on the gap being *derived*
#     from the boundary rather than restating it — which is why
#     `test double_wo_boundary` above is referenced twice and spelled once.
#     Pinned by
#     `double_wo_classification_at_ideographic_space_not_tagged_blank`.
#
# `test double_wo_boundary` spells the character `\u3000` rather than
# containing it. U+3000 is invisible in a diff, in review and in a terminal,
# and one was silently dropped from a predicate in this file during an
# earlier edit. Naming the test is what reduced the two spellings that had
# to agree to the one spelling that now exists. The example string below
# keeps the raw character: it is corpus text the tokenizer must see.
role: "helper"
fragment double_wo_fragment_boundary_body :=
  @double_wo_particle
  @double_wo_anchor
  (
    (
      (@double_wo_non_boundary_gap)*
      @double_wo_fragment_boundary
      (@double_wo_gap)*
    ) & []{1,8}
  )
  @double_wo_particle
  @double_wo_anchor.

# First-anchor evidence for productive or grammaticalized adverbial frames.
# Counts are routing evidence, not confirmed construction counts. Only a
# listed first anchor enters this bucket.
role: "helper"
fragment double_wo_adverbial_anchor :=
  [unidic.orthbase in @DOUBLE_WO_ADVERBIAL_ANCHORS & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_adverbial_body :=
  @double_wo_particle
  @double_wo_adverbial_anchor
  @double_wo_non_boundary_suffix.

# First-token focus evidence. A focus marker after the second を is not
# endpoint-safe and therefore remains for review in the candidate bucket.
role: "helper"
fragment double_wo_focus_anchor :=
  [unidic.orthbase in {も, さえ}
   & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_focus_body :=
  @double_wo_particle
  @double_wo_focus_anchor
  @double_wo_non_boundary_suffix.

# First-anchor evidence for characterized fixed or figurative families:
#   首を長く/長〜く 12; 声を大に/大きく 4;
#   口を極めて/きわめて 4; 目を丸く/皿 3.
# Known collateral is retained under this evidence name:
#   literal 2 (首にする, 口にする); ambiguous 1.
role: "helper"
fragment double_wo_lexicalized_anchor :=
  [unidic.orthbase in {首, 声, 口, 目}
   & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_lexicalized_body :=
  @double_wo_particle
  @double_wo_lexicalized_anchor
  @double_wo_non_boundary_suffix.

# Positive directional/scalar frame evidence must immediately follow the first
# anchor. An adjective alone is not sufficient: BCCWJ sampling found unrelated
# modifier sequences such as `をいずれ近いうちに…`. The explicit `に` token is
# constrained to be a valid raw gap token, and the remaining bound is reduced
# to seven to preserve the raw eight-token gap.
#
# The `に` requirement is pinned by this bucket's counterexample, which is the
# minimal pair of its example: `を上へ左手を下` differs from `を上に左手を下`
# in one token and falls to `double_wo_candidate` instead. Constructed for
# that contrast, not sampled from BCCWJ — its value is that it reaches the raw
# language at all, which is what makes the exclusion evidence rather than
# vacuously true.
role: "helper"
fragment double_wo_directional_anchor :=
  [unidic.orthbase in {
    上, 下, 前, 後ろ, 高, 低, 大, 小, 厚, 薄, 長, 短, 強, 弱, 多, 少, 濃
  } & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_directional_ni :=
  [unidic.orthbase=に & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_directional_body :=
  @double_wo_particle
  @double_wo_directional_anchor
  @double_wo_directional_ni
  (@double_wo_non_boundary_gap){0,7}
  @double_wo_particle
  @double_wo_anchor.

# Structural noise formerly removed silently. Only first-anchor evidence is
# endpoint-safe; the same spellings at the second anchor remain candidates.
role: "helper"
fragment double_wo_malformed_anchor :=
  [unidic.orthbase in {を, り}
   & unidic.pos1 not in @DOUBLE_WO_NON_CONTENT].

role: "helper"
fragment double_wo_malformed_body :=
  @double_wo_particle
  @double_wo_malformed_anchor
  @double_wo_non_boundary_suffix.

# EXPERIMENT VARIANT (Task A3, 2026-08-14): the `not_followed_by` cascade
# preludes are removed and the bucket priority is restated as
# `priority_order` metadata adjudicated by the `resolve` in the policy block
# at the end of this file. Match-time classification becomes publication-time
# resolution; the differential against patterns/double-wo.lx decides whether
# the two are the same catalog.

# `priority_order: 0` here is irrelevant: this bucket's language is disjoint
# from every other bucket's by construction — its gap requires a U+3000
# boundary token, which every other bucket's gap forbids via
# `double_wo_non_boundary_suffix` / `double_wo_non_boundary_gap` — so at one
# start position it never ties with another bucket and the rank never reaches
# `priority_order` for it. It carries a value only because it is in the
# `%double_wo` group and the key must say something.
role: "research"
category: "重複を格調査"
subcategory: "断片境界による除外"
examples: ["を予防　高血圧を抑制"]
groups: ["double_wo"]
priority_order: 0
pattern double_wo_exclude_fragment_boundary :=
  @double_wo_fragment_boundary_body.

role: "research"
category: "重複を格調査"
subcategory: "副詞的アンカーによる除外"
examples: ["を対象にアンケート調査を実施"]
groups: ["double_wo"]
priority_order: 0
pattern double_wo_exclude_adverbial_anchor :=
  @double_wo_adverbial_body.

role: "research"
category: "重複を格調査"
subcategory: "係助詞による除外"
examples: ["をも世をも"]
groups: ["double_wo"]
priority_order: 1
pattern double_wo_exclude_focus_marker :=
  @double_wo_focus_body.

role: "research"
category: "重複を格調査"
subcategory: "語彙化アンカーによる除外"
examples: ["を首を長くして待つ"]
groups: ["double_wo"]
priority_order: 2
pattern double_wo_exclude_lexicalized_anchor :=
  @double_wo_lexicalized_body.

role: "research"
category: "重複を格調査"
subcategory: "方向・尺度フレームによる除外"
examples: ["を上に左手を下"]
counterexamples: ["を上へ左手を下"]
groups: ["double_wo"]
priority_order: 3
pattern double_wo_exclude_directional_frame :=
  @double_wo_directional_body.

role: "research"
category: "重複を格調査"
subcategory: "解析ノイズによる除外"
examples: ["をり列を項目"]
groups: ["double_wo"]
priority_order: 4
pattern double_wo_exclude_malformed_first_anchor :=
  @double_wo_malformed_body.

role: "research"
category: "重複を格調査"
subcategory: "未除外候補"
examples: ["氏名を住所を備考に入力した"]
groups: ["double_wo"]
priority_order: 5
pattern double_wo_candidate :=
  @double_wo_particle
  @double_wo_anchor
  @double_wo_non_boundary_suffix.

policy
  resolve %double_wo rank: [longest, priority_order, start, end, pattern].
end
"#;

fn compiled(name: &str, source: &str) -> CompiledCatalog {
    compile_catalog(PatternSource::named(name, source), CompileLimits::default())
        .expect("catalog must compile")
        .matcher
}

fn token(orth_base: &str, pos1: &str, pos2: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth_base.into());
    token.set(TokenAttributeIndex::OrthBase, orth_base.into());
    token.set(TokenAttributeIndex::Pos1, pos1.into());
    token.set(TokenAttributeIndex::Pos2, pos2.into());
    token
}

fn wo() -> TokenInfo {
    token("を", "助詞", "格助詞")
}

fn noun(orth: &str) -> TokenInfo {
    token(orth, "名詞", "普通名詞-一般")
}

/// The complete published `Match` stream. `single_start_witnesses_agree`
/// compares these values whole — captures and contexts included in equality,
/// per the testing guide — while the divergence tests project through
/// [`summarized`] to `(pattern, start, end)` for their primary assertions and
/// use complete-`Match` membership only to pin that the resolve invents
/// nothing. Neither form's policy synthesizes `__policy__` evidence emitters
/// (the resolve's one statement has no fragment operand), so no filtering is
/// needed on either side.
fn published(matcher: &CompiledCatalog, tokens: &[TokenInfo]) -> Vec<Match> {
    matcher
        .find_published_matches(tokens)
        .expect("no admit statement, so anchor resolution cannot fail")
}

fn summarized(matches: &[Match]) -> Vec<(String, usize, usize)> {
    matches
        .iter()
        .map(|matched| (matched.pattern.to_string(), matched.start, matched.end))
        .collect()
}

/// The corpus differential was one-sided on every corpus — the resolve form
/// never published a record the cascade did not (`invented: 0` in the results
/// doc). Assert the same of a witness: every resolve-published record equals,
/// as a complete `Match`, some cascade-published record.
fn assert_resolve_invents_nothing(cascade: &[Match], resolve: &[Match]) {
    for record in resolve {
        assert!(
            cascade.contains(record),
            "the resolve form must only drop records, never invent one: {record:?}"
        );
    }
}

/// The minimal divergence witness: three を anchors produce two raw matches at
/// *different starts* with overlapping spans. The cascade classifies each
/// start independently and publishes both; the resolve contest keeps the
/// longer record and drops the other.
#[test]
fn cross_start_overlap_diverges() {
    // Raw matches: candidate at (0,4) and adverbial (対象) at (2,7),
    // overlapping on tokens 2..4.
    let tokens = vec![
        wo(),
        noun("住所"),
        wo(),
        noun("対象"),
        noun("欄"),
        wo(),
        noun("備考"),
    ];

    let cascade = published(&compiled("double-wo-cascade", DOUBLE_WO_CASCADE), &tokens);
    let resolve = published(&compiled("double-wo-resolve", DOUBLE_WO_RESOLVE), &tokens);

    assert_eq!(
        summarized(&cascade),
        vec![
            ("double_wo_candidate".to_string(), 0, 4),
            ("double_wo_exclude_adverbial_anchor".to_string(), 2, 7),
        ],
        "the cascade classifies each start independently and publishes both"
    );
    assert_eq!(
        summarized(&resolve),
        vec![("double_wo_exclude_adverbial_anchor".to_string(), 2, 7)],
        "the resolve overlap contest keeps only the longest record"
    );
    assert_resolve_invents_nothing(&cascade, &resolve);
}

/// The sharper corollary: `longest` outranks `priority_order`, so a longer
/// record from a *lower*-priority bucket eliminates a higher-priority record
/// at an overlapping start. The resolve does not merely thin the cascade's
/// stream — it can invert the bucket precedence the cascade encodes. Mirrors
/// science-artifact sentence 613023 (malformed beats adverbial).
#[test]
fn cross_start_overlap_inverts_bucket_priority() {
    // Malformed (anchor を, priority 4) matches (0,6); adverbial (対象,
    // priority 0) matches (1,6). The cascade publishes both; the resolve
    // drops the *adverbial* because the malformed record is longer.
    let tokens = vec![wo(), wo(), noun("対象"), noun("欄"), wo(), noun("調査")];

    let cascade = published(&compiled("double-wo-cascade", DOUBLE_WO_CASCADE), &tokens);
    let resolve = published(&compiled("double-wo-resolve", DOUBLE_WO_RESOLVE), &tokens);

    assert_eq!(
        summarized(&cascade),
        vec![
            ("double_wo_exclude_malformed_first_anchor".to_string(), 0, 6),
            ("double_wo_exclude_adverbial_anchor".to_string(), 1, 6),
        ],
    );
    assert_eq!(
        summarized(&resolve),
        vec![("double_wo_exclude_malformed_first_anchor".to_string(), 0, 6)],
        "longest ranks before priority_order, so priority 4 eliminates priority 0"
    );
    assert_resolve_invents_nothing(&cascade, &resolve);
}

/// The structural tie between the frozen [`DOUBLE_WO_RESOLVE`] constant and
/// the live catalog: both compiled forms must declare exactly the same
/// pattern names. A bucket added to, removed from, or renamed in
/// `patterns/double-wo.lx` fails here rather than silently rotting the
/// constant while its doc comment claims present-tense parity; the fix is to
/// re-derive the constant and re-run the corpus differential, not to edit
/// this assertion.
#[test]
fn both_forms_declare_the_same_buckets() {
    let cascade = compiled("double-wo-cascade", DOUBLE_WO_CASCADE);
    let resolve = compiled("double-wo-resolve", DOUBLE_WO_RESOLVE);

    let names = |matcher: &CompiledCatalog| {
        matcher
            .patterns()
            .keys()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        names(&cascade),
        names(&resolve),
        "the frozen resolve constant no longer mirrors the live catalog's \
         buckets; re-derive it (2026-08-14 parity) and re-run the differential"
    );
}

/// Where the hypothesis was *right*: at a single start position the two forms
/// publish identical complete `Match` values, one witness per bucket. This is
/// the endpoint-alignment property the catalog header documents, and it is
/// why the corpus differential shows only cross-start losses.
#[test]
fn single_start_witnesses_agree() {
    let particle = |orth: &str| token(orth, "助詞", "係助詞");
    let cases: Vec<(&str, Vec<TokenInfo>)> = vec![
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
        ),
        (
            "fragment-boundary (U+3000 tagged 空白)",
            vec![
                wo(),
                noun("対象"),
                token("\u{3000}", "空白", "*"),
                noun("調査"),
                wo(),
                noun("実施"),
            ],
        ),
        (
            // The pinned witness of
            // `double_wo_classification_at_ideographic_space_not_tagged_blank`:
            // the boundary tests `orth` alone, so the tag does not matter.
            "fragment-boundary (U+3000 not tagged 空白)",
            vec![
                wo(),
                noun("氏名"),
                token("\u{3000}", "名詞", "普通名詞-一般"),
                noun("欄"),
                wo(),
                noun("住所"),
            ],
        ),
        (
            "focus",
            vec![wo(), particle("も"), noun("世"), wo(), noun("見る")],
        ),
        (
            "lexicalized",
            vec![wo(), noun("首"), noun("長く"), wo(), noun("待つ")],
        ),
        (
            "directional",
            vec![
                wo(),
                noun("上"),
                particle("に"),
                noun("左手"),
                wo(),
                noun("下"),
            ],
        ),
        (
            "malformed",
            vec![wo(), noun("り"), noun("列"), wo(), noun("項目")],
        ),
        (
            "candidate",
            vec![wo(), noun("住所"), noun("欄"), wo(), noun("備考")],
        ),
    ];

    let cascade = compiled("double-wo-cascade", DOUBLE_WO_CASCADE);
    let resolve = compiled("double-wo-resolve", DOUBLE_WO_RESOLVE);

    for (label, tokens) in cases {
        let from_cascade = published(&cascade, &tokens);
        let from_resolve = published(&resolve, &tokens);
        assert!(
            !from_cascade.is_empty(),
            "{label}: the witness must reach its bucket"
        );
        assert_eq!(
            from_cascade, from_resolve,
            "{label}: at a single start the two forms publish identical records"
        );
    }
}
