use lingrep_core::fsm::Match;
use lingrep_core::testing::{compile_test_pattern, tokenize_japanese};

#[test]
fn quantifiers_match_their_token_count_boundaries() {
    let cases = [
        (
            "?",
            &[
                ("optional particle absent", "猫", Some(1)),
                ("optional particle present", "猫が", Some(2)),
            ][..],
        ),
        (
            "*",
            &[
                ("zero repetitions", "猫", Some(1)),
                ("multiple repetitions", "猫がは", Some(3)),
            ],
        ),
        (
            "+",
            &[
                ("one required repetition", "猫が", Some(2)),
                ("required repetition absent", "猫", None),
            ],
        ),
        ("{2}", &[("exact count", "猫がは", Some(3))]),
        ("{1,3}", &[("upper bound", "猫がはを", Some(4))]),
        ("{2,4}", &[("below lower bound", "猫が", None)]),
    ];

    for (quantifier, rows) in cases {
        let matcher = compile_test_pattern(&format!("[upos=NOUN] [upos=ADP]{quantifier}"));
        for (label, text, end) in rows {
            let expected: Vec<_> = end
                .map(|end| Match {
                    pattern: "test".into(),
                    start: 0,
                    end,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    interpretations: Box::default(),
                    lexical: None,
                })
                .into_iter()
                .collect();
            assert_eq!(
                matcher.find_matches(&tokenize_japanese(text)),
                expected,
                "{quantifier} {label}"
            );
        }
    }
}
