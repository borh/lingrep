//! Baseline for how repository catalogs project into the pattern browser.
//!
//! Metadata is not load-bearing for matching, so a migration can preserve every
//! match and still break the UI. `project_pattern_catalog` picks the browser's
//! grouping field out of metadata by a cardinality heuristic; nothing about
//! matching would notice if that pick changed.
//!
//! The lingrep patterns improvement spec requires the migration to preserve the selected
//! `default_grouping_field` and the grouping candidate set. It could not: the
//! only existing test of the projection feeds it synthetic descriptors, so
//! nothing recorded what the real catalogs produce. This is that record, and it
//! has to exist before the migration rather than after — afterwards there is
//! nothing left to compare against.
//!
//! Deliberately built from `pattern_metadata_pairs`, the function the server
//! itself calls, rather than a re-implementation of the conversion. A baseline
//! that pinned a copy would stay green while the real path broke.

use lingrep_core::testing::catalogs::tracked_catalog_sources;
use lingrep_web::pattern_catalog::{pattern_metadata_pairs, project_pattern_catalog};
use lingrep_web::patterns::compile_web_pattern;
use lingrep_web::snapshot::{PatternDescriptor, PatternKey, PatternOrigin};

/// One catalog's metadata-derived projection, as `(default field, candidate
/// fields with their populated counts)`.
type Projection = (Option<String>, Vec<(String, usize)>);

fn projection_for(source: &str) -> Projection {
    let output = compile_web_pattern(source).expect("a tracked catalog must compile");
    // Mirrors `SnapshotSink::new`: descriptors are seeded in catalog order with
    // no match counts yet. `exact_total` is runtime data, not a property of the
    // catalog, and it feeds only the result ordering and `auto_load` -- never
    // the grouping fields this pins. Leaving it zero keeps the baseline a
    // statement about the catalog alone.
    let descriptors = pattern_metadata_pairs(&output.matcher)
        .into_iter()
        .enumerate()
        .map(|(index, (name, metadata))| PatternDescriptor {
            key: PatternKey::new(index).expect("catalog fits the pattern key space"),
            name,
            exact_total: 0,
            metadata,
            origin: PatternOrigin::Authored { chained_total: 0 },
        })
        .collect::<Vec<_>>();

    let projection = project_pattern_catalog(&descriptors);
    (
        projection.default_grouping_field,
        projection
            .grouping_fields
            .into_iter()
            .map(|field| (field.key, field.populated))
            .collect(),
    )
}

/// The expected projection of every tracked catalog.
///
/// The *set* is discovered, so a new catalog fails with "no pinned projection"
/// rather than being skipped by not appearing here. Only the values are
/// written down.
///
/// The heuristic admits a key only when at least two definitions carry it and
/// its distinct values number at most half that count, so a key that is unique
/// per definition -- an `examples:`-style annotation -- is correctly never a
/// grouping candidate. Order is `category, subcategory, family, type`, then
/// everything else by descending count and then by key.
///
/// Counts are over *emitting* patterns, since `patterns()` is what the browser
/// receives. That is not the same as counting `metadata` blocks in the file,
/// and the difference is measured rather than theoretical:
/// `sf-experimental.lx` has 86 emitting definitions but 82 published names,
/// so four definitions share a result label and the browser sees the published
/// view rather than every authored definition.
/// `double-wo.lx` has 14 blocks and 7 emitting patterns. Anyone migrating
/// against the catalog's "86 definitions" figure should expect 82 here and not treat
/// the gap as a regression.
///
/// The zeros are load-bearing too: a migration that started *inventing*
/// metadata would break `test-simple.lx` and `test_metadata.lx` as loudly as
/// one that dropped it.
fn expected_projection(catalog: &str) -> Option<Projection> {
    let field = |key: &str, populated: usize| (key.to_owned(), populated);
    let grouped = |fields: Vec<(String, usize)>| (Some("category".to_owned()), fields);

    let projection: Projection = match catalog {
        // One emitting probe without metadata cannot form a grouping.
        "lingrep-core/tests/fixtures/catalogs/apparatus-depth2.lx" => (None, vec![]),
        // 27 emitting patterns; six metadata blocks contribute the retained
        // sentence-final categories.
        "patterns/sfe.lx" => grouped(vec![
            field("category", 7),
            field("subcategory", 4),
            field("type", 7),
            field("meidai", 6),
            field("status", 6),
        ]),
        // 9 emitting patterns and no `category` key at all, so the default
        // falls out of the "descending count, then key" tail rather than the
        // priority list. `member` is unique per pattern and `alternation`
        // names three families over nine, so the heuristic keeps the one a
        // researcher would group by; `examples` and `counterexamples` are
        // lists and never reach it.
        "patterns/alternation.lx" => (
            Some("alternation".to_owned()),
            vec![
                field("alternation", 9),
                field("slot", 9),
                field("source", 9),
                field("status", 9),
            ],
        ),
        // 13 emitting patterns, same shape as the sibling above. `construction`
        // is unique per pattern and `discriminator` is unique over the twelve
        // that carry it, so neither is a grouping candidate — which leaves
        // `frame`, the three-valued key, as the default.
        "patterns/collexeme.lx" => (
            Some("frame".to_owned()),
            vec![
                field("frame", 13),
                field("slot", 13),
                field("source", 13),
                field("status", 13),
            ],
        ),
        // 12 emitting patterns (six 呼応 constructions, two spellings each).
        // `category` has five distinct values over twelve patterns; `status`
        // is the defaults-block value on all twelve. `examples` and
        // `counterexamples` are lists and never reach the heuristic.
        "patterns/concord.lx" => grouped(vec![field("category", 12), field("status", 12)]),
        // 9 unrolled counting cost probes; `category` is "counting" on all
        // nine and `status` is the defaults-block value. `examples` is a list
        // and never reaches the heuristic.
        "patterns/counting.lx" => grouped(vec![field("category", 9), field("status", 9)]),
        // Two emitting patterns carry only compiler-owned ordered keys, which
        // never enter the browser's descriptive grouping heuristic.
        "patterns/distributions.lx" => (None, vec![]),
        // Five emitting patterns carry only compiler-owned ordered keys.
        "patterns/ngrams.lx" => (None, vec![]),
        "patterns/npv-collocation.lx" => (None, vec![]),
        // Two surface orders collapse to one published passive-agent relation;
        // one descriptor cannot form a grouping candidate.
        "patterns/passive-agent-dep.lx" => (None, vec![]),
        // The six evidence/state arms publish as two result names; passive
        // evidence varies within both rows.
        "patterns/passive-oblique-realization.lx" => (
            Some("passive_evidence".to_owned()),
            vec![field("passive_evidence", 2), field("status", 2)],
        ),
        // The two portable patterns likewise carry only the compiler-owned
        // `key: [upos]` publication field plus unique descriptions.
        "patterns/portable-content-words.lx" => (None, vec![]),
        // Single emitting pattern since the 2026-08-29 label-set collapse:
        // one definition can never reach `populated >= 2`. It was 8 patterns
        // over four keys, and the `subcategory` that distinguished them is
        // now the record's own `key:` projection rather than metadata.
        "patterns/double-wo-dep.lx" => (None, vec![]),
        // 7 emitting patterns; every one carries all four keys.
        "patterns/double-wo.lx" => grouped(vec![
            field("category", 7),
            field("role", 7),
            field("source", 7),
            field("status", 7),
        ]),
        // 2 emitting patterns; `category` is 重複・強調 on both and `status`
        // is the defaults-block value. `examples` and `counterexamples` are
        // lists and never reach the heuristic.
        "patterns/reduplication.lx" => grouped(vec![field("category", 2), field("status", 2)]),
        // All 52 emitting patterns carry a compact research category; aspect
        // helpers are fragments and do not enter the result grouping.
        "patterns/sentence-final.lx" => grouped(vec![field("category", 49)]),
        // 86 emitting definitions, projecting to 82 PUBLISHED names.
        //
        // This projection seeds `SnapshotSink`, which is a results view: it
        // aggregates per published name, and rejects a name it was not seeded
        // with. Once result policy is read, four of this catalog's definitions
        // publish under a name another definition already publishes —
        // `わけだ`/`わけである` both as `わけ`, `ようだ`/`ようである` both as
        // `よう`, and so on — so the browser shows the label the records
        // actually carry rather than two rows that can never be told apart.
        //
        // A recognized policy key is **withheld** from the grouping heuristic.
        // `priority_order` partitions these patterns exactly as `layer` does
        // because a `resolve` statement needs a total order over those layers,
        // but
        // the heuristic has no way to see that a key describes publication
        // rather than research. `project_pattern_catalog` now filters on
        // `compiler_owned_key`.
        //
        // This catalog is the only tracked one declaring result policy; every
        // other pin below is unchanged, which is the guard that wiring did not
        // disturb a policy-free catalog.
        "patterns/sf-experimental.lx" => grouped(vec![
            field("category", 82),
            field("layer", 82),
            field("role", 82),
            field("status", 82),
            // List-valued metadata such as `groups` and `examples` does not
            // reach the scalar grouping heuristic.
            //
            // `s0_requires_upper_layer` stood here at 3 for the same span of
            // the migration and left by the same door: `%s0_conditional`
            // membership replaced it, and membership is a list.
        ]),
        // 10 emitting relations. Relation, collocate, source, and examples
        // vary per definition, while head, role and status are carried by all
        // ten. The only tracked catalog whose default grouping field is **not**
        // `category`: it has no such key, so the heuristic falls through its
        // four preferred names to the by-count-then-key tail, where a
        // three-way tie at 10 is broken alphabetically. Grouping a word sketch
        // by `head` is also the right answer, which is why this is pinned
        // rather than corrected in the catalog.
        "patterns/sketch-grammar.lx" => (
            Some("head".to_owned()),
            vec![field("head", 10), field("role", 10), field("status", 10)],
        ),
        // 13 emitting patterns annotated with `layer` and inheriting `status`
        // from the file defaults, projecting to 12 rows: the two softener
        // definitions are one relation written in two linear orders and share
        // an `output_name`, so the browser groups them under one published
        // name.
        "patterns/comment-register.lx" => (
            Some("layer".to_owned()),
            vec![field("layer", 12), field("status", 12)],
        ),
        // Single pattern each: one definition can never reach `populated >= 2`.
        "patterns/test-simple.lx" => (None, vec![]),
        // 5 emitting where-clause patterns; `layer` and `status` come from
        // one annotation run and the file defaults respectively.
        "patterns/ud-dependency.lx" => (
            Some("layer".to_owned()),
            vec![field("layer", 5), field("status", 5)],
        ),
        // Both graph-assignment patterns inherit status. Their graph scopes
        // are distinct, so neither scope value can form a repeated group.
        "patterns/ud-enhanced-argument.lx" => (Some("status".to_owned()), vec![field("status", 2)]),
        // All four consistency instruments inherit status. `finding` has a
        // distinct value per pattern, so status is the only universal group.
        "patterns/ud-enhanced-consistency.lx" => {
            (Some("status".to_owned()), vec![field("status", 4)])
        }
        // All seven propagation motifs inherit status. `phenomenon` collapses
        // them into four values, so status is the only universal group.
        "patterns/ud-enhanced-propagation.lx" => {
            (Some("status".to_owned()), vec![field("status", 7)])
        }
        // Four direction variants collapse to one published crossing relation.
        "patterns/ud-nonprojectivity.lx" => (None, vec![]),
        // The five head classifiers all carry object state and inherited
        // status. Object state wins the equal-population alphabetical tie;
        // object class and pronominal state each appear on only one result per
        // value and are not groupable.
        "patterns/ud-object-realization.lx" => (
            Some("object_state".to_owned()),
            vec![field("object_state", 5), field("status", 5)],
        ),
        // Nine authored linearizations collapse to the fork and chain result
        // names. `topology` is unique per result, leaving inherited `status`
        // as the only grouping candidate.
        "patterns/ud-subtree-profile.lx" => (Some("status".to_owned()), vec![field("status", 2)]),
        // The two transitive orders collapse to one of six result names.
        // `frame` is unique per result, so inherited status is the grouping.
        "patterns/ud-valency.lx" => (Some("status".to_owned()), vec![field("status", 6)]),
        // Both ヲ格 definitions carry the same seven descriptive keys, so
        // every field collapses to one value and the browser groups on the
        // first key in sort order.
        "patterns/wo-predicate-dep.lx" => (
            Some("category".to_owned()),
            vec![
                field("category", 2),
                field("collocate", 2),
                field("head", 2),
                field("relation", 2),
                field("role", 2),
                field("source", 2),
                field("status", 2),
            ],
        ),
        "scripts/test/test_metadata.lx" => (None, vec![]),
        "lingrep/tests/fixtures/one.lx" => (None, vec![]),
        // Lexicons emit and seed browser descriptors, but these research
        // fixtures intentionally carry no descriptive metadata.
        "lingrep/tests/fixtures/lexicon-research-mix.lx" => (None, vec![]),
        "lingrep/tests/fixtures/lexicon-semantic.lx" => (None, vec![]),
        "lingrep/tests/fixtures/pron-complement.lx" => (None, vec![]),
        "lingrep/tests/fixtures/pron-exclusion.lx" => (None, vec![]),
        "lingrep/tests/fixtures/sentence-final.lx" => (None, vec![]),
        // 10 emitting patterns, all five metadata keys from one block.
        "tests/fixtures/selective-patterns.lx" => grouped(vec![
            field("category", 5),
            field("type", 5),
            field("description", 5),
        ]),
        _ => return None,
    };
    Some(projection)
}

#[test]
fn every_tracked_catalog_projects_to_its_pinned_grouping() {
    for (name, source) in tracked_catalog_sources() {
        let expected = expected_projection(&name).unwrap_or_else(|| {
            panic!(
                "{name} has no pinned projection — add one to expected_projection \
                 so the new catalog is covered rather than silently skipped"
            )
        });

        assert_eq!(
            projection_for(&source),
            expected,
            "{name}: the pattern browser's grouping changed. If this is a \
             deliberate metadata change, update the pin; if it is a migration \
             that was supposed to preserve metadata, it did not."
        );
    }
}

#[test]
fn the_baseline_would_notice_a_dropped_key() {
    // Guards the guard. The assertion above is only worth its line count if
    // removing metadata actually moves the projection -- otherwise a migration
    // could drop every key and the baseline would still pass.
    let with_category = "category: \"a\"\np := [unidic.pos1=代名詞].\n\
                         category: \"a\"\nq := [unidic.pos1=動詞].\n";
    // Same definitions with no metadata.
    let without = "p := [unidic.pos1=代名詞].\nq := [unidic.pos1=動詞].\n";

    assert_eq!(
        projection_for(with_category),
        (
            Some("category".to_owned()),
            vec![("category".to_owned(), 2)]
        )
    );
    assert_eq!(projection_for(without), (None, vec![]));
}
