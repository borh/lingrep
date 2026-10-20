//! Tests for japanese_sentence proptest strategy

use lingrep_core::testing::strategies::japanese_sentence;
use lingrep_core::token_match::TokenInfo;
use lingrep_core::token_match::tokenize;
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_japanese_sentence_valid(s in japanese_sentence()) {
        // Should be valid UTF-8
        let _validated = String::from_utf8(s.as_bytes().to_vec()).unwrap();

        // Should be non-empty
        prop_assert!(!s.is_empty());
    }

    #[test]
    fn test_japanese_sentence_has_japanese(s in japanese_sentence()) {
        // Should contain Japanese characters
        let has_japanese = s.chars().any(|c: char| {
            // Check for Hiragana, Katakana, or Kanji ranges
            ('\u{3040}'..='\u{309F}').contains(&c) ||  // Hiragana
            ('\u{30A0}'..='\u{30FF}').contains(&c) ||  // Katakana
            ('\u{4E00}'..='\u{9FFF}').contains(&c)      // Kanji
        });
        prop_assert!(has_japanese);
    }
}

#[test]
fn test_japanese_sentence_templates() {
    // Test that all templates are valid Japanese
    let templates = vec![
        "これはテストです。",
        "日本語の文章をテストしています。",
        "形態素解析の確認です。",
        "文末表現の研究データです。",
        "科学論文からのサンプル文です。",
        "データセットの検証用テキストです。",
        "システムの動作確認をします。",
        "パフォーマンスの計測です。",
        "正解率を検証します。",
        "エッジケースのテストです。",
    ];

    for template in templates {
        // Verify each template tokenizes successfully
        let tokenizer = lingrep_core::testing::test_tokenizer();
        let tokens: Vec<TokenInfo> = tokenize(template, tokenizer);

        // Should have at least one token
        assert!(!tokens.is_empty(), "Template should tokenize: {template}");
    }
}
