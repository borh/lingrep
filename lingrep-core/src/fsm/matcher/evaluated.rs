use super::assertion::AssertionSession;
use super::predicate::PredicateSession;
use super::program::{Program, ProgramMatch};
use crate::corpus::conllu::EnhancedGraph;
use crate::fsm::Match;
use crate::token_match::TokenSequence;

/// A definition the dense machine cannot recognize, recognized instead by the
/// tree evaluator.
///
/// Named for how it runs rather than for `same_as`, the construct that selects
/// it. Span equality compares two spans, which
/// a set-based simulation carrying no per-thread state cannot do. What selects
/// this plan is a property of the definition, not the name of a feature, so a
/// second construct needing the same treatment joins it here rather than
/// arriving as a parallel plan kind.
///
/// Distinct from [`CapturePlan`], which is not an alternative recognizer at all:
/// there the dense machine accepts the span and the program replays it to
/// recover bindings. Here the program *is* the recognizer, so the definition is
/// routed around the recognition NFA in `compiler::nfa` and contributes no
/// states to it.
///
/// [`CapturePlan`]: super::capture::CapturePlan
#[derive(Clone)]
pub(crate) struct EvaluatedPlan(pub(crate) Program);

impl EvaluatedPlan {
    pub(crate) fn find_matches_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Vec<Match> {
        self.to_matches(self.0.matches_without_assertions(predicates, tokens))
    }

    pub(crate) fn find_matches_without_assertions_with_graph<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        graph: &EnhancedGraph,
    ) -> Vec<Match> {
        self.to_matches(
            self.0
                .matches_without_assertions_with_graph(predicates, tokens, graph),
        )
    }

    pub(crate) fn find_matches<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Vec<Match> {
        let matches = self.0.matches(predicates, tokens, session);
        self.to_matches(matches)
    }

    pub(crate) fn find_matches_with_graph<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut AssertionSession<'_, '_, S>,
        graph: &EnhancedGraph,
    ) -> Vec<Match> {
        let matches = self
            .0
            .matches_with_graph(predicates, tokens, session, graph);
        self.to_matches(matches)
    }

    fn to_matches(&self, matches: Vec<ProgramMatch>) -> Vec<Match> {
        matches
            .into_iter()
            .map(|matched| Match {
                pattern: self.0.definition().name.clone(),
                start: matched.start,
                end: matched.end,
                captures: matched.captures,
                graph_bindings: matched.graph_bindings,
                lexical: None,
            })
            .collect()
    }
}
