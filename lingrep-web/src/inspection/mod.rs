mod catalog;
mod constellation;
mod projector;
mod snapshot;

pub use catalog::{TOKEN_FIELD_CATALOG, TokenFieldGroupKey, TokenFieldMeta};
pub use constellation::{
    CompositionGap, CompositionItem, CompositionRegion, Constellation, ConstellationGroup,
    ConstellationMatch, OverlapCandidate, PublishedEvidence, SentencePatternAnalysis,
    overlapping_candidates, project_sentence_patterns,
};
pub use projector::{
    CaptureInspection, CaptureLane, CaptureOrdinal, InspectedToken, InspectionProjector,
    MatchInspection, ResolvedCapture, TokenField, TokenFieldGroup, TokenInspection, TokenRole,
    TokenSummary, assign_capture_lanes,
};
pub use snapshot::{InspectionError, SnapshotInspector};
pub const WHOLE_SENTENCE_CONTEXT: usize = usize::MAX;

/// Split a projected match into left / key / right text. Shared by the
/// pattern-detail view and the results export so their KWIC text is identical
/// by construction.
///
/// Each token contributes its `summary.orth`, preceded by a space where its
/// `space_before` is set, so the three fields concatenate to the window's
/// text: a space between the left context and the key ends `left`, and one
/// between the key and the right context begins `right`. `key` carries
/// neither, which keeps it equal to the surface
/// `SnapshotSink::stage_surface` counts for the same span.
///
/// Missing-orth tokens (`summary.orth == None`) are transparent. Projected
/// tokens normally carry `orth = Some`; `None` indicates a degenerate record.
pub(crate) fn kwic_orth_split(inspection: &MatchInspection) -> (String, String, String) {
    let first_match = inspection
        .tokens
        .iter()
        .position(|token| token.role == TokenRole::Match)
        .unwrap_or(inspection.tokens.len());
    let last_match = inspection
        .tokens
        .iter()
        .rposition(|token| token.role == TokenRole::Match)
        .map_or(first_match, |index| index + 1);
    let field = |tokens: &[InspectedToken], leading: bool| {
        let mut text = String::new();
        for (index, token) in tokens.iter().enumerate() {
            if token.space_before && (index > 0 || leading) {
                text.push(' ');
            }
            if let Some(orth) = token.summary.orth.as_deref() {
                text.push_str(orth);
            }
        }
        text
    };
    let mut left = field(&inspection.tokens[..first_match], false);
    let key = &inspection.tokens[first_match..last_match];
    if first_match > 0 && key.first().is_some_and(|token| token.space_before) {
        left.push(' ');
    }
    (
        left,
        field(key, false),
        field(&inspection.tokens[last_match..], true),
    )
}

#[cfg(test)]
mod kwic_split_tests {
    use super::*;
    use lingrep_core::token_match::TokenInfo;

    fn tok(orth: &str) -> TokenInfo {
        TokenInfo::default().with_orth(orth)
    }

    #[test]
    fn kwic_orth_split_partitions_left_key_right() {
        let tokens = vec![tok("だと"), tok("思っ"), tok("た"), tok("のは")];
        let inspection = crate::inspection::projector::InspectionProjector::project_window(
            &tokens,
            1..3,
            WHOLE_SENTENCE_CONTEXT,
        );
        let (left, key, right) = kwic_orth_split(&inspection);
        assert_eq!(left, "だと");
        assert_eq!(key, "思った");
        assert_eq!(right, "のは");
    }

    #[test]
    fn kwic_orth_split_keeps_the_spaces_the_source_has_between_tokens() {
        use lingrep_core::testing::helpers::placed_tokens;
        let split = |source: &str, words: &[&str], span| {
            kwic_orth_split(
                &crate::inspection::projector::InspectionProjector::project_window(
                    &placed_tokens(source, words),
                    span,
                    WHOLE_SENTENCE_CONTEXT,
                ),
            )
        };
        assert_eq!(
            split(
                "I saw NHK news today",
                &["I", "saw", "NHK", "news", "today"],
                2..4
            ),
            ("I saw ".into(), "NHK news".into(), " today".into())
        );
        // UniDic splits a half-width Latin word into letters.
        assert_eq!(
            split(
                "昨日 NHK news を見た。",
                &[
                    "昨日", "N", "H", "K", "n", "e", "w", "s", "を", "見", "た", "。"
                ],
                1..4
            ),
            ("昨日 ".into(), "NHK".into(), " news を見た。".into())
        );
        assert_eq!(
            split(
                "昨日はニュースを見た。",
                &["昨日", "は", "ニュース", "を", "見", "た", "。"],
                2..4
            ),
            ("昨日は".into(), "ニュースを".into(), "見た。".into())
        );
    }

    #[test]
    fn kwic_orth_split_without_spans_spaces_adjacent_spaced_script_tokens() {
        // tokens (idx 0..7): 前 DNA machine learning 解析 model 評価, match 2..5.
        // Without source spans a space is guessed between two letters of a
        // spaced script: DNA→machine, which ends `left`, and machine→learning.
        let tokens = vec![
            tok("前"),
            tok("DNA"),
            tok("machine"),
            tok("learning"),
            tok("解析"),
            tok("model"),
            tok("評価"),
        ];
        let inspection = crate::inspection::projector::InspectionProjector::project_window(
            &tokens,
            2..5,
            WHOLE_SENTENCE_CONTEXT,
        );
        let (left, key, right) = kwic_orth_split(&inspection);
        assert_eq!(left, "前DNA ");
        assert_eq!(key, "machine learning解析");
        assert_eq!(right, "model評価");
    }

    #[test]
    fn kwic_orth_split_missing_orth_is_transparent_within_a_field() {
        // A token whose summary.orth is None contributes no text; the space
        // each token records before itself is still written.
        use crate::inspection::projector::{
            InspectedToken, MatchInspection, TokenRole, TokenSummary,
        };
        use compact_str::CompactString;
        let mk =
            |orth: Option<&str>, role: TokenRole, idx: usize, space_before: bool| InspectedToken {
                sentence_index: idx,
                role,
                space_before,
                summary: TokenSummary {
                    orth: orth.map(CompactString::from),
                    ..Default::default()
                },
                capture_membership: Box::default(),
            };
        let inspection = MatchInspection {
            tokens: Box::from(vec![
                mk(Some("alpha"), TokenRole::Context, 0, false),
                mk(None, TokenRole::Context, 1, false),
                mk(Some("beta"), TokenRole::Context, 2, true),
                mk(Some("key"), TokenRole::Match, 3, false),
            ]),
            visible_range: 0..4,
            captures: Box::default(),
            capture_lanes: Box::default(),
        };
        let (left, key, right) = kwic_orth_split(&inspection);
        assert_eq!(left, "alpha beta");
        assert_eq!(key, "key");
        assert_eq!(right, "");
    }

    #[test]
    fn kwic_orth_split_shows_the_surface_where_text_is_folded() {
        let folded = |text: &str, surface: &str| tok(text).with_surface(surface);
        let tokens = vec![tok("左"), folded("AB", "ＡＢ"), folded("U", "Ｕ")];
        let inspection = crate::inspection::projector::InspectionProjector::project_window(
            &tokens,
            1..2,
            WHOLE_SENTENCE_CONTEXT,
        );
        assert_eq!(
            kwic_orth_split(&inspection),
            ("左".into(), "ＡＢ".into(), "Ｕ".into())
        );
    }

    /// Characterization: the degenerate all-`None`-orth matched span. A
    /// A missing `orth` is transparent in projected display text.
    fn none_tok() -> TokenInfo {
        TokenInfo::default() // orth = None
    }

    #[test]
    fn degenerate_all_none_match_span_has_empty_display_text() {
        // All-None degenerate record: matched tokens carry no orth.
        let tokens: Vec<TokenInfo> = vec![none_tok(), none_tok(), none_tok()];
        let span = 0..2;
        let inspection = crate::inspection::projector::InspectionProjector::project_window(
            &tokens,
            span,
            WHOLE_SENTENCE_CONTEXT,
        );
        let (left, key, right) = kwic_orth_split(&inspection);
        assert_eq!((left.as_str(), key.as_str(), right.as_str()), ("", "", ""));
        // The match is not hidden in a way that breaks the invariant: the key
        // text is empty, but the match span offsets (and upstream detection)
        // still mark it.
        assert_eq!(key, "", "all-None match span → empty key text");
    }

    #[test]
    fn degenerate_mixed_none_in_key_is_transparent() {
        // [Some("a"), None, Some("b")] in the key field: the token without a
        // text contributes nothing, and no space is guessed beside it.
        let t1 = TokenInfo::default().with_orth("a");
        let t2 = TokenInfo::default(); // None
        let t3 = TokenInfo::default().with_orth("b");
        let tokens = vec![t1, t2, t3];
        let span = 0..3; // whole span is the key
        let inspection = crate::inspection::projector::InspectionProjector::project_window(
            &tokens,
            span,
            WHOLE_SENTENCE_CONTEXT,
        );
        let (left, key, right) = kwic_orth_split(&inspection);
        assert_eq!(key, "ab");
        assert_eq!((left.as_str(), right.as_str()), ("", ""));
    }
}
