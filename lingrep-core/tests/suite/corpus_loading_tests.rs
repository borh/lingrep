//! Tests for corpus loading functionality

// Integration helpers are exposed only by the testing feature.
#![cfg(feature = "testing")]

use lingrep_core::testing::AnnotatedSentence;

#[test]
fn test_load_synthetic_corpus() {
    let corpus = AnnotatedSentence::load_from_file("tests/corpus/synthetic.jsonl")
        .expect("Failed to load corpus");

    assert_eq!(corpus.len(), 3, "Should have 3 test sentences");
    assert_eq!(corpus[0].text, "猫");
    assert_eq!(corpus[0].expected_matches.len(), 1);
}
