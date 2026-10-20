//! Test fixture integration tests
//!
//! Verifies that the fixture system works correctly and can be used by other tests.

use crate::fixtures::TEST_CORPORA;
use crate::fixtures::corpora::*;

#[test]
fn test_fixture_corpora_accessible() {
    let corpora = &*TEST_CORPORA;

    // Verify all corpora are accessible
    assert!(!corpora.simple_sentence.is_empty());
    assert!(!corpora.scientific_text.is_empty());
    assert!(!corpora.mixed_script.is_empty());
    assert!(!corpora.sentence_with_punctuation.is_empty());
    assert!(!corpora.multi_sentence_text.is_empty());
}

#[test]
fn test_fixture_all_texts() {
    let corpora = &*TEST_CORPORA;
    let all_texts = corpora.all_texts();

    // Should return all texts
    assert_eq!(all_texts.len(), 5);

    // All should be non-empty
    assert!(all_texts.iter().all(|text| !text.is_empty()));
}

#[test]
fn test_corpora_helpers() {
    let sentences = get_test_sentences();

    // Should return predefined test sentences
    assert_eq!(sentences.len(), 4);
    assert_eq!(sentences[0].title, "Simple sentence");
    assert_eq!(sentences[0].text, "これはテストです。");
    assert_eq!(sentences[0].expected_tokens, 4);
}

#[test]
fn test_create_test_corpus() {
    let corpus = create_test_corpus(5);

    // Should create 5 sentences
    assert_eq!(corpus.len(), 5);

    // All should end with period
    assert!(corpus.iter().all(|s: &String| s.ends_with('。')));

    // Should contain incrementing numbers
    assert!(corpus[0].contains('0'));
    assert!(corpus[4].contains('4'));
}

#[test]
fn test_create_test_corpus_with_pattern() {
    let corpus = create_test_corpus_with_pattern(3, "テスト{}文");

    assert_eq!(corpus.len(), 3);
    assert_eq!(corpus[0], "テスト0文");
    assert_eq!(corpus[1], "テスト1文");
    assert_eq!(corpus[2], "テスト2文");
}

#[test]
fn test_generate_random_japanese_text() {
    let text = generate_random_japanese_text(5, 3, 5);

    assert_eq!(text.len(), 5);
    assert!(text.iter().all(|s: &String| s.ends_with('。')));
    assert!(text.iter().all(|s: &String| s.len() > 1));
}
