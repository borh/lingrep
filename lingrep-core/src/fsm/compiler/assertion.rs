use super::diagnostic::{CompileError, Diagnostic, DiagnosticCode};
use super::limits::CompileLimits;
use super::model::{
    BoundExpr, BoundProgram, DefinitionId, LookDirection, LookPolarity, SourceSpan,
};
use crate::fsm::AssertionOccurrenceId;
use crate::fsm::matcher::assertion::{
    AssertionAutomaton, AssertionCatalog, AssertionEdge, AssertionEdgeKind, AssertionOccurrence,
    AssertionProgram, AssertionProgramId, required_attributes,
};
use crate::fsm::{AttrValueType, SymbolicAttrValue};
use compact_str::CompactString;
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct AssertionCost {
    pub(super) expansion_work: usize,
    pub(super) generated_owned_bytes: usize,
    pub(super) states: usize,
    pub(super) transitions: usize,
}

pub(super) struct AssertionCompilation {
    pub(super) catalog: AssertionCatalog,
    pub(super) occurrences: BTreeMap<SourceSpan, AssertionOccurrenceId>,
    /// The program recognizing each declared anchor's `exclude`, by anchor name.
    pub(super) anchor_programs: BTreeMap<CompactString, AssertionProgramId>,
    pub(super) cost: AssertionCost,
}

/// Compiles every lookaround body, and every anchor's `exclude` fragment, into
/// the assertion catalog.
///
/// `anchors` maps an anchor name to the bound definition of the fragment
/// recognizing the trailing material it excludes, resolved by
/// [`super::policy::resolve_anchors`].
pub(super) fn compile_assertions(
    bound: &BoundProgram,
    limits: &CompileLimits,
    anchors: &BTreeMap<CompactString, DefinitionId>,
) -> Result<AssertionCompilation, CompileError> {
    let mut compiler = Compiler {
        bound,
        limits,
        programs: Vec::new(),
        keys: Vec::new(),
        occurrences: Vec::new(),
        occurrence_by_span: BTreeMap::new(),
        cost: AssertionCost::default(),
    };
    for definition in &bound.definitions {
        if let Some(expr) = definition.expr() {
            compiler.collect(expr, false)?;
        }
    }

    // Anchor recognizers are interned here rather than minted as definitions,
    // because an anchor is a position and never a record. Sharing the assertion
    // catalog gives them compiled predicates, dependency rows for any nested
    // lookaround, and — the reason this cannot be skipped — a place in the
    // assertion contribution to `finalize_required_attributes`, which unions
    // over *installed programs* alongside the recognition NFA, the capture plans
    // and the span-equality plans. A fragment named only by an `anchor:`
    // declaration is referenced by no pattern, so without this it reaches
    // neither the NFA nor the projection, and its attributes would be absent
    // from the cache at the moment the anchor is evaluated.
    let mut anchor_programs = BTreeMap::new();
    for (anchor, exclude) in anchors {
        let definition = &bound.definitions[exclude.index()];
        let id = compiler.intern_program(
            // Semantic, not an artifact of how the anchor pass happens to
            // iterate: the exclude must match *forward* from a candidate
            // boundary to the sentence end, since the anchor is the earliest `b`
            // whose `b..len` the fragment recognizes. That is a `followed_by`
            // fact by definition, independent of evaluator traversal. It also
            // decides whether the
            // exclude shares a program with a `followed_by` body or a
            // `preceded_by` one.
            LookDirection::FollowedBy,
            definition
                .expr()
                .expect("anchor excludes are ordinary fragments"),
            definition.span,
        )?;
        anchor_programs.insert(anchor.clone(), id);
    }

    Ok(AssertionCompilation {
        catalog: AssertionCatalog {
            programs: compiler.programs.into_boxed_slice(),
            occurrences: compiler.occurrences.into_boxed_slice(),
        },
        occurrences: compiler.occurrence_by_span,
        anchor_programs,
        cost: compiler.cost,
    })
}

struct Compiler<'a> {
    bound: &'a BoundProgram,
    limits: &'a CompileLimits,
    programs: Vec<AssertionProgram>,
    keys: Vec<(LookDirection, BoundExpr)>,
    occurrences: Vec<AssertionOccurrence>,
    occurrence_by_span: BTreeMap<SourceSpan, AssertionOccurrenceId>,
    cost: AssertionCost,
}

impl Compiler<'_> {
    /// Register an assertion occurrence for every lookaround that needs one.
    ///
    /// `nested` is true while walking the body of another lookaround. It exists
    /// because the `specialized_previous_token` fast path — a positive
    /// `preceded_by` over a single token, lowered to
    /// `PositionAssertion::PreviousToken` with no assertion program at all — is
    /// only available to consumers that check for it. `materialize.rs` and
    /// `nfa.rs` both do. [`AutomatonBuilder::compile`] cannot: inside an
    /// assertion body a lookaround becomes a `Fact` edge, which is addressed by
    /// occurrence. Skipping registration there left it looking up an occurrence
    /// that was never created, aborting the process instead of compiling.
    ///
    /// So the specialization is applied at the top level and declined inside a
    /// body, where the general path is the only one that works.
    ///
    /// Mutually recursive with [`Self::intern_program`], which walks the body it
    /// is given before compiling it — so the descent through a nested lookaround
    /// happens there rather than in the arm below. It terminates because `bind`
    /// already refused every reference cycle, leaving a DAG to walk.
    fn collect(&mut self, expr: &BoundExpr, nested: bool) -> Result<(), CompileError> {
        match expr {
            BoundExpr::Lookaround(lookaround) => {
                if (!nested && lookaround.specialized_previous_token().is_some())
                    || self.occurrence_by_span.contains_key(&lookaround.span)
                {
                    return Ok(());
                }
                // Walks the body itself, with `nested` true — see its doc.
                let program =
                    self.intern_program(lookaround.direction, &lookaround.body, lookaround.span)?;
                let occurrence = AssertionOccurrenceId::new(self.occurrences.len());
                self.charge_generated_owned_bytes(
                    lookaround.span,
                    size_of::<AssertionOccurrence>(),
                )?;
                self.occurrences.push(AssertionOccurrence {
                    program,
                    polarity: lookaround.polarity,
                });
                self.occurrence_by_span.insert(lookaround.span, occurrence);
            }
            BoundExpr::Sequence(children) | BoundExpr::Alternation(children) => {
                for child in children {
                    self.collect(child, nested)?;
                }
            }
            // Walked, not refused here. `collect` registers occurrences; the
            // refusal belongs to `AutomatonBuilder::compile`, which is the
            // thing that cannot represent an intersection, and which reports
            // against the operator's own span.
            BoundExpr::Intersection { operands, .. } | BoundExpr::AnyOrder { operands, .. } => {
                for operand in operands {
                    self.collect(operand, nested)?;
                }
            }
            BoundExpr::Repeat { expr, .. } | BoundExpr::Capture { expr, .. } => {
                self.collect(expr, nested)?
            }
            BoundExpr::Definition { id, .. } => self.collect(
                self.bound.definitions[id.index()]
                    .expr()
                    .expect("lexicon references are refused"),
                nested,
            )?,
            BoundExpr::Token(_) | BoundExpr::Assertion(_) | BoundExpr::SameAs { .. } => {}
        }
        Ok(())
    }

    /// The program recognizing `body` in `direction`, compiling it if new.
    ///
    /// Keyed by `(direction, body)` rather than by span, so that two expressions
    /// that mean the same thing share one program however they were written —
    /// an anchor's `exclude` fragment and a lookaround body over that same
    /// fragment intern to a single entry. `span` is the site to blame in a
    /// limits diagnostic, and deliberately not part of the key.
    ///
    /// `body` is walked with `nested = true` first, and here rather than at each
    /// call site. Anything interned *is* an assertion body, where the
    /// specialized single-token `preceded_by` lowering is unavailable and a
    /// lookaround has to be addressed by occurrence; a caller that skipped the
    /// walk would leave [`AutomatonBuilder::compile`] looking up an occurrence
    /// that was never created, aborting the process on a valid catalog. Holding
    /// it here makes that unstatable rather than merely stated. It is idempotent
    /// — `collect` returns early on an already-registered span — and it runs
    /// before the key check because a cache hit must not be able to skip it.
    fn intern_program(
        &mut self,
        direction: LookDirection,
        body: &BoundExpr,
        span: SourceSpan,
    ) -> Result<AssertionProgramId, CompileError> {
        self.collect(body, true)?;
        if let Some(index) = self
            .keys
            .iter()
            .position(|(key_direction, key_body)| *key_direction == direction && key_body == body)
        {
            return Ok(AssertionProgramId::new(index));
        }

        // A top-level alternation is the union of its branches, and a row is a
        // set of boundaries, so `row(A | B) == row(A) | row(B)` in either
        // direction: a body matches at a boundary exactly when some branch
        // does. Interning each branch separately and unioning them is what
        // makes that identity pay, because the key above is the *whole* body —
        // five cumulative bodies `A`, `A | B`, ... `A | B | C | D | E` share no
        // key and so swept the same branches fifteen times between them.
        //
        // Unconditional rather than conditioned on a branch already being
        // interned. That test would be order-dependent — it holds for the
        // catalog that motivated this and not for the same catalog written in
        // the other order — and an order-dependent compilation is a worse thing
        // to own than the one union it would save.
        if let BoundExpr::Alternation(children) = body
            && children.len() > 1
        {
            return self.intern_union(direction, body, children, span);
        }

        let mut builder = AutomatonBuilder {
            compiler: self,
            edges: Vec::new(),
            states: 0,
            dependencies: BTreeSet::new(),
            span,
        };
        let fragment = builder.compile(body)?;
        let automaton = AssertionAutomaton {
            states: builder.states,
            start: fragment.start,
            accept: fragment.end,
            edges: builder.edges.into_boxed_slice(),
        };
        let dependencies: Box<[AssertionProgramId]> = builder.dependencies.into_iter().collect();
        let required_attributes = required_attributes(&automaton);
        self.charge_program(span, &dependencies, &required_attributes)?;
        let id = AssertionProgramId::new(self.programs.len());
        self.keys.push((direction, body.clone()));
        self.programs.push(AssertionProgram {
            direction,
            automaton,
            dependencies,
            required_attributes,
            union_members: Box::default(),
        });
        Ok(id)
    }

    /// The program recognizing `children` unioned, in `direction`.
    ///
    /// Each branch is interned as a program of its own; the union reads their
    /// rows rather than sweeping the sentence again. The members become this
    /// program's dependencies, which is what makes `ensure` materialize every
    /// one of them first.
    ///
    /// The synthesized occurrences are always [`LookPolarity::Positive`] and
    /// are deliberately absent from `occurrence_by_span`. Polarity stays on the
    /// occurrence the author wrote: negating the union is `not (A or B)`, and
    /// negating each member first would compute `(not A) or (not B)`.
    ///
    /// Note that the automaton built below is **not executed**. The matcher's
    /// `evaluate` short-circuits on `union_members` and `or`s the members'
    /// rows, so these two states, the `Fact` edge per member and the
    /// occurrences behind them are the declarative statement of what the row
    /// means rather than the path that computes it. The short circuit therefore
    /// makes the synthesized polarity runtime-unobservable, but it must still
    /// describe the same semantics. Building it also costs a `CompiledAssertionAutomaton`
    /// per union that nothing sweeps; removing it would move the compiler-stats
    /// fingerprint and the transition pin, so it is a change of its own.
    fn intern_union(
        &mut self,
        direction: LookDirection,
        body: &BoundExpr,
        children: &[BoundExpr],
        span: SourceSpan,
    ) -> Result<AssertionProgramId, CompileError> {
        // The union is a node the source asked for, so it is charged like one.
        // Without this a catalog's expansion work would *fall* when an
        // alternation body is split, and a source the limits refused before
        // would quietly begin to compile — a loosening nothing asked for.
        self.charge_expansion(span)?;

        let mut members = BTreeSet::new();
        let mut facts = Vec::with_capacity(children.len());
        for child in children {
            // Interned by the definition's *own* expression, not by the
            // reference. `BoundExpr::Definition` carries the span of the site
            // that wrote it and derives `PartialEq`, so `@A` here and `@A` one
            // line down are unequal and would intern as two programs. See
            // `docs/results/2026-08-18-lookaround-alternation-union/`.
            let bound = self.bound;
            let body = match child {
                BoundExpr::Definition { id, .. } => bound.definitions[id.index()]
                    .expr()
                    .expect("lexicon references are refused"),
                other => other,
            };
            let program = self.intern_program(direction, body, span)?;
            members.insert(program);
            let occurrence = AssertionOccurrenceId::new(self.occurrences.len());
            self.charge_generated_owned_bytes(span, size_of::<AssertionOccurrence>())?;
            self.occurrences.push(AssertionOccurrence {
                program,
                polarity: LookPolarity::Positive,
            });
            facts.push(occurrence);
        }

        let mut builder = AutomatonBuilder {
            compiler: self,
            edges: Vec::new(),
            states: 0,
            dependencies: BTreeSet::new(),
            span,
        };
        let start = builder.state()?;
        let accept = builder.state()?;
        for occurrence in facts {
            builder.edge(start, AssertionEdgeKind::Fact(occurrence), accept)?;
        }
        let automaton = AssertionAutomaton {
            states: builder.states,
            start,
            accept,
            edges: builder.edges.into_boxed_slice(),
        };

        let dependencies: Box<[AssertionProgramId]> = members.into_iter().collect();
        let union_members = dependencies.clone();
        // Empty by construction: `required_attributes` reads consume and
        // previous-token edges, and a union has neither. Each branch charges
        // its own, and `finalize_required_attributes` unions over installed
        // programs — so nothing is lost by this one contributing none.
        let required_attributes = required_attributes(&automaton);
        self.charge_program(span, &dependencies, &required_attributes)?;
        let id = AssertionProgramId::new(self.programs.len());
        self.keys.push((direction, body.clone()));
        self.programs.push(AssertionProgram {
            direction,
            automaton,
            dependencies,
            required_attributes,
            union_members,
        });
        Ok(id)
    }

    /// Charges one installed program's owned bytes.
    fn charge_program(
        &mut self,
        span: SourceSpan,
        dependencies: &[AssertionProgramId],
        required_attributes: &[crate::token_match::AttributeKey],
    ) -> Result<(), CompileError> {
        let program_owned_bytes = dependencies
            .len()
            .checked_mul(size_of::<AssertionProgramId>())
            .and_then(|dependency_bytes| {
                required_attributes
                    .len()
                    .checked_mul(size_of::<crate::token_match::TokenAttributeIndex>())
                    .and_then(|attribute_bytes| {
                        size_of::<AssertionProgram>()
                            .checked_add(dependency_bytes)?
                            .checked_add(attribute_bytes)
                    })
            })
            .ok_or_else(|| {
                self.diagnostic(
                    span,
                    DiagnosticCode::LimitExceeded,
                    "assertion catalog owned-byte accounting overflowed",
                )
            })?;
        self.charge_generated_owned_bytes(span, program_owned_bytes)
    }

    fn diagnostic(&self, span: SourceSpan, code: DiagnosticCode, message: &str) -> CompileError {
        let range = span.byte_range();
        let (line, column) = self.bound.sources.line_column(span);
        CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
            code,
            self.bound.sources.source_label(span.source()),
            usize::try_from(range.start).expect("source offset fits usize")
                ..usize::try_from(range.end).expect("source offset fits usize"),
            message,
            line,
            column,
        )])
    }

    fn charge_expansion(&mut self, span: SourceSpan) -> Result<(), CompileError> {
        self.cost.expansion_work = self.cost.expansion_work.checked_add(1).ok_or_else(|| {
            self.diagnostic(
                span,
                DiagnosticCode::LimitExceeded,
                "assertion expansion work accounting overflowed",
            )
        })?;
        self.check_limit(
            span,
            "expansion work",
            self.cost.expansion_work,
            self.limits.max_expansion_work(),
        )
    }

    fn charge_state(&mut self, span: SourceSpan) -> Result<(), CompileError> {
        self.cost.states = self.cost.states.checked_add(1).ok_or_else(|| {
            self.diagnostic(
                span,
                DiagnosticCode::LimitExceeded,
                "assertion state accounting overflowed",
            )
        })?;
        self.check_limit(
            span,
            "NFA states",
            self.cost.states,
            self.limits.max_nfa_states(),
        )
    }

    fn charge_transition(&mut self, span: SourceSpan) -> Result<(), CompileError> {
        self.cost.transitions = self.cost.transitions.checked_add(1).ok_or_else(|| {
            self.diagnostic(
                span,
                DiagnosticCode::LimitExceeded,
                "assertion transition accounting overflowed",
            )
        })?;
        self.check_limit(
            span,
            "transitions",
            self.cost.transitions,
            self.limits.max_transitions(),
        )
    }

    fn charge_generated_owned_bytes(
        &mut self,
        span: SourceSpan,
        bytes: usize,
    ) -> Result<(), CompileError> {
        self.cost.generated_owned_bytes = self
            .cost
            .generated_owned_bytes
            .checked_add(bytes)
            .ok_or_else(|| {
                self.diagnostic(
                    span,
                    DiagnosticCode::LimitExceeded,
                    "assertion generated owned-byte accounting overflowed",
                )
            })?;
        self.check_limit(
            span,
            "generated owned bytes",
            self.cost.generated_owned_bytes,
            self.limits.max_generated_owned_bytes(),
        )
    }

    fn check_limit(
        &self,
        span: SourceSpan,
        budget: &str,
        requested: usize,
        maximum: usize,
    ) -> Result<(), CompileError> {
        if requested <= maximum {
            return Ok(());
        }
        Err(self.diagnostic(
            span,
            DiagnosticCode::LimitExceeded,
            &format!(
                "lingrep patterns program exceeds the {budget} budget: requested {requested}, maximum {maximum}"
            ),
        ))
    }
}

#[derive(Clone, Copy)]
struct Fragment {
    start: usize,
    end: usize,
}

struct AutomatonBuilder<'compiler, 'bound> {
    compiler: &'compiler mut Compiler<'bound>,
    edges: Vec<AssertionEdge>,
    states: usize,
    dependencies: BTreeSet<AssertionProgramId>,
    span: SourceSpan,
}

impl AutomatonBuilder<'_, '_> {
    fn state(&mut self) -> Result<usize, CompileError> {
        self.compiler.charge_state(self.span)?;
        let state = self.states;
        self.states += 1;
        Ok(state)
    }

    fn edge(
        &mut self,
        from: usize,
        kind: AssertionEdgeKind,
        to: usize,
    ) -> Result<(), CompileError> {
        self.compiler.charge_transition(self.span)?;
        let bytes = size_of::<AssertionEdge>()
            .checked_add(match &kind {
                AssertionEdgeKind::Consume(predicate) => predicate_owned_bytes(predicate)
                    .ok_or_else(|| {
                        self.compiler.diagnostic(
                            self.span,
                            DiagnosticCode::LimitExceeded,
                            "assertion predicate owned-byte accounting overflowed",
                        )
                    })?,
                AssertionEdgeKind::Epsilon
                | AssertionEdgeKind::Boundary(_)
                | AssertionEdgeKind::Fact(_) => 0,
            })
            .ok_or_else(|| {
                self.compiler.diagnostic(
                    self.span,
                    DiagnosticCode::LimitExceeded,
                    "assertion edge owned-byte accounting overflowed",
                )
            })?;
        self.compiler
            .charge_generated_owned_bytes(self.span, bytes)?;
        self.edges.push(AssertionEdge { from, to, kind });
        Ok(())
    }

    fn atom(&mut self, kind: AssertionEdgeKind) -> Result<Fragment, CompileError> {
        let start = self.state()?;
        let end = self.state()?;
        self.edge(start, kind, end)?;
        Ok(Fragment { start, end })
    }

    fn compile(&mut self, expr: &BoundExpr) -> Result<Fragment, CompileError> {
        self.compiler.charge_expansion(self.span)?;
        Ok(match expr {
            BoundExpr::Token(predicate) => {
                self.atom(AssertionEdgeKind::Consume(predicate.clone()))?
            }
            BoundExpr::Assertion(assertion) => {
                self.atom(AssertionEdgeKind::Boundary(assertion.clone()))?
            }
            BoundExpr::Lookaround(lookaround) => {
                let occurrence = *self
                    .compiler
                    .occurrence_by_span
                    .get(&lookaround.span)
                    // Established by `Compiler::collect`, which registers an
                    // occurrence for every lookaround reached with `nested`
                    // true. Keep the two in step: when `collect` declined to
                    // register the specialized single-token `preceded_by`, this
                    // aborted the process on valid input instead of compiling.
                    .expect("nested lookaround is registered by collect(.., nested = true)");
                let dependency = self.compiler.occurrences[occurrence.index()].program;
                self.dependencies.insert(dependency);
                self.atom(AssertionEdgeKind::Fact(occurrence))?
            }
            BoundExpr::Sequence(children) => {
                let mut children = children.iter();
                let Some(first) = children.next() else {
                    return self.atom(AssertionEdgeKind::Epsilon);
                };
                let mut result = self.compile(first)?;
                for child in children {
                    let next = self.compile(child)?;
                    self.edge(result.end, AssertionEdgeKind::Epsilon, next.start)?;
                    result.end = next.end;
                }
                result
            }
            BoundExpr::Alternation(children) => {
                let start = self.state()?;
                let end = self.state()?;
                if children.is_empty() {
                    self.edge(start, AssertionEdgeKind::Epsilon, end)?;
                }
                for child in children {
                    let branch = self.compile(child)?;
                    self.edge(start, AssertionEdgeKind::Epsilon, branch.start)?;
                    self.edge(branch.end, AssertionEdgeKind::Epsilon, end)?;
                }
                Fragment { start, end }
            }
            BoundExpr::Repeat { expr, min, max } => self.repeat(expr, *min, *max)?,
            BoundExpr::Definition { id, .. } => self.compile(
                self.compiler.bound.definitions[id.index()]
                    .expr()
                    .expect("lexicon references are refused"),
            )?,
            BoundExpr::Capture { span, .. } => {
                return Err(self.compiler.diagnostic(
                    *span,
                    DiagnosticCode::CaptureInsideLookaround,
                    "captures are not allowed inside lookaround",
                ));
            }
            BoundExpr::SameAs { span, .. } => {
                return Err(self.compiler.diagnostic(
                    *span,
                    DiagnosticCode::SpanEqualityInsideLookaround,
                    "span equality is not allowed inside lookaround",
                ));
            }
            BoundExpr::Intersection { span, .. } => {
                return Err(self.compiler.diagnostic(
                    *span,
                    DiagnosticCode::IntersectionInsideLookaround,
                    "`&` is not allowed inside lookaround: an assertion is answered by recording \
                     which boundaries an accepting path reaches, which does not carry the other \
                     end of a span — write the lookaround inside the intersection instead",
                ));
            }
            BoundExpr::AnyOrder { span, .. } => {
                return Err(self.compiler.diagnostic(
                    *span,
                    DiagnosticCode::CaptureInsideLookaround,
                    "`any_order` is not allowed inside lookaround because every operand is a capture",
                ));
            }
        })
    }

    fn repeat(
        &mut self,
        expr: &BoundExpr,
        min: usize,
        max: Option<usize>,
    ) -> Result<Fragment, CompileError> {
        let start = self.state()?;
        let mut tail = start;
        for _ in 0..min {
            let required = self.compile(expr)?;
            self.edge(tail, AssertionEdgeKind::Epsilon, required.start)?;
            tail = required.end;
        }
        let end = self.state()?;
        if let Some(max) = max {
            for _ in min..max {
                let optional = self.compile(expr)?;
                self.edge(tail, AssertionEdgeKind::Epsilon, end)?;
                self.edge(tail, AssertionEdgeKind::Epsilon, optional.start)?;
                tail = optional.end;
            }
            self.edge(tail, AssertionEdgeKind::Epsilon, end)?;
        } else {
            let repeated = self.compile(expr)?;
            self.edge(tail, AssertionEdgeKind::Epsilon, end)?;
            self.edge(tail, AssertionEdgeKind::Epsilon, repeated.start)?;
            self.edge(repeated.end, AssertionEdgeKind::Epsilon, tail)?;
        }
        Ok(Fragment { start, end })
    }
}

fn predicate_owned_bytes(predicate: &[SymbolicAttrValue]) -> Option<usize> {
    let mut bytes = predicate
        .len()
        .checked_mul(size_of::<SymbolicAttrValue>())?;
    for attribute in predicate {
        let value_bytes = match &attribute.value {
            AttrValueType::Single(value)
            | AttrValueType::SetHas(value)
            | AttrValueType::SetNotHas(value)
            | AttrValueType::Contains(value)
            | AttrValueType::NotContains(value) => value.len(),
            AttrValueType::OneOf(values)
            | AttrValueType::NotIn(values)
            | AttrValueType::SetHasAny(values)
            | AttrValueType::SetNotHasAny(values) => values.iter().try_fold(
                values
                    .len()
                    .checked_mul(size_of::<compact_str::CompactString>())?,
                |sum, value| sum.checked_add(value.len()),
            )?,
            AttrValueType::Regex(regex) | AttrValueType::NotRegex(regex) => regex.as_str().len(),
        };
        bytes = bytes.checked_add(value_bytes)?;
    }
    Some(bytes)
}
