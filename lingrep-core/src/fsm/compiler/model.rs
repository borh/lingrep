use crate::fsm::{AttrValueType, KeyProjection, PositionAssertion, SymbolicAttrValue};
use crate::token_match::AttributeKey;
use compact_str::CompactString;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

/// A typed conjunction of token-attribute predicates.
pub(crate) type TokenPredicate = Box<[SymbolicAttrValue]>;

/// One attribute test as parsed, before set references resolve.
///
/// Distinct from [`AttrValue`] because a set is scoped to the accumulated
/// catalog: a reference in one document may name a set declared in another,
/// so the operand cannot be a literal vector until [`bind`] has every
/// document. Binding is the only producer of [`AttrValue`], which keeps the
/// unresolved state out of a public, serde-serialized type.
///
/// [`bind`]: super::bind
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedAttr {
    pub(crate) attr: AttributeKey,
    pub(crate) value: ParsedAttrValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParsedAttrValue {
    /// Everything an inline operand produces, already final.
    Resolved(AttrValueType),
    /// `in @NAME` or `not in @NAME`, awaiting the catalog's set table.
    ///
    /// `negated` belongs to the *reference*, not the declaration: one set may
    /// be referenced under both polarities, and both resolve to the same
    /// members.
    Set {
        name: CompactString,
        negated: bool,
        span: SourceSpan,
    },
    /// `has any @NAME` or `not has any @NAME`.
    SetAny {
        name: CompactString,
        negated: bool,
        span: SourceSpan,
    },
}

/// A token test as parsed, before set and test references resolve.
///
/// Mirrors [`ParsedAttrValue`]'s reason for existing: a reference is scoped to
/// the accumulated catalog, so a test in one document may name a test declared
/// in another and cannot become a literal tree until [`bind`] holds every
/// document.
///
/// `And(vec![])` is the any-token predicate `[]`: distribution of an empty
/// conjunction is one empty branch, which is the zero-attribute predicate the
/// runtime has always compiled to `CompiledPredicate::Empty`.
///
/// [`bind`]: super::bind
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParsedTest {
    Attr(ParsedAttr),
    Reference {
        name: CompactString,
        span: SourceSpan,
    },
    And(Vec<ParsedTest>),
    Or(Vec<ParsedTest>),
    Not(Box<ParsedTest>),
}

/// A `test NAME := <test>.` declaration.
///
/// A value, like [`ParsedSet`] and unlike a definition: it carries no
/// metadata, emits nothing, and is absent from `patterns()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedTestDecl {
    pub(crate) name: CompactString,
    pub(crate) body: ParsedTest,
    pub(crate) span: SourceSpan,
}

/// A `set NAME := {…}.` declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedSet {
    pub(crate) name: CompactString,
    pub(crate) members: Vec<CompactString>,
    pub(crate) span: SourceSpan,
}

/// A `group NAME := key <op> value(s).` declaration.
///
/// Derives `%NAME` membership from a metadata predicate instead of a
/// per-definition `groups:` list. Like [`ParsedSet`] it is a value, not a
/// definition — it carries no metadata, emits nothing, and is absent from
/// `patterns()` — but unlike a set it lives in the `%` namespace, which is its
/// own: a group named like a fragment is legal, because nothing `@` resolves
/// can ever be a `%name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedGroupDecl {
    pub(crate) name: CompactString,
    /// The metadata key the predicate reads.
    pub(crate) key: CompactString,
    pub(crate) operator: GroupOperator,
    /// The predicate's value operands: one for a scalar operator, one or more
    /// for a set operator. These are always quoted `metadata_string`s because
    /// the predicate reads metadata.
    pub(crate) values: Vec<String>,
    pub(crate) span: SourceSpan,
}

/// The four forms a group predicate takes, mirroring the attribute modes a
/// researcher already writes inside `[…]`: `=`/`!=` take one literal, `in`/
/// `not in` take a braced set.
///
/// All four are existential and two-valued over presence: a definition lacking
/// the key, or carrying a non-string value, satisfies **no** form — including
/// the negated ones. That is the same presence rule `resolve … partition:`
/// applies to its classes, not the `[…]` complement rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupOperator {
    Equals,
    NotEquals,
    In,
    NotIn,
}

/// One metadata value and the span of the key that carried it.
///
/// The value and its span live in one entry rather than in parallel maps: a
/// second map keyed by the same names is an invariant a later mutator can
/// break silently, which is the shape `CatalogPolicyBuilder` was introduced to
/// remove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MetadataEntry {
    pub(crate) value: Value,
    pub(crate) span: SourceSpan,
}

/// Open, JSON-compatible researcher metadata attached to one definition.
pub(crate) type Metadata = Arc<HashMap<CompactString, MetadataEntry>>;

/// Deterministic identity for a source buffer during one compiler operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SourceId(u32);

impl SourceId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }

    /// The raw index, read only by tests asserting source attribution.
    #[cfg(test)]
    pub(crate) const fn value(self) -> u32 {
        self.0
    }
}

/// A checked byte range within one source buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SourceSpan {
    source: SourceId,
    start: u32,
    end: u32,
}

impl SourceSpan {
    pub(crate) fn new(source: SourceId, byte_range: Range<usize>) -> Result<Self, SourceSpanError> {
        if byte_range.start > byte_range.end {
            return Err(SourceSpanError::ReversedRange);
        }

        Ok(Self {
            source,
            start: u32::try_from(byte_range.start).map_err(|_| SourceSpanError::OffsetOverflow)?,
            end: u32::try_from(byte_range.end).map_err(|_| SourceSpanError::OffsetOverflow)?,
        })
    }

    pub(crate) const fn source(&self) -> SourceId {
        self.source
    }

    pub(crate) fn byte_range(&self) -> Range<u32> {
        self.start..self.end
    }

    pub(crate) const fn start(self) -> u32 {
        self.start
    }

    pub(crate) const fn with_source(self, source: SourceId) -> Self {
        Self { source, ..self }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceSpanError {
    ReversedRange,
    OffsetOverflow,
    InvalidRegistry,
}

/// Private source names and byte-based line indexes for one compile operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceRegistry {
    sources: Vec<ParsedSource>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedSource {
    name: Arc<str>,
    byte_len: u32,
    line_starts: Box<[u32]>,
}

impl ParsedSource {
    fn new(name: &str, text: &str) -> Result<Self, SourceSpanError> {
        let byte_len = u32::try_from(text.len()).map_err(|_| SourceSpanError::OffsetOverflow)?;
        let mut line_starts = vec![0];
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts
                    .push(u32::try_from(index + 1).map_err(|_| SourceSpanError::OffsetOverflow)?);
            }
        }
        Ok(Self {
            name: Arc::from(name),
            byte_len,
            line_starts: line_starts.into_boxed_slice(),
        })
    }
}

impl SourceRegistry {
    pub(crate) fn single(name: &str, text: &str) -> Result<Self, SourceSpanError> {
        let mut registry = Self::default();
        registry.register(name, text)?;
        Ok(registry)
    }

    pub(crate) fn register(&mut self, name: &str, text: &str) -> Result<SourceId, SourceSpanError> {
        self.push(ParsedSource::new(name, text)?)
    }

    pub(crate) fn append_single(&mut self, mut source: Self) -> Result<SourceId, SourceSpanError> {
        if source.sources.len() != 1 {
            return Err(SourceSpanError::InvalidRegistry);
        }
        self.push(source.sources.remove(0))
    }

    pub(crate) fn source_label(&self, source: SourceId) -> Arc<str> {
        Arc::clone(&self.source(source).name)
    }

    pub(crate) fn line_column(&self, span: SourceSpan) -> (usize, usize) {
        let source = self.source(span.source);
        assert!(
            span.end <= source.byte_len,
            "source spans must stay within their registered source"
        );
        let line_index = source
            .line_starts
            .partition_point(|line_start| *line_start <= span.start)
            .saturating_sub(1);
        let line_start = source.line_starts[line_index];
        (
            line_index + 1,
            usize::try_from(span.start - line_start).expect("u32 byte columns fit usize") + 1,
        )
    }

    fn push(&mut self, source: ParsedSource) -> Result<SourceId, SourceSpanError> {
        let source_id = SourceId::new(
            u32::try_from(self.sources.len()).map_err(|_| SourceSpanError::OffsetOverflow)?,
        );
        self.sources.push(source);
        Ok(source_id)
    }

    fn source(&self, source: SourceId) -> &ParsedSource {
        self.sources
            .get(source.index())
            .expect("source IDs must refer to a registered compile source")
    }
}

impl SourceId {
    fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 source IDs must fit usize")
    }
}

/// Typed lingrep patterns document meaning before document-wide reference binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedProgram {
    pub(crate) definitions: Vec<ParsedDefinition>,
    pub(crate) sources: SourceRegistry,
    /// The document's `policy` block, if it declared one.
    ///
    /// Authored, not bound: binding assigns a [`DefId`] over the catalog's
    /// complete definition list, and evidence synthesis has not appended to
    /// that list yet when a document is lowered.
    ///
    /// [`DefId`]: crate::fsm::policy_program::DefId
    pub(crate) policy: Option<crate::fsm::policy_program::Program>,
    /// Structured analyzer declarations, retained as catalog-wide alternatives.
    pub(crate) analyzers: Vec<ParsedAnalyzerConstraint>,
    /// The document's `set` declarations, in source order.
    ///
    /// Authored, not resolved: a reference may name a set from another
    /// accumulated document, so the catalog's table is assembled at bind.
    pub(crate) sets: Vec<ParsedSet>,
    /// The document's `test` declarations, in source order.
    ///
    /// Beside `sets` and for the identical reason: a test is a value scoped to
    /// the accumulated catalog, so a `[@X]` in one document may name a test
    /// declared in another.
    pub(crate) tests: Vec<ParsedTestDecl>,
    /// The document's `group` declarations, in source order.
    ///
    /// Beside `sets` and `tests` and for the same scoping reason: membership
    /// is derived over the accumulated catalog's definitions, so a predicate
    /// declared in one document selects patterns from another.
    pub(crate) groups: Vec<ParsedGroupDecl>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedAnalyzerConstraint {
    pub(crate) family: CompactString,
    pub(crate) resource: Option<CompactString>,
    pub(crate) split: Option<CompactString>,
    pub(crate) segmentation: Box<[CompactString]>,
}

/// One named lingrep patterns definition and its source-level annotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedDefinition {
    pub(crate) name: CompactString,
    pub(crate) body: ParsedDefinitionBody,
    pub(crate) metadata: Metadata,
    pub(crate) key: Option<ParsedKey>,
    pub(crate) span: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParsedDefinitionBody {
    Pattern {
        visibility: DefinitionVisibility,
        expr: ParsedExpr,
        relations: Box<[ParsedRelation]>,
        enhanced_relations: Box<[ParsedEnhancedRelation]>,
    },
    Lexicon {
        by: Box<[AttributeKey]>,
        members: Box<[CompactString]>,
    },
}

/// One matcher for a dependent's `dep.rel`.
///
/// `dep.rel` is DEPREL lowercased whole with its subtype included, so `obl`
/// and `obl:tmod` are two different labels and [`Self::Exact`] tells them
/// apart. [`Self::SubtypeOf`] is the family form authored `obl:*`: it holds
/// for any label that is the prefix followed by `:` and a nonempty subtype,
/// and deliberately **not** for the bare prefix — a wildcard that quietly
/// included the unsubtyped label would make `obl|obl:*` mean `obl:*`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RelationLabel {
    Exact(CompactString),
    /// The prefix before `:`; the authored form is `<prefix>:*`.
    SubtypeOf(CompactString),
}

impl RelationLabel {
    /// Whether one observed `dep.rel` value satisfies this matcher.
    pub(crate) fn matches(&self, observed: &str) -> bool {
        match self {
            Self::Exact(label) => observed == label.as_str(),
            Self::SubtypeOf(prefix) => observed
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_prefix(':'))
                .is_some_and(|subtype| !subtype.is_empty()),
        }
    }

    /// The authored spelling, for diagnostics.
    pub(crate) fn authored(&self) -> CompactString {
        match self {
            Self::Exact(label) => label.clone(),
            Self::SubtypeOf(prefix) => CompactString::from(format!("{prefix}:*")),
        }
    }

    /// Parses one authored term, or `None` when it is not a well-formed label.
    ///
    /// The grammar already constrains a literal term, so this exists for the
    /// other producer: a `set` member, which may be any literal at all.
    pub(crate) fn parse(term: &str) -> Option<Self> {
        let (prefix, subtype) = match term.split_once(':') {
            None => (term, None),
            Some((prefix, subtype)) => (prefix, Some(subtype)),
        };
        let label_chars = |text: &str| {
            let mut chars = text.chars();
            chars.next().is_some_and(|first| first.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        if !label_chars(prefix) {
            return None;
        }
        match subtype {
            None => Some(Self::Exact(CompactString::from(term))),
            Some("*") => Some(Self::SubtypeOf(CompactString::from(prefix))),
            Some(subtype) if label_chars(subtype) => Some(Self::Exact(CompactString::from(term))),
            Some(_) => None,
        }
    }
}

/// The relation predicate inside one `-[...]->` bracket.
///
/// `Any` means any present `dep.rel`; it is not represented as an empty or
/// synthetic label set, so every compiler phase must handle the distinction
/// explicitly. `Listed` is the existing disjunction of literal and named-set
/// terms before binding, and resolved label matchers afterwards.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RelationLabels<T> {
    Any,
    Listed(Box<[T]>),
}

/// One term of an authored `where` bracket, before set references resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParsedLabelTerm {
    Label(RelationLabel),
    /// `@name`, resolved against the accumulated catalog's `set` declarations
    /// at bind, exactly where `[x in @S]` resolves.
    SetReference {
        name: CompactString,
        span: SourceSpan,
    },
}

/// Whether a dependency-edge target must or must not exist.
///
/// Basic anonymous targets and enhanced exact edges carry this polarity. An
/// enhanced negative edge may only refer to endpoints bound elsewhere; it
/// never introduces a node or edge binding of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RelationPolarity {
    Positive,
    Negative,
}

/// The parsed dependent side of a `where` edge.
///
/// A predicate is retained as its unresolved Boolean test until binding can
/// resolve catalog-scoped `test` and `set` references under the shared
/// expansion budgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParsedRelationTarget<T> {
    Captured {
        capture: T,
        span: SourceSpan,
    },
    Anonymous {
        polarity: RelationPolarity,
        span: SourceSpan,
    },
    Predicate {
        polarity: RelationPolarity,
        test: ParsedTest,
        span: SourceSpan,
    },
}

/// The bound dependent side of a `where` edge.
///
/// `Captured` preserves the existing exact-edge constraint. `Anonymous` asks
/// only whether some matching dependent exists and deliberately allocates no
/// capture identity or result field. `Predicate` likewise allocates no
/// identity, but requires or excludes a witness satisfying one branch of its
/// normalized one-token test.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RelationTarget<T> {
    Captured {
        capture: T,
        span: SourceSpan,
    },
    Anonymous {
        polarity: RelationPolarity,
        span: SourceSpan,
    },
    Predicate {
        polarity: RelationPolarity,
        predicates: Box<[TokenPredicate]>,
        span: SourceSpan,
    },
}

/// One authored `where` constraint — `$head -[labels]-> $dependent` — before
/// its capture names and set references are resolved.
///
/// Operand names are resolved against the *materialized* expression, not the
/// authored one, so a capture that arrives through a fragment inclusion is a
/// legal operand exactly when it is a legal capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedRelation {
    pub(crate) head: CompactString,
    pub(crate) head_span: SourceSpan,
    pub(crate) labels: RelationLabels<ParsedLabelTerm>,
    pub(crate) label_span: SourceSpan,
    pub(crate) target: ParsedRelationTarget<CompactString>,
}

/// One `where` constraint with its label set resolved, before its capture
/// names are.
///
/// Set references are gone by this point: `labels` is either `Any` or holds
/// only matchers, which is why a `set` member that is not a well-formed
/// relation label is a bind diagnostic and not a runtime surprise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundRelation {
    pub(crate) head: CompactString,
    pub(crate) head_span: SourceSpan,
    pub(crate) labels: RelationLabels<RelationLabel>,
    pub(crate) label_span: SourceSpan,
    pub(crate) target: RelationTarget<CompactString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParsedEnhancedEndpoint {
    Reference {
        name: CompactString,
        span: SourceSpan,
    },
    Binding {
        name: CompactString,
        test: ParsedTest,
        span: SourceSpan,
    },
    BasicHead {
        dependent: CompactString,
        span: SourceSpan,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedEnhancedRelation {
    pub(crate) polarity: RelationPolarity,
    pub(crate) head: ParsedEnhancedEndpoint,
    pub(crate) labels: RelationLabels<ParsedLabelTerm>,
    pub(crate) label_span: SourceSpan,
    pub(crate) edge: Option<(CompactString, SourceSpan)>,
    pub(crate) dependent: ParsedEnhancedEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BoundEnhancedEndpoint {
    Reference {
        name: CompactString,
        span: SourceSpan,
    },
    Binding {
        name: CompactString,
        predicates: Box<[TokenPredicate]>,
        span: SourceSpan,
    },
    BasicHead {
        dependent: CompactString,
        span: SourceSpan,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundEnhancedRelation {
    pub(crate) polarity: RelationPolarity,
    pub(crate) head: BoundEnhancedEndpoint,
    pub(crate) labels: RelationLabels<RelationLabel>,
    pub(crate) label_span: SourceSpan,
    pub(crate) edge: Option<(CompactString, SourceSpan)>,
    pub(crate) dependent: BoundEnhancedEndpoint,
}

impl ParsedDefinition {
    pub(crate) fn visibility(&self) -> Option<DefinitionVisibility> {
        match &self.body {
            ParsedDefinitionBody::Pattern { visibility, .. } => Some(*visibility),
            ParsedDefinitionBody::Lexicon { .. } => None,
        }
    }

    pub(crate) fn expr(&self) -> Option<&ParsedExpr> {
        match &self.body {
            ParsedDefinitionBody::Pattern { expr, .. } => Some(expr),
            ParsedDefinitionBody::Lexicon { .. } => None,
        }
    }

    pub(crate) fn is_emitting(&self) -> bool {
        !matches!(self.visibility(), Some(DefinitionVisibility::Fragment))
    }

    pub(crate) fn is_ordinary_pattern(&self) -> bool {
        matches!(self.visibility(), Some(DefinitionVisibility::Pattern))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedKey {
    pub(crate) projections: Vec<KeyProjection>,
    pub(crate) source_span: SourceSpan,
    pub(crate) inherited: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DefinitionVisibility {
    Pattern,
    Fragment,
}

impl DefinitionVisibility {
    /// The keyword a catalog writes for this visibility.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Fragment => "fragment",
        }
    }
}

/// The token attribute a `same_as` span comparison projects through.
///
/// A thin wrapper over [`AttributeKey`] rather than a closed enum: the
/// identifier after `by=` resolves through the same name lookup a token
/// predicate's attribute does, so `[x=…]` and `by=x` can never diverge. The
/// projection changes only which attribute is compared — never the width or
/// slot mechanics the polynomial bound rests on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CaptureProjection(pub(crate) AttributeKey);

impl CaptureProjection {
    pub(crate) const fn attribute(&self) -> &AttributeKey {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LookDirection {
    PrecededBy,
    FollowedBy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LookPolarity {
    Positive,
    Negative,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LookaroundExpr<E> {
    pub(crate) direction: LookDirection,
    pub(crate) polarity: LookPolarity,
    pub(crate) body: Box<E>,
    pub(crate) span: SourceSpan,
}

/// Closed source-level lingrep patterns expression meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParsedExpr {
    /// One `[…]`, carrying the whole Boolean test rather than a conjunction.
    ///
    /// Distribution to a one-token alternation happens at bind, so this is the
    /// last layer that still knows how the researcher spelled it — which is
    /// what a diagnostic about the test has to point at.
    Token(ParsedTest),
    Assertion(PositionAssertion),
    Lookaround(LookaroundExpr<ParsedExpr>),
    Sequence(Vec<ParsedExpr>),
    Alternation(Vec<ParsedExpr>),
    /// `E1 & E2`: every operand must match the same span.
    ///
    /// Carries its own span, unlike [`Self::Sequence`] and
    /// [`Self::Alternation`], because one diagnostic has nowhere else to point:
    /// an intersection whose operand widths are disjoint is provably empty, and
    /// that is a property of the *operator*, not of any node inside it. Every
    /// other refusal this construct introduces reports at an inner node that
    /// carries a span already.
    Intersection {
        operands: Vec<ParsedExpr>,
        span: SourceSpan,
    },
    /// Named one-token nodes whose sentence order is unconstrained.
    AnyOrder {
        operands: Vec<ParsedExpr>,
        span: SourceSpan,
    },
    Repeat {
        expr: Box<ParsedExpr>,
        min: usize,
        max: Option<usize>,
    },
    Group(Box<ParsedExpr>),
    NamedGroup {
        name: CompactString,
        expr: Box<ParsedExpr>,
        span: SourceSpan,
    },
    SameAs {
        name: CompactString,
        projection: CaptureProjection,
        span: SourceSpan,
    },
    Reference {
        name: CompactString,
        span: SourceSpan,
    },
}

/// Stable source-order identity for a validated definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DefinitionId(u32);

impl DefinitionId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }

    pub(crate) fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 definition IDs must fit usize")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CaptureDeclId(u32);

impl CaptureDeclId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CaptureId(u32);

impl CaptureId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }

    pub(crate) fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 capture IDs must fit usize")
    }
}

/// Typed lingrep patterns document meaning after reference resolution and validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundProgram {
    pub(crate) definitions: Vec<BoundDefinition>,
    pub(crate) sources: SourceRegistry,
    /// The catalog's `set` declarations, carried past resolution so an unused
    /// one can be named and pointed at.
    pub(crate) sets: Vec<ParsedSet>,
    /// Which sets each definition referenced, recorded while binding resolved
    /// them.
    ///
    /// Resolution is lossy by design — a bound token holds `AttrValue` and no
    /// name — and reachability runs after it. Recovering this from predicate
    /// values afterwards is not possible, so it is carried rather than
    /// rediscovered.
    pub(crate) set_uses: BTreeMap<DefinitionId, BTreeSet<CompactString>>,
    /// The catalog's `test` declarations, carried past resolution for the
    /// reason `sets` is: an unused one has to be named and pointed at.
    pub(crate) tests: Vec<ParsedTestDecl>,
    /// Which tests each definition referenced, transitively through the test
    /// table, recorded while binding resolved them.
    ///
    /// Lossy for the same reason `set_uses` is: a bound token holds
    /// `AttrValue` and no name, so this cannot be recovered afterwards.
    pub(crate) test_uses: BTreeMap<DefinitionId, BTreeSet<CompactString>>,
}

/// A validated definition with stable identity and no source-buffer borrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundDefinition {
    pub(crate) id: DefinitionId,
    pub(crate) source: SourceId,
    pub(crate) name: CompactString,
    pub(crate) body: BoundDefinitionBody,
    pub(crate) metadata: Metadata,
    pub(crate) key: Option<ParsedKey>,
    /// The declaration's own span, kept so a diagnostic about the definition
    /// itself — rather than about something inside its body — has somewhere to
    /// point.
    pub(crate) span: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BoundDefinitionBody {
    Pattern {
        visibility: DefinitionVisibility,
        expr: BoundExpr,
        relations: Box<[BoundRelation]>,
        enhanced_relations: Box<[BoundEnhancedRelation]>,
    },
    Lexicon {
        by: Box<[AttributeKey]>,
        members: Box<[CompactString]>,
    },
}

impl BoundDefinition {
    pub(crate) fn visibility(&self) -> Option<DefinitionVisibility> {
        match &self.body {
            BoundDefinitionBody::Pattern { visibility, .. } => Some(*visibility),
            BoundDefinitionBody::Lexicon { .. } => None,
        }
    }

    pub(crate) fn expr(&self) -> Option<&BoundExpr> {
        match &self.body {
            BoundDefinitionBody::Pattern { expr, .. } => Some(expr),
            BoundDefinitionBody::Lexicon { .. } => None,
        }
    }

    pub(crate) fn relations(&self) -> &[BoundRelation] {
        match &self.body {
            BoundDefinitionBody::Pattern { relations, .. } => relations,
            BoundDefinitionBody::Lexicon { .. } => &[],
        }
    }

    pub(crate) fn enhanced_relations(&self) -> &[BoundEnhancedRelation] {
        match &self.body {
            BoundDefinitionBody::Pattern {
                enhanced_relations, ..
            } => enhanced_relations,
            BoundDefinitionBody::Lexicon { .. } => &[],
        }
    }

    pub(crate) fn is_emitting(&self) -> bool {
        !matches!(self.visibility(), Some(DefinitionVisibility::Fragment))
    }

    pub(crate) fn is_ordinary_pattern(&self) -> bool {
        matches!(self.visibility(), Some(DefinitionVisibility::Pattern))
    }
}

/// Closed validated lingrep patterns expression meaning with ID-based references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BoundExpr {
    Token(TokenPredicate),
    Assertion(PositionAssertion),
    Lookaround(LookaroundExpr<BoundExpr>),
    Sequence(Box<[BoundExpr]>),
    Alternation(Box<[BoundExpr]>),
    /// `E1 & E2`: every operand must match the same span.
    Intersection {
        operands: Box<[BoundExpr]>,
        span: SourceSpan,
    },
    AnyOrder {
        operands: Box<[BoundExpr]>,
        span: SourceSpan,
    },
    Repeat {
        expr: Box<BoundExpr>,
        min: usize,
        max: Option<usize>,
    },
    Capture {
        declaration: CaptureDeclId,
        name: CompactString,
        expr: Box<BoundExpr>,
        span: SourceSpan,
    },
    SameAs {
        declaration: CaptureDeclId,
        projection: CaptureProjection,
        span: SourceSpan,
    },
    Definition {
        id: DefinitionId,
        span: SourceSpan,
    },
}

impl LookaroundExpr<BoundExpr> {
    pub(crate) fn specialized_previous_token(&self) -> Option<&TokenPredicate> {
        if self.direction == LookDirection::PrecededBy
            && self.polarity == LookPolarity::Positive
            && let BoundExpr::Token(predicate) = self.body.as_ref()
        {
            return Some(predicate);
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionExpr {
    Token(TokenPredicate),
    Assertion(PositionAssertion),
    Sequence(Box<[ExecutionExpr]>),
    Alternation(Box<[ExecutionExpr]>),
    /// `E1 & E2`: every operand must match the same span.
    Intersection {
        operands: Box<[ExecutionExpr]>,
        span: SourceSpan,
    },
    AnyOrder {
        operands: Box<[ExecutionExpr]>,
        span: SourceSpan,
    },
    Repeat {
        expr: Box<ExecutionExpr>,
        min: usize,
        max: Option<usize>,
    },
    Capture {
        id: CaptureId,
        name: CompactString,
        path: CompactString,
        span: SourceSpan,
        expr: Box<ExecutionExpr>,
    },
    SameAs {
        capture: CaptureId,
        projection: CaptureProjection,
        span: SourceSpan,
    },
}

impl ExecutionExpr {
    /// Whether running this body ever consults a general lookaround.
    ///
    /// Asked of **every** materialized execution definition, including the
    /// `ExecutionCapability::Plain` ones that build no plan at all. Restricting
    /// it to the capture and span-equality definitions would be correct only
    /// because `DenseMachine::queries_lookaround` covers `Plain` — and that
    /// cross-half reasoning is what asking each executor about its own input
    /// exists to remove.
    ///
    /// A lookaround that lowered to `PositionAssertion::PreviousToken` is
    /// deliberately not counted: the specialization answers it from the token
    /// slice with no session at all. Nor is one reachable only from a *fragment*
    /// — an anchor's `exclude`, say — since materialization expands the
    /// non-fragment definitions and never reaches it.
    ///
    /// Exhaustive, with no `_` arm, so that a new [`ExecutionExpr`] variant
    /// carrying a body is a compile error here rather than a silently unsearched
    /// one. Under-reporting is not a slow path: a plan built as assertion-free
    /// reaches `NoAssertions::holds`, which is `unreachable!`.
    pub(crate) fn queries_lookaround(&self) -> bool {
        match self {
            Self::Assertion(assertion) => {
                matches!(assertion, crate::fsm::PositionAssertion::Lookaround(_))
            }
            Self::Token(_) | Self::SameAs { .. } => false,
            Self::Sequence(children) | Self::Alternation(children) => {
                children.iter().any(Self::queries_lookaround)
            }
            Self::Intersection { operands, .. } => operands.iter().any(Self::queries_lookaround),
            Self::AnyOrder { operands, .. } => operands.iter().any(Self::queries_lookaround),
            Self::Repeat { expr, .. } | Self::Capture { expr, .. } => expr.queries_lookaround(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionCapability {
    Plain,
    Captures,
    SpanEquality,
    Intersection,
    AnyOrder,
    /// A definition whose `where` clause constrains dependency edges between
    /// its captures, and which needs no other whole-definition capability.
    ///
    /// A definition that *also* uses `same_as` or `&` keeps that capability —
    /// the routing question has one answer either way, and the relation
    /// constraints live on the definition, applied by the program wherever
    /// its candidates are enumerated.
    Relations,
}

impl ExecutionCapability {
    /// Whether the tree evaluator recognizes the whole definition, rather than
    /// the dense machine recognizing it.
    ///
    /// The question `compiler::nfa` and `matcher::compiled` both have to answer,
    /// asked once here so the two cannot drift: the first routes such a
    /// definition around the recognition NFA, and the second must then build the
    /// plan that recognizes it instead. A variant added to one side and not the
    /// other yields a pattern that is in the catalog and matches nothing.
    ///
    /// [`Self::Captures`] is **not** one of these. There the dense machine
    /// accepts the span and a program replays it only to recover bindings.
    /// [`Self::Relations`] **is**: the relation filter must see every
    /// candidate span, and the dense sink has already kept only the longest
    /// span per start by the time captures exist — a failing longer candidate
    /// would invisibly shadow a passing shorter one.
    pub(crate) const fn evaluates_whole_definition(self) -> bool {
        match self {
            Self::SpanEquality | Self::Intersection | Self::AnyOrder | Self::Relations => true,
            Self::Plain | Self::Captures => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecutionDefinition {
    pub(crate) name: CompactString,
    pub(crate) expr: ExecutionExpr,
    pub(crate) capability: ExecutionCapability,
    /// The `where` clause's edge constraints, with operands resolved to the
    /// capture identities of the materialized expression. Empty for a
    /// definition without a clause.
    pub(crate) relations: Box<[ExecutionRelation]>,
    pub(crate) enhanced_relations: Box<[ExecutionEnhancedRelation]>,
    /// Observable graph variables in canonical binding-kind/name order.
    pub(crate) graph_bindings: Box<[ExecutionGraphBindingDecl]>,
    pub(crate) metadata: Metadata,
    pub(crate) key: Option<ParsedKey>,
    pub(crate) source: SourceId,
    pub(crate) span: SourceSpan,
}

/// One resolved `where` constraint: a directed basic dependency edge whose
/// relation predicate relates two exact-width-one captures.
///
/// `$head -[labels]-> $dependent` holds when the dependent's `dep.rel` is
/// present and either the predicate is `Any` or some listed member matches,
/// and its `dep.head` — CoNLL-U's
/// one-based sentence-local HEAD — designates the head capture's token. A
/// token carries exactly one `dep.rel`, so at most one member can match and
/// the matched label stays observable as the dependent capture's own
/// `dep.rel` — which is why the predicate needs no new record shape. Operand
/// spans are retained because the width refusal in `analyze` fires per
/// operand, after materialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecutionRelation {
    pub(crate) head: CaptureId,
    pub(crate) head_span: SourceSpan,
    pub(crate) labels: RelationLabels<RelationLabel>,
    pub(crate) label_span: SourceSpan,
    pub(crate) target: RelationTarget<CaptureId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionEnhancedEndpoint {
    Surface(CaptureId),
    BasicHead(CaptureId),
    Graph {
        name: CompactString,
        predicates: Box<[TokenPredicate]>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecutionEnhancedRelation {
    pub(crate) polarity: RelationPolarity,
    pub(crate) head: ExecutionEnhancedEndpoint,
    pub(crate) labels: RelationLabels<RelationLabel>,
    pub(crate) edge: Option<CompactString>,
    pub(crate) dependent: ExecutionEnhancedEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionGraphBindingDecl {
    Node(CompactString),
    Edge(CompactString),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_spans_preserve_source_identity_and_checked_offsets() {
        let source = SourceId::new(7);
        let span = SourceSpan::new(source, 4..6).expect("u32 offsets must be accepted");

        assert_eq!(span.source(), source);
        assert_eq!(span.byte_range(), 4..6);
        assert!(SourceSpan::new(source, std::ops::Range { start: 6, end: 4 }).is_err());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn source_spans_reject_offsets_that_do_not_fit_u32() {
        let source = SourceId::new(0);
        let out_of_range = usize::try_from(u32::MAX).expect("u32 fits usize") + 1;

        assert!(SourceSpan::new(source, 0..out_of_range).is_err());
    }

    /// The unresolved form never escapes the parsed layer.
    ///
    /// `AttrValueType` is public and serde-serialized; a variant meaning "not
    /// resolved yet" would leak a transient compiler state into the published
    /// type. `ParsedAttrValue` carries it instead, and binding is the seam that
    /// removes it.
    #[test]
    fn a_parsed_attribute_can_defer_a_set_reference() {
        let deferred = ParsedAttrValue::Set {
            name: "CONTENT".into(),
            negated: true,
            span: SourceSpan::new(SourceId::new(0), 0..0).expect("test span must be valid"),
        };
        match deferred {
            ParsedAttrValue::Set { name, negated, .. } => {
                assert_eq!(name, "CONTENT");
                assert!(
                    negated,
                    "`not in` is recorded at the reference, not the declaration"
                );
            }
            ParsedAttrValue::Resolved(_) | ParsedAttrValue::SetAny { .. } => {
                panic!("constructed the wrong variant")
            }
        }
    }
}
