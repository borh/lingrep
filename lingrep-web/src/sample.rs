//! Pure sample-text analysis over `CompiledCatalog`.
//!
//! `analyze_tokens` is the unit-testable core: it takes pre-tokenized lines
//! and never touches a tokenizer. `analyze_sample` is a thin wrapper that
//! tokenizes sample text and delegates. Projection failures (rendering a
//! token or capture inspection fragment) propagate as `SampleAnalysis::Failed`
//! rather than being swallowed into an empty result.

use askama::Template;

use crate::inspection::{
    CaptureInspection, CaptureOrdinal, CompositionItem, ConstellationGroup, InspectionProjector,
    ResolvedCapture, project_sentence_patterns,
};
use crate::patterns::compile_web_pattern;
use crate::patterns::format_pattern_compile_error;
use crate::templates::{CaptureInspectionFragment, TokenInspectionFragment, TrustedFragmentHtml};
use lingrep_core::token_match::{TokenInfo, tokenize};
use vibrato_rkyv::Tokenizer;

#[derive(Debug, Clone)]
pub struct TokenView {
    pub index: usize,
    pub orth: String,
    pub lemma: String,
    pub pos: String,
    pub cform: String,
    pub inspection_html: TrustedFragmentHtml,
}

impl TokenView {
    pub fn from_token_info(token: &TokenInfo, index: usize) -> Result<Self, askama::Error> {
        Ok(Self {
            index,
            orth: token.orth().unwrap_or_default().to_owned(),
            lemma: token.lemma().unwrap_or_default().to_owned(),
            pos: token.pos().unwrap_or_default().to_owned(),
            cform: token.cform().unwrap_or_default().to_owned(),
            inspection_html: TrustedFragmentHtml::from_rendered(render_sample_token(token)?),
        })
    }
}

pub fn render_sample_token(token: &TokenInfo) -> Result<String, askama::Error> {
    TokenInspectionFragment::new(InspectionProjector::project_token(token)).render()
}

#[derive(Debug, Clone)]
pub struct MatchView {
    pub render_index: usize,
    pub pattern_name: String,
    pub start: usize,
    pub end: usize,
    pub matched_text: String,
    pub tokens: Vec<TokenView>,
    pub captures: Vec<SampleCaptureData>,
    pub capture_lanes: Vec<SampleCaptureLane>,
}

#[derive(Debug, Clone)]
pub struct SampleCaptureData {
    pub ordinal: usize,
    pub name: String,
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub grid_start: usize,
    pub grid_span: usize,
    pub detail_html: TrustedFragmentHtml,
}

#[derive(Debug, Clone)]
pub struct SampleCaptureLane {
    pub captures: Vec<SampleCaptureData>,
}

/// One input line's raw-candidate constellation, published composition, and
/// token rail.
#[derive(Debug, Clone)]
pub struct SampleConstellation {
    pub tokens: Vec<TokenView>,
    pub groups: Box<[ConstellationGroup]>,
    /// Regions and gaps alternating in document order. Empty when policy
    /// published nothing on this line, which is a different fact from the
    /// matcher having seen no candidate.
    pub composition: Box<[CompositionItem]>,
}

pub struct SampleCaptureView {
    pub captures: Vec<SampleCaptureData>,
    pub lanes: Vec<SampleCaptureLane>,
}

pub fn sample_capture_view(
    tokens: &[TokenInfo],
    matched: &lingrep_core::fsm::Match,
    base_index: usize,
) -> Result<SampleCaptureView, askama::Error> {
    let resolved = matched
        .captures
        .iter()
        .enumerate()
        .map(|(index, capture)| ResolvedCapture {
            ordinal: CaptureOrdinal::new(index),
            name: capture.name.to_string(),
            path: capture.path.to_string(),
            span: capture.start..capture.end,
        })
        .collect::<Vec<_>>();
    let inspection =
        InspectionProjector::project_match(tokens, matched.start..matched.end, &resolved, 0);
    let render_capture = |capture: &CaptureInspection| -> Result<SampleCaptureData, askama::Error> {
        let mut display_capture = capture.clone();
        display_capture.span = base_index + capture.span.start..base_index + capture.span.end;
        Ok(SampleCaptureData {
            ordinal: capture.ordinal.index(),
            name: capture.name.clone(),
            path: capture.path.clone(),
            start: display_capture.span.start,
            end: display_capture.span.end,
            grid_start: capture.span.start.saturating_sub(matched.start) + 1,
            grid_span: capture.span.end.saturating_sub(capture.span.start).max(1),
            detail_html: TrustedFragmentHtml::from_rendered(
                CaptureInspectionFragment {
                    capture: &display_capture,
                }
                .render()?,
            ),
        })
    };
    let captures = inspection
        .captures
        .iter()
        .map(render_capture)
        .collect::<Result<Vec<_>, _>>()?;
    let lanes = inspection
        .capture_lanes
        .iter()
        .map(|lane| SampleCaptureLane {
            captures: lane
                .captures
                .iter()
                .filter_map(|ordinal| {
                    captures
                        .iter()
                        .find(|capture| capture.ordinal == ordinal.index())
                        .cloned()
                })
                .collect(),
        })
        .collect();
    Ok(SampleCaptureView { captures, lanes })
}

/// Result of analyzing sample text (or pre-tokenized lines) against an lingrep patterns
/// pattern source. Projection failures surface as `Failed`, never as a
/// silently-empty `Matched`.
#[derive(Debug, Clone)]
pub enum SampleAnalysis {
    Matched {
        tokens: Vec<TokenView>,
        matches: Vec<MatchView>,
        constellations: Vec<SampleConstellation>,
    },
    NoMatches {
        tokens: Vec<TokenView>,
    },
    FragmentsOnly {
        tokens: Vec<TokenView>,
    },
    Failed(SampleAnalysisError),
}

#[derive(Debug, Clone)]
pub struct SampleAnalysisError {
    pub message: String,
}

/// A view that could not be rendered, in the shape the sample surface reports.
fn rendering_failed(error: askama::Error) -> SampleAnalysisError {
    SampleAnalysisError {
        message: format!("Rendering failed: {error}"),
    }
}

/// The one-line pieces of a sample result, built from one explanation pass.
struct LineAnalysis {
    matches: Vec<MatchView>,
    constellation: SampleConstellation,
}

// The error is `SampleAnalysisError` rather than `askama::Error` because two
// independent operations can fail here, and only one is rendering: a
// declared admission anchor the catalog cannot resolve stops the seam before
// any view is built. Reporting that as "Rendering failed" would name the wrong
// stage, and reporting it as an empty match list would show the researcher a
// sample that matched nothing.
fn build_line_analysis(
    line: &[TokenInfo],
    matcher: &lingrep_core::fsm::CompiledCatalog,
    base_index: usize,
    render_offset: usize,
) -> Result<LineAnalysis, SampleAnalysisError> {
    let mut out = Vec::new();
    // The editor needs both raw candidates and what policy publishes. The
    // matcher owns that one policy application so this view cannot drift from
    // normal publication behavior.
    let explanation = matcher
        .explain_sentence(line)
        .map_err(|error| SampleAnalysisError {
            message: format!("Result policy failed: {error}"),
        })?;
    for m in explanation
        .candidates
        .iter()
        .filter_map(|candidate| candidate.published.as_ref())
    {
        let matched_text: String = line[m.start..m.end]
            .iter()
            .filter_map(|t| t.orth())
            .collect();
        let tokens: Vec<TokenView> = line[m.start..m.end]
            .iter()
            .enumerate()
            .map(|(i, t)| TokenView::from_token_info(t, base_index + m.start + i))
            .collect::<Result<_, _>>()
            .map_err(rendering_failed)?; // propagate, do NOT unwrap_or_default
        let capture_view = sample_capture_view(line, m, base_index).map_err(rendering_failed)?; // propagate
        out.push(MatchView {
            render_index: render_offset + out.len(),
            pattern_name: m.pattern.to_string(),
            start: base_index + m.start,
            end: base_index + m.end,
            matched_text,
            tokens,
            captures: capture_view.captures,
            capture_lanes: capture_view.lanes,
        });
    }
    let tokens = line
        .iter()
        .enumerate()
        .map(|(index, token)| TokenView::from_token_info(token, base_index + index))
        .collect::<Result<Vec<_>, _>>()
        .map_err(rendering_failed)?;
    // One projection pass feeds both the raw constellation and the published
    // composition; they must not be derived from two different explanations.
    let analysis = project_sentence_patterns(matcher, &explanation, line, base_index);
    Ok(LineAnalysis {
        matches: out,
        constellation: SampleConstellation {
            tokens,
            groups: analysis.constellation.groups,
            composition: analysis.composition,
        },
    })
}

pub fn analyze_tokens(pattern_source: &str, lines: &[Vec<TokenInfo>]) -> SampleAnalysis {
    let matcher = match compile_web_pattern(pattern_source) {
        Ok(output) => output.matcher,
        Err(error) => {
            return SampleAnalysis::Failed(SampleAnalysisError {
                message: format_pattern_compile_error(&error),
            });
        }
    };

    let mut all_tokens: Vec<TokenInfo> = Vec::new();
    let mut all_matches: Vec<MatchView> = Vec::new();
    let mut constellations: Vec<SampleConstellation> = Vec::new();
    let mut has_candidates = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let base_index = all_tokens.len();
        match build_line_analysis(line, &matcher, base_index, all_matches.len()) {
            Ok(mut analysis) => {
                has_candidates |= !analysis.constellation.groups.is_empty();
                all_matches.append(&mut analysis.matches);
                constellations.push(analysis.constellation);
            }
            Err(error) => return SampleAnalysis::Failed(error),
        }
        all_tokens.extend(line.iter().cloned());
    }

    let token_views = match all_tokens
        .iter()
        .enumerate()
        .map(|(i, t)| TokenView::from_token_info(t, i))
        .collect::<Result<Vec<_>, _>>()
        .map_err(rendering_failed)
    {
        Ok(views) => views,
        Err(error) => return SampleAnalysis::Failed(error),
    };

    if matcher.patterns().is_empty() {
        SampleAnalysis::FragmentsOnly {
            tokens: token_views,
        }
    } else if !has_candidates {
        SampleAnalysis::NoMatches {
            tokens: token_views,
        }
    } else {
        SampleAnalysis::Matched {
            tokens: token_views,
            matches: all_matches,
            constellations,
        }
    }
}

pub fn analyze_sample(
    pattern_source: &str,
    sample_text: &str,
    tokenizer: &Tokenizer,
) -> SampleAnalysis {
    let lines: Vec<Vec<TokenInfo>> = sample_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| tokenize(line, tokenizer))
        .collect();
    analyze_tokens(pattern_source, &lines)
}
