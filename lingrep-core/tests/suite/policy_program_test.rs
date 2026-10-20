//! Lowering, static rules and ownership for the ordered policy program.
//!
//! The IR itself is unit-tested in `lingrep_core::fsm::policy_program`; these are
//! the tests that need a real compile.

// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_used,
    clippy::expect_used
)]

use lingrep_core::fsm::policy_program::BoundOp;
use lingrep_core::fsm::{
    CompileLimits, CompileOutput, DiagnosticSeverity, PatternSource, compile_catalog,
};

fn compile(source: &str) -> CompileOutput {
    compile_catalog(
        PatternSource::named("t.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("must compile: {error}"))
}

fn refusal(source: &str) -> String {
    match compile_catalog(
        PatternSource::named("t.lx", source),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("must be refused: {source}"),
        Err(error) => error.to_string(),
    }
}

const DEFS: &str = "a := [text=あ].\nfragment N := [text=ぬ].\n";

#[test]
fn a_policy_block_lowers_to_ordered_operations() {
    let output = compile(&format!(
        "{DEFS}policy\n  \
         resolve all rank: [longest, start, end, pattern].\n  \
         exclude @a with: @N, relation: after, evidence: raw.\n\
         end\n"
    ));
    let ops = &output.matcher.catalog_policy().program().ops;
    assert_eq!(ops.len(), 2);
    assert!(matches!(ops[0], BoundOp::Resolve { .. }));
    assert!(matches!(ops[1], BoundOp::Associate { .. }));
}

#[test]
fn authored_order_is_preserved_because_it_is_semantic() {
    let output = compile(&format!(
        "{DEFS}policy\n  \
         exclude @a with: @N, relation: after, evidence: raw.\n  \
         resolve all rank: [longest, start, end, pattern].\n\
         end\n"
    ));
    let ops = &output.matcher.catalog_policy().program().ops;
    assert!(
        matches!(ops[0], BoundOp::Associate { .. }),
        "the exclude was authored first"
    );
}

#[test]
fn a_definition_named_for_a_verb_still_compiles() {
    let output = compile("resolve := [text=あ].");
    assert!(output.matcher.patterns().contains_key("resolve"));
}

#[test]
fn a_catalog_without_a_policy_block_has_an_empty_program() {
    let output = compile("a := [text=あ].");
    assert!(output.matcher.catalog_policy().program().ops.is_empty());
}

// --- Static rules: arguments, enums, references ------------------------------

#[test]
fn a_quoted_string_in_an_enum_position_names_the_bare_spelling() {
    let message = refusal(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: \"after\", evidence: raw.\nend\n"
    ));
    // `contains("after")` alone does NOT discriminate: the fallback message
    // lists the whole `Relation` domain, and `after` is in it. What the rule
    // owes the author is the statement they meant to write.
    assert!(
        message.contains("relation: after"),
        "names the bare spelling of what was written: {message}"
    );
}

#[test]
fn an_enum_outside_its_domain_is_refused_with_a_suggestion() {
    let message = refusal(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: aftr, evidence: raw.\nend\n"
    ));
    // As above, `contains("after")` alone is satisfied by the fallback that
    // lists the domain. The rule being tested is the *suggestion*.
    assert!(
        message.contains("did you mean `after`"),
        "one edit from a value in the domain: {message}"
    );
}

#[test]
fn an_association_without_evidence_is_refused() {
    let message = refusal(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: after.\nend\n"
    ));
    assert!(message.contains("evidence"), "{message}");
}

#[test]
fn an_unknown_argument_key_is_refused_with_a_suggestion() {
    let message = refusal(&format!(
        "{DEFS}policy\n  exclude @a wth: @N, relation: after, evidence: raw.\nend\n"
    ));
    // `contains("with")` alone does not discriminate: deleting the key check
    // leaves `take_required` reporting "this statement declares no `with:`",
    // which also contains it. The rule is the suggestion.
    assert!(
        message.contains("did you mean `with:`"),
        "one edit from an argument this verb has: {message}"
    );
}

#[test]
fn a_duplicate_argument_key_is_refused() {
    let message = refusal(&format!(
        "{DEFS}policy\n  exclude @a with: @N, with: @N, relation: after, evidence: raw.\nend\n"
    ));
    assert!(message.contains("with"), "{message}");
}

#[test]
fn an_unresolved_subject_is_refused_naming_it() {
    let message = refusal(&format!(
        "{DEFS}policy\n  resolve @missing rank: [longest, start, end, pattern].\nend\n"
    ));
    assert!(message.contains("missing"), "{message}");
}

#[test]
fn an_unresolved_group_is_refused_naming_it() {
    let message = refusal(&format!(
        "{DEFS}policy\n  resolve %nope rank: [longest, start, end, pattern].\nend\n"
    ));
    assert!(message.contains("nope"), "{message}");
}

#[test]
fn one_statement_reports_both_of_its_unresolved_names() {
    // `Program::bind` collects both; this proves the diagnostic layer does not
    // re-introduce an early return on its way out.
    let message = refusal(
        "a := [text=あ].\npolicy\n  \
         exclude @nosubject with: @nooperand, relation: after, evidence: raw.\nend\n",
    );
    assert!(message.contains("nosubject"), "{message}");
    assert!(message.contains("nooperand"), "{message}");
}

#[test]
fn a_fragment_operand_forces_raw_evidence() {
    let message = refusal(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: after, evidence: current.\nend\n"
    ));
    assert!(
        message.contains("fragment") || message.contains("survivor"),
        "a fragment has no survivor lifecycle: {message}"
    );
}

#[test]
fn detach_refuses_a_relation_naming_no_detachable_region() {
    let message = refusal(&format!(
        "{DEFS}policy\n  detach @a with: @N, relation: within, evidence: raw.\nend\n"
    ));
    assert!(message.contains("within"), "{message}");
}

// --- Static rules: rank totality ---------------------------------------------
//
// Deleting any of these rules leaves the catalog compiling, so `refusal`
// panics rather than matching a fallback message.

#[test]
fn a_non_total_rank_is_refused() {
    let message = refusal("a := [text=あ].\npolicy\n  resolve all rank: [longest].\nend\n");
    assert!(message.contains("total"), "{message}");
}

#[test]
fn the_language_does_not_silently_append_criteria() {
    let message = refusal("a := [text=あ].\npolicy\n  resolve all rank: [longest, start].\nend\n");
    assert!(message.contains("total"), "{message}");
}

#[test]
fn a_duplicate_rank_criterion_is_refused() {
    let message = refusal(
        "a := [text=あ].\npolicy\n  \
         resolve all rank: [longest, longest, start, end, pattern].\nend\n",
    );
    assert!(message.contains("longest"), "{message}");
}

#[test]
fn longest_with_shortest_is_refused() {
    let message = refusal(
        "a := [text=あ].\npolicy\n  \
         resolve all rank: [longest, shortest, start, end, pattern].\nend\n",
    );
    assert!(message.contains("shortest"), "{message}");
}

#[test]
fn a_criterion_after_pattern_is_refused_as_unreachable() {
    // Pins that the tail check is POSITIONAL. A membership check —
    // "does the rank mention start, end and pattern at all" — accepts this
    // and every other test here stays green, so without this one the
    // positionality claim in `rank_is_total`'s doc is unverified.
    let message = refusal(
        "a := [text=\u{3042}].\npolicy\n  \
         resolve all rank: [start, end, pattern, longest].\nend\n",
    );
    assert!(
        message.contains("after `pattern`"),
        "a rank carrying the tail early IS total; the defect is the dead criterion: {message}"
    );
}

#[test]
fn a_total_rank_compiles() {
    let output = compile(
        "a := [text=あ].\npolicy\n  \
         resolve all rank: [longest, priority_order, start, end, pattern].\nend\n",
    );
    assert_eq!(output.matcher.catalog_policy().program().ops.len(), 1);
}

// --- Ownership across an accumulated catalog ---------------------------------

use lingrep_core::fsm::FsmBuilder;

const POLICY_ONLY: &str = "policy\n  resolve all rank: [longest, start, end, pattern].\nend\n";

#[test]
fn a_policy_only_document_is_legal() {
    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("a.lx", "a := [text=あ]."))
        .expect("registers");
    builder
        .add_document(PatternSource::named("p.lx", POLICY_ONLY))
        .expect("policy-only is legal");
    let output = builder.build().expect("builds");
    assert_eq!(output.matcher.catalog_policy().program().ops.len(), 1);
}

#[test]
fn a_second_policy_block_is_rejected_at_build_naming_both_locations() {
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named("first.lx", POLICY_ONLY))
        .expect("first registers");
    builder
        .add_document(PatternSource::named("second.lx", POLICY_ONLY))
        .expect("the second registers; the conflict is a property of the catalog, not the call");
    let message = match builder.build() {
        Ok(_) => panic!("two blocks must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(message.contains("first.lx"), "names the earlier: {message}");
    assert!(message.contains("second.lx"), "names the later: {message}");
}

/// A program's operations without their spans.
///
/// `BoundProgram`'s `PartialEq` includes `PolicySpan`, and a span legitimately
/// differs between whole-document and incremental assembly — different source,
/// different offset. Comparing programs directly therefore asserts something
/// that is not true and is not wanted. This helper compares the span-free
/// semantics instead.
fn semantics(program: &lingrep_core::fsm::policy_program::BoundProgram) -> Vec<String> {
    program
        .ops
        .iter()
        .map(|op| match op {
            BoundOp::Resolve { subject, rank, .. } => format!("resolve {subject:?} {rank:?}"),
            BoundOp::Associate {
                effect,
                subject,
                operand,
                relation,
                evidence,
                ..
            } => format!("{effect:?} {subject:?} {operand:?} {relation:?} {evidence:?}"),
            BoundOp::Admit {
                subject,
                terminal,
                anchor,
                ..
            } => format!("admit {subject:?} {terminal:?} {anchor}"),
        })
        .collect()
}

#[test]
fn one_document_equals_its_definitions_plus_a_policy_only_document() {
    let whole = "a := [text=あ].\nb := [text=い].\n\
                 policy\n  resolve all rank: [longest, start, end, pattern].\nend\n";
    let from_document = compile(whole);

    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("a.lx", "a := [text=あ]."))
        .expect("a");
    builder
        .add_definition(PatternSource::named("b.lx", "b := [text=い]."))
        .expect("b");
    builder
        .add_document(PatternSource::named("p.lx", POLICY_ONLY))
        .expect("policy");
    let incremental = builder.build().expect("builds");

    assert_eq!(
        semantics(from_document.matcher.catalog_policy().program()),
        semantics(incremental.matcher.catalog_policy().program()),
        "whole-document and incremental assembly must agree"
    );
}

#[test]
fn a_document_carrying_both_definitions_and_a_policy_block_keeps_both() {
    // Every other ownership test feeds `add_document` a policy-ONLY document,
    // so none of them notices an `add_document` that keeps the policy only
    // when the document declared no definitions. That is the shape a real
    // catalog has.
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named(
            "both.lx",
            "a := [text=\u{3042}].\nb := [text=\u{3044}].\npolicy\n  \
             resolve all rank: [longest, start, end, pattern].\nend\n",
        ))
        .expect("registers");
    let output = builder.build().expect("builds");
    assert_eq!(output.matcher.catalog_policy().program().ops.len(), 1);
    assert_eq!(output.matcher.patterns().len(), 2, "and both definitions");
}

#[test]
fn a_diagnostic_about_an_operation_names_the_document_that_declared_it() {
    // `PolicySpan` carries a `SourceId`, so this is the test that actually
    // exercises remapping. An earlier revision asserted a bare byte offset,
    // which could not tell two documents of similar length apart and would
    // not have failed when remapping was removed.
    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("defs.lx", "a := [text=あ]."))
        .expect("a");
    builder
        .add_document(PatternSource::named(
            "policy.lx",
            "policy\n  resolve @missing rank: [longest, start, end, pattern].\nend\n",
        ))
        .expect("registers");
    let error = match builder.build() {
        Ok(_) => panic!("unresolved subject must be refused"),
        Err(error) => error,
    };
    // Asserted on the diagnostic's LOCATION, not on `to_string()`.
    // `CompileError`'s `Display` renders messages alone, so a substring
    // check against it could never see a source label and would pass with
    // remapping deleted. The location is the thing remapping moves.
    let named: Vec<&str> = error
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.location().source_name())
        .collect();
    assert_eq!(
        named,
        vec!["policy.lx"],
        "the diagnostic must name the document that declared the statement, not the one that \
         did not"
    );
}

// --- Evidence synthesis and the publication gate ------------------------------

fn hidden_names(output: &CompileOutput) -> Vec<&str> {
    output
        .matcher
        .catalog_policy()
        .hidden
        .keys()
        .map(compact_str::CompactString::as_str)
        .collect()
}

#[test]
fn a_program_fragment_operand_gets_an_evidence_emitter() {
    let output = compile(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: after, evidence: raw.\nend\n"
    ));
    assert_eq!(
        hidden_names(&output),
        vec!["__policy__N"],
        "the operand needs raw evidence"
    );
}

#[test]
fn a_synthesized_emitter_is_never_published() {
    let output = compile(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: after, evidence: raw.\nend\n"
    ));
    assert!(
        output
            .matcher
            .patterns()
            .keys()
            .all(|name| !name.starts_with("__policy__")),
        "evidence is never public"
    );
}

#[test]
fn a_fragment_member_of_an_operand_group_also_gets_evidence() {
    let output = compile(
        "a := [text=あ].\ngroups: [\"ev\"]\nfragment N := [text=ぬ].\n\
         policy\n  exclude @a with: %ev, relation: after, evidence: raw.\nend\n",
    );
    assert_eq!(
        hidden_names(&output),
        vec!["__policy__N"],
        "group members need evidence too"
    );
}

/// A **parity guard**, not an ordering discrimination — checked, not assumed.
///
/// Moving `bind_policy_program` ahead of `synthesize_hidden_emitters` fails no
/// test, this one included. The reason is the one `policy_group_test.rs`
/// records for the group index, and it is the same reason: synthesis only ever
/// **appends**, and appending never renumbers an authored `DefId`. `@a` is at
/// index 0 either way.
///
/// Binding-after-synthesis is therefore a domain-alignment guarantee — it is
/// what keeps `DefId` meaning the same thing in the program as in the
/// definition table, and what would still be right if synthesis ever inserted
/// rather than appended. What this test does pin is the parity itself: a
/// compile whose operand set mints an emitter resolves `@a` to the same
/// definition as one that mints none.
#[test]
fn synthesis_does_not_shift_a_def_id() {
    let with_evidence = compile(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: after, evidence: raw.\nend\n"
    ));
    let without = compile(&format!(
        "{DEFS}policy\n  resolve @a rank: [longest, start, end, pattern].\nend\n"
    ));
    assert_eq!(
        with_evidence.matcher.catalog_policy().program().ops[0].subject(),
        without.matcher.catalog_policy().program().ops[0].subject(),
        "`@a` is the same definition in both",
    );
}

#[test]
fn a_policy_only_catalog_reaches_the_publication_seam() {
    let output = compile(&format!(
        "{DEFS}policy\n  exclude @a with: @N, relation: after, evidence: raw.\nend\n"
    ));
    assert!(
        output.matcher.catalog_policy().declared(),
        "a program is a declaration; reading `entries` alone takes the raw fast path"
    );
}

/// A single-edge `detach` is non-empty by construction, so it needs no proof.
///
/// `WIDE` is wider than `a` — the case a width proof would reject. `K`'s strict
/// bound excludes a whole-span operand, so the detach either fires on a proper
/// trailing region or does not fire at all, and the record can never empty.
#[test]
fn a_single_edge_detach_needs_no_proof() {
    let output = compile(
        "a := [text=あ] [text=い].\nfragment WIDE := [text=あ] [text=い] [text=う].\n\
         policy\n  detach @a with: @WIDE, relation: at_end, evidence: raw.\nend\n",
    );
    assert_eq!(output.matcher.catalog_policy().program().ops.len(), 1);
}

#[test]
fn opposite_edge_detaches_carry_the_cumulative_obligation() {
    let message = refusal(
        "a := [text=あ] [text=い].\nfragment WIDE := [text=あ] [text=い] [text=う].\n\
         policy\n  \
         detach @a with: @WIDE, relation: at_start, evidence: raw.\n  \
         detach @a with: @WIDE, relation: at_end, evidence: raw.\nend\n",
    );
    assert!(message.contains("detach"), "{message}");
    assert!(message.contains("empty"), "{message}");
}

/// Two detaches on opposite edges that provably cannot meet.
///
/// The obligation is a width proof, not a ban: five tokens minus one at each
/// edge still leaves three. Without this, "refuse every opposite-edge pair"
/// would pass the test above and nothing would notice.
#[test]
fn opposite_edge_detaches_that_cannot_meet_are_admitted() {
    let output = compile(
        "a := [text=あ] [text=い] [text=う] [text=え] [text=お].\n\
         fragment HEAD := [text=あ].\nfragment TAIL := [text=お].\n\
         policy\n  \
         detach @a with: @HEAD, relation: at_start, evidence: raw.\n  \
         detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
    );
    assert_eq!(output.matcher.catalog_policy().program().ops.len(), 2);
}

/// An unbounded operand can never be proved to fit.
#[test]
fn an_unbounded_operand_rejects_an_opposite_edge_pair_immediately() {
    let message = refusal(
        "a := [text=あ] [text=い] [text=う] [text=え] [text=お].\n\
         fragment HEAD := [text=あ].\nfragment TAIL := [text=お]+.\n\
         policy\n  \
         detach @a with: @HEAD, relation: at_start, evidence: raw.\n  \
         detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
    );
    assert!(message.contains("detach"), "{message}");
}

/// `all` and `@a` are two spellings selecting one definition.
///
/// Aggregating by `BoundSubject` identity would see two unrelated subjects and
/// miss the obligation entirely.
#[test]
fn opposite_edge_obligations_are_found_across_different_subject_spellings() {
    let message = refusal(
        "a := [text=あ] [text=い].\nfragment WIDE := [text=あ] [text=い] [text=う].\n\
         policy\n  \
         detach all with: @WIDE, relation: at_start, evidence: raw.\n  \
         detach @a  with: @WIDE, relation: at_end, evidence: raw.\nend\n",
    );
    assert!(message.contains("detach"), "{message}");
    assert!(message.contains("empty"), "{message}");
}

/// Two detaches on the *same* edge remove the wider region, not both.
///
/// A public span narrows monotonically, so a second `at_start` can only push
/// the start where the first already could — or further, never cumulatively.
/// Summing them instead refuses this catalog, which cannot empty: the widest
/// head is two tokens, the tail one, and the record is five.
#[test]
fn two_detaches_on_one_edge_do_not_accumulate() {
    let output = compile(
        "a := [text=あ] [text=い] [text=う] [text=え] [text=お].\n\
         fragment HEAD := [text=あ] [text=い].\nfragment ALSO := [text=あ] [text=い].\n\
         fragment TAIL := [text=お].\n\
         policy\n  \
         detach @a with: @HEAD, relation: at_start, evidence: raw.\n  \
         detach @a with: @ALSO, relation: at_start, evidence: raw.\n  \
         detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
    );
    assert_eq!(output.matcher.catalog_policy().program().ops.len(), 3);
}

/// Operands that meet exactly are refused: the span they leave is empty.
///
/// The boundary the proof turns on. One token less on either side and the
/// record survives with a token to spare, which
/// `opposite_edge_detaches_that_cannot_meet_are_admitted` pins.
#[test]
fn operands_that_exactly_consume_the_record_are_refused() {
    let message = refusal(
        "a := [text=あ] [text=い].\n\
         fragment HEAD := [text=あ].\nfragment TAIL := [text=い].\n\
         policy\n  \
         detach @a with: @HEAD, relation: at_start, evidence: raw.\n  \
         detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
    );
    assert!(message.contains("empty"), "{message}");
}

/// The vocabularies a statement is assembled from, near-misses included.
///
/// Each list holds the real spellings plus one-edit neighbours and
/// wrong-kind values, because those are the inputs a hand-written table
/// stops covering first: the checker suggests a nearest key, and a
/// suggestion path is reached only by a key that is nearly right.
const VERBS: &[&str] = &["resolve", "exclude", "require", "detach", "admit", "resolv"];

const SUBJECTS: &[&str] = &["all", "@a", "@N", "%g", "@absent", "%absent", "al"];

const ARGUMENTS: &[&str] = &[
    "rank: [longest, start, end, pattern]",
    "rank: [longest]",
    "rank: [longest, start, end, patern]",
    "rank: 3",
    "rnk: [longest, start, end, pattern]",
    "with: @N",
    "with: %g",
    "with: all",
    "relation: at_start",
    "relation: at_end",
    "relation: atstart",
    "relation: [at_start]",
    "evidence: raw",
    "evidence: current",
    "evidence: rawe",
    "terminal: {exact: true, anchor: propositional_end}",
    "terminal: {within: 2, anchor: nowhere}",
    "terminal: true",
];

/// A catalog offering every name the vocabularies above can reach.
const DIFFERENTIAL_DEFS: &str = r#"
anchor: {name: "propositional_end", exclude: "TRAILING_APPARATUS"}
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment TRAILING_APPARATUS := [upos=PUNCT]+.

groups: ["g"]
a := [text=あ].
groups: ["g"]
fragment N := [text=ぬ].
"#;

proptest::proptest! {
    /// §8: every statement over the real vocabularies either lowers to one
    /// well-formed operation or is refused with a diagnostic.
    ///
    /// Never a third outcome. A panic here is the failure mode the whole
    /// declaration tier exists to remove: a catalog that says something the
    /// compiler cannot name is an authoring mistake, and an authoring mistake
    /// that aborts the process is indistinguishable from a compiler defect.
    #[test]
    fn every_generated_statement_lowers_or_is_refused(
        verb in proptest::sample::select(VERBS),
        subject in proptest::sample::select(SUBJECTS),
        arguments in proptest::collection::vec(proptest::sample::select(ARGUMENTS), 0..4),
    ) {
        let statement = if arguments.is_empty() {
            format!("{verb} {subject}.")
        } else {
            format!("{verb} {subject} {}.", arguments.join(", "))
        };
        let source = format!("{DIFFERENTIAL_DEFS}\npolicy\n  {statement}\nend\n");

        match compile_catalog(PatternSource::named("t.lx", &source), CompileLimits::default()) {
            Ok(output) => proptest::prop_assert_eq!(
                output.matcher.catalog_policy().program().ops.len(),
                1,
                "one statement lowers to one operation: {}",
                statement
            ),
            Err(error) => proptest::prop_assert!(
                !error.diagnostics.is_empty(),
                "a refusal must say why: {}",
                statement
            ),
        }
    }
}

/// Warning-severity diagnostics from a catalog expected to compile.
fn warnings(source: &str) -> Vec<String> {
    compile_catalog(
        PatternSource::named("t.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("must compile: {error}"))
    .diagnostics
    .into_iter()
    .filter(|diagnostic| diagnostic.severity() == DiagnosticSeverity::Warning)
    .map(|diagnostic| diagnostic.message)
    .collect()
}

/// Whether a catalog warns that a statement can never affect a record.
fn warns_inert(source: &str) -> bool {
    warnings(source)
        .iter()
        .any(|message| message.contains("can never affect a record"))
}

/// The case the design named: a group that has members and selects none.
///
/// The reference resolves, so nothing refuses it, and the statement runs on
/// every sentence over an empty set. This is the one shape an author cannot
/// distinguish from a working one by reading the catalog.
#[test]
fn a_group_holding_only_fragments_selects_nothing_and_warns() {
    let source = "groups: [\"helpers\"]\nfragment ONE := [text=あ].\n\
                  groups: [\"helpers\"]\nfragment TWO := [text=い].\n\
                  a := [text=う] @ONE @TWO.\n\
                  policy\n  resolve %helpers rank: [longest, start, end, pattern].\nend\n";
    assert!(warns_inert(source), "got {:?}", warnings(source));
}

/// One publishable member is enough, and the warning must not fire.
///
/// Without this the rule could warn unconditionally on every `%group` and every
/// test above would still pass.
#[test]
fn a_group_with_one_publishable_member_does_not_warn() {
    let source = "groups: [\"helpers\"]\nfragment ONE := [text=あ].\n\
                  groups: [\"helpers\"]\nb := [text=い].\n\
                  a := [text=う] @ONE.\n\
                  policy\n  resolve %helpers rank: [longest, start, end, pattern].\nend\n";
    assert!(!warns_inert(source), "got {:?}", warnings(source));
}

/// The same defect spelled `@fragment`.
///
/// A rule that named only the group would leave this silent, and it is the
/// spelling an author reaches for first.
#[test]
fn a_fragment_subject_warns_by_the_same_rule() {
    let source = "fragment N := [text=あ].\na := [text=う] @N.\n\
                  policy\n  resolve @N rank: [longest, start, end, pattern].\nend\n";
    assert!(warns_inert(source), "got {:?}", warnings(source));
}

/// And spelled `all`, in a catalog that publishes nothing.
#[test]
fn all_warns_when_the_catalog_declares_no_publishable_pattern() {
    let source = "fragment N := [text=あ].\n\
                  policy\n  resolve all rank: [longest, start, end, pattern].\nend\n";
    assert!(warns_inert(source), "got {:?}", warnings(source));
}

/// A warning, never a refusal: the engine executes the statement as written.
#[test]
fn an_inert_statement_still_compiles_and_still_lowers() {
    let output = compile(
        "fragment N := [text=あ].\na := [text=う] @N.\n\
         policy\n  resolve @N rank: [longest, start, end, pattern].\nend\n",
    );
    assert_eq!(output.matcher.catalog_policy().program().ops.len(), 1);
}

/// Diagnostics from a catalog expected to be refused.
fn refusals(source: &str) -> Vec<String> {
    match compile_catalog(
        PatternSource::named("t.lx", source),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("must be refused: {source}"),
        Err(error) => error
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
    }
}

/// Two malformed values in one statement report twice.
///
/// `Program::bind` already collects a statement's subject and operand together,
/// so a lowerer that stopped at the first bad value made "how many mistakes a
/// compile reports" depend on which phase caught them. An author fixing one
/// should not be told about the next on the following compile.
#[test]
fn two_malformed_values_in_one_statement_are_reported_together() {
    let messages = refusals(&format!(
        "{DEFS}policy\n           exclude @a with: @N, relation: sideways, evidence: hearsay.\nend\n"
    ));
    assert!(
        messages.iter().any(|message| message.contains("sideways"))
            && messages.iter().any(|message| message.contains("hearsay")),
        "both values are wrong and both must be named: {messages:?}"
    );
}

/// The same, for two *missing* keys rather than two bad values.
#[test]
fn two_missing_keys_in_one_statement_are_reported_together() {
    let messages = refusals(&format!("{DEFS}policy\n  exclude @a with: @N.\nend\n"));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("`relation:`"))
            && messages
                .iter()
                .any(|message| message.contains("`evidence:`")),
        "the statement declares neither, and naming one at a time costs a \
         compile per key: {messages:?}"
    );
}

/// The one surviving short-circuit, and it is deliberate.
///
/// An unrecognized key explains every other complaint the statement would draw.
/// Told "this statement declares no `relation:`" beside it, an author who typed
/// `realtion:` has to find the typo unaided — so the key is reported alone.
#[test]
fn an_unknown_argument_key_is_reported_without_its_consequences() {
    let messages = refusals(&format!(
        "{DEFS}policy\n           exclude @a with: @N, realtion: within, evidence: raw.\nend\n"
    ));
    assert!(
        messages.iter().any(|message| message.contains("realtion")),
        "got {messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.contains("declares no `relation:`")),
        "the missing key is the consequence of the typo, not a second defect: \
         {messages:?}"
    );
}

/// Statements keep reporting independently of each other.
#[test]
fn a_bad_value_in_one_statement_does_not_hide_the_next_statements() {
    let messages = refusals(&format!(
        "{DEFS}policy\n           exclude @a with: @N, relation: sideways, evidence: raw.\n           require @a with: @N, relation: backwards, evidence: raw.\nend\n"
    ));
    assert!(
        messages.iter().any(|message| message.contains("sideways"))
            && messages.iter().any(|message| message.contains("backwards")),
        "got {messages:?}"
    );
}
