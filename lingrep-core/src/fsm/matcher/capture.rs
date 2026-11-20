use super::assertion::AssertionSession;
use super::predicate::PredicateSession;
use super::program::Program;
use super::resource::{MatchBudget, MatchResourceError};
use crate::fsm::Match;
use crate::token_match::TokenSequence;

#[derive(Clone)]
pub(crate) struct CapturePlan(pub(crate) Program);

impl CapturePlan {
    pub(crate) fn extract_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        matched: &mut Match,
    ) -> Result<(), MatchResourceError> {
        if let Some(recipe) = &self.0.definition().interpretation {
            (matched.captures, matched.interpretations) = self.0.interpreted_captures(
                recipe,
                budget,
                predicates,
                tokens,
                matched.start..matched.end,
            )?;
        } else {
            matched.captures = self
                .0
                .extract_without_assertions(
                    &budget.output,
                    predicates,
                    tokens,
                    matched.start,
                    matched.end,
                )?
                .expect("NFA-accepted capture span must have a prioritized extraction");
        }
        Ok(())
    }

    pub(crate) fn extract<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        matched: &mut Match,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Result<(), MatchResourceError> {
        if self.0.definition().interpretation.is_some() {
            return self.extract_without_assertions(budget, predicates, tokens, matched);
        }
        matched.captures = self
            .0
            .extract(
                &budget.output,
                predicates,
                tokens,
                matched.start,
                matched.end,
                session,
            )?
            .expect("NFA-accepted capture span must have a prioritized extraction");
        Ok(())
    }
}
