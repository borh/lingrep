//! Neighbouring-sentence tokens that a `within=` lookaround body may match.

use crate::fsm::LookaroundScope;
use crate::token_match::{
    AttributeKey, AttributeLocator, AttributeValueRef, TokenAttributeIndex, TokenSequence,
};
use compact_str::CompactString;
use std::collections::BTreeMap;
use std::ops::Range;

/// Consecutive tokens of one neighbouring sentence.
pub struct ContextSegment<'a, S: TokenSequence + ?Sized> {
    pub tokens: &'a S,
    /// Token indices within `tokens`. `^` holds before the segment when the
    /// range starts at `0`, and `$` holds after it when the range ends at
    /// `tokens.len()`.
    pub range: Range<usize>,
}

/// The tokens of one document or division around the sentence being matched,
/// in document order: `before` ends where the sentence starts and `after`
/// starts where it ends.
pub struct ContextWindow<'a, S: TokenSequence + ?Sized> {
    pub before: Vec<ContextSegment<'a, S>>,
    pub after: Vec<ContextSegment<'a, S>>,
}

/// The context window for each scope a caller can supply.
///
/// A `within=` lookaround whose scope has no window matches sentence-locally.
pub struct LookaroundContext<'a, S: TokenSequence + ?Sized> {
    pub windows: BTreeMap<LookaroundScope, ContextWindow<'a, S>>,
}

impl<S: TokenSequence + ?Sized> Default for LookaroundContext<'_, S> {
    fn default() -> Self {
        Self {
            windows: BTreeMap::new(),
        }
    }
}

/// How many context tokens a catalog's `within=` lookarounds of one scope can
/// read on each side of a sentence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LookaroundReach {
    pub before: usize,
    pub after: usize,
}

pub(super) struct WindowTokens<'a, S: TokenSequence + ?Sized> {
    segments: Vec<(&'a S, Range<usize>)>,
    len: usize,
}

impl<'a, S: TokenSequence + ?Sized> WindowTokens<'a, S> {
    pub(super) fn join(
        sentence: &'a S,
        window: &ContextWindow<'a, S>,
        before: usize,
        after: usize,
    ) -> (Self, usize, Vec<bool>, Vec<bool>) {
        let mut leading = Vec::new();
        let mut remaining = before;
        for segment in window.before.iter().rev() {
            if remaining == 0 {
                break;
            }
            let start = segment
                .range
                .start
                .max(segment.range.end.saturating_sub(remaining));
            remaining -= segment.range.end - start;
            leading.push((segment.tokens, start..segment.range.end));
        }
        leading.reverse();
        let offset = before - remaining;

        let mut segments = leading;
        segments.push((sentence, 0..sentence.len()));
        let mut remaining = after;
        for segment in &window.after {
            if remaining == 0 {
                break;
            }
            let end = segment
                .range
                .end
                .min(segment.range.start.saturating_add(remaining));
            remaining -= end - segment.range.start;
            segments.push((segment.tokens, segment.range.start..end));
        }

        let len = segments.iter().map(|(_, range)| range.len()).sum();
        let mut starts = vec![false; len + 1];
        let mut ends = vec![false; len + 1];
        let mut position = 0;
        for (tokens, range) in &segments {
            if range.start == 0 {
                starts[position] = true;
            }
            position += range.len();
            if range.end == tokens.len() {
                ends[position] = true;
            }
        }
        (Self { segments, len }, offset, starts, ends)
    }

    fn locate(&self, mut token: usize) -> Option<(&'a S, usize)> {
        for (tokens, range) in &self.segments {
            if token < range.len() {
                return Some((tokens, range.start + token));
            }
            token -= range.len();
        }
        None
    }
}

impl<S: TokenSequence + ?Sized> TokenSequence for WindowTokens<'_, S> {
    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, token: usize, attribute: TokenAttributeIndex) -> Option<&CompactString> {
        let (tokens, index) = self.locate(token)?;
        tokens.get(index, attribute)
    }

    fn get_key(&self, token: usize, key: &AttributeKey) -> Option<&CompactString> {
        let (tokens, index) = self.locate(token)?;
        tokens.get_key(index, key)
    }

    fn value_key(&self, token: usize, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
        let (tokens, index) = self.locate(token)?;
        tokens.value_key(index, key)
    }

    fn scalar_key(&self, token: usize, key: &AttributeKey) -> Option<&str> {
        let (tokens, index) = self.locate(token)?;
        tokens.scalar_key(index, key)
    }

    fn value_locator(
        &self,
        token: usize,
        locator: &AttributeLocator,
    ) -> Option<AttributeValueRef<'_>> {
        let (tokens, index) = self.locate(token)?;
        tokens.value_locator(index, locator)
    }
}
