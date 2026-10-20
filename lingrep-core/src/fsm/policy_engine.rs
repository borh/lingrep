//! Execution of the ordered policy program.
//!
//! # Where this runs
//!
//! `apply_policy_records` is four steps: synthesized evidence is stripped, the
//! program runs, spans are shaped, and names are rewritten. The program is the
//! whole of suppression and admission — the three key-driven passes that used
//! to precede it retired with the keys that drove them, so its input is the
//! hidden-stripped canonical set and every ordering argument here reduces to
//! the authored order alone.
//!
//! It runs **before** shaping and relabeling, and that is not an arrangement of
//! convenience. Every subject, operand and `priority_order` lookup keys on the
//! **authored** name, which `PublishedRecord.raw.pattern` carries and
//! `record.name` stops carrying the moment `output_name` is applied. Running
//! after relabeling would make `@a` select nothing in a catalog that renames
//! `a`.
//!
//! # Identity and geometry
//!
//! A candidate's identity is [`RawId`], its index in the sentence's canonical
//! raw slice. It survives the snapshot clones the decide phase takes, which a
//! position in the shrinking survivor list would not, and it survives
//! relabeling, which the published name would not.
//!
//! Relations are evaluated on **raw** spans in every case. A
//! candidate's public span is what `detach` narrows and what shaping goes on to
//! project; it is never what a later operation measures against. `evidence`
//! changes which matches are operands, never the geometry they are compared by.
//!
//! # Phasing
//!
//! Each operation decides against a snapshot taken at its own entry and applies
//! afterwards, so no record's fate depends on where it sits in the candidate
//! list. `resolve` is why [`decide_operation`] returns one [`Decision`] per
//! snapshot entry rather than judging a record at a time: a contest has to see
//! the whole field before it can name a winner.
//!
//! # Observability
//!
//! Three questions that look alike, answered in three different places, because
//! only one of them is a fact about a run:
//!
//! * A statement that can *never* affect a record — its subject selects no
//!   publishable definition — is a **compile-time warning**
//!   (`DiagnosticCode::IneffectivePolicy`). No run is needed, and none would be
//!   more convincing.
//! * A statement that *could* affect a record and never does on this corpus is
//!   what [`ProgramStats`] is for, reported per statement in authored order.
//!   That is a fact about the text, so only a run can report it.
//! * *Why this record* is shaped as it is, or absent, is [`PolicyTrace`] —
//!   opt-in and capacity-bounded, because the honest answer is per record and
//!   the number of records is the corpus.
//!
//! [`ProgramStats`] is caller-owned: `apply_policy_records` takes one,
//! `PolicySink` accumulates one for a whole run, and `apply_policy` discards
//! one because a projection to spans and names has nowhere to put it.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use compact_str::CompactString;
use indexmap::IndexMap;

use super::policy::Terminal;
use super::policy_program::{
    BoundOp, BoundOperand, BoundSubject, Criterion, DefId, Effect, Evidence, GroupIndex,
    PartitionMap, RawId, Relation,
};
use super::policy_trace::{PolicyTrace, TracedOutcome};
use super::types::Match;

/// What an operation does to one candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Keep,
    Drop,
    /// Replace the public span. Raw geometry is untouched.
    Narrow(usize, usize),
}

/// A surviving record. `raw` is the identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub raw: RawId,
    pub definition: DefId,
    pub public_start: usize,
    pub public_end: usize,
}

/// Everything a decision needs that is not the operation or the snapshot.
///
/// The two lookups are closures rather than borrowed tables because their
/// sources differ: `definition_of` composes the catalog's definition domain
/// with the `__policy__` alias table, and `priority_of` reaches an entry map
/// keyed by name. Threading both tables plus the rule for combining them would
/// put the seam's knowledge here, where nothing else needs it.
pub(crate) struct ExecContext<'a> {
    pub groups: &'a GroupIndex,
    /// Authored definition name to [`DefId`], **including** each
    /// `__policy__<fragment>` alias mapping to the fragment it observes.
    pub definition_of: &'a dyn Fn(&str) -> Option<DefId>,
    pub is_pattern: &'a dyn Fn(DefId) -> bool,
    /// A definition's `priority_order`, defaulting to 0.
    pub priority_of: &'a dyn Fn(DefId) -> i64,
    /// Every anchor position the seam resolved for this sentence.
    ///
    /// Positions, not an `AnchorPositions`: the seam resolves every anchor an
    /// `admit` names before it judges any record, so by the time an operation
    /// runs there is nothing left to fail. Handing the engine the accessor
    /// instead would put a second, redundant failure path here — and one that
    /// could report an unresolved anchor *after* a statistic had moved, which
    /// is exactly what resolving up front prevents.
    pub anchors: &'a BTreeMap<&'a str, usize>,
}

/// Per-operation counters for one program.
///
/// One slot per authored operation, allocated once and accumulated across
/// sentences. One entry per (sentence, operation) would grow without bound on
/// a corpus run, which is the shape a researcher asking "which statement did
/// the work" actually wants aggregated anyway.
///
/// Separate from `PolicyStats`, which is `Copy` and would stop being so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramStats {
    per_operation: Vec<OperationCounters>,
}

/// What one operation did, summed over every sentence it ran on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperationCounters {
    pub before: u64,
    pub after: u64,
    pub dropped: u64,
    /// How many public spans this operation narrowed. Only `detach` does.
    pub narrowed: u64,
    /// How many captures those narrowings dropped.
    pub captures_dropped: u64,
}

impl ProgramStats {
    /// Counters sized for `program`, all zero.
    ///
    /// Sized from the program rather than grown on demand, so an operation that
    /// never fired reports zeros instead of being absent — "ran and did
    /// nothing" and "was never reached" are the same observation only if the
    /// slot is missing.
    #[must_use]
    pub fn for_program(program: &super::policy_program::BoundProgram) -> Self {
        Self {
            per_operation: vec![OperationCounters::default(); program.ops.len()],
        }
    }

    /// Each operation's counters, in authored order.
    #[must_use]
    pub fn per_operation(&self) -> &[OperationCounters] {
        &self.per_operation
    }

    pub(crate) fn merge_from(&mut self, other: Self) {
        debug_assert_eq!(self.per_operation.len(), other.per_operation.len());
        for (total, sentence) in self.per_operation.iter_mut().zip(other.per_operation) {
            total.before += sentence.before;
            total.after += sentence.after;
            total.dropped += sentence.dropped;
            total.narrowed += sentence.narrowed;
            total.captures_dropped += sentence.captures_dropped;
        }
    }

    /// Records one operation's candidate count before and after it applied.
    ///
    /// A no-op when `index` is out of range, which happens only if the counters
    /// were built for a different program. Panicking would turn a reporting
    /// mismatch into a failed run.
    pub(crate) fn record(&mut self, index: usize, before: usize, after: usize) {
        let Some(counters) = self.per_operation.get_mut(index) else {
            return;
        };
        counters.before += before as u64;
        counters.after += after as u64;
        counters.dropped += before.saturating_sub(after) as u64;
    }

    /// Records what one operation reshaped, before its decisions are applied.
    ///
    /// Separate from [`Self::record`] because it needs the decisions themselves
    /// rather than two counts, and because a narrow is the one decision that
    /// leaves the candidate count alone — counting reshapes from `before` and
    /// `after` is not possible even in principle.
    ///
    /// A capture is counted when it was inside the candidate's **current**
    /// public span and falls outside the new one. Counting against the raw span
    /// instead would charge this operation for what an earlier detach already
    /// removed.
    pub(crate) fn record_reshape(
        &mut self,
        index: usize,
        snapshot: &[Candidate],
        decisions: &[Decision],
        raw: &[Match],
    ) {
        let Some(counters) = self.per_operation.get_mut(index) else {
            return;
        };
        for (candidate, decision) in snapshot.iter().zip(decisions) {
            let Decision::Narrow(start, end) = *decision else {
                continue;
            };
            counters.narrowed += 1;
            counters.captures_dropped += u64::from(captures_dropped_by(candidate, start, end, raw));
        }
    }
}

/// How many of a candidate's captures one narrowing removes.
///
/// A capture counts when it was inside the candidate's **current** public span
/// and falls outside the new one. Counting against the raw span instead would
/// charge this narrowing for what an earlier `detach` already removed.
///
/// A free function because two readers need the same answer — the per-statement
/// counters and the per-record trace — and a second implementation of "which
/// captures did this narrowing drop" could disagree with the first about the
/// boundary while both looked right.
fn captures_dropped_by(candidate: &Candidate, start: usize, end: usize, raw: &[Match]) -> u32 {
    let within = |capture: &crate::fsm::types::Capture, start: usize, end: usize| {
        start <= capture.start && capture.end <= end
    };
    u32::try_from(
        raw[candidate.raw.index()]
            .captures
            .iter()
            .filter(|capture| {
                within(capture, candidate.public_start, candidate.public_end)
                    && !within(capture, start, end)
            })
            .count(),
    )
    .unwrap_or(u32::MAX)
}

/// The operand set for one subject candidate.
///
/// * **operand restriction** — without it any adjacent or edge-coincident match
///   of any definition triggers the operation;
/// * **self-exclusion** — a definition used as both subject and operand would
///   otherwise select its own record and annihilate all of them.
///
/// Defining them once here is why `detach`, which does not use a `holds`
/// helper, cannot lose self-exclusion the way an earlier draft did.
fn operands_for(
    operand: BoundOperand,
    evidence: Evidence,
    raw: &[Match],
    snapshot: &[Candidate],
    subject: RawId,
    context: &ExecContext<'_>,
) -> Vec<RawId> {
    let pool: Vec<RawId> = match evidence {
        Evidence::Raw => (0..raw.len()).map(RawId).collect(),
        Evidence::Current => snapshot.iter().map(|candidate| candidate.raw).collect(),
    };
    let wanted = context.groups.expand_operand(operand);
    pool.into_iter()
        .filter(|id| *id != subject)
        .filter(|id| {
            (context.definition_of)(&raw[id.index()].pattern)
                .is_some_and(|definition| wanted.contains(&definition))
        })
        .collect()
}

/// One decision per snapshot entry, in snapshot order.
///
/// The shape is what lets `resolve` be a set-wide contest; a per-record
/// function could not express one.
///
/// Private: [`run_program`] is the only caller and the only thing that knows a
/// decision list has to be applied before the next operation sees it.
fn decide_operation(
    op: &BoundOp,
    raw: &[Match],
    snapshot: &[Candidate],
    context: &ExecContext<'_>,
) -> Vec<Decision> {
    match op {
        BoundOp::Resolve {
            subject,
            rank,
            partition,
            ..
        } => decide_resolve(
            &selected(*subject, snapshot, context),
            snapshot,
            raw,
            rank,
            partition.as_ref(),
            context,
        ),
        BoundOp::Associate {
            effect,
            subject,
            operand,
            relation,
            evidence,
            ..
        } => decide_associate(
            *effect, *subject, *operand, *relation, *evidence, raw, snapshot, context,
        ),
        BoundOp::Admit {
            subject,
            terminal,
            anchor,
            ..
        } => decide_admit(*subject, *terminal, anchor, snapshot, context),
    }
}

/// Decides an admission against a declared anchor.
///
/// Reads the candidate's **published** end, which is the one place in the
/// engine that does. Every relation is located on raw spans because a
/// relation asks where two records sit relative to each other; admission asks
/// where *this* record ends, and what it ends at is what the catalog publishes.
/// A `detach` earlier in the program is therefore visible here, and the same
/// two statements in the other order mean something else — which is what makes
/// authored order semantic rather than decorative.
///
/// The distance comparison is [`Terminal::admits`], and it is **one-sided**: a
/// record ending before the anchor is admitted at the declared distance, and one
/// ending after it is not admitted at all. That asymmetry is the type's, not
/// this function's, so a second comparison written here could disagree with it
/// about the boundary.
fn decide_admit(
    subject: BoundSubject,
    terminal: Terminal,
    anchor: &str,
    snapshot: &[Candidate],
    context: &ExecContext<'_>,
) -> Vec<Decision> {
    // Total, not merely unlikely to fail: the seam resolves every anchor named
    // by an `Admit` in this same program before it judges any record, and
    // `CatalogPolicy::has_anchored_terminal` is true whenever one exists, so
    // the loop that fills this map is never the one that was skipped.
    let position = *context
        .anchors
        .get(anchor)
        .expect("resolved at the seam from this program's own operations");
    let mut decisions = vec![Decision::Keep; snapshot.len()];
    for position_in_snapshot in selected(subject, snapshot, context) {
        if !terminal.admits(snapshot[position_in_snapshot].public_end, position) {
            decisions[position_in_snapshot] = Decision::Drop;
        }
    }
    decisions
}

/// The snapshot positions a subject selects.
///
/// Positions rather than [`RawId`]s, because a decision list is indexed by
/// snapshot position and the two are equal only before the first drop.
///
/// `all` is `GroupIndex::selects`'s unconditional arm rather than a membership
/// test against the subject domain. The two agree here and only here: a
/// candidate exists because some definition emitted a record for it, and pass 1
/// already removed the synthesized emitters, so every candidate's definition is
/// subject-eligible. That is also why `ExecContext` does not carry the domain:
/// the compile-time detach analysis has no such guarantee and expands against
/// `DefinitionDomain::eligible` explicitly, but nothing at run time needs it.
fn selected(
    subject: BoundSubject,
    snapshot: &[Candidate],
    context: &ExecContext<'_>,
) -> Vec<usize> {
    snapshot
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            context.groups.selects(subject, candidate.definition)
                && (!matches!(subject, BoundSubject::Patterns)
                    || (context.is_pattern)(candidate.definition))
        })
        .map(|(position, _)| position)
        .collect()
}

/// Decides an association, one subject record at a time.
///
/// Unlike a contest, this is genuinely per-record: whether an operand stands in
/// the relation to *this* record answers for that record alone. The set-wide
/// signature is what `resolve` needs, not what every verb needs.
///
fn decide_associate(
    effect: Effect,
    subject: BoundSubject,
    operand: BoundOperand,
    relation: Relation,
    evidence: Evidence,
    raw: &[Match],
    snapshot: &[Candidate],
    context: &ExecContext<'_>,
) -> Vec<Decision> {
    let mut decisions = vec![Decision::Keep; snapshot.len()];
    for position in selected(subject, snapshot, context) {
        let candidate = &snapshot[position];
        let operands = operands_for(operand, evidence, raw, snapshot, candidate.raw, context);
        let subject_raw = &raw[candidate.raw.index()];
        decisions[position] = match effect {
            // `require` is `exclude`'s complement over the same operand set,
            // which is why they share every filter above rather than each
            // restating one.
            Effect::Exclude | Effect::Require => {
                let held = holds(relation, subject_raw, &operands, raw);
                if held == (effect == Effect::Exclude) {
                    Decision::Drop
                } else {
                    Decision::Keep
                }
            }
            Effect::Detach => decide_detach(relation, subject_raw, candidate, &operands, raw),
        };
    }
    decisions
}

/// Decides a detach.
///
/// `K`'s start bound is **strict**. An operand covering the subject's entire
/// span is therefore not in `K`, the detach does not fire, and the record
/// publishes whole. This is not a clamp: `detach` says *this trailing region
/// belongs to another record*, and a region that is the entire record is not a
/// trailing region.
///
/// It is also what makes a single-edge detach total and non-empty by
/// construction. With `>=` the compiler would need a width proof, and that
/// proof rejects 11 of the 13 sites the shipped catalog migrates — see
/// `docs/design/2026-08-06-policy-sublanguage-syntax.md` §3.
///
/// The region is located against the **raw** span and then applied to the
/// **public** one. Those differ once an earlier detach has narrowed the record,
/// and taking the extremum of the two is what keeps a public span monotone: a
/// later, shorter operand cannot give back what an earlier one removed.
fn decide_detach(
    relation: Relation,
    subject: &Match,
    candidate: &Candidate,
    operands: &[RawId],
    raw: &[Match],
) -> Decision {
    let regions = operands.iter().map(|id| &raw[id.index()]);
    match relation {
        Relation::AtEnd => regions
            .filter(|operand| operand.end == subject.end && operand.start > subject.start)
            .map(|operand| operand.start)
            .min()
            .map_or(Decision::Keep, |start| {
                Decision::Narrow(candidate.public_start, candidate.public_end.min(start))
            }),
        Relation::AtStart => regions
            .filter(|operand| operand.start == subject.start && operand.end < subject.end)
            .map(|operand| operand.end)
            .max()
            .map_or(Decision::Keep, |end| {
                Decision::Narrow(candidate.public_start.max(end), candidate.public_end)
            }),
        // Refused at compile time: `detach` removes a region at an edge, and
        // the other four relations name no such region.
        Relation::After | Relation::Before | Relation::Within | Relation::InSentence => {
            Decision::Keep
        }
    }
}

/// Whether any operand stands in `relation` to `subject`.
///
/// Self-exclusion is **not** here — it lives in [`operands_for`], which is the
/// whole point of defining it once. Evaluated on raw spans in every case
///; `evidence` changed the membership of `operands`, never geometry.
///
/// `in_sentence` is existential, not vacuous: it asks whether the operand set
/// is non-empty, so an operand that matched nothing holds for nobody. That is
/// what makes `require … in_sentence` a presence test rather than a no-op.
fn holds(relation: Relation, subject: &Match, operands: &[RawId], raw: &[Match]) -> bool {
    operands.iter().any(|id| {
        let operand = &raw[id.index()];
        match relation {
            Relation::AtStart => operand.start == subject.start,
            Relation::AtEnd => operand.end == subject.end,
            Relation::After => operand.start == subject.end,
            Relation::Before => operand.end == subject.start,
            Relation::Within => operand.start < subject.end && subject.start < operand.end,
            Relation::InSentence => true,
        }
    })
}

/// Decides a resolve contest over the selected subject set.
///
/// Sort `S` by `rank` over **raw** spans, walk in that order keeping a
/// candidate unless it overlaps one already kept. The caller applies in
/// snapshot order, which is why the rank never reaches publication order.
///
/// Adjudication is against the **kept** set, not against every loser, matching
/// what the legacy priority-group pass does: a chain A–B–C where A overlaps B
/// and B overlaps C but A does not overlap C publishes both A and C, because B
/// loses to A and then shields nothing.
///
/// A record the subject does not select starts at `Keep` and is never revisited
/// — it is not in the contest, so it neither wins nor loses one.
///
/// A `partition` subdivides the selected set by each record's class — the
/// compile-time [`PartitionMap`] entry for its definition's authored name —
/// and runs the same contest independently per class, so records of different
/// classes never contest and a class's own contest is exactly what an
/// unpartitioned resolve over that class would decide. A selected record whose
/// definition has **no** entry joins no class: it is not selected by this
/// statement at all, and keeps its `Keep` the way an unselected record does.
fn decide_resolve(
    selected: &[usize],
    snapshot: &[Candidate],
    raw: &[Match],
    rank: &[Criterion],
    partition: Option<&PartitionMap>,
    context: &ExecContext<'_>,
) -> Vec<Decision> {
    let mut decisions = vec![Decision::Keep; snapshot.len()];
    match partition {
        None => contest(&mut decisions, selected, snapshot, raw, rank, context),
        Some(classes) => {
            // A `BTreeMap` makes class order a property of class names rather
            // than snapshot order.
            let mut by_class: BTreeMap<&CompactString, Vec<usize>> = BTreeMap::new();
            for &position in selected {
                if let Some(class) = partition_class_of(classes, &snapshot[position], raw) {
                    by_class.entry(class).or_default().push(position);
                }
            }
            for members in by_class.values() {
                contest(&mut decisions, members, snapshot, raw, rank, context);
            }
        }
    }
    decisions
}

/// The class a candidate belongs to under a partitioned resolve, if any.
///
/// The one place the classing predicate is written: [`decide_resolve`] groups
/// contestants by it and [`observe_operation`] asks it to tell `kept` from
/// `not selected`, and two spellings of the lookup could disagree about which
/// name is looked up. The key is the raw record's **authored** `pattern` name,
/// which policy runs early enough to still carry — `output_name` rewrites it
/// only in a later pass — and which is how the compile-time [`PartitionMap`]
/// is keyed.
fn partition_class_of<'a>(
    classes: &'a PartitionMap,
    candidate: &Candidate,
    raw: &[Match],
) -> Option<&'a CompactString> {
    classes.get(raw[candidate.raw.index()].pattern.as_str())
}

/// One resolve contest over one selected set, written into `decisions`.
///
/// Sort by `rank` over raw spans, walk in that order keeping a candidate
/// unless it overlaps one already kept — the contest documented on
/// [`decide_resolve`], factored out so the partitioned and unpartitioned forms
/// run one implementation. The kept set is local, which is what makes two
/// classes' contests independent.
fn contest(
    decisions: &mut [Decision],
    selected: &[usize],
    snapshot: &[Candidate],
    raw: &[Match],
    rank: &[Criterion],
    context: &ExecContext<'_>,
) {
    let mut order: Vec<usize> = selected.to_vec();
    order.sort_by(|&left, &right| {
        compare_by_rank(&snapshot[left], &snapshot[right], raw, rank, context)
    });

    // Every contestant loses by default, so a record that is never reached by
    // the walk below cannot survive on the strength of not having been judged.
    for position in &order {
        decisions[*position] = Decision::Drop;
    }

    let mut kept: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0;
    while cursor < order.len() {
        let cohort_end = (cursor + 1..order.len())
            .find(|&next| {
                compare_by_rank(
                    &snapshot[order[cursor]],
                    &snapshot[order[next]],
                    raw,
                    rank,
                    context,
                ) != std::cmp::Ordering::Equal
            })
            .unwrap_or(order.len());
        let candidate = &raw[snapshot[order[cursor]].raw.index()];
        let overlaps = kept
            .iter()
            .any(|&(start, end)| candidate.start < end && start < candidate.end);
        if !overlaps {
            kept.push((candidate.start, candidate.end));
            for &position in &order[cursor..cohort_end] {
                decisions[position] = Decision::Keep;
            }
        }
        cursor = cohort_end;
    }
}

/// Orders two contestants by the authored rank, first deciding criterion wins.
///
/// Everything is read from the **raw** span. A `detach` earlier in the
/// program narrowed the public span, and ranking by that would make a contest's
/// outcome depend on how much of a record was given away rather than on what it
/// matched.
///
/// Total on rank cohorts when the rank ends `[…, Start, End, Pattern]`.
/// Enhanced graph assignments can deliberately share those surface fields;
/// they tie and survive or lose together rather than being selected by
/// producer order. The property test below is the standing proof.
fn compare_by_rank(
    left: &Candidate,
    right: &Candidate,
    raw: &[Match],
    rank: &[Criterion],
    context: &ExecContext<'_>,
) -> Ordering {
    let (left_raw, right_raw) = (&raw[left.raw.index()], &raw[right.raw.index()]);
    let width = |record: &Match| record.end - record.start;
    for criterion in rank {
        let ordering = match criterion {
            Criterion::Longest => width(right_raw).cmp(&width(left_raw)),
            Criterion::Shortest => width(left_raw).cmp(&width(right_raw)),
            // Lower wins, matching `priority_order`'s documented sense.
            Criterion::PriorityOrder => {
                (context.priority_of)(left.definition).cmp(&(context.priority_of)(right.definition))
            }
            Criterion::Start => left_raw.start.cmp(&right_raw.start),
            Criterion::End => left_raw.end.cmp(&right_raw.end),
            Criterion::Pattern => left_raw.pattern.cmp(&right_raw.pattern),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

/// Runs every operation in authored order over the candidate set.
///
/// Order is semantic: a `resolve` before an `exclude` contests a
/// different field than one after it, and no reordering here is a permitted
/// optimization.
///
/// Infallible, and that is a property of the seam rather than of the verbs:
/// the one question an operation can fail to answer is where an anchor sits,
/// and the caller has answered it for every `admit` before calling.
pub(crate) fn run_program(
    ops: &[BoundOp],
    raw: &[Match],
    current: Vec<Candidate>,
    context: &ExecContext<'_>,
    stats: &mut ProgramStats,
    mut trace: Option<&mut PolicyTrace>,
) -> Vec<Candidate> {
    let mut current = current;
    // One open trace record per candidate, indexed alike with the first
    // snapshot. `None` for a candidate past the trace's capacity, so a bounded
    // trace stops collecting without changing which records are judged.
    let mut traced: Vec<Option<usize>> = match trace.as_deref_mut() {
        Some(trace) => current
            .iter()
            .map(|candidate| {
                let record = &raw[candidate.raw.index()];
                trace.open(&record.pattern, record.start, record.end)
            })
            .collect(),
        None => Vec::new(),
    };

    for (index, op) in ops.iter().enumerate() {
        // The snapshot is what the whole operation decides against, so it is
        // taken before any decision is applied. Deciding and applying in one
        // pass would let an early drop change what a later record in the same
        // operation sees.
        let snapshot = current.clone();
        let decisions = decide_operation(op, raw, &snapshot, context);
        debug_assert_eq!(
            decisions.len(),
            snapshot.len(),
            "one decision per snapshot entry"
        );

        if let Some(trace) = trace.as_deref_mut() {
            observe_operation(
                trace, &traced, index, op, &snapshot, &decisions, raw, context,
            );
        }

        let before = current.len();
        stats.record_reshape(index, &snapshot, &decisions, raw);
        let next = apply_decisions(snapshot, &decisions);
        // A dropped candidate leaves the survivor list, and its open trace
        // record leaves `traced` with it. That is what makes a dropped record's
        // entries stop at the statement that dropped it.
        if trace.is_some() {
            traced = traced
                .into_iter()
                .zip(&decisions)
                .filter(|(_, decision)| !matches!(decision, Decision::Drop))
                .map(|(record, _)| record)
                .collect();
        }
        stats.record(index, before, next.len());
        current = next;
    }
    current
}

/// Records what one operation did to each candidate still being traced.
///
/// Selection is asked separately from the decision because the two are not the
/// same fact: a statement that did not select a record and one that selected it
/// and kept it both return [`Decision::Keep`], and only the first answers "why
/// is this record unchanged".
#[allow(clippy::too_many_arguments)]
fn observe_operation(
    trace: &mut PolicyTrace,
    traced: &[Option<usize>],
    index: usize,
    op: &BoundOp,
    snapshot: &[Candidate],
    decisions: &[Decision],
    raw: &[Match],
    context: &ExecContext<'_>,
) {
    let mut selected_positions = selected(op.subject(), snapshot, context);
    // A partitioned resolve selects only records whose definition carries the
    // partition key: one without an entry is in no class and no contest, and
    // reporting it `kept` would claim a contest it never entered.
    if let BoundOp::Resolve {
        partition: Some(classes),
        ..
    } = op
    {
        selected_positions
            .retain(|&position| partition_class_of(classes, &snapshot[position], raw).is_some());
    }
    for (position, (record, decision)) in traced.iter().zip(decisions).enumerate() {
        let Some(record) = *record else { continue };
        let outcome = match *decision {
            Decision::Narrow(start, end) => TracedOutcome::Narrowed {
                start,
                end,
                captures_dropped: captures_dropped_by(&snapshot[position], start, end, raw),
            },
            Decision::Drop => TracedOutcome::Dropped,
            Decision::Keep if selected_positions.contains(&position) => TracedOutcome::Kept,
            Decision::Keep => TracedOutcome::NotSelected,
        };
        trace.observe(record, index, outcome);
    }
}

/// The candidate set one operation's decisions leave behind.
///
/// Order is preserved, so the survivors stay in canonical order without being
/// re-sorted. `Narrow` rewrites the public span only: `raw` is the record's
/// identity and `definition` is what policy keys on, and an operation that
/// changed either would be renaming a record rather than reshaping one.
fn apply_decisions(snapshot: Vec<Candidate>, decisions: &[Decision]) -> Vec<Candidate> {
    snapshot
        .into_iter()
        .zip(decisions)
        .filter_map(|(candidate, decision)| match *decision {
            Decision::Keep => Some(candidate),
            Decision::Drop => None,
            Decision::Narrow(start, end) => Some(Candidate {
                public_start: start,
                public_end: end,
                ..candidate
            }),
        })
        .collect()
}

/// Resolves a record's authored name to the definition policy keys on.
///
/// A synthesized `__policy__<fragment>` emitter resolves to the **fragment**,
/// not to itself. Its records exist only so a fragment operand has something to
/// match; an association naming `@N` binds to `N`'s [`DefId`], so an emitter
/// answering with its own id would make every association over a fragment hold
/// for nothing — silently, since the emitter is stripped before publication and
/// nothing downstream could show the mismatch.
pub(crate) fn definition_of(
    name: &str,
    domain: &super::policy_program::DefinitionDomain,
    hidden: &IndexMap<CompactString, CompactString>,
) -> Option<DefId> {
    match hidden.get(name) {
        Some(observed) => domain.id(observed),
        None => domain.id(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::policy_program::{BoundOperand, DefinitionDomain, GroupId};

    /// Four definitions, ids 0 to 3, all subject-eligible.
    fn domain() -> DefinitionDomain {
        DefinitionDomain::new([
            ("a", true, true),
            ("b", true, true),
            ("N", false, false),
            ("__policy__N", false, false),
        ])
    }

    /// `a` and `b` share the group `g`; `N` and its emitter join none.
    fn groups() -> GroupIndex {
        GroupIndex::from_definition_groups(&[
            vec![CompactString::new("g")],
            vec![CompactString::new("g")],
            Vec::new(),
            Vec::new(),
        ])
    }

    fn raw(names: &[&str]) -> Vec<Match> {
        names
            .iter()
            .enumerate()
            .map(|(index, pattern)| Match {
                pattern: CompactString::new(*pattern),
                start: index,
                end: index + 1,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
            })
            .collect()
    }

    fn candidate(raw: usize, definition: usize) -> Candidate {
        Candidate {
            raw: RawId(raw),
            definition: DefId(definition),
            public_start: raw,
            public_end: raw + 1,
        }
    }

    /// A context for the operand tests.
    ///
    /// `eligible` is empty and `priority_of` is constant: `operands_for` reads
    /// neither, and supplying a value it ignores would suggest the answers
    /// below depend on one.
    fn context<'a>(
        groups: &'a GroupIndex,
        definition_of: &'a dyn Fn(&str) -> Option<DefId>,
        priority_of: &'a dyn Fn(DefId) -> i64,
        anchors: &'a BTreeMap<&'a str, usize>,
    ) -> ExecContext<'a> {
        ExecContext {
            groups,
            definition_of,
            is_pattern: &|_| true,
            priority_of,
            anchors,
        }
    }

    /// Positions for a catalog declaring no anchor.
    ///
    /// `operands_for` never consults them; the operation that does is `admit`,
    /// whose fixtures are integration tests because only a compilation can
    /// produce a bound `Admit` and the sentence positions it measures against.
    fn no_anchors() -> BTreeMap<&'static str, usize> {
        BTreeMap::new()
    }

    fn aliases() -> IndexMap<CompactString, CompactString> {
        let mut hidden = IndexMap::new();
        hidden.insert(CompactString::new("__policy__N"), CompactString::new("N"));
        hidden
    }

    #[test]
    fn an_evidence_emitter_resolves_to_the_fragment_it_observes() {
        let domain = domain();
        let hidden = aliases();
        assert_eq!(
            definition_of("__policy__N", &domain, &hidden),
            Some(DefId(2)),
            "the emitter must answer with `N`'s id, not its own"
        );
    }

    #[test]
    fn an_ordinary_definition_resolves_to_itself() {
        let domain = domain();
        let hidden = aliases();
        assert_eq!(definition_of("b", &domain, &hidden), Some(DefId(1)));
    }

    #[test]
    fn a_name_this_catalog_does_not_declare_resolves_to_nothing() {
        let domain = domain();
        let hidden = aliases();
        assert_eq!(definition_of("absent", &domain, &hidden), None);
    }

    /// The whole point of `operands_for`'s first filter.
    #[test]
    fn a_subject_is_never_its_own_operand() {
        let (domain, groups, hidden, anchors) = (domain(), groups(), aliases(), no_anchors());
        let definition_of = |name: &str| definition_of(name, &domain, &hidden);
        let priority_of = |_: DefId| 0;
        let context = context(&groups, &definition_of, &priority_of, &anchors);
        let raw = raw(&["a", "a"]);

        assert_eq!(
            operands_for(
                BoundOperand::Def(DefId(0)),
                Evidence::Raw,
                &raw,
                &[],
                RawId(0),
                &context,
            ),
            vec![RawId(1)],
            "`a` selecting `@a` sees the other record and never itself"
        );
    }

    /// The whole point of `operands_for`'s second filter.
    #[test]
    fn a_match_of_another_definition_is_not_an_operand() {
        let (domain, groups, hidden, anchors) = (domain(), groups(), aliases(), no_anchors());
        let definition_of = |name: &str| definition_of(name, &domain, &hidden);
        let priority_of = |_: DefId| 0;
        let context = context(&groups, &definition_of, &priority_of, &anchors);
        let raw = raw(&["a", "b", "__policy__N"]);

        assert_eq!(
            operands_for(
                BoundOperand::Def(DefId(2)),
                Evidence::Raw,
                &raw,
                &[],
                RawId(0),
                &context,
            ),
            vec![RawId(2)],
            "`@N` selects its emitter's record and nothing else"
        );
    }

    #[test]
    fn a_group_operand_selects_every_member() {
        let (domain, groups, hidden, anchors) = (domain(), groups(), aliases(), no_anchors());
        let definition_of = |name: &str| definition_of(name, &domain, &hidden);
        let priority_of = |_: DefId| 0;
        let context = context(&groups, &definition_of, &priority_of, &anchors);
        let raw = raw(&["a", "b", "__policy__N"]);

        assert_eq!(
            operands_for(
                BoundOperand::Group(GroupId(0)),
                Evidence::Raw,
                &raw,
                &[],
                RawId(0),
                &context,
            ),
            vec![RawId(1)],
            "`%g` holds `a` and `b`; the subject's own record is still excluded"
        );
    }

    /// `evidence` changes the pool, and nothing else.
    #[test]
    fn current_evidence_draws_from_the_snapshot_rather_than_the_raw_slice() {
        let (domain, groups, hidden, anchors) = (domain(), groups(), aliases(), no_anchors());
        let definition_of = |name: &str| definition_of(name, &domain, &hidden);
        let priority_of = |_: DefId| 0;
        let context = context(&groups, &definition_of, &priority_of, &anchors);
        let raw = raw(&["a", "b", "b"]);
        // The second `b` lost an earlier operation, so it is gone from the
        // snapshot while remaining in the raw slice forever.
        let snapshot = vec![candidate(0, 0), candidate(1, 1)];

        assert_eq!(
            operands_for(
                BoundOperand::Def(DefId(1)),
                Evidence::Current,
                &raw,
                &snapshot,
                RawId(0),
                &context,
            ),
            vec![RawId(1)],
        );
        assert_eq!(
            operands_for(
                BoundOperand::Def(DefId(1)),
                Evidence::Raw,
                &raw,
                &snapshot,
                RawId(0),
                &context,
            ),
            vec![RawId(1), RawId(2)],
            "the raw slice is immutable and still holds the suppressed record"
        );
    }

    #[test]
    fn dropping_and_narrowing_leave_identity_untouched() {
        // Both endpoints move. A narrowed span that happened to equal the
        // original would let a `Narrow` arm ignoring its payload pass.
        let wide = Candidate {
            raw: RawId(2),
            definition: DefId(1),
            public_start: 2,
            public_end: 6,
        };
        let survivors = apply_decisions(
            vec![candidate(0, 0), candidate(1, 1), wide],
            &[Decision::Keep, Decision::Drop, Decision::Narrow(3, 5)],
        );
        assert_eq!(
            survivors,
            vec![
                candidate(0, 0),
                Candidate {
                    raw: RawId(2),
                    definition: DefId(1),
                    public_start: 3,
                    public_end: 5,
                },
            ],
            "the narrowed record takes the decision's span and keeps its \
             `raw` and `definition`"
        );
    }

    #[test]
    fn counters_are_sized_for_the_program_and_accumulate_across_sentences() {
        let mut stats = ProgramStats {
            per_operation: vec![OperationCounters::default(); 2],
        };
        stats.record(0, 5, 3);
        stats.record(0, 4, 4);
        stats.record(1, 3, 0);

        assert_eq!(
            stats.per_operation()[0],
            OperationCounters {
                before: 9,
                after: 7,
                dropped: 2,
                narrowed: 0,
                captures_dropped: 0,
            },
        );
        assert_eq!(stats.per_operation()[1].dropped, 3);
    }

    /// An out-of-range index is a reporting mismatch, never a failed run.
    #[test]
    fn recording_against_a_program_these_counters_were_not_built_for_is_inert() {
        let mut stats = ProgramStats::default();
        stats.record(7, 5, 0);
        assert!(stats.per_operation().is_empty());
    }

    /// The rank the compile-time totality check accepts, and the property
    /// below is over: `[Longest, …, Start, End, Pattern]`.
    const TOTAL_RANK: [Criterion; 4] = [
        Criterion::Longest,
        Criterion::Start,
        Criterion::End,
        Criterion::Pattern,
    ];

    /// Records built from generated triples, deduped as the emitter dedups.
    ///
    /// The dedup is load-bearing rather than tidy. `MatchSink` keys on
    /// `(pattern, start, end)`, so two records agreeing on all three never
    /// reach the engine together — generating them would refute totality for a
    /// pair the emitter cannot produce, and the refutation would say nothing
    /// about the rank.
    fn distinct_records(triples: &[(usize, usize, usize)]) -> (Vec<Match>, Vec<Candidate>) {
        let mut seen = std::collections::BTreeSet::new();
        let mut raw = Vec::new();
        let mut candidates = Vec::new();
        for &(start, width, pattern) in triples {
            let end = start + width + 1;
            let name = CompactString::new(format!("p{pattern}"));
            if !seen.insert((name.clone(), start, end)) {
                continue;
            }
            candidates.push(Candidate {
                raw: RawId(raw.len()),
                definition: DefId(pattern),
                public_start: start,
                public_end: end,
            });
            raw.push(Match {
                pattern: name,
                start,
                end,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
            });
        }
        (raw, candidates)
    }

    proptest::proptest! {
        /// The comparator is a strict total order on records the emitter can
        /// emit — antisymmetric, total on distinct records, and transitive.
        ///
        /// A hand-picked set cannot establish a universal claim. `priority_order` is constant
        /// here because the criterion is absent from [`TOTAL_RANK`]; a rank
        /// ending in it would not be total, which is the point of the check
        /// rather than a gap in this test.
        #[test]
        fn the_rank_comparator_is_a_strict_total_order(
            triples in proptest::collection::vec((0usize..6, 0usize..4, 0usize..3), 0..12),
        ) {
            let (raw, candidates) = distinct_records(&triples);
            let groups = GroupIndex::empty();
            let definition_of = |_: &str| None;
            let priority_of = |_: DefId| 0;
            let anchors = no_anchors();
            let context = context(&groups, &definition_of, &priority_of, &anchors);
            let cmp = |left: &Candidate, right: &Candidate| {
                compare_by_rank(left, right, &raw, &TOTAL_RANK, &context)
            };

            for left in &candidates {
                proptest::prop_assert_eq!(cmp(left, left), Ordering::Equal, "reflexive");
                for right in &candidates {
                    proptest::prop_assert_eq!(
                        cmp(left, right),
                        cmp(right, left).reverse(),
                        "antisymmetric"
                    );
                    proptest::prop_assert_eq!(
                        cmp(left, right) == Ordering::Equal,
                        left.raw == right.raw,
                        "total on distinct records"
                    );
                    for third in &candidates {
                        if cmp(left, right) != Ordering::Greater
                            && cmp(right, third) != Ordering::Greater
                        {
                            proptest::prop_assert_ne!(
                                cmp(left, third),
                                Ordering::Greater,
                                "transitive"
                            );
                        }
                    }
                }
            }
        }
    }
}
