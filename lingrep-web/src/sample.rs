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
use lingrep_core::analysis::{AnalyzerPlan, VibratoPlan};
use lingrep_core::fsm::{CompileOutput, ExampleFailure, ExampleFailureKind, ExampleReport};
use lingrep_core::token_match::{TokenInfo, TokenSchema};

#[derive(Debug, Clone)]
pub struct TokenView {
    pub index: usize,
    pub orth: String,
    pub lemma: String,
    pub pos: String,
    pub cform: String,
    pub inspection_html: TrustedFragmentHtml,
    /// In a match: the pattern source ranges that consumed this token, as
    /// space-separated `from-to` UTF-16 offsets, the token element first and
    /// then each reference that reached it. Empty when not derived.
    pub source_ranges: String,
}

impl TokenView {
    pub fn from_token_info(token: &TokenInfo, index: usize) -> Result<Self, askama::Error> {
        Ok(Self {
            index,
            orth: token
                .surface()
                .or(token.orth())
                .unwrap_or_default()
                .to_owned(),
            lemma: token.lemma().unwrap_or_default().to_owned(),
            pos: token.pos().unwrap_or_default().to_owned(),
            cform: token.cform().unwrap_or_default().to_owned(),
            inspection_html: TrustedFragmentHtml::from_rendered(render_sample_token(token)?),
            source_ranges: String::new(),
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
    /// The edit that adds this match's sentence to the definition's `examples`.
    pub add_example: Option<ClaimInsert>,
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

/// Result of analyzing sample text (or pre-tokenized lines) against a lingrep patterns
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
    candidates: Box<[lingrep_core::fsm::ExplainedMatch]>,
}

// The error is `SampleAnalysisError` rather than `askama::Error` because two
// independent operations can fail here, and only one is rendering: a
// declared admission anchor the catalog cannot resolve stops the seam before
// any view is built. Reporting that as "Rendering failed" would name the wrong
// stage, and reporting it as an empty match list would show the researcher a
// sample that matched nothing.
/// `line` holds the tokens as displayed, `sequence` the same tokens as the
/// matcher read them, and `explanation` what the matcher found on `sequence`.
fn build_line_analysis<S: lingrep_core::token_match::TokenSequence + ?Sized>(
    line: &[TokenInfo],
    sequence: &S,
    explanation: lingrep_core::fsm::SentenceExplanation,
    output: &CompileOutput,
    pattern_source: &str,
    base_index: usize,
    render_offset: usize,
) -> Result<LineAnalysis, SampleAnalysisError> {
    let matcher = &output.matcher;
    let sentence = surface_text(line);
    let mut out = Vec::new();
    for m in explanation
        .candidates
        .iter()
        .filter_map(|candidate| candidate.published.as_ref())
    {
        let matched_text: String = line[m.start..m.end]
            .iter()
            .filter_map(|t| t.surface().or(t.orth()))
            .collect();
        let mut tokens: Vec<TokenView> = line[m.start..m.end]
            .iter()
            .enumerate()
            .map(|(i, t)| TokenView::from_token_info(t, base_index + m.start + i))
            .collect::<Result<_, _>>()
            .map_err(rendering_failed)?; // propagate, do NOT unwrap_or_default
        let utf16 = |byte: usize| {
            pattern_source
                .get(..byte)
                .map(|before| before.encode_utf16().count())
        };
        for consumed in output
            .consumed_tokens(&m.pattern, sequence, m.start, m.end)
            .unwrap_or_default()
        {
            let Some(view) = consumed
                .token
                .checked_sub(m.start)
                .and_then(|offset| tokens.get_mut(offset))
            else {
                continue;
            };
            view.source_ranges = consumed
                .elements
                .iter()
                .filter_map(|element| {
                    Some(format!(
                        "{}-{}",
                        utf16(element.byte_range.start)?,
                        utf16(element.byte_range.end)?
                    ))
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
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
            add_example: claim_insert(output, pattern_source, &m.pattern, "examples", &sentence),
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
        candidates: explanation.candidates,
    })
}

fn whole<'a>(lines: &[&'a [TokenInfo]]) -> Vec<lingrep_core::fsm::ContextSegment<'a, [TokenInfo]>> {
    lines
        .iter()
        .map(|line| lingrep_core::fsm::ContextSegment {
            tokens: *line,
            range: 0..line.len(),
        })
        .collect()
}

pub fn analyze_tokens(pattern_source: &str, lines: &[Vec<TokenInfo>]) -> SampleAnalysis {
    match compile_web_pattern(pattern_source) {
        Ok(output) => analyze_compiled(&output, pattern_source, lines).0,
        Err(error) => SampleAnalysis::Failed(SampleAnalysisError {
            message: format_pattern_compile_error(&error),
        }),
    }
}

/// Sample lines explained per definition, at most.
const UNMATCHED_LINE_LIMIT: usize = 20;
/// Definitions explained per unmatched line, at most.
const UNMATCHED_DEFINITION_LIMIT: usize = 64;

fn analyze_compiled(
    output: &CompileOutput,
    pattern_source: &str,
    lines: &[Vec<TokenInfo>],
) -> (SampleAnalysis, Vec<UnmatchedLine>) {
    let matcher = &output.matcher;
    let mut unmatched: Vec<UnmatchedLine> = Vec::new();
    let mut all_tokens: Vec<TokenInfo> = Vec::new();
    let mut all_matches: Vec<MatchView> = Vec::new();
    let mut constellations: Vec<SampleConstellation> = Vec::new();
    let mut has_candidates = false;
    let lines: Vec<&[TokenInfo]> = lines
        .iter()
        .map(Vec::as_slice)
        .filter(|line| !line.is_empty())
        .collect();
    for (index, line) in lines.iter().copied().enumerate() {
        let context = lingrep_core::fsm::LookaroundContext {
            windows: std::collections::BTreeMap::from([(
                lingrep_core::fsm::LookaroundScope::Document,
                lingrep_core::fsm::ContextWindow {
                    before: whole(&lines[..index]),
                    after: whole(&lines[index + 1..]),
                },
            )]),
        };
        let base_index = all_tokens.len();
        // The editor needs both raw candidates and what policy publishes. The
        // matcher owns that one policy application so this view cannot drift
        // from normal publication behavior.
        let explanation = match matcher.try_explain_sentence_in_context(line, &context) {
            Ok(explanation) => explanation,
            Err(error) => {
                return (
                    SampleAnalysis::Failed(SampleAnalysisError {
                        message: format!("Could not inspect this sentence: {error}"),
                    }),
                    Vec::new(),
                );
            }
        };
        match build_line_analysis(
            line,
            line,
            explanation,
            output,
            pattern_source,
            base_index,
            all_matches.len(),
        ) {
            Ok(mut analysis) => {
                if analysis.matches.is_empty() && unmatched.len() < UNMATCHED_LINE_LIMIT {
                    unmatched.push(UnmatchedLine {
                        text: surface_text(line),
                        definitions: matcher
                            .patterns()
                            .iter()
                            .take(UNMATCHED_DEFINITION_LIMIT)
                            .filter_map(|(name, _)| {
                                let mut why = why_not(
                                    output,
                                    pattern_source,
                                    name,
                                    line,
                                    &analysis.candidates,
                                )?;
                                if matches!(
                                    why.outcome,
                                    WhyNotOutcome::Stopped(_) | WhyNotOutcome::WhereRejected { .. }
                                ) {
                                    why.add_counterexample = claim_insert(
                                        output,
                                        pattern_source,
                                        name,
                                        "counterexamples",
                                        &surface_text(line),
                                    );
                                }
                                Some(why)
                            })
                            .collect(),
                    });
                }
                has_candidates |= !analysis.constellation.groups.is_empty();
                all_matches.append(&mut analysis.matches);
                constellations.push(analysis.constellation);
            }
            Err(error) => return (SampleAnalysis::Failed(error), Vec::new()),
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
        Err(error) => return (SampleAnalysis::Failed(error), Vec::new()),
    };

    let analysis = if matcher.patterns().is_empty() {
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
    };
    (analysis, unmatched)
}

fn surface_text(tokens: &[TokenInfo]) -> String {
    tokens
        .iter()
        .filter_map(|token| token.surface().or(token.orth()))
        .collect()
}

/// A sample line on which no definition published a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmatchedLine {
    pub text: String,
    pub definitions: Vec<WhyNot>,
}

/// Why one definition published no match for one sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyNot {
    pub definition: String,
    pub outcome: WhyNotOutcome,
    /// Start positions the explanation pass attempted.
    pub starts_tried: usize,
    /// The explanation pass reached its step limit.
    pub truncated: bool,
    /// The edit that adds the sentence to the definition's `counterexamples`.
    pub add_counterexample: Option<ClaimInsert>,
}

/// One insertion into the pattern source that adds a sentence to a
/// definition's `examples` or `counterexamples`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimInsert {
    /// Byte offset in the pattern source.
    pub offset: usize,
    pub text: String,
    /// The source text at `offset` when the edit was computed.
    pub expects: String,
}

/// `None` when the list already holds `sentence`, the key is inherited, or
/// `definition` is not a pattern of this catalog.
fn claim_insert(
    output: &CompileOutput,
    pattern_source: &str,
    definition: &str,
    key: &str,
    sentence: &str,
) -> Option<ClaimInsert> {
    use lingrep_core::fsm::ClaimSite;
    let quoted = serde_json::to_string(sentence).ok()?;
    let expects = |offset: usize| {
        pattern_source
            .get(offset..)
            .map(|rest| rest.chars().take(16).collect::<String>())
    };
    match output.claim_site(definition, key)? {
        ClaimSite::Inherited => None,
        ClaimSite::Absent { definition_start } => Some(ClaimInsert {
            offset: definition_start,
            text: format!("{key}: [{quoted}]\n"),
            expects: expects(definition_start)?,
        }),
        ClaimSite::Declared { key: written } => {
            let declared = output
                .matcher
                .patterns()
                .get(definition)?
                .metadata
                .get(key)?;
            let claims = declared.as_array()?;
            if claims.iter().any(|claim| claim.as_str() == Some(sentence)) {
                return None;
            }
            let close = list_close(pattern_source, written.end)?;
            Some(ClaimInsert {
                offset: close,
                text: if claims.is_empty() {
                    quoted
                } else {
                    format!(", {quoted}")
                },
                expects: expects(close)?,
            })
        }
    }
}

/// The byte offset of the `]` that closes the list written after the key
/// ending at `key_end`.
fn list_close(source: &str, key_end: usize) -> Option<usize> {
    let rest = source.get(key_end..)?;
    let (before, list) = rest.split_at_checked(rest.find('[')?)?;
    if before.trim() != ":" {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in list.char_indices() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(key_end + before.len() + index);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhyNotOutcome {
    Stopped(WhyNotStop),
    /// The definition matched these token spans and policy published none.
    NotPublished {
        spans: String,
    },
    /// The body matched these token spans and the `where` clause rejected each.
    WhereRejected {
        spans: String,
    },
    Unexplained {
        reason: String,
    },
}

/// The attempt that matched the most tokens before an element failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyNotStop {
    pub start: usize,
    pub consumed: usize,
    pub position: usize,
    /// The surface text of the token at `position`; `None` past the sentence end.
    pub token: Option<String>,
    pub kind: WhyNotStopKind,
    pub element: Option<WhyNotElement>,
    pub tests: Vec<lingrep_core::fsm::AttributeTestOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyNotStopKind {
    Token,
    SentenceEnded,
    NotSentenceStart,
    NotSentenceEnd,
    Lookaround,
}

/// The failing element as written in the pattern source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyNotElement {
    pub line: usize,
    /// One-based, in characters.
    pub column: usize,
    pub text: String,
}

/// `None` when the definition published a match.
fn why_not(
    output: &CompileOutput,
    pattern_source: &str,
    definition: &str,
    tokens: &[TokenInfo],
    candidates: &[lingrep_core::fsm::ExplainedMatch],
) -> Option<WhyNot> {
    why_not_over(
        output,
        pattern_source,
        definition,
        tokens,
        tokens,
        candidates,
    )
}

/// [`why_not`] with the tokens as displayed apart from the tokens as matched.
fn why_not_over<S: lingrep_core::token_match::TokenSequence + ?Sized>(
    output: &CompileOutput,
    pattern_source: &str,
    definition: &str,
    tokens: &[TokenInfo],
    sequence: &S,
    candidates: &[lingrep_core::fsm::ExplainedMatch],
) -> Option<WhyNot> {
    use lingrep_core::fsm::{DefinitionOutcome, StopReason};
    let explanation = output
        .no_match
        .explain_definition(definition, sequence, candidates);
    let spans = |spans: Vec<(usize, usize)>| {
        spans
            .iter()
            .map(|(start, end)| format!("{start}..{end}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let outcome = match explanation.outcome {
        DefinitionOutcome::Published { .. } => return None,
        DefinitionOutcome::NotPublished { spans: found } => WhyNotOutcome::NotPublished {
            spans: spans(found),
        },
        DefinitionOutcome::WhereRejected { spans: found } => WhyNotOutcome::WhereRejected {
            spans: spans(found),
        },
        DefinitionOutcome::Unexplained { reason } => WhyNotOutcome::Unexplained { reason },
        DefinitionOutcome::Stopped(attempt) => {
            let (kind, tests) = match attempt.reason {
                StopReason::Token { tests } => (WhyNotStopKind::Token, tests),
                StopReason::SentenceEnded => (WhyNotStopKind::SentenceEnded, Vec::new()),
                StopReason::NotSentenceStart => (WhyNotStopKind::NotSentenceStart, Vec::new()),
                StopReason::NotSentenceEnd => (WhyNotStopKind::NotSentenceEnd, Vec::new()),
                StopReason::Lookaround { .. } => (WhyNotStopKind::Lookaround, Vec::new()),
            };
            WhyNotOutcome::Stopped(WhyNotStop {
                start: attempt.start,
                consumed: attempt.position - attempt.start,
                position: attempt.position,
                token: tokens
                    .get(attempt.position)
                    .and_then(|token| token.surface().or(token.orth()))
                    .map(str::to_owned),
                kind,
                element: attempt.element.map(|element| {
                    let before = pattern_source
                        .get(..element.byte_range.start)
                        .unwrap_or_default();
                    WhyNotElement {
                        line: element.line,
                        column: before.chars().rev().take_while(|&c| c != '\n').count() + 1,
                        text: pattern_source
                            .get(element.byte_range)
                            .unwrap_or_default()
                            .to_owned(),
                    }
                }),
                tests,
            })
        }
    };
    Some(WhyNot {
        definition: definition.to_owned(),
        outcome,
        starts_tried: explanation.starts_tried,
        truncated: explanation.truncated,
        add_counterexample: None,
    })
}

/// A referenced attribute the sample's tokenizer does not supply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableAttribute {
    pub attribute: String,
    /// The definitions that reference it; empty when only the catalog
    /// configuration does.
    pub definitions: Vec<String>,
}

/// Whether the catalog binds to the sample tokenizer's token schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleBinding {
    Bound {
        unavailable: Vec<UnavailableAttribute>,
    },
    /// Binding error for the sample token schema.
    Refused(String),
}

/// The catalog's `examples` and `counterexamples` claims, checked with the
/// sample's tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleClaims {
    Undeclared,
    /// Skipped because binding failed or a referenced attribute was unavailable.
    Unverifiable,
    Checked(ClaimsReport),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimsReport {
    pub checked: usize,
    /// Claims declared after the first [`ClaimBudget::claims`].
    pub unchecked: usize,
    pub failures: Vec<ClaimFailure>,
    /// Counterexamples that hold and that the definition does not match with
    /// any one constraint removed.
    pub vacuous: Vec<VacuousClaim>,
    /// Checked counterexamples left out of the `vacuous` test because
    /// [`ClaimBudget::relaxation`] ran out.
    pub unrelaxed: usize,
}

/// The claim work one sample run does for a catalog.
///
/// Each claim is tokenized and matched twice, and each counterexample is
/// matched again once per constraint of its definition, so the work grows
/// with the catalog and not with the sample text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimBudget {
    /// Claims checked, in catalog order.
    pub claims: usize,
    /// Time after which no further counterexample is relaxed. The relaxation
    /// under way finishes.
    pub relaxation: std::time::Duration,
}

impl ClaimBudget {
    /// The budget of a sample request. The largest tracked catalog declares
    /// 116 claims and its relaxations take milliseconds.
    pub const REQUEST: Self = Self {
        claims: 256,
        relaxation: std::time::Duration::from_secs(2),
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VacuousClaim {
    pub definition: String,
    pub example: String,
    /// Relaxations tried: each attribute test, token element and lookaround.
    pub tried: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimFailure {
    pub definition: String,
    pub example: String,
    pub kind: ClaimFailureKind,
    /// For an example the definition does not match or publish.
    pub why_not: Option<WhyNot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimFailureKind {
    ExpectedMatch,
    UnexpectedMatch,
    Malformed,
    /// The definition matches its example, and result policy publishes no
    /// record for it.
    ExpectedPublication,
    /// The definition does not match its counterexample by itself, and result
    /// policy publishes a record under its name.
    UnexpectedPublication,
}

/// What ran a sample, and what that run cannot show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleRun {
    /// Sample lines without a published match, each explained per definition.
    pub unmatched: Vec<UnmatchedLine>,
    /// The analyzer and dictionary that tokenized the sample.
    pub analyzer: String,
    /// The tokens are a corpus sentence's stored analysis, and `analyzer`
    /// names the corpus's analyzer.
    pub stored: bool,
    /// The selected corpus's analyzer, when its token schema differs from the
    /// sample tokenizer's.
    pub corpus_analyzer: Option<String>,
    pub binding: SampleBinding,
    pub claims: SampleClaims,
}

/// Binds `output` to `schema` and, when every referenced attribute is
/// available, checks the catalog's claims through `tokenize` within `budget`.
pub fn sample_run(
    output: &CompileOutput,
    pattern_source: &str,
    schema: &TokenSchema,
    analyzer: String,
    tokenize: &dyn Fn(&str) -> Vec<TokenInfo>,
    budget: ClaimBudget,
) -> SampleRun {
    let binding = sample_binding(output, schema);
    sample_run_bound(output, pattern_source, analyzer, binding, tokenize, budget)
}

fn sample_binding(output: &CompileOutput, schema: &TokenSchema) -> SampleBinding {
    match output
        .matcher
        .bind_with_projection(schema, output.matcher.execution_attributes())
    {
        Ok(bound) => SampleBinding::Bound {
            unavailable: bound
                .unavailable_attributes()
                .iter()
                .map(|attribute| UnavailableAttribute {
                    attribute: attribute.as_str().to_owned(),
                    definitions: output
                        .analysis
                        .references_to(attribute)
                        .map(|site| site.definition.to_string())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                })
                .collect(),
        },
        Err(error) => SampleBinding::Refused(error.to_string()),
    }
}

fn sample_run_bound(
    output: &CompileOutput,
    pattern_source: &str,
    analyzer: String,
    binding: SampleBinding,
    tokenize: &dyn Fn(&str) -> Vec<TokenInfo>,
    budget: ClaimBudget,
) -> SampleRun {
    let declared = output.matcher.patterns().iter().any(|(_, pattern)| {
        pattern.metadata.contains_key("examples")
            || pattern.metadata.contains_key("counterexamples")
    });
    let complete =
        matches!(&binding, SampleBinding::Bound { unavailable } if unavailable.is_empty());
    let claims = if !declared {
        SampleClaims::Undeclared
    } else if !complete {
        SampleClaims::Unverifiable
    } else {
        match lingrep_core::fsm::try_verify_examples_within(
            &output.matcher,
            tokenize,
            budget.claims,
        ) {
            Ok(report) => {
                let mut report = claims_report(report);
                for failure in &mut report.failures {
                    if matches!(
                        failure.kind,
                        ClaimFailureKind::ExpectedMatch | ClaimFailureKind::ExpectedPublication
                    ) {
                        let tokens = tokenize(&failure.example);
                        failure.why_not = output
                            .matcher
                            .try_explain_sentence(tokens.as_slice())
                            .ok()
                            .and_then(|explained| {
                                why_not(
                                    output,
                                    pattern_source,
                                    &failure.definition,
                                    &tokens,
                                    &explained.candidates,
                                )
                            });
                    }
                }
                let relaxing = std::time::Instant::now();
                let counterexamples = lingrep_core::fsm::declared_claims(&output.matcher)
                    .take(budget.claims)
                    .filter(|(_, must_match, _)| !must_match);
                for (definition, _, example) in counterexamples {
                    if report.failures.iter().any(|failure| {
                        failure.definition == definition.as_str() && failure.example == example
                    }) {
                        continue;
                    }
                    if relaxing.elapsed() >= budget.relaxation {
                        report.unrelaxed += 1;
                        continue;
                    }
                    if let lingrep_core::fsm::RelaxationOutcome::Vacuous { tried } =
                        output.relax_definition(definition, tokenize(example).as_slice())
                    {
                        report.vacuous.push(VacuousClaim {
                            definition: definition.to_string(),
                            example: example.to_owned(),
                            tried: tried.len(),
                        });
                    }
                }
                SampleClaims::Checked(report)
            }
            Err(error) => SampleClaims::Failed(error.to_string()),
        }
    };
    SampleRun {
        unmatched: Vec::new(),
        analyzer,
        corpus_analyzer: None,
        stored: false,
        binding,
        claims,
    }
}

// Skip publication failures whose definition and text already have a raw-match failure.
fn claims_report(report: ExampleReport) -> ClaimsReport {
    let raw = |failure: ExampleFailure| ClaimFailure {
        kind: match failure.kind {
            ExampleFailureKind::ExpectedMatch => ClaimFailureKind::ExpectedMatch,
            ExampleFailureKind::UnexpectedMatch => ClaimFailureKind::UnexpectedMatch,
            ExampleFailureKind::Malformed => ClaimFailureKind::Malformed,
        },
        definition: failure.definition,
        example: failure.example,
        why_not: None,
    };
    let mut failures: Vec<ClaimFailure> = report.failures.into_iter().map(raw).collect();
    let published: Vec<ClaimFailure> = report
        .post_policy_failures
        .into_iter()
        .filter(|failure| {
            !failures.iter().any(|seen| {
                seen.definition == failure.definition && seen.example == failure.example
            })
        })
        .map(|failure| ClaimFailure {
            kind: match failure.kind {
                ExampleFailureKind::UnexpectedMatch => ClaimFailureKind::UnexpectedPublication,
                ExampleFailureKind::ExpectedMatch | ExampleFailureKind::Malformed => {
                    ClaimFailureKind::ExpectedPublication
                }
            },
            definition: failure.definition,
            example: failure.example,
            why_not: None,
        })
        .collect();
    failures.extend(published);
    ClaimsReport {
        checked: report.checked,
        unchecked: report.unchecked,
        failures,
        vacuous: Vec::new(),
        unrelaxed: 0,
    }
}

/// Sample analysis and its run details. `run` is absent when compilation fails.
#[derive(Debug, Clone)]
pub struct SampleReport {
    pub analysis: SampleAnalysis,
    pub run: Option<SampleRun>,
}

pub fn analyze_sample(
    pattern_source: &str,
    sample_text: &str,
    plan: &VibratoPlan,
    analyzer: String,
) -> SampleReport {
    let output = match compile_web_pattern(pattern_source) {
        Ok(output) => output,
        Err(error) => {
            return SampleReport {
                analysis: SampleAnalysis::Failed(SampleAnalysisError {
                    message: format_pattern_compile_error(&error),
                }),
                run: None,
            };
        }
    };
    let lines: Vec<Vec<TokenInfo>> = sample_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| plan.tokenize(line))
        .collect();
    let (analysis, unmatched) = analyze_compiled(&output, pattern_source, &lines);
    let mut run = sample_run(
        &output,
        pattern_source,
        plan.schema(),
        analyzer,
        &|text| plan.tokenize(text),
        ClaimBudget::REQUEST,
    );
    run.unmatched = unmatched;
    SampleReport {
        analysis,
        run: Some(run),
    }
}

/// A corpus sentence explained with its stored analysis: the catalog is bound
/// to the corpus's token schema and reads the stored tokens, including the
/// attributes the sample tokenizer cannot supply.
///
/// The catalog's examples and counterexamples are not checked, because the
/// corpus's analyzer is not available to analyze them.
pub fn analyze_stored_sentence(
    pattern_source: &str,
    schema: &TokenSchema,
    sentence: &lingrep_core::token_match::BoundTokenSentence,
    analyzer: String,
) -> SampleReport {
    let failed = |message: String, run| SampleReport {
        analysis: SampleAnalysis::Failed(SampleAnalysisError { message }),
        run,
    };
    let output = match compile_web_pattern(pattern_source) {
        Ok(output) => output,
        Err(error) => return failed(format_pattern_compile_error(&error), None),
    };
    let run = SampleRun {
        unmatched: Vec::new(),
        analyzer,
        corpus_analyzer: None,
        stored: true,
        binding: sample_binding(&output, schema),
        claims: SampleClaims::Undeclared,
    };
    let tokens = match (0..sentence.len())
        .map(|index| stored_token(schema, sentence, index))
        .collect::<Option<Vec<_>>>()
    {
        Some(tokens) => tokens,
        None => {
            return failed(
                "Could not read this sentence's tokens.".to_owned(),
                Some(run),
            );
        }
    };
    let views = |tokens: &[TokenInfo]| {
        tokens
            .iter()
            .enumerate()
            .map(|(index, token)| TokenView::from_token_info(token, index))
            .collect::<Result<Vec<_>, _>>()
            .map_err(rendering_failed)
    };
    let bound = match output
        .matcher
        .bind_with_projection(schema, schema.keys().iter().cloned())
    {
        Ok(bound) => bound,
        // `run.binding` carries the refusal.
        Err(_) => {
            return match views(&tokens) {
                Ok(tokens) => SampleReport {
                    analysis: SampleAnalysis::NoMatches { tokens },
                    run: Some(run),
                },
                Err(error) => SampleReport {
                    analysis: SampleAnalysis::Failed(error),
                    run: Some(run),
                },
            };
        }
    };
    let explanation = match bound.explain_input_in_windows(
        &lingrep_core::execution::BoundSentenceInput::new(
            sentence.clone(),
            lingrep_core::execution::EnhancedGraphInput::NotRequested,
        ),
        &[],
    ) {
        Ok(explanation) => explanation,
        Err(error) => {
            return failed(
                format!("Could not inspect this sentence: {error}"),
                Some(run),
            );
        }
    };
    let analysis = match build_line_analysis(
        &tokens,
        sentence,
        explanation,
        &output,
        pattern_source,
        0,
        0,
    ) {
        Ok(analysis) => analysis,
        Err(error) => {
            return SampleReport {
                analysis: SampleAnalysis::Failed(error),
                run: Some(run),
            };
        }
    };
    let mut run = run;
    if analysis.matches.is_empty() {
        run.unmatched.push(UnmatchedLine {
            text: surface_text(&tokens),
            definitions: output
                .matcher
                .patterns()
                .iter()
                .take(UNMATCHED_DEFINITION_LIMIT)
                .filter_map(|(name, _)| {
                    why_not_over(
                        &output,
                        pattern_source,
                        name,
                        &tokens,
                        sentence,
                        &analysis.candidates,
                    )
                })
                .collect(),
        });
    }
    let token_views = match views(&tokens) {
        Ok(views) => views,
        Err(error) => {
            return SampleReport {
                analysis: SampleAnalysis::Failed(error),
                run: Some(run),
            };
        }
    };
    let analysis = if output.matcher.patterns().is_empty() {
        SampleAnalysis::FragmentsOnly {
            tokens: token_views,
        }
    } else if analysis.constellation.groups.is_empty() {
        SampleAnalysis::NoMatches {
            tokens: token_views,
        }
    } else {
        SampleAnalysis::Matched {
            tokens: token_views,
            matches: analysis.matches,
            constellations: vec![analysis.constellation],
        }
    };
    SampleReport {
        analysis,
        run: Some(run),
    }
}

/// The stored token at `index` as the sample view displays one.
fn stored_token(
    schema: &TokenSchema,
    sentence: &lingrep_core::token_match::BoundTokenSentence,
    index: usize,
) -> Option<TokenInfo> {
    use lingrep_core::token_match::{AttributeValueRef, TokenAttributeIndex, TokenSequence};
    let mut token = TokenInfo::from_sequence(sentence, index)?;
    let scalar = |name: &str| {
        let key = schema.keys().iter().find(|key| key.as_str() == name)?;
        match sentence.value_key(index, key)? {
            AttributeValueRef::Scalar(value) => Some(value.to_owned()),
            AttributeValueRef::StringSet(_) => None,
        }
    };
    if token.surface().or(token.orth()).is_none()
        && let Some(text) = scalar("surface").or_else(|| scalar("text"))
    {
        token.set(TokenAttributeIndex::Orth, text.into());
    }
    if token.lemma().is_none()
        && let Some(lemma) = scalar("lemma")
    {
        token.set(TokenAttributeIndex::Lemma, lemma.into());
    }
    if token.pos().is_none()
        && let Some(pos) = scalar("upos")
    {
        token.set(TokenAttributeIndex::Pos, pos.into());
    }
    Some(token)
}
