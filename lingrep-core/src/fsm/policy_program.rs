//! The ordered policy program, authored and bound.
//!
//! Two representations, deliberately:
//!
//! * [`Program`] is what the author wrote — names and spans, suitable for
//!   diagnostics that must point at a statement.
//! * [`BoundProgram`] is what runs — [`DefId`] and [`GroupId`], resolved once
//!   at compile time. Carrying validated strings into the engine would mean
//!   re-resolving a name per record per sentence, and would leave "validated"
//!   as a property nothing in the type system holds.
//!
//! Record identity in the engine is [`RawId`], an index into the canonical raw
//! slice for one sentence. It is stable across the snapshot clones the decide
//! phase takes; a pointer is not, and the published name is not either, because
//! `output_name` rewrites it in a later pass.

use std::collections::BTreeMap;

use compact_str::CompactString;
use indexmap::IndexMap;

use super::compiler::model::SourceSpan;
use super::policy::Terminal;

/// Each partitioned definition's class, by authored definition name.
///
/// Resolved fully at policy-compile time from the accumulated catalog's
/// metadata — the string value of the statement's `partition:` key on each
/// definition its subject selects — so the engine stays metadata-free at run
/// time. A selected definition lacking the key has no entry, and a record of
/// it is not selected by the statement at all.
///
/// Keyed by name rather than [`DefId`] because the engine looks a candidate up
/// through its raw record's authored `pattern` name, which policy runs early
/// enough to still carry (`output_name` rewrites it only in a later pass).
pub type PartitionMap = BTreeMap<CompactString, CompactString>;

/// Where an operation was declared.
///
/// Opaque on purpose. It wraps the compiler's `SourceSpan`, which carries a
/// source identity as well as a byte range — a span holding only offsets could
/// not survive `FsmBuilder`'s source remapping, and a test asserting a bare
/// offset could not tell two documents of similar length apart. Consumers
/// compare and print spans; they do not read offsets out of them, and a
/// diagnostic naming a file is what a test should assert on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PolicySpan(pub(crate) SourceSpan);

/// A definition's index in the catalog's **complete** definition list.
///
/// Complete is load-bearing: an index assigned over a filtered list — say only
/// the definitions carrying a group — designates a different definition than
/// the same number does anywhere else, and group selection would silently act
/// on the wrong one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefId(pub(crate) usize);

impl DefId {
    /// This definition's index in the catalog's complete definition list.
    #[must_use]
    pub fn index(self) -> usize {
        self.0
    }
}

/// A declared group's index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupId(pub(crate) usize);

impl GroupId {
    /// This group's index.
    #[must_use]
    pub fn index(self) -> usize {
        self.0
    }
}

/// A raw match's index in the canonical slice for one sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawId(pub(crate) usize);

impl RawId {
    /// This match's index in the sentence's canonical raw slice.
    #[must_use]
    pub fn index(self) -> usize {
        self.0
    }
}

/// One catalog's authored policy program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    /// Operations in authored order. Order is semantic.
    pub ops: Vec<Op>,
    /// One label per entry in [`Self::ops`], at the same index.
    ///
    /// Built by the lowerer, which is the only stage holding both the statement
    /// node and the source text it came from. [`Self::bind`] carries each
    /// surviving operation's label across with it, so the correspondence is
    /// established once here rather than reconstructed downstream.
    pub labels: Vec<StatementLabel>,
}

/// What an operation acts on. Exactly one token in the surface syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    All,
    Patterns,
    Def(CompactString),
    Group(CompactString),
}

/// What an association looks for. No inline expressions in v1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    Def(CompactString),
    Group(CompactString),
}

/// What an association does to a subject record the relation holds for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Exclude,
    Require,
    Detach,
}

/// How a subject record and an operand match must stand to each other.
///
/// Evaluated on **raw** spans in every case: `evidence` changes the
/// membership of the operand set, never geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    AtStart,
    AtEnd,
    After,
    Before,
    Within,
    InSentence,
}

/// Which match set an association draws operands from.
///
/// There is no default: an association MUST declare one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// The immutable canonical match set, including synthesized evidence.
    Raw,
    /// The survivor snapshot taken at this operation's entry.
    Current,
}

/// One ranking dimension for `resolve`.
///
/// A rank must be total. `[…, Start, End, Pattern]` is total only
/// because the emitter dedups on `(pattern, start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Criterion {
    Longest,
    Shortest,
    PriorityOrder,
    Start,
    End,
    Pattern,
}

/// One authored operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Resolve {
        subject: Subject,
        rank: Vec<Criterion>,
        /// The `partition:` metadata key, still a name: the classes it names
        /// are a function of the catalog's metadata, which only the compiler
        /// holds, so [`Program::bind`]'s caller resolves it into the bound
        /// operation's [`PartitionMap`].
        partition: Option<CompactString>,
        span: PolicySpan,
    },
    Associate {
        effect: Effect,
        subject: Subject,
        operand: Operand,
        relation: Relation,
        evidence: Evidence,
        span: PolicySpan,
    },
    Admit {
        subject: Subject,
        terminal: Terminal,
        anchor: CompactString,
        span: PolicySpan,
    },
}

impl Op {
    /// This operation's source span.
    #[must_use]
    pub fn span(&self) -> PolicySpan {
        let (Self::Resolve { span, .. } | Self::Associate { span, .. } | Self::Admit { span, .. }) =
            self;
        *span
    }

    /// This operation's subject.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        let (Self::Resolve { subject, .. }
        | Self::Associate { subject, .. }
        | Self::Admit { subject, .. }) = self;
        subject
    }
}

/// How one statement is named to a human.
///
/// The author's own source text, whitespace-collapsed, plus the 1-based line it
/// opens on. Both observability surfaces need to name a statement — the
/// per-statement counters and the per-record trace — and a name each rendered
/// for itself would be two spellings of the language competing with the
/// grammar.
///
/// Sized by the catalog, not the corpus: one per authored statement, built once
/// at bind time. `patterns/sf-experimental.lx`, the only tracked catalog that
/// declares a block, contributes two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementLabel {
    text: CompactString,
    line: u32,
}

impl StatementLabel {
    /// Builds a label from a statement's source text and opening line.
    ///
    /// Runs of whitespace collapse to one space, so a statement broken across
    /// lines reads as one and the author's indentation reaches no reader.
    pub(crate) fn new(source_text: &str, line: u32) -> Self {
        Self {
            text: source_text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .into(),
            line,
        }
    }

    /// The statement as written, with runs of whitespace collapsed to one
    /// space.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The 1-based line the statement opens on, in the file that declared it.
    #[must_use]
    pub fn line(&self) -> u32 {
        self.line
    }
}

/// One catalog's bound policy program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundProgram {
    pub ops: Vec<BoundOp>,
    /// One label per entry in [`Self::ops`], at the same index.
    ///
    /// Parallel rather than a field on [`BoundOp`] because a label is a
    /// *presentation* fact and `BoundOp` is compared for equality by tests
    /// asserting about semantics. Folding the text in would make two programs
    /// that mean the same thing unequal because one was indented differently.
    ///
    /// Private, so the only way to build a program with a label is
    /// [`Self::new`], which is where the two lengths are checked.
    labels: Vec<StatementLabel>,
}

impl BoundProgram {
    /// A program and its statement labels, which must correspond by index.
    ///
    /// # Panics
    ///
    /// If the two lengths disagree. Every reader `zip`s them, and `zip`
    /// truncates silently — a reporting surface naming fewer statements than
    /// the catalog declares reads as a complete account of the run, which is
    /// the one failure this whole tier exists to prevent. Refusing to build
    /// such a program at all is cheaper than teaching each reader to notice.
    #[must_use]
    pub(crate) fn new(ops: Vec<BoundOp>, labels: Vec<StatementLabel>) -> Self {
        assert_eq!(
            ops.len(),
            labels.len(),
            "one statement label per bound operation"
        );
        Self { ops, labels }
    }

    /// Each statement's label, in authored order and indexed alike with
    /// [`Self::ops`].
    #[must_use]
    pub fn labels(&self) -> &[StatementLabel] {
        &self.labels
    }
}

/// A resolved subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundSubject {
    All,
    Patterns,
    Def(DefId),
    Group(GroupId),
}

/// A resolved operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundOperand {
    Def(DefId),
    Group(GroupId),
}

/// One bound operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundOp {
    Resolve {
        subject: BoundSubject,
        rank: Vec<Criterion>,
        /// `Some` iff the authored statement declared `partition:`. The rank
        /// contest runs independently per class; a candidate whose definition
        /// has no entry is not selected by this statement.
        ///
        /// [`Program::bind`] emits `None` — the map needs the catalog's
        /// metadata, which binding does not hold — and the compiler's
        /// `bind_policy_program` resolves it immediately after, before the
        /// program is installed. Nothing downstream observes the interim
        /// state.
        partition: Option<PartitionMap>,
        span: PolicySpan,
    },
    Associate {
        effect: Effect,
        subject: BoundSubject,
        operand: BoundOperand,
        relation: Relation,
        evidence: Evidence,
        span: PolicySpan,
    },
    Admit {
        subject: BoundSubject,
        terminal: Terminal,
        anchor: CompactString,
        span: PolicySpan,
    },
}

impl BoundOp {
    /// This operation's source span.
    #[must_use]
    pub fn span(&self) -> PolicySpan {
        let (Self::Resolve { span, .. } | Self::Associate { span, .. } | Self::Admit { span, .. }) =
            self;
        *span
    }

    /// This operation's subject.
    #[must_use]
    pub fn subject(&self) -> BoundSubject {
        let (Self::Resolve { subject, .. }
        | Self::Associate { subject, .. }
        | Self::Admit { subject, .. }) = self;
        *subject
    }
}

/// An unresolved name, carrying the statement that named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindError {
    pub name: CompactString,
    pub span: PolicySpan,
    pub kind: BindErrorKind,
}

/// Which namespace a [`BindError`]'s name failed to resolve in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindErrorKind {
    UnknownDefinition,
    UnknownGroup,
}

/// Group membership, keyed by [`GroupId`].
///
/// Built from the catalog's complete definition list so a [`DefId`] means the
/// same thing here as everywhere else. This type takes membership as given;
/// extraction decides how each definition acquires it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupIndex {
    names: Vec<CompactString>,
    members: Vec<Vec<DefId>>,
}

impl GroupIndex {
    /// An index declaring no groups.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Builds an index from the catalog's **complete** definition list.
    ///
    /// Each entry is a definition's groups, empty when it declares none. The
    /// slice index is the [`DefId`], which is what makes an ungrouped
    /// definition ahead of a grouped one harmless — filtering first would
    /// shift every later index down.
    ///
    /// A definition may name several groups: the set a `resolve` contests and
    /// the set a `detach` shapes have no reason to coincide.
    ///
    /// Groups are numbered in sorted-name order, so an id is **deterministic**
    /// and independent of which definition was read first — the same reason
    /// `synthesize_hidden_emitters` sorts. That is all the numbering gives:
    /// renaming `a` to `zz` moves it past `z` and changes its id, so
    /// invariance under consistent alpha-renaming is a property of the
    /// *engine* never deriving meaning from id order, and is tested end to end
    /// rather than claimed here.
    #[must_use]
    pub fn from_definition_groups(groups: &[Vec<CompactString>]) -> Self {
        let mut names: Vec<CompactString> = groups.iter().flatten().cloned().collect();
        names.sort_unstable();
        names.dedup();

        let mut members = vec![Vec::new(); names.len()];
        for (index, declared) in groups.iter().enumerate() {
            for group in declared {
                let position = names
                    .binary_search(group)
                    .expect("every declared group name is in the sorted set");
                members[position].push(DefId(index));
            }
        }

        Self { names, members }
    }

    /// Declares `name` with no members unless it already has some.
    ///
    /// For a derived `group` declaration whose predicate currently selects
    /// nothing: the group still exists — `%name` must resolve to it and draw
    /// the empty-subject warning rather than an unknown-group refusal claiming
    /// the catalog does not declare it. Membership lists can never encode an
    /// empty group, which is why this is a separate entry point rather than an
    /// input to [`Self::from_definition_groups`].
    ///
    /// Sorted-position insertion keeps ids deterministic; it is sound because
    /// every [`GroupId`] is issued by [`Self::id`] *after* installation, so no
    /// id exists yet for an insertion to shift.
    pub(crate) fn declare(&mut self, name: &CompactString) {
        if let Err(position) = self.names.binary_search(name) {
            self.names.insert(position, name.clone());
            self.members.insert(position, Vec::new());
        }
    }

    /// This group's identifier, if it is declared.
    #[must_use]
    pub fn id(&self, name: &str) -> Option<GroupId> {
        self.names
            .binary_search_by(|held| held.as_str().cmp(name))
            .ok()
            .map(GroupId)
    }

    /// Every definition in this group, by name.
    ///
    /// The public lookup. It takes a name rather than a [`GroupId`] because an
    /// id is only meaningful to the index that issued it, and one accepted
    /// from a different index would panic here.
    #[must_use]
    pub fn members_of(&self, name: &str) -> Option<&[DefId]> {
        self.id(name).map(|group| self.members(group))
    }

    /// Every definition in this group, ascending.
    ///
    /// # Panics
    ///
    /// If `group` was issued by a different [`GroupIndex`]. Crate-private for
    /// that reason: within the crate the issuing index is the one in hand.
    #[must_use]
    pub(crate) fn members(&self, group: GroupId) -> &[DefId] {
        &self.members[group.0]
    }

    /// This group's authored name.
    ///
    /// For diagnostics, which is the one thing that needs to go back the other
    /// way: a [`GroupId`] is what a bound program carries, and a warning about
    /// a group the author cannot name is a warning they cannot act on.
    ///
    /// # Panics
    ///
    /// If `group` was issued by a different [`GroupIndex`], as [`Self::members`]
    /// does and for the same reason.
    #[must_use]
    pub(crate) fn name(&self, group: GroupId) -> &str {
        &self.names[group.0]
    }

    /// Whether `subject` selects `definition`.
    #[must_use]
    pub(crate) fn selects(&self, subject: BoundSubject, definition: DefId) -> bool {
        match subject {
            BoundSubject::All => true,
            BoundSubject::Patterns => true, // narrowed by ExecContext at runtime
            BoundSubject::Def(id) => id == definition,
            BoundSubject::Group(group) => self.members(group).contains(&definition),
        }
    }

    /// Every definition an operand can select, ascending.
    ///
    /// A single definition yields one; a group yields its members.
    #[must_use]
    pub(crate) fn expand_operand(&self, operand: BoundOperand) -> Vec<DefId> {
        match operand {
            BoundOperand::Def(id) => vec![id],
            BoundOperand::Group(group) => self.members(group).to_vec(),
        }
    }

    /// Every definition a subject can select, ascending.
    ///
    /// `eligible` is the **subject domain**: the publishable emitting
    /// definitions, ascending. It is passed in rather than derived here
    /// because the two domains are not the same set and this index knows
    /// neither:
    ///
    /// * **Subject domain** — emitting definitions only. A fragment never
    ///   emits, and a synthesized `__policy__` emitter is stripped before the
    ///   candidate set exists, so neither can ever be a subject record.
    /// * **Operand domain** — patterns *and* fragments, the latter reached
    ///   through their evidence emitters under `evidence: raw`.
    ///
    /// A bare definition count would be worse than incomplete: it invites a
    /// caller to pass a number that disagrees with the list this index was
    /// built from, and it would make `all` manufacture obligations against
    /// definitions that can never be subjects.
    ///
    /// **The cumulative detach analysis must accumulate per `DefId`, never per
    /// [`BoundSubject`].** `all`, `%group` and `@definition` are three
    /// spellings that can select one definition, so aggregating obligations by
    /// subject identity would miss a program whose two detaches on opposite
    /// edges are spelled differently — `detach all … at_start` followed by
    /// `detach @a … at_end` targets `a` twice while looking like two unrelated
    /// subjects.
    #[must_use]
    pub(crate) fn expand_subject(&self, subject: BoundSubject, eligible: &[DefId]) -> Vec<DefId> {
        match subject {
            BoundSubject::All => eligible.to_vec(),
            BoundSubject::Patterns => eligible.to_vec(),
            BoundSubject::Def(id) => {
                if eligible.contains(&id) {
                    vec![id]
                } else {
                    Vec::new()
                }
            }
            BoundSubject::Group(group) => self
                .members(group)
                .iter()
                .filter(|member| eligible.contains(member))
                .copied()
                .collect(),
        }
    }
}

/// The catalog's definitions, as the engine looks them up.
///
/// Built from the **complete** definition list for the same reason
/// [`GroupIndex`] is: a [`DefId`] has to mean one definition everywhere, and an
/// index assigned over a filtered list designates a different one.
///
/// This answers the two questions the engine cannot answer from a record. A
/// record carries its authored name, and every subject, operand and
/// `priority_order` lookup is by [`DefId`], so the name has to be resolved
/// somewhere; and `all` needs the subject domain, which is a property of the
/// catalog rather than of the sentence.
///
/// It does **not** resolve `__policy__` aliases. A synthesized evidence emitter
/// is a definition in its own right and gets its own id here; mapping its
/// records back to the fragment they observe is the seam's job, because the
/// alias table lives on `CatalogPolicy` and this type takes no view of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefinitionDomain {
    /// Authored name to identifier. Insertion order is [`DefId`] order.
    ids: IndexMap<CompactString, DefId>,
    /// The subject domain, ascending.
    eligible: Vec<DefId>,
    patterns: Vec<DefId>,
}

impl DefinitionDomain {
    /// A domain declaring no definitions.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Builds the domain from the catalog's **complete** definition list.
    ///
    /// Each item is a definition's name and whether it can be a *subject*
    /// record: an emitting definition that is not a synthesized evidence
    /// emitter. Iteration order is [`DefId`] order, so the list must be
    /// complete and in declaration order.
    ///
    /// The eligibility flag is supplied rather than derived from the name,
    /// because visibility is the compiler's to know and a name test would make
    /// the hidden-emitter prefix load-bearing in a second place.
    pub fn new<'a>(definitions: impl IntoIterator<Item = (&'a str, bool, bool)>) -> Self {
        let mut ids = IndexMap::new();
        let mut eligible = Vec::new();
        let mut patterns = Vec::new();
        for (index, (name, subject_eligible, ordinary_pattern)) in
            definitions.into_iter().enumerate()
        {
            ids.insert(CompactString::new(name), DefId(index));
            if subject_eligible {
                eligible.push(DefId(index));
            }
            if ordinary_pattern {
                patterns.push(DefId(index));
            }
        }
        Self {
            ids,
            eligible,
            patterns,
        }
    }

    /// This definition's identifier, if the catalog declares it.
    #[must_use]
    pub fn id(&self, name: &str) -> Option<DefId> {
        self.ids.get(name).copied()
    }

    /// This definition's authored name.
    ///
    /// # Panics
    ///
    /// If `definition` was issued by a different [`DefinitionDomain`], for the
    /// same reason [`GroupIndex::members`] panics.
    #[must_use]
    pub(crate) fn name(&self, definition: DefId) -> &str {
        self.ids
            .get_index(definition.0)
            .map(|(name, _)| name.as_str())
            .expect("a DefId issued by this domain indexes it")
    }

    /// The subject domain: publishable emitting definitions, ascending.
    ///
    /// What `all` expands to. `GroupIndex`'s subject expansion documents why
    /// the subject and operand domains are not the same set.
    #[must_use]
    pub fn eligible(&self) -> &[DefId] {
        &self.eligible
    }

    /// The subject domain appropriate for this spelling.
    ///
    /// `patterns` is the one closed subject whose domain is narrower than
    /// `all`; keeping that choice here prevents catalog-final analyses from
    /// accidentally treating the two as synonyms once lexicons exist.
    #[must_use]
    pub(crate) fn eligible_for(&self, subject: BoundSubject) -> &[DefId] {
        if matches!(subject, BoundSubject::Patterns) {
            &self.patterns
        } else {
            &self.eligible
        }
    }

    #[must_use]
    pub fn patterns(&self) -> &[DefId] {
        &self.patterns
    }

    #[must_use]
    pub fn is_pattern(&self, definition: DefId) -> bool {
        self.patterns.contains(&definition)
    }
}

impl Program {
    /// Resolves every name against the catalog's definitions and groups.
    ///
    /// `definitions` is the **complete** definition list; a name's index is its
    /// [`DefId`]. Errors are collected rather than short-circuited so one
    /// compile reports every unresolved name.
    ///
    /// # Errors
    ///
    /// Every subject or operand naming no definition or no declared group.
    pub fn bind(
        &self,
        definitions: &[CompactString],
        groups: &GroupIndex,
    ) -> Result<BoundProgram, Vec<BindError>> {
        let mut errors = Vec::new();
        let mut ops = Vec::with_capacity(self.ops.len());
        // Pushed in lockstep with `ops` below, never afterwards from an index:
        // a statement that abandons binding pushes neither, so the two stay
        // aligned through every `continue` in the loop.
        let mut labels = Vec::with_capacity(self.ops.len());

        for (op, label) in self.ops.iter().zip(&self.labels) {
            let span = op.span();

            // Every name in the statement is resolved before any of them is
            // allowed to abandon it. Returning early on an unresolved subject
            // would hide an unresolved operand in the same statement, and the
            // author would fix one name only to be told about the next.
            let subject = bind_subject(op.subject(), span, definitions, groups, &mut errors);
            let operand = match op {
                Op::Associate { operand, .. } => Some(bind_operand(
                    operand,
                    span,
                    definitions,
                    groups,
                    &mut errors,
                )),
                Op::Resolve { .. } | Op::Admit { .. } => None,
            };

            let Some(subject) = subject else { continue };

            ops.push(match op {
                Op::Resolve { rank, .. } => BoundOp::Resolve {
                    subject,
                    rank: rank.clone(),
                    // Resolved by the caller from catalog metadata; see the
                    // field's own documentation.
                    partition: None,
                    span,
                },
                Op::Associate {
                    effect,
                    relation,
                    evidence,
                    ..
                } => {
                    let Some(Some(operand)) = operand else {
                        continue;
                    };
                    BoundOp::Associate {
                        effect: *effect,
                        subject,
                        operand,
                        relation: *relation,
                        evidence: *evidence,
                        span,
                    }
                }
                Op::Admit {
                    terminal, anchor, ..
                } => BoundOp::Admit {
                    subject,
                    terminal: *terminal,
                    anchor: anchor.clone(),
                    span,
                },
            });
            labels.push(label.clone());
        }

        if errors.is_empty() {
            Ok(BoundProgram::new(ops, labels))
        } else {
            Err(errors)
        }
    }
}

fn definition_id(
    name: &CompactString,
    span: PolicySpan,
    definitions: &[CompactString],
    errors: &mut Vec<BindError>,
) -> Option<DefId> {
    let found = definitions.iter().position(|held| held == name).map(DefId);
    if found.is_none() {
        errors.push(BindError {
            name: name.clone(),
            span,
            kind: BindErrorKind::UnknownDefinition,
        });
    }
    found
}

fn group_id(
    name: &CompactString,
    span: PolicySpan,
    groups: &GroupIndex,
    errors: &mut Vec<BindError>,
) -> Option<GroupId> {
    let found = groups.id(name);
    if found.is_none() {
        errors.push(BindError {
            name: name.clone(),
            span,
            kind: BindErrorKind::UnknownGroup,
        });
    }
    found
}

fn bind_subject(
    subject: &Subject,
    span: PolicySpan,
    definitions: &[CompactString],
    groups: &GroupIndex,
    errors: &mut Vec<BindError>,
) -> Option<BoundSubject> {
    match subject {
        Subject::All => Some(BoundSubject::All),
        Subject::Patterns => Some(BoundSubject::Patterns),
        Subject::Def(name) => definition_id(name, span, definitions, errors).map(BoundSubject::Def),
        Subject::Group(name) => group_id(name, span, groups, errors).map(BoundSubject::Group),
    }
}

fn bind_operand(
    operand: &Operand,
    span: PolicySpan,
    definitions: &[CompactString],
    groups: &GroupIndex,
    errors: &mut Vec<BindError>,
) -> Option<BoundOperand> {
    match operand {
        Operand::Def(name) => definition_id(name, span, definitions, errors).map(BoundOperand::Def),
        Operand::Group(name) => group_id(name, span, groups, errors).map(BoundOperand::Group),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::compiler::model::SourceId;

    fn span() -> PolicySpan {
        PolicySpan(SourceSpan::new(SourceId::new(0), 0..1).expect("a valid range"))
    }

    fn names(values: &[&str]) -> Vec<CompactString> {
        values
            .iter()
            .map(|value| CompactString::new(*value))
            .collect()
    }

    /// One definition per entry; `None` declares no group.
    ///
    /// Kept in the `Option` shape the tests were written against even though
    /// the index now takes a list, because none of them is *about* multiple
    /// membership — `multiple_membership` below covers that, and rewriting
    /// nine call sites to say `&[&["g"][..]]` would obscure what each is
    /// actually pinning.
    fn groups(values: &[Option<&str>]) -> Vec<Vec<CompactString>> {
        values
            .iter()
            .map(|value| value.map(CompactString::new).into_iter().collect())
            .collect()
    }

    fn total_rank() -> Vec<Criterion> {
        vec![
            Criterion::Longest,
            Criterion::Start,
            Criterion::End,
            Criterion::Pattern,
        ]
    }

    /// A program with one placeholder label per operation.
    ///
    /// The tests below are about *binding* — resolving names to ids — and a
    /// label is text the lowerer cut from a source these tests do not have.
    /// Supplying stubs keeps the one-label-per-op invariant satisfied without
    /// pretending these tests check it; `labels_and_operations_stay_index_aligned`
    /// in the integration suite checks it against a real catalog.
    fn authored(ops: Vec<Op>) -> Program {
        let labels = (0..ops.len())
            .map(|index| StatementLabel::new(&format!("statement {index}"), 1))
            .collect();
        Program { ops, labels }
    }

    /// An index built over the complete definition list, so an ungrouped
    /// definition ahead of a grouped one does not shift the grouped one down.
    ///
    /// Building from the *filtered* pairs — only definitions carrying a group —
    /// gives `grouped` index 0, and every selection then acts on `ungrouped`.
    #[test]
    fn a_def_id_survives_an_ungrouped_definition_ahead_of_it() {
        let index = GroupIndex::from_definition_groups(&groups(&[None, Some("g")]));
        let g = index.id("g").expect("`g` is declared");
        assert_eq!(
            index.members(g),
            &[DefId(1)],
            "the grouped definition is the second in the catalog"
        );
    }

    #[test]
    fn a_group_id_is_a_property_of_the_catalog_not_of_read_order() {
        let forwards = GroupIndex::from_definition_groups(&groups(&[Some("a"), Some("z")]));
        let backwards = GroupIndex::from_definition_groups(&groups(&[Some("z"), Some("a")]));
        assert_eq!(
            forwards.id("a").expect("declared"),
            backwards.id("a").expect("declared"),
            "sorted-name numbering, so declaration order cannot leak into an id"
        );
    }

    #[test]
    fn an_undeclared_group_has_no_id() {
        let index = GroupIndex::from_definition_groups(&groups(&[Some("g")]));
        assert!(index.id("absent").is_none());
    }

    #[test]
    fn binding_resolves_a_subject_to_its_definition_id() {
        let program = authored(vec![Op::Resolve {
            subject: Subject::Def(CompactString::new("b")),
            rank: total_rank(),
            partition: None,
            span: span(),
        }]);
        let bound = program
            .bind(&names(&["a", "b"]), &GroupIndex::empty())
            .expect("resolves");
        assert_eq!(bound.ops[0].subject(), BoundSubject::Def(DefId(1)));
    }

    #[test]
    fn binding_reports_an_unresolved_definition_rather_than_dropping_it() {
        let program = authored(vec![Op::Resolve {
            subject: Subject::Def(CompactString::new("missing")),
            rank: total_rank(),
            partition: None,
            span: span(),
        }]);
        let errors = program
            .bind(&names(&["a"]), &GroupIndex::empty())
            .expect_err("must not silently drop");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].name, "missing");
        assert_eq!(errors[0].kind, BindErrorKind::UnknownDefinition);
    }

    #[test]
    fn binding_reports_an_unresolved_group_in_the_group_namespace() {
        let program = authored(vec![Op::Resolve {
            subject: Subject::Group(CompactString::new("nope")),
            rank: total_rank(),
            partition: None,
            span: span(),
        }]);
        let errors = program
            .bind(&names(&["a"]), &GroupIndex::empty())
            .expect_err("must not silently drop");
        assert_eq!(errors[0].kind, BindErrorKind::UnknownGroup);
    }

    #[test]
    fn binding_collects_every_unresolved_name_in_one_pass() {
        let program = authored(vec![
            Op::Resolve {
                subject: Subject::Def(CompactString::new("one")),
                rank: total_rank(),
                partition: None,
                span: span(),
            },
            Op::Resolve {
                subject: Subject::Def(CompactString::new("two")),
                rank: total_rank(),
                partition: None,
                span: span(),
            },
        ]);
        let errors = program
            .bind(&names(&["a"]), &GroupIndex::empty())
            .expect_err("both are unresolved");
        assert_eq!(errors.len(), 2, "one compile reports every unresolved name");
    }

    #[test]
    fn an_operand_group_expands_to_its_members() {
        let index = GroupIndex::from_definition_groups(&groups(&[Some("g"), None, Some("g")]));
        let g = index.id("g").expect("declared");
        assert_eq!(
            index.expand_operand(BoundOperand::Group(g)),
            vec![DefId(0), DefId(2)],
            "expansion is what the cumulative detach analysis reasons over"
        );
    }

    #[test]
    fn a_subject_selects_exactly_what_it_names() {
        let index = GroupIndex::from_definition_groups(&groups(&[Some("g"), None]));
        let g = index.id("g").expect("declared");
        assert!(index.selects(BoundSubject::All, DefId(1)));
        assert!(index.selects(BoundSubject::Group(g), DefId(0)));
        assert!(
            !index.selects(BoundSubject::Group(g), DefId(1)),
            "an ungrouped definition is not in any group"
        );
        assert!(index.selects(BoundSubject::Def(DefId(1)), DefId(1)));
        assert!(!index.selects(BoundSubject::Def(DefId(1)), DefId(0)));
    }

    /// One statement naming two missing things reports both.
    ///
    /// The aggregation test above uses two separate `Resolve` operations, so
    /// it never exercises this path: an early return on the subject hides the
    /// operand in the *same* statement, and the author fixes one name only to
    /// be told about the next.
    #[test]
    fn one_statement_reports_its_unresolved_subject_and_operand_together() {
        let program = authored(vec![Op::Associate {
            effect: Effect::Exclude,
            subject: Subject::Def(CompactString::new("no_subject")),
            operand: Operand::Def(CompactString::new("no_operand")),
            relation: Relation::After,
            evidence: Evidence::Raw,
            span: span(),
        }]);
        let errors = program
            .bind(&names(&["a"]), &GroupIndex::empty())
            .expect_err("both names are unresolved");
        let reported: Vec<&str> = errors.iter().map(|error| error.name.as_str()).collect();
        assert!(reported.contains(&"no_subject"), "{reported:?}");
        assert!(reported.contains(&"no_operand"), "{reported:?}");
    }

    /// `all`, `%group` and `@definition` can select one definition, so the
    /// cumulative detach analysis must accumulate per `DefId`.
    #[test]
    fn three_subject_spellings_can_select_the_same_definition() {
        // DefId(0) is an emitting pattern in group `g`; DefId(1) is ungrouped.
        let index = GroupIndex::from_definition_groups(&groups(&[Some("g"), None]));
        let g = index.id("g").expect("declared");
        let eligible = [DefId(0), DefId(1)];

        assert!(
            index
                .expand_subject(BoundSubject::All, &eligible)
                .contains(&DefId(0))
        );
        assert!(
            index
                .expand_subject(BoundSubject::Group(g), &eligible)
                .contains(&DefId(0))
        );
        assert!(
            index
                .expand_subject(BoundSubject::Def(DefId(0)), &eligible)
                .contains(&DefId(0))
        );
    }

    /// The subject and operand domains are different sets.
    ///
    /// `DefId(1)` is a fragment: it never emits, so no record of it can ever
    /// be a subject — but a `%group` naming it *as an operand* reaches it
    /// through its synthesized evidence emitter. A static analysis that used
    /// one domain for both would impose detach obligations on a definition
    /// that can never be detached.
    #[test]
    fn all_excludes_a_fragment_that_a_group_operand_still_reaches() {
        let index = GroupIndex::from_definition_groups(&groups(&[Some("g"), Some("g")]));
        let g = index.id("g").expect("declared");
        let eligible = [DefId(0)]; // only the emitting pattern

        assert_eq!(
            index.expand_subject(BoundSubject::All, &eligible),
            vec![DefId(0)],
            "`all` ranges over subject records, and a fragment has none"
        );
        assert_eq!(
            index.expand_subject(BoundSubject::Group(g), &eligible),
            vec![DefId(0)],
            "a group used as a subject is filtered to the subject domain too"
        );
        assert_eq!(
            index.expand_operand(BoundOperand::Group(g)),
            vec![DefId(0), DefId(1)],
            "the same group used as an operand still reaches the fragment"
        );
    }

    /// A span carries source identity, not just an offset range.
    ///
    /// Two spans with equal byte ranges in different sources must differ, or
    /// `FsmBuilder`'s source remapping is unobservable and a test asserting a
    /// bare offset cannot tell two documents apart.
    #[test]
    fn a_span_distinguishes_two_sources_at_the_same_offset() {
        let first = PolicySpan(SourceSpan::new(SourceId::new(0), 0..4).expect("valid"));
        let second = PolicySpan(SourceSpan::new(SourceId::new(1), 0..4).expect("valid"));
        assert_ne!(first, second);
    }
}
