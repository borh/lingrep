//! Generated-input properties of the result policy's `resolve` statement.

use lingrep_core::fsm::policy_program::Overlaps;
use lingrep_core::fsm::{
    CompiledCatalog, Match, PolicyStats, ProgramStats, PublishedRecord, apply_policy_records,
};
use proptest::prelude::*;

use crate::policy_helpers::{matcher_for, no_anchors, tokens_with_orth};

/// `bb` carries no `layer`, so a `partition: layer` resolve does not select it.
const DEFINITIONS: &str = "layer: \"x\"\npriority_order: 2\na := [text=a].\n\
     layer: \"y\"\npriority_order: 1\na_again := [text=a].\n\
     layer: \"x\"\nab := [text=a] [text=b].\n\
     layer: \"y\"\nba := [text=b] [text=a].\n\
     layer: \"x\"\npriority_order: 3\naba := [text=a] [text=b] [text=a].\n\
     layer: \"y\"\nrun := [text=a]+.\n\
     bb := [text=b] [text=b].\n";

/// With `evidence: current`, a `resolve` placed first can drop the `@ab`
/// operand, and the block then publishes an `a` record that the block without
/// the `resolve` excludes.
const EXCLUDE: &str = "exclude @a with: @ab, relation: at_start, evidence: raw.";

fn words() -> impl Strategy<Value = Vec<&'static str>> {
    prop::collection::vec(prop::sample::select(vec!["a", "b", "c"]), 0..10)
}

fn resolve() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec!["all", "patterns"]),
        any::<bool>(),
        prop::sample::subsequence(vec!["longest", "shortest", "priority_order"], 0..=3)
            .prop_filter(
                "`longest` and `shortest` together fail compilation",
                |head| !(head.contains(&"longest") && head.contains(&"shortest")),
            )
            .prop_shuffle(),
    )
        .prop_map(|(subject, partitioned, head)| {
            let partition = if partitioned {
                " partition: layer,"
            } else {
                ""
            };
            let rank = [head.as_slice(), &["start", "end", "pattern"]]
                .concat()
                .join(", ");
            format!("resolve {subject}{partition} rank: [{rank}].")
        })
}

fn catalog(statements: &[&str]) -> CompiledCatalog {
    matcher_for(&format!(
        "{DEFINITIONS}policy\n  {}\nend\n",
        statements.join("\n  ")
    ))
}

fn publish(matcher: &CompiledCatalog, words: &[&str], raw: &[Match]) -> Vec<PublishedRecord> {
    let policy = matcher.catalog_policy();
    apply_policy_records(
        policy,
        raw,
        &tokens_with_orth(words),
        &no_anchors(),
        &mut PolicyStats::default(),
        &mut ProgramStats::for_program(policy.program()),
        None,
    )
    .expect("the catalog declares no anchor")
}

fn publish_canonical(matcher: &CompiledCatalog, words: &[&str]) -> Vec<PublishedRecord> {
    publish(
        matcher,
        words,
        &matcher.find_matches(&tokens_with_orth(words)),
    )
}

fn in_canonical_order(mut records: Vec<PublishedRecord>) -> Vec<PublishedRecord> {
    records.sort_by(|left, right| {
        (left.raw.start, left.raw.end, &left.raw.pattern).cmp(&(
            right.raw.start,
            right.raw.end,
            &right.raw.pattern,
        ))
    });
    records
}

proptest! {
    #[test]
    fn resolve_publishes_the_same_records_for_any_arrival_order_of_raw_matches(
        (words, shuffled) in words().prop_flat_map(|words| {
            let raw = matcher_for(DEFINITIONS).find_matches(&tokens_with_orth(&words));
            (Just(words), Just(raw).prop_shuffle())
        }),
        resolve in resolve(),
    ) {
        let matcher = catalog(&[&resolve]);
        prop_assert_eq!(
            in_canonical_order(publish(&matcher, &words, &shuffled)),
            publish_canonical(&matcher, &words)
        );
    }

    #[test]
    fn a_repeated_resolve_statement_publishes_the_same_records_as_one(
        words in words(),
        resolve in resolve(),
    ) {
        prop_assert_eq!(
            publish_canonical(&catalog(&[&resolve, &resolve]), &words),
            publish_canonical(&catalog(&[&resolve]), &words)
        );
    }

    #[test]
    fn overlaps_longest_publishes_no_two_overlapping_records(
        words in words(),
        excluding in any::<bool>(),
    ) {
        let matcher = if excluding { catalog(&[EXCLUDE]) } else { matcher_for(DEFINITIONS) }
            .with_overlaps(Overlaps::Longest);
        let published = publish_canonical(&matcher, &words);
        for (index, left) in published.iter().enumerate() {
            for right in &published[index + 1..] {
                prop_assert!(
                    left.end <= right.start || right.end <= left.start,
                    "{left:?} overlaps {right:?}"
                );
            }
        }
    }

    #[test]
    fn a_block_without_its_resolve_publishes_every_record_the_block_publishes(
        words in words(),
        resolve in resolve(),
        exclude_position in prop::option::of(0usize..=1),
    ) {
        let mut statements = vec![resolve.as_str()];
        if let Some(position) = exclude_position {
            statements.insert(position, EXCLUDE);
        }
        let resolved = catalog(&statements);
        let unresolved = publish_canonical(&resolved.clone().with_overlaps(Overlaps::Keep), &words);
        for record in publish_canonical(&resolved, &words) {
            prop_assert!(unresolved.contains(&record), "{record:?} is absent from {unresolved:?}");
        }
    }
}
