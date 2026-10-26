use lingrep_core::testing::run_matching_test;

#[test]
fn test_run_matching_empty() {
    let stats = run_matching_test(&[], &[]);

    assert_eq!(stats.sentences_processed, 0);
    assert_eq!(stats.matches_found, 0);
    assert_eq!(stats.tokens_processed, 0);
}

#[test]
fn matching_helper_totals_sentences_tokens_and_matches() {
    let sentences = ["猫", "犬", "猫"];
    let stats = run_matching_test(&[("cat", "[text=猫]")], &sentences);
    assert_eq!(stats.sentences_processed, 3);
    assert_eq!(stats.tokens_processed, 3);
    assert_eq!(stats.matches_found, 2);
}
