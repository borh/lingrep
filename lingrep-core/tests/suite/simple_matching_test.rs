use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::TokenInfo;

#[hegel::test(test_cases = 100, derandomize = true)]
fn scalar_predicates_distinguish_absent_empty_equal_and_unequal_values(tc: hegel::TestCase) {
    let values = [
        None,
        Some(""),
        Some("猫"),
        Some("NOUN"),
        Some("猫🐈"),
        Some("e\u{301}"),
    ];
    let symbols = tc.draw(hegel::generators::binary().max_size(64));
    let attributes = symbols
        .iter()
        .map(|symbol| {
            let symbol = usize::from(*symbol);
            [
                values[symbol % 6],
                values[(symbol / 6) % 6],
                values[(symbol / 36) % 6],
            ]
        })
        .collect::<Vec<_>>();
    let tokens = attributes
        .iter()
        .map(|attributes| {
            let mut token = TokenInfo::default();
            if let Some(value) = attributes[0] {
                token = token.with_orth(value);
            }
            if let Some(value) = attributes[1] {
                token = token.with_lemma(value);
            }
            if let Some(value) = attributes[2] {
                token = token.with_pos(value);
            }
            token
        })
        .collect::<Vec<_>>();

    for (column, key, literal) in [(0, "text", "猫"), (1, "lemma", "猫"), (2, "upos", "NOUN")] {
        let source = format!(
            "equal := [{key}={literal}]. different := [{key}!={literal}]. complement := [!({key}={literal})]."
        );
        let matcher = compile_catalog(
            PatternSource::named("scalar.lx", &source),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let mut expected = Vec::new();
        for (start, attributes) in attributes.iter().enumerate() {
            let value = attributes[column];
            for (pattern, accepts) in [
                ("complement", value != Some(literal)),
                ("different", value.is_some_and(|value| value != literal)),
                ("equal", value == Some(literal)),
            ] {
                if accepts {
                    expected.push(Match {
                        pattern: pattern.into(),
                        start,
                        end: start + 1,
                        captures: Box::default(),
                        graph_bindings: Box::default(),
                        lexical: None,
                    });
                }
            }
        }
        assert_eq!(matcher.find_matches(&tokens), expected, "{key}");
    }
}
