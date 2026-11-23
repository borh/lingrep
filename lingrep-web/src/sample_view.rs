//! Layout of a sample analysis for the results fragment: one row per
//! sentence, with match and capture bars packed into lanes under its tokens.

use crate::inspection::{CompositionItem, ConstellationGroup};
use crate::sample::{MatchView, SampleAnalysis, SampleSentence, TokenView, WhyNot};

/// What the fragment shows in place of sentence rows.
pub enum SampleNotice<'a> {
    None,
    /// The sample has no text.
    EmptySample,
    EmptyPattern,
    FragmentsOnly,
    Failed(&'a str),
}

pub struct SampleView<'a> {
    pub notice: SampleNotice<'a>,
    pub sentences: Vec<SentenceRow<'a>>,
    pub definitions: Vec<DefinitionCount<'a>>,
    pub match_count: usize,
    pub token_count: usize,
}

/// A definition and how often it is reported in the sample.
pub struct DefinitionCount<'a> {
    pub name: &'a str,
    pub count: usize,
    /// Number of the first sentence that reports it.
    pub first_sentence: usize,
}

pub struct SentenceRow<'a> {
    /// One-based, counting non-empty input lines.
    pub number: usize,
    pub tokens: Vec<TokenCell<'a>>,
    pub matches: &'a [MatchView],
    pub why_not: &'a [WhyNot],
    /// Candidates the result policy did not report, and the stretches no
    /// reported match covers. `None` when the policy hid nothing.
    pub policy: Option<Policy<'a>>,
}

pub struct TokenCell<'a> {
    pub token: &'a TokenView,
    pub matched: bool,
    /// `TokenView::source_ranges` from the first match that covers the token.
    pub source_ranges: &'a str,
    /// One cell per lane, top lane first, down to the lowest lane that has a
    /// bar under this token.
    pub lanes: Vec<LaneCell<'a>>,
}

pub enum LaneCell<'a> {
    Empty,
    /// The first token of a bar. `last` when the bar covers one token.
    Start {
        bar: Bar<'a>,
        last: bool,
    },
    /// A later token of a bar. `last` at its final token.
    Continue {
        /// [`Bar::id`] of the bar this cell belongs to.
        id: String,
        capture: bool,
        last: bool,
    },
}

pub struct Bar<'a> {
    pub label: &'a str,
    /// `MatchView::render_index` of the match the bar belongs to.
    pub match_index: usize,
    /// The capture's ordinal within its match; `None` for the match itself.
    pub capture: Option<usize>,
    /// One-based, inclusive.
    pub first: usize,
    pub last: usize,
}

impl Bar<'_> {
    /// Names the bar among the sample's bars: the match, then the capture.
    pub fn id(&self) -> String {
        match self.capture {
            Some(capture) => format!("{}-{capture}", self.match_index),
            None => self.match_index.to_string(),
        }
    }
}

pub struct Policy<'a> {
    pub groups: &'a [ConstellationGroup],
    pub composition: &'a [CompositionItem],
}

impl<'a> SampleView<'a> {
    pub fn new(analysis: &'a SampleAnalysis) -> Self {
        let (notice, sentences) = match analysis {
            SampleAnalysis::Matched { sentences } => (SampleNotice::None, sentences.as_slice()),
            SampleAnalysis::NoMatches { sentences } if sentences.is_empty() => {
                (SampleNotice::EmptySample, sentences.as_slice())
            }
            SampleAnalysis::NoMatches { sentences } => (SampleNotice::None, sentences.as_slice()),
            SampleAnalysis::EmptyPattern => (SampleNotice::EmptyPattern, &[][..]),
            SampleAnalysis::FragmentsOnly => (SampleNotice::FragmentsOnly, &[][..]),
            SampleAnalysis::Failed(error) => (SampleNotice::Failed(&error.message), &[][..]),
        };
        let mut definitions: Vec<DefinitionCount<'a>> = Vec::new();
        for (index, sentence) in sentences.iter().enumerate() {
            for matched in &sentence.matches {
                match definitions
                    .iter_mut()
                    .find(|definition| definition.name == matched.pattern_name)
                {
                    Some(definition) => definition.count += 1,
                    None => definitions.push(DefinitionCount {
                        name: &matched.pattern_name,
                        count: 1,
                        first_sentence: index + 1,
                    }),
                }
            }
        }
        Self {
            notice,
            match_count: sentences
                .iter()
                .map(|sentence| sentence.matches.len())
                .sum(),
            token_count: sentences.iter().map(|sentence| sentence.tokens.len()).sum(),
            sentences: sentences
                .iter()
                .enumerate()
                .map(|(index, sentence)| sentence_row(index + 1, sentence))
                .collect(),
            definitions,
        }
    }
}

fn sentence_row(number: usize, sentence: &SampleSentence) -> SentenceRow<'_> {
    let width = sentence.tokens.len();
    let mut lanes: Vec<Vec<LaneCell<'_>>> = Vec::new();
    // Columns of each match lane that a bar or its label already occupies.
    let mut occupied: Vec<usize> = Vec::new();
    let mut matched = vec![false; width];
    let mut source_ranges = vec![""; width];
    let mut capture_lanes: Vec<Vec<LaneCell<'_>>> = Vec::new();
    let mut matches: Vec<&MatchView> = sentence.matches.iter().collect();
    matches.sort_by_key(|matched| (matched.start, matched.end));
    for found in matches {
        let (start, end) = (found.start.min(width), found.end.min(width));
        if start >= end {
            continue;
        }
        matched[start..end].fill(true);
        for (ranges, token) in source_ranges[start..end].iter_mut().zip(&found.tokens) {
            if ranges.is_empty() {
                *ranges = &token.source_ranges;
            }
        }
        // A label is wider than a short bar; keep the next bar clear of it.
        // Three characters per token is the width of a typical token chip.
        let reach = end.max(start + found.pattern_name.chars().count().div_ceil(3));
        let lane = occupied
            .iter()
            .position(|until| *until <= start)
            .unwrap_or_else(|| {
                occupied.push(0);
                lanes.push(empty_lane(width));
                lanes.len() - 1
            });
        occupied[lane] = reach;
        place(
            &mut lanes[lane],
            Bar {
                label: &found.pattern_name,
                match_index: found.render_index,
                capture: None,
                first: start + 1,
                last: end,
            },
        );
        for captures in &found.capture_lanes {
            let mut lane = empty_lane(width);
            for capture in &captures.captures {
                let first = start + capture.grid_start;
                let last = (first + capture.grid_span - 1).min(width);
                if first > last {
                    continue;
                }
                place(
                    &mut lane,
                    Bar {
                        label: &capture.name,
                        match_index: found.render_index,
                        capture: Some(capture.ordinal),
                        first,
                        last,
                    },
                );
            }
            capture_lanes.push(lane);
        }
    }
    lanes.append(&mut capture_lanes);
    let mut columns: Vec<Vec<LaneCell<'_>>> = (0..width).map(|_| Vec::new()).collect();
    for lane in lanes {
        for (column, cell) in columns.iter_mut().zip(lane) {
            column.push(cell);
        }
    }
    // A token with nothing under it in the lower lanes needs no room for
    // them, so a wrapped line of unmatched tokens is one chip tall.
    for column in &mut columns {
        while matches!(column.last(), Some(LaneCell::Empty)) {
            column.pop();
        }
    }
    let hidden = sentence
        .groups
        .iter()
        .flat_map(|group| group.alternatives.iter())
        .any(|alternative| alternative.published.is_none());
    SentenceRow {
        number,
        tokens: sentence
            .tokens
            .iter()
            .zip(matched)
            .zip(source_ranges)
            .zip(columns)
            .map(|(((token, matched), source_ranges), lanes)| TokenCell {
                token,
                matched,
                source_ranges,
                lanes,
            })
            .collect(),
        matches: &sentence.matches,
        why_not: &sentence.why_not,
        policy: hidden.then_some(Policy {
            groups: &sentence.groups,
            composition: &sentence.composition,
        }),
    }
}

fn empty_lane<'a>(width: usize) -> Vec<LaneCell<'a>> {
    (0..width).map(|_| LaneCell::Empty).collect()
}

fn place<'a>(lane: &mut [LaneCell<'a>], bar: Bar<'a>) {
    let (first, last, capture) = (bar.first - 1, bar.last - 1, bar.capture.is_some());
    for (column, cell) in lane.iter_mut().enumerate().take(last + 1).skip(first + 1) {
        *cell = LaneCell::Continue {
            id: bar.id(),
            capture,
            last: column == last,
        };
    }
    lane[first] = LaneCell::Start {
        bar,
        last: first == last,
    };
}
