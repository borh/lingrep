use super::assertion::AssertionSession;
use super::predicate::PredicateSession;
use super::program::Program;
use crate::fsm::Capture;
use crate::token_match::TokenSequence;

#[derive(Clone)]
pub(crate) struct CapturePlan(pub(crate) Program);

impl CapturePlan {
    pub(crate) fn extract_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
    ) -> Option<Box<[Capture]>> {
        self.0
            .extract_without_assertions(predicates, tokens, start, end)
    }

    pub(crate) fn extract<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Option<Box<[Capture]>> {
        self.0.extract(predicates, tokens, start, end, session)
    }
}
