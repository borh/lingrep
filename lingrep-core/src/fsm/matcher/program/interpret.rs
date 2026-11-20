use super::{Candidate, Program, RuntimeExpr};
use crate::fsm::compiler::model::InterpretationRecipe;
use crate::fsm::matcher::predicate::{PREDICATE_SITE_CAPTURE_TOKEN, PredicateSession};
use crate::fsm::matcher::resource::{MatchBudget, MatchResourceError};
use crate::fsm::{Capture, Interpretation, RoleOccurrence};
use crate::token_match::TokenSequence;
use std::collections::BTreeMap;

type Span = (usize, usize);
type Event = (usize, usize, usize);

struct State {
    position: usize,
    slots: Vec<Option<Span>>,
    events: Vec<Event>,
}

struct Replay<'a, 'p, S: ?Sized> {
    budget: &'a mut MatchBudget,
    program: &'a Program,
    recipe: &'a InterpretationRecipe,
    predicates: &'a PredicateSession<'p>,
    tokens: &'a S,
    end: usize,
}

impl<S: TokenSequence + ?Sized> Replay<'_, '_, S> {
    fn charge(&mut self, steps: usize, bytes: usize) -> Result<(), MatchResourceError> {
        self.budget
            .interpretation(&self.program.definition.name, steps, bytes)
    }

    fn fork(&mut self, state: &State) -> Result<State, MatchResourceError> {
        let bytes = state
            .slots
            .len()
            .saturating_mul(size_of::<Option<Span>>())
            .saturating_add(state.events.len().saturating_mul(size_of::<Event>()));
        self.charge(1 + state.events.len() + state.slots.len(), bytes)?;
        Ok(State {
            position: state.position,
            slots: state.slots.clone(),
            events: state.events.clone(),
        })
    }

    fn push<T>(&mut self, out: &mut Vec<T>, value: T) -> Result<(), MatchResourceError> {
        if out.len() == out.capacity() {
            let capacity = out.capacity().saturating_mul(2).max(4);
            self.charge(1, capacity.saturating_mul(size_of::<T>()))?;
            out.reserve_exact(capacity - out.len());
        }
        out.push(value);
        Ok(())
    }

    fn add(&mut self, states: &mut Vec<State>, next: State) -> Result<(), MatchResourceError> {
        let compare_slots = self.program.has_node_or_span_filters();
        for old in states.iter() {
            self.charge(
                1 + old.events.len().min(next.events.len())
                    + if compare_slots { old.slots.len() } else { 0 },
                0,
            )?;
            if old.position == next.position
                && old.events == next.events
                && (!compare_slots || old.slots == next.slots)
            {
                return Ok(());
            }
        }
        self.push(states, next)
    }

    fn run(&mut self, expr: &RuntimeExpr, state: State) -> Result<Vec<State>, MatchResourceError> {
        self.charge(1, 0)?;
        let mut out = Vec::new();
        match expr {
            RuntimeExpr::Token(test) => {
                if state.position < self.end
                    && self
                        .predicates
                        .matches_at_site::<PREDICATE_SITE_CAPTURE_TOKEN, _>(
                            *test,
                            self.tokens,
                            state.position,
                        )
                {
                    self.push(
                        &mut out,
                        State {
                            position: state.position + 1,
                            ..state
                        },
                    )?;
                }
            }
            RuntimeExpr::Sequence(parts) => {
                self.push(&mut out, state)?;
                for part in parts {
                    let mut next = Vec::new();
                    for previous in out {
                        for found in self.run(part, previous)? {
                            self.add(&mut next, found)?;
                        }
                    }
                    out = next;
                    if out.is_empty() {
                        break;
                    }
                }
            }
            RuntimeExpr::Alternation(parts) => {
                for part in parts {
                    let branch = self.fork(&state)?;
                    for found in self.run(part, branch)? {
                        self.add(&mut out, found)?;
                    }
                }
            }
            RuntimeExpr::Capture { id, expr, .. } => {
                let start = state.position;
                out = self.run(expr, state)?;
                for found in &mut out {
                    found.slots[id.index()] = Some((start, found.position));
                    if let Some(template) = self.recipe.captures.get(id) {
                        let event = (start, found.position, *template);
                        self.charge(1 + found.events.len(), 0)?;
                        let i = found.events.partition_point(|item| *item <= event);
                        self.push(&mut found.events, event)?;
                        found.events[i..].rotate_right(1);
                    }
                }
            }
            RuntimeExpr::Repeat { expr, min, max } => {
                let ceiling = self
                    .end
                    .saturating_sub(state.position)
                    .min(max.unwrap_or(usize::MAX));
                let mut levels = Vec::new();
                let mut level = Vec::new();
                self.push(&mut level, state)?;
                for depth in 0..=ceiling {
                    let mut next = Vec::new();
                    if depth < ceiling {
                        for previous in &level {
                            let branch = self.fork(previous)?;
                            for found in self.run(expr, branch)? {
                                if found.position > previous.position {
                                    self.add(&mut next, found)?;
                                }
                            }
                        }
                    }
                    if depth >= *min {
                        self.push(&mut levels, level)?;
                    }
                    if next.is_empty() {
                        break;
                    }
                    level = next;
                }
                for level in levels.into_iter().rev() {
                    for found in level {
                        self.add(&mut out, found)?;
                    }
                }
            }
            RuntimeExpr::Assertion(_)
            | RuntimeExpr::SameAs { .. }
            | RuntimeExpr::Intersection(_)
            | RuntimeExpr::AnyOrder(_) => {
                return Err(MatchResourceError::InterpretationReplay {
                    pattern: self.program.definition.name.clone(),
                });
            }
        }
        Ok(out)
    }

    fn publish(&mut self, events: Vec<Event>) -> Result<Interpretation, MatchResourceError> {
        use super::{btree_storage_bytes, compact_string_heap_bytes};
        use crate::fsm::AnalyticalValue;
        use compact_str::CompactString;
        self.budget
            .output
            .reserve_buffer::<RoleOccurrence>(events.len())?;
        let mut roles = Vec::with_capacity(events.len());
        let mut classification: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (start, end, template) in events {
            let (role, labels) = &self.recipe.roles[template];
            self.charge(1 + labels.len(), 0)?;
            let classify = self.recipe.classify_by.as_ref() == Some(role);
            let mut bytes =
                compact_string_heap_bytes(role)
                    .saturating_add(btree_storage_bytes::<CompactString, AnalyticalValue>(
                        labels.len(),
                    ));
            for (key, value) in labels {
                let string = match value {
                    AnalyticalValue::String(value) => compact_string_heap_bytes(value),
                    _ => 0,
                };
                let label_bytes = compact_string_heap_bytes(key).saturating_add(string);
                bytes = bytes.saturating_add(label_bytes);
                if classify {
                    // The growing value lists and their final boxed copies coexist during conversion.
                    bytes = bytes
                        .saturating_add(label_bytes)
                        .saturating_add(size_of::<AnalyticalValue>() * 8)
                        .saturating_add(btree_storage_bytes::<CompactString, Vec<AnalyticalValue>>(
                            1,
                        ))
                        .saturating_add(
                            btree_storage_bytes::<CompactString, Box<[AnalyticalValue]>>(1),
                        );
                }
            }
            self.budget.output.reserve(bytes)?;
            if classify {
                for (key, value) in labels {
                    self.charge((1 + key.len()).saturating_mul(1 + classification.len()), 0)?;
                    let values = classification.entry(key.clone()).or_default();
                    let value_width = match value {
                        AnalyticalValue::String(s) => s.len(),
                        _ => 1,
                    };
                    self.charge(
                        (1 + value_width).saturating_mul(values.len().max(1).ilog2() as usize + 1)
                            + values.len(),
                        0,
                    )?;
                    if let Err(index) = values.binary_search(value) {
                        values.insert(index, value.clone());
                    }
                }
            }
            roles.push(RoleOccurrence {
                role: role.clone(),
                labels: labels.clone(),
                start,
                end,
            });
        }
        let classification = classification
            .into_iter()
            .map(|(key, values)| (key, values.into_boxed_slice()))
            .collect();
        Ok(Interpretation {
            roles: roles.into_boxed_slice(),
            classification,
        })
    }
}

impl Program {
    pub(in crate::fsm::matcher) fn interpreted_captures<S: TokenSequence + ?Sized>(
        &self,
        recipe: &InterpretationRecipe,
        budget: &mut MatchBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        range: std::ops::Range<usize>,
    ) -> Result<(Box<[Capture]>, Box<[Interpretation]>), MatchResourceError> {
        self.interpreted_candidates(recipe, budget, predicates, tokens, range, |_, _| Ok(true))
    }

    pub(super) fn interpreted_candidates<S: TokenSequence + ?Sized>(
        &self,
        recipe: &InterpretationRecipe,
        budget: &mut MatchBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        range: std::ops::Range<usize>,
        mut accept: impl FnMut(&Candidate, &mut MatchBudget) -> Result<bool, MatchResourceError>,
    ) -> Result<(Box<[Capture]>, Box<[Interpretation]>), MatchResourceError> {
        let (start, end) = (range.start, range.end);
        let mut replay = Replay {
            budget,
            program: self,
            recipe,
            predicates,
            tokens,
            end,
        };
        replay.charge(1, self.captures.len() * size_of::<Option<Span>>())?;
        let initial = State {
            position: start,
            slots: vec![None; self.captures.len()],
            events: Vec::new(),
        };
        let mut histories: Vec<Vec<Event>> = Vec::new();
        let mut captures = None;
        for state in replay
            .run(&self.runtime, initial)?
            .into_iter()
            .filter(|state| state.position == end)
        {
            let candidate = Candidate {
                position: end,
                slots: state.slots,
                any_order_branches: Vec::new(),
            };
            if !accept(&candidate, replay.budget)? {
                continue;
            }
            if captures.is_none() {
                captures = Some(self.public_captures(&candidate, &replay.budget.output)?);
            }
            let comparisons = histories.len().max(1).ilog2() as usize + 1;
            replay.charge(
                comparisons.saturating_mul(1 + state.events.len()) + histories.len(),
                0,
            )?;
            if let Err(index) = histories.binary_search(&state.events) {
                replay.push(&mut histories, state.events)?;
                histories[index..].rotate_right(1);
            }
        }
        replay
            .budget
            .output
            .reserve_buffer::<Interpretation>(histories.len())?;
        let mut alternatives = Vec::with_capacity(histories.len());
        for history in histories {
            alternatives.push(replay.publish(history)?);
        }
        Ok((
            captures.ok_or_else(|| MatchResourceError::InterpretationReplay {
                pattern: self.definition.name.clone(),
            })?,
            alternatives.into_boxed_slice(),
        ))
    }
}
