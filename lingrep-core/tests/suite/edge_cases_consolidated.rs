use lingrep_core::testing::{compile_test_pattern, test_tokenizer, tokenize};

#[test]
fn repeated_text_matches_every_occurrence_in_order() {
    let tokens = tokenize(&"テスト ".repeat(5000), test_tokenizer());
    let matcher = compile_test_pattern("[text=テスト]");
    let expected = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.orth() == Some("テスト"))
        .map(|(start, _)| lingrep_core::fsm::Match {
            pattern: "test".into(),
            start,
            end: start + 1,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        })
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 5000);
    assert_eq!(matcher.find_matches(&tokens), expected);
}

#[test]
fn tokenization_preserves_written_forms_and_omits_whitespace() {
    let cases: &[(&str, &[&str])] = &[
        ("", &[]),
        ("\n", &[]),
        (" ", &[]),
        ("\t", &[]),
        ("     ", &[]),
        ("\t\t\t", &[]),
        (" \t \n ", &[]),
        ("\x00\x01\x02", &["\x00\x01\x02"]),
        ("あ", &["あ"]),
        ("。", &["。"]),
        ("、", &["、"]),
        ("🚀", &["🚀"]),
        ("😊", &["😊"]),
        (
            "🚀は速いです。😊",
            &["🚀", "は", "速い", "です", "。", "😊"],
        ),
    ];
    for (text, expected) in cases {
        let tokens = tokenize(text, test_tokenizer());
        let written = tokens
            .iter()
            .map(|token| token.orth().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&written, expected, "{text:?}");
    }
}

#[test]
fn trailing_line_endings_preserve_matches() {
    let matcher = compile_test_pattern("[text=テスト]");
    let expected = vec![lingrep_core::fsm::Match {
        pattern: "test".into(),
        start: 0,
        end: 1,
        captures: Box::default(),
        graph_bindings: Box::default(),
        lexical: None,
    }];
    for ending in ["", "\n", "\r\n", "\r"] {
        let tokens = tokenize(&format!("テスト{ending}"), test_tokenizer());
        assert_eq!(matcher.find_matches(&tokens), expected, "{ending:?}");
    }
}
