//! Which neighbouring sentences each `within=` lookaround scope reads.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use crate::corpus::{DivisionId, DocumentId};
use crate::fsm::{
    BoundCatalog, BoundExplanationError, LookaroundReach, LookaroundScope, SentenceExplanation,
};
use crate::token_match::TokenSequence;

use super::{BoundSentenceInput, SentenceLocation};

/// Computes, sentence by sentence, whether each scope's window continues
/// from the previous sentence: the sentence directly follows it in its
/// document, and for a division scope both have the same innermost division
/// of that kind.
pub(crate) struct WindowLinks {
    scopes: Vec<(Option<usize>, LookaroundReach)>,
    last: Option<(DocumentId, u64)>,
    last_divisions: Option<Arc<[Option<DivisionId>]>>,
}

impl WindowLinks {
    pub(crate) fn new(reach: BTreeMap<LookaroundScope, LookaroundReach>) -> Self {
        let mut divisions = 0..;
        let scopes = reach
            .into_iter()
            .map(|(scope, reach)| match scope {
                LookaroundScope::Document => (None, reach),
                LookaroundScope::Division(_) => (divisions.next(), reach),
            })
            .collect();
        Self {
            scopes,
            last: None,
            last_divisions: None,
        }
    }

    pub(crate) fn reach(&self) -> impl Iterator<Item = LookaroundReach> + '_ {
        self.scopes.iter().map(|(_, reach)| *reach)
    }

    pub(crate) fn scope_count(&self) -> usize {
        self.scopes.len()
    }

    /// Appends one link per scope for the sentence at `location`.
    ///
    /// `adjacent` states whether the sentence directly follows the previous
    /// one. Without it, the sentence directly follows the previous one when
    /// both are in one document and its `document_sentence_ordinal` is one
    /// greater.
    pub(crate) fn push(
        &mut self,
        location: &SentenceLocation,
        adjacent: Option<bool>,
        links: &mut Vec<bool>,
    ) {
        let position = (location.document_id, location.document_sentence_ordinal);
        let adjacent = adjacent.unwrap_or_else(|| {
            self.last
                .is_some_and(|(document, ordinal)| (document, ordinal + 1) == position)
        });
        self.last = Some(position);
        let divisions = location.lookaround_divisions.clone();
        let innermost = |divisions: &Option<Arc<[Option<DivisionId>]>>, index: usize| {
            divisions
                .as_ref()
                .and_then(|ids| ids.get(index).copied().flatten())
        };
        for (division, _) in &self.scopes {
            links.push(
                adjacent
                    && division.is_none_or(|index| {
                        innermost(&divisions, index)
                            .is_some_and(|id| Some(id) == innermost(&self.last_divisions, index))
                    }),
            );
        }
        self.last_divisions = divisions;
    }
}

pub(crate) struct Window {
    pub(crate) before: Range<usize>,
    pub(crate) after: Range<usize>,
    /// The window reaches the first sentence short of its preceding reach.
    pub(crate) open_before: bool,
    /// The window reaches the last sentence short of its following reach.
    pub(crate) open_after: bool,
}

/// The sentences before and after `sentences[index]` that the window of
/// scope `scope` covers, given `links` from [`WindowLinks`] with `scopes`
/// entries per sentence.
pub(crate) fn window_at<V: TokenSequence>(
    sentences: &[V],
    links: &[bool],
    scopes: usize,
    scope: usize,
    reach: LookaroundReach,
    index: usize,
) -> Window {
    let linked = |sentence: usize| links[sentence * scopes + scope];
    let mut start = index;
    let mut before = 0;
    while start > 0 && linked(start) && before < reach.before {
        start -= 1;
        before += sentences[start].len();
    }
    let mut end = index + 1;
    let mut after = 0;
    while end < sentences.len() && linked(end) && after < reach.after {
        after += sentences[end].len();
        end += 1;
    }
    Window {
        before: start..index,
        after: index + 1..end,
        open_before: start == 0 && before < reach.before,
        open_after: end == sentences.len() && after < reach.after,
    }
}

/// Explains `sentences[target]` as a run over `sentences` matches it, with
/// `within=` lookarounds reading the same windows.
///
/// `sentences` and their `locations` must be consecutive sentences of one
/// source, in order, after admission. Returns `Ok(None)` when a window
/// reaches the first sentence while `more_before` is set, or the last while
/// `more_after` is set, short of its reach: sentences beyond that end can
/// change the explanation.
///
/// # Errors
///
/// Returns an error when a sentence does not match the catalog's schema, or
/// the catalog's result policy cannot resolve an anchor.
pub fn explain_in_windows(
    catalog: &BoundCatalog,
    locations: &[SentenceLocation],
    sentences: &[BoundSentenceInput],
    target: usize,
    more_before: bool,
    more_after: bool,
) -> Result<Option<SentenceExplanation>, BoundExplanationError> {
    debug_assert_eq!(locations.len(), sentences.len());
    let reach = catalog.compiled.lookaround_reach();
    let mut links = WindowLinks::new(reach.clone());
    let mut linked = Vec::with_capacity(sentences.len() * links.scope_count());
    for location in locations {
        links.push(location, None, &mut linked);
    }
    let mut windows = Vec::with_capacity(reach.len());
    for (scope, (name, scope_reach)) in reach.iter().enumerate() {
        let window = window_at(sentences, &linked, reach.len(), scope, *scope_reach, target);
        if (window.open_before && more_before) || (window.open_after && more_after) {
            return Ok(None);
        }
        windows.push((name, &sentences[window.before], &sentences[window.after]));
    }
    catalog
        .explain_input_in_windows(&sentences[target], &windows)
        .map(Some)
}
