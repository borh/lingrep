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
//! Compile-time acceptance for declared result policy.
//!
//! Policy is read off metadata at compile time and stored on the compiled
//! matcher. What this pins is the compiler's half of that contract: what is
//! accepted, what is refused, what is merely warned about, what a declaration
//! leaves behind on the compiled matcher — including the attribute projection a
//! declared anchor widens — and, the part with teeth, that a catalog declaring
//! no policy is completely unaffected.

use crate::policy_helpers::{
    PUNCT_ANCHOR, catalog_policy_for, compile_errors, compile_warnings, matcher_for,
    punct_anchor_matcher, punct_anchor_with_distance, punct_anchor_with_terminal,
    sym_anchor_matcher, tokens_with_pos,
};
use compact_str::CompactString;
use lingrep_core::fsm::{
    AnchorSetDigest, CompileLimits, DiagnosticCode, DiagnosticSeverity, Match, PatternSource,
    Terminal, compile_catalog,
};
use lingrep_core::testing::token;
use lingrep_core::token_match::TokenAttributeIndex;

pub(crate) fn compile(
    text: &str,
) -> Result<lingrep_core::fsm::CompileOutput, lingrep_core::fsm::CompileError> {
    compile_catalog(
        PatternSource::named("test.lx", text),
        CompileLimits::default(),
    )
}

/// `CompileOutput` is not `Debug`, so `expect_err` is unavailable.
fn compile_err(text: &str, why: &str) -> lingrep_core::fsm::CompileError {
    match compile(text) {
        Ok(_) => panic!("expected compilation to fail: {why}"),
        Err(error) => error,
    }
}

/// A diagnostic about a metadata value points at its key, not the annotated
/// definition.
#[test]
fn a_policy_value_diagnostic_points_at_its_own_key() {
    const SOURCE: &str = "output_name: \"published\"\npriority_order: \"yes\"\nbad := [text=a].\n";
    let error = match compile_catalog(
        PatternSource::named("t.lx", SOURCE),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("a value the engine cannot use is fatal"),
        Err(error) => error,
    };
    let range = error.diagnostics[0].location().byte_range();
    assert_eq!(
        &SOURCE[range.clone()],
        "priority_order",
        "the diagnostic names the key whose value is wrong"
    );
    assert!(
        range.start > SOURCE.find("output_name").expect("first key present"),
        "the offending key is reported, not the first key on the definition"
    );
    assert!(
        range.end < SOURCE.find("bad :=").expect("definition present"),
        "the key is reported, not the definition it annotates"
    );
}

/// The complement, so the rewire cannot quietly become "always the key".
///
/// A fragment declaring result policy is a defect of the *definition* — the
/// policy keys on it are each individually fine — so it keeps the definition's
/// span. Provenance decides the span, not file membership.
#[test]
fn a_whole_definition_defect_still_points_at_the_definition() {
    const SOURCE: &str = "output_name: \"published\"\nfragment f := [text=a].\n";
    let output = compile_catalog(
        PatternSource::named("t.lx", SOURCE),
        CompileLimits::default(),
    )
    .expect("a fragment declaring policy is a warning, not an error");
    let warning = output
        .diagnostics
        .iter()
        .find(|d| d.code() == DiagnosticCode::IneffectivePolicy)
        .expect("a fragment declaring policy is reported");
    let range = warning.location().byte_range();
    assert!(
        range.start >= SOURCE.find("fragment f").expect("definition present"),
        "the definition is the subject, so the definition is the span"
    );
}

#[test]
fn a_declared_policy_reaches_the_compiled_matcher() {
    let output = compile(
        r#"
output_name: "published"
priority_order: 2
renamed := [text=a].
"#,
    )
    .expect("catalog compiles");

    let policy = output.matcher.catalog_policy();
    assert!(policy.declared());
    let entry = policy.get("renamed").expect("renamed declares policy");
    assert_eq!(entry.output_name.as_deref(), Some("published"));
    assert_eq!(entry.priority_order, Some(2));
}

#[test]
fn a_policy_reference_to_no_definition_fails_compilation() {
    // Reuses the body-level code deliberately: an operand naming `@missing` and
    // a body-level `@missing` are the same mistake, and a consumer filtering
    // `UndefinedReference` must see both.
    let error = compile_err(
        "dangling := [text=a].\n\
         policy\n  exclude @dangling with: @missing, relation: within, evidence: raw.\nend\n",
        "an unresolvable policy reference is fatal",
    );

    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::UndefinedReference)
        .expect("reported as an undefined reference");
    assert!(
        diagnostic.message.contains("missing"),
        "the message must name the operand: {}",
        diagnostic.message
    );
}

#[test]
fn a_malformed_policy_value_fails_compilation() {
    let error = compile_err(
        r#"
priority_order: "yes"
bad := [text=a].
"#,
        "a value the engine cannot use is fatal",
    );
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::InvalidPolicyValue),
        "{:#?}",
        error.diagnostics
    );
}

#[test]
fn a_suspected_typo_warns_without_failing() {
    // An unrecognized key one edit from a recognized one is inert, and
    // inert-but-plausible is exactly what a researcher cannot see by reading.
    let output = compile(
        r#"
output_namme: "x"
typo := [text=a].
"#,
    )
    .expect("a suspected typo is not fatal");

    let warning = output
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::SuspiciousPolicyKey)
        .expect("the typo is reported");
    assert_eq!(warning.severity(), DiagnosticSeverity::Warning);
    assert!(
        warning.message.contains("output_namme") && warning.message.contains("output_name"),
        "{}",
        warning.message
    );
    // Inert means inert: the misspelling must not be honored as policy.
    assert!(!output.matcher.catalog_policy().declared());
}

#[test]
fn the_synthesis_namespace_is_not_authorable() {
    let error = compile_err(
        "__policy__collide := [text=a].",
        "the reserved prefix is refused in source",
    );
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::ReservedName),
        "{:#?}",
        error.diagnostics
    );
}

#[test]
fn a_fragment_reference_synthesizes_an_emitter_that_matches_but_is_not_published() {
    // The three parts of the hidden-emitter contract only mean something
    // together: the fragment's matches must exist (the
    // engine needs them as evidence for a `detach`), the synthesized
    // definition must NOT appear in the public catalog (no researcher wrote
    // it), and it must be recorded in `hidden` (the publication seam has to
    // know what to strip).
    let output = compile(
        r#"
fragment citation := [text=引用].

cited := [text=論] @citation.

policy
  detach @cited with: @citation, relation: at_end, evidence: raw.
end
"#,
    )
    .expect("catalog compiles");

    let policy = output.matcher.catalog_policy();
    assert_eq!(
        policy
            .hidden
            .iter()
            .map(|(hidden, fragment)| (hidden.as_str(), fragment.as_str()))
            .collect::<Vec<_>>(),
        [("__policy__citation", "citation")]
    );

    let published: Vec<&str> = output
        .matcher
        .patterns()
        .keys()
        .map(CompactString::as_str)
        .collect();
    assert!(
        !published.iter().any(|name| name.starts_with("__policy__")),
        "the synthesized emitter must not reach the public catalog: {published:?}"
    );
    assert!(
        published.contains(&"cited"),
        "the researcher's own pattern is still published: {published:?}"
    );

    // The evidence half: the fragment's own span is matched and reported.
    let tokens = [token("論"), token("引用")];
    let matches = output.matcher.find_matches(&tokens);
    let synthesized: Vec<&Match> = matches
        .iter()
        .filter(|found| found.pattern == "__policy__citation")
        .collect();
    assert_eq!(
        synthesized.len(),
        1,
        "the synthesized emitter must produce the fragment's match: {matches:#?}"
    );
    assert_eq!((synthesized[0].start, synthesized[0].end), (1, 2));
}

/// The direct-matching counterpart of the execution seam.
///
/// `execute` applies policy for every run surface, but previews, playgrounds,
/// and embedders match a sentence directly and never reach it. Before this
/// existed the TUI playground published raw records — synthesized
/// `__policy__*` names among them — and lingrep-web's sample view held its own copy
/// of the rule. Both surfaces must show what the catalog publishes.
#[test]
fn find_published_matches_applies_policy_where_find_matches_does_not() {
    let output = compile(
        r#"
fragment citation := [text=引用].

cited := [text=論] [text=引用].

policy
  detach @cited with: @citation, relation: at_end, evidence: raw.
end
"#,
    )
    .expect("catalog compiles");

    let tokens = [token("論"), token("引用")];

    let raw = output.matcher.find_matches(&tokens);
    assert!(
        raw.iter()
            .any(|found| found.pattern == "__policy__citation"),
        "the canonical stream carries the evidence record: {raw:#?}"
    );

    let published = output
        .matcher
        .find_published_matches(&tokens)
        .expect("this catalog declares no anchor");
    assert!(
        !published
            .iter()
            .any(|found| found.pattern.starts_with("__policy__")),
        "no synthesized record may reach a published stream: {published:#?}"
    );
    let cited: Vec<&Match> = published
        .iter()
        .filter(|found| found.pattern == "cited")
        .collect();
    assert_eq!(
        cited.len(),
        1,
        "the researcher's match survives: {published:#?}"
    );
    assert_eq!(
        (cited[0].start, cited[0].end),
        (0, 1),
        "the declared trim shrank the span, so policy really ran"
    );
}

/// A catalog declaring nothing must not pay for the engine's existence, and
/// must not observe it either.
#[test]
fn find_published_matches_equals_find_matches_without_a_declaration() {
    let output = compile("plain := [text=論].").expect("catalog compiles");
    let tokens = [token("論")];

    assert_eq!(
        output
            .matcher
            .find_published_matches(&tokens)
            .expect("this catalog declares no anchor"),
        output.matcher.find_matches(&tokens)
    );
}

#[test]
fn a_referenced_pattern_synthesizes_nothing() {
    // It already emits, so there is nothing to observe that is not observed.
    let output = compile(
        r#"
pattern aside := [text=注].
pattern prose := [text=文].

policy
  exclude @prose with: @aside, relation: within, evidence: raw.
end
"#,
    )
    .expect("catalog compiles");
    assert!(
        output.matcher.catalog_policy().hidden.is_empty(),
        "{:#?}",
        output.matcher.catalog_policy()
    );
}

#[test]
fn one_fragment_referenced_twice_is_synthesized_once() {
    let output = compile(
        r#"
fragment copula := [lemma=だ].

first := [text=a] @copula.
second := [text=b].

policy
  detach @first with: @copula, relation: at_end, evidence: raw.
  exclude @second with: @copula, relation: within, evidence: raw.
end
"#,
    )
    .expect("catalog compiles");
    assert_eq!(output.matcher.catalog_policy().hidden.len(), 1);
}

#[test]
fn a_catalog_declaring_no_policy_is_untouched() {
    // The load-bearing guarantee: adding a policy engine must be invisible to
    // every catalog that does not ask for one. `patterns/sfe.lx` is the
    // largest
    // tracked catalog and declares no policy key.
    let sources = lingrep_core::testing::catalogs::tracked_catalog_sources();
    let (_, text) = sources
        .iter()
        .find(|(path, _)| path == "patterns/sfe.lx")
        .expect("patterns/sfe.lx is tracked");

    let output = compile_catalog(
        PatternSource::named("patterns/sfe.lx", text),
        CompileLimits::default(),
    )
    .expect("patterns/sfe.lx compiles");

    assert!(
        !output.matcher.catalog_policy().declared(),
        "patterns/sfe.lx declares no result policy: {:#?}",
        output.matcher.catalog_policy()
    );
    assert!(
        output.matcher.catalog_policy().hidden.is_empty(),
        "a catalog with no policy synthesizes nothing"
    );
    assert!(
        !output.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.code(),
            DiagnosticCode::SuspiciousPolicyKey
                | DiagnosticCode::IneffectivePolicy
                | DiagnosticCode::InvalidPolicyValue
        )),
        "no policy diagnostic may fire on a policy-free catalog: {:#?}",
        output.diagnostics
    );
}

#[test]
fn sf_experimental_declares_its_migrated_result_policy() {
    // Pin the tracked catalog's complete declared publication policy: thirteen
    // renames, thirteen subjects, and one hidden emitter.
    let sources = lingrep_core::testing::catalogs::tracked_catalog_sources();
    let (_, text) = sources
        .iter()
        .find(|(path, _)| path == "patterns/sf-experimental.lx")
        .expect("sf-experimental.lx is tracked");

    let output = compile_catalog(
        PatternSource::named("patterns/sf-experimental.lx", text),
        CompileLimits::default(),
    )
    .expect("sf-experimental.lx compiles");

    let policy = output.matcher.catalog_policy();
    let count = |predicate: fn(&lingrep_core::fsm::PatternPolicy) -> bool| {
        policy.entries().values().filter(|e| predicate(e)).count()
    };
    assert_eq!(
        count(|entry| entry.output_name.is_some()),
        13,
        "declared renames: {policy:#?}"
    );
    // Every rename still trims the copula it renames away, but the catalog now
    // says it once. Both halves are asserted: the membership that makes the
    // thirteen selectable, and the single statement that acts on them. Pinning
    // only the membership would stay green if the `detach` were deleted.
    assert_eq!(
        count(|entry| entry.groups.iter().any(|group| group == "trailing_copula")),
        13,
        "thirteen subjects are selectable as `%trailing_copula`: {policy:#?}"
    );
    assert_eq!(
        policy
            .group_index()
            .members_of("trailing_copula")
            .map(<[_]>::len),
        Some(13),
        "and the bound index agrees with the declarations: {policy:#?}"
    );

    // Subjects and operands are declared before any statement names them. A
    // group is inert until a statement selects it, which is what
    // lets the membership land as its own reviewable change — and what makes
    // this assertion meaningful on its own rather than a restatement of the
    // program below.
    //
    // `upper_layer` is every non-S0 pattern — **derived**, since the metadata
    // derivation migration, by `group upper_layer := layer in {"S1", "S2",
    // "S3"}.` rather than restated on 82 `groups:` lists — and
    // `s0_conditional` is the three S0 patterns that are not `だ`. The
    // asymmetry is semantic: `だ` survives a bare-S0 sentence by
    // being *absent* from the group, which is a declaration a single boolean
    // per definition could not make. The curated three stay literal lists:
    // they are upstream name-lists, not functions of any metadata key.
    for (group, expected) in [
        ("upper_layer", 82),
        ("s0_conditional", 3),
        ("aspect", 2),
        ("trailing_copula", 13),
    ] {
        assert_eq!(
            policy.group_index().members_of(group).map(<[_]>::len),
            Some(expected),
            "`%{group}` membership: {policy:#?}"
        );
    }
    // The per-layer groups left with the stage-1 collapse's follow-up: layer
    // identity lives in `layer:` metadata alone, and no statement names a
    // `%S<n>` any more, so the names are gone rather than inert.
    for gone in ["S0", "S1", "S2", "S3"] {
        assert_eq!(
            policy.group_index().members_of(gone),
            None,
            "`%{gone}` is no longer declared: {policy:#?}"
        );
    }

    let ops = &policy.program().ops;
    let detaches = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                lingrep_core::fsm::policy_program::BoundOp::Associate {
                    effect: lingrep_core::fsm::policy_program::Effect::Detach,
                    subject: lingrep_core::fsm::policy_program::BoundSubject::Group(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(detaches, 1, "one `detach` over the group: {policy:#?}");

    let admits = ops
        .iter()
        .filter(|op| matches!(op, lingrep_core::fsm::policy_program::BoundOp::Admit { .. }))
        .count();
    assert_eq!(
        admits, 2,
        "the distance rule and the という rule: {policy:#?}"
    );

    // Each referenced fragment synthesizes exactly one hidden emitter, and none
    // may ever be publishable. A fragment never emits, so an `exclude` naming
    // one needs an evidence emitter to have records to relate against.
    assert_eq!(
        policy
            .hidden
            .iter()
            .map(|(hidden, fragment)| (hidden.as_str(), fragment.as_str()))
            .collect::<Vec<_>>(),
        [("__policy__DA", "DA"), ("__policy__NEG", "NEG")]
    );
    assert!(
        output
            .matcher
            .patterns()
            .keys()
            .all(|name| !name.starts_with("__policy__")),
        "the synthesized emitter must stay out of the public catalog"
    );

    // Pin the complete program in authored order and authored text.
    //
    // The successor is stronger. Counting contests could not tell a `resolve`
    // over the wrong group from one over the right group, nor notice a
    // statement inserted at the wrong position — and position is semantic
    // here, because a `resolve` before an `exclude` contests a different field
    // than one after it. The ordered labels pin all three.
    //
    // The finished program: eight statements, in upstream's stage order —
    // stage 1's four per-layer resolves collapsed into one partitioned
    // resolve when the language gained `partition:`.
    let authored: Vec<&str> = policy
        .program()
        .labels()
        .iter()
        .map(lingrep_core::fsm::policy_program::StatementLabel::text)
        .collect();
    assert_eq!(
        authored,
        vec![
            "resolve all partition: layer, rank: [longest, start, end, pattern].",
            "exclude @ている with: @NEG, relation: after, evidence: raw.",
            "exclude @される with: %aspect, relation: after, evidence: current.",
            "resolve patterns rank: [longest, priority_order, start, end, pattern].",
            "detach %trailing_copula with: @DA, relation: at_end, evidence: raw.",
            "admit all terminal: {within: 5, anchor: propositional_end}.",
            "admit @という terminal: {exact: true, anchor: propositional_end}.",
            "require %s0_conditional with: %upper_layer, relation: in_sentence, evidence: current.",
        ],
        "{policy:#?}"
    );
}

/// The tracked catalog's compiled policy.
///
/// Shared so every policy-shape assertion reads the same compiled catalog.
fn sf_experimental_policy() -> lingrep_core::fsm::CatalogPolicy {
    let sources = lingrep_core::testing::catalogs::tracked_catalog_sources();
    let (_, text) = sources
        .iter()
        .find(|(path, _)| path == "patterns/sf-experimental.lx")
        .expect("sf-experimental.lx is tracked");
    compile_catalog(
        PatternSource::named("patterns/sf-experimental.lx", text),
        CompileLimits::default(),
    )
    .expect("sf-experimental.lx compiles")
    .matcher
    .catalog_policy()
    .clone()
}

/// Stage 1 is one partitioned resolve, contesting within each layer only.
///
/// It was four statements — one `resolve %S<n>` per layer — because a
/// `resolve`'s subject is a single competing set and the language had no
/// partition-by-dimension operator. `partition: layer` subdivides the selected
/// set by each definition's `layer` metadata and runs the contest per class,
/// so one statement now says what four did without resolving the layers
/// *against each other* — that cross-layer contest is stage 4's job, and the
/// two still differ in rank: within one layer there is no layer to break a
/// tie.
///
/// Asserted on the bound partition classes, not only on the statement text: a
/// `partition:` over the wrong key would satisfy a label check while
/// contesting the wrong classes.
#[test]
fn stage_one_resolves_within_each_layer_by_partition() {
    use lingrep_core::fsm::policy_program::BoundOp;

    let policy = sf_experimental_policy();
    let program = policy.program();

    // Stage 1 comes first: a `resolve` after an `exclude` contests a
    // different field than one before it.
    assert_eq!(
        program
            .labels()
            .first()
            .map(lingrep_core::fsm::policy_program::StatementLabel::text),
        Some("resolve all partition: layer, rank: [longest, start, end, pattern]."),
        "stage 1 is the one partitioned resolve, and it is first: {program:#?}"
    );

    let BoundOp::Resolve {
        partition: Some(classes),
        ..
    } = &program.ops[0]
    else {
        panic!("stage 1 carries a resolved partition map: {program:#?}");
    };
    let mut layers: Vec<&str> = classes
        .values()
        .map(compact_str::CompactString::as_str)
        .collect();
    layers.sort_unstable();
    layers.dedup();
    assert_eq!(
        layers,
        vec!["S0", "S1", "S2", "S3"],
        "the classes are the four layers: {classes:#?}"
    );
    assert_eq!(
        classes.len(),
        86,
        "every emitting pattern carries `layer`, so all 86 are partitioned"
    );

    // Stage 4's cross-layer resolve is still present and still unpartitioned:
    // resolving the layers against each other is its job alone.
    assert!(
        matches!(
            &program.ops[3],
            BoundOp::Resolve {
                partition: None,
                ..
            }
        ),
        "the cross-layer resolve does not partition: {program:#?}"
    );
}

/// The cross-layer resolve is the only statement that ranks by layer.
///
/// Within one layer there is no layer to break a tie, so `priority_order`
/// belongs in the cross-layer contest and not the per-layer resolves. A test
/// counting `resolve` statements, or asserting the criterion appears
/// *somewhere*, would pass with it in all five — which would silently make
/// a per-layer resolve adjudicate by a constant layer rank, where every contestant
/// has the same one.
///
/// The direction — lower wins, so `S3: 0` is outermost — is upstream's
/// `_LAYER_PRIORITY` sorted ascending, and is already pinned behaviourally by
/// `priority_order_decides_a_contest_the_earlier_criteria_tie` in
/// `policy_engine_program_test`. Not restated here: one behavioural pin is
/// enough, and a second written against this catalog would be a slower way to
/// ask the same question.
#[test]
fn only_the_cross_layer_resolve_ranks_by_priority_order() {
    let policy = sf_experimental_policy();

    let ranked_by_layer: Vec<&str> = policy
        .program()
        .labels()
        .iter()
        .map(lingrep_core::fsm::policy_program::StatementLabel::text)
        .filter(|text| text.contains("priority_order"))
        .collect();

    assert_eq!(
        ranked_by_layer,
        vec!["resolve patterns rank: [longest, priority_order, start, end, pattern]."],
        "exactly one statement ranks by layer: {policy:#?}"
    );
}

/// Every emitting pattern carries the layer rank the cross-layer `resolve`
/// reads.
///
/// A missing `priority_order:` is **not** a compile error — the key is optional
/// and `PatternPolicy::priority_order_or_default` answers `0`. So a pattern the
/// bulk edit skipped would silently rank as `0`, which is `S3`, the outermost
/// layer, and would win cross-layer contests it should lose. Nothing else in
/// the workspace would notice: the catalog compiles, the program runs, and the
/// only symptom is a research result that is quietly wrong.
///
/// Asserted as a count per rank rather than as "all 86 are present", because
/// the two failure modes are different. A skipped pattern and a
/// mis-transcribed layer both leave 86 patterns carrying the key; only the
/// distribution tells them apart.
#[test]
fn every_sf_experimental_pattern_declares_its_layer_rank() {
    let policy = sf_experimental_policy();

    let mut by_rank = std::collections::BTreeMap::new();
    for entry in policy.entries().values() {
        *by_rank.entry(entry.priority_order).or_insert(0usize) += 1;
    }

    // Upstream's `_LAYER_PRIORITY = {"S3": 0, "S2": 1, "S1": 2, "S0": 3}`
    // (`sf_experimental.py:1177`), outermost first, against this catalog's
    // 5 / 27 / 50 / 4 layer inventory.
    assert_eq!(
        by_rank,
        [(Some(0), 5), (Some(1), 27), (Some(2), 50), (Some(3), 4)]
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>(),
        "one rank per layer, and no pattern left without one: {policy:#?}"
    );
}

/// The distance rule reads the same declared anchor the という rule reads.
///
/// `filter_pipeline.py:100-101` passes one `effective_end` to both stages, so a
/// catalog where they resolved differently would mean the anchor pre-pass had
/// been consulted twice with different answers. Asserted on the two `Admit` ops
/// naming the same anchor, not on either one alone: `admit @という` alone has
/// been correct independently, and pinning its anchor a second time would not
/// verify the distance rule.
///
/// The pairing is also where a plausible mistake would land. `sentence_end` is
/// reserved and resolves without declaration, so a distance rule written against
/// it compiles, runs, and measures against the raw token count — an anchor the
/// catalog's other `admit` does not use. Upstream measures both against the same
/// `effective_end`, and this is the assertion that a divergence has to pass.
#[test]
fn both_admits_measure_against_the_same_declared_anchor() {
    let policy = sf_experimental_policy();

    let admits: Vec<(Terminal, &str)> = policy
        .program()
        .ops
        .iter()
        .filter_map(|op| match op {
            lingrep_core::fsm::policy_program::BoundOp::Admit {
                terminal, anchor, ..
            } => Some((*terminal, anchor.as_str())),
            _ => None,
        })
        .collect();

    assert_eq!(
        admits,
        vec![
            (Terminal::Within(5), "propositional_end"),
            (Terminal::Exact, "propositional_end"),
        ],
        "the distance rule and the exact rule share one anchor: {policy:#?}"
    );
}

// --- `groups:`, the group selector -------------------------------------------

/// A definition may belong to several groups, and `%g` selects it from each.
///
/// List membership permits the same definition to participate in overlapping
/// `detach` and `resolve` groups.
#[test]
fn a_definition_may_belong_to_several_groups() {
    let policy = catalog_policy_for(
        "groups: [\"a\", \"b\"]\np := [upos=NOUN].\n\
         groups: [\"b\"]\nq := [upos=VERB].\n",
    );
    let index = policy.group_index();
    assert_eq!(index.members_of("a").map(<[_]>::len), Some(1));
    assert_eq!(index.members_of("b").map(<[_]>::len), Some(2));
}

/// Membership alone adjudicates nothing.
///
/// A group is inert until a policy statement selects it; both overlapping
/// members publish here.
#[test]
fn group_membership_alone_adjudicates_nothing() {
    let matcher = matcher_for(
        "groups: [\"g\"]\np := [upos=NOUN] [upos=VERB].\n\
         groups: [\"g\"]\nq := [upos=VERB].\n",
    );
    let tokens = tokens_with_pos(&["NOUN", "VERB"]);
    let matches = matcher.find_matches(&tokens);
    let mut stats = lingrep_core::fsm::PolicyStats::default();
    let mut program =
        lingrep_core::fsm::ProgramStats::for_program(matcher.catalog_policy().program());
    let records = lingrep_core::fsm::apply_policy_records(
        matcher.catalog_policy(),
        &matches,
        &tokens,
        &matcher.anchor_positions(&tokens),
        &mut stats,
        &mut program,
        None,
    )
    .expect("no anchor is declared");
    assert_eq!(records.len(), 2, "overlapping group members both publish");
}

/// An empty list provides no groups, so it is rejected. Writing no key is how a
/// definition joins no group; an empty list is a value the author did not
/// finish.
#[test]
fn an_empty_groups_list_is_rejected() {
    let diagnostics = compile_errors("groups: []\np := [upos=NOUN].\n");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code() == DiagnosticCode::InvalidPolicyValue
                && d.message.contains("at least one group name")),
        "{diagnostics:?}"
    );
}

/// A non-string element is refused with the shape, not with a cast failure.
#[test]
fn a_groups_list_of_non_strings_is_refused() {
    let diagnostics = compile_errors("groups: [1]\np := [upos=NOUN].\n");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code() == DiagnosticCode::InvalidPolicyValue
                && d.message.contains("a list of strings")),
        "{diagnostics:?}"
    );
}

/// A group name must be spellable as `%name`.
///
/// A group that cannot be referenced can be declared and never selected — an
/// inert declaration that still looks executable. The accepted set is the
/// grammar's identifier, including non-ASCII, because definition names here are
/// routinely Japanese and a group has no reason to be stricter.
#[test]
fn a_group_name_that_cannot_be_written_as_a_reference_is_refused() {
    for name in [
        "",
        " ",
        "has space",
        "1leading",
        "has-dash",
        "%sigil",
        "a.b",
    ] {
        let source = format!("groups: [{name:?}]\np := [upos=NOUN].\n");
        let diagnostics = compile_errors(&source);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code() == DiagnosticCode::InvalidPolicyValue
                    && d.message.contains("`%name`")),
            "{name:?}: {diagnostics:?}"
        );
    }
}

/// The complement, so the rule cannot quietly become "reject everything".
#[test]
fn a_group_name_the_grammar_accepts_is_admitted() {
    for name in ["g", "g1", "_g", "aB_9", "ようだ"] {
        let source = format!("groups: [{name:?}]\np := [upos=NOUN].\n");
        let policy = catalog_policy_for(&source);
        assert!(
            policy.group_index().members_of(name).is_some(),
            "{name:?} should be a declarable group"
        );
    }
}

/// A repeated name is refused rather than deduplicated.
///
/// Deduplicating would make `["g", "g"]` and `["g"]` the same declaration, so a
/// catalog could carry a duplicate for years with nothing saying so. The author
/// meant one of them; which is not the compiler's guess to make.
#[test]
fn a_repeated_group_name_is_refused() {
    let diagnostics = compile_errors("groups: [\"g\", \"g\"]\np := [upos=NOUN].\n");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code() == DiagnosticCode::InvalidPolicyValue && d.message.contains("twice")),
        "{diagnostics:?}"
    );
}

// --- Transform vocabulary ----------------------------------------------------
//
// What extraction *accepts* is pinned end-to-end in `policy_transform_test.rs`,
// which drives authored metadata through to a published record. What is pinned
// here is what it **refuses**, because a refusal has no other observable
// consequence: an unusable declaration that compiles is a researcher writing a
// rule that silently does nothing, which is the failure this whole tier exists
// to close.

/// The message of the first fatal diagnostic, for asserting that a refusal
/// tells the author what to write instead.
fn refusal(text: &str, why: &str) -> String {
    let error = compile_err(text, why);
    match error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity() == DiagnosticSeverity::Error)
    {
        Some(diagnostic) => diagnostic.message.clone(),
        None => panic!("expected a fatal diagnostic: {why}"),
    }
}

/// A one-definition catalog carrying `line` as its only policy.
fn declaring(line: &str) -> String {
    format!("{line}\np := [text=a].\n")
}

#[test]
fn an_unknown_transform_operation_names_the_operations_that_exist() {
    let message = refusal(
        &declaring("transform: [{op: \"reticulate\"}]"),
        "an invented operation is fatal",
    );
    assert!(message.contains("reticulate"), "{message}");
    assert!(message.contains("`drop_edge`"), "{message}");
    assert!(message.contains("`join`"), "{message}");
}

#[test]
fn a_substitute_with_no_later_join_is_refused_as_provably_inert() {
    // In v1 the only reader of a substitution is `join`; the renderer registry
    // is empty. So this declaration would pass `check` and produce output
    // byte-identical to omitting it.
    let message = refusal(
        &declaring("transform: [{op: \"substitute\", component: \"last\", attr: \"lemma\"}]"),
        "a substitution nothing renders is fatal",
    );
    assert!(message.contains("join"), "{message}");
}

#[test]
fn a_join_before_a_substitute_does_not_satisfy_the_check() {
    // Order is the meaning of the list. A `join` that ran *before* the
    // substitution rendered a unit the substitution cannot reach.
    let message = refusal(
        &declaring(
            "transform: [{op: \"join\"}, {op: \"substitute\", component: \"last\", attr: \"lemma\"}]",
        ),
        "a join preceding the substitution renders the unsubstituted unit",
    );
    assert!(message.contains("join"), "{message}");
}

#[test]
fn a_drop_edge_without_an_edge_is_refused_rather_than_defaulted() {
    let message = refusal(
        &declaring(r#"transform: [{op: "drop_edge", drop: "[upos=AUX]"}, {op: "join"}]"#),
        "which edge is removed has no default",
    );
    assert!(message.contains("leading"), "{message}");
    assert!(message.contains("trailing"), "{message}");
}

#[test]
fn a_guard_naming_a_definition_points_at_the_key_that_takes_one() {
    // Reference-driven removal is a `policy` block's `detach`; admitting it
    // here too would ship two mechanisms answering
    // one question.
    let message = refusal(
        &declaring(
            r#"transform: [{op: "drop_edge", edge: "trailing", drop: "@DA"}, {op: "join"}]"#,
        ),
        "a reference is not a guard",
    );
    assert!(message.contains("detach"), "{message}");
}

#[test]
fn a_multi_token_guard_is_refused_where_the_author_wrote_it() {
    let message = refusal(
        &declaring(
            r#"transform: [{op: "drop_edge", edge: "trailing", drop: "[upos=AUX] [upos=ADP]"}, {op: "join"}]"#,
        ),
        "no operation can evaluate a guard wider than one token",
    );
    assert!(message.contains("single token predicate"), "{message}");
}

#[test]
fn a_substitute_targeting_more_than_one_token_says_why_it_is_deferred() {
    let message = refusal(
        &declaring(
            "transform: [{op: \"substitute\", component: \"span\", attr: \"lemma\"}, {op: \"join\"}]",
        ),
        "writing several tokens from one attribute has no defined meaning",
    );
    assert!(message.contains("first"), "{message}");
    assert!(message.contains("last"), "{message}");
}

#[test]
fn a_tag_map_mixing_sigilled_and_plain_keys_is_refused() {
    let message = refusal(
        &declaring("tags: {voice: {$head: \"lemma\", plain: \"active\"}}"),
        "the `$` sigil is what tells a projection from a presence mapping",
    );
    assert!(message.contains("$capture"), "{message}");
}

#[test]
fn a_tag_projecting_off_two_captures_is_refused_because_a_tag_holds_one_value() {
    let message = refusal(
        &declaring("tags: {voice: {$head: \"lemma\", $tail: \"lemma\"}}"),
        "one tag carries one value",
    );
    assert!(message.contains("exactly one"), "{message}");
}

#[test]
fn every_render_is_refused_while_the_registry_is_empty() {
    // The key and validation mechanism exist even while the registry is empty.
    let error = compile_err(
        &declaring("render: \"verb_unit\""),
        "an unregistered renderer is fatal",
    );
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DiagnosticCode::UnknownRenderer),
        "{:#?}",
        error.diagnostics
    );
}

#[test]
fn a_misspelled_transform_key_is_reported_as_a_near_miss() {
    // One typo detector covers every recognized key, which is the
    // reason `POLICY_KEYS` is one list rather than a parser-side set plus a
    // detector-side set.
    //
    // A plural, not a transposition: the detector measures **one** edit, and
    // `transfrom` is two (a swap is a deletion plus an insertion). That is a
    // property of the detector, not of this key, and it is pinned exhaustively
    // against full Levenshtein in `policy.rs`.
    let output =
        compile(&declaring("transforms: [{op: \"join\"}]")).expect("a warning still compiles");
    assert!(
        output.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::SuspiciousPolicyKey
                && diagnostic.message.contains("transform")
        }),
        "{:#?}",
        output.diagnostics
    );
}

#[test]
fn an_admit_mapping_carries_an_anchor_reference() {
    let policy = catalog_policy_for(&punct_anchor_with_terminal(
        "{within: 5, anchor: propositional_end}",
    ));
    let (terminal, anchor) = sole_admit(&policy);
    assert_eq!(terminal, Terminal::Within(5));
    assert_eq!(anchor, "propositional_end");
}

#[test]
fn an_admit_mapping_accepts_exact() {
    let policy = catalog_policy_for(PUNCT_ANCHOR);
    assert_eq!(sole_admit(&policy).0, Terminal::Exact);
}

/// The distance and anchor of the fixture's one `admit`.
fn sole_admit(policy: &lingrep_core::fsm::CatalogPolicy) -> (Terminal, String) {
    let mut found = policy.program().ops.iter().filter_map(|op| match op {
        lingrep_core::fsm::policy_program::BoundOp::Admit {
            terminal, anchor, ..
        } => Some((*terminal, anchor.to_string())),
        _ => None,
    });
    let admit = found.next().expect("the fixture declares one `admit`");
    assert!(found.next().is_none(), "the fixture declares only one");
    admit
}

#[test]
fn an_admit_mapping_is_closed_and_refuses_extra_fields() {
    let source =
        punct_anchor_with_terminal("{exact: true, anchor: propositional_end, edge: start}");
    let errors = compile_errors(&source);
    assert!(
        errors.iter().any(|d| d.message.contains("edge")),
        "an unknown `terminal:` field must be refused, not ignored; got {errors:?}"
    );
}

#[test]
fn an_admit_mapping_requires_exactly_one_distance() {
    // Each case pairs its input with a fragment of the refusal it must draw.
    // The code alone would not distinguish these from being refused by the
    // closed-field check, which would leave the distance rule untested.
    for (value, expected) in [
        ("{anchor: propositional_end}", "declares a distance"),
        (
            "{exact: true, within: 5, anchor: propositional_end}",
            "not both",
        ),
        // `exact: false` earns its own sentence rather than the generic one:
        // seeing `false` echoed back beside a demand for `true` leaves the
        // author unable to tell whether it is a typo or unsupported. It names
        // the *whole statement* as the thing to delete, because deleting only
        // `exact: false` leaves `{anchor: …}`, which draws the refusal above.
        (
            "{exact: false, anchor: propositional_end}",
            "delete the whole `admit` statement",
        ),
    ] {
        let source = punct_anchor_with_terminal(value);
        let errors = compile_errors(&source);
        assert!(
            errors.iter().any(|d| d.message.contains(expected)),
            "`terminal: {value}` must be refused for its distance, not incidentally; got {errors:?}"
        );
    }
}

/// An `admit` names its anchor bare, and a quoted one says so.
///
/// Refused at the surface rather than left to resolution: a quoted name would
/// otherwise be reported as an anchor that was never declared, which names the
/// consequence and not the mistake. This is the separation the metadata form
/// could not have — there, every value was quoted.
#[test]
fn an_admit_refuses_a_quoted_anchor() {
    for value in [
        r#"{within: 5, anchor: "propositional_end"}"#,
        "{within: 5, anchor: 7}",
    ] {
        let source = punct_anchor_with_terminal(value);
        let errors = compile_errors(&source);
        assert!(
            errors
                .iter()
                .any(|d| d.message.contains("without quoting it")),
            "`terminal: {value}` must be refused for its anchor; got {errors:?}"
        );
    }
}

/// An `admit` window wide enough to admit nearly every record warns.
///
/// The hazard is a property of the admitted distance, independent of syntax.
#[test]
fn an_implausibly_large_admit_window_warns() {
    let over = punct_anchor_with_distance(Terminal::Within(65));
    let warnings = compile_warnings(&over);
    assert!(
        warnings
            .iter()
            .any(|d| d.code() == DiagnosticCode::ImplausiblePolicyValue
                && d.message.contains("65")),
        "a window over 64 tokens admits nearly everything and must warn; got {warnings:?}"
    );

    let at = punct_anchor_with_distance(Terminal::Within(64));
    assert!(
        !compile_warnings(&at)
            .iter()
            .any(|d| d.code() == DiagnosticCode::ImplausiblePolicyValue),
        "64 is the boundary and must not warn"
    );
}

// ---------------------------------------------------------------------------
// The `anchor:` declaration
// ---------------------------------------------------------------------------

#[test]
fn an_anchor_declaration_records_its_exclude_fragment() {
    let policy = catalog_policy_for(PUNCT_ANCHOR);
    assert_eq!(
        policy
            .anchors
            .get("propositional_end")
            .map(CompactString::as_str),
        Some("TRAILING_APPARATUS")
    );
}

/// A flow sequence, not a block one: the grammar's `metadata_line` is one line,
/// so `anchor:` followed by indented `- …` items parses as three metadata lines
/// and is refused as a duplicate key.
#[test]
fn an_anchor_list_declares_several() {
    let catalog = r#"
anchor: [{name: "a", exclude: "F"}, {name: "b", exclude: "G"}]
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment F := [upos=PUNCT]+.

max_nesting: {depth: 0, witness: ["これは重要である。|"], divergence: ["これは重要である|（田中）。"]}
fragment G := [upos=PUNCT]+ [upos=PUNCT].

pattern one := [text=x].

pattern two := [text=y].

policy
  admit @one terminal: {exact: true, anchor: a}.
  admit @two terminal: {exact: true, anchor: b}.
end
"#;
    let anchors = catalog_policy_for(catalog).anchors;
    assert_eq!(anchors.len(), 2, "{anchors:?}");
    assert_eq!(anchors.get("a").map(CompactString::as_str), Some("F"));
    assert_eq!(anchors.get("b").map(CompactString::as_str), Some("G"));
}

/// Every malformed `anchor:` shape, with the code it draws and the word the
/// message must carry.
///
/// The code is the primary assertion; the message fragment is secondary, and
/// necessary because most of these cases share `InvalidAnchorDeclaration` and
/// the code alone would not tell them apart.
#[test]
fn anchor_diagnostics_refuse_each_malformed_shape() {
    let cases: &[(&str, DiagnosticCode, &str)] = &[
        (
            "anchor: {name: \"a\", exclude: \"P\"}\npattern P := [text=x].\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "fragment",
        ),
        (
            "anchor: {name: \"a\", exclude: \"F\"}\nfragment F := [text=x]?.\n",
            DiagnosticCode::NullablePattern,
            "",
        ),
        (
            "anchor: {name: \"a\", exclude: \"MISSING\"}\nfragment F := [text=x].\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "MISSING",
        ),
        (
            "anchor: [{name: \"a\", exclude: \"F\"}, {name: \"a\", exclude: \"F\"}]\nfragment F := [text=x].\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "declared twice",
        ),
        (
            "anchor: {name: \"a\", exclude: \"F\", edge: \"end\"}\nfragment F := [text=x].\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "edge",
        ),
        (
            "anchor: {name: \"a\"}\nfragment F := [text=x].\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "exclude",
        ),
        (
            "anchor: {name: \"a\", exclude: \"F\"}\nfragment F := (?<x> [text=y]).\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "capture",
        ),
        (
            "anchor: {name: \"a\", exclude: \"F\"}\nfragment F := (?<x> [text=y]) same_as($x).\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "same_as",
        ),
        (
            "anchor: \"propositional_end\"\nfragment F := [text=x].\n",
            DiagnosticCode::InvalidAnchorDeclaration,
            "mapping",
        ),
    ];
    for (source, code, fragment) in cases {
        let errors = compile_errors(source);
        assert!(
            errors
                .iter()
                .any(|d| d.code() == *code && d.message.contains(fragment)),
            "expected {code:?} mentioning `{fragment}` for:\n{source}\ngot {errors:?}"
        );
    }
}

/// One `anchor:` name may not mean two things.
///
/// Separate from the shape table because it is the *only* duplicate a metadata
/// block cannot produce by replication: a block attaches its keys to every
/// definition it wraps, so identical repeats are one claim and must stay legal.
#[test]
fn two_declarations_of_one_anchor_name_must_agree() {
    let conflicting = "anchor: {name: \"a\", exclude: \"F\"}\nfragment F := [text=x].\n\nanchor: {name: \"a\", exclude: \"G\"}\nfragment G := [text=y].\n";
    assert!(
        compile_errors(conflicting).iter().any(|d| {
            d.code() == DiagnosticCode::InvalidAnchorDeclaration
                && d.message.contains("declared twice")
        }),
        "an anchor excluding two different fragments has no answer to give"
    );
    // The repetition case: the identical `anchor:` declaration written twice
    // says one thing, so it is one anchor rather than a conflict. Only `F`
    // carries the claim, because only `F` is excluded — an author writes
    // `max_nesting:` on the definition it is about.
    let replicated = r#"
anchor: {name: "a", exclude: "F"}
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment F := [upos=PUNCT]+.
anchor: {name: "a", exclude: "F"}
fragment G := [upos=SYM]+.

pattern p := [text=x] @G.

policy
  admit @p terminal: {exact: true, anchor: a}.
end
"#;
    assert_eq!(catalog_policy_for(replicated).anchors.len(), 1);
}

/// A disagreement written inside one `anchor:` list is a disagreement.
///
/// It must not be told the second entry "says nothing the first does not" —
/// advice that, followed, silently changes which fragment the anchor strips.
/// The same mistake spelled as two blocks gets the same message.
#[test]
fn a_conflict_inside_one_anchor_list_is_reported_as_a_conflict() {
    let source = "anchor: [{name: \"a\", exclude: \"F\"}, {name: \"a\", exclude: \"G\"}]\nfragment F := [text=x].\nanchor: [{name: \"a\", exclude: \"F\"}, {name: \"a\", exclude: \"G\"}]\nfragment G := [text=y].\n";
    let errors = compile_errors(source);
    assert!(
        errors.iter().any(|d| {
            d.code() == DiagnosticCode::InvalidAnchorDeclaration
                && d.message.contains("`F`")
                && d.message.contains("`G`")
        }),
        "the two excludes disagree and the message must name both; got {errors:?}"
    );
    // A true repeat keeps the "says nothing the first does not" advice, which
    // is only safe when the two entries really are the same claim.
    let repeat = "anchor: [{name: \"a\", exclude: \"F\"}, {name: \"a\", exclude: \"F\"}]\nfragment F := [text=x].\n";
    assert!(
        compile_errors(repeat).iter().any(|d| {
            d.code() == DiagnosticCode::InvalidAnchorDeclaration
                && d.message.contains("says nothing the first does not")
        }),
        "an identical repeat is safe to delete and should say so"
    );
}

/// A malformed `anchor:` on a multi-definition block is reported once.
///
/// Block metadata clones onto every definition it wraps, so reading each copy
/// printed the identical error once per definition, each naming a different
/// definition as the culprit for a mistake written once.
#[test]
fn a_malformed_anchor_on_a_multi_definition_block_is_reported_once() {
    let source = "anchor: {name: \"a\", exclude: \"MISSING\"}\nfragment F := [text=x].\nanchor: {name: \"a\", exclude: \"MISSING\"}\nfragment G := [text=y].\n";
    let reports = compile_errors(source)
        .into_iter()
        .filter(|d| d.code() == DiagnosticCode::InvalidAnchorDeclaration)
        .count();
    assert_eq!(reports, 1, "one mistake, one report");
}

/// Lookaround is permitted inside an `exclude`, and the assertion compiler
/// walks nested lookaround bodies.
///
/// Expressed as a successful compile — `compile_errors` panics on success by
/// design — with the recorded declaration as the evidence it was accepted.
#[test]
fn an_exclude_fragment_may_use_lookaround() {
    let catalog = PUNCT_ANCHOR.replace(
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+.",
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+ not_preceded_by([text=な]).",
    );
    // The assertion below holds for unmodified `PUNCT_ANCHOR` too, so without
    // this the test would keep passing the day the needle drifts and stop
    // exercising lookaround at all. Its `assert_ne!` siblings guard themselves.
    assert!(
        catalog.contains("not_preceded_by"),
        "the lookaround edit must have applied"
    );
    assert_eq!(
        catalog_policy_for(&catalog)
            .anchors
            .get("propositional_end")
            .map(CompactString::as_str),
        Some("TRAILING_APPARATUS")
    );
}

/// An `exclude` is walked as an assertion body, so a *positive single-token*
/// `preceded_by` inside one both compiles and reaches the projection.
///
/// Not covered by the sibling above, which uses `not_preceded_by`: negative
/// polarity is never specialized, so that one takes the general path either way.
/// This shape is the one the assertion compiler declines to register an
/// occurrence for at the top level, because it lowers to
/// `PositionAssertion::PreviousToken` and needs none. An `exclude` is compiled
/// as an assertion body, where that specialization is unavailable — so unless
/// the fragment is walked as a body, interning it looks up an occurrence that
/// was never created and aborts the process on a valid catalog.
///
/// The lookaround reads `lemma`, which nothing else in [`PUNCT_ANCHOR`] claims,
/// and the assertion is that `lemma` reaches the projection. That fails unless
/// the exclude was walked as an assertion body, so the test states its own
/// subject rather than restating the anchors map its sibling above already
/// pins — and it additionally shows a *nested* program's attributes arriving,
/// not just the outer fragment's.
#[test]
fn an_exclude_holding_a_specialized_lookaround_is_walked_as_an_assertion_body() {
    let catalog = PUNCT_ANCHOR.replace(
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+.",
        "fragment TRAILING_APPARATUS := preceded_by([lemma=な]) [upos=PUNCT]+.",
    );
    assert!(
        catalog.contains("preceded_by([lemma=な])"),
        "the lookaround edit must have applied"
    );
    let matcher = matcher_for(&catalog);
    assert!(
        matcher
            .required_attributes()
            .contains(&TokenAttributeIndex::Lemma.into()),
        "an attribute read only inside a lookaround inside an exclude must reach \
         the projection; got {:?}",
        matcher.required_attributes()
    );
}

/// An `admit` naming an anchor the catalog never declares is refused.
///
/// Left unchecked, the runtime `AnchorError::Unknown` that `AnchorPositions`
/// documents as unreachable from a compiled catalog would be reachable — and it
/// would surface per sentence, as a wiring error, with no source span.
#[test]
fn an_admit_naming_an_undeclared_anchor_is_refused() {
    let catalog = "pattern p := [text=x].\n\
                   policy\n  admit @p terminal: {exact: true, anchor: nowhere}.\nend\n";
    assert!(
        compile_errors(catalog).iter().any(|d| {
            d.code() == DiagnosticCode::InvalidPolicyValue && d.message.contains("nowhere")
        }),
        "an `admit` naming an anchor the catalog never declares must be refused"
    );
}

/// An `admit` is a reference, so it answers the unreferenced-anchor warning.
///
/// The warning says an anchor cannot affect any record. An `admit` measuring
/// against it is precisely a record it affects, so warning here would tell an
/// author to delete a declaration their program depends on.
#[test]
fn an_anchor_measured_against_only_by_an_admit_is_referenced() {
    // `PUNCT_ANCHOR` declares one anchor and one `admit` measuring against it.
    let warnings = compile_warnings(PUNCT_ANCHOR);
    assert!(
        !warnings
            .iter()
            .any(|d| d.code() == DiagnosticCode::UnreferencedAnchor),
        "the `admit` measures against it; got {warnings:?}"
    );
}

/// An `admit` naming nothing is a *declaration* error, and this module reads
/// declarations before binding precisely so one does not wait behind a body
/// error to be seen.
///
/// `anchor: 7` is refused at the surface, and `anchor: nowhere` must be
/// reported beside it rather than swallowed by an unrelated nullable pattern.
#[test]
fn an_unresolvable_anchor_does_not_queue_behind_a_body_error() {
    let source = "pattern p := [text=x].\n\npattern always_empty := [text=y]*.\n\
                  policy\n  admit @p terminal: {exact: true, anchor: nowhere}.\nend\n";
    let errors = compile_errors(source);
    assert!(
        errors.iter().any(|d| {
            d.code() == DiagnosticCode::InvalidPolicyValue && d.message.contains("nowhere")
        }),
        "the unresolvable anchor must be reported even though the catalog also \
         has a body error; got {errors:?}"
    );
}

/// Two anchors may share one `exclude`, so the unreferenced warning points at
/// the definition carrying the declaration rather than at the fragment.
///
/// Pointing at the fragment reported an unreferenced anchor against a
/// definition that is used by the other anchor, which is the wrong place to
/// look for the line to delete. It must point at the `anchor:` key itself.
#[test]
fn an_unreferenced_anchor_points_at_its_own_declaration() {
    let catalog = r#"
anchor: {name: "measured", exclude: "TRAILING_APPARATUS"}
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment TRAILING_APPARATUS := [upos=PUNCT]+.

anchor: {name: "unmeasured", exclude: "TRAILING_APPARATUS"}
fragment ALSO_HERE := [upos=SYM]+.

pattern p := [text=x] @ALSO_HERE.

policy
  admit @p terminal: {exact: true, anchor: measured}.
end
"#;
    let warnings = compile_warnings(catalog);
    let unreferenced: Vec<_> = warnings
        .iter()
        .filter(|d| d.code() == DiagnosticCode::UnreferencedAnchor)
        .collect();
    assert_eq!(unreferenced.len(), 1, "{warnings:?}");
    let range = unreferenced[0].location().byte_range();
    assert_eq!(
        &catalog[range.clone()],
        "anchor",
        "the warning names the key that declares the unreferenced anchor"
    );
    assert_eq!(
        range.start,
        catalog
            .rfind("anchor: {name: \"unmeasured\"")
            .expect("the unmeasured declaration is present"),
        "the `unmeasured` declaration is reported, not the `measured` one above it \
         and not the exclude fragment they share"
    );
}

/// A declared anchor nothing measures against is inert, and inert and honored
/// look identical from the catalog — so it warns rather than compiling silently.
#[test]
fn an_unreferenced_anchor_warns() {
    let catalog = PUNCT_ANCHOR.replace(
        "anchor: {name: \"propositional_end\", exclude: \"TRAILING_APPARATUS\"}",
        "anchor: [{name: \"propositional_end\", exclude: \"TRAILING_APPARATUS\"}, \
         {name: \"unmeasured_end\", exclude: \"TRAILING_APPARATUS\"}]",
    );
    let warnings = compile_warnings(&catalog);
    assert!(
        warnings.iter().any(|d| {
            d.code() == DiagnosticCode::UnreferencedAnchor && d.message.contains("unmeasured_end")
        }),
        "got {warnings:?}"
    );
    assert!(
        !warnings
            .iter()
            .any(|d| d.code() == DiagnosticCode::UnreferencedAnchor
                && d.message.contains("`propositional_end`")),
        "the referenced anchor must not warn; got {warnings:?}"
    );
}

/// An `exclude` fragment is referenced — by the anchor declaration.
///
/// Reachability was seeded from emitting definitions only, so declaring an
/// anchor over a fragment nothing else mentions reported it as never referenced
/// and unable to match. Both halves of that sentence are false of an anchor's
/// recognizer.
#[test]
fn an_exclude_fragment_is_not_reported_unused() {
    let warnings = compile_warnings(PUNCT_ANCHOR);
    assert!(
        !warnings
            .iter()
            .any(|d| d.code() == DiagnosticCode::UnusedFragment),
        "an anchor's exclude fragment is referenced by the declaration; got {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// The anchor-set digest
// ---------------------------------------------------------------------------

#[test]
fn structurally_identical_catalogs_share_a_digest() {
    assert_eq!(
        catalog_policy_for(PUNCT_ANCHOR).anchor_digest,
        catalog_policy_for(PUNCT_ANCHOR).anchor_digest
    );
}

#[test]
fn a_different_exclude_predicate_changes_the_digest() {
    assert_ne!(
        punct_anchor_matcher().catalog_policy().anchor_digest,
        sym_anchor_matcher().catalog_policy().anchor_digest
    );
}

/// The root expression is byte-identical in both; only a transitively
/// referenced helper differs. Hashing the root alone would call these equal.
#[test]
fn a_changed_transitive_helper_changes_the_digest() {
    let base = r#"
fragment INNER := [upos=PUNCT].
anchor: {name: "a", exclude: "OUTER"}
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment OUTER := @INNER+.

pattern p := [text=x].

policy
  admit @p terminal: {exact: true, anchor: a}.
end
"#;
    // `[unidic.pos2=句点]` rather than a body the fixture's own `max_nesting:` claim
    // would stop being true of: both bodies recognize a trailing `。` and
    // neither recognizes `（`, so the depth-0 claim holds either way while the
    // digest still has to notice the substitution. Checked by hand — nothing
    // tokenizes this fixture, so no test would notice if it stopped holding.
    let changed = base.replace(
        "fragment INNER := [upos=PUNCT].",
        "fragment INNER := [unidic.pos2=句点].",
    );
    assert_ne!(
        catalog_policy_for(base).anchor_digest,
        catalog_policy_for(&changed).anchor_digest
    );
}

/// Only a nested lookaround body differs.
#[test]
fn a_changed_nested_lookaround_body_changes_the_digest() {
    let base = PUNCT_ANCHOR.replace(
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+.",
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+ not_preceded_by([text=な]).",
    );
    let changed = base.replace("[text=な]", "[text=に]");
    assert_ne!(
        catalog_policy_for(&base).anchor_digest,
        catalog_policy_for(&changed).anchor_digest
    );
}

/// Polarity is semantics, not decoration.
#[test]
fn a_changed_lookaround_polarity_changes_the_digest() {
    let base = PUNCT_ANCHOR.replace(
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+.",
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+ not_preceded_by([text=な]).",
    );
    let changed = base.replace("not_preceded_by", "preceded_by");
    assert_ne!(
        catalog_policy_for(&base).anchor_digest,
        catalog_policy_for(&changed).anchor_digest
    );
}

#[test]
fn a_changed_repetition_bound_changes_the_digest() {
    let base = PUNCT_ANCHOR.replace("[upos=PUNCT]+", "[upos=PUNCT]{1,3}");
    let changed = PUNCT_ANCHOR.replace("[upos=PUNCT]+", "[upos=PUNCT]{1,4}");
    assert_ne!(
        catalog_policy_for(&base).anchor_digest,
        catalog_policy_for(&changed).anchor_digest
    );
}

/// Declaring nothing changes nothing: a catalog with no anchors carries the
/// default digest, not the hash of an empty walk.
#[test]
fn a_catalog_with_no_anchors_has_the_empty_digest() {
    assert_eq!(
        catalog_policy_for("pattern p := [text=x].\n").anchor_digest,
        AnchorSetDigest::default()
    );
    assert_ne!(
        matcher_for(PUNCT_ANCHOR).catalog_policy().anchor_digest,
        AnchorSetDigest::default(),
        "a catalog that declares one must not carry the empty digest"
    );
}

/// An `exclude` fragment's attributes must reach the cache projection.
///
/// `pos` is used by nothing else in [`PUNCT_ANCHOR`]: the only emitting pattern
/// reads `orth`, and the fragment is named only by the `anchor:` declaration.
/// Nothing about the projection is inherited from *declaring* an anchor — the
/// projection is the union over installed assertion programs, so the fragment
/// has to be interned as one. A catalog whose anchor reads an attribute the
/// cache did not project would evaluate that anchor against absent values and
/// silently pick the wrong boundary.
#[test]
fn an_attribute_used_only_in_an_anchor_exclude_is_projected() {
    let matcher = punct_anchor_matcher();
    assert!(
        matcher
            .required_attributes()
            .contains(&TokenAttributeIndex::Pos.into()),
        "an anchor exclude's attributes must reach the projection, as a \
         lookaround body's do; got {:?}",
        matcher.required_attributes()
    );
}

/// A declared anchor's `max_nesting` reaches the compiled matcher, split.
///
/// The sentinel is resolved at compile time, so a stored entry cannot carry a
/// spelling the verifier would have to re-validate. What the tokenizer is
/// handed is the sentence *without* the marker.
#[test]
fn a_nesting_claim_reaches_the_compiled_matcher() {
    let matcher = punct_anchor_matcher();
    let claims = matcher.nesting_claims();
    assert_eq!(claims.len(), 1, "{claims:?}");
    let claim = claims
        .get("propositional_end")
        .expect("keyed by anchor name, not by the exclude fragment");
    assert_eq!(claim.depth, 0);
    assert_eq!(claim.witness.len(), 1);
    assert_eq!(claim.divergence.len(), 1);
    assert_eq!(claim.witness[0].sentence(), "これは重要である。");
    assert_eq!(claim.witness[0].sentinel(), 8);
    assert_eq!(claim.witness[0].to_string(), "これは重要である|。");
}

/// An `exclude` fragment without a declared depth bound is a recognizer whose
/// approximation nobody recorded.
#[test]
fn an_exclude_fragment_without_a_nesting_claim_is_refused() {
    let source = PUNCT_ANCHOR
        .lines()
        .filter(|line| !line.trim_start().starts_with("max_nesting:"))
        .collect::<Vec<_>>()
        .join("\n");
    let errors = compile_errors(&source);
    assert!(
        errors.iter().any(|d| {
            d.code() == DiagnosticCode::InvalidNestingClaim && d.message.contains("max_nesting")
        }),
        "got {errors:?}"
    );
}

/// Every malformed `max_nesting` shape is refused without a dictionary.
///
/// The claim's own encoding is what makes `depth:` impossible to leave inert,
/// so a shape that would let a depth stand alone — or a witness whose expected
/// anchor cannot be located — has to be refused before anything is executed.
#[test]
fn a_malformed_nesting_claim_reports_without_a_dictionary() {
    for value in [
        "{depth: 0}",
        r#"{depth: 0, witness: [], divergence: ["a|b"]}"#,
        r#"{depth: 0, witness: ["これは重要である|。"], divergence: []}"#,
        r#"{depth: 0, witness: ["no sentinel"], divergence: ["a|b"]}"#,
        r#"{depth: 0, witness: ["two|sentinels|here"], divergence: ["a|b"]}"#,
        r#"{depth: -1, witness: ["a|b"], divergence: ["c|d"]}"#,
        r#"{depth: 0, witness: ["a|b"], divergence: ["c|d"], upstream: "x"}"#,
        "2",
    ] {
        let source = PUNCT_ANCHOR.replace(
            r#"{depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}"#,
            value,
        );
        let errors = compile_errors(&source);
        assert!(
            errors
                .iter()
                .any(|d| d.code() == DiagnosticCode::InvalidNestingClaim),
            "{value}: got {errors:?}"
        );
    }
}

/// A claim nothing executes is as inert as an unchecked number.
#[test]
fn a_nesting_claim_on_a_fragment_that_is_no_anchor_exclude_is_refused() {
    let source = PUNCT_ANCHOR.to_owned()
        + "\nmax_nesting: {depth: 0, witness: [\"a|b\"], divergence: [\"c|d\"]}\nfragment UNCLAIMED := [upos=SYM]+.\n";
    let errors = compile_errors(&source);
    assert!(
        errors
            .iter()
            .any(|d| d.code() == DiagnosticCode::InvalidNestingClaim
                && d.message.contains("UNCLAIMED")),
        "got {errors:?}"
    );
}

/// A metadata block attaches its keys to every definition it wraps, so one
/// `max_nesting:` line on a two-fragment block arrives twice — once on the
/// `exclude` fragment and once on a fragment that is no anchor's exclude.
///
/// Reading the replica as a second, unreferenced claim would refuse the
/// recommended layout: `anchor:` and `max_nesting:` on the block holding the
/// recognizer's parts.
#[test]
fn a_nesting_claim_replicated_across_a_block_is_read_once() {
    let source = PUNCT_ANCHOR.replace(
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+.",
        "fragment TRAILING_APPARATUS := [upos=PUNCT]+ @TAIL.\nfragment TAIL := [upos=PUNCT].",
    );
    assert_eq!(catalog_policy_for(&source).nesting_claims.len(), 1);
}

/// A nullable `exclude` is refused as a nullable pattern, and that refusal must
/// not be displaced by the missing-claim one.
///
/// Both are true of the same catalog. The nullability is the cause — an anchor
/// whose recognizer matches the empty span lands on every boundary — so it is
/// the diagnostic the author needs, and it comes from binding while the claim
/// check does not.
#[test]
fn a_nesting_claim_on_a_nullable_fragment_names_nullable_pattern() {
    let source = PUNCT_ANCHOR.replace("[upos=PUNCT]+.", "[upos=PUNCT]?.");
    let errors = compile_errors(&source);
    assert!(
        errors
            .iter()
            .any(|d| d.code() == DiagnosticCode::NullablePattern),
        "got {errors:?}"
    );
}

/// `max_nesting` is engine-recognized, so a misspelling of it is inert — and
/// inert and honored look identical from the catalog.
///
/// Written on the emitting pattern's block, leaving the real claim in place:
/// misspelling *that* one draws the missing-claim error instead, which is a
/// different report about a different mistake.
#[test]
fn a_misspelled_max_nesting_is_suggested() {
    let warnings = compile_warnings(
        &PUNCT_ANCHOR.replace("pattern toiu :=", "max_nestng: 2\npattern toiu :="),
    );
    assert!(
        warnings.iter().any(|d| {
            d.code() == DiagnosticCode::SuspiciousPolicyKey && d.message.contains("max_nesting")
        }),
        "got {warnings:?}"
    );
}

/// A byte-identical claim in a *separate* block is a second claim.
///
/// The replica rule tolerates one `max_nesting:` line reaching several
/// definitions because a metadata block attaches its keys to every definition
/// it wraps. Value identity does not establish that: the claim is attached to
/// whichever fragment the block happens to wrap, so two blocks spelling the
/// same claim are two claims about two fragments, and the second is attached to
/// a fragment no anchor excludes.
///
/// The asymmetry with `read_anchor_declarations`' `already_read` is the point.
/// Two identical `anchor: {name: a, exclude: F}` values genuinely say the same
/// thing wherever they are written, because the declaration names its own
/// subject. A claim does not — its subject is the definition carrying it.
#[test]
fn an_identical_claim_in_a_separate_block_is_still_unreferenced() {
    let source = PUNCT_ANCHOR.to_owned()
        + "\nmax_nesting: {depth: 0, witness: [\"これは重要である|。\"], divergence: [\"これは重要である|（田中）。\"]}\nfragment UNCLAIMED := [upos=SYM]+.\n";
    let errors = compile_errors(&source);
    assert!(
        errors
            .iter()
            .any(|d| d.code() == DiagnosticCode::InvalidNestingClaim
                && d.message.contains("UNCLAIMED")),
        "got {errors:?}"
    );
}

/// A nullable `exclude` carrying no claim is told about the nullability.
///
/// This is what makes `read_nesting_claims`' placement after `bind` load-bearing
/// rather than merely reasoned. Both faults are real here, and `compile_program`
/// returns on `policy_errors` before binding — so reading claims pre-bind would
/// report the missing claim and never reach `resolve_anchors`, telling the
/// author to document the depth of a recognizer that can never be built.
///
/// `a_nesting_claim_on_a_nullable_fragment_names_nullable_pattern` does not
/// cover this: it replaces only the fragment body, so the claim stays present
/// and well-formed and no claim error arises either way.
#[test]
fn a_nullable_exclude_without_a_claim_still_names_nullable_pattern() {
    let source = PUNCT_ANCHOR
        .replace("[upos=PUNCT]+.", "[upos=PUNCT]?.")
        .lines()
        .filter(|line| !line.trim_start().starts_with("max_nesting:"))
        .collect::<Vec<_>>()
        .join("\n");
    let errors = compile_errors(&source);
    assert!(
        errors
            .iter()
            .any(|d| d.code() == DiagnosticCode::NullablePattern),
        "the nullability is the cause, and the diagnostic the author needs; got {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|d| d.code() == DiagnosticCode::InvalidNestingClaim),
        "the missing claim must not displace it; got {errors:?}"
    );
}

/// A statement's label is the researcher's own text, not a re-rendering.
///
/// Reconstructing the statement from the bound IR would be a second spelling of
/// the language, free to drift from the grammar and from itself. Slicing the
/// source cannot drift: if the label and the file disagree, the span is wrong —
/// and the span is what every policy diagnostic already points with, so a
/// disagreement here is a defect those diagnostics share.
#[test]
fn a_statement_label_is_its_source_text_and_line() {
    let matcher = matcher_for(
        "pattern a := [upos=NOUN].\n\
         pattern b := [upos=VERB].\n\
         policy\n  \
         resolve all rank: [longest, start, end, pattern].\n\
         end\n",
    );
    let labels = matcher.catalog_policy().program().labels();
    assert_eq!(labels.len(), 1, "one statement");
    assert_eq!(
        labels[0].text(),
        "resolve all rank: [longest, start, end, pattern]."
    );
    assert_eq!(labels[0].line(), 4, "1-based, counting the policy header");
}

/// A statement broken across lines labels on one line, at its first.
///
/// The label is a display artifact for a progress channel that writes whole
/// lines, so an embedded newline would corrupt that surface rather than inform
/// it. Collapsing runs of whitespace also makes the label independent of the
/// author's indentation, which no reader of a trace is asking about.
#[test]
fn a_multi_line_statement_collapses_to_one_label_line() {
    let matcher = matcher_for(
        "fragment DA := [lemma=だ].\n\
         pattern a := [upos=NOUN].\n\
         policy\n  \
         detach @a\n    \
         with: @DA,\n    \
         relation: at_end,\n    \
         evidence: raw.\n\
         end\n",
    );
    let labels = matcher.catalog_policy().program().labels();
    assert_eq!(
        labels[0].text(),
        "detach @a with: @DA, relation: at_end, evidence: raw."
    );
    assert_eq!(labels[0].line(), 4);
}

/// Every operation has a label, at its own index.
///
/// The two vectors are parallel and every reader `zip`s them, which truncates
/// silently when they disagree. A reporting surface that quietly names fewer
/// statements than the catalog declares is the exact failure this tier exists
/// to prevent, so the agreement is asserted rather than assumed.
#[test]
fn labels_and_operations_stay_index_aligned() {
    let matcher = matcher_for(
        "fragment DA := [lemma=だ].\n\
         pattern a := [upos=NOUN].\n\
         policy\n  \
         resolve all rank: [longest, start, end, pattern].\n  \
         detach @a with: @DA, relation: at_end, evidence: raw.\n  \
         admit @a terminal: {exact: true, anchor: sentence_end}.\n\
         end\n",
    );
    let program = matcher.catalog_policy().program();
    assert_eq!(program.ops.len(), 3);
    assert_eq!(program.labels().len(), program.ops.len());
    assert!(program.labels()[2].text().starts_with("admit @a"));
}

// --- publish: ----------------------------------------------------------------

/// `publish:` says what a record must carry, without saying anything about
/// what it matches.
///
/// The witnessed consumer is character-offset reconstruction from CoNLL-U,
/// which needs `conllu.misc` for `SpaceAfter=No`. Before this key the only
/// spelling was a predicate written never to fire —
/// `[upos=VERB & !(conllu.misc has __never__)]` — which works only because a
/// true complement holds for a missing attribute, and reads as a mistake.
#[test]
fn a_published_attribute_joins_the_projection_without_constraining_anything() {
    let plain = matcher_for("pattern p := [upos=VERB].\n");
    let declared = matcher_for("publish: [\"conllu.misc\"]\npattern p := [upos=VERB].\n");

    let names = |matcher: &lingrep_core::fsm::CompiledCatalog| {
        matcher
            .execution_attributes()
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    };
    assert!(!names(&plain).contains(&"conllu.misc".to_owned()));
    assert!(names(&declared).contains(&"conllu.misc".to_owned()));

    // And it constrains nothing: the two catalogs match identically.
    let tokens = tokens_with_pos(&["VERB", "NOUN"]);
    assert_eq!(plain.find_matches(&tokens), declared.find_matches(&tokens));
}

/// Unlike every operator, `publish:` names no value kind: nothing reads the
/// attribute, so a string-set one is carried as a string set and a scalar one
/// as a scalar. A kind requirement here would refuse exactly the case the key
/// was added for.
#[test]
fn a_published_attribute_binds_against_either_value_kind() {
    use lingrep_core::token_match::{AttributeKind, SchemaAttribute, TokenSchema};

    let matcher = matcher_for("publish: [\"conllu.misc\"]\npattern p := [upos=VERB].\n");
    for kind in [AttributeKind::Scalar, AttributeKind::StringSet] {
        let schema = TokenSchema::new([
            SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("conllu.misc", kind).unwrap(),
        ])
        .unwrap();
        // Through the projection a run actually binds with, which is what
        // carries publication attributes; `bind` alone resolves matching.
        let bound = matcher
            .bind_with_projection(
                &schema,
                lingrep_core::execution::cached_run_attributes(&matcher),
            )
            .unwrap_or_else(|error| panic!("{kind:?} must bind: {error}"));
        assert!(
            bound
                .projection()
                .keys()
                .iter()
                .any(|key| key.as_str() == "conllu.misc"),
            "{kind:?}"
        );
    }
}

#[test]
fn an_unusable_publish_list_is_refused() {
    for (line, expected) in [
        ("publish: []", "a non-empty list"),
        ("publish: \"conllu.misc\"", "a non-empty list"),
        ("publish: [1]", "a list of strings"),
        ("publish: [\"Not An Attribute\"]", "not a token attribute"),
        ("publish: [\"conllu.misc\", \"conllu.misc\"]", "named twice"),
    ] {
        let message = refusal(&declaring(line), line);
        assert!(message.contains(expected), "{line}: {message}");
    }
}

/// A fragment never emits, so it publishes nothing — the same warning every
/// other publication key draws there.
#[test]
fn publish_on_a_fragment_is_ineffective_policy() {
    let warnings =
        compile_warnings("publish: [\"norm\"]\nfragment f := [upos=VERB].\npattern p := @f.\n");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code() == DiagnosticCode::IneffectivePolicy),
        "{warnings:?}"
    );
}

#[test]
fn published_entries_keep_the_first_representative_in_the_callers_order() {
    let matcher = matcher_for(
        r#"
output_name: "shared"
first := [text=a].
output_name: "shared"
second := [text=b].
other := [text=c].
"#,
    );
    let policy = matcher.catalog_policy();
    let entries = [("second", 20), ("other", 30), ("first", 10), ("second", 40)];
    assert_eq!(
        policy.published_entries(entries).collect::<Vec<_>>(),
        [("shared", 20), ("other", 30)]
    );
    assert_eq!(
        policy.published_names(entries.iter().map(|(name, _)| *name)),
        ["shared", "other"]
    );
}
