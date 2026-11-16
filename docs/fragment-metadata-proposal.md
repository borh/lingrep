---
title: Fragment metadata and match evidence: design proposal
type: investigation
status: current
summary: Keep authored definition metadata local; treat selected fragment occurrences as a separate, opt-in evidence feature if a concrete consumer requires them.
updated: 2026-11-16
scope:
  - Pattern metadata, fragment references, and match evidence in lingrep
refs:
  - vibrato-pipe-ovcn
  - docs/pattern-language-reference.md
  - lingrep-core/src/fsm/types.rs
  - lingrep-core/src/fsm/identity.rs
---

# Fragment metadata and match evidence: design proposal

Keep fragment metadata local to its authored definition. Use metadata on emitting
patterns to classify and aggregate those patterns. If an analysis must identify
which fragment supplied part of an individual match, introduce an explicit,
opt-in evidence projection with bounded selected occurrences. Do not merge that
evidence into a pattern's descriptive metadata or enumerate every successful
execution history.

This is a recommendation for review, not an adopted language change. The
implementation of filtering and aggregation by authored pattern metadata is
independent of this proposal. No fragment-propagation behavior is implemented by
this document.

## The two questions require different information

A catalog author asks, “Which patterns describe past tense?” An analyst asks,
“Which alternative supplied the ending in this particular example?” The first
question concerns a definition; the second concerns a successful execution and a
role within it. A reference graph can answer which fragments a pattern could use,
but cannot identify which alternatives participated in one match.

Use these terms consistently:

| Term | Meaning | Example |
| --- | --- | --- |
| Authored metadata | A typed value attached to a definition, after file defaults and explicit annotations are applied | A pattern has `aspect: "progressive"` |
| Static dependency | A fragment that an expression references, directly or transitively, whether or not a match uses it | A tail expression references past and nonpast alternatives |
| Selected occurrence | One successful consuming fragment call, at one call site and token span, in the selected witness for a match | The past-tail call covers the final auxiliary |
| Selected witness | The deterministic successful assignment used to explain an emitted match | One prioritized alternative explains an ambiguous accepted span |
| All-derivation provenance | Every successful execution history capable of producing a match | Several nested alternatives accept the same tokens |
| Analytical category | The explicit unit used to filter or aggregate results | A definition's authored tense, or a separately specified classification of its head occurrence |

A static dependency, a selected occurrence, and an analytical category must not
share an unqualified `metadata` field. Their absence has different meanings.

## Current behavior and its consequences

The following facts are established from baseline commit `553eac32`:

- Metadata annotations attach to the following definition. `defaults` supplies
  file-level values; `none` removes a default for a definition. Schema scopes
  determine where keys are required. The lowerer applies these rules in
  `DocumentDraft::materialize_metadata` in
  [lower.rs](../lingrep-core/src/fsm/compiler/lower.rs).
- Fragments do not emit ordinary matches or appear in the runtime pattern
  catalog. Their authored metadata remains available to compiler-side catalog
  inspection. See `ParsedDefinition::is_emitting` in
  [model.rs](../lingrep-core/src/fsm/compiler/model.rs) and the complete-definition
  metadata snapshot in
  [metadata_baseline.rs](../lingrep-core/src/fsm/compiler/metadata_baseline.rs).
- Derived groups test string metadata on emitting definitions; missing keys and
  non-string values do not satisfy those predicates. Policy `resolve` can
  partition records by a string metadata key. Consequently, adding inherited
  metadata can change publication-policy selection and counts even when token
  predicates remain unchanged. See [Metadata](pattern-language-reference.md#metadata)
  and [Subjects](pattern-language-reference.md#subjects).
- A public `Match` contains its pattern, span, captures, graph bindings and lexical
  identity. It contains neither fragment-call history nor inherited metadata.
  Equality includes all of these fields in
  [types.rs](../lingrep-core/src/fsm/types.rs).
- In ordinary dense matching, `MatchSink` deduplicates by `(pattern, start, end)`.
  Captures are extracted afterwards from a prioritized successful candidate.
  Enhanced graph assignments and evaluated expression paths have additional
  observable identity; the dense rule is not a claim that every execution mode
  emits one record per span. See
  [emit.rs](../lingrep-core/src/fsm/matcher/emit.rs),
  `CompiledCatalog::find_matches_with_context` in
  [compiled.rs](../lingrep-core/src/fsm/matcher/compiled.rs), and
  `Program::extract_with` in
  [program.rs](../lingrep-core/src/fsm/matcher/program.rs).
- Named captures already identify selected subspans, including hygienic fragment
  expansion paths. They can provide useful evidence without a new global
  inheritance rule, but their results do not enumerate all accepting histories.

The production catalogs already separate helper roles from pattern categories.
For example, [alternation.lx](../patterns/alternation.lx) marks fragments as
`role: "helper"` and annotates emitting constructions with `alternation`, `member`
and `slot`. Copying every fragment key to every referencing pattern would collapse
that separation.

## Scenarios that distinguish the alternatives

The examples use a simplified UniDic-oriented tail analysis to distinguish
metadata ownership. They do not define a complete linguistic analysis of tense.

```pattern
schema
  tense: string in {"past", "nonpast"}
end

tense: "past"
fragment past_tail := [unidic.pos1=助動詞 & lemma=た].

tense: "nonpast"
fragment nonpast_tail := [unidic.pos1=動詞 & unidic.cform ~ /\A終止形/].

pattern finite_tail := (@past_tail | @nonpast_tail).
```

| Scenario | Required interpretation | Incorrect inference |
| --- | --- | --- |
| A match uses `past_tail` | Its selected tail evidence can identify that occurrence as past | The definition `finite_tail` is globally past |
| Another match uses `nonpast_tail` | The same definition can have different occurrence evidence | A static list of both values proves both occurred |
| A sentence contains past material in a quotation and a nonpast reporting predicate | Evidence retains the occurrence's role and span; classifying the reporting clause requires an explicit subject | All fragment labels describe the clause's main tense |
| A fragment is optional and is skipped | The selected witness has no occurrence for that call | Every referenced fragment participated |
| One fragment is called twice | The calls remain distinct by call site and span | A set of fragment names completely explains the result |
| Two alternatives accept the same span | Selection follows a declared deterministic witness rule | Both alternatives become two new matches |
| A positive assertion inspects another sentence | The asserted evidence is distinct from consumed material | Its label describes the consumed span |
| A negative assertion succeeds | Success establishes an absence condition | A failed fragment call becomes a successful occurrence |
| An intersection requires two expressions | Both constraints can hold over one span | Their metadata values are mutually exclusive alternatives |
| An author writes `tense: none` to remove a file default | The authored key remains absent | Later inheritance restores the removed value |

For definition-level analysis, authors can use two emitting patterns with explicit
annotations:

```pattern
tense: "past"
pattern past_example := @past_tail.

tense: "nonpast"
pattern nonpast_example := @nonpast_tail.
```

These definitions represent separate catalog results. If both emit records over
the same span, aggregation must follow its documented match-record counting rule;
reference sharing does not authorize deduplication between definitions.

For an individual example, explicit captures can identify the selected role:

```pattern
pattern finite_tail := ((?<past> @past_tail) | (?<nonpast> @nonpast_tail)).
```

A consumer can read which capture is present under the existing capture contract.
Captures supply spans rather than a general category-derivation language. A new
projection is justified only when that distinction or the burden of explicit
captures prevents a concrete analysis.

## Alternatives and the conditions under which they fit

| Alternative | What it supplies | Main cost or failure mode | When to choose it |
| --- | --- | --- | --- |
| Keep local metadata; annotate emitting patterns | Stable definition categories, with existing defaults for shared values | Repeated annotations; one alternation does not acquire per-match categories | The research question groups definitions, as authored-pattern filtering does |
| Implicit static inheritance | A metadata map derived from reference dependencies | Conflicts, optional/negative references, helper keys and policy effects; no per-match answer | Only if the product adopts dependency descriptions as the meaning of those keys |
| Explicit static derivation for selected keys | A checked compile-time value for an opted-in key | New declaration syntax and provenance; agreement still says nothing about participation | A real catalog has repeated definition-level categories that defaults cannot express |
| Selected-occurrence evidence | Fragment identity, role and span for one declared successful witness | Extra extraction, limits and transport; explicit semantics for assertions and transforms | An analyst must explain or classify individual matches |
| All-derivation provenance | Every successful path, enumerated or represented in a shared derivation graph | Combinatorial materialization; graph evaluation and counting still require a separate contract | A dedicated ambiguity-analysis tool with its own bounded contract |
| Explicit labelled alternatives or capture-based classification | The author chooses which role determines a category | New classification syntax if existing captures are insufficient | Several fragment labels occur, but only a named role answers the research question |

The recommendation combines the first alternative with the fourth only if its
consumer is established. It avoids making a convenience for repeated annotations
change the identity of corpus matches.

### Prior art separates declarations from selected alternatives

Tree-sitter query directives attach key/value properties to a query pattern, while
captures identify nodes in query results. Its `#set!` mechanism is a useful
precedent for definition-level properties rather than proof of runtime branch
participation. See the official
[Predicates and Directives documentation](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/3-predicates-and-directives.html#the-set-directive).

ANTLR's explicit alternative labels generate distinct rule-context types and
listener events. The author labels the alternatives that must be observable.
That supports evaluating explicit selected-alternative evidence separately from
rule-level annotations. See the official
[Alternative Labels documentation](https://github.com/antlr/antlr4/blob/master/doc/parser-rules.md#alternative-labels).

These are design comparisons, not evidence that either system's ambiguity or
counting semantics can be copied into lingrep unchanged.

## Recommended invariants and conflict rules

Retain the following rules for authored metadata:

1. A definition's effective descriptive metadata comes from its file defaults and
   its own annotations. Referencing a fragment does not add or remove values.
2. A fragment metadata edit does not change a referencing pattern's category or
   publication-policy membership through inference. An edit to catalog source
   still changes the source-based catalog fingerprint.
3. Missing metadata stays missing. A list is not substituted for a declared
   string, and conflicting values are not resolved by reference order.
4. Compiler-owned operational keys, such as publication policy, grouping keys,
   transforms and skip controls, do not enter a general descriptive inheritance
   mechanism.
5. Definition categories and runtime evidence use distinct interfaces. Enabling
   evidence does not add, remove, merge or reorder matches.

With no inheritance, conflicting values on different fragments are valid local
annotations. A pattern may have its own value, a different value, or no value.
There is no inferred conflict to resolve. A consumer that classifies selected
occurrences must specify its target role and ambiguity behavior.

If explicit static derivation is chosen instead, use this restricted candidate
rather than implicit copying:

- The author opts in per descriptive key. Only scalar strings are eligible for
  the first implementation; lists and maps retain their authored types.
- Gather authored effective values from transitively reachable consuming fragment
  dependencies, deduplicated by definition ID. Include optional and alternative
  dependencies because this is a static dependency query; keep assertion-only
  references outside that set. Repeated call paths do not create extra values.
- Apply an existing effective pattern value unchanged. Derive a missing value
  only when every declared contributing fragment value agrees. Missing
  contributors remain visible in derivation evidence and do not establish an
  exhaustive category.
- When derivation is required, reject disagreement for a requested scalar key
  with source locations for the contributing values. Do not choose the last
  reference or coerce the result to a list. An existing default prevents
  derivation; an explicit pattern annotation can supply the value that resolves
  a conflict.
- Preserve an explicit unset marker until derivation has finished, so an unset
  remains absent. The current lowerer removes `none` while materializing
  metadata; applying inheritance afterwards without retaining that marker would
  lose the author's opt-out.
- Keep derivation provenance outside the effective value: direct/default/derived
  origin and contributing definition IDs. Perform schema validation after the
  final effective map is constructed.

This candidate describes dependency-wide agreement. Even unanimous metadata on
an optional fragment does not prove that the fragment participated. It must not
be advertised as per-match classification.

## A bounded selected-occurrence feature, if adopted

A useful first capability would answer: “For this match and selected consuming
role, which fragment occurrence supplied the evidence?” It would not answer
“What are all possible derivations?” or infer an entire sentence's tense from
any fragment that happened to match.

The representation should keep three facts separate:

- The ordinary `Match`, with its existing equality and counting meaning.
- An observation identifying the selected witness and its fragment occurrences.
- A catalog dictionary mapping stable catalog-scoped definition IDs to names and
  authored metadata, serialized once per run rather than copied onto every
  occurrence.

Each occurrence would identify a catalog-scoped call site, a fragment definition,
its hygienic reference path, and its raw token span. A repeated call produces
separate occurrences. Reference-chain paths distinguish nested roles. IDs are
stable only within the catalog fingerprint; changing source can renumber them.

Use an explicit result state: evidence not requested, complete evidence with zero
or more occurrences, or a declared failure. An empty complete list means no
eligible consuming occurrence; it must not stand for “not computed.” The first
implementation should fail the request when its evidence budget is exceeded,
using the existing failed/incomplete stream contract. A later partial-evidence
mode would require a declared omission reason and consumer support.

Attach the observation to an immutable internal record identity through policy
selection and span transforms. A final JSONL sequence number can identify the
exported record, but is assigned too late to associate raw evidence with a record
through rewriting. Joining solely by span would confuse graph assignments and
records that share the same surface interval.

The initial semantics should retain one deterministic successful witness, aligned
with the chosen execution's capture and graph assignment. Ordinary dense
extraction already selects one prioritized candidate. Other execution paths must
prove equivalent witness selection before claiming support; rerunning an
unrelated first-success search could disagree with the emitted assignment.

For the first scope, expose consuming calls in the main expression. Calls inside
positive assertions, negative assertions and anchor-observer machinery require
separate semantics and an explicit unsupported-scope diagnostic when requested.
Do not present their absence as proof that no assertion used a fragment.
Intersection and repetition require complete occurrence accounting within the
selected witness and budgeted work; a set of definition names is insufficient.

After a policy changes the published span, preserve raw occurrence coordinates
with a declared raw-coordinate contract, or provide a separate retained view.
Do not silently clamp spans or discard evidence while reporting it as complete.
The existing raw-versus-published capture contract in
[MatchData](../lingrep/src/protocol/query.rs) provides the compatibility question
that must be resolved, not permission to assume identical behavior.

## Counting, identity and compatibility

| Boundary | Consequence |
| --- | --- |
| Match identity and order | Evidence is an annotation of an emitted record. It does not create one record per fragment or execution history. Complete `Match` values remain equal with evidence off/on. |
| Pattern metadata aggregation | Filter and aggregate authored values on emitting definitions. Runtime labels do not enter those groups without a separate declared analysis. |
| Selected-occurrence aggregation | Count occurrences, matches-with-an-occurrence, or distinct spans only through an explicitly named measure; these are different denominators. |
| Catalog fingerprint | Source metadata already participates because `catalog_fingerprint` hashes trimmed source with grammar/compiler/policy semantic identities. Adding or editing annotations changes this fingerprint. |
| Matcher semantic identity | Advance the matcher identity when an implementation changes counts for an unchanged catalog fingerprint and corpus analysis identity. An additive evidence projection must establish whether existing result counts remain identical. |
| Prepared-corpus identity | These changes operate on queries over stored tokens. They do not require retokenization or a token-schema revision while reader/analyzer inputs remain unchanged. |
| Count tables | Tables record corpus analysis identity, catalog fingerprint and matcher revision. Reject stale tables after a relevant semantic revision; do not invalidate token artifacts merely to rebuild result counts. |
| Query protocol | Negotiate an optional evidence capability or introduce a versioned contract after inspecting real consumers. Existing Rust deserialization behavior is not proof that every external schema validator accepts new fields or events. |
| Browser and saved questions | Store the evidence projection in the reproducible query recipe. Expired evidence is rerun under the declared semantics; an old saved result does not imply that newly requested evidence was computed. |

The semantic identity rules are defined in
[identity.rs](../lingrep-core/src/fsm/identity.rs). Count-table compatibility checks
are in [count_table.rs](../lingrep-corpus/src/count_table.rs). Prepared analysis
identity is a separate input boundary in
[corpus identity.rs](../lingrep-corpus/src/identity.rs).

A new syntax feature follows
[pattern-language co-development](pattern-language-co-development.md).
Syntax additions that preserve existing source semantics do not automatically
require invalidating every existing catalog. A change to existing lowering or
policy behavior does require reviewing the corresponding semantic revision.

## Cost model and the experiment that would change the recommendation

Static copying consumes compile-time storage proportional to derived values and
contributing references. Expanding each dependency path rather than deduplicating
by definition can grow much faster than the authored graph. Charge allocations
and traversal under the compiler's existing metadata, generated-owned-byte and
expansion-work budgets. Reuse a bounded dependency traversal; do not materialize
every reference path just to derive one value.

Selected-occurrence evidence costs extraction work and output proportional to the
retained occurrences in each match. Repetition can make that count proportional
to sentence length. All-derivation provenance can multiply accepting choices
through nested alternations and repetition even when the observable match set is
small. A shared derivation graph could compress repeated structure, but would
require its own consumer operations, equality and counting semantics. It does
not make enumeration of every history bounded. A small output limit alone does
not bound the search work spent producing it.

The first implementation therefore requires limits for both occurrence count or
bytes and extraction work, charged before allocation or traversal. Per-worker
state may depend on the current bounded sentence/batch; it must not accumulate
across the corpus. Preserve budget headroom for ordinary matching and cancel
extraction when the request is superseded. No speed or capacity improvement is
claimed by this proposal.

Before committing to a new feature, run one disposable experiment with this
contract:

| Field | Experiment contract |
| --- | --- |
| Decision | Do existing captures answer the proposed per-match consumer's question, or is a new selected-occurrence projection justified? |
| Alternatives | Explicit emitting-pattern metadata; explicit captures interpreted by the consumer; bounded consuming-call evidence. |
| Observation | On identical examples, compare the classifications that can be recovered, complete match equality, extraction work and retained occurrence bytes. Include equal-span ambiguity, repetition, nesting, optional calls, intersections and graph assignments. |
| Disconfirmation | If captures provide the required role-specific classification without losing information, retain the existing contract. If a selected-call probe disagrees with the emitted assignment or cannot enforce work limits, reject that candidate. |
| Realism boundary | Real compiler and match values over synthetic ambiguity fixtures plus the Soranoha author archive; no browser workflow or all-history enumeration. |
| Stop condition | The named consumer can recover its required distinctions, or one required distinction is unsupported with the reason established. |
| Disposal | Delete the probe; retain its fixture descriptions, bounded observations and decision in the issue tracker. Promote only acceptance fixtures needed by an adopted implementation. |

This experiment is proposed, not executed. A concrete per-match consumer and its
target role are still required to make its result decision-relevant.

## Verification and implementation handoff

If local metadata remains the selected behavior, add a reference example covering
a fragment with metadata and an unannotated referencing pattern. A compiler
characterization test should establish that the referencing definition remains
unannotated and that derived groups classify only emitting definitions. Keep the
existing authored-pattern filtering and aggregation work independent.

If evidence or explicit static derivation is adopted, split implementation into
independently verifiable work at these boundaries:

1. Specify the consumer, projection scope, conflicts, witness selection and limits.
   Pin optional calls, ambiguous alternatives, repetition and transformations before
   changing lowering or matching.
2. Implement the compiler model and validated catalog dictionary. A syntax change
   includes the coordinated grammar, lowerer, installed reference and contract
   tests. Reject unsupported keys/scopes at compilation or request binding.
3. Implement bounded extraction or static derivation. Property tests compare
   complete off/on `Match` values for evidence and compare typed values plus
   contributor diagnostics for derivation. Use an independent small evaluator for
   exhaustive finite ambiguity fixtures when available.
4. Add one integration contract through CLI export, corpus resolver and browser
   inspection. Verify direct/prepared parity, saved-recipe restoration, raw and
   rewritten spans, complete/failed streams and capability negotiation.
5. Profile a baseline before performance work; run the required counted-work and
   paired campaign lanes for any optimization claim. Test real Soranoha author
   data locally and the full corpus before a version or release, following
   [the testing guide](testing-guide.md#soranoha-corpus-validation).

The durable production changes would belong in current schemas, types, tests and
user documentation after a decision. Proposed implementation work and experiment
results remain in the issue tracker until adopted.

## Decisions for the maintainer

The recommended decision is to retain local authored metadata and leave general
propagation disabled. Existing defaults and explicit emitting patterns cover
static categories. Existing captures are the first candidate for a concrete
per-match role before a new feature is added.

Three independent choices remain for review:

1. Is there a named analysis that must classify individual matches by selected
   fragment role, beyond the definition-level categories provided by authored
   pattern metadata? Supply one ambiguous or repeated example that captures fail
   to express before adopting a new projection.
2. Is reducing repeated definition annotations sufficient justification for an
   explicit static-derivation declaration, with scalar agreement, explicit
   overrides and unsets preserved? This would remain independent of per-match
   evidence.
3. If per-match evidence is required, is one deterministic witness sufficient?
   Requiring all derivations selects a separate ambiguity-analysis feature with
   different output, identity and resource contracts.

The proposal can be reviewed while the three analysis features are implemented.
Accepting the recommendation resolves fragment metadata ownership; requesting
runtime evidence instead starts the bounded consumer experiment above.
