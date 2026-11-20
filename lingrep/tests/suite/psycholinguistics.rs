// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use crate::support::{assert_success, fixture, lingrep_without_dictionary, parse_jsonl};
use serde_json::json;

#[test]
fn reading_predictors_preserve_word_types_span_occurrences_and_argument_witnesses() {
    let catalog =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../patterns/psycholinguistics.lx");
    let output = lingrep_without_dictionary()
        .args([
            "--json",
            "matches",
            "--evidence",
            "--input-format",
            "conllu",
            "--patterns",
        ])
        .arg(catalog)
        .arg(fixture("psycholinguistics.conllu"))
        .output()
        .unwrap();
    assert_success(&output);
    let rows = parse_jsonl(&output.stdout);
    let matches = |name: &str| {
        rows.iter()
            .filter(|row| row["type"] == "match" && row["data"]["pattern"] == name)
            .map(|row| &row["data"])
            .collect::<Vec<_>>()
    };
    let surfaces = |name: &str| {
        matches(name)
            .into_iter()
            .map(|row| row["surface"].as_str().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(surfaces("school_neighbor"), ["学生", "高校", "学生"]);
    assert_eq!(surfaces("katakana_token"), ["カレー"]);
    let inventory = matches("reading_bunsetsu");
    assert!(
        inventory
            .iter()
            .all(|unit| unit["corpus_sentence_ordinal"] != 4)
    );
    let first_sentence = inventory
        .iter()
        .filter(|unit| unit["corpus_sentence_ordinal"] == 0)
        .map(|unit| (unit["surface"].as_str().unwrap(), &unit["span_ids"]))
        .collect::<Vec<_>>();
    assert_eq!(
        first_sentence,
        [
            ("新聞を", &json!([0])),
            ("子どもが", &json!([1])),
            ("読む", &json!([2]))
        ]
    );
    let edges = matches("earlier_dependent")
        .into_iter()
        .filter(|edge| edge["corpus_sentence_ordinal"] == 0)
        .map(|edge| {
            edge["captures"]
                .as_array()
                .unwrap()
                .iter()
                .map(|cap| {
                    (
                        cap["name"].as_str().unwrap(),
                        cap["span"]["start"].as_u64().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        edges,
        [
            vec![("dependent", 0), ("governor", 4)],
            vec![("dependent", 2), ("governor", 4)]
        ]
    );
    for (name, sentence) in [("subject_gap_candidate", 1), ("object_gap_candidate", 2)] {
        let hits = matches(name);
        assert_eq!(
            hits.len(),
            1,
            "{name}: the fully saturated clause must be excluded"
        );
        let hit = hits[0];
        assert_eq!(hit["corpus_sentence_ordinal"], sentence);
        assert_eq!(hit["span"], json!({"start":2,"end":3}));
        let witnesses = hit["graph_bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|binding| {
                (
                    binding["name"].as_str().unwrap(),
                    binding["node"]["token_index"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(witnesses, [("argument", 0), ("head", 4), ("marker", 1)]);
    }
}
