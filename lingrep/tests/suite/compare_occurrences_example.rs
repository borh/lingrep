// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

//! `examples/compare_occurrences.py` over real `ling matches --json --evidence`
//! streams: the recipe in docs/cli-usage-guide.md.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

use crate::dependency_evidence::{CATALOG, adjacent, conllu_sentence, nonadjacent, write};
use crate::support::{assert_success, lingrep};

const NPV: &str = "npv";
const ADJACENT: &str = "npv_adjacent";

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("examples/compare_occurrences.py")
}

/// 猫を見た with 猫 as `nmod`: adjacent, but not an `npv` role.
fn not_a_role() -> String {
    conllu_sentence(
        "not-a-role",
        &[
            ("猫", "猫", "NOUN", "名詞", 3, "nmod"),
            ("を", "を", "ADP", "助詞", 1, "case"),
            ("見た", "見る", "VERB", "動詞", 0, "root"),
        ],
    )
}

/// 坂を上り、道を下った: two occurrences, each verb adjacent to its particle.
fn two_verbs() -> String {
    conllu_sentence(
        "two-verbs",
        &[
            ("坂", "坂", "NOUN", "名詞", 3, "obj"),
            ("を", "を", "ADP", "助詞", 1, "case"),
            ("上り", "上る", "VERB", "動詞", 7, "advcl"),
            ("、", "、", "PUNCT", "補助記号", 3, "punct"),
            ("道", "道", "NOUN", "名詞", 7, "obj"),
            ("を", "を", "ADP", "助詞", 5, "case"),
            ("下った", "下る", "VERB", "動詞", 0, "root"),
        ],
    )
}

struct Fixture {
    directory: TempDir,
    catalog: PathBuf,
}

impl Fixture {
    /// Two works that repeat 坂を一つ上った.
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let catalog = write(directory.path(), "class6.lx", CATALOG);
        let works = directory.path().join("works");
        std::fs::create_dir(&works).unwrap();
        write(&works, "a.conllu", &[nonadjacent(), two_verbs()].concat());
        write(
            &works,
            "b.conllu",
            &[nonadjacent(), adjacent(), not_a_role()].concat(),
        );
        Self { directory, catalog }
    }

    /// Saves `ling matches` stdout to `name` and returns its path.
    fn matches(&self, name: &str, root: &str, extra: &[&str]) -> PathBuf {
        let output = lingrep()
            .args([
                "--json",
                "matches",
                "--evidence",
                "--input-format",
                "conllu",
            ])
            .args(extra)
            .arg("--patterns")
            .arg(&self.catalog)
            .arg(self.directory.path().join(root))
            .output()
            .unwrap();
        assert_success(&output);
        write(
            self.directory.path(),
            name,
            std::str::from_utf8(&output.stdout).unwrap(),
        )
    }
}

fn compare(a: &Path, b: &Path, args: &[&str]) -> Output {
    Command::new("python3")
        .arg(script())
        .arg(a)
        .arg(b)
        .args(args)
        .output()
        .unwrap()
}

fn rejected(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "{stderr}");
}

const HEADER: &str = "status\tcount_a\tcount_b\tsource_name\tdocument_ordinal\t\
                      document_sentence_ordinal\tunit\twords\tsentence\n";

#[test]
fn compare_aligns_unequal_spans_by_sentence_and_role_words() {
    let fixture = Fixture::new();
    let stream = fixture.matches("class6.jsonl", "works", &[]);
    let output = compare(
        &stream,
        &stream,
        &["--a-pattern", NPV, "--b-pattern", ADJACENT],
    );
    assert_success(&output);
    let slope = "n=坂(1,obj→4) p=を(2,case→1) v=上った(4,root→0)\t坂を一つ上った";
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        [
            HEADER,
            &format!("a_only\t1\t0\ta.conllu\t0\t0\tn=1 p=2 v=4\t{slope}\n"),
            "shared\t1\t1\ta.conllu\t0\t1\tn=1 p=2 v=3\t\
             n=坂(1,obj→3) p=を(2,case→1) v=上り(3,advcl→7)\t坂を上り、道を下った\n",
            "shared\t1\t1\ta.conllu\t0\t1\tn=5 p=6 v=7\t\
             n=道(5,obj→7) p=を(6,case→5) v=下った(7,root→0)\t坂を上り、道を下った\n",
            &format!("a_only\t1\t0\tb.conllu\t1\t0\tn=1 p=2 v=4\t{slope}\n"),
            "shared\t1\t1\tb.conllu\t1\t1\tn=1 p=2 v=3\t\
             n=坂(1,obj→3) p=を(2,case→1) v=上った(3,root→0)\t坂を上った\n",
            "b_only\t0\t1\tb.conllu\t1\t2\tn=1 p=2 v=3\t\
             n=猫(1,nmod→3) p=を(2,case→1) v=見た(3,root→0)\t猫を見た\n",
        ]
        .concat()
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "shared 3, a_only 2, b_only 1\n"
    );
}

#[test]
fn compare_reports_identical_and_one_sided_results() {
    let fixture = Fixture::new();
    let stream = fixture.matches("class6.jsonl", "works", &[]);

    let identical = compare(&stream, &stream, &["--a-pattern", NPV, "--b-pattern", NPV]);
    assert_success(&identical);
    assert_eq!(
        String::from_utf8(identical.stderr).unwrap(),
        "shared 5, a_only 0, b_only 0\n"
    );

    // `pv` binds the same roles but consumes only the particle; restricted to
    // `n` and `p`, every occurrence of either pattern is shared.
    let by_noun = compare(
        &stream,
        &stream,
        &["--a-pattern", NPV, "--b-pattern", "pv", "--roles", "n,p"],
    );
    assert_success(&by_noun);
    assert_eq!(
        String::from_utf8(by_noun.stderr).unwrap(),
        "shared 5, a_only 0, b_only 0\n"
    );

    let one_work = fixture.directory.path().join("one");
    std::fs::create_dir(&one_work).unwrap();
    write(&one_work, "c.conllu", &nonadjacent());
    let single = fixture.matches("one.jsonl", "one", &[]);
    let one_sided = compare(
        &single,
        &single,
        &["--a-pattern", NPV, "--b-pattern", ADJACENT],
    );
    assert_success(&one_sided);
    assert_eq!(
        String::from_utf8(one_sided.stdout).unwrap(),
        format!(
            "{HEADER}a_only\t1\t0\tc.conllu\t0\t0\tn=1 p=2 v=4\t\
             n=坂(1,obj→4) p=を(2,case→1) v=上った(4,root→0)\t坂を一つ上った\n"
        )
    );
}

#[test]
fn compare_rejects_incomplete_sampled_and_mismatched_streams() {
    let fixture = Fixture::new();
    let complete = fixture.matches("class6.jsonl", "works", &[]);
    let pick = ["--a-pattern", NPV, "--b-pattern", ADJACENT];

    let limited = fixture.matches("limited.jsonl", "works", &["--limit", "1"]);
    rejected(
        &compare(&complete, &limited, &pick),
        "B: the run stopped early",
    );

    let retained = fixture.matches("retained.jsonl", "works", &["--retain", "2"]);
    rejected(
        &compare(&retained, &complete, &pick),
        "A: the run kept a --retain sample",
    );

    let text = std::fs::read_to_string(&complete).unwrap();
    let truncated = text
        .lines()
        .filter(|line| !line.contains("\"type\":\"complete\""))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let truncated = write(fixture.directory.path(), "truncated.jsonl", &truncated);
    rejected(
        &compare(&complete, &truncated, &pick),
        "B: no complete event",
    );

    let other = fixture.directory.path().join("other");
    std::fs::create_dir(&other).unwrap();
    write(&other, "a.conllu", &nonadjacent());
    let other = fixture.matches("other.jsonl", "other", &[]);
    rejected(
        &compare(&complete, &other, &pick),
        "A and B differ in corpus, analysis or --where scope",
    );

    let without_evidence = lingrep()
        .args([
            "--json",
            "matches",
            "--input-format",
            "conllu",
            "--patterns",
        ])
        .arg(&fixture.catalog)
        .arg(fixture.directory.path().join("works"))
        .output()
        .unwrap();
    assert_success(&without_evidence);
    let without_evidence = write(
        fixture.directory.path(),
        "plain.jsonl",
        std::str::from_utf8(&without_evidence.stdout).unwrap(),
    );
    rejected(
        &compare(&without_evidence, &complete, &pick),
        "match has no evidence; rerun with --evidence",
    );

    rejected(
        &compare(&complete, &complete, &[]),
        "A: the run has several patterns",
    );
}
