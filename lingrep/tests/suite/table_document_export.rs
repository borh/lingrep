#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::support::{
    assert_success, lingrep, lingrep_std, lingrep_without_dictionary, parse_jsonl,
};
use lingrep_core::testing::MiniDictionary;

struct Fixture {
    _directory: tempfile::TempDir,
    artifact: PathBuf,
    catalog: PathBuf,
    table: PathBuf,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("documents.jsonl");
    fs::write(&input, concat!(
        "{\"text\":\"猫が猫を見る。\",\"metadata\":{\"tags\":[\"x\",\"y\"],\"year\":2001,\"checked\":true}}\n",
        "{\"text\":\"\",\"metadata\":{\"tags\":[\"x\"],\"year\":2001}}\n",
        "{\"text\":\"犬。\",\"metadata\":{\"tags\":[\"x\"],\"year\":2001}}\n",
        "{\"text\":\"猫。\",\"metadata\":{\"tags\":[\"y\"],\"year\":2002}}\n",
    )).unwrap();
    let dictionary = MiniDictionary::new();
    let artifact = directory.path().join("documents.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert_success(&built);
    let catalog = directory.path().join("counts.lx");
    fs::write(&catalog, "cat := [text=猫].\nkey: [term, segments(text)]\nlexicon words by [text] := {\"猫\", \"猫が猫\"}.\n").unwrap();
    let table = directory.path().join("counts.lingtable");
    let built = lingrep_without_dictionary()
        .args(["--json", "summarize", "--patterns"])
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .arg("--write-table")
        .arg(&table)
        .output()
        .unwrap();
    assert_success(&built);
    Fixture {
        _directory: directory,
        artifact,
        catalog,
        table,
    }
}

fn export(f: &Fixture, flags: &[&str]) -> std::process::Output {
    lingrep_without_dictionary()
        .args(["--json", "table"])
        .arg(&f.table)
        .arg("--corpus")
        .arg(&f.artifact)
        .arg("--patterns")
        .arg(&f.catalog)
        .arg("--by-document")
        .args(flags)
        .output()
        .unwrap()
}

#[test]
fn sparse_document_export_reconstructs_scoped_summaries_with_typed_keys_and_zero_documents() {
    let f = fixture();
    let contract = lingrep::protocol::registry::get(lingrep::protocol::ContractId::Table)
        .to_detail()
        .unwrap();
    let validator = jsonschema::validator_for(contract.json_schema.as_ref().unwrap()).unwrap();
    let mut full_dictionary = None;
    for clause in [
        None,
        Some("@document.year = 2001"),
        Some("@document.tags contains x"),
        Some("@document.year = 1999"),
    ] {
        let mut flags = vec![
            "--source-metadata",
            "@document.tags",
            "--source-metadata",
            "@document.year",
            "--source-metadata",
            "@document.checked",
        ];
        if let Some(clause) = clause {
            flags.extend(["--where", clause]);
        }
        let mut export_flags = flags.clone();
        export_flags.extend(["--max-temp-bytes", "1"]);
        let output = export(&f, &export_flags);
        assert_success(&output);
        let records = parse_jsonl(&output.stdout);
        assert_eq!(records.first().unwrap()["type"], "run");
        assert_eq!(records[0]["data"]["by_document"], true);
        assert!(records[0]["data"]["source_identity"].as_str().is_some());
        assert_eq!(records.last().unwrap()["type"], "complete");
        let dictionary = records
            .iter()
            .filter(|r| r["type"] == "entry")
            .map(|r| (r["data"]["entry"].as_u64().unwrap(), r["data"].clone()))
            .collect::<BTreeMap<_, _>>();
        if let Some(expected) = &full_dictionary {
            assert_eq!(&dictionary, expected);
        } else {
            full_dictionary = Some(dictionary.clone());
        }
        assert!(dictionary.values().any(|e| {
            e["group"]["fields"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|field| field["kind"] == "list"))
        }));
        let documents = records
            .iter()
            .filter(|r| r["type"] == "document")
            .map(|r| {
                (
                    r["data"]["document_id"].as_u64().unwrap(),
                    r["data"].clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let reconstructed = records
            .iter()
            .filter(|r| r["type"] == "cell")
            .map(|r| {
                let c = &r["data"];
                let entry = &dictionary[&c["entry"].as_u64().unwrap()];
                let document = &documents[&c["document_id"].as_u64().unwrap()];
                (
                    (
                        document["document"].as_u64().unwrap(),
                        entry["pattern"].to_string(),
                        entry["group"].to_string(),
                    ),
                    c["count"].as_u64().unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let summary = lingrep_without_dictionary()
            .args(["--json", "summarize", "--by-document", "--top", "all"])
            .arg("--corpus")
            .arg(&f.artifact)
            .arg("--patterns")
            .arg(&f.catalog)
            .args(&flags)
            .output()
            .unwrap();
        assert_success(&summary);
        let expected = parse_jsonl(&summary.stdout);
        let counts = expected
            .iter()
            .filter(|r| r["type"] == "summary")
            .map(|r| {
                let d = &r["data"];
                (
                    (
                        d["document"].as_u64().unwrap(),
                        d["pattern"].to_string(),
                        d["group"].to_string(),
                    ),
                    d["count"].as_u64().unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(reconstructed, counts, "{clause:?}");
        let expected_docs = expected
            .iter()
            .filter(|r| r["type"] == "document")
            .map(|r| (r["data"]["document"].as_u64().unwrap(), &r["data"]))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(documents.len(), expected_docs.len());
        for doc in documents.values() {
            let expected = expected_docs[&doc["document"].as_u64().unwrap()];
            for key in [
                "source_name",
                "sentences_admitted",
                "tokens_admitted",
                "source_metadata",
            ] {
                assert_eq!(doc[key], expected[key], "{clause:?} {key}");
            }
        }
        let complete = &records.last().unwrap()["data"];
        let summary_complete = &expected.last().unwrap()["data"];
        assert_eq!(
            complete["entries"].as_u64().unwrap(),
            dictionary.len() as u64
        );
        assert_eq!(
            complete["rows"].as_u64().unwrap(),
            reconstructed.len() as u64
        );
        for key in ["documents_admitted", "sentences_admitted"] {
            assert_eq!(
                complete[key], summary_complete["counters"][key],
                "{clause:?} {key}"
            );
        }
        assert_eq!(
            complete["tokens_admitted"].as_u64().unwrap(),
            expected_docs
                .values()
                .map(|d| d["tokens_admitted"].as_u64().unwrap())
                .sum::<u64>()
        );
        if clause.is_none() {
            assert!(documents.values().any(|d| {
                !reconstructed
                    .keys()
                    .any(|(ordinal, _, _)| *ordinal == d["document"].as_u64().unwrap())
            }));
        }
        for record in records {
            assert!(validator.is_valid(&record), "{record}");
            serde_json::from_value::<lingrep::protocol::table::TableEnvelope>(record).unwrap();
        }
    }
}

#[test]
fn sparse_document_export_rejects_truncation_wrong_metadata_owners_and_human_output() {
    let f = fixture();
    for flags in [
        vec!["--top", "1"],
        vec!["--top", "all"],
        vec!["--select", "cat"],
        vec!["--key", "text=猫"],
        vec!["--entry", "cat=猫", "--by-metadata", "@document.tags"],
        vec!["--source-metadata", "@sentence.year"],
    ] {
        let result = export(&f, &flags);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{flags:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
    }
    let result = lingrep_without_dictionary()
        .arg("table")
        .arg(&f.table)
        .arg("--corpus")
        .arg(&f.artifact)
        .arg("--patterns")
        .arg(&f.catalog)
        .arg("--by-document")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires --json"));
    assert!(result.stdout.is_empty());
}

#[test]
fn sparse_document_export_rejects_mismatched_catalogs_and_corrupt_tables_before_output() {
    let f = fixture();
    let original = fs::read(&f.catalog).unwrap();
    fs::write(&f.catalog, "dog := [text=犬].\n").unwrap();
    let result = export(&f, &[]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    fs::write(&f.catalog, original).unwrap();
    let path = f.table.join("pair_counts.u32");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();
    let result = export(&f, &[]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}

#[test]
fn sparse_document_export_reports_a_closed_pipe_as_failure() {
    use std::process::Stdio;
    let f = fixture();
    let mut child = lingrep_std()
        .args(["--json", "table"])
        .arg(&f.table)
        .arg("--corpus")
        .arg(&f.artifact)
        .arg("--patterns")
        .arg(&f.catalog)
        .arg("--by-document")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(!result.stderr.is_empty());
}
