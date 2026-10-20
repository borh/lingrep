//! Sample-debugger projection for a sentence's match constellation.
//!
//! The matcher owns raw-candidate identity and policy publication. This module
//! only gives those facts a research-facing shape: coincident spans become
//! alternatives, and strictly-contained spans become a forest.

use std::collections::BTreeMap;
use std::ops::Range;

use lingrep_core::fsm::{CompiledCatalog, SentenceExplanation};
use lingrep_core::token_match::TokenInfo;

use super::projector::TokenSummary;

/// Research-facing span groups for one sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constellation {
    /// Groups in containment-preorder: a parent always precedes its children.
    pub groups: Box<[ConstellationGroup]>,
}

/// All pattern candidates that recognized one exact raw token span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstellationGroup {
    /// Document-global token span shared by every alternative in this group.
    pub span: Range<usize>,
    /// Index of the nearest strictly-containing group, if any.
    pub parent: Option<usize>,
    /// Containment depth for a simple server-rendered indentation.
    pub depth: usize,
    /// Coincident raw candidates in canonical matching order.
    pub alternatives: Box<[ConstellationMatch]>,
}

/// What result policy published for one raw candidate.
///
/// Name and span travel together because a suppressed candidate has neither.
/// A published span may differ from its group's raw span (`detach`), and a
/// published name may differ from `raw_name` (`output_name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedEvidence {
    /// Published pattern name.
    pub name: String,
    /// Document-global published token span.
    pub span: Range<usize>,
}

/// One raw candidate at a constellation span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstellationMatch {
    /// Authored pattern name, before output-name policy relabeling.
    pub raw_name: String,
    /// Policy's publication decision, or `None` when it suppressed this
    /// otherwise visible candidate.
    pub published: Option<PublishedEvidence>,
    /// Optional catalog-specific research labels (such as `S2C`).
    pub classes: Box<[String]>,
}

/// One maximal component of strictly overlapping published spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionRegion {
    /// Document-global union span of every occurrence in the component.
    pub span: Range<usize>,
    /// How many published alternatives this component absorbed. Nothing is
    /// discarded, so these sum to the sentence's published-alternative count.
    pub occurrence_count: usize,
    /// Display-only derivation of those alternatives' research classes,
    /// de-duplicated in first-occurrence order. Not a classification source.
    pub class_summary: Box<[String]>,
}

/// The literal tokens between two consecutive regions.
///
/// Diagnostic evidence for catalog development, never an implicit wildcard: a
/// gap is reported whole, and a wide one is a missing expression rather than
/// something the matcher would have accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionGap {
    /// Document-global `left_region.end..right_region.start`. Empty when the
    /// two regions are adjacent.
    pub span: Range<usize>,
    /// Every intervening token, in order, never truncated.
    pub tokens: Box<[TokenSummary]>,
    /// `tokens.len() <= INITIAL_GAP_MAX_TOKENS`. Derived here so the threshold
    /// is stated once, in Rust, rather than restated in a template.
    pub within_initial_budget: bool,
}

/// One entry of the composition strip.
///
/// The sequence strictly alternates and both starts and ends with a `Region`,
/// which is why there is no separate gap list to pair up by index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositionItem {
    Region(CompositionRegion),
    Gap(CompositionGap),
}

/// One sentence's raw-candidate constellation and its published composition,
/// derived from a single explanation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentencePatternAnalysis {
    /// Raw recognition and policy evidence.
    pub constellation: Constellation,
    /// Regions and gaps alternating in document order. Empty when policy
    /// published nothing on this sentence.
    pub composition: Box<[CompositionItem]>,
}

/// The initial expected layout between sentence expressions is `[]{0,2}`.
///
/// A wider gap is evidence that the catalog lacks an expression, not an
/// allowance the matcher makes. Presentation-only.
const INITIAL_GAP_MAX_TOKENS: usize = 2;

/// Projects a core explanation into coincident alternatives and a containment
/// forest for display.
///
/// Crossing or merely adjacent spans have no invented relation and remain
/// roots for this initial presentation.
///
/// Spans are **sentence-local** here; `shift` moves a whole constellation into
/// the sample's document-global coordinates in one pass.
fn build_constellation(
    matcher: &CompiledCatalog,
    explanation: &SentenceExplanation,
) -> Constellation {
    let mut alternatives = BTreeMap::<(usize, usize), Vec<ConstellationMatch>>::new();
    for candidate in &explanation.candidates {
        let raw_name = candidate.raw.pattern.to_string();
        let classes = matcher
            .patterns()
            .get(raw_name.as_str())
            .and_then(|pattern| pattern.metadata.get("research_classes"))
            .and_then(serde_json::Value::as_array)
            .map(|classes| {
                classes
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        alternatives
            .entry((candidate.raw.start, candidate.raw.end))
            .or_default()
            .push(ConstellationMatch {
                raw_name,
                published: candidate
                    .published
                    .as_ref()
                    .map(|published| PublishedEvidence {
                        name: published.pattern.to_string(),
                        span: published.start..published.end,
                    }),
                classes,
            });
    }

    let mut groups = alternatives
        .into_iter()
        .map(|((start, end), alternatives)| ConstellationGroup {
            span: start..end,
            parent: None,
            depth: 0,
            alternatives: alternatives.into_boxed_slice(),
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then_with(|| right.span.end.cmp(&left.span.end))
    });

    for child in 0..groups.len() {
        let child_span = groups[child].span.clone();
        groups[child].parent = groups
            .iter()
            .enumerate()
            .filter(|(parent, candidate)| {
                *parent != child
                    && candidate.span.start <= child_span.start
                    && child_span.end <= candidate.span.end
                    && candidate.span != child_span
            })
            .min_by_key(|(_, candidate)| candidate.span.end - candidate.span.start)
            .map(|(parent, _)| parent);
        groups[child].depth = groups[child]
            .parent
            .map_or(0, |parent| groups[parent].depth + 1);
    }

    Constellation {
        groups: groups.into_boxed_slice(),
    }
}

/// Moves a whole sentence-local constellation into document-global
/// coordinates.
///
/// Every span this crate publishes is offset exactly here or in
/// [`compose_published`], and no value is ever offset and then translated
/// back — which is what makes clipping and window guards unnecessary rather
/// than merely handled.
fn shift(constellation: Constellation, base_index: usize) -> Constellation {
    Constellation {
        groups: constellation
            .groups
            .into_vec()
            .into_iter()
            .map(|group| ConstellationGroup {
                span: base_index + group.span.start..base_index + group.span.end,
                alternatives: group
                    .alternatives
                    .into_vec()
                    .into_iter()
                    .map(|alternative| ConstellationMatch {
                        published: alternative.published.map(|published| PublishedEvidence {
                            span: base_index + published.span.start
                                ..base_index + published.span.end,
                            ..published
                        }),
                        ..alternative
                    })
                    .collect(),
                ..group
            })
            .collect(),
    }
}

/// Builds the alternating region/gap sequence from published spans alone.
///
/// `constellation` and `tokens` are sentence-local; `base_index` is applied
/// only when a public span is constructed, so `tokens` is always sliced with
/// the coordinates it is indexed by.
fn compose_published(
    constellation: &Constellation,
    tokens: &[TokenInfo],
    base_index: usize,
) -> Box<[CompositionItem]> {
    // Constellation order — groups by start, then containment; alternatives in
    // canonical matching order — is the tiebreak a stable sort preserves, so
    // the sweep is deterministic without materializing occurrence indices.
    let mut published = constellation
        .groups
        .iter()
        .flat_map(|group| group.alternatives.iter())
        .filter_map(|alternative| {
            alternative
                .published
                .as_ref()
                .map(|evidence| (evidence.span.clone(), &alternative.classes))
        })
        .collect::<Vec<_>>();
    published.sort_by(|left, right| {
        left.0
            .start
            .cmp(&right.0.start)
            .then_with(|| left.0.end.cmp(&right.0.end))
    });

    // Maximal connected components of *strictly* overlapping intervals. The
    // `>=` boundary is the point: intervals that merely touch stay separate so
    // adjacency remains visible as a zero-token gap.
    let mut regions: Vec<(Range<usize>, usize, Vec<String>)> = Vec::new();
    for (span, classes) in published {
        match regions.last_mut() {
            Some((union, occurrences, summary)) if span.start < union.end => {
                union.end = union.end.max(span.end);
                *occurrences += 1;
                extend_summary(summary, classes);
            }
            _ => {
                let mut summary = Vec::new();
                extend_summary(&mut summary, classes);
                regions.push((span, 1, summary));
            }
        }
    }

    let mut items = Vec::with_capacity(regions.len().saturating_mul(2));
    let mut previous_end: Option<usize> = None;
    for (span, occurrence_count, class_summary) in regions {
        if let Some(previous_end) = previous_end {
            // In bounds by construction: both ends came from matches over
            // exactly this token slice.
            let gap_tokens = &tokens[previous_end..span.start];
            items.push(CompositionItem::Gap(CompositionGap {
                span: base_index + previous_end..base_index + span.start,
                tokens: gap_tokens.iter().map(TokenSummary::from).collect(),
                within_initial_budget: gap_tokens.len() <= INITIAL_GAP_MAX_TOKENS,
            }));
        }
        previous_end = Some(span.end);
        items.push(CompositionItem::Region(CompositionRegion {
            span: base_index + span.start..base_index + span.end,
            occurrence_count,
            class_summary: class_summary.into_boxed_slice(),
        }));
    }
    items.into_boxed_slice()
}

/// Appends classes not already summarized, preserving first-occurrence order.
fn extend_summary(summary: &mut Vec<String>, classes: &[String]) {
    for class in classes {
        if !summary.iter().any(|seen| seen == class) {
            summary.push(class.clone());
        }
    }
}

/// Projects one sentence's raw constellation and its published composition
/// from a single explanation.
///
/// `tokens` is the sentence the explanation was produced from. `base_index`
/// shifts sentence-local spans into the sample's document-global token
/// coordinates.
pub fn project_sentence_patterns(
    matcher: &CompiledCatalog,
    explanation: &SentenceExplanation,
    tokens: &[TokenInfo],
    base_index: usize,
) -> SentencePatternAnalysis {
    let local = build_constellation(matcher, explanation);
    let composition = compose_published(&local, tokens, base_index);
    SentencePatternAnalysis {
        constellation: shift(local, base_index),
        composition,
    }
}
