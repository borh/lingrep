//! Result-policy vocabulary and its per-sentence publication engine.
//!
//! Matching produces canonical raw matches. Policy decides which records are
//! published, how their spans are shaped, and under which names, without
//! changing the raw match stream.
//!
//! This module owns the policy value types and pure execution engine. The
//! compiler extracts and validates catalog declarations; the matcher computes
//! derived anchor positions through [`crate::fsm::AnchorEvaluator`].
//!
//! Transform operations run once in declaration order against each record's
//! current state. The policy program decides survival and span before
//! [`apply_policy_records`] applies record transforms.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter, Write as _};

use compact_str::CompactString;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::fsm::matcher::predicate::{PredicateCatalog, PredicateId};
use crate::fsm::policy_engine::ProgramStats;
use crate::fsm::types::{Capture, Match};
use crate::token_match::{
    AttributeKey, AttributeKind, SequenceToken, TokenInfoTrait, TokenSequence,
};

/// The metadata keys the engine recognizes.
///
/// Kept as one list because it is three things at once: what
/// `compiler::policy` parses, what the typo detector measures unknown keys
/// against, and the documented surface a researcher writes. A key added
/// to the parser but not here would be silently unrecognized *and* would make
/// its own correct spelling look like a typo of something else.
pub const POLICY_KEYS: &[&str] = &[
    "output_name",
    "groups",
    "priority_order",
    "key",
    "publish",
    "transform",
    "tags",
    "render",
];

/// Named, code-reviewed unit renderers.
///
/// The registry is empty, so every `render:` use is a compile error naming the
/// known set.
///
/// **Adding any entry changes what a `render:` key means, and therefore MUST
/// bump [`crate::fsm::POLICY_SEMANTICS_IDENTITY`] in the same commit.** An
/// entry shipped under the old identity would fingerprint new semantics under
/// a name that promises the old ones. Adding the first entry also relaxes the `substitute`-without-`join`
/// check in extraction, because a renderer makes
/// [`PublishedRecord::overrides`] observable.
pub const RENDERER_REGISTRY: &[&str] = &[];

/// A compiled transform guard: one lingrep patterns token predicate.
///
/// Guards are the pattern language's own token predicates rather than a second
/// predicate dialect, so `[upos in {AUX, ADP} & lemma != れる]` means here
/// exactly what it means in a definition body — including the rule that a token
/// lacking the attribute satisfies neither polarity, which comes free from
/// reusing the predicate evaluator instead of reimplementing comparison.
///
/// Compilation lives in the compiler (`compiler::guard`), because it needs the
/// parser; evaluation lives here, because the engine runs it. Only single token
/// predicates compile: every v1 op judges one token at a time, so a wider body
/// would be a guard no op could evaluate. Rejecting a bare `@FRAG` operand is
/// also load-bearing rather than incidental — reference-driven edge removal is
/// what `detach` does, and admitting it here would create a second general
/// edge-removal mechanism.
#[derive(Clone)]
pub struct CompiledGuard {
    /// The predicate's attribute constraints. Empty is the any-token predicate
    /// `[]`, which admits every token including one with no attributes set.
    attrs: Vec<crate::fsm::SymbolicAttrValue>,
    /// The authored source, for diagnostics.
    source: String,
    /// A one-entry catalog holding `attrs` in evaluable form. Derived from
    /// `attrs`, so it takes no part in equality or formatting.
    catalog: PredicateCatalog,
    id: PredicateId,
}

impl CompiledGuard {
    /// Interns `attrs` into a private catalog. Called by the compiler once per
    /// authored guard, at catalog compile time.
    pub(crate) fn new(attrs: Vec<crate::fsm::SymbolicAttrValue>, source: String) -> Self {
        let mut catalog = PredicateCatalog::default();
        let id = catalog.intern(&attrs);
        Self {
            attrs,
            source,
            catalog,
            id,
        }
    }

    /// Whether this guard admits `token`.
    pub(crate) fn matches<T: TokenInfoTrait>(&self, token: &T) -> bool {
        self.catalog.matches(self.id, token)
    }

    /// The attributes this guard reads, so a run can cache them.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeKey> + '_ {
        self.attrs.iter().map(|attr| attr.attr.clone())
    }

    /// The value kinds required by this guard's operators.
    pub(crate) fn required_attribute_kinds(&self) -> Vec<(AttributeKey, AttributeKind)> {
        self.catalog.required_kinds()
    }
}

impl std::fmt::Debug for CompiledGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The authored source is the only part a reader of a failing assertion
        // can act on; the interned catalog is an implementation detail and
        // `AttrValue`'s Debug is verbose enough to bury it.
        formatter
            .debug_tuple("CompiledGuard")
            .field(&self.source)
            .finish()
    }
}

impl PartialEq for CompiledGuard {
    /// Two guards are equal when they constrain tokens identically. The source
    /// is deliberately excluded: `[upos=AUX]` and `[ upos = AUX ]` are one guard,
    /// and a policy comparison should not turn on whitespace.
    fn eq(&self, other: &Self) -> bool {
        self.attrs == other.attrs
    }
}

impl Eq for CompiledGuard {}

/// One entry of a `transform:` list: an operation and the span it rewrites.
///
/// The list is ordered and each entry runs exactly once. A repeated op kind is
/// legal because execution applies list entries, not unique operation kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransformStep {
    /// The named capture this step is confined to, or the whole record when
    /// absent.
    ///
    /// Scoping is what lets one record carry a transformed *part* and keep the
    /// rest. Unscoped, `drop_edge` narrows the record span and
    /// `narrow_to_span` then discards every capture that fell outside it —
    /// so a `key:` projecting one of those captures cannot, and the record
    /// publishes unkeyed. Measured over 946 NPV records on a 600-sentence
    /// GiNZA CoNLL-U corpus: 384 unkeyed, 40.6%, with `captures_dropped: 384`.
    /// The same `key:` over the same corpus projects fine with no `transform:`
    /// declared, which is what isolates the interaction rather than the
    /// projection.
    ///
    /// A scoped step rewrites **only** its own capture. The record span, the
    /// other captures, and any capture nested inside this one are untouched:
    /// captures are independent facts about one match, and a step that said
    /// where it applies should not silently reach past it. A step naming a
    /// capture the match did not bind — an alternation branch that did not
    /// fire — does nothing, for the same reason a token lacking an attribute
    /// is left alone rather than blanked.
    pub scope: Option<CompactString>,
    /// The operation to run over that span.
    pub op: TransformOp,
}

/// One operation of a `transform:` list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransformOp {
    /// Remove tokens from one edge while `drop` admits them and `keep` does
    /// not, scanning monotonically inward.
    ///
    /// This is **predicate-driven** removal: a judgement about tokens, with P1
    /// witnesses (飛び立つでしょう → 飛び立つ). It is not a generalization of
    /// the `detach` statement, which is **reference-driven** — a judgement
    /// about records, saying that a named region belongs to some other record.
    /// Greediness is a property of an expression, not an operation. Combining
    /// the two mechanisms would force reference-driven removal to inherit the
    /// monotonic scan used here, so `drop_edge` and `detach` remain distinct.
    DropEdge {
        /// Scan from the start when true, from the end when false.
        leading: bool,
        /// Tokens this admits are candidates for removal.
        drop: CompiledGuard,
        /// Candidates this admits are kept, ending the scan. P1's witness is
        /// the voice auxiliary natsume preserves (`lemma in {れる, られる}`).
        keep: Option<CompiledGuard>,
    },
    /// Cut the span at the first token `stop` admits, keeping what precedes it.
    ///
    /// One position scan, no loop: the *first* satisfying position wins, and a
    /// span with no satisfying position is unchanged.
    TruncateBefore { stop: CompiledGuard },
    /// Replace one edge token's rendered surface with `attr`, mapped through
    /// `table`.
    ///
    /// `component:` names an **edge** token and always did: writing several
    /// tokens from one attribute has no defined meaning, so `span` and a
    /// `$capture` component stay refused. What a capture selects is the *span
    /// the edge is an edge of*, and that is [`TransformStep::scope`] — a
    /// separate field, so the two questions "which span" and "which end of it"
    /// keep their separate answers.
    Substitute {
        target: SubstituteTarget,
        /// The attribute read off the target token. A token lacking it is left
        /// unsubstituted, for the same reason a token lacking an attribute
        /// satisfies neither polarity of a predicate.
        attr: AttributeKey,
        /// Value rewrites applied to what `attr` yielded, e.g. `為る → する`.
        /// A value absent from the table passes through unchanged. Linear
        /// lookup: real tables have one or two entries.
        table: Vec<(String, String)>,
    },
    /// Render the unit from the current span state, honouring substitutions.
    Join,
}

/// Which edge token a `substitute` writes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubstituteTarget {
    First,
    Last,
}

/// Where one tag's value comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagSource {
    /// Form (a): the same value on every record of this pattern.
    Static(String),
    /// Form (b): the value of whichever named capture participated in this
    /// match — how a record says which alternation branch fired.
    ///
    /// Sorted by capture name so the policy's stored shape is a property of the
    /// catalog rather than of the build graph; see the note on
    /// [`PatternPolicy::key`] for why that is not paranoia.
    CapturePresence(Vec<(CompactString, String)>),
    /// Form (c): an attribute projected off a named capture's sole token.
    ///
    /// Single-token by construction — `capture_key` refuses a reference it
    /// cannot prove covers exactly one token, and unlike [`KeyProjection`] there is
    /// no `{first|last|span}` spelling to waive that proof.
    Projection {
        capture: CompactString,
        attr: AttributeKey,
    },
}

/// How far before the anchor a record may end and still be published.
///
/// The argument to an `admit` statement's `terminal:`, measured against an
/// *anchor*: the boundary a record is judged sentence-final against.
/// `{exact: true, …}` admits only a record ending exactly at the anchor;
/// `{within: N, …}` also admits one ending up to N tokens before it. The two
/// are one concept spelled two ways, so they are one type rather than two
/// fields a statement could declare together.
///
/// See [`Terminal::admits`] for the comparison itself, including the exclusive
/// end convention under which "ending at the anchor" is `end == anchor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminal {
    /// The record must end at the anchor.
    Exact,
    /// The record must end at most this many tokens before the anchor.
    /// `Within(0)` is equivalent to [`Terminal::Exact`].
    Within(u32),
}

impl Terminal {
    /// Whether a record ending at `end` is admitted against `anchor`.
    ///
    /// `end` is exclusive, matching `Match`'s span convention, so a record
    /// ending at the anchor has `end == anchor`.
    ///
    /// **One-sided** (`policy-sublanguage.md:248`): a record ending *past* the
    /// anchor is refused under both variants, at any distance. Material after
    /// the propositional end is citation apparatus, and a record reaching into
    /// it is not sentence-final in the sense a researcher means.
    #[must_use]
    pub fn admits(self, end: usize, anchor: usize) -> bool {
        match self {
            Self::Exact => end == anchor,
            // `checked_sub` carries the one-sidedness: a record ending past
            // the anchor has no distance to compare, so no separate guard
            // stands in front of a subtraction.
            Self::Within(allowed) => anchor
                .checked_sub(end)
                .is_some_and(|distance| distance <= allowed as usize),
        }
    }
}

/// One definition's declared publication policy.
///
/// Every field is optional: a catalog declares only the decisions it actually
/// makes, and a definition carrying no policy key has no entry at all rather
/// than an all-default one (see [`CatalogPolicy::entries`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatternPolicy {
    /// Publish under this name instead of the definition's own.
    pub output_name: Option<CompactString>,
    /// The groups this definition belongs to, selectable as `%name` from a
    /// `policy` block.
    ///
    /// Membership alone does nothing. Under the retired `priority_group:` a
    /// declaration *was* the adjudication — naming a group ran an overlap
    /// contest — so a catalog could not group definitions for any other
    /// purpose. A group is inert until a statement names it, which is what lets
    /// `patterns/sf-experimental.lx` group thirteen definitions as a `detach`
    /// subject and contest none of them.
    ///
    /// A list rather than one name because the two uses are independent: the
    /// set a `resolve` contests and the set a `detach` shapes have no reason to
    /// coincide, and one string forced a catalog to choose.
    pub groups: Vec<CompactString>,
    /// Rank a `resolve` reads through
    /// [`Criterion::PriorityOrder`](crate::fsm::policy_program::Criterion);
    /// lower wins. Absent means `0`.
    ///
    /// **Not retired with the group keys**, because it is not one of them: they
    /// declared a contest, and this supplies one term of a rank the author
    /// writes. A `rank: [priority_order, …]` is where it is read now.
    ///
    /// Order dominates length wherever both appear, and the criterion list is
    /// where the author says so. This differs from JAPE's `appelt` and
    /// dm-annotations' `suppress_inner_layers`, which put length first.
    pub priority_order: Option<i64>,
    /// Group and count this record by an ordered projection.
    ///
    /// Source order is semantic: it is the tuple/column order downstream
    /// consumers observe. The dedicated lingrep patterns list syntax retains that order
    /// directly instead of routing it through a map representation.
    pub key: Vec<KeyProjection>,
    /// Token attributes to carry into published records without constraining
    /// on them.
    ///
    /// A run publishes the attributes it *reads* — the catalog's own, plus
    /// `text`, plus what its policy projects — which leaves a consumer that
    /// needs a value the query does not test with no way to ask for it. The
    /// witnessed case is reconstructing character offsets from CoNLL-U, which
    /// needs `conllu.misc` for `SpaceAfter=No`; before this key the only
    /// spelling was a predicate contrived never to fire,
    /// `[upos=VERB & !(conllu.misc has __never__)]`, which works only because
    /// the true complement holds for a missing attribute and reads as a typo.
    ///
    /// Declared per definition and unioned across the catalog, exactly as
    /// `key:` is: the run's projection is one set, so an attribute one pattern
    /// publishes rides on every record. That is the existing shape rather than
    /// a new one, and narrowing it per pattern would be a protocol change
    /// nothing has asked for.
    ///
    /// It contributes no *required* value kind, because nothing reads it: an
    /// attribute is carried at whatever kind the schema declares, and a schema
    /// that does not declare it at all is the ordinary
    /// `warning[attribute_unavailable]`.
    pub publish: Vec<AttributeKey>,
    /// Ordered record transformations, applied exactly once each.
    ///
    /// Crate-internal deliberately: the vocabulary is the engine's, and what a
    /// consumer needs is the *result* — [`PublishedRecord`]'s `unit`, `key` and
    /// `tags`, which are public. Exposing the op list would make every
    /// downstream crate a party to changes in how a unit gets built.
    pub transform: Vec<TransformStep>,
    /// Per-instance annotations, in catalog order.
    pub tags: Vec<(CompactString, TagSource)>,
    /// A named renderer from [`RENDERER_REGISTRY`].
    pub render: Option<CompactString>,
}

/// Which tokens of a capture a key field reads.
///
/// There is no default for a capture wider than one token, and that is the
/// finding rather than an omission. Measured over 500 documents
/// (`docs/results/2026-08-04-multi-token-keys/`), 93.7% of capture occurrences
/// are a single token; the remaining 6.3% splits into two populations wanting
/// **opposite** rules. Compound nominals want concatenation — `細胞間`, where
/// reading the leftmost token gives `細胞`. Suru compounds want the leftmost
/// token — `減少し`, where concatenation gives `減少為る`, which is not a word.
/// Either rule as a default silently mangles the other population.
///
/// The names are **positional, not linguistic**. `First` says where the token
/// is, not that it is the head: 減少し has its semantic head leftmost because
/// し is a light verb, while Japanese compounds are generally right-headed. A
/// variant called `Head` would claim an analysis the engine cannot perform.
///
/// They are also not invented here — `first`, `last` and `span` are the
/// multi-token values of the transform sublanguage's existing `component`
/// rule, so the words mean one thing across the language. The concatenating
/// variant is `Span` and not `Join` for the same reason: `{op: join}` is
/// already a transform op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyComponent {
    /// The capture's only token.
    ///
    /// Distinct from [`KeyComponent::First`] even though the two project
    /// identically: this is the form the compiler *proves* covers exactly one
    /// token, and `First` is the form that waives that proof. Collapsing them
    /// would discard the proof obligation, which is the whole guard.
    #[default]
    Sole,
    /// The leftmost token.
    First,
    /// The rightmost token.
    Last,
    /// Every token, concatenated in order with no separator.
    Span,
}

impl KeyComponent {
    /// The spellings an author may write, for diagnostics that have to list
    /// them. [`KeyComponent::Sole`] is absent because it is spelled by
    /// omitting an explicit component, not by naming it.
    pub const EXPLICIT_NAMES: &'static [&'static str] = &["first", "last", "span"];

    /// Parses an authored component name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "first" => Some(Self::First),
            "last" => Some(Self::Last),
            "span" => Some(Self::Span),
            _ => None,
        }
    }
}

/// Which span one field of a record key projects from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyTarget {
    /// The complete published match, statically proven exactly one token wide.
    Match,
    /// One named capture and the component of its span to read.
    Capture {
        name: CompactString,
        component: KeyComponent,
    },
    /// One named enhanced graph node. It has identity but no token span.
    GraphNode { name: CompactString },
    /// One named enhanced edge; the first slice exposes its `rel` value.
    GraphEdge { name: CompactString },
    /// The authored member that produced a lexical occurrence.
    LexicalTerm,
    /// Per-token values across the raw lexical occurrence span.
    LexicalSegments,
}

/// One field of a record key, retained in authored order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyProjection {
    pub target: KeyTarget,
    /// The attribute to read off them.
    pub attribute: AttributeKey,
    canonical_name: CompactString,
}

impl KeyProjection {
    #[must_use]
    pub fn match_attribute(attribute: AttributeKey) -> Self {
        let canonical_name = CompactString::new(attribute.as_str());
        Self {
            target: KeyTarget::Match,
            attribute,
            canonical_name,
        }
    }

    #[must_use]
    pub fn capture(
        name: impl Into<CompactString>,
        component: KeyComponent,
        attribute: AttributeKey,
    ) -> Self {
        let name = name.into();
        let component_name = match component {
            KeyComponent::Sole => "",
            KeyComponent::First => ".first",
            KeyComponent::Last => ".last",
            KeyComponent::Span => ".span",
        };
        let canonical_name = format!("${name}{component_name}.{attribute}").into();
        Self {
            target: KeyTarget::Capture { name, component },
            attribute,
            canonical_name,
        }
    }

    #[must_use]
    pub(crate) fn unresolved_edge_relation(name: impl Into<CompactString>) -> Self {
        let name = name.into();
        Self {
            target: KeyTarget::Capture {
                name: name.clone(),
                component: KeyComponent::Sole,
            },
            attribute: AttributeKey::new("dep.rel").expect("dep.rel is canonical"),
            canonical_name: format!("${name}.rel").into(),
        }
    }

    pub(crate) fn retarget_graph_node(&mut self, name: CompactString) {
        self.target = KeyTarget::GraphNode { name };
    }

    pub(crate) fn retarget_graph_edge(&mut self, name: CompactString) {
        self.target = KeyTarget::GraphEdge { name };
    }

    #[must_use]
    pub fn lexical_term() -> Self {
        Self {
            target: KeyTarget::LexicalTerm,
            attribute: AttributeKey::new("text").expect("form is canonical"),
            canonical_name: CompactString::const_new("term"),
        }
    }

    #[must_use]
    pub fn lexical_segments(attribute: AttributeKey) -> Self {
        let canonical_name = format!("segments({attribute})").into();
        Self {
            target: KeyTarget::LexicalSegments,
            attribute,
            canonical_name,
        }
    }

    /// Canonical source spelling, also used as the published field name.
    #[must_use]
    pub fn name(&self) -> CompactString {
        self.canonical_name.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectedKeyField {
    pub name: CompactString,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LexicalProjectedKeyValue {
    Scalar(String),
    Segments(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LexicalProjectedKeyField {
    pub name: CompactString,
    pub value: LexicalProjectedKeyValue,
}

impl PatternPolicy {
    /// Whether this policy declares any decision at all.
    ///
    /// A `PatternPolicy` that answers `false` here must never be stored: an
    /// entry's presence is what makes `declared()` meaningful, so an empty
    /// entry would make a policy-free catalog claim to have a policy.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The group rank used when ordering within a priority group.
    #[must_use]
    pub fn order(&self) -> i64 {
        self.priority_order.unwrap_or(0)
    }
}

/// Structural identity of a catalog's declared anchor set.
///
/// Unkeyed BLAKE3, matching the workspace's V6 artifact convention.
///
/// **Structural, not per-instance, and that is the point.** An anchor position
/// is a pure function of the sentence's tokens and the `exclude` expression, so
/// two independently compiled catalogs declaring the same anchors are genuinely
/// interchangeable; an instance identity would refuse a substitution that cannot
/// change an answer. Clone-stable by construction.
///
/// [`Self::default`] is the identity of a catalog that declares **no** anchors,
/// and is deliberately not the hash of an empty walk: declaring nothing must
/// leave a catalog bit-identical to one written before anchors existed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnchorSetDigest([u8; 32]);

impl AnchorSetDigest {
    /// Wraps a computed digest. Crate-internal: the only supported way to
    /// obtain one is to compile a catalog.
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Why an anchor position could not be read.
///
/// **Both variants are library wiring errors, not authoring mistakes**, so both
/// messages are terse and factual and neither carries a fix-it. The compiler
/// already refuses every authoring mistake in this area — an `admit` naming
/// an undeclared anchor is an `InvalidPolicyValue` diagnostic with a source span
/// and its own remediation — so a researcher who could act on advice here has
/// been told already, and one who sees this string cannot act on it.
///
/// A caller with a user-facing surface should wrap these rather than print
/// them, which is where remediation phrased for that surface belongs.
/// [`AnchorResolutionError`] is the engine's own wrapper, and adds the one fact
/// no reader of this type can recover: which anchor was asked for.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnchorError {
    /// The positions were computed by a catalog declaring a different anchor
    /// set, so the position asked for would mean something else.
    #[error("anchor positions were computed by a catalog declaring a different anchor set")]
    CatalogMismatch,
    /// The catalog declares no anchor by this name.
    ///
    /// Deliberately not a fall back to the raw sentence end; see
    /// [`AnchorPositions`].
    ///
    /// Reachable only for a name that came from neither catalog. Past the digest
    /// check the two anchor sets are structurally identical — `anchor_set_digest`
    /// hashes each anchor's *name* alongside its `exclude` body — so a name in
    /// one is in the other, and a name in neither did not come from a compiled
    /// `admit` at all.
    #[error("no anchor named `{0}` is declared by this catalog")]
    Unknown(CompactString),
}

/// A declaration whose anchor position could not be read.
///
/// What [`apply_policy`] fails with, and the reason it does not simply return
/// [`AnchorError`]: the engine knows *which anchor* was asked for, and nothing
/// downstream can recover that. `AnchorError` is the accessor's answer to
/// "could this position be read?" and stays that; this is the engine's answer
/// to "which declaration went unanswered?".
///
/// An `admit` statement is the only thing that names an anchor at publication,
/// so the message names one without an identifier: a statement has no name, and
/// carrying its [`PolicySpan`] would need the compiler's source registry, which
/// no runtime caller holds — a carried span would be a pair of integers a
/// reader could do nothing with. Reported for the first `admit` that fails, in
/// authored order; the cause is catalog-wide rather than per-record, so there
/// is no second failure with different information.
///
/// **A wiring error, not an authoring one, and the message says so by saying
/// nothing about editing lingrep patterns.** An `admit` naming an undeclared anchor is
/// refused at compile time with a source span and its own remediation
/// (`compiler::policy::resolve_anchor_references`), and a compiled catalog's own
/// positions always carry its own digest — so a researcher who reaches this
/// string is looking at a caller that paired a policy with someone else's
/// positions. Remediation phrased for a particular surface belongs in whatever
/// wraps this, which knows what that surface is.
///
/// [`PolicySpan`]: crate::fsm::policy_program::PolicySpan
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("an `admit` statement measures against anchor `{anchor}`: {cause}")]
pub struct AnchorResolutionError {
    /// The anchor it named.
    pub anchor: CompactString,
    /// Why the position could not be read.
    #[source]
    pub cause: AnchorError,
}

/// One sentence's derived anchor positions.
///
/// Opaque, and deliberately without a `Default` or a public constructor. An
/// earlier design made this a plain map and told anchor-free callers to pass an
/// empty one, which left nothing preventing an *anchored* catalog from being
/// handed that same empty value and silently admitting against the raw sentence
/// end. A missing
/// entry is an [`AnchorError::Unknown`], never a fallback, and the only way to
/// obtain a value is to ask a compiled catalog for one.
///
/// The carried [`AnchorSetDigest`] is *structural*, so positions from a second
/// compilation of the same source are accepted. That is not a weakening: an
/// anchor position is a pure function of the tokens and the `exclude`
/// expression, so the substitution cannot change an answer.
#[derive(Clone, Debug)]
pub struct AnchorPositions {
    digest: AnchorSetDigest,
    positions: BTreeMap<CompactString, usize>,
}

impl AnchorPositions {
    /// The position of `name`, checked against the anchor set the caller
    /// expects.
    ///
    /// `expected` is normally [`crate::fsm::CompiledCatalog::anchor_digest`] of the
    /// catalog whose policy is being applied. Passing it is what makes the
    /// mismatch reportable at all; there is no unchecked accessor.
    ///
    /// # Errors
    ///
    /// [`AnchorError::CatalogMismatch`] when the positions come from a
    /// structurally different anchor set, and [`AnchorError::Unknown`] when the
    /// set declares no such anchor.
    pub fn get(&self, name: &str, expected: &AnchorSetDigest) -> Result<usize, AnchorError> {
        if self.digest != *expected {
            return Err(AnchorError::CatalogMismatch);
        }
        self.positions
            .get(name)
            .copied()
            .ok_or_else(|| AnchorError::Unknown(CompactString::new(name)))
    }

    /// Crate-internal, so only the anchor evaluator can mint one.
    pub(crate) fn new(digest: AnchorSetDigest, positions: BTreeMap<CompactString, usize>) -> Self {
        Self { digest, positions }
    }

    /// Hand-built positions, for tests that assert admission without running a
    /// recognizer.
    ///
    /// `#[cfg(test)]` alone would not reach `tests/`, which is a separate crate
    /// and depends on `lingrep-core` with `features = ["testing"]` for exactly this.
    ///
    /// The digest still has to be a real one — obtained from a compiled catalog
    /// — so a test cannot fabricate an anchor set that no catalog declares.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn new_for_test(
        digest: AnchorSetDigest,
        positions: BTreeMap<CompactString, usize>,
    ) -> Self {
        Self::new(digest, positions)
    }
}

/// One `witness:` or `divergence:` sentence, with its expected anchor.
///
/// Written `…|…`: a single `|` marks the token boundary the anchor is expected
/// to land on. The claim therefore carries its input and its expected outcome
/// in one string and needs no token indices, which would be tokenizer-dependent
/// — the portability failure the upstream differential recorded for
/// `filter_sentence_final`'s five-token window.
///
/// Constructed only by the compiler's crate-internal parse, so the sentinel is
/// always a valid char offset into the sentence and the verifier has no
/// re-validation to do. What is stored is the sentence *without* the marker,
/// because that is what a tokenizer is handed; [`Display`] puts the marker back
/// for a diagnostic to quote the entry as the author wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestingEntry {
    sentence: String,
    sentinel: usize,
}

impl NestingEntry {
    /// Splits `…|…` into the sentence and the sentinel's char offset.
    ///
    /// # Errors
    ///
    /// The number of `|` found, when it is not exactly one. A sentence
    /// containing a literal `|` therefore cannot serve as a witness — stated
    /// and checked rather than assumed.
    pub(crate) fn parse(entry: &str) -> Result<Self, usize> {
        let found = entry.matches('|').count();
        if found != 1 {
            return Err(found);
        }
        let Some((before, after)) = entry.split_once('|') else {
            // Unreachable given the count above, and written as a value rather
            // than an `expect` so this stays panic-free by construction.
            return Err(found);
        };
        Ok(Self {
            sentence: format!("{before}{after}"),
            sentinel: before.chars().count(),
        })
    }

    /// The sentence a tokenizer is handed: the entry without its sentinel.
    #[must_use]
    pub fn sentence(&self) -> &str {
        &self.sentence
    }

    /// The sentinel's offset in **chars**, which the verifier maps onto a token
    /// boundary.
    #[must_use]
    pub fn sentinel(&self) -> usize {
        self.sentinel
    }
}

impl Display for NestingEntry {
    /// The entry as authored, sentinel restored.
    ///
    /// Rebuilt char by char rather than by slicing, because the sentinel is a
    /// char offset and this crate denies `clippy::string_slice`.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let mut written = 0usize;
        for character in self.sentence.chars() {
            if written == self.sentinel {
                formatter.write_char('|')?;
            }
            formatter.write_char(character)?;
            written += 1;
        }
        if written == self.sentinel {
            formatter.write_char('|')?;
        }
        Ok(())
    }
}

/// How deep an anchor's `exclude` fragment nests, and the sentences that prove
/// it.
///
/// An `exclude` fragment is a **bounded approximation** of something unbounded:
/// upstream's citation stripper is a scanner with a counter, and a lingrep patterns
/// fragment is a regular expression, so it can only recognize bracket nesting
/// to a fixed depth. This records where the approximation stops.
///
/// A catalog must write all three fields, with both lists non-empty; that is
/// the load-bearing choice in the *encoding*, because it leaves a depth no
/// sentence exercises no way to be spelled. The refusal lives in
/// `compiler::policy::read_nesting_claims`, not in this type, which is a parsed
/// result and constrains nothing on its own.
/// `docs/results/2026-08-04-anchor-nesting-claim/` records how the schema was
/// settled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestingClaim {
    /// The nesting the fragment unrolls.
    pub depth: u32,
    /// Sentences nesting to exactly [`Self::depth`], whose derived anchor MUST
    /// equal the sentinel.
    pub witness: Vec<NestingEntry>,
    /// Sentences nesting to `depth + 1`, whose derived anchor MUST NOT equal
    /// the sentinel.
    ///
    /// This half is why a `counterexamples:`-shaped claim would mean nothing:
    /// at depth N+1 the fragment still matches, and what fails is *where the
    /// anchor lands*.
    pub divergence: Vec<NestingEntry>,
}

/// Every policy decision one catalog declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogPolicy {
    /// Keyed by definition name, in declaration order. A definition carrying
    /// no policy key is absent — see [`PatternPolicy::is_empty`].
    ///
    /// Write-once: set by [`CatalogPolicyBuilder::build`] and unreachable for
    /// mutation afterwards. Read it with [`Self::entries`]; edit it by way of
    /// [`Self::into_builder`], which rebuilds.
    entries: IndexMap<CompactString, PatternPolicy>,
    /// Derived sentence anchors this catalog declares: anchor name to
    /// the name of the `exclude` fragment recognizing the trailing material
    /// that anchor strips.
    ///
    /// A `BTreeMap` for the same reason [`Self::hidden`]'s producer sorts: the
    /// stored shape must be a property of the catalog, not of which definition
    /// happened to carry the declaration.
    pub anchors: BTreeMap<CompactString, CompactString>,
    /// Structural identity of [`Self::anchors`]. See [`AnchorSetDigest`].
    pub anchor_digest: AnchorSetDigest,
    /// Each declared anchor's `max_nesting` claim, keyed by **anchor name**
    /// rather than by the `exclude` fragment carrying it, because two anchors
    /// may share one `exclude` and a claim is verified per anchor.
    ///
    /// The same keys as [`Self::anchors`]: every declared anchor must carry a
    /// claim, so a missing one is refused at compile
    /// time. It is separate from `anchors` all the same — a claim is
    /// provenance, and no part of what [`Self::anchor_digest`] hashes, because
    /// editing a witness changes nothing about which boundary an anchor picks.
    pub nesting_claims: BTreeMap<CompactString, NestingClaim>,
    /// Emitters synthesized to observe a fragment named as a policy statement's
    /// operand, mapping `__policy__<fragment>` to the fragment's own name.
    ///
    /// These are evidence, never output: they are absent from
    /// `CompiledCatalog::patterns()` and stripped unconditionally at the
    /// publication seam, `--raw-matches` included.
    pub hidden: IndexMap<CompactString, CompactString>,
    /// Group membership over the catalog's **complete** definition list.
    ///
    /// Private, unlike the four `pub` fields above — those are a recorded
    /// defect rather than a pattern to follow. Installed exactly once by
    /// `compiler::policy::install_group_index`, after synthesis has appended
    /// its evidence emitters, because [`DefId`] indexes the complete list.
    ///
    /// [`DefId`]: crate::fsm::policy_program::DefId
    group_index: crate::fsm::policy_program::GroupIndex,
    /// The catalog's definitions, for resolving a record's authored name to
    /// the [`DefId`] every program lookup keys on.
    ///
    /// Installed beside [`Self::group_index`] and under the same constraint:
    /// both index the **complete** definition list, so both wait for synthesis
    /// to finish appending.
    ///
    /// [`DefId`]: crate::fsm::policy_program::DefId
    definitions: crate::fsm::policy_program::DefinitionDomain,
    /// The ordered policy program, empty when none is declared.
    ///
    /// Private and write-once for the same reason `entries` is: a second
    /// writer would be a second thing to keep in step. Installed by the
    /// compiler after the group index, because binding resolves `%name`
    /// through it.
    program: crate::fsm::policy_program::BoundProgram,
}

impl CatalogPolicy {
    /// The ordered policy program, in authored order.
    ///
    /// Empty for a catalog declaring no `policy` block.
    #[must_use]
    pub fn program(&self) -> &crate::fsm::policy_program::BoundProgram {
        &self.program
    }

    /// Installs the bound program. Called once, after the group index.
    pub(crate) fn install_program(&mut self, program: crate::fsm::policy_program::BoundProgram) {
        debug_assert!(
            self.program.ops.is_empty(),
            "the policy program is installed exactly once"
        );
        self.program = program;
    }

    /// Group membership, for resolving `%name`.
    ///
    /// Empty for a catalog declaring no `groups:` anywhere. Today that is every
    /// tracked catalog but `patterns/sf-experimental.lx`, whose thirteen
    /// copula definitions carry `groups: ["trailing_copula"]`.
    #[must_use]
    pub fn group_index(&self) -> &crate::fsm::policy_program::GroupIndex {
        &self.group_index
    }

    /// Installs the group index. Called once, after synthesis appends.
    pub(crate) fn install_group_index(&mut self, index: crate::fsm::policy_program::GroupIndex) {
        debug_assert!(
            self.group_index == crate::fsm::policy_program::GroupIndex::empty(),
            "the group index is installed exactly once"
        );
        self.group_index = index;
    }

    /// The catalog's definitions, for resolving an authored name.
    ///
    /// Empty for a catalog declaring no policy at all: it is installed only
    /// where the program needs it, and building one per compilation for a
    /// catalog that will never consult it is work with no reader.
    #[must_use]
    pub fn definitions(&self) -> &crate::fsm::policy_program::DefinitionDomain {
        &self.definitions
    }

    /// Installs the definition domain. Called once, after synthesis appends.
    pub(crate) fn install_definitions(
        &mut self,
        domain: crate::fsm::policy_program::DefinitionDomain,
    ) {
        debug_assert!(
            self.definitions == crate::fsm::policy_program::DefinitionDomain::empty(),
            "the definition domain is installed exactly once"
        );
        self.definitions = domain;
    }
}

impl CatalogPolicy {
    /// Whether this catalog declares any policy.
    ///
    /// Hidden emitters alone do not count: they exist only to serve a declared
    /// operation, so they cannot be present without one. A **program** does
    /// count — reading `entries` alone sent a policy-only catalog down
    /// [`CompiledCatalog::find_published_matches`]'s raw fast path, publishing
    /// nothing the program decided.
    ///
    /// [`CompiledCatalog::find_published_matches`]: crate::fsm::CompiledCatalog::find_published_matches
    #[must_use]
    pub fn declared(&self) -> bool {
        !self.entries.is_empty() || !self.program.ops.is_empty()
    }

    /// The policy declared for `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PatternPolicy> {
        self.entries.get(name)
    }

    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut PatternPolicy> {
        self.entries.get_mut(name)
    }

    /// Every declared entry, by definition name, in declaration order.
    #[must_use]
    pub fn entries(&self) -> &IndexMap<CompactString, PatternPolicy> {
        &self.entries
    }

    /// Whether the catalog's program names an anchor.
    ///
    /// A catalog-wide fact, so a per-sentence caller can decide once whether
    /// anchor resolution has anything to do. An `admit` is the only statement
    /// that names one, so this answers for one source; it answered for two
    /// until `terminal:` retired.
    ///
    /// `apply_policy_records` skips its anchor-resolution loop on this, and
    /// that loop is what reports `Unknown`/`CatalogMismatch` **before** any
    /// statistic moves. Skipping it while an `Admit` names an anchor would
    /// decide records against an unresolved anchor.
    ///
    /// Computed at read time rather than stored, because the program is
    /// installed after [`CatalogPolicyBuilder::build`] runs — a stored flag
    /// would be derived before the thing it describes exists.
    #[must_use]
    pub fn program_names_an_anchor(&self) -> bool {
        self.program
            .ops
            .iter()
            .any(|op| matches!(op, crate::fsm::policy_program::BoundOp::Admit { .. }))
    }

    /// Reopens this policy for editing, discarding the derived flag.
    ///
    /// The only way to change a built policy's entries, and it cannot leave the
    /// flag stale because it does not carry the flag: [`CatalogPolicyBuilder`]
    /// has no field for it, and `build` recomputes.
    #[must_use]
    pub fn into_builder(self) -> CatalogPolicyBuilder {
        CatalogPolicyBuilder {
            entries: self.entries,
            anchors: self.anchors,
            anchor_digest: self.anchor_digest,
            nesting_claims: self.nesting_claims,
            hidden: self.hidden,
        }
    }

    /// Every token attribute this catalog's policy reads, sorted and deduped.
    ///
    /// Matching and publication read different attributes, and only matching's
    /// set is derivable from the compiled automaton. A catalog whose patterns
    /// never mention `lemma` but whose `key:` projects it needs `lemma` cached
    /// all the same — otherwise a prepared-artifact run projects the attribute
    /// away and every record comes back unkeyed, with the run reporting a
    /// count that looks like an authoring mistake rather than a dropped
    /// projection. The same holds for a guard's attributes, a `substitute`
    /// target's, and the written form `join` renders from.
    ///
    /// Callers combine this with [`CompiledCatalog::required_attributes`]; see
    /// [`crate::execution::cached_run_attributes`], which owns the union.
    ///
    /// [`CompiledCatalog::required_attributes`]: crate::fsm::CompiledCatalog::required_attributes
    #[must_use]
    pub fn projected_attributes(&self) -> Vec<AttributeKey> {
        let mut attributes = BTreeSet::new();
        for policy in self.entries.values() {
            attributes.extend(
                policy
                    .key
                    .iter()
                    .filter(|field| {
                        !(matches!(field.target, KeyTarget::GraphEdge { .. })
                            || matches!(field.target, KeyTarget::GraphNode { .. })
                                && matches!(field.attribute.as_str(), "node.kind" | "node.id"))
                    })
                    .map(|field| field.attribute.clone()),
            );
            // Declared for a downstream consumer rather than read here, so it
            // joins the projection and nothing else.
            attributes.extend(policy.publish.iter().cloned());
            for step in &policy.transform {
                match &step.op {
                    TransformOp::DropEdge { drop, keep, .. } => {
                        attributes.extend(drop.attributes());
                        attributes.extend(keep.iter().flat_map(CompiledGuard::attributes));
                    }
                    TransformOp::TruncateBefore { stop } => {
                        attributes.extend(stop.attributes());
                    }
                    TransformOp::Substitute { attr, .. } => {
                        attributes.insert(attr.clone());
                    }
                    // `join` renders the written form of every unsubstituted
                    // token in the span, so it reads one attribute the authored
                    // text never names.
                    TransformOp::Join => {
                        attributes.insert(unit_surface().clone());
                    }
                }
            }
            for (_, source) in &policy.tags {
                match source {
                    TagSource::Projection { attr, .. } => {
                        attributes.insert(attr.clone());
                    }
                    // Presence is decided by the capture list, not by reading
                    // any token.
                    TagSource::Static(_) | TagSource::CapturePresence(_) => {}
                }
            }
        }
        attributes.into_iter().collect()
    }

    /// Value kinds required by every token attribute read during publication.
    ///
    /// This is the kind-bearing counterpart to [`Self::projected_attributes`].
    /// Keeping it on the policy prevents a new projection or transform from
    /// being cached by name while silently reading the wrong physical kind.
    pub(crate) fn required_attribute_kinds(&self) -> Vec<(AttributeKey, AttributeKind)> {
        let mut requirements = BTreeSet::new();
        for policy in self.entries.values() {
            requirements.extend(
                policy
                    .key
                    .iter()
                    .filter(|field| {
                        !(matches!(
                            field.target,
                            KeyTarget::LexicalTerm | KeyTarget::GraphEdge { .. }
                        ) || matches!(field.target, KeyTarget::GraphNode { .. })
                            && matches!(field.attribute.as_str(), "node.kind" | "node.id"))
                    })
                    .map(|field| (field.attribute.clone(), AttributeKind::Scalar)),
            );
            for step in &policy.transform {
                match &step.op {
                    TransformOp::DropEdge { drop, keep, .. } => {
                        requirements.extend(drop.required_attribute_kinds());
                        requirements.extend(
                            keep.iter()
                                .flat_map(CompiledGuard::required_attribute_kinds),
                        );
                    }
                    TransformOp::TruncateBefore { stop } => {
                        requirements.extend(stop.required_attribute_kinds());
                    }
                    TransformOp::Substitute { attr, .. } => {
                        requirements.insert((attr.clone(), AttributeKind::Scalar));
                    }
                    TransformOp::Join => {
                        requirements.insert((unit_surface().clone(), AttributeKind::Scalar));
                    }
                }
            }
            requirements.extend(policy.tags.iter().filter_map(|(_, source)| match source {
                TagSource::Projection { attr, .. } => Some((attr.clone(), AttributeKind::Scalar)),
                TagSource::Static(_) | TagSource::CapturePresence(_) => None,
            }));
        }
        requirements.into_iter().collect()
    }

    /// The name `definition` publishes under.
    ///
    /// This is the definition's own name unless it declares `output_name`.
    /// Any consumer that keys on pattern names — a per-pattern row table, a
    /// descriptor catalog — must seed itself from *these* names rather than
    /// from `CompiledCatalog::patterns()`, because a published record carries this
    /// name and a sink seeded the other way will not recognize it.
    ///
    /// The mapping is deliberately many-to-one: several definitions may
    /// publish under one name, which is how a catalog merges surface variants
    /// under a single label. Consumers building a name list must therefore
    /// deduplicate — see [`Self::published_names`].
    #[must_use]
    pub fn published_name<'a>(&'a self, definition: &'a str) -> &'a str {
        self.get(definition)
            .and_then(|entry| entry.output_name.as_deref())
            .unwrap_or(definition)
    }

    /// Returns distinct published names in the supplied definition order.
    #[must_use]
    pub fn published_names<'a>(
        &'a self,
        definitions: impl IntoIterator<Item = &'a str>,
    ) -> Vec<&'a str> {
        self.published_entries(definitions.into_iter().map(|name| (name, ())))
            .map(|(name, ())| name)
            .collect()
    }

    /// Maps entries to published names, keeping the first entry per name in input order.
    pub fn published_entries<'a, T>(
        &'a self,
        definitions: impl IntoIterator<Item = (&'a str, T)>,
    ) -> impl Iterator<Item = (&'a str, T)> {
        let mut seen = BTreeSet::new();
        definitions
            .into_iter()
            .map(move |(definition, entry)| (self.published_name(definition), entry))
            .filter(move |(name, _)| seen.insert(*name))
    }
}

/// A [`CatalogPolicy`] under construction.
///
/// It exists so a built policy's entries cannot be reached for mutation:
/// editing one means [`CatalogPolicy::into_builder`] and building again.
///
/// Only `anchors` is written during construction; the compiler installs
/// `anchor_digest` and `nesting_claims` on the built policy, and
/// `synthesize_hidden_emitters` installs `hidden`. The builder carries all four
/// anyway, because `into_builder` must return everything it took — a digest
/// dropped on a rebuild would fail an `AnchorPositions` check far from here.
#[derive(Clone, Debug, Default)]
pub struct CatalogPolicyBuilder {
    entries: IndexMap<CompactString, PatternPolicy>,
    /// See [`CatalogPolicy::anchors`].
    pub anchors: BTreeMap<CompactString, CompactString>,
    /// See [`CatalogPolicy::anchor_digest`].
    pub anchor_digest: AnchorSetDigest,
    /// See [`CatalogPolicy::nesting_claims`].
    pub nesting_claims: BTreeMap<CompactString, NestingClaim>,
    /// See [`CatalogPolicy::hidden`].
    pub hidden: IndexMap<CompactString, CompactString>,
}

impl CatalogPolicyBuilder {
    /// Declares `entry` for `name`, replacing any earlier one.
    pub fn insert_entry(&mut self, name: CompactString, entry: PatternPolicy) {
        self.entries.insert(name, entry);
    }

    /// Edits the entry for `name`, reporting whether one was there.
    pub fn modify_entry(&mut self, name: &str, edit: impl FnOnce(&mut PatternPolicy)) -> bool {
        let Some(entry) = self.entries.get_mut(name) else {
            return false;
        };
        edit(entry);
        true
    }

    /// Every entry declared so far, by definition name, in declaration order.
    #[must_use]
    pub fn entries(&self) -> &IndexMap<CompactString, PatternPolicy> {
        &self.entries
    }

    /// Freezes this into a [`CatalogPolicy`].
    #[must_use]
    pub fn build(self) -> CatalogPolicy {
        CatalogPolicy {
            entries: self.entries,
            anchors: self.anchors,
            anchor_digest: self.anchor_digest,
            nesting_claims: self.nesting_claims,
            hidden: self.hidden,
            // Empty until `install_group_index` runs, which is after synthesis
            // — the builder cannot know the complete definition list, because
            // synthesis has not appended to it yet.
            group_index: crate::fsm::policy_program::GroupIndex::empty(),
            // Empty for the same reason, and installed at the same point.
            definitions: crate::fsm::policy_program::DefinitionDomain::empty(),
            // Installed last because binding needs the post-synthesis group index.
            program: crate::fsm::policy_program::BoundProgram::default(),
        }
    }
}

/// The name prefix reserved for synthesized policy emitters.
///
/// Source-authored definitions may not start with it (the compiler rejects
/// them), which is what keeps synthesis from colliding with a catalog name.
pub const HIDDEN_PREFIX: &str = "__policy__";

/// Whether `name` is a synthesized policy emitter.
#[must_use]
pub fn is_hidden(name: &str) -> bool {
    name.starts_with(HIDDEN_PREFIX)
}

/// What one sentence's policy application suppressed or dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PolicyStats {
    /// Captures dropped because a span rewrite removed the tokens they covered.
    pub captures_dropped: u64,
    /// Records excluded from keyed output because their declared `key:` could
    /// not be projected.
    ///
    /// One counter for all three causes, because they have one disposition: a
    /// named capture did not participate in this match, a span rewrite dropped
    /// it, or the token it points at does not carry the projected attribute.
    /// Folding any of them in under a partial or empty key would silently group
    /// unrelated records together, so the record leaves keyed output and is
    /// counted instead.
    pub records_unkeyed: u64,
}

impl PolicyStats {
    pub(crate) fn merge_from(&mut self, other: Self) {
        self.captures_dropped += other.captures_dropped;
        self.records_unkeyed += other.records_unkeyed;
    }
}

/// Removes synthesized-emitter records, publishing nothing else.
///
/// This is pass 1 of [`apply_policy`] on its own, and it is what `--raw-matches`
/// runs. Raw output is therefore the catalog's own canonical matches exactly:
/// synthetics are evidence for the engine and never reach any published
/// stream, policy applied or not.
#[must_use]
pub fn strip_hidden(policy: &CatalogPolicy, matches: &[Match]) -> Vec<Match> {
    if policy.hidden.is_empty() {
        return matches.to_vec();
    }
    matches
        .iter()
        .filter(|record| !policy.hidden.contains_key(record.pattern.as_str()))
        .cloned()
        .collect()
}

/// Projects one record's declared key, or `None` when it has no key.
///
/// The key is what downstream grouping counts on: a `summarize` keyed on
/// the matched surface counts *spans*, and spans are not collocations —
/// `実験を行っ`, `実験を行う` and `実験を行い` are one (noun, verb) pair
/// published as three rows. Declaring `key: [$noun.lemma, $verb.lemma]`
/// makes them one.
///
/// `None` has two causes a caller must not conflate, and which it separates by
/// consulting the policy itself:
///
/// * the pattern declares no `key:` — nothing was asked for;
/// * it declares one, but a named capture did not participate in *this* match
///   (an alternation branch that skipped it). That record is genuinely
///   unkeyable and must be dropped from keyed output and **counted**, never
///   folded in under a partial key — a partial key would
///   collide with some other pattern's complete one.
///
/// Returns `(capture name, projected value)` pairs in the catalog's declared
/// order, because the key is a tuple.
pub fn project_key<S: TokenSequence + ?Sized>(
    policy: &CatalogPolicy,
    matched: &Match,
    tokens: &S,
) -> Option<Vec<ProjectedKeyField>> {
    let declared = &policy.get(&matched.pattern)?.key;
    project_key_fields(
        declared,
        matched.start..matched.end,
        &matched.captures,
        &matched.graph_bindings,
        // A raw match carries no substitutions: overrides are written by the
        // transform executor, which runs over a published record.
        &[],
        tokens,
    )
}

/// The projection itself, over an explicit capture set.
///
/// Separate from [`project_key`] because the transform executor keys off the
/// **surviving** components rather than the raw ones: a capture a span rewrite
/// dropped is gone for keying too. One implementation, two callers — the alternative was two copies
/// that would agree until the first edit.
/// Projects a record's declared key fields.
///
/// `overrides` are the per-token surfaces a `substitute` wrote. They are read
/// **only** for the written form [`unit_surface`] returns, because that is
/// what a substitution rewrites: the rendered surface, not the token's
/// `lemma`, `norm` or any other attribute. Reading raw tokens here was the
/// second half of the transform/key interaction — a `substitute` mapping
/// 為る → する reached the unit and never the key, so two fields of one record
/// disagreed about the same span.
fn project_key_fields<S: TokenSequence + ?Sized>(
    declared: &[KeyProjection],
    match_span: std::ops::Range<usize>,
    captures: &[Capture],
    graph_bindings: &[crate::fsm::GraphBinding],
    overrides: &[(usize, String)],
    tokens: &S,
) -> Option<Vec<ProjectedKeyField>> {
    if declared.is_empty() {
        return None;
    }
    let mut projected = Vec::with_capacity(declared.len());
    for field in declared {
        if let KeyTarget::GraphNode { name } | KeyTarget::GraphEdge { name } = &field.target {
            let binding = graph_bindings
                .iter()
                .find(|binding| binding.name() == name.as_str())?;
            projected.push(ProjectedKeyField {
                name: field.name(),
                value: binding.scalar(&field.attribute)?.to_owned(),
            });
            continue;
        }
        let (start, end, component) = match &field.target {
            KeyTarget::Match => (match_span.start, match_span.end, KeyComponent::Sole),
            KeyTarget::Capture { name, component } => {
                let capture = captures.iter().find(|capture| capture.name == *name)?;
                (capture.start, capture.end, *component)
            }
            KeyTarget::GraphNode { .. }
            | KeyTarget::GraphEdge { .. }
            | KeyTarget::LexicalTerm
            | KeyTarget::LexicalSegments => return None,
        };
        // A zero-width capture has no token to read, and is unkeyable for the
        // same reason a skipped one is. The compiler refuses to declare one,
        // so this guards the runtime against a shape it proved absent rather
        // than duplicating the check.
        if start >= end || end > tokens.len() {
            return None;
        }
        let substituted = |index: usize| -> Option<&str> {
            (field.attribute == *unit_surface())
                .then(|| {
                    overrides
                        .iter()
                        .find(|(at, _)| *at == index)
                        .map(|(_, value)| value.as_str())
                })
                .flatten()
        };
        let read = |index: usize| -> Option<String> {
            match substituted(index) {
                Some(value) => Some(value.to_owned()),
                None => tokens
                    .scalar_key(index, &field.attribute)
                    .map(ToString::to_string),
            }
        };
        let value = match component {
            // `Sole` is proven one token wide, so its start IS the token;
            // `First` waived that proof and reads the same position.
            KeyComponent::Sole | KeyComponent::First => read(start)?,
            KeyComponent::Last => read(end - 1)?,
            KeyComponent::Span => {
                let mut joined = String::new();
                for index in start..end {
                    // One missing attribute unkeys the whole record rather
                    // than silently yielding a shortened value: a truncated
                    // concatenation is a plausible key that is wrong, which is
                    // exactly what no output could be used to detect.
                    joined.push_str(&read(index)?);
                }
                joined
            }
        };
        projected.push(ProjectedKeyField {
            name: field.name(),
            value,
        });
    }
    Some(projected)
}

fn project_lexical_key_fields<S: TokenSequence + ?Sized>(
    declared: &[KeyProjection],
    matched: &Match,
    tokens: &S,
) -> Option<Vec<LexicalProjectedKeyField>> {
    let lexical = matched.lexical.as_deref()?;
    if matched.start > matched.end || matched.end > tokens.len() {
        return None;
    }
    declared
        .iter()
        .map(|field| {
            let value = match field.target {
                KeyTarget::LexicalTerm => {
                    LexicalProjectedKeyValue::Scalar(lexical.term.to_string())
                }
                KeyTarget::LexicalSegments => LexicalProjectedKeyValue::Segments(
                    (matched.start..matched.end)
                        .map(|index| {
                            tokens
                                .scalar_key(index, &field.attribute)
                                .map(ToString::to_string)
                        })
                        .collect::<Option<Vec<_>>>()?,
                ),
                KeyTarget::Match
                | KeyTarget::Capture { .. }
                | KeyTarget::GraphNode { .. }
                | KeyTarget::GraphEdge { .. } => return None,
            };
            Some(LexicalProjectedKeyField {
                name: field.name(),
                value,
            })
        })
        .collect()
}

/// One published record: raw identity plus everything policy derived from it.
///
/// Keeping raw identity separate from the published projection preserves
/// emission order: ordering and `--raw-matches` diffing key on
/// `raw`, so a transform that makes two spans equal cannot reorder them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedRecord {
    /// The canonical match this record came from, untouched.
    pub raw: Match,
    /// Published span start, after span rewrites.
    pub start: usize,
    /// Published span end, after span rewrites.
    pub end: usize,
    /// Published name — `output_name` applied last.
    pub name: CompactString,
    /// The rendered unit. `None` until a `join` runs.
    pub unit: Option<String>,
    /// The projected key. `None` when the pattern declares no `key:`,
    /// **and also** when it declares one that could not be projected — the two
    /// are distinguished by consulting the policy, and the second is counted in
    /// [`PolicyStats::records_unkeyed`].
    pub key: Option<Vec<ProjectedKeyField>>,
    /// A lexicon-only key, whose segment projections retain token boundaries.
    pub lexical_key: Option<Vec<LexicalProjectedKeyField>>,
    /// Per-instance annotations, in catalog order. A tag whose source is
    /// absent is omitted rather than emitted empty.
    pub tags: Vec<(CompactString, String)>,
    /// The raw captures that survived every span rewrite.
    ///
    /// One rule governs every narrowing operation: a capture whose span leaves the published span
    /// is **dropped, never clamped**, and counted in
    /// [`PolicyStats::captures_dropped`]. A half-surviving capture would report
    /// a span the researcher never wrote.
    pub components: Box<[Capture]>,
    /// Per-token surface overrides from `substitute`, consumed by `join`.
    ///
    /// Indices are sentence-absolute — the same coordinate space as spans and
    /// captures — and therefore governed by the same rule stated on
    /// `components`: an override whose token a later span rewrite removed is
    /// dropped, never retargeted. Ops are ordered and unrestricted, so
    /// `[substitute, drop_edge]` is authorable and this case is reachable.
    pub overrides: Vec<(usize, String)>,
}

impl PublishedRecord {
    /// The published span and name as a plain [`Match`].
    ///
    /// This is the complete projection returned by [`apply_policy`].
    #[must_use]
    pub fn into_match(self) -> Match {
        Match {
            pattern: self.name,
            start: self.start,
            end: self.end,
            captures: self.components,
            graph_bindings: self.raw.graph_bindings,
            lexical: self.raw.lexical,
        }
    }
}

/// One record as a publishing sink sees it, whichever mode produced it.
///
/// A sink that publishes derived fields needs one emission path, not two: the
/// raw stream and the policy-applied stream differ only in whether the derived
/// fields are there to publish. Borrowing rather than owning is what keeps the
/// raw path free — building a [`PublishedRecord`] per canonical match so that
/// one code path could serve both would clone a capture box for every record
/// that derived nothing, which is most of them on most catalogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordView<'a> {
    /// Published pattern name — `output_name` already applied.
    pub pattern: &'a str,
    /// The name the researcher wrote, before `output_name`.
    ///
    /// Every policy lookup keys on this. `output_name` runs last so earlier
    /// passes see the authored name, and a consumer
    /// that looks a record's own policy up by `pattern` finds nothing on
    /// exactly the definitions that renamed themselves.
    pub definition: &'a str,
    /// Published span start.
    pub start: usize,
    /// Published span end.
    pub end: usize,
    /// The captures this mode defines: post-rewrite survivors under policy, the
    /// canonical match's own captures raw. See [`PublishedRecord::components`].
    pub captures: &'a [Capture],
    /// Enhanced graph assignments. Span rewrites do not alter graph identity.
    pub graph_bindings: &'a [crate::fsm::GraphBinding],
    /// The rendered unit, present only after a `join` ran.
    pub unit: Option<&'a str>,
    /// The projected key, absent when none was declared and equally
    /// absent when a declared one could not be projected.
    pub key: Option<&'a [ProjectedKeyField]>,
    /// Lexicon-only typed key, separate from the ordinary scalar hot path.
    pub lexical_key: Option<&'a [LexicalProjectedKeyField]>,
    /// Authored lexical identity and the projections that recognized it.
    pub lexical: Option<&'a crate::fsm::LexicalMatch>,
    /// Per-instance annotations, empty when none applied.
    pub tags: &'a [(CompactString, String)],
}

impl<'a> RecordView<'a> {
    /// The view of a canonical match, which derived nothing.
    #[must_use]
    pub fn raw(matched: &'a Match) -> Self {
        Self {
            pattern: matched.pattern.as_str(),
            definition: matched.pattern.as_str(),
            start: matched.start,
            end: matched.end,
            captures: &matched.captures,
            graph_bindings: &matched.graph_bindings,
            unit: None,
            key: None,
            lexical_key: None,
            lexical: matched.lexical.as_deref(),
            tags: &[],
        }
    }

    /// The view of a published record, derived fields included.
    #[must_use]
    pub fn published(record: &'a PublishedRecord) -> Self {
        Self {
            pattern: record.name.as_str(),
            definition: record.raw.pattern.as_str(),
            start: record.start,
            end: record.end,
            captures: &record.components,
            graph_bindings: &record.raw.graph_bindings,
            unit: record.unit.as_deref(),
            key: record.key.as_deref(),
            lexical_key: record.lexical_key.as_deref(),
            lexical: record.raw.lexical.as_deref(),
            tags: &record.tags,
        }
    }
}

/// Drops what a span rewrite removed.
///
/// Called after every narrowing so no operation has to enforce capture and
/// override removal independently. Returns early on a span that did
/// not move, which is the common case.
fn narrow_to_span(
    components: &mut Box<[Capture]>,
    overrides: &mut Vec<(usize, String)>,
    start: usize,
    end: usize,
    stats: &mut PolicyStats,
) {
    let kept: Vec<Capture> = components
        .iter()
        .filter(|capture| capture.start >= start && capture.end <= end)
        .cloned()
        .collect();
    stats.captures_dropped += (components.len() - kept.len()) as u64;
    *components = kept.into_boxed_slice();
    overrides.retain(|(index, _)| (start..end).contains(index));
}

/// Which span one [`TransformStep`] rewrites.
#[derive(Clone, Copy)]
enum TransformTarget {
    /// The published record's own span.
    Record,
    /// One capture's span, by position in `components`.
    Capture(usize),
}

impl TransformTarget {
    fn span(self, record: &PublishedRecord) -> (usize, usize) {
        match self {
            Self::Record => (record.start, record.end),
            Self::Capture(index) => {
                let capture = &record.components[index];
                (capture.start, capture.end)
            }
        }
    }
}

/// Applies a span rewrite computed by `drop_edge` or `truncate_before`.
///
/// The record case narrows the record and drops what fell outside, which is
/// the historical behavior. The capture case narrows only that capture, and
/// discards only the overrides inside the region it removed — a substitution
/// elsewhere in the record belongs to whatever span produced it and is not
/// this step's to delete.
fn narrow(
    target: TransformTarget,
    record: &mut PublishedRecord,
    start: usize,
    end: usize,
    stats: &mut PolicyStats,
) {
    let (previous_start, previous_end) = target.span(record);
    if (start, end) == (previous_start, previous_end) {
        return;
    }
    match target {
        TransformTarget::Record => {
            record.start = start;
            record.end = end;
            narrow_to_span(
                &mut record.components,
                &mut record.overrides,
                start,
                end,
                stats,
            );
        }
        TransformTarget::Capture(index) => {
            record.components[index].start = start;
            record.components[index].end = end;
            record.overrides.retain(|(at, _)| {
                !((previous_start..start).contains(at) || (end..previous_end).contains(at))
            });
        }
    }
}

/// Renders one span, honouring the substitutions recorded for its tokens.
///
/// Shared by `join` and by key projection of the written form, so a
/// substitution reaches the key and the unit through one rule rather than two
/// that can disagree.
fn render<S: TokenSequence + ?Sized>(
    record: &PublishedRecord,
    tokens: &S,
    start: usize,
    end: usize,
) -> String {
    let mut rendered = String::new();
    for index in start..end {
        if let Some((_, substituted)) = record.overrides.iter().find(|(at, _)| *at == index) {
            rendered.push_str(substituted);
        } else if let Some(text) = tokens.scalar_key(index, unit_surface()) {
            rendered.push_str(text);
        }
    }
    rendered
}

/// Runs one record's declared transform list, then keys and tags it.
///
/// **Total by construction.** One ordered pass; each list entry runs
/// exactly once, in declaration order; each guard is evaluated against the
/// record's *current* state — the span as already rewritten by earlier ops, not
/// the raw one. No recursion, no fixpoint, and no op scans more than once:
/// `drop_edge` walks monotonically inward from one edge, `truncate_before` is a
/// single position scan, `substitute` touches one token, `join` renders once.
///
/// Keying and tagging run after the list, because they read final component
/// state. Both are total in the same sense: every failure to project has a
/// defined disposition rather than a placeholder.
pub(crate) fn apply_transforms<S: TokenSequence + ?Sized>(
    policy: &PatternPolicy,
    record: &mut PublishedRecord,
    tokens: &S,
    stats: &mut PolicyStats,
) {
    for step in &policy.transform {
        let target = match &step.scope {
            None => TransformTarget::Record,
            Some(name) => {
                let Some(index) = record
                    .components
                    .iter()
                    .position(|capture| capture.name == *name)
                else {
                    // The capture is in an alternation branch this match did
                    // not take, so the step has no span. Skipping is the only
                    // total answer: rewriting the record instead would make a
                    // scoped step silently unscoped.
                    continue;
                };
                TransformTarget::Capture(index)
            }
        };
        let (span_start, span_end) = target.span(record);
        match &step.op {
            TransformOp::DropEdge {
                leading,
                drop,
                keep,
            } => {
                let (mut start, mut end) = (span_start, span_end);
                while start < end {
                    let index = if *leading { start } else { end - 1 };
                    let Some(token) = SequenceToken::new(tokens, index) else {
                        break;
                    };
                    // `keep` is checked second and wins because it names
                    // exceptions to the drop rule.
                    if !drop.matches(&token)
                        || keep.as_ref().is_some_and(|keep| keep.matches(&token))
                    {
                        break;
                    }
                    if *leading {
                        start += 1;
                    } else {
                        end -= 1;
                    }
                }
                narrow(target, record, start, end, stats);
            }
            TransformOp::TruncateBefore { stop } => {
                let cut = (span_start..span_end).find(|&index| {
                    SequenceToken::new(tokens, index).is_some_and(|token| stop.matches(&token))
                });
                if let Some(cut) = cut {
                    narrow(target, record, span_start, cut, stats);
                }
            }
            TransformOp::Substitute {
                target: edge,
                attr,
                table,
            } => {
                if span_start >= span_end {
                    continue;
                }
                let index = match edge {
                    SubstituteTarget::First => span_start,
                    SubstituteTarget::Last => span_end - 1,
                };
                // A token lacking the attribute is left alone rather than
                // blanked: absence is normal, and writing an empty surface
                // would silently shorten the unit.
                if let Some(value) = tokens.scalar_key(index, attr) {
                    let substituted = table
                        .iter()
                        .find(|(from, _)| from == value)
                        .map_or_else(|| value.to_string(), |(_, to)| to.clone());
                    record.overrides.retain(|(at, _)| *at != index);
                    record.overrides.push((index, substituted));
                }
            }
            TransformOp::Join => {
                record.unit = Some(render(record, tokens, span_start, span_end));
            }
        }
    }

    if !policy.key.is_empty() {
        if record.raw.lexical.is_some() {
            record.lexical_key = project_lexical_key_fields(&policy.key, &record.raw, tokens);
        } else {
            record.key = project_key_fields(
                &policy.key,
                record.start..record.end,
                &record.components,
                &record.raw.graph_bindings,
                &record.overrides,
                tokens,
            );
        }
        if record.key.is_none() && record.lexical_key.is_none() {
            stats.records_unkeyed += 1;
        }
    }

    for (name, source) in &policy.tags {
        if let Some(value) = project_tag(source, &record.components, tokens) {
            record.tags.push((name.clone(), value));
        }
    }
}

/// The attribute `join` renders a token as when nothing substituted it.
///
/// The written form, not a base form: `join` reconstructs a unit from the
/// sentence, and any normalization is a `substitute` the author asked for.
pub(crate) fn unit_surface() -> &'static AttributeKey {
    static FORM: std::sync::LazyLock<AttributeKey> =
        std::sync::LazyLock::new(|| AttributeKey::new("text").expect("form is canonical"));
    &FORM
}

/// One tag's value, or `None` when its source is absent.
///
/// An absent source yields an absent tag rather than an empty one. Tags are
/// per-instance annotations, so "this record has no such annotation" is a
/// meaningful state, where an empty string is a value that would compare equal
/// across unrelated records.
fn project_tag<S: TokenSequence + ?Sized>(
    source: &TagSource,
    components: &[Capture],
    tokens: &S,
) -> Option<String> {
    match source {
        TagSource::Static(value) => Some(value.clone()),
        TagSource::CapturePresence(mapping) => {
            // Exactly one participating capture yields a tag. Zero is the
            // absent case. Two or more is *ambiguous*, and resolving it by
            // declaration order would be inventing a winner out of what is
            // really an authoring mistake — under a map-valued metadata key,
            // "declaration order" is not even stable across builds. Both
            // non-answers get the same disposition: no tag.
            let mut present = mapping
                .iter()
                .filter(|(capture, _)| components.iter().any(|found| found.name == *capture));
            let (_, value) = present.next()?;
            present.next().is_none().then(|| value.clone())
        }
        TagSource::Projection { capture, attr } => {
            // `found.start` is the capture's only token: `capture_key` refuses
            // projections from captures it cannot prove are exactly one token.
            let found = components.iter().find(|found| found.name == *capture)?;
            Some(tokens.scalar_key(found.start, attr)?.to_string())
        }
    }
}

/// Applies one catalog's declared policy to one sentence's canonical matches.
///
/// `matches` is the canonical `(start, end, pattern)`-ordered stream for a
/// single sentence and `tokens` is that sentence, whose length is the sentence
/// length. The returned records are
/// the published ones, **in the input's relative order**. Transformed spans
/// never participate in ordering, so a trim that makes two spans equal
/// cannot reorder them.
///
/// The evaluation order is fixed and observable:
/// synthesized evidence is stripped, then the policy program decides which
/// records survive and where they end, then span shaping, then relabeling. The
/// program's relations are evaluated on *raw* spans, which is why shaping comes
/// after it — narrowing first would change which records overlap.
///
/// Memory is bounded by the sentence: index vectors and one scratch list per
/// statement, nothing proportional to the corpus.
///
/// This is the published-span-and-name projection of [`apply_policy_records`],
/// which is the single implementation. A caller that needs the unit, key or
/// tags a transform produced wants that function instead.
///
/// Per-operation [`ProgramStats`] are discarded here and no explanation trace
/// is collected, and there is no parameter for either: this projection answers
/// "what does the catalog publish", and a caller asking which statement did the
/// work — or why one record among them looks as it does — is already past what
/// a span and a name can say. [`apply_policy_records`] takes both.
///
/// # Errors
///
/// [`AnchorResolutionError`] when an `admit` names an anchor `anchors` cannot
/// answer for. See [`apply_policy_records`].
pub fn apply_policy<S: TokenSequence + ?Sized>(
    policy: &CatalogPolicy,
    matches: &[Match],
    tokens: &S,
    anchors: &AnchorPositions,
    stats: &mut PolicyStats,
) -> Result<Vec<Match>, AnchorResolutionError> {
    let mut program = ProgramStats::for_program(policy.program());
    Ok(
        apply_policy_records(policy, matches, tokens, anchors, stats, &mut program, None)?
            .into_iter()
            .map(PublishedRecord::into_match)
            .collect(),
    )
}

/// Applies one catalog's declared policy, returning complete published records.
///
/// Same evaluation order and same guarantees as [`apply_policy`]; this returns
/// derived fields — the rendered unit, projected key, tags, and
/// components that survived span rewrites — rather than only the span and name.
///
/// `tokens` is the sentence, and its length is the sentence length: taking the
/// slice rather than a separate count removes a parameter the two could
/// disagree on, and every call site already had it.
///
/// `anchors` is this sentence's derived anchor positions, from
/// [`crate::fsm::CompiledCatalog::anchor_positions`] of the catalog whose policy
/// this is. It is mandatory rather than optional because an optional one would
/// have a `None` case, and the only thing that case could mean is "measure
/// against the raw sentence end", which would silently change admission.
///
/// # Errors
///
/// [`AnchorResolutionError`] when an `admit` names an anchor `anchors` cannot
/// answer for, either because the positions were computed by a
/// structurally different catalog or because no anchor of that name is
/// declared. Nothing in `stats` or `program` moves in that case: every
/// referenced anchor is resolved before any record is judged.
pub fn apply_policy_records<S: TokenSequence + ?Sized>(
    policy: &CatalogPolicy,
    matches: &[Match],
    tokens: &S,
    anchors: &AnchorPositions,
    stats: &mut PolicyStats,
    program: &mut ProgramStats,
    trace: Option<&mut crate::fsm::policy_trace::PolicyTrace>,
) -> Result<Vec<PublishedRecord>, AnchorResolutionError> {
    let sentence_len = tokens.len();
    // Hidden records are evidence for the program below and are never
    // published, so they leave the candidate set immediately while remaining
    // visible in `matches` for a statement's operand lookups.
    let surviving: Vec<usize> = (0..matches.len())
        .filter(|&index| !is_hidden(&matches[index].pattern))
        .collect();

    // Anchor resolution for the program's `admit` statements.
    //
    // Every referenced anchor is resolved before any record is judged, and this
    // is the only fallible part of publication: once `resolved` is built,
    // nothing below can fail. Doing it here rather than lazily means a
    // statistic can never record work from a computation that then failed, so
    // the caller cannot mistake a partial accounting for a completed run's.
    //
    // An absent anchor, or positions from a structurally different catalog, is
    // an error rather than a fallback. Silently measuring against the raw
    // sentence end would publish a plausible but incorrect smaller result.
    //
    // Skipped wholesale for a catalog whose program names no anchor, which is
    // every catalog that declares no `admit`. The loop would otherwise visit
    // every statement to `continue` on each, once per sentence.
    //
    // This removes work that cannot affect the answer; it is not a measured
    // speedup claim. See `docs/results/2026-08-04-anchor-performance.md`.
    let mut resolved: BTreeMap<&str, usize> = BTreeMap::new();
    if policy.program_names_an_anchor() {
        for op in &policy.program().ops {
            let crate::fsm::policy_program::BoundOp::Admit { anchor, .. } = op else {
                continue;
            };
            // The reserved anchor is the token count, so it resolves without
            // consulting `anchors` at all. That is not an optimization: it is
            // what lets an `admit` measure against the raw end in a catalog
            // that declares no anchor, where `AnchorPositions` carries the
            // empty-catalog digest and `get` would reject every lookup.
            if anchor == SENTENCE_END_ANCHOR {
                resolved.insert(SENTENCE_END_ANCHOR, sentence_len);
                continue;
            }
            resolve_anchor(&mut resolved, anchors, policy, anchor)?;
        }
    }

    // The ordered policy program. It is the whole of suppression and
    // admission now that the key-driven passes have retired: what used to be
    // passes 2 to 4 ran here, before shaping, and the program runs in their
    // place. `crate::fsm::policy_engine` documents why *before* shaping: the
    // program keys on the authored name, which the relabeling pass rewrites.
    //
    // Skipping catalogs with no block makes the mechanism inert for them:
    // nothing below this guard runs.
    let (surviving, narrowed) = if policy.program().ops.is_empty() {
        (surviving, BTreeMap::new())
    } else {
        run_policy_program(policy, matches, &resolved, surviving, program, trace)
    };

    // Shaping, then transforms/keying/tagging, then relabeling. All are
    // per-record, so they run in one walk over the survivors, which are already
    // in input order.
    //
    // Relabeling is last and that matters: everything earlier looks the
    // record's policy up by its *own* name, so renaming first would make a
    // definition publishing under another's name inherit that one's policy.
    Ok(surviving
        .into_iter()
        .map(|index| {
            let raw = matches[index].clone();
            // The program is the only thing that narrows a span before
            // publication: a `detach` sets this record's public span, and a
            // record it did not reshape publishes the raw one.
            let (start, end) = narrowed
                .get(&index)
                .copied()
                .unwrap_or((raw.start, raw.end));
            // A capture the detached region touches is **dropped, never
            // clamped**: the region belongs to another record, so a
            // capture reaching into it no longer covers what it named.
            //
            // Apply this only when the program narrowed the record. Otherwise
            // the matcher's invariant already places every capture inside the
            // raw bounds.
            let components = if narrowed.contains_key(&index) {
                raw.captures
                    .iter()
                    .filter(|capture| start <= capture.start && capture.end <= end)
                    .cloned()
                    .collect()
            } else {
                raw.captures.clone()
            };
            let mut record = PublishedRecord {
                start,
                end,
                name: raw.pattern.clone(),
                components,
                unit: None,
                key: None,
                lexical_key: None,
                tags: Vec::new(),
                overrides: Vec::new(),
                raw,
            };
            if let Some(entry) = policy.get(&record.name) {
                apply_transforms(entry, &mut record, tokens, stats);
                if let Some(published) = &entry.output_name {
                    record.name = published.clone();
                }
            }
            record
        })
        .collect())
}

/// Runs the catalog's policy program over the records that are not evidence.
///
/// Returns the surviving raw indices, ascending, and the public span of each
/// record a `detach` narrowed. A record the program did not reshape is absent
/// from the map, so an empty one means the shaping walk reads raw spans as
/// they did before programs existed.
///
/// Assembling the [`ExecContext`] belongs here rather than in the engine.
/// Every field comes from a different part of this policy, and composing the
/// definition domain with the `__policy__` alias table is a fact about how a
/// catalog stores its definitions — not about how a decision is made.
///
/// `anchors` is every anchor position this sentence resolved, which the caller
/// read **before** judging any record. Passing the resolved map rather than the
/// [`AnchorPositions`] is what keeps that promise a property of one place: an
/// `admit` here cannot ask a question the caller has not already answered, so
/// the engine has no failure of its own to report.
///
/// [`ExecContext`]: crate::fsm::policy_engine::ExecContext
fn run_policy_program(
    policy: &CatalogPolicy,
    matches: &[Match],
    anchors: &BTreeMap<&str, usize>,
    surviving: Vec<usize>,
    stats: &mut ProgramStats,
    trace: Option<&mut crate::fsm::policy_trace::PolicyTrace>,
) -> (Vec<usize>, BTreeMap<usize, (usize, usize)>) {
    use crate::fsm::policy_engine::{Candidate, ExecContext, definition_of};
    use crate::fsm::policy_program::{DefId, RawId};

    let domain = policy.definitions();
    let resolve = |name: &str| definition_of(name, domain, &policy.hidden);
    let priority_of = |id: DefId| policy.get(domain.name(id)).map_or(0, PatternPolicy::order);
    let context = ExecContext {
        groups: policy.group_index(),
        definition_of: &resolve,
        is_pattern: &|id| domain.is_pattern(id),
        priority_of: &priority_of,
        anchors,
    };

    // A record whose name this catalog does not declare is carried through
    // untouched. No subject can select it and no operand can match it, so the
    // program has nothing to say about it — and every other pass here already
    // answers an unknown name that way, by finding no entry and keeping the
    // record.
    //
    // Unreachable for a caller pairing a policy with its own matcher's output,
    // which is every caller in this workspace. Reachable for one that does
    // not, because this function is public and nothing in its signature pairs
    // them. Refusing to run at all is the one answer no other pass gives.
    let mut candidates = Vec::with_capacity(surviving.len());
    let mut foreign = Vec::new();
    for &index in &surviving {
        match resolve(&matches[index].pattern) {
            Some(definition) => candidates.push(Candidate {
                raw: RawId(index),
                definition,
                public_start: matches[index].start,
                public_end: matches[index].end,
            }),
            None => foreign.push(index),
        }
    }

    let survivors = crate::fsm::policy_engine::run_program(
        &policy.program().ops,
        matches,
        candidates,
        &context,
        stats,
        trace,
    );

    let mut narrowed = BTreeMap::new();
    let mut kept: Vec<usize> = Vec::with_capacity(survivors.len() + foreign.len());
    for candidate in &survivors {
        let index = candidate.raw.index();
        kept.push(index);
        let raw = &matches[index];
        if (candidate.public_start, candidate.public_end) != (raw.start, raw.end) {
            narrowed.insert(index, (candidate.public_start, candidate.public_end));
        }
    }
    // Guarded, because re-sorting an already-ascending list on every sentence
    // would be paid by every catalog to serve a case that cannot arise for any
    // of them.
    if !foreign.is_empty() {
        kept.extend(foreign);
        kept.sort_unstable();
    }
    (kept, narrowed)
}

/// Reads one anchor's position into `resolved`, once per name.
///
/// A free function rather than a closure over `resolved`, because the caller
/// hands it a name borrowed from `policy` and a closure would have to be
/// higher-ranked over that lifetime to insert it.
fn resolve_anchor<'policy>(
    resolved: &mut BTreeMap<&'policy str, usize>,
    anchors: &AnchorPositions,
    policy: &CatalogPolicy,
    name: &'policy str,
) -> Result<(), AnchorResolutionError> {
    if resolved.contains_key(name) {
        return Ok(());
    }
    let position =
        anchors
            .get(name, &policy.anchor_digest)
            .map_err(|cause| AnchorResolutionError {
                anchor: CompactString::new(name),
                cause,
            })?;
    resolved.insert(name, position);
    Ok(())
}

/// Whether `name` can be written as a `%name` group reference.
///
/// A group that cannot be referenced can be declared and never selected — an
/// inert declaration that still looks executable, which is the failure the
/// whole policy tier exists to close. Validating at the declaration is the only
/// place the author is looking; by the time a `policy` block fails to parse,
/// the error points at the statement rather than at the `groups:` key that made
/// it unwritable.
///
/// The rule is the grammar's identifier, **derived by probing the pinned
/// revision** rather than copied from it: this workspace does not vendor the
/// grammar, so the alternative was to invent a second rule and hope. Measured
/// against `a4f113f7`, `g`, `g1`, `_g`, `aB_9` and `ようだ` reach reference
/// resolution, while `1g`, `a-b`, `a b`, `a.b` and `%x` are syntax errors.
/// Non-ASCII is admitted because definition names here are routinely Japanese
/// and a group has no reason to be stricter.
///
/// If the grammar and this disagree, **this is the one that is wrong**: the
/// grammar decides what parses, and this only decides what is reported early.
pub(crate) fn is_group_reference_spelling(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_')
        && chars.all(|character| character.is_alphanumeric() || character == '_')
}

/// Whether `a` and `b` differ by at most one insertion, deletion, or
/// substitution.
///
/// This is the typo detector's complete measure. A full Levenshtein matrix
/// would compute a distance nothing reads: the question is only ever "is this
/// unknown key one edit from a key I recognize?", which a single walk answers
/// in linear time and constant space.
///
/// Comparison is over `char`s, not bytes, so a non-ASCII key is measured in
/// the units a researcher actually typed.
#[must_use]
pub(crate) fn levenshtein_at_most_one(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (long, short) = if a.len() >= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if long.len() - short.len() > 1 {
        return false;
    }

    let mut long_index = 0;
    let mut short_index = 0;
    let mut edits = 0usize;
    while long_index < long.len() && short_index < short.len() {
        if long[long_index] == short[short_index] {
            long_index += 1;
            short_index += 1;
            continue;
        }
        edits += 1;
        // Equal lengths can only be a substitution; unequal lengths can only
        // be a deletion from the longer side, because their difference is 1.
        long_index += 1;
        if long.len() == short.len() {
            short_index += 1;
        }
    }
    // One guard, deliberately. An early `if edits > 1 { return false }` is a
    // pure optimization on keys this short, and having both meant neither
    // could be broken alone — a mutation of either still passed, because the
    // other caught it. Whatever remains unconsumed is an insertion or
    // deletion, so it counts toward the same budget.
    edits + (long.len() - long_index) + (short.len() - short_index) <= 1
}

/// The recognized key `key` is at most one edit from, if any.
///
/// The catalog-level key naming the dictionary the catalog's tagset belongs to.
///
/// Deliberately outside [`POLICY_KEYS`]: it is not a per-pattern publication
/// decision, and it must not draw `IneffectivePolicy` on a fragment — a
/// fragment block is the *recommended* place to declare it, since it is a
/// property of the whole catalog and fragments are where shared definitions
/// already live.
pub const DICTIONARY_KEY: &str = "dictionary";

/// The catalog-level key declaring a derived sentence anchor.
///
/// Deliberately outside [`POLICY_KEYS`], exactly as [`DICTIONARY_KEY`] is: it is
/// not a per-pattern publication decision, and it must not draw
/// `IneffectivePolicy` on the fragment block where it naturally lives, beside
/// the `exclude` fragment it names.
pub const ANCHOR_KEY: &str = "anchor";

/// The compiler-owned anchor naming the raw sentence end.
///
/// Every other anchor is *derived*: `propositional_end` is the last
/// non-apparatus token, computed per sentence from a declared `exclude`. This
/// one is the token count, so it needs no declaration, consults no
/// [`AnchorPositions`], and cannot fail to resolve.
///
/// It exists because the retired scalar `terminal: true` and `terminal: <n>`
/// measured against exactly this, and `admit` must be able to say what they
/// said. Spelling it as a name rather than as an omitted `anchor:` keeps every
/// `admit` naming the boundary it measures against. An implicit boundary is
/// dangerous because nothing
/// on the line says which one was used.
///
/// Declaring an anchor by this name is an error rather than a shadow: under
/// shadowing, which meaning a statement got would depend on declaration order,
/// which is correct in the compiler and unlearnable at the desk. See
/// `docs/design/2026-08-06-raw-sentence-end-admission.md`.
pub const SENTENCE_END_ANCHOR: &str = "sentence_end";

/// The key carrying an `exclude` fragment's [`NestingClaim`].
///
/// Outside [`POLICY_KEYS`] for the same reason [`ANCHOR_KEY`] is: it is not a
/// per-pattern publication decision, and it lives on the fragment block beside
/// the `anchor:` declaration that names it.
pub const MAX_NESTING_KEY: &str = "max_nesting";

/// Whether the compiler, rather than the catalog, owns this key's schema.
///
/// Deliberately not the complement of the typo detector's non-policy set: that
/// answers which spellings the detector must leave alone, and so groups
/// `examples`/`counterexamples` — authored content that must stay declarable —
/// with the compiler-owned three.
///
/// Two callers, and they read it for different reasons. The schema checker
/// refuses a declaration of one of these keys, because the compiler already
/// defines it. `lingrep-web`'s pattern browser withholds them from its grouping
/// heuristic, because a key the compiler owns describes how a record is
/// published rather than what a researcher wanted to ask — and the heuristic,
/// which only asks "does this partition the catalog usefully", cannot tell the
/// difference on its own. `priority_order` is the case that forced it: it
/// partitions `sf-experimental.lx` exactly as `layer` does, because a
/// `resolve` statement needed a total order over the same layers.
#[must_use]
pub fn compiler_owned_key(key: &str) -> bool {
    POLICY_KEYS.contains(&key) || matches!(key, DICTIONARY_KEY | ANCHOR_KEY | MAX_NESTING_KEY)
}

/// Returns the first match in [`POLICY_KEYS`] order, which is stable, so a key
/// equidistant from two recognized keys always reports the same suggestion.
///
/// [`DICTIONARY_KEY`], [`ANCHOR_KEY`] and [`MAX_NESTING_KEY`] are searched too:
/// all three are engine-recognized, so a misspelling of any is inert, and inert
/// and honored look identical from the catalog. They are checked last so none
/// can displace a policy-key suggestion.
#[must_use]
pub(crate) fn suggest_policy_key(key: &str) -> Option<&'static str> {
    POLICY_KEYS
        .iter()
        .copied()
        .chain([DICTIONARY_KEY, ANCHOR_KEY, MAX_NESTING_KEY])
        .find(|candidate| levenshtein_at_most_one(key, candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_edit_covers_substitution_insertion_and_deletion() {
        assert!(levenshtein_at_most_one("terminal", "terminal"));
        assert!(levenshtein_at_most_one("terminal", "termanal")); // substitution
        assert!(levenshtein_at_most_one("terminal", "termminal")); // insertion
        assert!(levenshtein_at_most_one("terminal", "termnal")); // deletion
    }

    /// Textbook Levenshtein. Exists only to cross-check the one-edit walk —
    /// nothing in the engine computes a distance.
    fn reference_distance(a: &str, b: &str) -> usize {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        let mut previous: Vec<usize> = (0..=b.len()).collect();
        let mut current = vec![0usize; b.len() + 1];
        for (i, left) in a.iter().enumerate() {
            current[0] = i + 1;
            for (j, right) in b.iter().enumerate() {
                let substitution = previous[j] + usize::from(left != right);
                current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
            }
            std::mem::swap(&mut previous, &mut current);
        }
        previous[b.len()]
    }

    #[test]
    fn the_one_edit_walk_agrees_with_full_levenshtein_exhaustively() {
        // The walk is a hand-rolled shortcut around a well-known algorithm, so
        // it is checked against that algorithm rather than against a handful of
        // examples the author happened to think of. Every string over {a,b,c}
        // up to length 4, both sides: 121 x 121 pairs.
        let mut words = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..4 {
            let mut next = Vec::new();
            for word in &frontier {
                for letter in ['a', 'b', 'c'] {
                    let mut grown = word.clone();
                    grown.push(letter);
                    next.push(grown);
                }
            }
            words.extend(next.iter().cloned());
            frontier = next;
        }
        assert_eq!(words.len(), 121);

        for left in &words {
            for right in &words {
                assert_eq!(
                    levenshtein_at_most_one(left, right),
                    reference_distance(left, right) <= 1,
                    "disagreed on {left:?} vs {right:?} (distance {})",
                    reference_distance(left, right)
                );
            }
        }
    }

    #[test]
    fn two_edits_are_not_one_edit() {
        assert!(!levenshtein_at_most_one("terminal", "termanl"));
        assert!(!levenshtein_at_most_one("terminal", "terminal_only"));
        assert!(!levenshtein_at_most_one("output_name", "mapping_note"));
    }

    #[test]
    fn distance_is_measured_in_characters_not_bytes() {
        // A one-character substitution in a multi-byte key is one edit, not
        // three. Metadata keys are researcher-authored and need not be ASCII.
        assert!(levenshtein_at_most_one("出力名", "出力語"));
        assert!(!levenshtein_at_most_one("出力名", "名前語"));
    }

    #[test]
    fn the_real_catalog_keys_are_not_mistaken_for_typos() {
        // Non-policy metadata keys researchers have actually written in the
        // tracked catalogs. A false positive here would put a warning on
        // correct declarations, which is how a lint gets switched off
        // wholesale.
        //
        // Four of these no longer appear in any catalog:
        // `split_trailing_copula` and `terminal_only` were replaced by policy
        // statements, and `s0_always_keep` and `s0_requires_upper_layer` by
        // `%s0_conditional` membership. They stay in the list because the
        // question it asks is about the *shape* of a researcher-authored key,
        // and a key that was plausible enough to be written here once is
        // plausible enough to be written again.
        for key in [
            "status",
            "source",
            "role",
            "category",
            "examples",
            "layer",
            "split_trailing_copula",
            "subcategory",
            "type",
            "meidai",
            "s0_requires_upper_layer",
            "terminal_only",
            "s0_always_keep",
            "mapping_note",
            "counterexamples",
        ] {
            assert_eq!(
                suggest_policy_key(key),
                None,
                "`{key}` is a real catalog key and must not be reported as a typo"
            );
        }
    }

    #[test]
    fn a_near_miss_of_a_recognized_key_is_suggested() {
        assert_eq!(suggest_policy_key("output_namme"), Some("output_name"));
        assert_eq!(suggest_policy_key("priority_ordr"), Some("priority_order"));
    }

    #[test]
    fn terminal_admits_by_distance_from_the_anchor() {
        assert!(Terminal::Exact.admits(5, 5));
        assert!(!Terminal::Exact.admits(4, 5));
        assert!(Terminal::Within(0).admits(5, 5));
        assert!(!Terminal::Within(0).admits(4, 5)); // `Within(0)` is `Exact`
        assert!(Terminal::Within(1).admits(4, 5));
        assert!(!Terminal::Within(1).admits(3, 5));
    }

    /// `admits` is one-sided (`policy-sublanguage.md:248`).
    ///
    /// Material after the propositional end is citation apparatus, so a record
    /// ending inside it is not sentence-final at any distance. The previously
    /// shipped `saturating_sub` form clamped instead, making `Exact` true for
    /// every record ending past the boundary — the defect that document
    /// pre-registers.
    #[test]
    fn admits_refuses_a_record_ending_past_the_anchor() {
        assert!(!Terminal::Exact.admits(5, 3));
        assert!(!Terminal::Within(0).admits(5, 3));
        assert!(!Terminal::Within(2).admits(5, 3));
        assert!(!Terminal::Within(64).admits(5, 3));
    }

    proptest::proptest! {
        /// One-sided and clamped admission agree for every `end <= sentence_len`.
        ///
        /// What makes the one-sided rewrite behaviour-preserving for every
        /// unanchored declaration in every shipped catalog, rather than only on
        /// the pinned examples above: against a raw sentence length a match
        /// never exceeds its own sentence, so `end <= sentence_len` always
        /// holds and the previous `saturating_sub` never saturated.
        ///
        /// The slack is generated rather than filtered for. Conditioning two
        /// independent uniforms on `end <= sentence_len` is uniform over the
        /// triangle, which puts almost no weight on the boundary and the narrow
        /// `0..allowed` band — the only places `Within` decides anything.
        #[test]
        fn one_sided_and_clamped_admission_agree_below_the_raw_end(
            end in 0usize..64,
            slack in 0usize..8,
            allowed in 0u32..16,
        ) {
            let sentence_len = end + slack;
            let clamped_exact = sentence_len.saturating_sub(end) == 0;
            let clamped_within = sentence_len.saturating_sub(end) <= allowed as usize;
            proptest::prop_assert_eq!(Terminal::Exact.admits(end, sentence_len), clamped_exact);
            proptest::prop_assert_eq!(
                Terminal::Within(allowed).admits(end, sentence_len),
                clamped_within
            );
        }
    }

    #[test]
    fn an_empty_policy_is_recognizable_as_empty() {
        assert!(PatternPolicy::default().is_empty());
        assert!(
            !PatternPolicy {
                priority_order: Some(1),
                ..PatternPolicy::default()
            }
            .is_empty()
        );
        assert_eq!(PatternPolicy::default().order(), 0);
    }

    #[test]
    fn a_catalog_with_no_entries_declares_nothing() {
        assert!(!CatalogPolicy::default().declared());
    }

    /// `modify_entry` reports whether it found anything, and edits nothing when
    /// it did not.
    ///
    /// The return value is the whole contract: a caller that ignored it would
    /// silently accept an edit naming a definition the catalog does not declare.
    ///
    /// Every edit here goes through `into_builder`, because that is the only
    /// route a built policy has back to its entries.
    #[test]
    fn editing_an_absent_entry_reports_it_and_changes_nothing() {
        let mut builder = CatalogPolicyBuilder::default();
        builder.insert_entry(
            CompactString::new("plain"),
            PatternPolicy {
                priority_order: Some(1),
                ..PatternPolicy::default()
            },
        );
        let policy = builder.build();

        let mut builder = policy.into_builder();
        assert!(builder.modify_entry("plain", |entry| entry.priority_order = Some(2)));
        let policy = builder.build();
        assert_eq!(
            policy.get("plain").and_then(|entry| entry.priority_order),
            Some(2)
        );

        let mut builder = policy.into_builder();
        let mut edited = false;
        assert!(!builder.modify_entry("absent", |_| edited = true));
        assert!(!edited, "an absent entry leaves the edit unrun");
    }

    /// `into_builder` carries every field it is not there to change.
    ///
    /// The digest most of all: a rebuild that dropped it would leave a policy
    /// whose `admit` measures against an anchor set it no longer names, and
    /// the `AnchorPositions` check would report a mismatch far from the edit.
    #[test]
    fn reopening_a_policy_preserves_what_it_does_not_edit() {
        let mut builder = CatalogPolicyBuilder::default();
        builder.insert_entry(CompactString::new("p"), PatternPolicy::default());
        builder
            .anchors
            .insert(CompactString::new("end"), CompactString::new("TRAILING"));
        builder
            .hidden
            .insert(CompactString::new("__policy__f"), CompactString::new("f"));
        builder.nesting_claims.insert(
            CompactString::new("end"),
            NestingClaim {
                depth: 0,
                witness: vec![NestingEntry::parse("あ|。").expect("one sentinel")],
                divergence: vec![NestingEntry::parse("あ|（い）。").expect("one sentinel")],
            },
        );
        let mut policy = builder.build();
        policy.anchor_digest = AnchorSetDigest::from_bytes([7; 32]);

        assert_eq!(policy.clone().into_builder().build(), policy);
    }
}
