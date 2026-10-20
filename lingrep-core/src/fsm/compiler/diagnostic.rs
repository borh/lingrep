use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::ops::Range;
use std::sync::Arc;

/// Stable machine-readable categories for lingrep patterns compiler diagnostics.
///
/// The enum order is the diagnostic-code sort order used when the compiler
/// reports multiple independent failures at one source location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticCode {
    Syntax,
    UnknownNode,
    DuplicateDefinition,
    UndefinedReference,
    ReferenceCycle,
    InvalidRegex,
    InvalidQuantifier,
    InvalidCaptureProjection,
    DuplicateCapture,
    UndefinedCapture,
    ForwardCaptureReference,
    CaptureOutOfScope,
    CaptureNotDefinitelyAssigned,
    /// `any_order` with fewer than two node operands.
    AnyOrderTooFewOperands,
    /// An `any_order` operand that is not a named capture.
    AnyOrderOperandMustBeCapture,
    /// An `any_order` node capture whose body is not statically one token.
    AnyOrderOperandNotSingleToken,
    /// An `any_order` node capture whose body declares another capture.
    AnyOrderNestedCapture,
    /// An `any_order` whose permutation count exceeds the existing expansion
    /// work budget.
    AnyOrderTooComplex,
    CaptureInsideLookaround,
    SpanEqualityInsideLookaround,
    /// `&` inside a lookaround body.
    ///
    /// The sibling of [`Self::SpanEqualityInsideLookaround`], and refused for
    /// the same underlying reason: a lookaround is answered by a boundary
    /// sweep that records only *which boundaries an accepting path reaches*,
    /// deliberately discarding the other endpoint. Intersection is a statement
    /// about both endpoints of one span, which that representation cannot
    /// carry. The reverse nesting — a lookaround inside `&` — is fine, and
    /// stays allowed.
    IntersectionInsideLookaround,
    /// A capture inside an operand of `&`.
    CaptureInsideIntersection,
    /// A `same_as` inside an operand of `&`.
    SpanEqualityInsideIntersection,
    /// An `&` whose operands cannot match a span of the same length.
    ///
    /// Sound but incomplete by construction: disjoint width intervals prove
    /// the intersection empty, and overlapping intervals prove nothing.
    EmptyIntersection,
    BackreferencedCaptureNullable,
    BackreferencedCaptureUnbounded,
    BackreferencedCaptureTooWide,
    MultipleBackreferencedCaptures,
    UnboundedSpanEqualityRepeat,
    NullablePattern,
    NullableRepeat,
    NullableUnboundedRepeat,
    DefinitionDropped,
    /// A `fragment` no pattern references. Non-fatal: it compiles, but it can
    /// never contribute to a match because fragments never emit.
    UnusedFragment,
    /// A `set` no reachable definition references.
    ///
    /// Its own code rather than `UnusedFragment` because the message names a
    /// kind, and a set is not a fragment.
    UnusedSet,
    /// A `test` no reachable definition references.
    ///
    /// Its own code rather than `UnusedFragment` because the message names a
    /// kind, and a test is not a fragment.
    UnusedTest,
    /// A token test whose disjunctive normal form exceeds the compile budget.
    ///
    /// An error rather than a warning: the alternative is building a branch
    /// list whose size is exponential in the authored source, which is the
    /// unbounded compile this budget exists to prevent.
    TokenTestTooComplex,
    /// A token test whose `test` references, once substituted, exceed the
    /// resolved-node budget.
    ///
    /// Its own code rather than [`Self::TokenTestTooComplex`] because the two
    /// bound different things — this is substitution size, checked before
    /// distribution ever runs, and a chain that names another `test` twice
    /// doubles this count at every level without ever widening a branch. An
    /// error for the same reason: the alternative is materializing an
    /// exponentially large resolved tree.
    TokenTestExpansionTooLarge,
    /// A documented claim (`examples:`, `counterexamples:`) that can never be
    /// executed — attached to a definition that never emits, or holding a
    /// value that is not a list of strings. Non-fatal: the catalog compiles and
    /// matches correctly; what is broken is the annotation.
    UnverifiableExamples,
    /// A recognized result-policy declaration whose value the engine cannot use
    /// — an `admit`'s `terminal:` that is not a well-formed mapping, or a
    /// `transform:` that is not a list. Fatal: the catalog declares
    /// a publication rule that cannot be executed, and silently ignoring it
    /// would publish records the catalog says it does not publish.
    InvalidPolicyValue,
    /// An unrecognized metadata key within one edit of a recognized
    /// result-policy key. Non-fatal: free-form annotation is legal, so this
    /// cannot be an error — but a misspelled policy key is inert, and inert
    /// and honored look identical from the catalog.
    SuspiciousPolicyKey,
    /// Result policy declared where it can never apply — on a `fragment`,
    /// which never emits, or a policy statement whose subject selects no
    /// publishable definition. Non-fatal, and the same class as
    /// [`DiagnosticCode::UnverifiableExamples`]: the catalog compiles and
    /// matches correctly; what is broken is the declaration.
    ///
    /// The statement case is the one an author cannot see: a `%group` holding
    /// only fragments *has* members, so the reference resolves and the
    /// statement runs — over an empty set, on every sentence. Inert and honored
    /// look identical from the catalog, which is why this warns rather than
    /// staying silent, and why it does not refuse: the engine executes the
    /// statement exactly as written.
    IneffectivePolicy,
    /// A recognized result-policy declaration the engine can execute exactly as
    /// written, but whose magnitude says the author meant something else — an
    /// `admit … terminal: {within: N, …}` wide enough to admit nearly every
    /// record.
    /// Non-fatal: the catalog is internally consistent, so refusing it would be
    /// the compiler overruling a declaration it can honor.
    ImplausiblePolicyValue,
    /// A `partition:` argument on a policy verb other than `resolve`. Fatal,
    /// and its own code rather than the generic unknown-argument syntax error:
    /// partitioning subdivides a contest, and only `resolve` holds one — the
    /// author's mistake is the verb, not the spelling of the key.
    PartitionOnNonResolve,
    /// A `resolve … partition: <key>` selecting a definition that carries the
    /// key with a non-string value. Fatal: a class is the string value of the
    /// key, so a number or list names no class, and silently skipping the
    /// definition would move it out of every contest without saying so. Points
    /// at that definition's own key, per the metadata-diagnostic rule.
    PartitionKeyNotString,
    /// A catalog-level `anchor:` declaration the compiler cannot turn into a
    /// derived sentence anchor — a malformed mapping, a name declared twice, an
    /// `exclude` naming nothing or naming a pattern, or an `exclude` whose body
    /// an anchor may not contain. Fatal: an anchor is what an `admit` measures
    /// against, so a declaration that cannot be built would silently move every
    /// admission decision back to the raw sentence end.
    InvalidAnchorDeclaration,
    /// A declared anchor no `admit` measures against. Non-fatal, and the
    /// same class as [`DiagnosticCode::UnusedFragment`]: the catalog compiles
    /// and matches correctly, but an anchor nothing consults cannot affect any
    /// result, and inert and honored look identical from the catalog.
    UnreferencedAnchor,
    /// A `max_nesting:` claim the compiler cannot execute — a malformed or
    /// incomplete mapping, an empty `witness:`/`divergence:`, an entry with
    /// other than exactly one `|` sentinel, a claim on a fragment that is no
    /// anchor's `exclude`, or a declared anchor whose `exclude` carries no
    /// claim at all. Fatal: an `exclude` fragment is a bounded approximation of
    /// an unbounded scanner, and a bound nothing records or executes is the
    /// "number that looks load-bearing and is not" the compound encoding exists
    /// to prevent.
    InvalidNestingClaim,
    /// A `render:` naming something outside the renderer registry. Fatal: a
    /// renderer is a named, code-reviewed way of building a unit, and an
    /// unknown name would otherwise publish records with no unit while the
    /// catalog claims one. The registry is empty, so every `render:` lands here.
    UnknownRenderer,
    /// A `key:` reference that cannot be resolved to exactly one capture
    /// occurrence — naming no capture, naming one that expands more than once,
    /// or naming one under a quantifier. Fatal: a quantified capture retains
    /// only its last iteration, so a key projected off one would be plausible
    /// and undetectable in any output.
    AmbiguousCaptureReference,
    /// A `key:` projection onto a capture that can cover more than one token.
    /// Fatal for the same reason `substitute` refuses the shape: reading one
    /// attribute off a multi-token span has no defined answer, and a
    /// head-token rule would be invented rather than witnessed.
    MultiTokenCaptureProjection,
    /// A whole-match key projection on a definition not proven to consume
    /// exactly one token.
    MultiTokenMatchProjection,
    /// A source-authored definition in a namespace the compiler reserves for
    /// definitions it synthesizes. Fatal: a collision would let a catalog name
    /// silently take over a synthesized emitter's records.
    ReservedName,
    /// An unquoted attribute value or `set` member whose text contains `|`.
    ///
    /// The grammar's bare `value` token is terminated by `&` but not by `|`,
    /// so `[text=a[text=b]` is one atom whose value is the seven characters
    /// `a[text=b` — a predicate that matches nothing — while the spaced
    /// `[text=a [text=b]` is the disjunction the author meant. The unspaced
    /// spelling is the exact analogue of the unspaced `&` that *is* legal, so
    /// the wrong reading is also the natural one.
    ///
    /// Its own code rather than [`Self::Syntax`] because the message names two
    /// specific recoveries: `\u007C` if the pipe belongs inside the literal,
    /// and spacing the operands if a disjunction was meant.
    ///
    /// An **error**. It shipped as a warning, on the argument that `|` inside
    /// `[…]` had exactly one possible reading before the Boolean token-test
    /// syntax existed and that refusing it would narrow the language. What the
    /// warning left behind is what overturned that: a catalog that compiles,
    /// matches nothing, and publishes an empty result a reader cannot tell
    /// from an absent phenomenon.
    ///
    /// Nothing is narrowed, because escape decoding runs after tokenization —
    /// `|` reaches every position a bare literal can occupy, so a value
    /// that genuinely holds a pipe stays writable. It is silent on every
    /// tracked catalog, which is why the promotion breaks nothing that ships
    /// here.
    PipeInBareValue,
    /// An unquoted attribute value or `set` member wrapped in double quotes.
    ///
    /// There is no quoted-value form in a token test, so the quotes are
    /// ordinary characters and `[text="の"]` is a predicate for the
    /// three-character literal `"の"` — one that matches nothing, compiles
    /// clean, and publishes an empty result indistinguishable from an absent
    /// phenomenon. Exactly [`Self::PipeInBareValue`]'s failure mode, reached
    /// from the opposite direction: metadata values are *always* quoted, so
    /// an author moving between a `key:` block and a token test carries the
    /// convention across a boundary where it inverts.
    ///
    /// Its own code rather than [`Self::Syntax`] because the message names the
    /// recovery, and its own code rather than [`Self::PipeInBareValue`]
    /// because the two name different recoveries for different mistakes.
    ///
    /// An **error**, on the same argument: escape decoding runs after
    /// tokenization, so a literal that genuinely begins and ends with a double
    /// quote stays writable as `\u0022…\u0022`. It is silent on every tracked
    /// catalog.
    QuotedAttributeValue,
    /// A `key: none` annotation for a key no `defaults` block supplies.
    ///
    /// `none` is not a value: it deletes the entry the `defaults` block would
    /// have materialized onto the annotated definition. With no default to
    /// delete, the annotation asks for nothing that omitting the key would not
    /// already do — and an accepted no-op here would silently become a real
    /// unset the day someone adds the default. Fatal, and points at the key
    /// that carried the `none`, per the metadata-diagnostic rule.
    UnsetWithoutDefault,
    /// A `none` anywhere but the top level of a per-definition annotation
    /// entry — inside a list or map, or as a `defaults` entry's value.
    ///
    /// The grammar admits `none` wherever a metadata value sits and leaves the
    /// scoping here, the way `_policy_value` parses open-endedly. Only an
    /// annotation entry's whole value can mean "unset the file default":
    /// nested inside a value there is no default to delete, and inside the
    /// `defaults` block it would be unsetting the very entry it declares.
    /// Fatal; points at the key for a `defaults` entry and at the `none`
    /// itself when nested, because the defect sits on the inner value.
    UnsetOutsideAnnotation,
    /// A second `group` declaration with an already-declared name.
    ///
    /// Its own code rather than [`Self::DuplicateDefinition`] because the
    /// namespaces differ: `%` is its own space, shared with nothing `@`
    /// resolves, so a group colliding with a fragment is legal and only a
    /// group colliding with a group is a collision at all. Fatal, reported at
    /// the second declaration and naming the first: keeping the last
    /// declaration silently would let two predicates contest one name.
    DuplicateGroupDefinition,
    /// A literal `groups:` entry naming a derived `group`.
    ///
    /// Derivation and restatement must not coexist: membership in a derived
    /// group is a function of the declaration's predicate, and a literal entry
    /// restating it would hold the same answer in two places — right until an
    /// edit moves one and not the other, silently. Fatal, and points at the
    /// `groups:` key that carried the entry, per the metadata-diagnostic rule.
    LiteralEntryForDerivedGroup,
    /// A `where` clause on a `fragment` definition.
    ///
    /// A fragment never emits, so its clause could act only through inclusion
    /// — and inclusion-time capture renaming is its own design, deferred until
    /// witnessed. The grammar parses the clause here deliberately; a sentence
    /// naming the refusal beats an `ERROR` node.
    WhereOnFragment,
    /// A `where` operand naming no capture in the definition's materialized
    /// expression.
    WhereCaptureUndefined,
    /// A `where` operand whose capture is not statically exactly one token
    /// wide.
    ///
    /// An edge endpoint is one word: `dep.head` designates a single token, so
    /// a multi-token operand has no head ordinal to compare against.
    WhereCaptureNotSingleToken,
    /// Both operands of one `where` constraint naming the same capture.
    ///
    /// A basic UD tree contains no self-edge, so the constraint could never
    /// hold and the pattern would silently never match.
    WhereSelfEdge,
    /// One `where` label set repeating a term.
    ///
    /// A dependent carries exactly one `dep.rel`, so a repeat adds nothing.
    /// Refused rather than deduped, for the reason the language refuses a
    /// duplicate name: the repeat is more likely a typo than intent.
    DuplicateRelationLabel,
    /// A `where` label term that is not a well-formed relation label.
    ///
    /// Unreachable for a literal, which the grammar already constrains. The
    /// producer is a `set` member: a set names arbitrary literals, so
    /// `set S := {動詞}.` is legal and `-[@S]->` is not.
    InvalidRelationLabel,
    /// Two `where` constraints sharing a dependent capture with disjoint
    /// label sets.
    ///
    /// A token carries exactly one `dep.rel`, so disjoint sets on one
    /// dependent can never both hold and the pattern would silently never
    /// match — the `WhereSelfEdge` defect class. Sound and deliberately
    /// incomplete: nested captures legally bind one token, so a shared
    /// dependent whose sets *do* intersect stays legal.
    WhereContradictoryLabels,
    /// Positive and negative anonymous edge existence constraints on the same
    /// head have overlapping label predicates.
    ///
    /// A complete dependency observation cannot both contain and omit the
    /// same class of outgoing edge, so the conjunction would silently match
    /// nothing.
    WhereContradictoryExistence,
    /// Two `where` operands that no single match can bind together.
    ///
    /// A capture in an alternation branch binds only when that branch is
    /// taken, so operands in different branches of a single-entry alternation
    /// can never both be bound — and a clause is a conjunction, so the
    /// definition silently never matches. The `WhereSelfEdge` defect class in
    /// its third spelling.
    ///
    /// "Single-entry" is load-bearing and is the whole analysis: an
    /// alternation under a repetition that can run twice is entered once per
    /// iteration, so two iterations legally bind two branches' captures.
    /// Sound and deliberately incomplete — exclusion arising from disjoint
    /// token predicates is a satisfiability question and is not decided here.
    WhereExclusiveCaptures,
    /// An enhanced graph binding reuses a surface or graph binding name.
    EnhancedGraphBindingDuplicate,
    /// An enhanced endpoint names neither a surface capture nor a graph binding.
    EnhancedGraphBindingUndefined,
    /// An enhanced graph component has no exact-width-one surface anchor.
    EnhancedGraphComponentUnanchored,
    LimitExceeded,
}

impl DiagnosticCode {
    /// Returns the stable presentation name used at application boundaries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "Syntax",
            Self::UnknownNode => "UnknownNode",
            Self::WhereOnFragment => "WhereOnFragment",
            Self::WhereCaptureUndefined => "WhereCaptureUndefined",
            Self::WhereCaptureNotSingleToken => "WhereCaptureNotSingleToken",
            Self::WhereSelfEdge => "WhereSelfEdge",
            Self::DuplicateRelationLabel => "DuplicateRelationLabel",
            Self::InvalidRelationLabel => "InvalidRelationLabel",
            Self::WhereContradictoryLabels => "WhereContradictoryLabels",
            Self::WhereContradictoryExistence => "WhereContradictoryExistence",
            Self::WhereExclusiveCaptures => "WhereExclusiveCaptures",
            Self::EnhancedGraphBindingDuplicate => "EnhancedGraphBindingDuplicate",
            Self::EnhancedGraphBindingUndefined => "EnhancedGraphBindingUndefined",
            Self::EnhancedGraphComponentUnanchored => "EnhancedGraphComponentUnanchored",
            Self::DuplicateDefinition => "DuplicateDefinition",
            Self::UndefinedReference => "UndefinedReference",
            Self::ReferenceCycle => "ReferenceCycle",
            Self::InvalidRegex => "InvalidRegex",
            Self::InvalidQuantifier => "InvalidQuantifier",
            Self::InvalidCaptureProjection => "InvalidCaptureProjection",
            Self::DuplicateCapture => "DuplicateCapture",
            Self::UndefinedCapture => "UndefinedCapture",
            Self::ForwardCaptureReference => "ForwardCaptureReference",
            Self::CaptureOutOfScope => "CaptureOutOfScope",
            Self::CaptureNotDefinitelyAssigned => "CaptureNotDefinitelyAssigned",
            Self::AnyOrderTooFewOperands => "AnyOrderTooFewOperands",
            Self::AnyOrderOperandMustBeCapture => "AnyOrderOperandMustBeCapture",
            Self::AnyOrderOperandNotSingleToken => "AnyOrderOperandNotSingleToken",
            Self::AnyOrderNestedCapture => "AnyOrderNestedCapture",
            Self::AnyOrderTooComplex => "AnyOrderTooComplex",
            Self::CaptureInsideLookaround => "CaptureInsideLookaround",
            Self::SpanEqualityInsideLookaround => "SpanEqualityInsideLookaround",
            Self::IntersectionInsideLookaround => "IntersectionInsideLookaround",
            Self::CaptureInsideIntersection => "CaptureInsideIntersection",
            Self::SpanEqualityInsideIntersection => "SpanEqualityInsideIntersection",
            Self::EmptyIntersection => "EmptyIntersection",
            Self::BackreferencedCaptureNullable => "BackreferencedCaptureNullable",
            Self::BackreferencedCaptureUnbounded => "BackreferencedCaptureUnbounded",
            Self::BackreferencedCaptureTooWide => "BackreferencedCaptureTooWide",
            Self::MultipleBackreferencedCaptures => "MultipleBackreferencedCaptures",
            Self::UnboundedSpanEqualityRepeat => "UnboundedSpanEqualityRepeat",
            Self::NullablePattern => "NullablePattern",
            Self::NullableRepeat => "NullableRepeat",
            Self::NullableUnboundedRepeat => "NullableUnboundedRepeat",
            Self::DefinitionDropped => "DefinitionDropped",
            Self::UnusedFragment => "UnusedFragment",
            Self::UnusedSet => "UnusedSet",
            Self::UnusedTest => "UnusedTest",
            Self::TokenTestTooComplex => "TokenTestTooComplex",
            Self::TokenTestExpansionTooLarge => "TokenTestExpansionTooLarge",
            Self::UnverifiableExamples => "UnverifiableExamples",
            Self::InvalidPolicyValue => "InvalidPolicyValue",
            Self::SuspiciousPolicyKey => "SuspiciousPolicyKey",
            Self::IneffectivePolicy => "IneffectivePolicy",
            Self::ImplausiblePolicyValue => "ImplausiblePolicyValue",
            Self::PartitionOnNonResolve => "PartitionOnNonResolve",
            Self::PartitionKeyNotString => "PartitionKeyNotString",
            Self::InvalidAnchorDeclaration => "InvalidAnchorDeclaration",
            Self::UnreferencedAnchor => "UnreferencedAnchor",
            Self::InvalidNestingClaim => "InvalidNestingClaim",
            Self::UnknownRenderer => "UnknownRenderer",
            Self::AmbiguousCaptureReference => "AmbiguousCaptureReference",
            Self::MultiTokenCaptureProjection => "MultiTokenCaptureProjection",
            Self::MultiTokenMatchProjection => "MultiTokenMatchProjection",
            Self::ReservedName => "ReservedName",
            Self::PipeInBareValue => "PipeInBareValue",
            Self::QuotedAttributeValue => "QuotedAttributeValue",
            Self::UnsetWithoutDefault => "UnsetWithoutDefault",
            Self::UnsetOutsideAnnotation => "UnsetOutsideAnnotation",
            Self::DuplicateGroupDefinition => "DuplicateGroupDefinition",
            Self::LiteralEntryForDerivedGroup => "LiteralEntryForDerivedGroup",
            Self::LimitExceeded => "LimitExceeded",
        }
    }
}

impl Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether a diagnostic prevents compilation from producing a matcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

/// A source location that owns its display data and never borrows lingrep patterns text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiagnosticLocation {
    source_name: Arc<str>,
    byte_range: Range<usize>,
    line: usize,
    column: usize,
}

impl DiagnosticLocation {
    /// Creates a source location with caller-owned display data.
    pub fn new(
        source_name: impl Into<String>,
        byte_range: Range<usize>,
        line: usize,
        column: usize,
    ) -> Self {
        Self::with_source_label(Arc::from(source_name.into()), byte_range, line, column)
    }

    pub(crate) fn with_source_label(
        source_name: Arc<str>,
        byte_range: Range<usize>,
        line: usize,
        column: usize,
    ) -> Self {
        Self {
            source_name,
            byte_range,
            line,
            column,
        }
    }

    /// Returns the caller-supplied display name of the source.
    pub fn source_name(&self) -> &str {
        self.source_name.as_ref()
    }

    /// Returns the primary byte range in the source.
    pub fn byte_range(&self) -> Range<usize> {
        self.byte_range.clone()
    }

    /// Returns the one-based source line of the diagnostic.
    pub fn line(&self) -> usize {
        self.line
    }

    /// Returns the one-based source column of the diagnostic.
    pub fn column(&self) -> usize {
        self.column
    }
}

/// An auxiliary location associated with a primary compiler diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelatedDiagnosticLocation {
    location: DiagnosticLocation,
    message: String,
}

impl RelatedDiagnosticLocation {
    /// Creates an auxiliary source location and its explanatory message.
    pub fn new(location: DiagnosticLocation, message: impl Into<String>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }

    /// Returns the related location.
    pub fn location(&self) -> &DiagnosticLocation {
        &self.location
    }

    /// Returns the explanatory message for the related location.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A structured lingrep patterns compiler diagnostic.
///
/// Callers must use [`Self::code`] rather than matching presentation text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    code: DiagnosticCode,
    severity: DiagnosticSeverity,
    location: DiagnosticLocation,
    related_locations: Vec<RelatedDiagnosticLocation>,
    pub message: String,
}

impl Diagnostic {
    /// Creates an error diagnostic at a self-contained source location.
    pub fn error(
        code: DiagnosticCode,
        source_name: impl Into<String>,
        byte_range: Range<usize>,
        message: impl Into<String>,
        line: usize,
        column: usize,
    ) -> Self {
        Self::new(
            code,
            DiagnosticSeverity::Error,
            DiagnosticLocation::new(source_name, byte_range, line, column),
            message,
        )
    }

    pub(crate) fn error_with_source_label(
        code: DiagnosticCode,
        source_name: Arc<str>,
        byte_range: Range<usize>,
        message: impl Into<String>,
        line: usize,
        column: usize,
    ) -> Self {
        Self::new(
            code,
            DiagnosticSeverity::Error,
            DiagnosticLocation::with_source_label(source_name, byte_range, line, column),
            message,
        )
    }

    /// Creates a warning diagnostic at a self-contained source location.
    pub fn warning(
        code: DiagnosticCode,
        source_name: impl Into<String>,
        byte_range: Range<usize>,
        message: impl Into<String>,
        line: usize,
        column: usize,
    ) -> Self {
        Self::new(
            code,
            DiagnosticSeverity::Warning,
            DiagnosticLocation::new(source_name, byte_range, line, column),
            message,
        )
    }

    pub(crate) fn warning_with_source_label(
        code: DiagnosticCode,
        source_name: Arc<str>,
        byte_range: Range<usize>,
        message: impl Into<String>,
        line: usize,
        column: usize,
    ) -> Self {
        Self::new(
            code,
            DiagnosticSeverity::Warning,
            DiagnosticLocation::with_source_label(source_name, byte_range, line, column),
            message,
        )
    }

    /// Returns the stable machine-readable diagnostic code.
    pub fn code(&self) -> DiagnosticCode {
        self.code
    }

    /// Returns whether the diagnostic prevents compilation.
    pub fn severity(&self) -> DiagnosticSeverity {
        self.severity
    }

    /// Returns the primary source location.
    pub fn location(&self) -> &DiagnosticLocation {
        &self.location
    }

    /// Returns the optional related source locations.
    pub fn related_locations(&self) -> &[RelatedDiagnosticLocation] {
        &self.related_locations
    }

    /// Adds an explanatory related source location.
    #[must_use]
    pub fn with_related_location(mut self, related_location: RelatedDiagnosticLocation) -> Self {
        self.related_locations.push(related_location);
        self
    }

    fn new(
        code: DiagnosticCode,
        severity: DiagnosticSeverity,
        location: DiagnosticLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            location,
            related_locations: Vec::new(),
            message: message.into(),
        }
    }
}

/// A structured failure at the lingrep patterns compilation boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub diagnostics: Vec<Diagnostic>,
}

impl CompileError {
    pub(crate) fn new(source_name: &str, message: impl Display) -> Self {
        Self::from_diagnostics(vec![Diagnostic::error(
            DiagnosticCode::Syntax,
            source_name,
            0..0,
            format!("{source_name}: {message}"),
            1,
            1,
        )])
    }

    pub(crate) fn new_with_source_label(source_name: Arc<str>, message: impl Display) -> Self {
        Self::from_diagnostics(vec![Diagnostic::error_with_source_label(
            DiagnosticCode::Syntax,
            source_name,
            0..0,
            message.to_string(),
            1,
            1,
        )])
    }

    pub(crate) fn from_diagnostics(mut diagnostics: Vec<Diagnostic>) -> Self {
        diagnostics.sort_by(|left, right| {
            left.location
                .source_name
                .cmp(&right.location.source_name)
                .then_with(|| {
                    left.location
                        .byte_range
                        .start
                        .cmp(&right.location.byte_range.start)
                })
                .then_with(|| {
                    left.location
                        .byte_range
                        .end
                        .cmp(&right.location.byte_range.end)
                })
                .then_with(|| left.code.cmp(&right.code))
                .then_with(|| left.message.cmp(&right.message))
        });
        Self { diagnostics }
    }
}

impl Display for CompileError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let mut diagnostics = self.diagnostics.iter();
        if let Some(first) = diagnostics.next() {
            formatter.write_str(&first.message)?;
        }
        for diagnostic in diagnostics {
            write!(formatter, "\n{}", diagnostic.message)?;
        }
        Ok(())
    }
}

impl Error for CompileError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_are_self_contained() {
        let diagnostic = Diagnostic::error(
            DiagnosticCode::UndefinedReference,
            "x.lx",
            4..6,
            "undefined @x",
            1,
            5,
        );

        assert_eq!(diagnostic.code(), DiagnosticCode::UndefinedReference);
        assert_eq!(diagnostic.location().source_name(), "x.lx");
        assert_eq!(diagnostic.location().byte_range(), 4..6);
        assert_eq!(diagnostic.location().line(), 1);
        assert_eq!(diagnostic.location().column(), 5);
    }

    #[test]
    fn diagnostic_codes_are_stable_independent_of_message_text() {
        let first = Diagnostic::error(
            DiagnosticCode::InvalidRegex,
            "x.lx",
            0..1,
            "first wording",
            1,
            1,
        );
        let second = Diagnostic::error(
            DiagnosticCode::InvalidRegex,
            "x.lx",
            2..3,
            "second wording",
            1,
            3,
        );

        assert_eq!(first.code(), second.code());
        assert!(DiagnosticCode::Syntax < DiagnosticCode::UndefinedReference);
    }
}
