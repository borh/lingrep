use std::ops::Range;

use crate::provenance_view::ProvenanceField;
use askama::Template;
use lingrep_corpus::StoredSentenceAnalysis;
use serde::Deserialize;

pub(crate) const MAX_READING_SENTENCES: usize = 20;
pub(crate) const MAX_READING_TEXT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_READING_SENTENCE_BYTES: usize = 128 * 1024;
pub(crate) const MAX_READING_HTML_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadingQuery {
    #[serde(default)]
    pub show_excluded: bool,
    pub start: Option<u64>,
    pub before: Option<u64>,
    pub paragraph: Option<u64>,
}

pub(crate) fn page_ordinals(
    range: Range<u64>,
    selected: u64,
    query: &ReadingQuery,
) -> Option<Vec<u64>> {
    if !range.contains(&selected) || (query.start.is_some() && query.before.is_some()) {
        return None;
    }
    if let Some(before) = query.before {
        if before <= range.start || before > range.end {
            return None;
        }
        return Some(
            (range
                .start
                .max(before.saturating_sub(MAX_READING_SENTENCES as u64))..before)
                .rev()
                .collect(),
        );
    }
    let start = query.start.unwrap_or(selected);
    if !range.contains(&start) {
        return None;
    }
    Some(
        (start
            ..range
                .end
                .min(start.saturating_add(MAX_READING_SENTENCES as u64)))
            .collect(),
    )
}

pub(crate) fn source_highlight(
    analysis: &StoredSentenceAnalysis,
    words: Range<usize>,
) -> Option<Range<usize>> {
    if words.start >= words.end {
        return None;
    }
    let mut covered = words.start;
    let mut start = None;
    let mut end = 0;
    for surface in &analysis.surfaces {
        let word_start = surface.word_range.start as usize;
        let word_end = surface.word_range.end as usize;
        if word_end <= words.start || word_start >= words.end {
            continue;
        }
        if word_start > covered || (start.is_none() && word_start != words.start) {
            return None;
        }
        start.get_or_insert(surface.source_range.start as usize);
        end = surface.source_range.end as usize;
        covered = word_end;
    }
    let range = start?..end;
    (covered == words.end && analysis.text.get(range.clone()).is_some()).then_some(range)
}

pub(crate) struct ReadingSentence {
    pub excluded: bool,
    pub ordinal: u64,
    pub number: u64,
    pub selected: bool,
    pub left: String,
    pub key: String,
    pub right: String,
    pub reconstructed: bool,
    pub omitted: bool,
    pub paragraph_starts: Vec<u64>,
    pub sections: Vec<String>,
}

pub(crate) struct ReadingParagraphChoice {
    pub id: u64,
    pub first: u64,
    pub last: u64,
}

#[derive(Template)]
#[template(path = "fragments/reading.html")]
pub(crate) struct ReadingPage {
    pub show_excluded: bool,
    pub root_url: String,
    pub target_id: String,
    pub source: String,
    pub metadata: Vec<ProvenanceField>,
    pub metadata_omitted: bool,
    pub document: bool,
    pub paragraph_query: String,
    pub paragraph_choices: Vec<ReadingParagraphChoice>,
    pub paragraph_missing: bool,
    pub sentences: Vec<ReadingSentence>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
}

impl ReadingPage {
    pub fn rows(&self) -> Vec<&ReadingSentence> {
        self.sentences
            .iter()
            .enumerate()
            .filter_map(|(index, sentence)| {
                (self.show_excluded
                    || !sentence.excluded
                    || index == 0
                    || !self.sentences[index - 1].excluded)
                    .then_some(sentence)
            })
            .collect()
    }

    pub fn has_excluded(&self) -> bool {
        self.sentences.iter().any(|sentence| sentence.excluded)
    }

    pub fn page_query(&self) -> String {
        format!(
            "{}{}",
            self.paragraph_query,
            if self.show_excluded {
                "show_excluded=true&"
            } else {
                ""
            }
        )
    }

    pub fn context_url(&self) -> String {
        format!(
            "{}/{}?{}start={}&show_excluded={}",
            self.root_url,
            self.scope(),
            self.paragraph_query,
            self.sentences
                .first()
                .map_or(0, |sentence| sentence.ordinal),
            !self.show_excluded
        )
    }

    pub fn title(&self) -> Option<&str> {
        self.metadata
            .iter()
            .find(|field| field.key == "title")
            .and_then(|field| field.values.first())
            .map(String::as_str)
            .filter(|title| !title.is_empty())
    }

    fn metadata_html_bytes(&self) -> usize {
        self.metadata.iter().map(metadata_field_html_bytes).sum()
    }

    pub fn push_metadata_bounded(&mut self, field: ProvenanceField) {
        if self
            .metadata_html_bytes()
            .saturating_add(metadata_field_html_bytes(&field))
            > MAX_READING_TEXT_BYTES
        {
            self.metadata_omitted = true;
        } else {
            self.metadata.push(field);
        }
    }

    pub fn push_bounded(&mut self, sentence: ReadingSentence) -> bool {
        if self.sentences.len() >= MAX_READING_SENTENCES {
            return false;
        }
        let mut text_bytes = 0usize;
        let mut html_bytes = 16 * 1024
            + self.metadata_html_bytes()
            + self
                .title()
                .map_or(0, |title| title.len().saturating_mul(6))
            + self.root_url.len().saturating_mul(48)
            + self.source.len().saturating_mul(6);
        for sentence in self.sentences.iter().chain(std::iter::once(&sentence)) {
            let bytes = sentence.left.len()
                + sentence.key.len()
                + sentence.right.len()
                + sentence.sections.iter().map(String::len).sum::<usize>();
            text_bytes += bytes;
            html_bytes = html_bytes.saturating_add(
                bytes.saturating_mul(6)
                    + sentence.paragraph_starts.len() * 256
                    + sentence.sections.len() * 128
                    + 2048,
            );
        }
        if text_bytes > MAX_READING_TEXT_BYTES || html_bytes > MAX_READING_HTML_BYTES {
            return false;
        }
        self.sentences.push(sentence);
        true
    }

    pub fn scope(&self) -> &'static str {
        if self.document {
            "document"
        } else {
            "paragraph"
        }
    }
}

fn metadata_field_html_bytes(field: &ProvenanceField) -> usize {
    field.values.iter().fold(
        field.key.len().saturating_mul(6).saturating_add(256),
        |bytes, value| bytes.saturating_add(value.len().saturating_mul(6).saturating_add(32)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::generators as g;

    fn empty_page() -> ReadingPage {
        ReadingPage {
            show_excluded: false,
            root_url: "api/hit-context/reading/1/0/0".into(),
            target_id: "reader".into(),
            source: "document.md".into(),
            metadata: Vec::new(),
            metadata_omitted: false,
            document: true,
            paragraph_query: String::new(),
            paragraph_choices: Vec::new(),
            paragraph_missing: false,
            sentences: Vec::new(),
            previous: None,
            next: None,
        }
    }

    fn sentence(ordinal: u64, text: String, paragraph_count: usize) -> ReadingSentence {
        ReadingSentence {
            excluded: false,
            ordinal,
            number: ordinal + 1,
            selected: false,
            left: text,
            key: String::new(),
            right: String::new(),
            reconstructed: false,
            omitted: false,
            paragraph_starts: (0..paragraph_count as u64).collect(),
            sections: Vec::new(),
        }
    }

    #[test]
    fn escaped_text_and_many_paragraph_boundaries_are_paged_within_the_html_limit() {
        let mut page = empty_page();
        page.push_metadata_bounded(ProvenanceField {
            key: "title".into(),
            values: Box::new(["'".repeat(40 * 1024)]),
        });
        for ordinal in 0..20 {
            let mut row = sentence(ordinal, "'".repeat(13 * 1024), 256);
            row.sections = vec!["'".repeat(4 * 1024)];
            if !page.push_bounded(row) {
                break;
            }
        }
        assert!(!page.sentences.is_empty());
        assert!(page.sentences.len() < 20);
        assert!(page.render().unwrap().len() <= MAX_READING_HTML_BYTES);
    }

    #[test]
    fn oversized_metadata_fields_are_labelled_and_do_not_hide_smaller_fields_or_text() {
        let mut page = empty_page();
        assert!(!page.render().unwrap().contains("data-reading-metadata"));
        page.push_metadata_bounded(ProvenanceField {
            key: "large".into(),
            values: Box::new(["'".repeat(MAX_READING_TEXT_BYTES)]),
        });
        page.push_metadata_bounded(ProvenanceField {
            key: "title".into(),
            values: Box::new(["<Title>".into()]),
        });
        assert!(page.push_bounded(sentence(0, "reading text".into(), 0)));
        let html = page.render().unwrap();
        assert!(html.contains("reading.metadata_omitted"));
        assert!(html.contains("&#60;Title&#62;"));
        assert!(html.contains("reading text"));
        assert!(!html.contains("<dt>large</dt>"));
        assert!(html.len() <= MAX_READING_HTML_BYTES);
    }

    #[hegel::test(test_cases = 100, derandomize = true)]
    fn byte_limited_pages_cover_all_sentences_forward_and_backward(tc: hegel::TestCase) {
        let sizes = tc.draw(g::vecs(g::integers::<u16>()).max_size(50));
        if sizes.is_empty() {
            return;
        }
        let end = sizes.len() as u64;
        for backward in [false, true] {
            let mut cursor = if backward { end } else { 0 };
            let mut visited = Vec::new();
            loop {
                let query = ReadingQuery {
                    show_excluded: false,
                    start: (!backward).then_some(cursor),
                    before: backward.then_some(cursor),
                    paragraph: None,
                };
                let plan = page_ordinals(0..end, 0, &query).unwrap();
                let mut page = empty_page();
                for ordinal in plan {
                    if !page.push_bounded(sentence(
                        ordinal,
                        "&".repeat(usize::from(sizes[ordinal as usize])),
                        0,
                    )) {
                        break;
                    }
                }
                assert!(!page.sentences.is_empty());
                assert!(page.render().unwrap().len() <= MAX_READING_HTML_BYTES);
                assert!(
                    page.sentences
                        .iter()
                        .map(|row| row.left.len())
                        .sum::<usize>()
                        <= MAX_READING_TEXT_BYTES
                );
                visited.extend(page.sentences.iter().map(|row| row.ordinal));
                let last = page.sentences.last().unwrap().ordinal;
                if (backward && last == 0) || (!backward && last + 1 == end) {
                    break;
                }
                cursor = if backward { last } else { last + 1 };
            }
            if backward {
                visited.reverse();
            }
            assert_eq!(visited, (0..end).collect::<Vec<_>>());
        }
    }

    #[hegel::test(test_cases = 500, derandomize = true)]
    fn page_plans_accept_only_in_scope_ordinals_and_valid_cursor_combinations(tc: hegel::TestCase) {
        let first = tc.draw(g::integers::<u64>());
        let end = first.saturating_add(tc.draw(g::integers::<u16>()) as u64);
        let selected = first.saturating_add(tc.draw(g::integers::<u16>()) as u64);
        let in_scope = first.saturating_add(
            tc.draw(g::integers::<u16>()) as u64 % end.saturating_sub(first).max(1),
        );
        let raw_start = if tc.draw(g::integers::<u8>()).is_multiple_of(2) {
            in_scope
        } else {
            tc.draw(g::integers::<u64>())
        };
        let raw_before = if tc.draw(g::integers::<u8>()).is_multiple_of(2) {
            in_scope.saturating_add(1)
        } else {
            tc.draw(g::integers::<u64>())
        };
        let kind = tc.draw(g::integers::<u8>().max_value(3));
        let query = ReadingQuery {
            show_excluded: false,
            start: (kind == 1 || kind == 3).then_some(raw_start),
            before: (kind == 2 || kind == 3).then_some(raw_before),
            paragraph: None,
        };
        let valid = (first..end).contains(&selected)
            && match kind {
                0 => true,
                1 => (first..end).contains(&raw_start),
                2 => first < raw_before && raw_before <= end,
                _ => false,
            };
        let result = page_ordinals(first..end, selected, &query);
        assert_eq!(result.is_some(), valid);
        if let Some(ordinals) = result {
            assert!(!ordinals.is_empty());
            assert!(ordinals.len() <= MAX_READING_SENTENCES);
            assert!(
                ordinals
                    .iter()
                    .all(|ordinal| (first..end).contains(ordinal))
            );
            assert!(ordinals.windows(2).all(|pair| if query.before.is_some() {
                pair[0] - 1 == pair[1]
            } else {
                pair[0] + 1 == pair[1]
            }));
        }
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn source_highlight_never_includes_words_outside_the_selected_span(tc: hegel::TestCase) {
        let width = tc.draw(g::integers::<u32>().min_value(1));
        let exact = tc.draw(g::integers::<u8>()).is_multiple_of(2);
        let start = if exact {
            0
        } else {
            tc.draw(g::integers::<u32>())
        };
        let stop = if exact {
            width
        } else {
            tc.draw(g::integers::<u32>())
        };
        let analysis = StoredSentenceAnalysis {
            text: "  猫😺  ".into(),
            surfaces: Box::new([lingrep_corpus::StoredSurfaceAlignment {
                source_range: 2..9,
                word_range: 0..width,
            }]),
            input_unit_id: None,
            input_kind: None,
            normalization: None,
            input_identity: None,
            source_range: None,
        };
        assert_eq!(
            source_highlight(&analysis, start as usize..stop as usize),
            (start == 0 && stop == width).then_some(2..9)
        );
    }

    #[test]
    fn page_plans_handle_the_last_u64_sentence_without_overflow() {
        assert_eq!(
            page_ordinals(
                u64::MAX - 1..u64::MAX,
                u64::MAX - 1,
                &ReadingQuery::default()
            ),
            Some(vec![u64::MAX - 1])
        );
    }
}
