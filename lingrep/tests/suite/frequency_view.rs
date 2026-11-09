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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use hegel::generators;
use lingrep_core::testing::MiniDictionary;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::{assert_success, lingrep, lingrep_binary_path, parse_jsonl};

const CATALOG: &str = "cat := [text=猫].\n\
    key: [text]\n\
    pattern word := [].\n\
    pattern pair := [] [].\n";
const PATTERNS: [&str; 3] = ["cat", "word", "pair"];

const DOCUMENTS: &str = "\
{\"text\":\"猫が猫を見る。犬。\",\"metadata\":{\"genre\":\"novel\",\"tags\":[\"x\",\"y\"],\"year\":2001}}
{\"text\":\"猫。\",\"metadata\":{\"genre\":\"essay\",\"tags\":[\"x\"],\"year\":2002}}
{\"text\":\"犬が鳥を見る。\",\"metadata\":{\"genre\":\"novel\",\"year\":2001}}
{\"text\":\"猫と犬。\",\"metadata\":{\"genre\":\"poem\",\"tags\":[\"y\"],\"year\":2004}}
{\"text\":\"鳥。\",\"metadata\":{\"genre\":\"essay\",\"tags\":[\"z\",\"x\"],\"year\":2003}}
{\"text\":\"猫。猫。\",\"metadata\":{\"tags\":[\"y\"],\"year\":2003}}
{\"text\":\"犬。鳥。猫。\",\"metadata\":{\"genre\":\"novel\",\"tags\":[\"z\"],\"year\":2005}}
{\"text\":\"見る。\",\"metadata\":{\"genre\":\"poem\",\"tags\":[\"x\"],\"year\":2002}}
";
const GENRES: [&str; 3] = ["essay", "novel", "poem"];
const TAGS: [&str; 3] = ["x", "y", "z"];

struct Fixture {
    directory: TempDir,
    dictionary: MiniDictionary,
    artifact: PathBuf,
    root: PathBuf,
    catalog: PathBuf,
    table: PathBuf,
}

fn build_artifact(
    directory: &Path,
    dictionary: &MiniDictionary,
    name: &str,
    documents: &str,
) -> PathBuf {
    let input = directory.join(format!("{name}.jsonl"));
    fs::write(&input, documents).unwrap();
    let artifact = directory.join(format!("{name}.lingcorpus"));
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert_success(&built);
    artifact
}

fn write_table(artifact: &Path, catalog: &Path, table: &Path) {
    let output = crate::support::lingrep_without_dictionary()
        .args(["--json", "summarize", "--patterns"])
        .arg(catalog)
        .arg("--corpus")
        .arg(artifact)
        .arg("--write-table")
        .arg(table)
        .output()
        .unwrap();
    assert_success(&output);
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let dictionary = MiniDictionary::new();
    let artifact = build_artifact(directory.path(), &dictionary, "works", DOCUMENTS);
    let root = directory.path().join("patterns");
    fs::create_dir(&root).unwrap();
    let catalog = root.join("table.lx");
    fs::write(&catalog, CATALOG).unwrap();
    let table = directory.path().join("works.lingtable");
    write_table(&artifact, &catalog, &table);
    Fixture {
        directory,
        dictionary,
        artifact,
        root,
        catalog,
        table,
    }
}

struct Server {
    child: Child,
    address: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(fixture: &Fixture, extra: &[OsString]) -> Result<Server, (Option<i32>, String)> {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let stderr_path = fixture
        .directory
        .path()
        .join(format!("serve-{port}.stderr"));
    let mut child = Command::new(lingrep_binary_path())
        .arg("serve")
        .arg("--dict-path")
        .arg(fixture.dictionary.path())
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(fs::File::create(&stderr_path).unwrap())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        match lines.next() {
            Some(Ok(line)) if line.contains("listening on") => {
                return Ok(Server {
                    child,
                    address: format!("127.0.0.1:{port}"),
                });
            }
            Some(Ok(_)) => {}
            _ => {
                let status = child.wait().unwrap();
                return Err((status.code(), fs::read_to_string(&stderr_path).unwrap()));
            }
        }
    }
}

fn args(parts: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    parts.iter().map(|part| part.as_ref().to_owned()).collect()
}

fn served(fixture: &Fixture, extra: &[OsString]) -> Server {
    let mut all = args(&[
        &"--input",
        &fixture.artifact,
        &"--pattern-root",
        &fixture.root,
    ]);
    all.extend_from_slice(extra);
    serve(fixture, &all).unwrap_or_else(|(code, stderr)| panic!("exit {code:?}: {stderr}"))
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn query(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("{key}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn get(server: &Server, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.address
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let response = String::from_utf8(response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(
        !head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "{head}"
    );
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_owned())
}

fn get_json(server: &Server, path: &str) -> Value {
    let (status, body) = get(server, path);
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

#[derive(Clone, Debug)]
enum Condition {
    In(Vec<&'static str>),
    Out(Vec<&'static str>),
    Range(Option<i64>, Option<i64>),
}

impl Condition {
    fn json(&self) -> Value {
        match self {
            Self::In(values) => json!({"m": "in", "v": values}),
            Self::Out(values) => json!({"m": "out", "v": values}),
            Self::Range(lo, hi) => {
                let mut range = json!({"m": "range"});
                if let Some(lo) = lo {
                    range["lo"] = json!(lo);
                }
                if let Some(hi) = hi {
                    range["hi"] = json!(hi);
                }
                range
            }
        }
    }

    fn predicate(&self, key: &str) -> Option<String> {
        let quoted = |values: &[&str]| {
            values
                .iter()
                .map(|value| format!("\"{value}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let list = key == "tags";
        match self {
            Self::In(values) | Self::Out(values) if values.is_empty() => None,
            Self::In(values) if list => Some(format!(
                "({})",
                values
                    .iter()
                    .map(|value| format!("@document.{key} contains \"{value}\""))
                    .collect::<Vec<_>>()
                    .join(" | ")
            )),
            Self::Out(values) if list => Some(
                values
                    .iter()
                    .map(|value| format!("@document.{key} not contains \"{value}\""))
                    .collect::<Vec<_>>()
                    .join(" & "),
            ),
            Self::In(values) => Some(format!("@document.{key} in {{{}}}", quoted(values))),
            Self::Out(values) => Some(format!("@document.{key} not in {{{}}}", quoted(values))),
            Self::Range(None, None) => None,
            Self::Range(lo, hi) => Some(
                lo.map(|lo| format!("@document.{key} >= {lo}"))
                    .into_iter()
                    .chain(hi.map(|hi| format!("@document.{key} <= {hi}")))
                    .collect::<Vec<_>>()
                    .join(" & "),
            ),
        }
    }
}

type Conditions = BTreeMap<&'static str, Condition>;

fn clause(conditions: &Conditions) -> String {
    conditions
        .iter()
        .filter_map(|(key, condition)| condition.predicate(key))
        .collect::<Vec<_>>()
        .join(" & ")
}

fn conditions_json(conditions: &Conditions) -> String {
    Value::Object(
        conditions
            .iter()
            .map(|(key, condition)| ((*key).to_owned(), condition.json()))
            .collect(),
    )
    .to_string()
}

fn summarize(fixture: &Fixture, clause: &str, extra: &[&str]) -> Vec<Value> {
    let mut command = crate::support::lingrep_without_dictionary();
    command
        .args(["--json", "summarize", "--patterns"])
        .arg(&fixture.catalog)
        .arg("--corpus")
        .arg(&fixture.artifact)
        .args(extra);
    if !clause.is_empty() {
        command.args(["--where", clause]);
    }
    let output = command.output().unwrap();
    assert_success(&output);
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|record| record["type"] == "summary")
        .map(|record| record["data"].clone())
        .collect()
}

struct Expected {
    rows: Vec<(u64, Value, u64, u64)>,
    occurrences: u64,
    works: u64,
}

fn expected(fixture: &Fixture, clause: &str) -> BTreeMap<String, Expected> {
    let all = summarize(fixture, clause, &["--top", "all"]);
    let by_document = summarize(fixture, clause, &["--by-document"]);
    let mut works = BTreeMap::<(String, String), u64>::new();
    let mut documents = BTreeMap::<String, BTreeSet<u64>>::new();
    for record in &by_document {
        let pattern = record["pattern"].as_str().unwrap().to_owned();
        *works
            .entry((pattern.clone(), record["group"].to_string()))
            .or_default() += 1;
        documents
            .entry(pattern)
            .or_default()
            .insert(record["document"].as_u64().unwrap());
    }
    PATTERNS
        .iter()
        .map(|pattern| {
            let rows = all
                .iter()
                .filter(|record| record["pattern"] == *pattern)
                .map(|record| {
                    (
                        record["rank"].as_u64().unwrap(),
                        record["group"].clone(),
                        record["count"].as_u64().unwrap(),
                        works[&((*pattern).to_owned(), record["group"].to_string())],
                    )
                })
                .collect::<Vec<_>>();
            (
                (*pattern).to_owned(),
                Expected {
                    occurrences: rows.iter().map(|row| row.2).sum(),
                    works: documents.get(*pattern).map_or(0, |set| set.len() as u64),
                    rows,
                },
            )
        })
        .collect()
}

fn group_text(group: &Value) -> String {
    match group["kind"].as_str().unwrap() {
        "key" => group["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| match field["kind"].as_str().unwrap() {
                "list" => field["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(","),
                _ => field["value"].as_str().unwrap().to_owned(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => group["value"].as_str().unwrap().to_owned(),
    }
}

fn answer(
    server: &Server,
    pattern: &str,
    conditions: &Conditions,
    sort: &str,
    filter: &str,
) -> Value {
    get_json(
        server,
        &format!(
            "/api/frequency?{}",
            query(&[
                ("corpus", "default"),
                ("catalog", "table.lx"),
                ("pattern", pattern),
                ("w", &conditions_json(conditions)),
                ("sort", sort),
                ("q", filter),
                ("limit", "1000"),
            ])
        ),
    )
}

fn check(server: &Server, fixture: &Fixture, conditions: &Conditions, sort: &str, filter: &str) {
    let clause = clause(conditions);
    let expected = expected(fixture, &clause);
    for pattern in PATTERNS {
        let reply = answer(server, pattern, conditions, sort, filter);
        let context = format!("{pattern} where {clause:?} sort {sort} q {filter:?}");
        assert_eq!(reply["status"], "ok", "{context}: {reply}");
        let want = &expected[pattern];
        assert_eq!(reply["occurrences"], want.occurrences, "{context}");
        assert_eq!(reply["works"], want.works, "{context}");
        assert_eq!(reply["entries"], want.rows.len() as u64, "{context}");
        let mut rows = want
            .rows
            .iter()
            .filter(|row| group_text(&row.1).contains(filter))
            .cloned()
            .collect::<Vec<_>>();
        match sort {
            "works" => rows.sort_by(|left, right| right.3.cmp(&left.3).then(left.0.cmp(&right.0))),
            "key" => rows.sort_by(|left, right| {
                group_text(&left.1)
                    .cmp(&group_text(&right.1))
                    .then(left.0.cmp(&right.0))
            }),
            _ => {}
        }
        assert_eq!(reply["matching"], rows.len() as u64, "{context}");
        let got = reply["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                assert_eq!(
                    row["text"].as_str().unwrap(),
                    group_text(&row["group"]),
                    "{context}"
                );
                (
                    row["rank"].as_u64().unwrap(),
                    row["group"].clone(),
                    row["count"].as_u64().unwrap(),
                    row["works"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(got, rows, "{context}");
        for offset in [1, usize::MAX] {
            let page = get_json(
                server,
                &format!(
                    "/api/frequency?{}",
                    query(&[
                        ("corpus", "default"),
                        ("catalog", "table.lx"),
                        ("pattern", pattern),
                        ("w", &conditions_json(conditions)),
                        ("sort", sort),
                        ("q", filter),
                        ("offset", &offset.to_string()),
                        ("limit", "1"),
                    ])
                ),
            );
            let expected = reply["rows"]
                .as_array()
                .unwrap()
                .iter()
                .skip(offset)
                .take(1)
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(page["rows"], json!(expected), "{context} offset {offset}");
            assert_eq!(page["matching"], reply["matching"]);
        }

        let stated = conditions
            .iter()
            .filter(|(key, condition)| condition.predicate(key).is_some())
            .map(|(key, _)| *key)
            .collect::<BTreeSet<_>>();
        let kept = reply["kept"].as_object().unwrap();
        assert_eq!(
            kept.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            stated,
            "{context}"
        );
        for key in stated {
            let mut others = conditions.clone();
            others.remove(key);
            let of = expected_occurrences(fixture, &self::clause(&others), pattern);
            assert_eq!(
                (kept[key]["kept"].clone(), kept[key]["of"].clone()),
                (json!(want.occurrences), json!(of)),
                "{context}: {key}"
            );
        }
    }
}

fn expected_occurrences(fixture: &Fixture, clause: &str, pattern: &str) -> u64 {
    summarize(fixture, clause, &["--top", "all"])
        .iter()
        .filter(|record| record["pattern"] == pattern)
        .map(|record| record["count"].as_u64().unwrap())
        .sum()
}

#[test]
fn exclusions_report_missing_fields_within_the_other_conditions() {
    let fixture = fixture();
    let server = served(&fixture, &args(&[&"--count-table", &fixture.table]));
    let cases = [
        (
            "genre",
            [("genre", Condition::Out(vec!["novel"]))]
                .into_iter()
                .collect::<Conditions>(),
            1,
        ),
        (
            "tags",
            [("tags", Condition::Out(vec!["x"]))].into_iter().collect(),
            1,
        ),
        (
            "genre",
            [
                ("genre", Condition::Out(vec!["novel"])),
                ("year", Condition::Range(Some(2004), None)),
            ]
            .into_iter()
            .collect(),
            0,
        ),
        (
            "tags",
            [
                ("tags", Condition::Out(vec!["x"])),
                ("genre", Condition::In(vec!["essay"])),
            ]
            .into_iter()
            .collect(),
            0,
        ),
    ];
    for (key, conditions, expected) in cases {
        let reply = answer(&server, "word", &conditions, "count", "");
        assert_eq!(
            reply["kept"][key]["excluded_missing_field_documents"],
            expected
        );
        if conditions.len() == 1 {
            let output = crate::support::lingrep_without_dictionary()
                .args(["--json", "summarize", "--patterns"])
                .arg(&fixture.catalog)
                .arg("--corpus")
                .arg(&fixture.artifact)
                .args(["--where", &clause(&conditions)])
                .output()
                .unwrap();
            assert_success(&output);
            let records = parse_jsonl(&output.stdout);
            let complete = records.iter().find(|r| r["type"] == "complete").unwrap();
            assert_eq!(
                reply["kept"][key]["excluded_missing_field_documents"],
                complete["data"]["scope"]["documents"]["excluded_missing_field"]
            );
        }
    }
}

#[test]
fn frequency_rows_equal_summarize_where_for_every_condition_set() {
    let fixture = fixture();
    let server = served(&fixture, &args(&[&"--count-table", &fixture.table]));

    let meta = get_json(&server, "/api/frequency/meta");
    assert_eq!(
        meta["pairs"],
        json!([{
            "corpus": "default",
            "corpus_name": meta["pairs"][0]["corpus_name"],
            "catalog": "table.lx",
            "patterns": [
                {"name": "cat", "source": "cat := [text=猫]."},
                {"name": "word", "source": "key: [text]\npattern word := []."},
                {"name": "pair", "source": "pattern pair := [] []."},
            ],
            "workbench": meta["pairs"][0]["workbench"],
        }])
    );
    assert_eq!(
        meta["dimensions"]["default"],
        json!([
            {"key": "genre", "documents": 7, "kind": "categorical", "list": false, "values": 3},
            {"key": "tags", "documents": 7, "kind": "categorical", "list": true, "values": 3},
            {"key": "year", "documents": 8, "kind": "range", "min": 2001.0, "max": 2005.0},
        ])
    );

    let named = |pairs: &[(&'static str, Condition)]| pairs.iter().cloned().collect::<Conditions>();
    for (conditions, sort, filter) in [
        (Conditions::new(), "count", ""),
        (
            named(&[("genre", Condition::In(vec!["novel"]))]),
            "count",
            "",
        ),
        (
            named(&[("genre", Condition::Out(vec!["novel", "poem"]))]),
            "works",
            "",
        ),
        (named(&[("tags", Condition::In(vec!["y", "z"]))]), "key", ""),
        (named(&[("tags", Condition::Out(vec!["x"]))]), "count", "猫"),
        (
            named(&[("year", Condition::Range(Some(2002), Some(2003)))]),
            "count",
            "",
        ),
        (
            named(&[("year", Condition::Range(None, Some(2002)))]),
            "count",
            "",
        ),
        (
            named(&[
                ("genre", Condition::In(vec!["novel", "essay"])),
                ("tags", Condition::In(vec!["x"])),
                ("year", Condition::Range(Some(2001), None)),
            ]),
            "works",
            "",
        ),
        (
            named(&[
                ("genre", Condition::In(vec!["poem"])),
                ("year", Condition::Range(None, Some(2001))),
            ]),
            "count",
            "",
        ),
        (
            named(&[("year", Condition::Range(Some(1999), Some(1999)))]),
            "count",
            "",
        ),
        (
            named(&[
                ("genre", Condition::In(vec![])),
                ("year", Condition::Range(None, None)),
            ]),
            "count",
            "",
        ),
    ] {
        check(&server, &fixture, &conditions, sort, filter);
    }

    hegel::Hegel::new(|tc: hegel::TestCase| {
        let mut conditions = Conditions::new();
        for (key, values) in [("genre", GENRES), ("tags", TAGS)] {
            let chosen = values
                .iter()
                .copied()
                .filter(|_| tc.draw(generators::booleans()))
                .collect::<Vec<_>>();
            match tc.draw(generators::integers::<u8>().max_value(2)) {
                0 => {}
                1 => {
                    conditions.insert(key, Condition::In(chosen));
                }
                _ => {
                    conditions.insert(key, Condition::Out(chosen));
                }
            }
        }
        if tc.draw(generators::booleans()) {
            let bound = || {
                tc.draw(generators::booleans()).then(|| {
                    tc.draw(
                        generators::integers::<i64>()
                            .min_value(2000)
                            .max_value(2006),
                    )
                })
            };
            let lo = bound();
            let hi = bound();
            conditions.insert("year", Condition::Range(lo, hi));
        }
        let sort = ["count", "works", "key"]
            [usize::from(tc.draw(generators::integers::<u8>().max_value(2)))];
        let filter = ["", "猫", "犬", "見る"]
            [usize::from(tc.draw(generators::integers::<u8>().max_value(3)))];
        check(&server, &fixture, &conditions, sort, filter);
    })
    .settings(hegel::Settings::new().test_cases(25).derandomize(true))
    .run();
}

/// Each value of `key` with the occurrences of `pattern` in the admitted
/// documents that carry it, from `summarize --by-metadata`; values whose
/// documents are all excluded count 0.
fn expected_values(
    fixture: &Fixture,
    clause: &str,
    pattern: &str,
    key: &str,
) -> Vec<(String, u64)> {
    let mut command = crate::support::lingrep_without_dictionary();
    command
        .args(["--json", "summarize", "--patterns"])
        .arg(&fixture.catalog)
        .arg("--corpus")
        .arg(&fixture.artifact)
        .args(["--by-metadata", &format!("@document.{key}")]);
    if !clause.is_empty() {
        command.args(["--where", clause]);
    }
    let output = command.output().unwrap();
    assert_success(&output);
    let mut counts = BTreeMap::<String, u64>::new();
    for record in parse_jsonl(&output.stdout) {
        if record["type"] != "metadata_value" {
            continue;
        }
        let Some(value) = record["data"]["value"].as_str() else {
            continue;
        };
        let matches = record["data"]["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|count| count["pattern"] == pattern)
            .map_or(0, |count| count["matches"].as_u64().unwrap());
        counts.insert(value.to_owned(), matches);
    }
    let all: &[&str] = match key {
        "genre" => &GENRES,
        "tags" => &TAGS,
        _ => &["2001", "2002", "2003", "2004", "2005"],
    };
    let mut values = all
        .iter()
        .map(|value| {
            (
                (*value).to_owned(),
                counts.get(*value).copied().unwrap_or(0),
            )
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    values
}

#[test]
fn condition_values_count_the_pattern_under_the_other_conditions() {
    let fixture = fixture();
    let server = served(&fixture, &args(&[&"--count-table", &fixture.table]));
    let named = |pairs: &[(&'static str, Condition)]| pairs.iter().cloned().collect::<Conditions>();
    for conditions in [
        Conditions::new(),
        named(&[("genre", Condition::In(vec!["novel"]))]),
        named(&[
            ("tags", Condition::Out(vec!["x"])),
            ("year", Condition::Range(Some(2002), None)),
        ]),
        named(&[("year", Condition::Range(Some(1999), Some(1999)))]),
    ] {
        for pattern in PATTERNS {
            for key in ["genre", "tags", "year"] {
                let mut others = conditions.clone();
                others.remove(key);
                let reply = get_json(
                    &server,
                    &format!(
                        "/api/frequency/values?{}",
                        query(&[
                            ("corpus", "default"),
                            ("catalog", "table.lx"),
                            ("pattern", pattern),
                            ("key", key),
                            ("w", &conditions_json(&conditions)),
                        ])
                    ),
                );
                let context = format!("{pattern} {key} {conditions:?}");
                assert_eq!(reply["status"], "ok", "{context}: {reply}");
                let got = reply["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|pair| {
                        (
                            pair[0].as_str().unwrap().to_owned(),
                            pair[1].as_u64().unwrap(),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    got,
                    expected_values(&fixture, &clause(&others), pattern, key),
                    "{context}"
                );
            }
        }
    }
}

#[test]
fn the_shown_command_reproduces_the_rows() {
    let mut fixture = fixture();
    for (path, folder) in [
        (&mut fixture.table, "results"),
        (&mut fixture.artifact, "corpora"),
    ] {
        let directory = fixture.directory.path().join(folder);
        fs::create_dir(&directory).unwrap();
        let moved = directory.join(path.file_name().unwrap());
        fs::rename(&*path, &moved).unwrap();
        *path = moved;
    }
    let server = served(&fixture, &args(&[&"--count-table", &fixture.table]));
    let conditions = [
        ("genre", Condition::In(vec!["novel", "essay"])),
        ("year", Condition::Range(Some(2001), Some(2003))),
    ]
    .into_iter()
    .collect::<Conditions>();
    let reply = answer(&server, "word", &conditions, "count", "");
    let clause = reply["clause"].as_str().unwrap();
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
    assert_eq!(
        reply["command"],
        format!(
            "ling table './results/works.lingtable' --top 100 --where '{clause}' --corpus './corpora/works.lingcorpus' --patterns './patterns/table.lx'"
        )
    );
    let rows = parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|record| record["type"] == "row" && record["data"]["pattern"] == "word")
        .map(|record| {
            (
                record["data"]["group"].clone(),
                record["data"]["count"].clone(),
                record["data"]["works"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reply["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (
                row["group"].clone(),
                row["count"].clone(),
                row["works"].clone()
            ))
            .collect::<Vec<_>>(),
        rows
    );
}

#[test]
fn copied_command_preserves_shell_metacharacters_in_filenames() {
    let mut fixture = fixture();
    for (path, name) in [
        (
            &mut fixture.artifact,
            "-works ' ; $(touch injected).lingcorpus",
        ),
        (&mut fixture.table, "-table ' ; $(touch injected).lingtable"),
        (&mut fixture.catalog, "-catalog ' ; $(touch injected).lx"),
    ] {
        let renamed = path.with_file_name(name);
        fs::rename(&*path, &renamed).unwrap();
        *path = renamed;
    }
    let server = served(&fixture, &args(&[&"--count-table", &fixture.table]));
    let reply = get_json(
        &server,
        &format!(
            "/api/frequency?{}",
            query(&[
                ("corpus", "default"),
                (
                    "catalog",
                    fixture.catalog.file_name().unwrap().to_str().unwrap()
                ),
                ("pattern", "word"),
                ("where", "@document.genre = \"novel\""),
            ])
        ),
    );
    let command = reply["command"].as_str().unwrap();
    let output = Command::new("sh")
        .args([
            "-c",
            &format!("ling() {{ printf '%s\\0' \"$@\"; }}\n{command}"),
        ])
        .current_dir(fixture.directory.path())
        .output()
        .unwrap();
    assert_success(&output);
    let got = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| std::str::from_utf8(arg).unwrap())
        .collect::<Vec<_>>();
    let table = format!("./{}", fixture.table.file_name().unwrap().to_str().unwrap());
    let corpus = format!(
        "./{}",
        fixture.artifact.file_name().unwrap().to_str().unwrap()
    );
    let catalog = format!(
        "./{}",
        fixture
            .catalog
            .strip_prefix(fixture.directory.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(
        got,
        [
            "table",
            &table,
            "--top",
            "100",
            "--where",
            "@document.genre = \"novel\"",
            "--corpus",
            &corpus,
            "--patterns",
            &catalog
        ]
    );
    let executed = Command::new("sh")
        .args([
            "-c",
            &format!("ling() {{ \"$LINGREP_BINARY\" --json \"$@\"; }}\n{command}"),
        ])
        .env("LINGREP_BINARY", lingrep_binary_path())
        .current_dir(fixture.directory.path())
        .output()
        .unwrap();
    assert_success(&executed);
    assert!(!command.contains(fixture.directory.path().to_str().unwrap()));
    assert!(!fixture.directory.path().join("injected").exists());
}

#[test]
fn unsupported_frequency_queries_return_an_explanation() {
    let fixture = fixture();
    let server = served(&fixture, &args(&[&"--count-table", &fixture.table]));
    let ask = |params: &[(&str, &str)]| {
        let mut all = vec![
            ("corpus", "default"),
            ("catalog", "table.lx"),
            ("pattern", "cat"),
        ];
        all.extend_from_slice(params);
        get_json(&server, &format!("/api/frequency?{}", query(&all)))
    };
    assert_eq!(
        ask(&[("where", "@section = abstract")]),
        json!({"status": "unsupported", "reason": "section"})
    );
    assert_eq!(
        ask(&[("where", "@section in {methods} & @document.year >= 2001")]),
        json!({"status": "unsupported", "reason": "section"})
    );
    assert_eq!(
        ask(&[("w", r#"{"author":{"m":"in","v":["a"]}}"#)]),
        json!({"status": "unsupported", "reason": "unknown_key", "key": "author"})
    );
    assert_eq!(
        ask(&[("w", r#"{"year":{"m":"in","v":["2001"]}}"#)]),
        json!({"status": "unsupported", "reason": "shape", "key": "year"})
    );
    assert_eq!(ask(&[("where", "@document.year >=")])["reason"], "syntax");
    let empty = ask(&[("where", "@document.year = 1999")]);
    assert_eq!(
        (&empty["status"], &empty["occurrences"], &empty["rows"]),
        (&json!("ok"), &json!(0), &json!([]))
    );

    let unavailable = get_json(
        &server,
        &format!(
            "/api/frequency?{}",
            query(&[
                ("corpus", "default"),
                ("catalog", "other.lx"),
                ("pattern", "cat")
            ])
        ),
    );
    assert_eq!(unavailable, json!({"status": "unavailable"}));
    let (status, _) = get(
        &server,
        &format!(
            "/api/frequency?{}",
            query(&[
                ("corpus", "default"),
                ("catalog", "table.lx"),
                ("pattern", "nope")
            ])
        ),
    );
    assert_eq!(status, 400);
}

#[test]
fn the_view_appears_only_with_tables_and_answers_under_a_subpath() {
    let fixture = fixture();
    let plain = served(&fixture, &[]);
    let (status, index) = get(&plain, "/");
    assert_eq!(status, 200);
    assert!(!index.contains("href=\"frequencies\""));
    assert_eq!(
        get_json(&plain, "/api/frequency/meta"),
        json!({"pairs": [], "dimensions": {}, "labels": {}})
    );
    drop(plain);

    let sub = served(
        &fixture,
        &args(&[
            &"--count-table",
            &fixture.table,
            &"--base-path",
            &"/lingrep",
        ]),
    );
    let (status, index) = get(&sub, "/lingrep/");
    assert_eq!(status, 200);
    assert!(index.contains("href=\"frequencies\""), "{index}");
    let (status, page) = get(&sub, "/lingrep/frequencies");
    assert_eq!(status, 200);
    assert!(page.contains("src=\"assets/frequency.js\""), "{page}");
    for asset in [
        "/lingrep/assets/frequency.js",
        "/lingrep/assets/frequency.css",
    ] {
        assert_eq!(get(&sub, asset).0, 200, "{asset}");
    }
    let reply = get_json(
        &sub,
        &format!(
            "/lingrep/api/frequency?{}",
            query(&[
                ("corpus", "default"),
                ("catalog", "table.lx"),
                ("pattern", "cat")
            ])
        ),
    );
    assert_eq!(reply["status"], "ok");
}

#[test]
fn a_table_that_cannot_be_bound_stops_startup_naming_it() {
    let fixture = fixture();
    let base = |extra: &[&dyn AsRef<std::ffi::OsStr>]| {
        let mut all = args(&[
            &"--input",
            &fixture.artifact,
            &"--pattern-root",
            &fixture.root,
        ]);
        all.extend(args(extra));
        all
    };
    let fails = |extra: Vec<OsString>| match serve(&fixture, &extra) {
        Ok(_) => panic!("startup succeeded with {extra:?}"),
        Err((code, stderr)) => {
            assert_eq!(code, Some(2), "{stderr}");
            stderr
        }
    };
    let shown = |path: &Path| format!("--count-table {}", path.display());

    let garbage = fixture.directory.path().join("garbage.lingtable");
    fs::write(&garbage, "not a table").unwrap();
    let stderr = fails(base(&[&"--count-table", &garbage]));
    assert!(
        stderr.contains(&format!("{}: ", shown(&garbage))),
        "{stderr}"
    );

    let other = build_artifact(
        fixture.directory.path(),
        &fixture.dictionary,
        "other",
        "{\"text\":\"犬。\"}\n",
    );
    let other_table = fixture.directory.path().join("other.lingtable");
    write_table(&other, &fixture.catalog, &other_table);
    let stderr = fails(base(&[&"--count-table", &other_table]));
    assert!(
        stderr.contains(&format!(
            "{}: matches no served corpus: it counts analysis identity ",
            shown(&other_table)
        )),
        "{stderr}"
    );

    let edited_root = fixture.directory.path().join("edited");
    fs::create_dir(&edited_root).unwrap();
    fs::write(
        edited_root.join("table.lx"),
        format!("{CATALOG}pattern extra := [].\n"),
    )
    .unwrap();
    let stderr = fails(args(&[
        &"--input",
        &fixture.artifact,
        &"--pattern-root",
        &edited_root,
        &"--count-table",
        &fixture.table,
    ]));
    assert!(
        stderr.contains(&format!(
            "{}: matches no catalog under the pattern root: it counts catalog fingerprint ",
            shown(&fixture.table)
        )),
        "{stderr}"
    );

    let copy = fixture.directory.path().join("copy.lingtable");
    copy_dir(&fixture.table, &copy);
    let stderr = fails(base(&[
        &"--count-table",
        &fixture.table,
        &"--count-table",
        &copy,
    ]));
    assert!(
        stderr.contains(&format!(
            "{} and {} both count catalog table.lx over corpus \"default\"",
            shown(&fixture.table),
            shown(&copy)
        )),
        "{stderr}"
    );
}

#[test]
fn an_invalid_label_file_stops_startup_naming_it() {
    let fixture = fixture();
    let labels = fixture.directory.path().join("labels.json");
    fs::write(&labels, r#"{"keys": {}}"#).unwrap();
    let extra = args(&[
        &"--input",
        &fixture.artifact,
        &"--pattern-root",
        &fixture.root,
        &"--count-table",
        &fixture.table,
        &"--metadata-labels",
        &labels,
    ]);
    let Err((code, stderr)) = serve(&fixture, &extra) else {
        panic!("startup succeeded with an invalid label file");
    };
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "--metadata-labels {}: missing field",
            labels.display()
        )),
        "{stderr}"
    );
}

#[test]
fn a_label_for_a_key_no_corpus_carries_is_reported_at_startup() {
    let fixture = fixture();
    let labels = fixture.directory.path().join("labels.json");
    fs::write(
        &labels,
        r#"{"vocabulary": "v", "edition": "1", "source": "s", "licence": "l",
        "keys": {"@document.no_such_key": {"labels": {"en": "typo"}}}}"#,
    )
    .unwrap();
    let server = served(
        &fixture,
        &args(&[
            &"--count-table",
            &fixture.table,
            &"--metadata-labels",
            &labels,
        ]),
    );
    let port = server.address.rsplit(':').next().unwrap();
    let stderr = fs::read_to_string(
        fixture
            .directory
            .path()
            .join(format!("serve-{port}.stderr")),
    )
    .unwrap();
    assert!(
        stderr.contains(&format!(
            "--metadata-labels {}: no corpus with a count table carries @document.no_such_key",
            labels.display()
        )),
        "{stderr}"
    );
}

fn copy_dir(from: &Path, to: &Path) {
    if from.is_dir() {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_dir(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        fs::copy(from, to).unwrap();
    }
}

#[test]
fn frequency_endpoints_page_large_tables() {
    use lingrep_corpus::{CountTable, CountTableWriter, GroupKind, GroupSchema};
    let fixture = fixture();
    let key = CountTable::read_key(&fixture.table).unwrap();
    let original = CountTable::open(&fixture.table, &key).unwrap();
    let documents = original.documents().collect::<Vec<_>>();
    let values = (0..100_001)
        .map(|n| format!("v{n:06}{}", "x".repeat(700)))
        .collect::<Vec<_>>();
    let path = fixture.directory.path().join("large.lingtable");
    let mut writer = CountTableWriter::create(
        &path,
        key,
        PATTERNS.iter().map(|name| (*name).to_owned()).collect(),
        vec![GroupSchema {
            kind: GroupKind::Surface,
            names: Vec::new(),
        }],
        &values,
        &documents,
    )
    .unwrap();
    for index in 0..values.len() {
        writer.begin_entry(1, 0, &[index as u32]).unwrap();
        writer.add_pair(0, 1).unwrap();
    }
    writer
        .finish(&lingrep_core::execution::CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();
    assert!(
        CountTable::open(&path, &CountTable::read_key(&path).unwrap())
            .unwrap()
            .mapped_bytes()
            > 64 * 1024 * 1024
    );
    let server = served(&fixture, &args(&[&"--count-table", &path]));
    for endpoint in ["frequency", "frequency/values"] {
        let (status, body) = get(
            &server,
            &format!(
                "/api/{endpoint}?{}",
                query(&[
                    ("corpus", "default"),
                    ("catalog", "table.lx"),
                    ("pattern", "word"),
                    ("limit", "1"),
                    ("key", "genre"),
                ])
            ),
        );
        assert_eq!(status, 200, "{body}");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["status"], "ok");
        if endpoint == "frequency" {
            assert_eq!(body["entries"], 100_001);
            assert_eq!(body["rows"].as_array().unwrap().len(), 1);
        } else {
            assert_eq!(body["values"].as_array().unwrap().len(), 1);
        }
    }
}
