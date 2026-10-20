//! Test corpus helpers
//!
//! Provides utilities for generating and managing test corpora data.

use rand::Rng;
use serde::{Deserialize, Serialize};

/// Test sentence with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSentence {
    pub title: String,
    pub text: String,
    pub expected_tokens: usize,
}

/// Get predefined test sentences
///
/// Returns a collection of test sentences with metadata for use in tests.
/// Each sentence includes expected token count for validation.
pub fn get_test_sentences() -> Vec<TestSentence> {
    vec![
        TestSentence {
            title: "Simple sentence".to_string(),
            text: "これはテストです。".to_string(),
            expected_tokens: 4,
        },
        TestSentence {
            title: "Scientific text".to_string(),
            text: "実験結果は有意差が見られました。".to_string(),
            expected_tokens: 8,
        },
        TestSentence {
            title: "Mixed script".to_string(),
            text: "CPUの温度は80°Cです。".to_string(),
            expected_tokens: 9,
        },
        TestSentence {
            title: "With punctuation".to_string(),
            text: "テスト、です。「これ」は引用？".to_string(),
            expected_tokens: 7,
        },
    ]
}

/// Create test corpus with N sentences
///
/// Generates a corpus of test sentences with incrementing numbers.
/// Useful for creating larger test datasets for performance testing.
///
/// # Arguments
/// * `count` - Number of sentences to generate
///
/// # Returns
/// Vector of generated test sentences
///
/// # Example
/// ```rust
/// let corpus = create_test_corpus(3);
/// assert_eq!(corpus.len(), 3);
/// assert!(corpus[0].contains("テスト文章0"));
/// ```
pub fn create_test_corpus(count: usize) -> Vec<String> {
    let mut rng = rand::rng();
    let templates = [
        "これはテスト文章{}です。",
        "形態素解析のテストデータ{}。",
        "日本語の文章{}を生成します。",
        "これは{}番目の文です。",
        "テストデータ{}を作成中。",
    ];

    (0..count)
        .map(|i| {
            let template = templates[rng.random_range(0..templates.len())];
            template.replace("{}", &i.to_string())
        })
        .collect()
}

/// Create test corpus with custom pattern
///
/// Generates sentences using a custom template pattern.
/// The pattern should contain "{}" as a placeholder for the sentence number.
///
/// # Arguments
/// * `count` - Number of sentences to generate
/// * `pattern` - Template pattern with "{}" placeholder
///
/// # Example
/// ```rust
/// let corpus = create_test_corpus_with_pattern(3, "文番号{}");
/// assert_eq!(corpus[0], "文番号0");
/// ```
pub fn create_test_corpus_with_pattern(count: usize, pattern: &str) -> Vec<String> {
    (0..count)
        .map(|i| pattern.replace("{}", &i.to_string()))
        .collect()
}

/// Generate random Japanese-style text
///
/// Creates pseudo-random Japanese text by combining common words and patterns.
/// Useful for testing with more varied input.
///
/// # Arguments
/// * `count` - Number of sentences to generate
/// * `min_words` - Minimum words per sentence
/// * `max_words` - Maximum words per sentence
pub fn generate_random_japanese_text(
    count: usize,
    min_words: usize,
    max_words: usize,
) -> Vec<String> {
    let mut rng = rand::rng();

    let words = vec![
        "これ",
        "それ",
        "あれ",
        "この",
        "その",
        "あの",
        "テスト",
        "データ",
        "解析",
        "結果",
        "実験",
        "は",
        "が",
        "を",
        "に",
        "で",
        "と",
        "です",
        "である",
        "した",
        "なる",
    ];

    (0..count)
        .map(|_| {
            let word_count = rng.random_range(min_words..=max_words);
            let sentence: Vec<&str> = (0..word_count)
                .map(|_| words[rng.random_range(0..words.len())])
                .collect();
            sentence.join("") + "。"
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_test_sentences() {
        let sentences = get_test_sentences();

        assert_eq!(sentences.len(), 4);
        assert_eq!(sentences[0].title, "Simple sentence");
        assert_eq!(sentences[0].text, "これはテストです。");
        assert_eq!(sentences[0].expected_tokens, 4);
    }

    #[test]
    fn test_create_test_corpus() {
        let corpus = create_test_corpus(5);

        assert_eq!(corpus.len(), 5);
        // Each sentence should end with a period
        assert!(corpus.iter().all(|s| s.ends_with('。')));
        // Should contain incrementing numbers
        assert!(corpus[0].contains('0'));
        assert!(corpus[4].contains('4'));
    }

    #[test]
    fn test_create_test_corpus_empty() {
        let corpus = create_test_corpus(0);
        assert_eq!(corpus.len(), 0);
    }

    #[test]
    fn test_create_test_corpus_with_pattern() {
        let corpus = create_test_corpus_with_pattern(3, "文番号{}終了");

        assert_eq!(corpus.len(), 3);
        assert_eq!(corpus[0], "文番号0終了");
        assert_eq!(corpus[1], "文番号1終了");
        assert_eq!(corpus[2], "文番号2終了");
    }

    #[test]
    fn test_create_test_corpus_with_pattern_no_placeholder() {
        let corpus = create_test_corpus_with_pattern(2, "固定パターン");

        assert_eq!(corpus.len(), 2);
        assert_eq!(corpus[0], "固定パターン");
        assert_eq!(corpus[1], "固定パターン");
    }

    #[test]
    fn test_generate_random_japanese_text() {
        let text = generate_random_japanese_text(5, 3, 5);

        assert_eq!(text.len(), 5);
        // Each sentence should end with a period
        assert!(text.iter().all(|s| s.ends_with('。')));
        // Sentences should not be empty
        assert!(text.iter().all(|s| s.len() > 1));
    }

    #[test]
    fn test_generate_random_japanese_text_empty() {
        let text = generate_random_japanese_text(0, 3, 5);
        assert_eq!(text.len(), 0);
    }

    #[test]
    fn test_generate_random_japanese_text_single_word() {
        let text = generate_random_japanese_text(1, 1, 1);

        assert_eq!(text.len(), 1);
        assert!(text[0].ends_with('。'));
        assert!(text[0].len() > 1); // At least one word + period
    }
}
