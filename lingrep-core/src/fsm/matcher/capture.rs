use super::assertion::AssertionSession;
use super::predicate::PredicateSession;
use super::program::Program;
use super::resource::{MatchResourceError, OutputBudget};
use crate::fsm::Capture;
use crate::token_match::TokenSequence;

#[derive(Clone)]
pub(crate) struct CapturePlan(pub(crate) Program);

impl CapturePlan {
    pub(crate) fn extract_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        output: &OutputBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
    ) -> Result<Option<Box<[Capture]>>, MatchResourceError> {
        self.0
            .extract_without_assertions(output, predicates, tokens, start, end)
    }

    pub(crate) fn extract<S: TokenSequence + ?Sized>(
        &self,
        output: &OutputBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Result<Option<Box<[Capture]>>, MatchResourceError> {
        self.0
            .extract(output, predicates, tokens, start, end, session)
    }
}
