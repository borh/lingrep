//! Every tracked lingrep patterns catalog must compile.
//!
//! `patterns/sfe.lx` sat broken in the tree for an unknown span of time: it
//! carried a `(?!…)` lookahead the Rust regex crate cannot build, marked
//! `# FIXME`, and nothing failed because no test compiled it. The catalogs that
//! *are* covered are covered one file at a time by name, so a new or newly
//! vendored catalog joins the repository with no coverage at all — which is
//! exactly how `sfe.lx` arrived (`5600207c`).
//!
//! The inventory this uses discovers catalogs instead of naming them, so the
//! coverage cannot drift behind the tree. It lives in `support::catalogs`
//! because `catalog_examples.rs` needs the same set, and two discoveries of
//! one set is two things to keep in step.

use lingrep_core::fsm::{
    CompileLimits, ExpandedDefinitionKind, PatternSource, compile_catalog,
    expand_pattern_definition, format_pattern_source, render_pattern_error,
};
use std::collections::BTreeMap;

use lingrep_core::testing::catalogs::{
    GuardrailRole, RuntimeRegime, catalog_campaign_descriptors, repository_root,
    tracked_catalog_sources, tracked_catalogs,
};

#[test]
fn every_tracked_catalog_compiles() {
    let root = repository_root();
    let catalogs = tracked_catalogs();

    // A discovery bug that found nothing would make this test vacuously green.
    assert!(
        catalogs.len() >= 4,
        "expected the shipped catalogs to be discovered, found {catalogs:?}"
    );

    let mut failures = Vec::new();
    for catalog in &catalogs {
        let relative = catalog.strip_prefix(&root).unwrap_or(catalog);
        let name = relative.to_string_lossy().into_owned();
        let source = std::fs::read_to_string(catalog)
            .unwrap_or_else(|error| panic!("{name} must be readable: {error}"));

        if let Err(error) = compile_catalog(
            PatternSource::named(&name, &source),
            CompileLimits::default(),
        ) {
            failures.push(render_pattern_error(Some(&source), &error));
        }
    }

    assert!(
        failures.is_empty(),
        "tracked catalogs must compile:\n\n{}",
        failures.join("\n")
    );
}

/// The formatter may respace a line. It may not restructure a catalog.
///
/// It did both. `render` emitted exactly one newline for any gap that
/// contained one, so every blank line in every tracked catalog was deleted —
/// `sf-experimental.lx` came back 99 lines shorter, `sketch-grammar.lx` 23 —
/// and the running two-space counter it indented with knew only about
/// `defaults` and `schema`, so every continuation line, every hand-aligned
/// nesting level in `double-wo.lx`, and every `policy` statement was flattened
/// to column zero. Both losses land on the browser editor, which formats on
/// blur: a researcher who clicked away from a pattern got their catalog's
/// paragraphs and indentation dissolved under them.
///
/// This asserts the property rather than a golden text, so it covers a catalog
/// added later for free, and it deliberately says nothing about what happens
/// *within* a line — respacing operators is the formatter's job.
#[test]
fn formatting_a_tracked_catalog_preserves_its_line_structure() {
    let sources = tracked_catalog_sources();
    assert!(
        sources.len() >= 4,
        "expected the shipped catalogs to be discovered"
    );

    for (name, source) in sources {
        let formatted = format_pattern_source(
            PatternSource::named(&name, &source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{name} must format: {error}"))
        .source;

        let before: Vec<&str> = source.lines().collect();
        let after: Vec<&str> = formatted.lines().collect();
        assert_eq!(
            before.len(),
            after.len(),
            "{name}: formatting changed the line count"
        );

        for (index, (original, emitted)) in before.iter().zip(&after).enumerate() {
            let line = index + 1;
            assert_eq!(
                original.trim().is_empty(),
                emitted.trim().is_empty(),
                "{name}:{line}: a blank line appeared or vanished"
            );
            if original.trim().is_empty() {
                continue;
            }
            let indent = |text: &str| text.len() - text.trim_start().len();
            assert_eq!(
                indent(original),
                indent(emitted),
                "{name}:{line}: indentation moved\n  - {original}\n  + {emitted}"
            );
        }

        let again = format_pattern_source(
            PatternSource::named(&name, &formatted),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{name} must re-format: {error}"))
        .source;
        assert_eq!(again, formatted, "{name}: formatting is not idempotent");
    }
}

/// Every definition in the tree expands to lingrep patterns that compiles on its own.
///
/// This is the load-bearing claim of reference expansion, and it is only worth
/// making against real catalogs: the shapes that break a textual splice —
/// a quantified fragment, a `test` behind `!`, a `set` behind `in`, a fragment
/// spanning four hand-indented lines, an intersection operand — are all in the
/// tree and none of them are in a hand-written fixture. `double-wo.lx` alone
/// nests references four levels deep under a bounded intersection.
///
/// Compiling is the assertion because the output is offered as lingrep patterns: the
/// browser shows it next to a copy button, so text that does not compile is a
/// defect and not a rendering choice.
#[test]
fn every_tracked_definition_expands_to_compilable_pattern() {
    let mut expanded_definitions = 0;
    let mut with_references = 0;

    for (name, source) in tracked_catalog_sources() {
        let compiled = compile_catalog(
            PatternSource::named(&name, &source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{name} must compile: {}",
                render_pattern_error(None, &error)
            )
        });

        for definition in compiled.matcher.patterns().keys() {
            let expansion = expand_pattern_definition(
                PatternSource::named(&name, &source),
                CompileLimits::default(),
                definition,
            )
            .unwrap_or_else(|error| panic!("{name}: expanding {definition}: {error}"));

            let Some(expansion) = expansion else {
                // A lexicon is in `patterns()` and is not a definition this
                // indexes by that name only when the catalog spells it
                // differently; anything else absent is a real gap.
                continue;
            };
            expanded_definitions += 1;
            assert_eq!(expansion.kind, ExpandedDefinitionKind::Pattern);

            let Some(text) = expansion.expanded else {
                assert!(
                    expansion.references.is_empty(),
                    "{name}: {definition} recorded references without an expansion"
                );
                continue;
            };
            with_references += 1;
            assert!(
                !expansion.references.is_empty(),
                "{name}: {definition} expanded without recording what it substituted"
            );
            compile_catalog(
                PatternSource::named("expansion.lx", &text),
                CompileLimits::default(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{name}: the expansion of {definition} does not compile: {}\n{text}",
                    render_pattern_error(None, &error)
                )
            });
        }
    }

    // A discovery or lookup bug that expanded nothing would make every
    // assertion above vacuous.
    assert!(
        expanded_definitions > 100,
        "expected the tracked catalogs' patterns to be expanded, saw {expanded_definitions}"
    );
    assert!(
        with_references > 50,
        "expected most tracked patterns to stand on a fragment, saw {with_references}"
    );
}

#[test]
fn tracked_pattern_execution_attributes_are_pinned() {
    let expected: BTreeMap<&str, &[&str]> = BTreeMap::from([
        // `norm` enters through the `key:` projections alone, exactly as
        // `reduplication.lx`'s `kana` enters through `by=`: what a catalog
        // reads to build its records is part of what it executes.
        (
            "patterns/alternation.lx",
            &[
                "lemma",
                "norm",
                "unidic.cform",
                "unidic.orthbase",
                "unidic.pos1",
                "unidic.pos2",
                "unidic.pos3",
            ][..],
        ),
        (
            "patterns/collexeme.lx",
            &[
                "lemma",
                "norm",
                "unidic.cform",
                "unidic.orthbase",
                "unidic.pos1",
                "unidic.pos2",
            ][..],
        ),
        (
            "patterns/concord.lx",
            &[
                "lemma",
                "text",
                "unidic.cform",
                "unidic.pos1",
                "unidic.pos2",
            ][..],
        ),
        (
            "patterns/counting.lx",
            &["unidic.orthbase", "unidic.pos1", "unidic.pos2"][..],
        ),
        (
            "patterns/distributions.lx",
            &[
                "unidic.pos1",
                "unidic.pos2",
                "unidic.pos3",
                "unidic.pos4",
                "upos",
            ][..],
        ),
        // The `where` clauses contribute `dep.head`/`dep.rel`; the skeleton's
        // predicates contribute `text` and `upos`.
        (
            "patterns/double-wo-dep.lx",
            &["dep.head", "dep.rel", "text", "upos"][..],
        ),
        (
            "patterns/double-wo.lx",
            &["text", "unidic.orthbase", "unidic.pos1", "unidic.pos2"][..],
        ),
        ("patterns/ngrams.lx", &["text", "upos"][..]),
        (
            "patterns/npv-collocation.lx",
            &["dep.head", "dep.rel", "lemma", "text", "upos"][..],
        ),
        (
            "patterns/passive-agent-dep.lx",
            &["dep.head", "dep.rel", "lemma", "upos"][..],
        ),
        (
            "patterns/passive-oblique-realization.lx",
            &["dep.head", "dep.rel", "feat.Voice", "lemma", "upos"][..],
        ),
        ("patterns/portable-content-words.lx", &["upos"][..]),
        // `kana` and `orthbase` enter through `by=` projections alone — the
        // SpanEquality catalog's execution attributes must include what its
        // comparisons read, not only what its predicates test.
        (
            "patterns/reduplication.lx",
            &[
                "text",
                "unidic.cform",
                "unidic.kana",
                "unidic.orthbase",
                "unidic.pos1",
            ][..],
        ),
        (
            "patterns/sentence-final.lx",
            &[
                "lemma",
                "norm",
                "text",
                "unidic.cform",
                "unidic.orthbase",
                "unidic.pos1",
                "unidic.pos2",
                "unidic.pos3",
                "upos",
            ][..],
        ),
        (
            "patterns/sf-experimental.lx",
            &[
                "lemma",
                "text",
                "unidic.cform",
                "unidic.orthbase",
                "unidic.pos1",
                "unidic.pos2",
                "unidic.pos3",
                "upos",
            ][..],
        ),
        (
            "patterns/sfe.lx",
            &[
                "lemma",
                "text",
                "unidic.cform",
                "unidic.pos1",
                "unidic.pos2",
                "unidic.pos3",
                "upos",
            ][..],
        ),
        (
            "patterns/sketch-grammar.lx",
            &[
                "lemma",
                "norm",
                "unidic.cform",
                "unidic.orthbase",
                "unidic.pos1",
                "unidic.pos2",
                "unidic.pos3",
            ][..],
        ),
        // The English register catalog: `dep.head`/`dep.rel` from the `where`
        // clauses, `feat.VerbForm` and `feat.Tense` from the gerund test, and
        // the portable lemma/text/upos trio. `feat.Tense` is there because the
        // gerund test carries two spellings — UD's `VerbForm=Ger` and spaCy's
        // `VerbForm=Part` with `Tense=Pres` — and the tense is what keeps the
        // second from also admitting a past participle. No provider-namespaced
        // key appears, which is what lets one catalog read any CoNLL-U
        // treebank.
        (
            "patterns/comment-register.lx",
            &[
                "dep.head",
                "dep.rel",
                "feat.Tense",
                "feat.VerbForm",
                "lemma",
                "text",
                "upos",
            ][..],
        ),
        ("patterns/test-simple.lx", &["text"][..]),
        // `dep.head` and `dep.rel` are contributed by the `where` clauses as
        // well as by the `[dep.rel=…]` predicates: a relation-bearing program
        // reports both so schema binding projects them or refuses.
        (
            "patterns/ud-dependency.lx",
            &["dep.head", "dep.rel", "text", "upos"][..],
        ),
        (
            "patterns/ud-enhanced-argument.lx",
            &["dep.enhanced", "lemma", "upos"][..],
        ),
        (
            "patterns/ud-enhanced-consistency.lx",
            &["dep.enhanced", "dep.head", "dep.rel", "lemma", "upos"][..],
        ),
        (
            "patterns/ud-enhanced-propagation.lx",
            &["dep.enhanced", "lemma", "upos"][..],
        ),
        (
            "patterns/ud-nonprojectivity.lx",
            &["dep.head", "dep.rel", "upos"][..],
        ),
        (
            "patterns/ud-object-realization.lx",
            &["dep.head", "dep.rel", "lemma", "upos"][..],
        ),
        (
            "patterns/ud-subtree-profile.lx",
            &["dep.head", "dep.rel", "upos"][..],
        ),
        (
            "patterns/ud-valency.lx",
            &["dep.head", "dep.rel", "lemma", "upos"][..],
        ),
        // The linear arm predicates on `xpos` prefixes and `lemma`; the
        // dependency arm's `where` clause contributes `dep.head`/`dep.rel`.
        (
            "patterns/wo-predicate-dep.lx",
            &["dep.head", "dep.rel", "lemma", "text", "upos", "xpos"][..],
        ),
    ]);

    let observed: BTreeMap<String, Vec<lingrep_core::token_match::AttributeKey>> =
        tracked_catalog_sources()
            .into_iter()
            .filter(|(name, _)| name.starts_with("patterns/"))
            .map(|(name, source)| {
                let matcher = compile_catalog(
                    PatternSource::named(&name, &source),
                    CompileLimits::default(),
                )
                .unwrap_or_else(|error| panic!("{name} must compile: {error:?}"))
                .matcher;
                (name, matcher.execution_attributes())
            })
            .collect();

    assert_eq!(
        observed.len(),
        28,
        "every pattern catalog needs a pinned projection"
    );
    for (name, attributes) in &observed {
        let observed_keys = attributes
            .iter()
            .map(lingrep_core::token_match::AttributeKey::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            observed_keys.as_slice(),
            *expected
                .get(name.as_str())
                .unwrap_or_else(|| panic!("{name} needs a pinned projection"))
        );
    }
}

/// The stratified performance suite must cover every catalog under `patterns/`.
///
/// The roles and regimes themselves live in
/// [`lingrep_core::testing::catalogs::catalog_campaign_descriptors`], because the
/// `lingrep-perf` campaign adjudicator reads them too and a second copy would be a
/// second thing to forget to update. This test is the completeness guard on
/// that source: it takes the *discovered* set from the tree and fails until a
/// new catalog is given a role and a regime, so a catalog cannot join the
/// repository already exempt from every campaign.
///
/// The tier rules select catalogs by the regime a change can affect, so a
/// catalog missing from the suite is silently exempt. That list lived in prose
/// and drifted: `collexeme.lx` compiles lookaround — one of only four
/// catalogs that do — and was outside the suite when a lookaround optimization
/// was measured against it, as were `alternation.lx` and `sfe.lx`.
/// Discovery must not reach into a tree of generated output.
///
/// "Found rather than listed" is what keeps catalog coverage from drifting
/// behind the tree, and it cuts the other way too: anything `.lx`-shaped
/// anywhere becomes a repository catalog that every pin is judged against. A
/// harbor benchmark run writes each agent's tool invocations under
/// `benchmarks/harbor/jobs/`, and an agent asked to author lingrep patterns leaves `.lx`
/// files there. One run turned seventeen tests red at once — a duplicate
/// definition and a syntax error in a generated `patterns/0.lx` reported as
/// defects in *this* repository's catalogs.
///
/// Asserted over the discovered set rather than by unit-testing the skip list,
/// because the property that matters is "no generated file reached a pin", not
/// "the constant contains this string".
#[test]
fn discovery_ignores_generated_output_trees() {
    let offenders: Vec<String> = tracked_catalog_sources()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("benchmarks/harbor/jobs/"))
        .collect();

    assert!(
        offenders.is_empty(),
        "generated benchmark output reached catalog discovery: {offenders:?}"
    );
}

#[test]
fn every_pattern_catalog_has_a_pinned_guardrail_role() {
    let descriptors = catalog_campaign_descriptors();

    let discovered: Vec<String> = tracked_catalog_sources()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("patterns/"))
        .collect();

    let pinned: Vec<String> = descriptors
        .iter()
        .map(|descriptor| descriptor.path.to_owned())
        .collect();
    assert_eq!(
        discovered, pinned,
        "a catalog under patterns/ must be given a guardrail role, or it is exempt \
         from every performance campaign without anyone deciding that"
    );

    // A role nobody carries would make the suite an empty selection.
    assert!(
        descriptors
            .iter()
            .any(|descriptor| descriptor.role == GuardrailRole::Substantive),
        "the suite must contain substantive catalogs"
    );
    assert!(
        descriptors
            .iter()
            .all(|descriptor| !descriptor.regimes.is_empty()),
        "a catalog with no regime is selected by no campaign, which is the silent \
         exemption this suite exists to prevent"
    );
    assert!(
        descriptors
            .iter()
            .all(|descriptor| !descriptor.description.trim().is_empty()),
        "the description is what a human reads when scoping a campaign"
    );

    // Tags are what a campaign selects on, so the ones with a reliable source
    // oracle are checked rather than trusted. `Lookaround` matters most: it is
    // the regime that scoped the 2026-08-18 campaign, and `collexeme` was
    // outside the suite entirely while a lookaround optimization was measured
    // against it.
    //
    // The oracle is the construct name followed by `(`. A lookaround name is
    // also legal as a bracketed value — `[text=followed_by]` — which the
    // trailing parenthesis excludes.
    let root = repository_root();
    for descriptor in &descriptors {
        let source = std::fs::read_to_string(root.join(descriptor.path))
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", descriptor.path));

        let uses_lookaround = ["preceded_by(", "followed_by("]
            .iter()
            .any(|construct| source.contains(construct));
        assert_eq!(
            descriptor.regimes.contains(&RuntimeRegime::Lookaround),
            uses_lookaround,
            "{}: the Lookaround tag and the catalog's own lookaround constructs disagree",
            descriptor.path
        );

        assert_eq!(
            descriptor.regimes.contains(&RuntimeRegime::SameAs),
            source.contains("same_as("),
            "{}: the SameAs tag and the catalog's own `same_as` use disagree",
            descriptor.path
        );

        // The edge arrow's closing token: unambiguous in the grammar, and not
        // spellable in a bare value, so its presence is the `where` oracle.
        assert_eq!(
            descriptor.regimes.contains(&RuntimeRegime::Relations),
            source.contains("]->"),
            "{}: the Relations tag and the catalog's own `where` arrows disagree",
            descriptor.path
        );

        assert_eq!(
            descriptor
                .regimes
                .contains(&RuntimeRegime::EnhancedRelations),
            source.contains("-[enhanced "),
            "{}: the EnhancedRelations tag and the catalog's enhanced edge constraints disagree",
            descriptor.path
        );

        assert_eq!(
            descriptor.regimes.contains(&RuntimeRegime::PolicyProgram),
            source.lines().any(|line| line.trim_end() == "policy"),
            "{}: the PolicyProgram tag and the catalog's own policy block disagree",
            descriptor.path
        );

        // A `transform:` annotation starts its own line, and the key cannot be
        // a bare value, so the line prefix is the oracle.
        assert_eq!(
            descriptor.regimes.contains(&RuntimeRegime::RecordTransform),
            source.lines().any(|line| line.starts_with("transform:")),
            "{}: the RecordTransform tag and the catalog's own transform list disagree",
            descriptor.path
        );

        // `{` alone would match a set literal or a policy `terminal: {…}`; a
        // quantifier is the only place a digit follows the brace immediately.
        let bytes = source.as_bytes();
        let uses_bounded_repeat = bytes.iter().enumerate().any(|(index, byte)| {
            *byte == b'{' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
        });
        assert_eq!(
            descriptor.regimes.contains(&RuntimeRegime::BoundedRepeat),
            uses_bounded_repeat,
            "{}: the BoundedRepeat tag and the catalog's own `{{n,m}}` use disagree",
            descriptor.path
        );
    }
}

/// A campaign scoped to one regime must select exactly the catalogs that
/// exercise it, by set membership rather than by matching prose.
///
/// The tier rules give an affected catalog three paired blocks and everything
/// else a single screen pair, so a selection that is one catalog wrong is a
/// campaign that is one catalog under- or over-measured.
#[test]
fn a_regime_scoped_campaign_selects_exactly_the_catalogs_that_exercise_it() {
    let descriptors = catalog_campaign_descriptors();

    let selected = |regimes: &[RuntimeRegime]| -> Vec<&str> {
        descriptors
            .iter()
            .filter(|descriptor| descriptor.affected_by(regimes))
            .map(|descriptor| descriptor.path)
            .collect()
    };

    // Seven catalogs compile lookaround. Two independent readings agree: the
    // constructs in their sources, and the assertion row traffic the
    // counted-work screen reports for them and for no other catalog.
    //
    // `comment-register` is the newest and the only English one. Its
    // `dash_aside` requires a non-punctuation neighbour on each side, which
    // is a zero-width assertion precisely so the definition stays one token
    // wide and keeps projecting its own spelling.
    assert_eq!(
        selected(&[RuntimeRegime::Lookaround]),
        [
            "patterns/collexeme.lx",
            "patterns/comment-register.lx",
            "patterns/concord.lx",
            "patterns/double-wo.lx",
            "patterns/npv-collocation.lx",
            "patterns/sentence-final.lx",
            "patterns/sf-experimental.lx",
        ]
    );

    // The transform seam had no tracked user before this catalog, which is
    // why the tag exists: a campaign naming it selected nothing.
    assert_eq!(
        selected(&[RuntimeRegime::RecordTransform]),
        ["patterns/npv-collocation.lx"]
    );

    assert_eq!(
        selected(&[RuntimeRegime::SameAs]),
        ["patterns/reduplication.lx"]
    );
    assert_eq!(
        selected(&[RuntimeRegime::Intersection]),
        ["patterns/double-wo.lx"]
    );

    // A campaign naming several regimes takes the union, and a catalog in two
    // of them is selected once.
    assert_eq!(
        selected(&[RuntimeRegime::Intersection, RuntimeRegime::SameAs]),
        ["patterns/double-wo.lx", "patterns/reduplication.lx"]
    );

    // A regime nothing exercises selects nothing rather than everything, which
    // is the direction that fails a campaign loudly instead of silently
    // measuring the whole suite.
    assert!(selected(&[]).is_empty());
}

#[test]
fn sentence_final_identical_bodies_have_reviewed_categories() {
    let source = std::fs::read_to_string(repository_root().join("patterns/sentence-final.lx"))
        .expect("sentence-final catalog");
    let input = PatternSource::named("patterns/sentence-final.lx", &source);
    let compiled = compile_catalog(input, CompileLimits::default()).expect("valid catalog");
    let mut bodies: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, pattern) in compiled.matcher.patterns() {
        let definition = expand_pattern_definition(input, CompileLimits::default(), name)
            .expect("valid definition")
            .expect("emitting definition");
        let formatted = format_pattern_source(
            PatternSource::named("definition.lx", &definition.declaration),
            CompileLimits::default(),
        )
        .expect("formatted definition");
        let body = formatted
            .source
            .split_once(":=")
            .expect("definition body")
            .1
            .trim()
            .to_owned();
        bodies.entry(body).or_default().push((
            name.to_string(),
            pattern.metadata["category"]
                .as_str()
                .expect("category")
                .to_owned(),
        ));
    }
    let mut duplicates: Vec<_> = bodies
        .into_values()
        .filter(|names| names.len() > 1)
        .collect();
    for names in &mut duplicates {
        names.sort();
    }
    duplicates.sort();
    let mut reviewed: Vec<Vec<(String, String)>> = vec![
        vec![
            ("と考えられる".into(), "S2C 推量判断の提示".into()),
            (
                "と考えられる_S1F".into(),
                "S1F 判断の言い換え・再提示".into(),
            ),
        ],
        vec![
            ("と言える".into(), "S2D 書き手による判断明示".into()),
            ("と言える_S1F".into(), "S1F 判断の言い換え・再提示".into()),
        ],
        vec![
            ("のである_S2F".into(), "S2H 強調".into()),
            ("のである_S1E".into(), "S1E 説明・提示".into()),
        ],
    ];
    for names in &mut reviewed {
        names.sort();
    }
    reviewed.sort();
    assert_eq!(
        duplicates, reviewed,
        "review new identical bodies and their categories"
    );
}
