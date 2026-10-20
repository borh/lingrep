use crate::token_match::{AttributeKey, TokenAttributeIndex};
use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::hash::Hash;
use std::sync::Arc;

use crate::corpus::conllu::EnhancedNodeId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct FinalState {
    pub state_id: usize,
    pub pattern_name: CompactString,
}

#[derive(Debug, Clone, Copy)]
pub struct StateInfo {
    pub state: usize,          // Current state in the state machine
    pub start_token: usize,    // Index of the first token in the sequence that led to this state
    pub position: usize,       // Current position in token stream (tokens consumed total)
    pub entry_position: usize, // Position when we entered this state (for counting transition tokens)
}

impl Hash for StateInfo {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.state.hash(state);
        self.start_token.hash(state);
        self.position.hash(state);
        self.entry_position.hash(state);
    }
}

impl PartialEq for StateInfo {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
            && self.start_token == other.start_token
            && self.position == other.position
            && self.entry_position == other.entry_position
    }
}

impl Eq for StateInfo {}

impl PartialOrd for StateInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StateInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.state
            .cmp(&other.state)
            .then_with(|| self.start_token.cmp(&other.start_token))
            .then_with(|| self.position.cmp(&other.position))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Reject,
    Match,
    Advance,
    Retry,
    MatchExtend,
    RetryAdvance,
    RetryExtend,
    MatchReject,
    MatchDouble,
}

/// A pattern match found in a token sequence
///
/// # End Position Convention
///
/// **IMPORTANT:** `end` uses **exclusive** positioning (Rust range convention):
/// - A match covering tokens at positions 0, 1, 2 has `start=0, end=3`
/// - Length calculation: `end - start` gives token count
/// - Slicing: `tokens[start..end]` works naturally
///
/// This matches the compiled NFA matcher's convention and Rust's range syntax
/// (`0..n`).
/// One observable named capture within a successful lingrep patterns match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct Capture {
    /// Authored capture name.
    pub name: CompactString,
    /// Hygienic display path, including fragment expansion occurrences.
    pub path: CompactString,
    /// Inclusive token start.
    pub start: usize,
    /// Exclusive token end.
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Match {
    /// Name of the pattern that matched
    pub pattern: CompactString,
    /// Start position of match (inclusive, first token index)
    pub start: usize,
    /// End position of match (exclusive, one past last token)
    pub end: usize,
    /// Canonically ordered named subspans participating in this match.
    pub captures: Box<[Capture]>,
    /// Enhanced graph nodes and edges bound by this assignment, in canonical
    /// binding-kind/name order. Empty for ordinary sequence-only matches.
    pub graph_bindings: Box<[GraphBinding]>,
    /// Authored lexical identity when this record came from an inline lexicon.
    pub lexical: Option<Box<LexicalMatch>>,
}

/// One scalar value carried by a bound enhanced graph node.
///
/// The complete CoNLL-U scalar projection is retained so `key:` can read an
/// empty node after matching without keeping the sentence graph alive through
/// publication. String-set attributes remain predicate-only in this slice.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct GraphNodeAttribute {
    pub name: AttributeKey,
    pub value: CompactString,
}

/// Observable identity introduced by an enhanced `where` constraint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum GraphBinding {
    Node {
        name: CompactString,
        node: EnhancedNodeId,
        attributes: Box<[GraphNodeAttribute]>,
    },
    Edge {
        name: CompactString,
        head: EnhancedNodeId,
        dependent: EnhancedNodeId,
        relation: CompactString,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnhancedGraphExecutionError {
    pub expected_words: usize,
    pub actual_words: usize,
}

impl fmt::Display for EnhancedGraphExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "enhanced graph has {} words, but the token sequence has {}",
            self.actual_words, self.expected_words
        )
    }
}

impl std::error::Error for EnhancedGraphExecutionError {}

impl GraphBinding {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Node { name, .. } | Self::Edge { name, .. } => name,
        }
    }

    #[must_use]
    pub fn scalar(&self, attribute: &AttributeKey) -> Option<&str> {
        match self {
            Self::Node { attributes, .. } => attributes
                .iter()
                .find(|value| value.name == *attribute)
                .map(|value| value.value.as_str()),
            Self::Edge { relation, .. } if attribute.as_str() == "dep.rel" => Some(relation),
            Self::Edge { .. } => None,
        }
    }
}

/// Lexicon-specific identity attached only to lexical occurrences.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct LexicalMatch {
    pub term: CompactString,
    pub matched_by: Box<[AttributeKey]>,
}

/// A membership list is `Arc<[CompactString]>` rather than `Vec`, because a
/// named `set` is resolved into every site that references it. With `Vec`,
/// M reference sites to an N-member set materialized M·N strings from
/// O(N + M) bytes of source — so a catalog well inside `max_source_bytes`
/// could allocate past the generated-owned-bytes budget before that budget
/// is charged, which happens after binding. Sharing makes resolution a
/// refcount bump, so materialized size tracks source size the way every
/// other budget assumes. Matching is unaffected: `Arc<[T]>` derefs to the
/// same slice the scan already walked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttrValueType {
    Single(CompactString),
    OneOf(Arc<[CompactString]>),
    NotIn(Arc<[CompactString]>),
    /// Membership in a schema-declared string-set value.
    SetHas(CompactString),
    /// Nonempty intersection with a schema-declared string-set value.
    SetHasAny(Arc<[CompactString]>),
    /// Present-set membership negation.
    SetNotHas(CompactString),
    /// Present-set intersection negation.
    SetNotHasAny(Arc<[CompactString]>),
    /// Substring containment against a literal needle (`~ ない`).
    Contains(CompactString),
    /// Negated substring containment (`!~ ない`).
    NotContains(CompactString),
    /// A regex predicate. Supply a plain `Regex::new` value: `CompiledCatalog`
    /// evaluates an ID-indexed per-thread `Regex::new(as_str())` clone and has
    /// no fallback that preserves non-default `RegexBuilder` options.
    #[serde(with = "serde_regex")]
    Regex(regex::Regex),
    /// Negated regex search (`!~ /…/`).
    #[serde(with = "serde_regex")]
    NotRegex(regex::Regex),
}

// Manual PartialEq implementation that compares regex patterns as strings
impl PartialEq for AttrValueType {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Single(l0), Self::Single(r0)) => l0 == r0,
            (Self::OneOf(l0), Self::OneOf(r0)) => l0 == r0,
            (Self::NotIn(l0), Self::NotIn(r0)) => l0 == r0,
            (Self::SetHas(l0), Self::SetHas(r0)) => l0 == r0,
            (Self::SetHasAny(l0), Self::SetHasAny(r0)) => l0 == r0,
            (Self::SetNotHas(l0), Self::SetNotHas(r0)) => l0 == r0,
            (Self::SetNotHasAny(l0), Self::SetNotHasAny(r0)) => l0 == r0,
            (Self::Contains(l0), Self::Contains(r0)) => l0 == r0,
            (Self::NotContains(l0), Self::NotContains(r0)) => l0 == r0,
            (Self::Regex(l0), Self::Regex(r0)) => l0.as_str() == r0.as_str(),
            (Self::NotRegex(l0), Self::NotRegex(r0)) => l0.as_str() == r0.as_str(),
            _ => false,
        }
    }
}

impl Eq for AttrValueType {}

// Deterministic ordering for AttrValueType
impl PartialOrd for AttrValueType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AttrValueType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (AttrValueType::Single(l), AttrValueType::Single(r)) => l.cmp(r),
            (AttrValueType::Single(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::Single(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::OneOf(l), AttrValueType::OneOf(r)) => l.cmp(r),
            (AttrValueType::OneOf(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::OneOf(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::NotIn(l), AttrValueType::NotIn(r)) => l.cmp(r),
            (AttrValueType::NotIn(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::NotIn(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::SetHas(l), AttrValueType::SetHas(r)) => l.cmp(r),
            (AttrValueType::SetHas(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::SetHas(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::SetHasAny(l), AttrValueType::SetHasAny(r)) => l.cmp(r),
            (AttrValueType::SetHasAny(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::SetHasAny(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::SetNotHas(l), AttrValueType::SetNotHas(r)) => l.cmp(r),
            (AttrValueType::SetNotHas(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::SetNotHas(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::SetNotHasAny(l), AttrValueType::SetNotHasAny(r)) => l.cmp(r),
            (AttrValueType::SetNotHasAny(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::SetNotHasAny(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::Contains(l), AttrValueType::Contains(r)) => l.cmp(r),
            (AttrValueType::Contains(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::Contains(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::NotContains(l), AttrValueType::NotContains(r)) => l.cmp(r),
            (AttrValueType::NotContains(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::NotContains(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::Regex(l), AttrValueType::Regex(r)) => l.as_str().cmp(r.as_str()),
            (AttrValueType::Regex(_), _) => std::cmp::Ordering::Less,
            (_, AttrValueType::Regex(_)) => std::cmp::Ordering::Greater,

            (AttrValueType::NotRegex(l), AttrValueType::NotRegex(r)) => l.as_str().cmp(r.as_str()),
        }
    }
}

impl fmt::Display for AttrValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttrValueType::Single(val) => write!(f, "{val}"),
            AttrValueType::OneOf(vals) => write!(f, "OneOf({vals:?})"),
            AttrValueType::NotIn(vals) => write!(f, "NotIn({vals:?})"),
            AttrValueType::SetHas(val) => write!(f, "SetHas({val})"),
            AttrValueType::SetHasAny(vals) => write!(f, "SetHasAny({vals:?})"),
            AttrValueType::SetNotHas(val) => write!(f, "SetNotHas({val})"),
            AttrValueType::SetNotHasAny(vals) => write!(f, "SetNotHasAny({vals:?})"),
            AttrValueType::Contains(val) => write!(f, "Contains({val})"),
            AttrValueType::NotContains(val) => write!(f, "NotContains({val})"),
            AttrValueType::Regex(re) => write!(f, "Regex({re})"),
            AttrValueType::NotRegex(re) => write!(f, "NotRegex({re})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operator {
    Exact(usize),        // Represents {n}
    Range(usize, usize), // Represents {n,m}
    AtLeast(usize),      // Represents {n,}
    AtMost(usize),       // Represents {,m}
    ZeroOrOne,           // Represents '?'
    ZeroOrMore,          // Represents '*'
    OneOrMore,           // Represents '+'
    Not,                 // Represents '!'
}

// Deterministic ordering for Operator to enable consistent sorting
impl PartialOrd for Operator {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Operator {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // First compare by discriminant (variant type)
        match (self, other) {
            (Operator::Exact(lhs), Operator::Exact(rhs)) => lhs.cmp(rhs),
            (Operator::Exact(_), _) => std::cmp::Ordering::Less,
            (_, Operator::Exact(_)) => std::cmp::Ordering::Greater,

            (Operator::Range(lmin, lmax), Operator::Range(rmin, rmax)) => {
                lmin.cmp(rmin).then_with(|| lmax.cmp(rmax))
            }
            (Operator::Range(_, _), _) => std::cmp::Ordering::Less,
            (_, Operator::Range(_, _)) => std::cmp::Ordering::Greater,

            (Operator::AtLeast(lhs), Operator::AtLeast(rhs)) => lhs.cmp(rhs),
            (Operator::AtLeast(_), _) => std::cmp::Ordering::Less,
            (_, Operator::AtLeast(_)) => std::cmp::Ordering::Greater,

            (Operator::AtMost(lhs), Operator::AtMost(rhs)) => lhs.cmp(rhs),
            (Operator::AtMost(_), _) => std::cmp::Ordering::Less,
            (_, Operator::AtMost(_)) => std::cmp::Ordering::Greater,

            (Operator::ZeroOrOne, Operator::ZeroOrOne) => std::cmp::Ordering::Equal,
            (Operator::ZeroOrOne, _) => std::cmp::Ordering::Less,
            (_, Operator::ZeroOrOne) => std::cmp::Ordering::Greater,

            (Operator::ZeroOrMore, Operator::ZeroOrMore) => std::cmp::Ordering::Equal,
            (Operator::ZeroOrMore, _) => std::cmp::Ordering::Less,
            (_, Operator::ZeroOrMore) => std::cmp::Ordering::Greater,

            (Operator::OneOrMore, Operator::OneOrMore) => std::cmp::Ordering::Equal,
            (Operator::OneOrMore, _) => std::cmp::Ordering::Less,
            (_, Operator::OneOrMore) => std::cmp::Ordering::Greater,

            (Operator::Not, Operator::Not) => std::cmp::Ordering::Equal,
        }
    }
}

impl fmt::Display for Operator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operator::Exact(n) => write!(f, "Exact({n})"),
            Operator::Range(min, max) => write!(f, "Range({min}, {max})"),
            Operator::AtLeast(n) => write!(f, "AtLeast({n})"),
            Operator::AtMost(n) => write!(f, "AtMost({n})"),
            Operator::ZeroOrOne => write!(f, "ZeroOrOne"),
            Operator::ZeroOrMore => write!(f, "ZeroOrMore"),
            Operator::OneOrMore => write!(f, "OneOrMore"),
            Operator::Not => write!(f, "Not"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttrValue<A = TokenAttributeIndex> {
    pub attr: A,
    pub value: AttrValueType,
    /// Whether this atom is a **true complement** (`!(attr op value)`).
    ///
    /// Distinct from the negated operators (`!=`, `not in`, `!~`), which are
    /// false for a token lacking the attribute because presence is checked
    /// before the operator. A complemented atom is *true* for that token: the
    /// complement is taken over the whole test, presence included. See
    /// `a_complemented_atom_holds_for_a_token_missing_the_attribute`.
    ///
    /// `#[serde(default)]` so an artifact without this field reads back as the
    /// uncomplemented atom it recorded.
    #[serde(default)]
    pub complement: bool,
}

// Manual implementations for Eq/Hash
impl<A: PartialEq> PartialEq for AttrValue<A> {
    fn eq(&self, other: &Self) -> bool {
        self.attr == other.attr && self.value == other.value && self.complement == other.complement
    }
}

impl<A: Eq> Eq for AttrValue<A> {}

impl<A: Hash> Hash for AttrValue<A> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.attr.hash(state);
        // Discriminant first: the arms below hash only the inner value, so
        // `Single("x")`, `Contains("x")`, and `NotContains("x")` — three
        // values that are unequal under `PartialEq` — would otherwise share a
        // bucket. No persisted artifact keys on this hash, so changing it is
        // free.
        std::mem::discriminant(&self.value).hash(state);
        match &self.value {
            AttrValueType::Single(s) => s.hash(state),
            AttrValueType::OneOf(v) => v.hash(state),
            AttrValueType::NotIn(v) => v.hash(state),
            AttrValueType::SetHas(s) => s.hash(state),
            AttrValueType::SetHasAny(v) => v.hash(state),
            AttrValueType::SetNotHas(s) => s.hash(state),
            AttrValueType::SetNotHasAny(v) => v.hash(state),
            AttrValueType::Contains(s) => s.hash(state),
            AttrValueType::NotContains(s) => s.hash(state),
            AttrValueType::Regex(re) => re.as_str().hash(state),
            AttrValueType::NotRegex(re) => re.as_str().hash(state),
        }
        // Appended, and only when set. Hashing `false` unconditionally would
        // move every existing catalog's digest to record a property no
        // tracked catalog uses. This matches the append-only discipline of
        // other semantic hash tags.
        if self.complement {
            state.write_u8(1);
        }
    }
}

/// Analyzer-independent token predicate atom retained by a compiled catalog.
pub(crate) type SymbolicAttrValue = AttrValue<AttributeKey>;

// Deterministic ordering for AttrValue
impl<A: Ord> PartialOrd for AttrValue<A> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<A: Ord> Ord for AttrValue<A> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Put complement last so adding complement support does not reorder
        // existing all-uncomplemented catalogs.
        self.attr
            .cmp(&other.attr)
            .then_with(|| self.value.cmp(&other.value))
            .then_with(|| self.complement.cmp(&other.complement))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenSpec {
    pub op: Operator,
    pub attrs: Vec<SymbolicAttrValue>,
}

// Deterministic ordering for TokenSpec to enable consistent sorting
impl PartialOrd for TokenSpec {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TokenSpec {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Compare by operator first, then by attributes lexicographically
        self.op
            .cmp(&other.op)
            .then_with(|| self.attrs.cmp(&other.attrs))
    }
}

impl TokenSpec {
    /// Creates an epsilon transition (empty transition that doesn't consume input)
    pub fn epsilon() -> Self {
        TokenSpec {
            op: Operator::Exact(0),
            attrs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[doc(hidden)]
pub struct AssertionOccurrenceId(u32);

impl AssertionOccurrenceId {
    pub(crate) fn new(value: usize) -> Self {
        Self(u32::try_from(value).expect("assertion occurrence count is compiler-bounded"))
    }

    pub(crate) fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 assertion occurrence IDs fit usize")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PositionAssertion {
    SentenceStart,
    SentenceEnd,
    PreviousToken(Box<[SymbolicAttrValue]>),
    #[doc(hidden)]
    Lookaround(AssertionOccurrenceId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    Epsilon,
    Consume,
    Assert(PositionAssertion),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransitionLabel {
    kind: TransitionKind,
    spec: TokenSpec,
}

impl TransitionLabel {
    pub fn epsilon() -> Self {
        Self {
            kind: TransitionKind::Epsilon,
            spec: TokenSpec::epsilon(),
        }
    }

    pub fn consume(spec: TokenSpec) -> Self {
        Self {
            kind: TransitionKind::Consume,
            spec,
        }
    }

    pub fn assertion(assertion: PositionAssertion) -> Self {
        let attrs = match &assertion {
            PositionAssertion::PreviousToken(attrs) => attrs.to_vec(),
            PositionAssertion::SentenceStart
            | PositionAssertion::SentenceEnd
            | PositionAssertion::Lookaround(_) => Vec::new(),
        };
        Self {
            kind: TransitionKind::Assert(assertion),
            spec: TokenSpec {
                op: Operator::Exact(0),
                attrs,
            },
        }
    }

    pub fn kind(&self) -> &TransitionKind {
        &self.kind
    }
}

impl std::ops::Deref for TransitionLabel {
    type Target = TokenSpec;

    fn deref(&self) -> &Self::Target {
        &self.spec
    }
}

impl fmt::Display for TransitionLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TransitionKind::Epsilon => formatter.write_str("ε"),
            TransitionKind::Consume => self.spec.fmt(formatter),
            TransitionKind::Assert(assertion) => write!(formatter, "assert({assertion:?})"),
        }
    }
}

impl fmt::Display for TokenSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({:?})", self.op, self.attrs)
    }
}

/// Lowered expression consumed by the NFA state-machine builder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum PatternElement {
    TokenSpec(TokenSpec),
    And(Vec<PatternElement>),
    Or(Vec<PatternElement>),
    Repeat {
        element: Box<PatternElement>,
        min: usize,
        max: Option<usize>,
    },
    Assertion(PositionAssertion),
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub name: CompactString,
    pub(crate) token_specs: Vec<PatternElement>,
    pub metadata: HashMap<CompactString, Value>,
}

/// Collection of patterns with deterministic iteration order.
/// Uses IndexMap (not HashMap) to ensure consistent pattern processing
/// order across runs, which is critical for reproducible results.
#[derive(Clone)]
pub(crate) struct Patterns(pub(crate) indexmap::IndexMap<CompactString, Pattern>);

impl Display for Patterns {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, pattern) in &self.0 {
            writeln!(f, "Pattern name: {name}")?;
            for token_spec in &pattern.token_specs {
                writeln!(f, "  TokenSpec: {token_spec:?}")?;
            }
        }
        Ok(())
    }
}

impl<'a> IntoIterator for &'a Patterns {
    type Item = (&'a CompactString, &'a Pattern);
    type IntoIter = indexmap::map::Iter<'a, CompactString, Pattern>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub state: usize,
    pub token: CompactString,
    pub matched: bool,
    pub next_state: usize,
    pub possible_patterns: Vec<CompactString>,
}

#[derive(Debug, PartialEq, Clone)]
pub enum TraceEvent {
    Transition(TraceEntry),
    CaptureOpen {
        path: CompactString,
        position: usize,
    },
    CaptureClose {
        path: CompactString,
        start: usize,
        end: usize,
    },
    SpanComparison {
        path: CompactString,
        projection: AttributeKey,
        start: usize,
        end: usize,
        matched: bool,
    },
}

#[derive(Debug)]
pub struct Trace {
    pub input: CompactString,
    pub entries: Vec<TraceEntry>,
    pub events: Vec<TraceEvent>,
    pub matches: Vec<Match>,
}
