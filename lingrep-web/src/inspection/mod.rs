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

/// Split a projected match into orth-joined left / key / right text. Shared by
/// the pattern-detail view and the results export so their KWIC text is
/// identical by construction. All consumers route through
/// [`lingrep_core::utils::join_orth`].
///
/// Missing-orth tokens (`summary.orth == None`) are filtered before joining —
/// the contract is that they are transparent (boundaries become adjacent).
/// Projected tokens normally carry `orth = Some`; `None` indicates a degenerate
/// record.
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
    fn orths(tokens: &[InspectedToken]) -> impl Iterator<Item = &str> {
        tokens.iter().filter_map(|t| t.summary.orth.as_deref())
    }
    (
        lingrep_core::utils::join_orth(orths(&inspection.tokens[..first_match])),
        lingrep_core::utils::join_orth(orths(&inspection.tokens[first_match..last_match])),
        lingrep_core::utils::join_orth(orths(&inspection.tokens[last_match..])),
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
    fn kwic_orth_split_spaces_spaced_script_inside_each_field() {
        // F6: boundaries inside pre, key, and post.
        // tokens (idx 0..7): 前 DNA machine learning 解析 model 評価
        // match span 2..5 → key = [machine, learning, 解析]
        //   pre  = [前, DNA]          → 前→DNA CJK→spaced → concat → "前DNA"
        //   key  = [machine, learning, 解析] → machine→learning spaced→spaced → SPACE;
        //                                          learning→解析 spaced→CJK → concat
        //                                     → "machine learning解析"
        //   post = [model, 評価]      → model→評価 spaced→CJK → concat → "model評価"
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
        assert_eq!(left, "前DNA");
        assert_eq!(key, "machine learning解析");
        assert_eq!(right, "model評価");
    }

    #[test]
    fn kwic_orth_split_missing_orth_is_transparent_within_a_field() {
        // F5: a token whose summary.orth is None is filtered out before
        // join_orth; the surrounding present orths in the SAME field become
        // adjacent (transparent contract). The None sits between two Context
        // tokens in the pre field, so alpha and beta become adjacent and
        // (both spaced-script) get a joining space.
        use crate::inspection::projector::{
            InspectedToken, MatchInspection, TokenRole, TokenSummary,
        };
        use compact_str::CompactString;
        let mk = |orth: Option<&str>, role: TokenRole, idx: usize| InspectedToken {
            sentence_index: idx,
            role,
            summary: TokenSummary {
                orth: orth.map(CompactString::from),
                ..Default::default()
            },
            capture_membership: Box::default(),
        };
        // pre field (Context): alpha, None, beta ; key field (Match): key
        let inspection = MatchInspection {
            tokens: Box::from(vec![
                mk(Some("alpha"), TokenRole::Context, 0),
                mk(None, TokenRole::Context, 1),
                mk(Some("beta"), TokenRole::Context, 2),
                mk(Some("key"), TokenRole::Match, 3),
            ]),
            visible_range: 0..4,
            captures: Box::default(),
            capture_lanes: Box::default(),
        };
        // None filtered → pre = [alpha, beta] → join_orth spaces them.
        let (left, key, right) = kwic_orth_split(&inspection);
        assert_eq!(
            left, "alpha beta",
            "missing orth transparent within a field"
        );
        assert_eq!(key, "key");
        assert_eq!(right, "");
    }

    #[test]
    fn kwic_orth_split_preserves_the_shared_join_policy() {
        let tokens = vec![
            tok("前"),
            tok("DNA"),
            tok("machine"),
            tok("learning"),
            tok("解析"),
            tok("model"),
            tok("評価"),
        ];
        let match_span = 2..5;
        let inspection = crate::inspection::projector::InspectionProjector::project_window(
            &tokens,
            match_span,
            WHOLE_SENTENCE_CONTEXT,
        );
        let (left, key, right) = kwic_orth_split(&inspection);

        assert_eq!(
            (left.as_str(), key.as_str(), right.as_str()),
            ("前DNA", "machine learning解析", "model評価")
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
        // [Some("a"), None, Some("b")] in the key field — empty-transparent
        // (core, orth()="") ≡ filter-None (web). Both yield "a b".
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
        assert_eq!(key, "a b", "web: filter-None → same spaced boundary");
        assert_eq!((left.as_str(), right.as_str()), ("", ""));
    }
}
