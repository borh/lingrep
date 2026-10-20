use compact_str::CompactString;

use crate::token_match::{AttributeKey, AttributeOrdinal, AttributeValueRef, TokenAttributeIndex};

#[doc(hidden)]
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttributeLocator {
    key: AttributeKey,
    ordinal: Option<AttributeOrdinal>,
}

impl AttributeLocator {
    pub(crate) fn symbolic(key: AttributeKey) -> Self {
        Self { key, ordinal: None }
    }

    pub(crate) fn bind(&mut self, ordinal: AttributeOrdinal) {
        self.ordinal = Some(ordinal);
    }

    pub(crate) fn key(&self) -> &AttributeKey {
        &self.key
    }

    pub(crate) const fn ordinal(&self) -> Option<AttributeOrdinal> {
        self.ordinal
    }
}

pub trait TokenInfoTrait {
    fn get(&self, index: TokenAttributeIndex) -> Option<&CompactString>;

    fn get_key(&self, key: &AttributeKey) -> Option<&CompactString> {
        self.get(TokenAttributeIndex::from_attribute_key(key)?)
    }

    fn value_key(&self, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
        self.get_key(key)
            .map(|value| AttributeValueRef::Scalar(value.as_str()))
    }

    fn scalar_key(&self, key: &AttributeKey) -> Option<&str> {
        match self.value_key(key)? {
            AttributeValueRef::Scalar(value) => Some(value),
            AttributeValueRef::StringSet(_) => None,
        }
    }

    fn value_locator(&self, locator: &AttributeLocator) -> Option<AttributeValueRef<'_>> {
        self.value_key(locator.key())
    }
}

/// Indexed read-only token storage used by matching and publication.
/// A half-open range of Unicode code points within one sentence's own text.
///
/// Code points, not UTF-8 bytes, and not the byte `SourceRange` the analysis
/// layer uses internally: this is the quantity a standoff-annotation consumer
/// addresses text by, and every such consumer reachable from here — brat, NIF,
/// Python, R — counts characters. The conversion happens once, at the reader
/// that knows the sentence text, rather than at each consumer that would have
/// to guess which unit it was handed.
///
/// Sentence-relative, because that is the only origin defined on every path
/// that can supply one. A pretokenized CoNLL-U file has no underlying document
/// to be standoff against — offsets into the `.conllu` bytes would address the
/// annotation, not the text it annotates — so the sentence is where the two
/// halves of the pipeline can agree.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CharSpan {
    /// Inclusive first code point.
    pub start: u32,
    /// Exclusive last code point.
    pub end: u32,
}

impl CharSpan {
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Converts ordered UTF-8 surface ranges to sentence-relative code points.
    /// Every word in a multiword surface receives its enclosing surface range.
    /// Returns `None` for invalid boundaries or incomplete word coverage.
    pub fn from_surface_ranges(
        text: &str,
        word_count: usize,
        surfaces: impl IntoIterator<Item = (std::ops::Range<usize>, std::ops::Range<usize>)>,
    ) -> Option<Vec<Self>> {
        let mut spans = Vec::with_capacity(word_count);
        let mut byte_end = 0;
        let mut char_end = 0u32;
        for (source, words) in surfaces {
            if source.start < byte_end
                || source.start >= source.end
                || words.start != spans.len()
                || words.start >= words.end
                || words.end > word_count
            {
                return None;
            }
            let gap = u32::try_from(text.get(byte_end..source.start)?.chars().count()).ok()?;
            let length = u32::try_from(text.get(source.clone())?.chars().count()).ok()?;
            let start = char_end.checked_add(gap)?;
            char_end = start.checked_add(length)?;
            spans.resize(words.end, Self::new(start, char_end));
            byte_end = source.end;
        }
        (spans.len() == word_count).then_some(spans)
    }

    /// The span covering both operands, used to widen a token span to a
    /// record or capture span.
    #[must_use]
    pub const fn join(self, other: Self) -> Self {
        Self {
            start: if self.start < other.start {
                self.start
            } else {
                other.start
            },
            end: if self.end > other.end {
                self.end
            } else {
                other.end
            },
        }
    }
}

pub trait TokenSequence {
    fn len(&self) -> usize;

    /// This token's sentence-relative code-point span, when the source
    /// supplies one.
    ///
    /// Defaulted to `None` so a sequence that cannot answer says so rather
    /// than inventing a position. That is the whole contract: an offset that
    /// is absent is recoverable by a consumer, and one that is wrong points
    /// confidently at the wrong characters. Reconstructing offsets from token
    /// lengths outside the engine is exactly the wrong answer this exists to
    /// replace — it cannot see `SpaceAfter=No`, and it silently disagrees with
    /// the sentence text wherever the two differ.
    fn char_span(&self, _token: usize) -> Option<CharSpan> {
        None
    }

    fn get(&self, token: usize, attribute: TokenAttributeIndex) -> Option<&CompactString>;

    fn get_key(&self, token: usize, key: &AttributeKey) -> Option<&CompactString> {
        self.get(token, TokenAttributeIndex::from_attribute_key(key)?)
    }

    fn value_key(&self, token: usize, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
        self.get_key(token, key)
            .map(|value| AttributeValueRef::Scalar(value.as_str()))
    }

    fn scalar_key(&self, token: usize, key: &AttributeKey) -> Option<&str> {
        match self.value_key(token, key)? {
            AttributeValueRef::Scalar(value) => Some(value),
            AttributeValueRef::StringSet(_) => None,
        }
    }

    fn value_locator(
        &self,
        token: usize,
        locator: &AttributeLocator,
    ) -> Option<AttributeValueRef<'_>> {
        self.value_key(token, locator.key())
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn token(&self, index: usize) -> Option<SequenceToken<'_, Self>>
    where
        Self: Sized,
    {
        SequenceToken::new(self, index)
    }
}

pub struct SequenceToken<'a, S: TokenSequence + ?Sized> {
    sequence: &'a S,
    index: usize,
}

impl<'a, S: TokenSequence + ?Sized> SequenceToken<'a, S> {
    pub fn new(sequence: &'a S, index: usize) -> Option<Self> {
        (index < sequence.len()).then_some(Self { sequence, index })
    }
}

impl<S: TokenSequence + ?Sized> TokenInfoTrait for SequenceToken<'_, S> {
    fn get(&self, index: TokenAttributeIndex) -> Option<&CompactString> {
        self.sequence.get(self.index, index)
    }

    fn value_key(&self, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
        self.sequence.value_key(self.index, key)
    }

    fn value_locator(&self, locator: &AttributeLocator) -> Option<AttributeValueRef<'_>> {
        self.sequence.value_locator(self.index, locator)
    }
}

impl<T: TokenInfoTrait> TokenSequence for [T] {
    fn len(&self) -> usize {
        <[T]>::len(self)
    }

    fn get(&self, token: usize, attribute: TokenAttributeIndex) -> Option<&CompactString> {
        <[T]>::get(self, token)?.get(attribute)
    }
}

impl<T: TokenInfoTrait> TokenSequence for &[T] {
    fn len(&self) -> usize {
        <[T]>::len(self)
    }

    fn get(&self, token: usize, attribute: TokenAttributeIndex) -> Option<&CompactString> {
        <[T]>::get(self, token)?.get(attribute)
    }
}

impl<T: TokenInfoTrait> TokenSequence for Vec<T> {
    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn get(&self, token: usize, attribute: TokenAttributeIndex) -> Option<&CompactString> {
        self.as_slice().get(token)?.get(attribute)
    }
}

impl<T: TokenInfoTrait, const N: usize> TokenSequence for [T; N] {
    fn len(&self) -> usize {
        N
    }

    fn get(&self, token: usize, attribute: TokenAttributeIndex) -> Option<&CompactString> {
        self.as_slice().get(token)?.get(attribute)
    }
}

#[cfg(test)]
mod offset_tests {
    use super::CharSpan;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn surface_offsets_select_original_unicode_with_shared_words(
            parts in prop::collection::vec(("[^\\p{C}]{1,8}", 1usize..4), 0..40)
        ) {
            let mut text = String::new();
            let mut ranges = Vec::new();
            let mut expected = Vec::new();
            let mut words = 0;
            for (part, width) in &parts {
                text.push(' ');
                let start = text.len();
                text.push_str(part);
                ranges.push((start..text.len(), words..words + width));
                expected.extend(std::iter::repeat_n(part, *width));
                words += width;
            }
            let offsets = CharSpan::from_surface_ranges(&text, words, ranges).unwrap();
            for (span, expected) in offsets.iter().zip(expected) {
                let actual: String = text.chars().skip(span.start as usize).take((span.end-span.start) as usize).collect();
                prop_assert_eq!(actual, expected.as_str());
            }
        }
    }

    #[test]
    fn invalid_utf8_and_incomplete_word_coverage_have_no_offsets() {
        assert!(CharSpan::from_surface_ranges("😀あ", 1, [(1..4, 0..1)]).is_none());
        assert!(CharSpan::from_surface_ranges("😀あ", 2, [(0..4, 0..1)]).is_none());
        assert!(CharSpan::from_surface_ranges("😀あ", 2, [(0..4, 0..1), (0..7, 1..2)]).is_none());
        assert_eq!(
            CharSpan::from_surface_ranges("😀あ", 2, [(0..4, 0..1), (4..7, 1..2)]).unwrap(),
            vec![CharSpan::new(0, 1), CharSpan::new(1, 2)]
        );
    }
}
