#![allow(clippy::unwrap_used)]
use crate::support::lingrep_without_dictionary;

#[test]
fn ending_report_includes_unmatched_sentences_and_uses_published_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let input = root.join("input.conllu");
    let sentences = [
        vec![
            "こと", "も", "ある", "こと", "は", "否定", "でき", "ない", "。",
        ],
        vec!["こと", "。"],
        vec!["未知", "。"],
        vec!["。"],
    ];
    let mut conllu = String::new();
    for words in sentences {
        for (index, word) in words.into_iter().enumerate() {
            let pos = if word == "。" { "PUNCT" } else { "NOUN" };
            conllu.push_str(&format!(
                "{}\t{word}\t{word}\t{pos}\t_\t_\t0\troot\t_\t_\n",
                index + 1
            ));
        }
        conllu.push('\n');
    }
    std::fs::write(&input, conllu).unwrap();
    let catalog = root.join("query.lx");
    std::fs::write(
        &catalog,
        "koto := [text=こと].\nlong := [text=こと] [text=も] [text=ある] [text=こと].\n",
    )
    .unwrap();
    let report = root.join("endings.json");
    let output = lingrep_without_dictionary()
        .args(["matches", "--json", "--patterns"])
        .arg(&catalog)
        .arg("--ending-report")
        .arg(&report)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = std::fs::read(&report).unwrap();
    let data: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(data["sentences"], 4);
    assert_eq!(data["documents"], 1);
    assert_eq!(data["unexplained_sentences"], 2);
    let groups = data["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    let partial = groups
        .iter()
        .find(|g| g["kind"] == "trailing_after_match")
        .unwrap();
    assert_eq!(
        partial["tokens"],
        serde_json::json!(["は", "否定", "でき", "ない"])
    );
    assert_eq!(partial["occurrences"], 1);
    assert_eq!(partial["documents"], 1);
    assert_eq!(
        partial["examples"][0]["span"],
        serde_json::json!({"start":4,"end":8})
    );
    assert_eq!(partial["examples"][0]["tokens"][8], "。");
    assert_eq!(
        partial["examples"][0]["preceding"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        groups.iter().any(
            |g| g["kind"] == "no_published_match" && g["tokens"] == serde_json::json!(["未知"])
        )
    );

    let artifact = root.join("prepared.lingcorpus");
    let built = lingrep_without_dictionary()
        .args(["corpus", "build", "--output"])
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let prepared_report = root.join("prepared.json");
    let prepared = lingrep_without_dictionary()
        .args(["matches", "--json", "--patterns"])
        .arg(&catalog)
        .arg("--ending-report")
        .arg(&prepared_report)
        .arg("--corpus")
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let prepared: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&prepared_report).unwrap()).unwrap();
    assert_eq!(prepared["groups"], data["groups"]);
    assert_eq!(prepared["sentences"], data["sentences"]);

    std::fs::write(&catalog, "koto := [text=こと].\npolicy\n  admit all terminal: {exact: true, anchor: sentence_end}.\nend\n").unwrap();
    let suppressed_report = root.join("suppressed.json");
    let suppressed = lingrep_without_dictionary()
        .args(["matches", "--json", "--patterns"])
        .arg(&catalog)
        .arg("--ending-report")
        .arg(&suppressed_report)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        suppressed.status.success(),
        "{}",
        String::from_utf8_lossy(&suppressed.stderr)
    );
    let suppressed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&suppressed_report).unwrap()).unwrap();
    assert_eq!(suppressed["unexplained_sentences"], 3);
    assert!(
        suppressed["groups"]
            .as_array()
            .unwrap()
            .iter()
            .all(|group| group["kind"] == "no_published_match")
    );
    assert!(
        suppressed["basis"]
            .as_str()
            .unwrap()
            .contains("policy-suppressed")
    );
    let failed_report = root.join("failed.json");
    let invalid = root.join("invalid.conllu");
    std::fs::write(&invalid, "not a CoNLL-U record\n").unwrap();
    let failed = lingrep_without_dictionary()
        .args(["matches", "--json", "--patterns"])
        .arg(&catalog)
        .arg("--ending-report")
        .arg(&failed_report)
        .arg(&invalid)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(!failed_report.exists());
    let again = lingrep_without_dictionary()
        .args(["matches", "--json", "--patterns"])
        .arg(&catalog)
        .arg("--ending-report")
        .arg(&report)
        .arg(&input)
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert_eq!(std::fs::read(&report).unwrap(), original);
}
