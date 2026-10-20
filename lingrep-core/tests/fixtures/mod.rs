//! Test fixtures and test data
//!
//! This module provides shared test corpora and test data helpers to reduce
//! duplication across integration tests.

pub mod corpora;

use std::sync::LazyLock;

/// Shared test corpora collection
///
/// This static collection provides commonly-used test sentences and corpora
/// for integration tests. It's lazily initialized to avoid overhead when not used.
pub static TEST_CORPORA: LazyLock<TestCorpora> = LazyLock::new(TestCorpora::new);

/// Test corpora collection
///
/// Contains various Japanese text samples for testing:
/// - Simple sentences
/// - Scientific texts
/// - Mixed script (kanji, katakana, alphabet, numbers)
/// - Longer form texts
#[derive(Debug, Clone)]
pub struct TestCorpora {
    pub simple_sentence: String,
    pub scientific_text: String,
    pub mixed_script: String,
    pub sentence_with_punctuation: String,
    pub multi_sentence_text: String,
}

impl TestCorpora {
    fn new() -> Self {
        Self {
            simple_sentence: "これはテストです。".to_string(),
            scientific_text: "実験結果は有意差が見られました(p<0.05)。".to_string(),
            mixed_script: "CPUの温度は80°Cです。RAMは16GBあります。".to_string(),
            sentence_with_punctuation: "テスト、です。「これ」は引用？".to_string(),
            multi_sentence_text: "これは1つ目の文です。これは2つ目です。最後の文です。".to_string(),
        }
    }

    /// Get all test texts as a vector
    pub fn all_texts(&self) -> Vec<&str> {
        vec![
            self.simple_sentence.as_str(),
            self.scientific_text.as_str(),
            self.mixed_script.as_str(),
            self.sentence_with_punctuation.as_str(),
            self.multi_sentence_text.as_str(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_corpora_initialization() {
        let corpora = &*TEST_CORPORA;

        assert!(!corpora.simple_sentence.is_empty());
        assert!(!corpora.scientific_text.is_empty());
        assert!(!corpora.mixed_script.is_empty());
        assert!(!corpora.sentence_with_punctuation.is_empty());
        assert!(!corpora.multi_sentence_text.is_empty());
    }

    #[test]
    fn test_all_texts_returns_all_corpora() {
        let corpora = &*TEST_CORPORA;
        let all_texts = corpora.all_texts();

        assert_eq!(all_texts.len(), 5);
        assert!(all_texts.iter().all(|text| !text.is_empty()));
    }

    #[test]
    fn test_mixed_script_contains_various_scripts() {
        let corpora = &*TEST_CORPORA;

        // Should contain katakana, kanji, alphabet, and numbers
        assert!(corpora.mixed_script.contains('C')); // Alphabet
        assert!(corpora.mixed_script.contains('°')); // Symbol
        assert!(corpora.mixed_script.contains('P')); // Alphabet
        assert!(corpora.mixed_script.contains('R')); // Alphabet
        assert!(corpora.mixed_script.contains('A')); // Alphabet
        assert!(corpora.mixed_script.contains('M')); // Alphabet
    }
}
