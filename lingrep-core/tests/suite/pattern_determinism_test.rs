// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use compact_str::CompactString;
use indexmap::IndexMap;

#[test]
fn test_indexmap_insertion_order_deterministic() {
    // Test that IndexMap preserves insertion order consistently
    let mut map1 = IndexMap::new();
    map1.insert("first".to_string(), 1);
    map1.insert("second".to_string(), 2);
    map1.insert("third".to_string(), 3);

    let mut map2 = IndexMap::new();
    map2.insert("first".to_string(), 1);
    map2.insert("second".to_string(), 2);
    map2.insert("third".to_string(), 3);

    let keys1: Vec<_> = map1.keys().cloned().collect();
    let keys2: Vec<_> = map2.keys().cloned().collect();

    assert_eq!(keys1, vec!["first", "second", "third"]);
    assert_eq!(keys1, keys2);
}

#[test]
fn test_multiple_indexmap_iterations_consistent() {
    // Test that multiple iterations of the same IndexMap produce the same order
    let mut map = IndexMap::new();
    map.insert("TEST1".to_string(), 1);
    map.insert("TEST2".to_string(), 2);
    map.insert("TEST3".to_string(), 3);

    let keys1: Vec<_> = map.keys().cloned().collect();
    let keys2: Vec<_> = map.keys().cloned().collect();
    let keys3: Vec<_> = map.keys().cloned().collect();

    assert_eq!(keys1, keys2);
    assert_eq!(keys2, keys3);
    assert_eq!(keys1, vec!["TEST1", "TEST2", "TEST3"]);
}

#[test]
fn test_compact_string_indexmap_ordering() {
    // Use the same key type as the compiled pattern catalog.
    let mut map = IndexMap::new();
    map.insert(CompactString::new("KAKO"), 1);
    map.insert(CompactString::new("NAI"), 2);
    map.insert(CompactString::new("形容動詞"), 3);

    let keys: Vec<_> = map.keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            CompactString::new("KAKO"),
            CompactString::new("NAI"),
            CompactString::new("形容動詞")
        ]
    );
}

#[test]
fn test_compiled_pattern_order_is_deterministic() {
    let results_1 = build_matcher_and_get_patterns();
    let results_2 = build_matcher_and_get_patterns();
    let results_3 = build_matcher_and_get_patterns();

    assert_eq!(
        results_1, results_2,
        "first and second compilations should preserve pattern order"
    );
    assert_eq!(
        results_2, results_3,
        "second and third compilations should preserve pattern order"
    );
    assert_eq!(
        results_1, results_3,
        "first and third compilations should preserve pattern order"
    );

    let expected_patterns = vec![
        "TEST1".to_string(),
        "TEST2".to_string(),
        "TEST3".to_string(),
    ];
    assert_eq!(
        results_1, expected_patterns,
        "Pattern order should match insertion order"
    );
}

/// Builds a matcher and returns pattern names in authored order.
fn build_matcher_and_get_patterns() -> Vec<String> {
    let patterns = [
        ("TEST1", "TEST1 := a b c."),
        ("TEST2", "TEST2 := x y z."),
        ("TEST3", "TEST3 := d e f."),
    ];

    let source = patterns
        .iter()
        .map(|(_, pattern_text)| *pattern_text)
        .collect::<Vec<_>>()
        .join("\n");
    let matcher = lingrep_core::fsm::compile_catalog(
        lingrep_core::fsm::PatternSource::named("determinism.lx", &source),
        lingrep_core::fsm::CompileLimits::default(),
    )
    .unwrap()
    .matcher;

    let pattern_names: Vec<String> = matcher
        .patterns()
        .keys()
        .map(|k: &CompactString| k.to_string())
        .collect();

    pattern_names
}
