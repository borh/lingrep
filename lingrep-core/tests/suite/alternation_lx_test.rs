//! The distinctive-collexeme catalog, and what its three policy statements do
//! at the publication seam.
//!
//! `catalog_examples.rs` runs this catalog's `examples:` claims with every
//! other tracked catalog's, but those claims are about `find_matches` — the
//! RAW match set. This catalog's policy block runs afterwards, so the whole of
//! what it decides is invisible to an `examples:` annotation: the quotative
//! sentence that must not publish DOES match raw, and saying so in a
//! `counterexamples:` list would be a false claim about the language rather
//! than a true one about publication.
//!
//! Each test below therefore contrasts raw against published, which is the
//! only way to state "this record exists and is deliberately withheld".

use lingrep_core::fsm::{CompileLimits, CompiledCatalog, Match, PatternSource, compile_catalog};
use lingrep_core::testing::tokenize_japanese;

fn matcher() -> CompiledCatalog {
    compile_catalog(
        PatternSource::named(
            "patterns/alternation.lx",
            include_str!("../../../patterns/alternation.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("alternation lingrep patterns must compile strictly")
    .matcher
}

fn raw(matcher: &CompiledCatalog, sentence: &str) -> Vec<Match> {
    matcher.find_matches(&tokenize_japanese(sentence))
}

fn published(matcher: &CompiledCatalog, sentence: &str) -> Vec<Match> {
    matcher
        .find_published_matches(&tokenize_japanese(sentence))
        .expect("the catalog declares no anchor, so publication cannot fail on one")
}

fn names(records: &[Match]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record.pattern.as_str())
        .collect()
}

// `every_documented_alternation_claim_holds_against_the_dictionary` lived
// here, checking this catalog and no other. `catalog_examples.rs` now checks
// every tracked catalog through `verify_examples`; two implementations of one
// check drift.

/// Statement 1, within a family: a nested light-verb nominal resolves to the
/// longest.
///
/// 研究発表をする offers `lv_wo_suru` two nominals, because 研究 and 発表 are
/// both サ変可能. Publishing both counts one construction token twice and
/// splits its collexeme between two rows.
#[test]
fn a_nested_light_verb_nominal_publishes_only_the_longest() {
    let matcher = matcher();
    let sentence = "研究発表をする。";

    let raw_records = raw(&matcher, sentence);
    let raw_light_verb = names(&raw_records)
        .into_iter()
        .filter(|name| *name == "lv_wo_suru")
        .count();
    assert_eq!(
        raw_light_verb, 2,
        "the raw language must offer both nominals, or this test proves nothing"
    );

    let records = published(&matcher, sentence);
    assert_eq!(names(&records), ["lv_wo_suru"]);
    let record = &records[0];
    assert_eq!(
        (record.start, record.end),
        (0, 4),
        "the published record must cover 研究発表をする whole"
    );
}

/// Statement 1, across families: `partition: alternation` is what keeps a
/// negation observation from costing a conditional observation.
///
/// 実験を行わなければならない contains `neg_nai` over 行わ〜なけれ and
/// `cond_ba` over なけれ〜ば. They overlap on なけれ and are the same length,
/// so an UNPARTITIONED `resolve … rank: [longest, start, …]` would tie on
/// length, break the tie on start, and delete the conditional. Both must
/// survive: they are observations of different alternations that happen to
/// share a token.
#[test]
fn overlapping_records_from_different_families_both_publish() {
    let matcher = matcher();
    let records = published(&matcher, "実験を行わなければならない。");
    let published_names = names(&records);

    assert!(
        published_names.contains(&"neg_nai"),
        "the negation observation is missing: {published_names:?}"
    );
    assert!(
        published_names.contains(&"cond_ba"),
        "the conditional observation was deleted by a cross-family contest: {published_names:?}"
    );

    let span = |name: &str| {
        records
            .iter()
            .find(|record| record.pattern == name)
            .map_or_else(
                || panic!("{name} must publish"),
                |record| (record.start, record.end),
            )
    };
    let (negation_start, negation_end) = span("neg_nai");
    let (conditional_start, conditional_end) = span("cond_ba");
    assert!(
        negation_start < conditional_end && conditional_start < negation_end,
        "the two records must actually overlap, or the partition is untested: \
         {negation_start}..{negation_end} and {conditional_start}..{conditional_end}"
    );
}

/// Statement 2: a quotative と matches `cond_to` and must not publish.
///
/// UniDic tags the conditional と as 格助詞 after a 終止形 predicate — the
/// same tag the quotative carries — so the predicate cannot separate them and
/// the exclusion runs on adjacency instead. The raw assertion is the load
/// bearing half: it states that the record exists and is withheld, which is
/// the thing an `examples:` annotation cannot say.
#[test]
fn a_quotative_to_matches_but_does_not_publish() {
    let matcher = matcher();
    let quotative = "彼はそういうことができると言った。";

    assert!(
        names(&raw(&matcher, quotative)).contains(&"cond_to"),
        "the raw language must reach the quotative, or the exclusion is untested"
    );
    assert!(
        !names(&published(&matcher, quotative)).contains(&"cond_to"),
        "a quotative と published as a conditional: {:?}",
        names(&published(&matcher, quotative))
    );

    // The same shape with a non-quotative main verb is kept, so the statement
    // is an exclusion rather than a blanket refusal of と.
    assert!(
        names(&published(&matcher, "研究を進めると分かる。")).contains(&"cond_to"),
        "an ordinary conditional と must still publish"
    );
}

/// Statement 3: a conditional antecedent whose sentence reaches no
/// sentence-final predicate does not publish.
///
/// Worth 18,079 records on the reference corpus, 15.9% of the four members'
/// publication. The `$` anchor in `@al_final_predicate` is what makes the
/// requirement non-vacuous — without it the 終止形 antecedent of `cond_to`
/// would satisfy the requirement itself.
#[test]
fn a_conditional_without_a_consequent_does_not_publish() {
    let matcher = matcher();
    let truncated = "実験を行うと";

    assert!(
        names(&raw(&matcher, truncated)).contains(&"cond_to"),
        "the raw language must reach the truncated antecedent, or the requirement is untested"
    );
    assert!(
        !names(&published(&matcher, truncated)).contains(&"cond_to"),
        "a conditional with no consequent published: {:?}",
        names(&published(&matcher, truncated))
    );
    assert!(
        names(&published(&matcher, "実験を行うと分かる。")).contains(&"cond_to"),
        "the same antecedent with a consequent must publish"
    );
}

/// The three families are exactly the three the schema closes `alternation:`
/// to, and every emitting pattern carries a family and a key.
///
/// `alternation` is the `partition:` dimension and the `%conditional`
/// derivation predicate. A pattern that lost the key would silently become its
/// own resolution class — selected by no statement, contesting with nobody —
/// which is a publication change with no visible cause.
#[test]
fn every_pattern_carries_its_family_and_its_key() {
    let matcher = matcher();
    let patterns = matcher.patterns();
    assert_eq!(patterns.len(), 9, "nine competing members");

    // `alternation:` is descriptive and lives in the metadata map; `key:` is
    // compiler-owned and lives in the catalog policy, which is where the engine
    // reads it from.
    let policy = matcher.catalog_policy();
    let mut families: Vec<&str> = patterns
        .iter()
        .map(|(name, pattern)| {
            let family = pattern
                .metadata
                .get("alternation")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| panic!("{name} carries no alternation family"));
            assert!(
                policy
                    .get(name.as_str())
                    .is_some_and(|declared| !declared.key.is_empty()),
                "{name} would summarize by surface rather than by collexeme"
            );
            family
        })
        .collect();
    families.sort_unstable();
    families.dedup();
    assert_eq!(families, ["conditional", "light_verb", "negation"]);
}
