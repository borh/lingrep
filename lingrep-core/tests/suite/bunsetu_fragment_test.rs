//! The bunsetu definitions in the CLI guide's spaCy adapter section, read from
//! the guide and run over GiNZA's `BunsetuBILabel` values for the sentence the
//! guide describes.

use lingrep_core::fsm::{Capture, CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, BoundTokenSentence, SchemaAttribute, TokenSchema,
};

const GUIDE: &str = include_str!("../../../docs/cli-usage-guide.md");

/// The fenced `pattern` block that defines `BUNSETU`.
fn guide_catalog() -> String {
    let (_, block) = GUIDE
        .split_once("```pattern\nfragment BUNSETU")
        .expect("the guide defines the BUNSETU fragment");
    let (body, _) = block
        .split_once("```")
        .expect("the BUNSETU block is closed");
    format!("fragment BUNSETU{body}")
}

fn matched(pattern: &str, start: usize, end: usize, captures: &[(&str, usize, usize)]) -> Match {
    Match {
        pattern: pattern.into(),
        start,
        end,
        captures: captures
            .iter()
            .map(|(name, start, end)| Capture {
                name: (*name).into(),
                path: format!("{pattern}/{name}").into(),
                start: *start,
                end: *end,
            })
            .collect(),
        graph_bindings: Box::default(),
        lexical: None,
    }
}

#[test]
fn the_guide_bunsetu_definitions_match_ginza_bunsetu_labels() {
    let catalog = compile_catalog(
        PatternSource::named("cli-usage-guide.md", &guide_catalog()),
        CompileLimits::default(),
    )
    .expect("the guide's bunsetu catalog compiles")
    .matcher;
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("spacy.ginza.bunsetu", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    let bound = catalog
        .bind(&schema)
        .expect("the schema supplies every key");

    let run = |sentence: &[(&str, &str, &str)]| {
        let keys: Vec<&str> = bound
            .projection()
            .keys()
            .iter()
            .map(AttributeKey::as_str)
            .collect();
        let values = sentence
            .iter()
            .flat_map(|(text, upos, bunsetu)| {
                keys.iter().map(move |key| {
                    Some(
                        match *key {
                            "text" => *text,
                            "upos" => *upos,
                            "spacy.ginza.bunsetu" => *bunsetu,
                            other => panic!("unexpected projected key {other}"),
                        }
                        .into(),
                    )
                })
            })
            .collect();
        let tokens =
            BoundTokenSentence::from_projected_values(bound.projection(), sentence.len(), values)
                .expect("the values follow the projection");
        bound.find_matches(&tokens).expect("schemas agree")
    };

    // 猫が|庭で|寝ている。 as `ginza -f conllu` labels it.
    let matches = run(&[
        ("猫", "NOUN", "B"),
        ("が", "ADP", "I"),
        ("庭", "NOUN", "B"),
        ("で", "ADP", "I"),
        ("寝", "VERB", "B"),
        ("て", "SCONJ", "I"),
        ("いる", "AUX", "I"),
        ("。", "PUNCT", "I"),
    ]);

    assert_eq!(
        matches,
        vec![
            matched("bunsetu", 0, 2, &[]),
            matched("ga_bunsetu", 0, 2, &[]),
            matched("bunsetu", 2, 4, &[]),
            matched("bunsetu", 4, 8, &[]),
            matched("verb_bunsetu", 4, 8, &[("head", 4, 5)]),
        ]
    );

    // GiNZA keeps the comma in the bunsetu, so 猫が、 does not end in が.
    let matches = run(&[
        ("猫", "NOUN", "B"),
        ("が", "ADP", "I"),
        ("、", "PUNCT", "I"),
        ("寝る", "VERB", "B"),
    ]);
    assert_eq!(
        matches,
        vec![
            matched("bunsetu", 0, 3, &[]),
            matched("bunsetu", 3, 4, &[]),
            matched("verb_bunsetu", 3, 4, &[("head", 3, 4)]),
        ]
    );
}
