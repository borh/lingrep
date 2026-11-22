use super::types::*;
#[cfg(test)]
use crate::fsm::AttrValueType;
use compact_str::CompactString;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StateMachineData {
    pub(crate) state_transitions: HashMap<usize, Vec<(TransitionLabel, usize)>>,

    pub(crate) final_states: HashSet<FinalState>,

    pub(crate) state_patterns: HashMap<usize, Vec<CompactString>>,
}

#[derive(Clone, Debug)]
struct Transition {
    from: usize,
    label: TransitionLabel,
    to: usize,
}

/// Mutable construction of immutable NFA state-machine data.
pub(crate) struct StateMachineBuilder {
    next_state_id: usize,
    transitions: Vec<Transition>,
    final_states: HashSet<FinalState>,
    state_patterns: HashMap<usize, Vec<CompactString>>,
}

impl Default for StateMachineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachineBuilder {
    pub(crate) fn new() -> Self {
        Self {
            next_state_id: 1, // State 0 is reserved for start
            transitions: Vec::new(),
            final_states: HashSet::new(),
            state_patterns: HashMap::new(),
        }
    }

    pub(crate) fn add_pattern(&mut self, name: &str, pattern: &Pattern) -> Result<(), String> {
        let pattern_name = CompactString::from(name);

        // A dedicated start keeps different patterns' initial transitions independent.
        let pattern_start_state = self.allocate_state();

        self.add_epsilon(0, pattern_start_state);

        let element = if pattern.token_specs.len() == 1 {
            pattern.token_specs[0].clone()
        } else {
            PatternElement::And(pattern.token_specs.clone())
        };

        let final_states =
            self.process_pattern_element(&element, pattern_start_state, &pattern_name)?;

        for &state in &final_states {
            self.add_pattern_to_state(state, &pattern_name);
            self.final_states.insert(FinalState {
                state_id: state,
                pattern_name: pattern_name.clone(),
            });
        }

        Ok(())
    }

    pub(crate) fn build(self) -> StateMachineData {
        let mut state_transitions: HashMap<usize, Vec<(TransitionLabel, usize)>> = HashMap::new();

        for transition in self.transitions {
            state_transitions
                .entry(transition.from)
                .or_default()
                .push((transition.label, transition.to));
        }

        StateMachineData {
            state_transitions,
            final_states: self.final_states,
            state_patterns: self.state_patterns,
        }
    }

    fn allocate_state(&mut self) -> usize {
        let id = self.next_state_id;
        self.next_state_id += 1;
        id
    }

    fn add_pattern_to_state(&mut self, state: usize, pattern_name: &CompactString) {
        self.state_patterns
            .entry(state)
            .or_default()
            .push(pattern_name.clone());
        if let Some(patterns) = self.state_patterns.get_mut(&state) {
            patterns.sort();
        }
    }

    fn add_transition(&mut self, from: usize, spec: TokenSpec, to: usize) {
        self.transitions.push(Transition {
            from,
            label: TransitionLabel::consume(spec),
            to,
        });
    }

    fn add_epsilon(&mut self, from: usize, to: usize) {
        self.transitions.push(Transition {
            from,
            label: TransitionLabel::epsilon(),
            to,
        });
    }

    fn add_assertion(&mut self, from: usize, assertion: PositionAssertion, to: usize) {
        self.transitions.push(Transition {
            from,
            label: TransitionLabel::assertion(assertion),
            to,
        });
    }

    fn process_pattern_element(
        &mut self,
        element: &PatternElement,
        current_state: usize,
        pattern_name: &CompactString,
    ) -> Result<Vec<usize>, String> {
        match element {
            PatternElement::And(elements) => {
                self.process_and_sequence(elements, current_state, pattern_name)
            }
            PatternElement::Or(elements) => {
                self.process_or_branches(elements, current_state, pattern_name)
            }
            PatternElement::TokenSpec(token_spec) => {
                self.process_token_spec(token_spec, current_state, pattern_name)
            }
            PatternElement::Repeat { element, min, max } => {
                self.process_repeated_element(element, current_state, pattern_name, *min, *max)
            }
            PatternElement::Assertion(assertion) => {
                let next = self.allocate_state();
                self.add_assertion(current_state, assertion.clone(), next);
                Ok(vec![next])
            }
        }
    }

    fn process_repeated_element(
        &mut self,
        element: &PatternElement,
        start: usize,
        pattern_name: &CompactString,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<usize>, String> {
        let mut current = vec![start];
        for _ in 0..min {
            current = self.process_element_from_states(element, &current, pattern_name)?;
        }

        if let Some(maximum) = max {
            for _ in min..maximum {
                let join = self.allocate_state();
                for state in &current {
                    self.add_epsilon(*state, join);
                }
                let consumed = self.process_element_from_states(element, &current, pattern_name)?;
                for state in consumed {
                    self.add_epsilon(state, join);
                }
                current = vec![join];
            }
            Ok(current)
        } else {
            let loop_state = self.allocate_state();
            for state in current {
                self.add_epsilon(state, loop_state);
            }
            let consumed = self.process_pattern_element(element, loop_state, pattern_name)?;
            for state in consumed {
                self.add_epsilon(state, loop_state);
            }
            Ok(vec![loop_state])
        }
    }

    fn process_element_from_states(
        &mut self,
        element: &PatternElement,
        states: &[usize],
        pattern_name: &CompactString,
    ) -> Result<Vec<usize>, String> {
        let mut finals = Vec::new();
        for state in states {
            finals.extend(self.process_pattern_element(element, *state, pattern_name)?);
        }
        Ok(finals)
    }

    fn process_and_sequence(
        &mut self,
        elements: &[PatternElement],
        start_state: usize,
        pattern_name: &CompactString,
    ) -> Result<Vec<usize>, String> {
        let mut current_states = vec![start_state];

        for elem in elements {
            let mut next_states = Vec::new();
            for &state in &current_states {
                next_states.extend(self.process_pattern_element(elem, state, pattern_name)?);
            }
            current_states = next_states;
        }

        // Do NOT add pattern names here - they will be added by add_pattern()
        // for the final states. This prevents intermediate states from getting
        // pattern names when processing multi-token patterns.

        Ok(current_states)
    }

    fn process_or_branches(
        &mut self,
        elements: &[PatternElement],
        start_state: usize,
        pattern_name: &CompactString,
    ) -> Result<Vec<usize>, String> {
        let mut all_final_states = Vec::new();

        for elem in elements {
            all_final_states.extend(self.process_pattern_element(
                elem,
                start_state,
                pattern_name,
            )?);
        }

        Ok(all_final_states)
    }

    fn process_token_spec(
        &mut self,
        token_spec: &TokenSpec,
        current_state: usize,
        _pattern_name: &CompactString,
    ) -> Result<Vec<usize>, String> {
        let next = self.allocate_state();
        self.add_transition(current_state, token_spec.clone(), next);

        // Do NOT add pattern name here - it will be added by the parent context
        // (process_and_sequence or add_pattern)
        // This prevents intermediate states from getting pattern names

        Ok(vec![next])
    }
}

#[cfg(test)]
mod state_machine_data_tests {
    use super::*;

    #[test]
    fn test_builder_allocate_state() {
        let mut builder = StateMachineBuilder::new();
        assert_eq!(builder.allocate_state(), 1);
        assert_eq!(builder.allocate_state(), 2);
        assert_eq!(builder.allocate_state(), 3);
    }

    #[test]
    fn test_builder_add_pattern_to_state() {
        let mut builder = StateMachineBuilder::new();
        builder.add_pattern_to_state(0, &CompactString::from("test_pattern"));

        let data = builder.build();
        assert_eq!(
            data.state_patterns
                .get(&0)
                .expect("Pattern should exist")
                .len(),
            1
        );
        assert_eq!(
            data.state_patterns.get(&0).expect("Pattern should exist")[0],
            "test_pattern"
        );
    }

    #[test]
    fn test_builder_add_transition() {
        let mut builder = StateMachineBuilder::new();
        let spec = TokenSpec {
            attrs: vec![AttrValue {
                attr: crate::token_match::TokenAttributeIndex::Pos.into(),
                value: AttrValueType::Single(CompactString::from("NOUN")),
                complement: false,
            }],
        };

        builder.add_transition(0, spec.clone(), 1);

        let data = builder.build();
        assert_eq!(
            data.state_transitions
                .get(&0)
                .expect("Transition should exist")
                .len(),
            1
        );
        assert_eq!(
            data.state_transitions
                .get(&0)
                .expect("Transition should exist")[0]
                .1,
            1
        );
    }

    #[test]
    fn test_process_pattern_element_and_sequence() {
        let mut builder = StateMachineBuilder::new();

        let element = PatternElement::And(vec![
            PatternElement::TokenSpec(TokenSpec {
                attrs: vec![AttrValue {
                    attr: crate::token_match::TokenAttributeIndex::Pos.into(),
                    value: AttrValueType::Single(CompactString::from("NOUN")),
                    complement: false,
                }],
            }),
            PatternElement::TokenSpec(TokenSpec {
                attrs: vec![AttrValue {
                    attr: crate::token_match::TokenAttributeIndex::Pos.into(),
                    value: AttrValueType::Single(CompactString::from("VERB")),
                    complement: false,
                }],
            }),
        ]);

        let result = builder.process_pattern_element(&element, 0, &CompactString::from("test"));

        assert!(result.is_ok());
        let final_states = result.unwrap();
        assert_eq!(final_states.len(), 1);
    }

    #[test]
    fn test_process_token_spec_simple() {
        let mut builder = StateMachineBuilder::new();
        let spec = TokenSpec {
            attrs: vec![AttrValue {
                attr: crate::token_match::TokenAttributeIndex::Pos.into(),
                value: AttrValueType::Single(CompactString::from("NOUN")),
                complement: false,
            }],
        };

        let result = builder
            .process_token_spec(&spec, 0, &CompactString::from("test"))
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], 1);
    }
}
