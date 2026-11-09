#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use lingrep_core::testing::MiniDictionary;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{assert_success, lingrep, lingrep_without_dictionary, parse_jsonl};

const CATALOG: &str = "cat := [text=猫].\n\
    key: [text]\n\
    pattern word := [].\n";

const DOCUMENTS: &str = "\
{\"text\":\"猫が猫を見る。犬。\",\"metadata\":{\"genre\":\"novel\",\"tags\":[\"x\",\"y\"]}}
{\"text\":\"猫。\",\"metadata\":{\"genre\":\"essay\",\"tags\":[\"x\"]}}
{\"text\":\"犬が鳥を見る。\",\"metadata\":{\"genre\":\"novel\",\"tags\":[\"y\"]}}
{\"text\":\"猫と犬。犬。犬。\",\"metadata\":{\"genre\":\"poem\",\"tags\":[\"y\"]}}
{\"text\":\"鳥。\",\"metadata\":{\"genre\":\"essay\",\"tags\":[\"z\",\"x\"]}}
{\"text\":\"猫。猫。鳥。\",\"metadata\":{\"tags\":[\"y\"]}}
";

pub(crate) struct Fixture {
    #[cfg_attr(not(feature = "web"), allow(dead_code))]
    pub(crate) directory: TempDir,
    #[cfg_attr(not(feature = "web"), allow(dead_code))]
    pub(crate) dictionary: MiniDictionary,
    pub(crate) artifact: PathBuf,
    pub(crate) catalog: PathBuf,
    pub(crate) table: PathBuf,
}

pub(crate) fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let dictionary = MiniDictionary::new();
    let input = directory.path().join("works.jsonl");
    fs::write(&input, DOCUMENTS).unwrap();
    let artifact = directory.path().join("works.lingcorpus");
    assert_success(
        &lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("--output")
            .arg(&artifact)
            .arg(&input)
            .output()
            .unwrap(),
    );
    let root = directory.path().join("patterns");
    fs::create_dir(&root).unwrap();
    let catalog = root.join("contrast.lx");
    fs::write(&catalog, CATALOG).unwrap();
    let table = directory.path().join("works.lingtable");
    assert_success(
        &lingrep_without_dictionary()
            .args(["--json", "summarize", "--patterns"])
            .arg(&catalog)
            .arg("--corpus")
            .arg(&artifact)
            .arg("--write-table")
            .arg(&table)
            .output()
            .unwrap(),
    );
    Fixture {
        directory,
        dictionary,
        artifact,
        catalog,
        table,
    }
}

fn run(fixture: &Fixture, command: &str, extra: &[&str]) -> std::process::Output {
    let mut invocation = lingrep_without_dictionary();
    invocation.args(["--json", command]);
    if command == "table" {
        invocation.arg(&fixture.table);
    } else {
        invocation.arg("--table").arg(&fixture.table);
    }
    invocation
        .arg("--patterns")
        .arg(&fixture.catalog)
        .arg("--corpus")
        .arg(&fixture.artifact)
        .args(extra)
        .output()
        .unwrap()
}

pub(crate) fn contrast(fixture: &Fixture, extra: &[&str]) -> Vec<Value> {
    let output = run(fixture, "contrast", extra);
    assert_success(&output);
    let events = parse_jsonl(&output.stdout);
    assert!(
        events
            .iter()
            .all(|event| event["schema"] == "lingrep.contrast/v1")
    );
    events
}

fn table(fixture: &Fixture, clause: Option<&str>) -> (BTreeMap<(String, String), (u64, u64)>, u64) {
    let extra = clause.map_or_else(Vec::new, |clause| vec!["--where", clause]);
    let output = run(fixture, "table", &extra);
    assert_success(&output);
    let mut rows = BTreeMap::new();
    let mut tokens = 0;
    for event in parse_jsonl(&output.stdout) {
        let data = &event["data"];
        match event["type"].as_str().unwrap() {
            "row" => {
                rows.insert(
                    (
                        data["pattern"].as_str().unwrap().to_owned(),
                        data["group"].to_string(),
                    ),
                    (
                        data["count"].as_u64().unwrap(),
                        data["works"].as_u64().unwrap(),
                    ),
                );
            }
            "complete" => tokens = data["tokens_admitted"].as_u64().unwrap(),
            _ => {}
        }
    }
    (rows, tokens)
}

fn g2(a: f64, n1: f64, b: f64, n2: f64) -> f64 {
    let e1 = n1 * (a + b) / (n1 + n2);
    let e2 = n2 * (a + b) / (n1 + n2);
    let cell = |o: f64, e: f64| if o == 0.0 { 0.0 } else { o * (o / e).ln() };
    2.0 * (cell(a, e1) + cell(b, e2))
}

fn close(actual: &Value, expected: f64) -> bool {
    (actual.as_f64().unwrap() - expected).abs() <= 1e-9 * expected.abs().max(1.0)
}

fn check(fixture: &Fixture, first: Option<&str>, second: Option<&str>, relation: &str) {
    let mut extra = Vec::new();
    if let Some(first) = first {
        extra.extend(["--where", first]);
    }
    if let Some(second) = second {
        extra.extend(["--versus", second]);
    }
    let events = contrast(fixture, &extra);
    let (first_rows, n1) = table(fixture, first);
    let (second_rows, n2) = table(fixture, second);
    let both = match (first, second) {
        (Some(first), Some(second)) => Some(format!("({first}) & ({second})")),
        (Some(clause), None) | (None, Some(clause)) => Some(clause.to_owned()),
        (None, None) => None,
    };
    let (shared_rows, shared_tokens) = table(fixture, both.as_deref());

    let scopes = &events
        .iter()
        .find(|event| event["type"] == "scopes")
        .unwrap()["data"];
    assert_eq!(scopes["where"]["tokens"], n1);
    assert_eq!(scopes["versus"]["tokens"], n2);
    assert_eq!(scopes["shared"]["tokens"], shared_tokens);
    assert_eq!(scopes["relation"], relation, "{first:?} {second:?}");

    let rows = events
        .iter()
        .filter(|event| event["type"] == "row")
        .map(|event| &event["data"])
        .collect::<Vec<_>>();
    let mut keys = first_rows
        .keys()
        .chain(second_rows.keys())
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    assert_eq!(rows.len(), keys.len());
    for row in &rows {
        let key = (
            row["pattern"].as_str().unwrap().to_owned(),
            row["group"].to_string(),
        );
        let (a, a_works) = first_rows.get(&key).copied().unwrap_or_default();
        let (b, b_works) = second_rows.get(&key).copied().unwrap_or_default();
        let shared = shared_rows.get(&key).copied().unwrap_or_default();
        assert_eq!(
            (
                row["where"]["count"].as_u64(),
                row["where"]["works"].as_u64()
            ),
            (Some(a), Some(a_works))
        );
        assert_eq!(
            (
                row["versus"]["count"].as_u64(),
                row["versus"]["works"].as_u64()
            ),
            (Some(b), Some(b_works))
        );
        assert_eq!(
            (
                row["shared"]["count"].as_u64(),
                row["shared"]["works"].as_u64()
            ),
            (Some(shared.0), Some(shared.1))
        );
        if n1 == 0 || n2 == 0 {
            for field in ["g2", "min_expected", "log2_ratio", "direction"] {
                assert!(row[field].is_null(), "{field}: {row}");
            }
            for (side, n) in [("where", n1), ("versus", n2)] {
                assert_eq!(row[side]["per_million"].is_null(), n == 0);
            }
            continue;
        }
        let (a, b, n1, n2) = (a as f64, b as f64, n1 as f64, n2 as f64);
        assert!(close(&row["g2"], g2(a, n1, b, n2)), "{row}");
        assert!(close(&row["where"]["per_million"], a * 1e6 / n1), "{row}");
        assert!(close(&row["versus"]["per_million"], b * 1e6 / n2), "{row}");
        assert!(close(
            &row["min_expected"],
            (n1.min(n2)) * (a + b) / (n1 + n2)
        ));
        if a == 0.0 || b == 0.0 {
            assert!(row["log2_ratio"].is_null());
        } else {
            assert!(close(&row["log2_ratio"], ((a / n1) / (b / n2)).log2()));
        }
        let direction = if a / n1 > b / n2 {
            "where"
        } else if a / n1 < b / n2 {
            "versus"
        } else {
            "equal"
        };
        assert_eq!(row["direction"], direction);
        if relation == "identical" {
            assert_eq!(row["g2"], 0.0);
        }
    }
    for (left, right) in rows.iter().zip(rows.iter().skip(1)) {
        if left["pattern"] != right["pattern"] {
            continue;
        }
        assert_eq!(
            right["rank"].as_u64().unwrap(),
            left["rank"].as_u64().unwrap() + 1
        );
        let score = |row: &Value| row["g2"].as_f64().unwrap_or(-1.0);
        let total = |row: &Value| {
            row["where"]["count"].as_u64().unwrap() + row["versus"]["count"].as_u64().unwrap()
        };
        assert!(
            score(left) > score(right)
                || (score(left) == score(right) && total(left) >= total(right)),
            "{left} before {right}"
        );
    }
}

#[test]
fn contrast_rows_equal_g2_over_the_counts_of_ling_table_for_each_scope() {
    let fixture = fixture();
    let x = "@document.tags contains \"x\"";
    let y = "@document.tags contains \"y\"";
    let novel = "@document.genre = \"novel\"";
    let essay = "@document.genre = \"essay\"";
    let nothing = "@document.genre = \"opera\"";
    for (first, second, relation) in [
        (Some(x), Some(y), "overlapping"),
        (Some(novel), Some(essay), "disjoint"),
        (Some(x), Some(x), "identical"),
        (Some(y), None, "overlapping"),
        (None, None, "identical"),
        (Some(nothing), Some(novel), "disjoint"),
        (Some(novel), Some(nothing), "disjoint"),
        (Some(nothing), Some(nothing), "identical"),
    ] {
        check(&fixture, first, second, relation);
    }
}

#[test]
fn contrast_top_keeps_the_first_rows_of_the_full_ranking() {
    let fixture = fixture();
    let clauses = [
        "--where",
        "@document.tags contains \"x\"",
        "--versus",
        "@document.tags contains \"y\"",
    ];
    let rows = |extra: &[&str]| {
        contrast(&fixture, &[&clauses[..], extra].concat())
            .into_iter()
            .filter(|event| event["type"] == "row")
            .collect::<Vec<_>>()
    };
    let all = rows(&[]);
    let top = rows(&["--top", "2"]);
    for pattern in ["cat", "word"] {
        let of = |rows: &[Value]| {
            rows.iter()
                .filter(|row| row["data"]["pattern"] == pattern)
                .cloned()
                .collect::<Vec<_>>()
        };
        let all = of(&all);
        assert_eq!(of(&top), all[..all.len().min(2)]);
    }
}

#[test]
fn contrast_rejects_clauses_a_count_table_cannot_answer() {
    let fixture = fixture();
    for flag in ["--where", "--versus"] {
        let output = run(&fixture, "contrast", &[flag, "@sentence.speaker = \"a\""]);
        assert_eq!(output.status.code(), Some(2), "{flag}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(flag),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn table_and_contrast_report_unusable_inputs_with_these_messages() {
    let fixture = fixture();
    let other = fixture.directory.path().join("patterns").join("other.lx");
    fs::write(&other, "other := [text=x].").unwrap();
    let missing = fixture.directory.path().join("missing.lingtable");
    let invoke =
        |command: &str, table: &std::path::Path, catalog: &std::path::Path, extra: &[&str]| {
            let mut invocation = lingrep_without_dictionary();
            invocation.args(["--json", command]);
            if command == "table" {
                invocation.arg(table);
            } else {
                invocation.arg("--table").arg(table);
            }
            let output = invocation
                .arg("--patterns")
                .arg(catalog)
                .arg("--corpus")
                .arg(&fixture.artifact)
                .args(extra)
                .output()
                .unwrap();
            let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
            let message = diagnostic["data"]["message"]
                .as_str()
                .unwrap()
                .replace(&fixture.directory.path().display().to_string(), "DIR")
                .split(' ')
                .map(|word| {
                    let digest = word.trim_end_matches([',', ';']);
                    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        word.replace(digest, "DIGEST")
                    } else {
                        word.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            (output.status.code(), message)
        };
    let cases: &[(
        &str,
        &[&str],
        Option<&std::path::Path>,
        Option<&std::path::Path>,
        i32,
        &str,
    )] = &[
        (
            "table",
            &["--where", "@section = \"a\""],
            None,
            None,
            2,
            "Invalid arguments: table --where accepts only @document conditions: a count table holds counts per document",
        ),
        (
            "table",
            &["--where", "@sentence.speaker = \"a\""],
            None,
            None,
            2,
            "Invalid arguments: table --where accepts only @document conditions: a count table holds counts per document",
        ),
        (
            "table",
            &["--where", "@document.genre ="],
            None,
            None,
            2,
            "Invalid arguments: --where is not a valid scope clause at byte 17: expected a value",
        ),
        (
            "table",
            &[],
            Some(&missing),
            None,
            1,
            "Corpus error: I/O error: No such file or directory (os error 2)",
        ),
        (
            "table",
            &[],
            None,
            Some(&other),
            1,
            "Corpus error: count table DIR/works.lingtable was written for catalog fingerprint DIGEST, but the current one is DIGEST; rebuild it with `ling summarize --write-table`",
        ),
        (
            "contrast",
            &["--where", "@section = \"a\""],
            None,
            None,
            2,
            "Invalid arguments: contrast --where accepts only @document conditions: a count table holds counts per document",
        ),
        (
            "contrast",
            &["--versus", "@sentence.speaker = \"a\""],
            None,
            None,
            2,
            "Invalid arguments: contrast --versus accepts only @document conditions: a count table holds counts per document",
        ),
        (
            "contrast",
            &["--where", "@document.genre ="],
            None,
            None,
            2,
            "Invalid arguments: --where is not a valid scope clause at byte 17: expected a value",
        ),
        (
            "contrast",
            &["--versus", "@document.genre ="],
            None,
            None,
            2,
            "Invalid arguments: --versus is not a valid scope clause at byte 17: expected a value",
        ),
        (
            "contrast",
            &[],
            Some(&missing),
            None,
            1,
            "Corpus error: I/O error: No such file or directory (os error 2)",
        ),
        (
            "contrast",
            &[],
            None,
            Some(&other),
            1,
            "Corpus error: count table DIR/works.lingtable was written for catalog fingerprint DIGEST, but the current one is DIGEST; rebuild it with `ling summarize --write-table`",
        ),
    ];
    let mut observed = Vec::new();
    for (command, extra, table, catalog, _, _) in cases {
        observed.push(invoke(
            command,
            table.unwrap_or(&fixture.table),
            catalog.unwrap_or(&fixture.catalog),
            extra,
        ));
    }
    let expected = cases
        .iter()
        .map(|case| (Some(case.4), case.5.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(observed, expected, "{observed:#?}");
}

#[cfg(feature = "web")]
mod served {
    use std::ffi::OsString;
    use std::process::Command;

    use serde_json::Value;

    use super::{Fixture, contrast, fixture};
    use crate::support::{Server, assert_success, get, lingrep_binary_path, parse_jsonl};

    fn serve(fixture: &Fixture) -> Server {
        let args: Vec<OsString> = vec![
            "--input".into(),
            fixture.artifact.clone().into(),
            "--pattern-root".into(),
            fixture.catalog.parent().unwrap().into(),
            "--count-table".into(),
            fixture.table.clone().into(),
        ];
        crate::support::serve(fixture.directory.path(), fixture.dictionary.path(), &args)
            .unwrap_or_else(|(code, stderr)| panic!("ling serve exited with {code:?}: {stderr}"))
    }

    fn get_json(server: &Server, params: &[(&str, &str)]) -> Value {
        let (status, body) = get_at(server, "contrast", params);
        assert_eq!(status, 200, "{body}");
        body
    }

    fn get_at(server: &Server, endpoint: &str, params: &[(&str, &str)]) -> (u16, Value) {
        let query = params
            .iter()
            .map(|(key, value)| {
                let value = value
                    .bytes()
                    .map(|byte| match byte {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                            (byte as char).to_string()
                        }
                        _ => format!("%{byte:02X}"),
                    })
                    .collect::<String>();
                format!("{key}={value}")
            })
            .collect::<Vec<_>>()
            .join("&");
        let (status, body) = get(server, &format!("/api/{endpoint}?{query}"));
        (status, serde_json::from_str(&body).unwrap())
    }

    fn row_values<'a>(rows: impl Iterator<Item = &'a Value>) -> Vec<Value> {
        rows.map(|row| {
            serde_json::json!([
                row["group"],
                row["where"],
                row["versus"],
                row["shared"],
                row["g2"],
                row["log2_ratio"],
                row["direction"]
            ])
        })
        .collect()
    }

    #[test]
    fn the_compare_endpoint_ranks_as_ling_contrast_and_its_command_reproduces_it() {
        let fixture = fixture();
        let server = serve(&fixture);
        let x = r#"{"tags":{"m":"in","v":["x"]}}"#;
        let y = r#"{"tags":{"m":"in","v":["y"]}}"#;
        for (w, v, relation) in [
            (x, y, "overlapping"),
            (x, x, "identical"),
            (x, "", "overlapping"),
        ] {
            let reply = get_json(
                &server,
                &[
                    ("corpus", "default"),
                    ("catalog", "contrast.lx"),
                    ("pattern", "word"),
                    ("w", w),
                    ("v", v),
                ],
            );
            assert_eq!(reply["status"], "ok", "{reply}");
            assert_eq!(reply["relation"], relation);
            let clause = |condition: &str| {
                (!condition.is_empty()).then_some("@document.tags contains \"x\"")
            };
            assert_eq!(reply["where"]["clause"], clause(w).unwrap_or(""));

            let output = Command::new("sh")
                .args([
                    "-c",
                    &format!(
                        "ling() {{ \"$LINGREP_BINARY\" --json \"$@\"; }}\n{}",
                        reply["command"].as_str().unwrap()
                    ),
                ])
                .env("LINGREP_BINARY", lingrep_binary_path())
                .current_dir(fixture.directory.path())
                .output()
                .unwrap();
            assert_success(&output);
            let events = parse_jsonl(&output.stdout);
            let scopes = &events
                .iter()
                .find(|event| event["type"] == "scopes")
                .unwrap()["data"];
            assert_eq!(scopes["relation"], relation);
            assert_eq!(scopes["where"]["tokens"], reply["where"]["tokens"]);
            assert_eq!(scopes["versus"]["tokens"], reply["versus"]["tokens"]);
            assert_eq!(scopes["shared"]["tokens"], reply["shared"]["tokens"]);
            let rows = events
                .iter()
                .filter(|event| event["type"] == "row" && event["data"]["pattern"] == "word")
                .map(|event| &event["data"])
                .collect::<Vec<_>>();
            assert!(!rows.is_empty());
            assert_eq!(
                row_values(reply["rows"].as_array().unwrap().iter()),
                row_values(rows.iter().copied())
            );
            let pattern = events
                .iter()
                .find(|event| event["type"] == "pattern" && event["data"]["pattern"] == "word")
                .unwrap();
            assert_eq!(reply["entries"], pattern["data"]["entries"]);
            assert_eq!(
                reply["where"]["occurrences"],
                pattern["data"]["where"]["count"]
            );
        }
        let direct = contrast(
            &fixture,
            &[
                "--where",
                "@document.tags contains \"x\"",
                "--versus",
                "@document.tags contains \"y\"",
            ],
        );
        let paged = get_json(
            &server,
            &[
                ("corpus", "default"),
                ("catalog", "contrast.lx"),
                ("pattern", "word"),
                ("w", x),
                ("v", y),
                ("offset", "2"),
                ("limit", "2"),
            ],
        );
        let expected = direct
            .iter()
            .filter(|event| event["type"] == "row" && event["data"]["pattern"] == "word")
            .skip(2)
            .take(2)
            .map(|event| &event["data"]);
        assert_eq!(
            row_values(paged["rows"].as_array().unwrap().iter()),
            row_values(expected)
        );
    }

    #[test]
    fn a_scope_the_table_cannot_answer_is_reported_with_its_flag() {
        let fixture = fixture();
        let server = serve(&fixture);
        let reply = get_json(
            &server,
            &[
                ("corpus", "default"),
                ("catalog", "contrast.lx"),
                ("pattern", "word"),
                ("versus", "@section = \"a\""),
            ],
        );
        assert_eq!(reply["status"], "unsupported");
        assert_eq!(reply["scope"], "versus");
        assert_eq!(reply["reason"], "section");
    }

    #[test]
    fn an_offset_at_or_past_the_last_entry_is_an_empty_page() {
        let fixture = fixture();
        let server = serve(&fixture);
        for endpoint in ["contrast", "frequency"] {
            let params = |offset: &'static str| {
                [
                    ("corpus", "default"),
                    ("catalog", "contrast.lx"),
                    ("pattern", "word"),
                    ("offset", offset),
                ]
            };
            let (status, first) = get_at(&server, endpoint, &params("0"));
            assert_eq!(status, 200, "{endpoint}: {first}");
            let entries = first["entries"].as_u64().unwrap();
            assert!(entries > 0, "{endpoint}: {first}");
            let at_end: &'static str = entries.to_string().leak();
            for offset in [at_end, "100001", "18446744073709551615"] {
                let (status, past) = get_at(&server, endpoint, &params(offset));
                assert_eq!(status, 200, "{endpoint} {offset}: {past}");
                assert_eq!(past["rows"], serde_json::json!([]), "{endpoint} {offset}");
                assert_eq!(past["entries"], first["entries"], "{endpoint} {offset}");
            }
        }
    }
}
