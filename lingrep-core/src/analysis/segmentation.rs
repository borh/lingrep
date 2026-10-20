use crate::utils::{is_sentence_closing_quotation, is_sentence_delimiter};

use super::{AnalysisError, SourceRange, TextUnit};

/// Versioned native Japanese sentence-boundary policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum JapaneseSegmentationPolicy {
    V1,
}

impl JapaneseSegmentationPolicy {
    /// Returns ordered UTF-8 byte ranges without copying admitted text.
    pub fn sentence_ranges(self, unit: &TextUnit) -> Result<Box<[SourceRange]>, AnalysisError> {
        let mut ranges = Vec::new();
        let mut start = 0_usize;
        let mut characters = unit.text().char_indices().peekable();
        while let Some((offset, character)) = characters.next() {
            if !is_sentence_delimiter(character) {
                continue;
            }
            let mut end = offset + character.len_utf8();
            if let Some(&(quote_offset, quote)) = characters.peek()
                && is_sentence_closing_quotation(quote)
            {
                characters.next();
                end = quote_offset + quote.len_utf8();
            }
            ranges.push(
                unit.source_range(start, end)
                    .map_err(|_| AnalysisError::InvalidSourceRange)?,
            );
            start = end;
        }
        if start < unit.len() {
            ranges.push(
                unit.source_range(start, unit.len())
                    .map_err(|_| AnalysisError::InvalidSourceRange)?,
            );
        }
        Ok(ranges.into_boxed_slice())
    }
}
