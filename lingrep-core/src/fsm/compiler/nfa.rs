//! Lowering from the validated lingrep patterns graph to NFA builder inputs.

use crate::fsm::matcher::CompiledCatalog;
use crate::fsm::state_machine::StateMachineBuilder;
use crate::fsm::{
    AttrValueType, Operator, Pattern, PatternElement, Patterns, SymbolicAttrValue, TokenSpec,
};
use compact_str::CompactString;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use super::diagnostic::{CompileError, Diagnostic, DiagnosticCode};
use super::limits::CompileLimits;
use super::model::{
    BoundExpr, BoundProgram, DefinitionId, ExecutionDefinition, Metadata, SourceSpan,
};
use crate::fsm::AssertionOccurrenceId;

pub(super) struct NfaOutput {
    pub(super) matcher: CompiledCatalog,
    pub(super) expansion_work: usize,
    pub(super) generated_owned_bytes: usize,
    pub(super) states: usize,
    pub(super) transitions: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NfaCost {
    states: usize,
    transitions: usize,
    finals: usize,
    element_owned_bytes: usize,
    transition_owned_bytes: usize,
}

struct LoweredExpr {
    element: PatternElement,
    cost: NfaCost,
}

enum WorkItem<'expr> {
    Enter(&'expr BoundExpr),
    AssembleSequence(usize),
    AssembleAlternation(usize),
    AssembleRepeat(usize, Option<usize>),
    ExitDefinition(DefinitionId),
}

struct CompatibilityLowerer<'a> {
    program: &'a BoundProgram,
    limits: &'a CompileLimits,
    expansion_work: usize,
    generated_owned_bytes: usize,
    states: usize,
    transitions: usize,
    assertion_occurrences: &'a BTreeMap<SourceSpan, AssertionOccurrenceId>,
}

impl<'a> CompatibilityLowerer<'a> {
    fn new(
        program: &'a BoundProgram,
        limits: &'a CompileLimits,
        assertion_occurrences: &'a BTreeMap<SourceSpan, AssertionOccurrenceId>,
    ) -> Self {
        Self {
            program,
            limits,
            expansion_work: 0,
            generated_owned_bytes: 0,
            states: 0,
            transitions: 0,
            assertion_occurrences,
        }
    }

    fn lower(mut self, execution: &[ExecutionDefinition]) -> Result<NfaOutput, CompileError> {
        let mut patterns = Patterns(IndexMap::with_capacity(self.program.definitions.len()));
        let evaluated = execution
            .iter()
            .filter(|definition| definition.capability.evaluates_whole_definition())
            .map(|definition| definition.name.clone())
            .collect::<HashSet<_>>();
        let mut nfa_patterns = HashSet::new();

        for definition in &self.program.definitions {
            if !definition.is_ordinary_pattern() {
                continue;
            }
            // Contributes no states to the recognition machine: it is
            // recognized by an `EvaluatedPlan` instead. The entry is still
            // inserted so the definition keeps its place and metadata in the
            // published catalog.
            if evaluated.contains(&definition.name) {
                patterns.0.insert(
                    definition.name.clone(),
                    Pattern {
                        name: definition.name.clone(),
                        token_specs: Vec::new(),
                        metadata: project_metadata(&definition.metadata),
                    },
                );
                continue;
            }
            let source_label = self.program.sources.source_label(definition.source);
            let lowered = self.lower_expr(
                definition
                    .expr()
                    .expect("ordinary patterns have expressions"),
                definition.id,
                &source_label,
            )?;

            // StateMachineBuilder allocates one private start state and one
            // global-start epsilon transition for every top-level pattern.
            self.charge(
                NfaCost {
                    states: checked_add(lowered.cost.states, 1, &source_label)?,
                    transitions: checked_add(lowered.cost.transitions, 1, &source_label)?,
                    finals: lowered.cost.finals,
                    ..NfaCost::default()
                },
                &source_label,
            )?;

            // Charge every source-derived owned clone made after compatibility
            // lowering before the catalog or state-machine builder allocates it.
            // Fixed state/transition structures are bounded by their count
            // budgets; this dimension covers their variable-sized payloads.
            let deferred_owned_bytes = generated_output_owned_bytes(
                definition.name.as_str(),
                &definition.metadata,
                lowered.cost,
                &source_label,
            )?;
            self.charge_generated_owned_bytes(deferred_owned_bytes, &source_label)?;

            patterns.0.insert(
                definition.name.clone(),
                Pattern {
                    name: definition.name.clone(),
                    token_specs: vec![lowered.element],
                    metadata: project_metadata(&definition.metadata),
                },
            );
            nfa_patterns.insert(definition.name.clone());
        }

        // Construction starts only after every definition has lowered and the
        // complete generated-NFA budget has been accepted.
        let mut builder = StateMachineBuilder::new();
        for (name, pattern) in &patterns.0 {
            if !nfa_patterns.contains(name) {
                continue;
            }
            builder
                .add_pattern(name, pattern)
                .map_err(|message| CompileError::new(name, message))?;
        }
        let data = builder.build();

        debug_assert_eq!(
            data.state_transitions.values().map(Vec::len).sum::<usize>(),
            self.transitions
        );
        let highest_state = data
            .state_transitions
            .iter()
            .flat_map(|(from, transitions)| {
                std::iter::once(*from).chain(transitions.iter().map(|(_, to)| *to))
            })
            .chain(data.final_states.iter().map(|state| state.state_id))
            .max()
            .unwrap_or(0);
        debug_assert_eq!(highest_state, self.states);

        Ok(NfaOutput {
            matcher: CompiledCatalog::new(patterns, data),
            expansion_work: self.expansion_work,
            generated_owned_bytes: self.generated_owned_bytes,
            states: self.states,
            transitions: self.transitions,
        })
    }

    fn lower_expr(
        &mut self,
        expr: &BoundExpr,
        root_definition: DefinitionId,
        source_label: &Arc<str>,
    ) -> Result<LoweredExpr, CompileError> {
        let mut work = vec![WorkItem::Enter(expr)];
        let mut values = Vec::new();
        let mut active_definitions = HashSet::from([root_definition]);

        while let Some(item) = work.pop() {
            match item {
                WorkItem::Enter(expr) => {
                    self.charge_expansion_visit(source_label)?;
                    match expr {
                        BoundExpr::Token(predicate) => {
                            let operator = Operator::Exact(1);
                            let predicate_owned_bytes =
                                predicate_owned_bytes(predicate, source_label)?;
                            let cost = token_cost(operator, predicate_owned_bytes, source_label)?;
                            self.ensure_cost(cost, source_label)?;
                            self.charge_generated_owned_bytes(predicate_owned_bytes, source_label)?;
                            values.push(LoweredExpr {
                                element: PatternElement::TokenSpec(TokenSpec {
                                    op: operator,
                                    attrs: predicate.to_vec(),
                                }),
                                cost,
                            });
                        }
                        BoundExpr::Assertion(assertion) => {
                            let owned_bytes = match assertion {
                                crate::fsm::PositionAssertion::PreviousToken(predicate) => {
                                    predicate_owned_bytes(predicate, source_label)?
                                }
                                crate::fsm::PositionAssertion::SentenceStart
                                | crate::fsm::PositionAssertion::SentenceEnd
                                | crate::fsm::PositionAssertion::Lookaround(_) => 0,
                            };
                            let cost = NfaCost {
                                states: 1,
                                transitions: 1,
                                finals: 1,
                                transition_owned_bytes: owned_bytes,
                                ..NfaCost::default()
                            };
                            self.ensure_cost(cost, source_label)?;
                            self.charge_generated_owned_bytes(owned_bytes, source_label)?;
                            values.push(LoweredExpr {
                                element: PatternElement::Assertion(assertion.clone()),
                                cost,
                            });
                        }
                        BoundExpr::Lookaround(lookaround) => {
                            let assertion = if let Some(predicate) =
                                lookaround.specialized_previous_token()
                            {
                                crate::fsm::PositionAssertion::PreviousToken(predicate.clone())
                            } else {
                                let occurrence = *self
                                    .assertion_occurrences
                                    .get(&lookaround.span)
                                    .expect("general lookaround occurrence is compiled before NFA lowering");
                                crate::fsm::PositionAssertion::Lookaround(occurrence)
                            };
                            let owned_bytes = lookaround
                                .specialized_previous_token()
                                .map_or(Ok(0), |predicate| {
                                    predicate_owned_bytes(predicate, source_label)
                                })?;
                            let cost = NfaCost {
                                states: 1,
                                transitions: 1,
                                finals: 1,
                                transition_owned_bytes: owned_bytes,
                                ..NfaCost::default()
                            };
                            self.ensure_cost(cost, source_label)?;
                            self.charge_generated_owned_bytes(owned_bytes, source_label)?;
                            values.push(LoweredExpr {
                                element: PatternElement::Assertion(assertion),
                                cost,
                            });
                        }
                        BoundExpr::Sequence(expressions) => {
                            work.push(WorkItem::AssembleSequence(expressions.len()));
                            for expression in expressions.iter().rev() {
                                work.push(WorkItem::Enter(expression));
                            }
                        }
                        BoundExpr::Alternation(expressions) => {
                            work.push(WorkItem::AssembleAlternation(expressions.len()));
                            for expression in expressions.iter().rev() {
                                work.push(WorkItem::Enter(expression));
                            }
                        }
                        BoundExpr::Repeat { expr, min, max } => {
                            work.push(WorkItem::AssembleRepeat(*min, *max));
                            work.push(WorkItem::Enter(expr));
                        }
                        BoundExpr::Capture { expr, .. } => {
                            work.push(WorkItem::Enter(expr));
                        }
                        BoundExpr::SameAs { span, .. } => {
                            return Err(diagnostic_at(
                                &self.program.sources,
                                *span,
                                DiagnosticCode::DefinitionDropped,
                                "span equality requires the history execution plan",
                            ));
                        }
                        // Unreachable from a definition that compiled: `lower`
                        // routes every definition the tree evaluator owns away
                        // before descending, exactly as it does for span
                        // equality. Arriving here is a routing defect, not
                        // authored input, so it reports the same way its
                        // sibling above does rather than panicking.
                        BoundExpr::Intersection { span, .. } => {
                            return Err(diagnostic_at(
                                &self.program.sources,
                                *span,
                                DiagnosticCode::DefinitionDropped,
                                "intersection requires the tree execution plan",
                            ));
                        }
                        BoundExpr::AnyOrder { span, .. } => {
                            return Err(diagnostic_at(
                                &self.program.sources,
                                *span,
                                DiagnosticCode::DefinitionDropped,
                                "any_order requires the tree execution plan",
                            ));
                        }
                        BoundExpr::Definition { id, .. } => {
                            if !active_definitions.insert(*id) {
                                return Err(diagnostic_error(
                                    source_label,
                                    DiagnosticCode::ReferenceCycle,
                                    "bound definition graph contains a reference cycle",
                                ));
                            }
                            let definition =
                                self.program.definitions.get(id.index()).ok_or_else(|| {
                                    diagnostic_error(
                                        source_label,
                                        DiagnosticCode::UndefinedReference,
                                        "bound definition graph contains an invalid definition ID",
                                    )
                                })?;
                            work.push(WorkItem::ExitDefinition(*id));
                            work.push(WorkItem::Enter(
                                definition.expr().expect("lexicon references are refused"),
                            ));
                        }
                    }
                }
                WorkItem::AssembleSequence(count) => {
                    let children = take_children(&mut values, count, source_label)?;
                    let container_owned_bytes =
                        checked_owned_mul(count, size_of::<PatternElement>(), source_label)?;
                    self.charge_generated_owned_bytes(container_owned_bytes, source_label)?;
                    let mut elements = Vec::with_capacity(count);
                    let mut cost = NfaCost {
                        finals: 1,
                        ..NfaCost::default()
                    };
                    for child in children {
                        cost = sequence_cost(cost, child.cost, source_label)?;
                        self.ensure_cost(cost, source_label)?;
                        elements.push(child.element);
                    }
                    cost.element_owned_bytes = checked_owned_add(
                        cost.element_owned_bytes,
                        container_owned_bytes,
                        source_label,
                    )?;
                    values.push(LoweredExpr {
                        element: PatternElement::And(elements),
                        cost,
                    });
                }
                WorkItem::AssembleAlternation(count) => {
                    let children = take_children(&mut values, count, source_label)?;
                    let container_owned_bytes =
                        checked_owned_mul(count, size_of::<PatternElement>(), source_label)?;
                    self.charge_generated_owned_bytes(container_owned_bytes, source_label)?;
                    let mut elements = Vec::with_capacity(count);
                    let mut cost = NfaCost::default();
                    for child in children {
                        cost = alternation_cost(cost, child.cost, source_label)?;
                        self.ensure_cost(cost, source_label)?;
                        elements.push(child.element);
                    }
                    cost.element_owned_bytes = checked_owned_add(
                        cost.element_owned_bytes,
                        container_owned_bytes,
                        source_label,
                    )?;
                    values.push(LoweredExpr {
                        element: PatternElement::Or(elements),
                        cost,
                    });
                }
                WorkItem::AssembleRepeat(min, max) => {
                    let child = values.pop().ok_or_else(|| {
                        diagnostic_error(
                            source_label,
                            DiagnosticCode::UnknownNode,
                            "bound repeat underflowed the compatibility value stack",
                        )
                    })?;
                    let cost = repeat_cost(child.cost, min, max, source_label)?;
                    self.ensure_cost(cost, source_label)?;
                    values.push(LoweredExpr {
                        element: PatternElement::Repeat {
                            element: Box::new(child.element),
                            min,
                            max,
                        },
                        cost,
                    });
                }
                WorkItem::ExitDefinition(id) => {
                    active_definitions.remove(&id);
                }
            }
        }

        if values.len() != 1 {
            return Err(diagnostic_error(
                source_label,
                DiagnosticCode::UnknownNode,
                "bound expression produced an invalid compatibility value stack",
            ));
        }
        Ok(values.pop().expect("one lowered value was checked above"))
    }

    fn charge_expansion_visit(&mut self, source_label: &Arc<str>) -> Result<(), CompileError> {
        self.expansion_work = checked_add(self.expansion_work, 1, source_label)?;
        if self.expansion_work > self.limits.max_expansion_work() {
            return Err(limit_error(
                source_label,
                "lingrep patterns program exceeds the NFA expansion work budget",
            ));
        }
        Ok(())
    }

    fn charge(&mut self, cost: NfaCost, source_label: &Arc<str>) -> Result<(), CompileError> {
        self.states = checked_add(self.states, cost.states, source_label)?;
        if self.states > self.limits.max_nfa_states() {
            return Err(limit_error(
                source_label,
                "lingrep patterns program exceeds the NFA states budget",
            ));
        }

        self.transitions = checked_add(self.transitions, cost.transitions, source_label)?;
        if self.transitions > self.limits.max_transitions() {
            return Err(limit_error(
                source_label,
                "lingrep patterns program exceeds the transitions budget",
            ));
        }
        Ok(())
    }

    fn charge_generated_owned_bytes(
        &mut self,
        bytes: usize,
        source_label: &Arc<str>,
    ) -> Result<(), CompileError> {
        self.generated_owned_bytes =
            checked_owned_add(self.generated_owned_bytes, bytes, source_label)?;
        if self.generated_owned_bytes > self.limits.max_generated_owned_bytes() {
            return Err(limit_error(
                source_label,
                "lingrep patterns program exceeds the generated owned bytes budget",
            ));
        }
        Ok(())
    }

    fn ensure_cost(&self, cost: NfaCost, source_label: &Arc<str>) -> Result<(), CompileError> {
        let states = checked_add(
            self.states,
            checked_add(cost.states, 1, source_label)?,
            source_label,
        )?;
        if states > self.limits.max_nfa_states() {
            return Err(limit_error(
                source_label,
                "lingrep patterns program exceeds the NFA states budget",
            ));
        }

        let transitions = checked_add(
            self.transitions,
            checked_add(cost.transitions, 1, source_label)?,
            source_label,
        )?;
        if transitions > self.limits.max_transitions() {
            return Err(limit_error(
                source_label,
                "lingrep patterns program exceeds the transitions budget",
            ));
        }
        Ok(())
    }
}

fn take_children(
    values: &mut Vec<LoweredExpr>,
    count: usize,
    source_label: &Arc<str>,
) -> Result<Vec<LoweredExpr>, CompileError> {
    let start = values.len().checked_sub(count).ok_or_else(|| {
        diagnostic_error(
            source_label,
            DiagnosticCode::UnknownNode,
            "bound expression underflowed the compatibility value stack",
        )
    })?;
    Ok(values.split_off(start))
}

pub(super) fn lower_to_nfa(
    program: &BoundProgram,
    limits: &CompileLimits,
    execution: &[ExecutionDefinition],
    assertion_occurrences: &BTreeMap<SourceSpan, AssertionOccurrenceId>,
) -> Result<NfaOutput, CompileError> {
    CompatibilityLowerer::new(program, limits, assertion_occurrences).lower(execution)
}

fn token_cost(
    operator: Operator,
    predicate_owned_bytes: usize,
    source_label: &Arc<str>,
) -> Result<NfaCost, CompileError> {
    let (states, transitions, consuming_transitions) = match operator {
        Operator::Exact(count) => (count, count, count),
        Operator::Range(min, max) => {
            let optional = max.checked_sub(min).ok_or_else(|| {
                diagnostic_error(
                    source_label,
                    DiagnosticCode::InvalidQuantifier,
                    "quantifier minimum exceeds its maximum",
                )
            })?;
            (
                max,
                checked_add(min, checked_mul(optional, 2, source_label)?, source_label)?,
                max,
            )
        }
        Operator::AtLeast(min) => {
            let consuming_transitions = checked_add(min, 1, source_label)?;
            (min, consuming_transitions, consuming_transitions)
        }
        Operator::AtMost(max) => (max, checked_mul(max, 2, source_label)?, max),
        Operator::ZeroOrOne => (1, 2, 1),
        Operator::ZeroOrMore => (1, 3, 2),
        Operator::OneOrMore => (2, 3, 2),
        Operator::Not => (1, 1, 1),
    };
    Ok(NfaCost {
        states,
        transitions,
        finals: 1,
        element_owned_bytes: predicate_owned_bytes,
        transition_owned_bytes: checked_owned_mul(
            predicate_owned_bytes,
            consuming_transitions,
            source_label,
        )?,
    })
}

fn repeat_cost(
    child: NfaCost,
    min: usize,
    max: Option<usize>,
    source_label: &Arc<str>,
) -> Result<NfaCost, CompileError> {
    let mut cost = NfaCost {
        finals: 1,
        ..NfaCost::default()
    };
    for _ in 0..min {
        cost = sequence_cost(cost, child, source_label)?;
    }

    if let Some(maximum) = max {
        for _ in min..maximum {
            let parent_finals = cost.finals;
            let child_finals = checked_mul(parent_finals, child.finals, source_label)?;
            cost.states = checked_add(
                checked_add(
                    cost.states,
                    checked_mul(parent_finals, child.states, source_label)?,
                    source_label,
                )?,
                1,
                source_label,
            )?;
            cost.transitions = checked_add(
                checked_add(
                    checked_add(
                        cost.transitions,
                        checked_mul(parent_finals, child.transitions, source_label)?,
                        source_label,
                    )?,
                    parent_finals,
                    source_label,
                )?,
                child_finals,
                source_label,
            )?;
            cost.transition_owned_bytes = checked_owned_add(
                cost.transition_owned_bytes,
                checked_owned_mul(parent_finals, child.transition_owned_bytes, source_label)?,
                source_label,
            )?;
            cost.finals = 1;
        }
    } else {
        cost.states = checked_add(
            checked_add(cost.states, 1, source_label)?,
            child.states,
            source_label,
        )?;
        cost.transitions = checked_add(
            checked_add(
                checked_add(cost.transitions, cost.finals, source_label)?,
                child.transitions,
                source_label,
            )?,
            child.finals,
            source_label,
        )?;
        cost.transition_owned_bytes = checked_owned_add(
            cost.transition_owned_bytes,
            child.transition_owned_bytes,
            source_label,
        )?;
        cost.finals = 1;
    }
    cost.element_owned_bytes = child.element_owned_bytes;
    Ok(cost)
}

fn sequence_cost(
    prefix: NfaCost,
    next: NfaCost,
    source_label: &Arc<str>,
) -> Result<NfaCost, CompileError> {
    Ok(NfaCost {
        states: checked_add(
            prefix.states,
            checked_mul(prefix.finals, next.states, source_label)?,
            source_label,
        )?,
        transitions: checked_add(
            prefix.transitions,
            checked_mul(prefix.finals, next.transitions, source_label)?,
            source_label,
        )?,
        finals: checked_mul(prefix.finals, next.finals, source_label)?,
        element_owned_bytes: checked_owned_add(
            prefix.element_owned_bytes,
            next.element_owned_bytes,
            source_label,
        )?,
        transition_owned_bytes: checked_owned_add(
            prefix.transition_owned_bytes,
            checked_owned_mul(prefix.finals, next.transition_owned_bytes, source_label)?,
            source_label,
        )?,
    })
}

fn alternation_cost(
    left: NfaCost,
    right: NfaCost,
    source_label: &Arc<str>,
) -> Result<NfaCost, CompileError> {
    Ok(NfaCost {
        states: checked_add(left.states, right.states, source_label)?,
        transitions: checked_add(left.transitions, right.transitions, source_label)?,
        finals: checked_add(left.finals, right.finals, source_label)?,
        element_owned_bytes: checked_owned_add(
            left.element_owned_bytes,
            right.element_owned_bytes,
            source_label,
        )?,
        transition_owned_bytes: checked_owned_add(
            left.transition_owned_bytes,
            right.transition_owned_bytes,
            source_label,
        )?,
    })
}

fn generated_output_owned_bytes(
    pattern_name: &str,
    metadata: &Metadata,
    cost: NfaCost,
    source_label: &Arc<str>,
) -> Result<usize, CompileError> {
    // The final catalog owns a one-element token-spec vector. Its compatibility
    // tree is then cloned once by StateMachineBuilder before each consuming
    // transition receives its own TokenSpec payload clone.
    let mut bytes = size_of::<PatternElement>();
    bytes = checked_owned_add(bytes, cost.element_owned_bytes, source_label)?;
    bytes = checked_owned_add(bytes, cost.transition_owned_bytes, source_label)?;
    bytes = checked_owned_add(
        bytes,
        metadata_owned_bytes(metadata, source_label)?,
        source_label,
    )?;

    // Two catalog names, one builder-local name, and two names per final state
    // (FinalState plus state_patterns) own the source-level pattern name.
    let final_name_copies = checked_owned_mul(cost.finals, 2, source_label)?;
    let name_copies = checked_owned_add(3, final_name_copies, source_label)?;
    checked_owned_add(
        bytes,
        checked_owned_mul(pattern_name.len(), name_copies, source_label)?,
        source_label,
    )
}

fn predicate_owned_bytes(
    predicate: &[SymbolicAttrValue],
    source_label: &Arc<str>,
) -> Result<usize, CompileError> {
    let mut bytes = checked_owned_mul(
        predicate.len(),
        size_of::<SymbolicAttrValue>(),
        source_label,
    )?;
    for attribute in predicate {
        bytes = checked_owned_add(
            bytes,
            match &attribute.value {
                AttrValueType::Single(value)
                | AttrValueType::SetHas(value)
                | AttrValueType::SetNotHas(value)
                | AttrValueType::Contains(value)
                | AttrValueType::NotContains(value) => value.len(),
                AttrValueType::OneOf(values)
                | AttrValueType::NotIn(values)
                | AttrValueType::SetHasAny(values)
                | AttrValueType::SetNotHasAny(values) => {
                    let mut value_bytes =
                        checked_owned_mul(values.len(), size_of::<CompactString>(), source_label)?;
                    for value in values.iter() {
                        value_bytes = checked_owned_add(value_bytes, value.len(), source_label)?;
                    }
                    value_bytes
                }
                // Charge pattern bytes per clone even though clones share the
                // compiled regex program. This keeps the bound conservative if
                // that implementation detail changes.
                AttrValueType::Regex(regex) | AttrValueType::NotRegex(regex) => {
                    regex.as_str().len()
                }
            },
            source_label,
        )?;
    }
    Ok(bytes)
}

/// Drops per-key spans at the public boundary.
///
/// `Pattern.metadata` is `HashMap<CompactString, Value>` and stays that way:
/// spans are a compiler-internal aid for diagnostics, not part of what a
/// compiled catalog publishes.
fn project_metadata(metadata: &Metadata) -> HashMap<CompactString, Value> {
    metadata
        .iter()
        .map(|(key, entry)| (key.clone(), entry.value.clone()))
        .collect()
}

/// Owned bytes of the *projected* metadata map a `Pattern` will carry.
///
/// Measured off the compiler's entries but sized as the projection: the span
/// beside each value is compiler-internal and never reaches the catalog. A
/// materialized default is a real copy in every `Pattern` that carries it, so
/// every copy is counted here — charging one authored entry once would make
/// the owned-memory budget understate the artifact.
fn metadata_owned_bytes(
    metadata: &Metadata,
    source_label: &Arc<str>,
) -> Result<usize, CompileError> {
    let entry_bytes = checked_owned_mul(
        metadata.len(),
        size_of::<(CompactString, Value)>(),
        source_label,
    )?;
    // Use a deterministic 2x entry-storage allowance. Exact allocator control
    // bytes remain structural memory bounded by the metadata/count budgets.
    let mut bytes = checked_owned_mul(entry_bytes, 2, source_label)?;
    for (key, entry) in metadata.iter() {
        bytes = checked_owned_add(bytes, key.len(), source_label)?;
        bytes = checked_owned_add(
            bytes,
            json_value_owned_bytes(&entry.value, source_label)?,
            source_label,
        )?;
    }
    Ok(bytes)
}

fn json_value_owned_bytes(value: &Value, source_label: &Arc<str>) -> Result<usize, CompileError> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(0),
        Value::String(value) => Ok(value.len()),
        Value::Array(values) => {
            let mut bytes = checked_owned_mul(values.len(), size_of::<Value>(), source_label)?;
            for value in values {
                bytes = checked_owned_add(
                    bytes,
                    json_value_owned_bytes(value, source_label)?,
                    source_label,
                )?;
            }
            Ok(bytes)
        }
        Value::Object(values) => {
            let entry_bytes =
                checked_owned_mul(values.len(), size_of::<(String, Value)>(), source_label)?;
            let mut bytes = checked_owned_mul(entry_bytes, 2, source_label)?;
            for (key, value) in values {
                bytes = checked_owned_add(bytes, key.len(), source_label)?;
                bytes = checked_owned_add(
                    bytes,
                    json_value_owned_bytes(value, source_label)?,
                    source_label,
                )?;
            }
            Ok(bytes)
        }
    }
}

fn checked_add(left: usize, right: usize, source_label: &Arc<str>) -> Result<usize, CompileError> {
    left.checked_add(right)
        .ok_or_else(|| limit_error(source_label, "generated NFA count overflowed"))
}

fn checked_mul(left: usize, right: usize, source_label: &Arc<str>) -> Result<usize, CompileError> {
    left.checked_mul(right)
        .ok_or_else(|| limit_error(source_label, "generated NFA count overflowed"))
}

fn checked_owned_add(
    left: usize,
    right: usize,
    source_label: &Arc<str>,
) -> Result<usize, CompileError> {
    left.checked_add(right)
        .ok_or_else(|| limit_error(source_label, "generated owned bytes overflowed"))
}

fn checked_owned_mul(
    left: usize,
    right: usize,
    source_label: &Arc<str>,
) -> Result<usize, CompileError> {
    left.checked_mul(right)
        .ok_or_else(|| limit_error(source_label, "generated owned bytes overflowed"))
}

fn limit_error(source_label: &Arc<str>, message: &str) -> CompileError {
    diagnostic_error(source_label, DiagnosticCode::LimitExceeded, message)
}

fn diagnostic_error(source_label: &Arc<str>, code: DiagnosticCode, message: &str) -> CompileError {
    CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
        code,
        Arc::clone(source_label),
        0..0,
        message,
        1,
        1,
    )])
}

fn diagnostic_at(
    sources: &super::model::SourceRegistry,
    span: super::model::SourceSpan,
    code: DiagnosticCode,
    message: &str,
) -> CompileError {
    let (line, column) = sources.line_column(span);
    CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
        code,
        sources.source_label(span.source()),
        usize::try_from(span.byte_range().start).expect("source offsets fit usize")
            ..usize::try_from(span.byte_range().end).expect("source offsets fit usize"),
        message,
        line,
        column,
    )])
}
