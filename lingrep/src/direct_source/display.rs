//! Token context projection and surface/display text formatting.

use std::ops::Range;

use lingrep_core::execution::{ExecutionError, SentenceLocation};
use lingrep_core::token_match::{TokenInfo, TokenSequence};

/// Return bounded context without modifying the matched token slice.
pub fn project_context(
    sentence: &dyn TokenSequence,
    span: Range<usize>,
    requested: usize,
) -> (Vec<TokenInfo>, Vec<TokenInfo>) {
    let start = span.start.min(sentence.len());
    let end = span.end.min(sentence.len()).max(start);
    let left_start = start.saturating_sub(requested);
    let right_end = end.saturating_add(requested).min(sentence.len());
    (
        (left_start..start)
            .filter_map(|index| TokenInfo::from_sequence(sentence, index))
            .collect(),
        (end..right_end)
            .filter_map(|index| TokenInfo::from_sequence(sentence, index))
            .collect(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "missing required attribute {attribute} at {source_name}, corpus sentence \
     {corpus_sentence_ordinal}, token {token_index}"
)]
pub struct MissingRequiredAttribute {
    pub attribute: &'static str,
    pub source_name: String,
    pub corpus_sentence_ordinal: u64,
    pub token_index: usize,
}

impl From<MissingRequiredAttribute> for ExecutionError {
    fn from(error: MissingRequiredAttribute) -> Self {
        Self::MissingRequiredAttribute {
            attribute: error.attribute,
            source_name: error.source_name,
            corpus_sentence_ordinal: error.corpus_sentence_ordinal,
            token_index: error.token_index,
        }
    }
}

/// The exported `surface` of `span`: its tokens joined with one space
/// wherever their character offsets show a gap, and with nothing across a
/// boundary without offsets.
///
/// [`lingrep_core::execution::surface_group`] is the count key, which fails
/// at such a boundary, and [`displayed_text`] is the text a reader sees.
pub fn matched_surface(
    sentence: &dyn TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
) -> Result<String, MissingRequiredAttribute> {
    joined_orths(sentence, span, location, |index| {
        lingrep_core::token_match::source_space_before(sentence, index) == Some(true)
    })
}

/// Display text for interactive previews:
/// [`lingrep_core::token_match::display_text`], with an error for a token
/// that has no text.
pub fn displayed_text(
    sentence: &dyn TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
) -> Result<String, MissingRequiredAttribute> {
    joined_orths(sentence, span, location, |index| {
        lingrep_core::token_match::display_space_before(sentence, index)
    })
}

fn joined_orths(
    sentence: &dyn TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
    space_before: impl Fn(usize) -> bool,
) -> Result<String, MissingRequiredAttribute> {
    let mut text = String::new();
    let first = span.start;
    visit_matched_orths(sentence, span, location, |index, orth| {
        if index > first && space_before(index) {
            text.push(' ');
        }
        text.push_str(orth);
    })?;
    Ok(text)
}

pub fn matched_orth_tokens(
    sentence: &dyn TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
) -> Result<Vec<String>, MissingRequiredAttribute> {
    let mut result = Vec::with_capacity(span.end.saturating_sub(span.start));
    visit_matched_orths(sentence, span, location, |_, orth| {
        result.push(orth.to_owned());
    })?;
    Ok(result)
}

fn visit_matched_orths(
    sentence: &dyn TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
    mut visit: impl FnMut(usize, &str),
) -> Result<(), MissingRequiredAttribute> {
    let end = span.end.min(sentence.len());
    for index in span.start.min(end)..end {
        let text =
            lingrep_core::token_match::display_surface(sentence, index).ok_or_else(|| {
                MissingRequiredAttribute {
                    attribute: "text",
                    source_name: location.source_name.to_string(),
                    corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                    token_index: index,
                }
            })?;
        visit(index, text);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn matched_surfaces_preserve_word_boundaries() {
        use lingrep_core::testing::helpers::placed_tokens;
        let location = SentenceLocation {
            lookaround_divisions: None,
            source_metadata: None,
            corpus_sentence_ordinal: 0,
            document_id: lingrep_core::corpus::DocumentId::new(0),
            document_ordinal: 0,
            document_sentence_ordinal: 0,
            source_name: Arc::from("doc.txt"),
        };
        let both = |source: &str, words: &[&str], span: Range<usize>| {
            let sentence = placed_tokens(source, words);
            (
                displayed_text(&sentence, span.clone(), &location).unwrap(),
                matched_surface(&sentence, span, &location).unwrap(),
            )
        };
        assert_eq!(
            both("I saw  NHK\tnews", &["I", "saw", "NHK", "news"], 1..4),
            ("saw NHK news".into(), "saw NHK news".into())
        );
        assert_eq!(
            both(
                "昨日 NHK を見た",
                &["昨日", "N", "H", "K", "を", "見", "た"],
                0..5
            ),
            ("昨日 NHK を".into(), "昨日 NHK を".into())
        );
        assert_eq!(
            both("昨日は見た", &["昨日", "は", "見", "た"], 0..4),
            ("昨日は見た".into(), "昨日は見た".into())
        );
    }

    /// Without character offsets, display text guesses a space from the
    /// script, an exported surface concatenates, and a count key is an error
    /// naming the token after the boundary. With offsets the joins are equal.
    #[test]
    fn joins_differ_only_at_a_boundary_without_offsets() {
        use lingrep_core::execution::{ExecutionError, surface_group};
        use lingrep_core::token_match::{TokenInfo, display_text};
        let location = SentenceLocation {
            lookaround_divisions: None,
            source_metadata: None,
            corpus_sentence_ordinal: 7,
            document_id: lingrep_core::corpus::DocumentId::new(0),
            document_ordinal: 0,
            document_sentence_ordinal: 7,
            source_name: Arc::from("doc.conllu"),
        };
        let unplaced = |words: &[&str]| -> Vec<TokenInfo> {
            words
                .iter()
                .map(|word| TokenInfo::default().with_orth(*word))
                .collect()
        };
        for (words, displayed, exported) in [
            (&["NHK", "news"][..], "NHK news", "NHKnews"),
            (&["N", "H", "K"][..], "N H K", "NHK"),
            (&["猫", "が", "走る"][..], "猫が走る", "猫が走る"),
        ] {
            let sentence = unplaced(words);
            let span = 0..sentence.len();
            assert_eq!(
                displayed_text(&sentence, span.clone(), &location).unwrap(),
                displayed
            );
            assert_eq!(display_text(&sentence, span.clone()), displayed);
            assert_eq!(
                matched_surface(&sentence, span.clone(), &location).unwrap(),
                exported
            );
            assert!(matches!(
                surface_group(&sentence, span, &location),
                Err(ExecutionError::MissingSurfaceOffsets {
                    source_name,
                    corpus_sentence_ordinal: 7,
                    token_index: 1,
                }) if source_name == "doc.conllu"
            ));
            assert_eq!(surface_group(&sentence, 1..2, &location).unwrap(), words[1]);
        }
        let placed = lingrep_core::testing::helpers::placed_tokens(
            "I saw  NHK\tnews",
            &["I", "saw", "NHK", "news"],
        );
        for joined in [
            displayed_text(&placed, 1..4, &location).unwrap(),
            display_text(&placed, 1..4),
            matched_surface(&placed, 1..4, &location).unwrap(),
            surface_group(&placed, 1..4, &location).unwrap(),
        ] {
            assert_eq!(joined, "saw NHK news");
        }
    }
}
