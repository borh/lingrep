// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::unwrap_used)]

use lingrep_core::fsm::{CompileLimits, LexicalMatch, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn token(orth: Option<&str>, norm: Option<&str>) -> TokenInfo {
    let mut token = TokenInfo::default();
    if let Some(value) = orth {
        token.set(TokenAttributeIndex::Orth, value.into());
    }
    if let Some(value) = norm {
        token.set(TokenAttributeIndex::Norm, value.into());
    }
    token
}

fn matcher(source: &str) -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("lex.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

fn lexical_spans(source: &str, tokens: &[TokenInfo]) -> Vec<(usize, usize, String, Vec<String>)> {
    matcher(source)
        .find_matches(tokens)
        .into_iter()
        .map(|matched| {
            let lexical = matched.lexical.unwrap();
            (
                matched.start,
                matched.end,
                lexical.term.to_string(),
                lexical
                    .matched_by
                    .iter()
                    .map(|projection| projection.pattern_key().to_owned())
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn prefixes_overlaps_and_multitoken_members_all_survive() {
    let source = r#"lexicon words by [text] := {"大学", "大学院", "学院"}."#;
    assert_eq!(
        lexical_spans(
            source,
            &[token(Some("大学"), None), token(Some("院"), None)]
        ),
        vec![
            (0, 1, "大学".into(), vec!["text".into()]),
            (0, 2, "大学院".into(), vec!["text".into()]),
        ]
    );
    assert_eq!(
        lexical_spans(
            source,
            &[
                token(Some("大"), None),
                token(Some("学"), None),
                token(Some("院"), None)
            ]
        ),
        vec![
            (0, 2, "大学".into(), vec!["text".into()]),
            (0, 3, "大学院".into(), vec!["text".into()]),
            (1, 3, "学院".into(), vec!["text".into()]),
        ]
    );
}

#[test]
fn hits_require_token_boundaries_and_missing_or_empty_values_flush_runs() {
    let source = r#"lexicon words by [text] := {"Web.config", "ab"}."#;
    assert_eq!(
        lexical_spans(
            source,
            &[
                token(Some("Web"), None),
                token(Some("."), None),
                token(Some("config"), None)
            ]
        ),
        vec![(0, 3, "Web.config".into(), vec!["text".into()])]
    );
    assert!(
        lexical_spans(
            source,
            &[
                token(Some("a"), None),
                token(None, None),
                token(Some("b"), None)
            ]
        )
        .is_empty()
    );
    assert!(
        lexical_spans(
            source,
            &[
                token(Some("a"), None),
                token(Some(""), None),
                token(Some("b"), None)
            ]
        )
        .is_empty()
    );
}

#[test]
fn projection_union_preserves_authored_order_and_term_identity() {
    let both = r#"lexicon words by [norm, text] := {"する"}."#;
    assert_eq!(
        lexical_spans(both, &[token(Some("する"), Some("する"))]),
        vec![(0, 1, "する".into(), vec!["norm".into(), "text".into()])]
    );

    let collision = r#"lexicon words by [text, norm] := {"為る", "する"}."#;
    assert_eq!(
        lexical_spans(collision, &[token(Some("為る"), Some("する"))]),
        vec![
            (0, 1, "する".into(), vec!["norm".into()]),
            (0, 1, "為る".into(), vec!["text".into()]),
        ]
    );
}

#[test]
fn lexical_payload_keeps_ordinary_matches_small() {
    assert_eq!(
        std::mem::size_of::<Option<Box<LexicalMatch>>>(),
        std::mem::size_of::<usize>()
    );
}
