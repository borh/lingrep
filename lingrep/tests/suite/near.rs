// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::support::{lingrep_without_dictionary, parse_jsonl};

/// Cosines to `walk`: stroll 0.8, run 0.6, cook 0.
const VECTORS: &str = "4 2\nwalk 1 0\nstroll 0.8 0.6\nrun 0.6 0.8\ncook 0 1\n";

const CONLLU: &str = concat!(
    "# sent_id = one\n# text = walk stroll run cook\n",
    "1\twalk\twalk\tVERB\t_\t_\t0\troot\t_\t_\n",
    "2\tstroll\tstroll\tVERB\t_\t_\t1\tconj\t_\t_\n",
    "3\trun\trun\tVERB\t_\t_\t1\tconj\t_\t_\n",
    "4\tcook\tcook\tVERB\t_\t_\t1\tconj\t_\t_\n\n",
);

struct Fixture {
    _directory: tempfile::TempDir,
    vectors: PathBuf,
    corpus: PathBuf,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let vectors = directory.path().join("walking.vec");
    let corpus = directory.path().join("one.conllu");
    fs::write(&vectors, VECTORS).unwrap();
    fs::write(&corpus, CONLLU).unwrap();
    Fixture {
        _directory: directory,
        vectors,
        corpus,
    }
}

fn matches(pattern: &str, corpus: &Path, vectors: Option<&Path>, env: Option<&Path>) -> Output {
    let mut command = lingrep_without_dictionary();
    command.env_remove("LINGREP_VECTORS_PATH");
    if let Some(path) = env {
        command.env("LINGREP_VECTORS_PATH", path);
    }
    command.args([
        "--json",
        "matches",
        "--input-format",
        "conllu",
        "--pattern",
        pattern,
    ]);
    if let Some(path) = vectors {
        command.arg("--vectors").arg(path);
    }
    command.arg(corpus).output().unwrap()
}

fn matched_lemmas(output: &Output) -> Vec<String> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .iter()
        .filter(|event| event["type"] == "match")
        .map(|event| event["data"]["surface"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_near_test_matches_the_values_the_vector_file_puts_at_or_above_the_threshold() {
    let fixture = fixture();
    let run = |pattern: &str| {
        matched_lemmas(&matches(
            pattern,
            &fixture.corpus,
            Some(&fixture.vectors),
            None,
        ))
    };
    assert_eq!(run("[lemma near walk >= 0.7]"), ["walk", "stroll"]);
    assert_eq!(run("[lemma near walk >= 0.6]"), ["walk", "stroll", "run"]);
    assert_eq!(run("[lemma not near walk >= 0.7]"), ["run", "cook"]);
}

#[test]
fn the_environment_names_the_vector_file_when_the_flag_is_absent() {
    let fixture = fixture();
    let output = matches(
        "[lemma near walk >= 0.7]",
        &fixture.corpus,
        None,
        Some(&fixture.vectors),
    );
    assert_eq!(matched_lemmas(&output), ["walk", "stroll"]);
}

#[test]
fn a_near_test_without_a_vector_file_or_without_the_word_is_a_compile_error() {
    let fixture = fixture();
    let cases = [
        (
            matches("[lemma near walk >= 0.7]", &fixture.corpus, None, None),
            "none was supplied",
        ),
        (
            matches(
                "[lemma near swim >= 0.7]",
                &fixture.corpus,
                Some(&fixture.vectors),
                None,
            ),
            "no vector for `swim`",
        ),
    ];
    for (output, expected) in cases {
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("WordVectorUnavailable"), "{stderr}");
        assert!(stderr.contains(expected), "{stderr}");
    }
}

#[test]
fn the_fingerprint_follows_the_values_a_near_test_resolves_to() {
    let fixture = fixture();
    let other = fixture.vectors.with_file_name("other.vec");
    fs::write(&other, VECTORS.replace("stroll 0.8 0.6", "stroll 0 1")).unwrap();
    let fingerprint = |vectors: &Path| {
        let output = matches(
            "[lemma near walk >= 0.7]",
            &fixture.corpus,
            Some(vectors),
            None,
        );
        assert!(output.status.success());
        parse_jsonl(&output.stdout)
            .iter()
            .find(|event| event["type"] == "run")
            .map(|event| {
                event["data"]["catalog_fingerprint"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .unwrap()
    };
    assert_eq!(fingerprint(&fixture.vectors), fingerprint(&fixture.vectors));
    assert_ne!(fingerprint(&fixture.vectors), fingerprint(&other));
}

#[test]
fn a_catalog_file_is_checked_against_the_digest_of_the_file_that_was_read() {
    let fixture = fixture();
    let digest = blake3::hash(VECTORS.as_bytes()).to_hex().to_string();
    let catalog = fixture.vectors.with_file_name("near.lx");
    let run = |header: &str| {
        fs::write(
            &catalog,
            format!("{header}p := [lemma near walk >= 0.7].\n"),
        )
        .unwrap();
        let output = lingrep_without_dictionary()
            .env_remove("LINGREP_VECTORS_PATH")
            .args([
                "--json",
                "matches",
                "--input-format",
                "conllu",
                "--patterns",
            ])
            .arg(&catalog)
            .arg("--vectors")
            .arg(&fixture.vectors)
            .arg(&fixture.corpus)
            .output()
            .unwrap();
        assert_eq!(matched_lemmas(&output), ["walk", "stroll"]);
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let declared = |resource: &str, blake3: &str| {
        format!("vectors: {{resource: \"{resource}\", blake3: \"{blake3}\"}}\n")
    };

    let stderr = run(&declared("any-label", &digest));
    assert!(!stderr.contains("vectors_"), "{stderr}");

    let other = "0".repeat(64);
    let stderr = run(&declared("walking", &other));
    assert!(stderr.contains("vectors_binding_mismatch"), "{stderr}");
    assert!(
        stderr.contains(&digest) && stderr.contains(&other),
        "{stderr}"
    );

    let stderr = run(&format!(
        "{}{}",
        declared("old", &other),
        declared("new", &digest)
    ));
    assert!(!stderr.contains("vectors_"), "{stderr}");

    let stderr = run("");
    assert!(stderr.contains("vectors_undeclared"), "{stderr}");
    assert!(
        stderr.contains(&declared("walking", &digest).trim_end().replace('"', "\\\"")),
        "{stderr}"
    );
}

#[test]
fn an_inline_pattern_gets_no_notice_for_declaring_no_table() {
    let fixture = fixture();
    let output = matches(
        "[lemma near walk >= 0.7]",
        &fixture.corpus,
        Some(&fixture.vectors),
        None,
    );
    assert_eq!(matched_lemmas(&output), ["walk", "stroll"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("vectors_"), "{stderr}");
}

/// Rows of `width` six-decimal numbers in (-1, 1) for the words `w0`, `w1`,
/// and so on, from a fixed linear congruential sequence.
fn generated_rows(words: usize, width: usize) -> Vec<(String, Vec<String>)> {
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    (0..words)
        .map(|word| {
            let numbers = (0..width)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    let unit = (state >> 11) as f64 / (1_u64 << 53) as f64;
                    format!("{:.6}", unit * 2.0 - 1.0)
                })
                .collect();
            (format!("w{word}"), numbers)
        })
        .collect()
}

fn conllu_of(lemmas: &[String]) -> String {
    let mut conllu = String::from("# text = generated\n");
    for (index, lemma) in lemmas.iter().enumerate() {
        let (head, relation) = if index == 0 { (0, "root") } else { (1, "dep") };
        conllu.push_str(&format!(
            "{}\t{lemma}\t{lemma}\tX\t_\t_\t{head}\t{relation}\t_\t_\n",
            index + 1
        ));
    }
    conllu.push('\n');
    conllu
}

#[test]
fn every_value_is_near_itself_and_near_selects_the_values_neighbors_lists() {
    let directory = tempfile::tempdir().unwrap();
    let rows = generated_rows(120, 96);
    let mut table = String::new();
    for (word, numbers) in &rows {
        table.push_str(&format!("{word} {}\n", numbers.join(" ")));
    }
    table.push_str(&format!("w1 {}\n", rows[2].1.join(" ")));
    table.push_str(&format!("zero {}\n\n", vec!["0"; 96].join(" ")));
    let vectors = directory.path().join("generated.vec");
    fs::write(&vectors, table).unwrap();
    let words: Vec<String> = rows.iter().map(|(word, _)| word.clone()).collect();
    let corpus = directory.path().join("generated.conllu");
    fs::write(&corpus, conllu_of(&words)).unwrap();

    let catalog = directory.path().join("self.lx");
    fs::write(
        &catalog,
        words
            .iter()
            .map(|word| format!("pattern {word} := [lemma near {word} >= 1].\n"))
            .collect::<String>(),
    )
    .unwrap();
    let output = lingrep_without_dictionary()
        .env_remove("LINGREP_VECTORS_PATH")
        .args([
            "--json",
            "matches",
            "--input-format",
            "conllu",
            "--patterns",
        ])
        .arg(&catalog)
        .arg("--vectors")
        .arg(&vectors)
        .arg(&corpus)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut matched_itself: Vec<(String, String)> = parse_jsonl(&output.stdout)
        .iter()
        .filter(|event| event["type"] == "match")
        .map(|event| {
            (
                event["data"]["pattern"].as_str().unwrap().to_owned(),
                event["data"]["surface"].as_str().unwrap().to_owned(),
            )
        })
        .filter(|(pattern, surface)| pattern == surface)
        .collect();
    matched_itself.sort();
    let mut expected: Vec<(String, String)> = words
        .iter()
        .map(|word| (word.clone(), word.clone()))
        .collect();
    expected.sort();
    assert_eq!(matched_itself, expected);

    for threshold in ["0", "0.05", "0.1", "0.2", "1"] {
        let listed = lingrep_without_dictionary()
            .args(["--json", "neighbors", "--vectors"])
            .arg(&vectors)
            .args(["--min-similarity", threshold, "w1"])
            .output()
            .unwrap();
        assert!(listed.status.success());
        let mut listed: Vec<String> = parse_jsonl(&listed.stdout)
            .iter()
            .filter(|event| event["type"] == "neighbor")
            .map(|event| event["data"]["value"].as_str().unwrap().to_owned())
            .collect();
        listed.sort();
        let mut selected = matched_lemmas(&matches(
            &format!("[lemma near w1 >= {threshold}]"),
            &corpus,
            Some(&vectors),
            None,
        ));
        selected.sort();
        assert!(selected.contains(&"w1".to_owned()), "{threshold}");
        assert_eq!(selected, listed, "{threshold}");
    }
}

#[test]
fn an_unusable_vector_file_is_reported_as_the_file_and_not_as_catalog_syntax() {
    let fixture = fixture();
    let missing = fixture.vectors.with_file_name("missing.vec");
    let ragged = fixture.vectors.with_file_name("ragged.vec");
    fs::write(&ragged, "walk 1 0\nstroll 0.8\n").unwrap();
    let garbled = fixture.vectors.with_file_name("garbled.vec");
    fs::write(&garbled, "walk 1 0\nstroll 0.8 x\n").unwrap();
    for (vectors, expected) in [
        (&missing, ["file_not_found", "missing.vec"]),
        (&ragged, ["WordVectorTableUnreadable", "ragged.vec: line 2"]),
        (
            &garbled,
            ["WordVectorTableUnreadable", "garbled.vec: line 2"],
        ),
    ] {
        let output = matches(
            "[lemma near walk >= 0.7]",
            &fixture.corpus,
            Some(vectors),
            None,
        );
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        for part in expected {
            assert!(stderr.contains(part), "{stderr}");
        }
        assert!(!stderr.contains("Syntax"), "{stderr}");
    }
}

#[test]
fn the_run_record_names_the_vector_file_only_when_a_near_test_was_answered_from_it() {
    let fixture = fixture();
    let run = |pattern: &str| {
        let output = matches(pattern, &fixture.corpus, Some(&fixture.vectors), None);
        assert!(output.status.success());
        parse_jsonl(&output.stdout)
            .into_iter()
            .find(|event| event["type"] == "run")
            .unwrap()
    };
    let near = run("[lemma near walk >= 0.70]");
    assert_eq!(
        near["data"]["word_vectors"],
        serde_json::json!({
            "path": fixture.vectors.display().to_string(),
            "blake3": blake3::hash(VECTORS.as_bytes()).to_hex().to_string(),
            "near": [{"word": "walk", "threshold": "0.7", "value_count": 2}],
        })
    );
    assert!(run("[lemma=walk]")["data"].get("word_vectors").is_none());
}

#[test]
fn check_compares_catalogs_that_have_near_tests_and_reports_an_undeclared_table() {
    let fixture = fixture();
    let catalog = fixture.vectors.with_file_name("a.lx");
    let other = fixture.vectors.with_file_name("b.lx");
    fs::write(&catalog, "pattern p := [lemma near walk >= 0.7].\n").unwrap();
    fs::write(&other, "pattern p := [lemma near walk >= 0.6].\n").unwrap();
    let output = lingrep_without_dictionary()
        .env_remove("LINGREP_VECTORS_PATH")
        .args(["--json", "check", "--patterns"])
        .arg(&catalog)
        .arg("--compare-with")
        .arg(&other)
        .arg("--vectors")
        .arg(&fixture.vectors)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("vectors_undeclared"), "{stderr}");
    let drifted: Vec<String> = parse_jsonl(&output.stdout)
        .iter()
        .filter(|event| event["type"] == "check")
        .flat_map(|event| event["data"]["drift"].as_array().unwrap().clone())
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(drifted, ["p"]);
}

#[test]
fn an_enhanced_graph_node_test_holds_for_exactly_the_listed_values_of_a_long_list() {
    let directory = tempfile::tempdir().unwrap();
    let mut conllu = String::new();
    for (index, head) in ["walk", "stroll", "run", "cook"].iter().enumerate() {
        conllu.push_str(&format!(
            "# text = {head} d{index}\n\
             1\t{head}\t{head}\tVERB\t_\t_\t0\troot\t0:root\t_\n\
             2\td{index}\td{index}\tPRON\t_\t_\t1\tnsubj\t1:nsubj\t_\n\n"
        ));
    }
    let corpus = directory.path().join("heads.conllu");
    fs::write(&corpus, conllu).unwrap();
    let mut listed: Vec<String> = (0..40).map(|filler| format!("z{}", 97 - filler)).collect();
    listed.insert(7, "run".into());
    listed.insert(31, "walk".into());
    let dependents = |operator: &str| {
        matched_lemmas(&matches(
            &format!(
                "(?<a> []) where (?<h> [lemma {operator} {{{}}}]) -[enhanced nsubj]-> $a",
                listed.join(", ")
            ),
            &corpus,
            None,
            None,
        ))
    };
    assert_eq!(dependents("in"), ["d0", "d2"]);
    assert_eq!(dependents("not in"), ["d1", "d3"]);
}
