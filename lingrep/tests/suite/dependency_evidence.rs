// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

//! `ling matches --evidence`: the words a match's relation judgment rests on,
//! including graph-bound words outside the consuming span.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::{assert_success, lingrep, parse_jsonl};

pub(crate) const CATALOG: &str = "\
set CASE := {が, を, に, で, から, より, と, へ}.
set NOMINAL := {NOUN, PROPN, PRON, NUM}.
set ROLE := {nsubj, obj, obl}.
npv := (?<n> [upos in @NOMINAL & dep.rel in @ROLE])
       (?<p> [dep.rel=case & lemma in @CASE])
       not_followed_by([dep.rel=fixed])
       where (?<v> [upos=VERB]) -[@ROLE]-> $n, $n -[case]-> $p.
npv_adjacent := (?<n> [upos in @NOMINAL]) (?<p> [lemma in @CASE]) (?<v> [upos=VERB]).
pv := (?<p> [dep.rel=case & lemma in @CASE])
      where (?<n> [upos in @NOMINAL]) -[case]-> $p, (?<v> [upos=VERB]) -[@ROLE]-> $n.
";

type Word<'a> = (&'a str, &'a str, &'a str, &'a str, usize, &'a str);

pub(crate) fn conllu_sentence(id: &str, words: &[Word<'_>]) -> String {
    let mut sentence = format!(
        "# sent_id = {id}\n# text = {}\n",
        words.iter().map(|word| word.0).collect::<String>()
    );
    for (index, (form, lemma, upos, xpos, head, rel)) in words.iter().enumerate() {
        sentence.push_str(&format!(
            "{}\t{form}\t{lemma}\t{upos}\t{xpos}\t_\t{head}\t{rel}\t_\t_\n",
            index + 1
        ));
    }
    sentence.push('\n');
    sentence
}

pub(crate) fn nonadjacent() -> String {
    conllu_sentence(
        "nonadjacent",
        &[
            ("坂", "坂", "NOUN", "名詞", 4, "obj"),
            ("を", "を", "ADP", "助詞", 1, "case"),
            ("一つ", "一つ", "NUM", "名詞", 4, "obl"),
            ("上った", "上る", "VERB", "動詞", 0, "root"),
        ],
    )
}

pub(crate) fn adjacent() -> String {
    conllu_sentence(
        "adjacent",
        &[
            ("坂", "坂", "NOUN", "名詞", 3, "obj"),
            ("を", "を", "ADP", "助詞", 1, "case"),
            ("上った", "上る", "VERB", "動詞", 0, "root"),
        ],
    )
}

fn head_first() -> String {
    conllu_sentence(
        "head-first",
        &[
            ("見た", "見る", "VERB", "_", 0, "root"),
            ("、", "、", "PUNCT", "補助記号", 1, "punct"),
            ("猫", "猫", "NOUN", "名詞", 1, "obj"),
            ("を", "を", "ADP", "助詞", 3, "case"),
        ],
    )
}

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

fn long() -> String {
    let mut words: Vec<Word<'_>> = vec![
        ("坂", "坂", "NOUN", "名詞", 303, "obj"),
        ("を", "を", "ADP", "助詞", 1, "case"),
    ];
    words.extend(std::iter::repeat_n(
        ("、", "、", "PUNCT", "補助記号", 303, "punct"),
        300,
    ));
    words.push(("上った", "上る", "VERB", "動詞", 0, "root"));
    conllu_sentence("long", &words)
}

pub(crate) fn write(directory: &Path, name: &str, contents: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

fn run(args: &[&str], catalog: &Path, input: &Path) -> Output {
    let output = lingrep()
        .args(args)
        .arg("--patterns")
        .arg(catalog)
        .arg(input)
        .output()
        .unwrap();
    assert_success(&output);
    output
}

fn build(input: &Path, artifact: &Path) {
    let output = lingrep()
        .args(["corpus", "build", "--output"])
        .arg(artifact)
        .arg(input)
        .output()
        .unwrap();
    assert_success(&output);
}

fn matches(output: &Output) -> Vec<Value> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| event["type"] == "match")
        .map(|event| event["data"].clone())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn word(
    word: u64,
    roles: &[&str],
    in_span: bool,
    text: &str,
    lemma: &str,
    upos: &str,
    xpos: Option<&str>,
    head: &str,
    rel: &str,
) -> Value {
    json!({
        "word": word,
        "roles": roles,
        "in_span": in_span,
        "text": text,
        "lemma": lemma,
        "norm": null,
        "upos": upos,
        "xpos": xpos,
        "head": head,
        "rel": rel,
    })
}

fn relation_evidence(records: &[Value]) -> Vec<Value> {
    records
        .iter()
        .filter(|record| record["pattern"] != "pos")
        .map(|record| {
            json!([
                record["pattern"],
                record["document_sentence_ordinal"],
                record["span"],
                record["evidence"],
            ])
        })
        .collect()
}

#[test]
fn graph_assignment_exhaustion_reports_a_matching_error_without_complete_results() {
    let directory = TempDir::new().unwrap();
    let clauses = (0..5)
        .map(|index| format!("$v -[*]-> (?<b{index}> [])"))
        .collect::<Vec<_>>()
        .join(", ");
    let catalog = write(
        directory.path(),
        "wide.lx",
        &format!("pattern connected := (?<v> [upos=VERB]) where {clauses}."),
    );
    let mut words = vec![("v", "v", "VERB", "_", 0, "root")];
    words.extend((0..31).map(|_| ("n", "n", "NOUN", "_", 1, "obj")));
    let input = write(
        directory.path(),
        "wide.conllu",
        &conllu_sentence("wide", &words),
    );
    let output = lingrep()
        .args(["--json", "matches", "--patterns"])
        .arg(catalog)
        .arg(input)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostics = parse_jsonl(&output.stderr);
    let diagnostic = diagnostics
        .iter()
        .find(|event| event["data"]["code"] == "graph_assignment_limit")
        .expect("graph resource diagnostic");
    assert_eq!(diagnostic["data"]["stage"], "matching");
    let description = String::from_utf8(output.stderr.clone()).unwrap();
    assert!(description.contains("connected"));
    assert!(description.contains("ordering constraint"));
    assert!(
        !parse_jsonl(&output.stdout)
            .iter()
            .any(|event| event["type"] == "complete")
    );
    assert!(matches(&output).is_empty());
}

#[test]
fn evidence_names_graph_bound_words_outside_the_span_direct_and_prepared() {
    let directory = TempDir::new().unwrap();
    let catalog = write(directory.path(), "class6.lx", CATALOG);
    let corpus = [nonadjacent(), adjacent(), head_first(), two_verbs(), long()].concat();
    let input = write(directory.path(), "class6.conllu", &corpus);
    let artifact = directory.path().join("class6.lingcorpus");
    build(&input, &artifact);

    let direct = matches(&run(
        &["--json", "matches", "--evidence", "--limit", "all"],
        &catalog,
        &input,
    ));
    let prepared = lingrep()
        .args(["--json", "matches", "--evidence", "--limit", "all"])
        .arg("--patterns")
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .output()
        .unwrap();
    assert_success(&prepared);
    assert_eq!(matches(&prepared), direct, "direct and prepared evidence");

    let saka = |in_span| {
        [
            word(
                1,
                &["n"],
                true,
                "坂",
                "坂",
                "NOUN",
                Some("名詞"),
                "4",
                "obj",
            ),
            word(
                2,
                &["p"],
                true,
                "を",
                "を",
                "ADP",
                Some("助詞"),
                "1",
                "case",
            ),
            word(
                4,
                &["v"],
                in_span,
                "上った",
                "上る",
                "VERB",
                Some("動詞"),
                "0",
                "root",
            ),
        ]
    };
    let s1 = json!({"sentence": "坂を一つ上った", "words": saka(false)});
    let adjacent_words = |v_in_span| {
        json!({"sentence": "坂を上った", "words": [
            word(1, &["n"], true, "坂", "坂", "NOUN", Some("名詞"), "3", "obj"),
            word(2, &["p"], true, "を", "を", "ADP", Some("助詞"), "1", "case"),
            word(3, &["v"], v_in_span, "上った", "上る", "VERB", Some("動詞"), "0", "root"),
        ]})
    };
    let s3_words = |n_in_span| {
        json!({"sentence": "見た、猫を", "words": [
            word(1, &["v"], false, "見た", "見る", "VERB", None, "0", "root"),
            word(3, &["n"], n_in_span, "猫", "猫", "NOUN", Some("名詞"), "1", "obj"),
            word(4, &["p"], true, "を", "を", "ADP", Some("助詞"), "3", "case"),
        ]})
    };
    let s4 = "坂を上り、道を下った";
    let s4_first = |pattern_span: bool| {
        json!({"sentence": s4, "words": [
            word(1, &["n"], pattern_span, "坂", "坂", "NOUN", Some("名詞"), "3", "obj"),
            word(2, &["p"], true, "を", "を", "ADP", Some("助詞"), "1", "case"),
            word(3, &["v"], false, "上り", "上る", "VERB", Some("動詞"), "7", "advcl"),
        ]})
    };
    let s4_second = |pattern_span: bool| {
        json!({"sentence": s4, "words": [
            word(5, &["n"], pattern_span, "道", "道", "NOUN", Some("名詞"), "7", "obj"),
            word(6, &["p"], true, "を", "を", "ADP", Some("助詞"), "5", "case"),
            word(7, &["v"], false, "下った", "下る", "VERB", Some("動詞"), "0", "root"),
        ]})
    };
    let long_sentence = format!("坂を{}上った", "、".repeat(300));
    let long_words = |n_in_span| {
        json!({"sentence": long_sentence, "words": [
            word(1, &["n"], n_in_span, "坂", "坂", "NOUN", Some("名詞"), "303", "obj"),
            word(2, &["p"], true, "を", "を", "ADP", Some("助詞"), "1", "case"),
            word(303, &["v"], false, "上った", "上る", "VERB", Some("動詞"), "0", "root"),
        ]})
    };
    let mut s4_adjacent_first = s4_first(true);
    s4_adjacent_first["words"][2]["in_span"] = json!(true);
    let mut s4_adjacent_second = s4_second(true);
    s4_adjacent_second["words"][2]["in_span"] = json!(true);
    let span = |start: u64, end: u64| json!({"start": start, "end": end});

    assert_eq!(
        relation_evidence(&direct),
        [
            json!(["npv", 0, span(0, 2), s1]),
            json!(["pv", 0, span(1, 2), {
                "sentence": "坂を一つ上った",
                "words": [
                    word(1, &["n"], false, "坂", "坂", "NOUN", Some("名詞"), "4", "obj"),
                    word(2, &["p"], true, "を", "を", "ADP", Some("助詞"), "1", "case"),
                    word(4, &["v"], false, "上った", "上る", "VERB", Some("動詞"), "0", "root"),
                ],
            }]),
            json!(["npv", 1, span(0, 2), adjacent_words(false)]),
            json!(["npv_adjacent", 1, span(0, 3), adjacent_words(true)]),
            json!(["pv", 1, span(1, 2), {
                "sentence": "坂を上った",
                "words": [
                    word(1, &["n"], false, "坂", "坂", "NOUN", Some("名詞"), "3", "obj"),
                    word(2, &["p"], true, "を", "を", "ADP", Some("助詞"), "1", "case"),
                    word(3, &["v"], false, "上った", "上る", "VERB", Some("動詞"), "0", "root"),
                ],
            }]),
            json!(["npv", 2, span(2, 4), s3_words(true)]),
            json!(["pv", 2, span(3, 4), s3_words(false)]),
            json!(["npv", 3, span(0, 2), s4_first(true)]),
            json!(["npv_adjacent", 3, span(0, 3), s4_adjacent_first]),
            json!(["pv", 3, span(1, 2), s4_first(false)]),
            json!(["npv", 3, span(4, 6), s4_second(true)]),
            json!(["npv_adjacent", 3, span(4, 7), s4_adjacent_second]),
            json!(["pv", 3, span(5, 6), s4_second(false)]),
            json!(["npv", 4, span(0, 2), long_words(true)]),
            json!(["pv", 4, span(1, 2), long_words(false)]),
        ]
    );
}

#[test]
fn evidence_is_additive_to_the_default_json_records() {
    let directory = TempDir::new().unwrap();
    let catalog = write(directory.path(), "class6.lx", CATALOG);
    let input = write(
        directory.path(),
        "class6.conllu",
        &[nonadjacent(), two_verbs()].concat(),
    );
    let artifact = directory.path().join("class6.lingcorpus");
    build(&input, &artifact);
    let records = |extra: &[&str], corpus: &[&str]| {
        let output = lingrep()
            .args(["--json", "matches", "--context", "2"])
            .args(extra)
            .arg("--patterns")
            .arg(&catalog)
            .args(corpus)
            .output()
            .unwrap();
        assert_success(&output);
        parse_jsonl(&output.stdout)
            .into_iter()
            .filter(|event| event["type"] != "complete")
            .map(|mut event| {
                if let Some(data) = event["data"].as_object_mut() {
                    data.remove("run_id");
                }
                event
            })
            .collect::<Vec<_>>()
    };
    let direct = [input.to_str().unwrap()];
    let prepared = ["--corpus", artifact.to_str().unwrap()];
    for corpus in [&direct[..], &prepared[..]] {
        let default = records(&[], corpus);
        let mut evidence = records(&["--evidence"], corpus);
        for event in &mut evidence {
            if event["type"] == "match" {
                assert!(event["data"]["evidence"].is_object(), "{event}");
                event["data"].as_object_mut().unwrap().remove("evidence");
            }
        }
        assert!(default.iter().any(|event| event["type"] == "match"));
        assert_eq!(evidence, default, "{corpus:?}");
    }
}

#[test]
fn evidence_tsv_rows_are_one_word_per_row_with_the_full_sentence() {
    let directory = TempDir::new().unwrap();
    let catalog = write(directory.path(), "class6.lx", CATALOG);
    let input = write(directory.path(), "class6.conllu", &nonadjacent());
    let output = run(&["matches", "--tsv", "--evidence"], &catalog, &input);
    let rows = String::from_utf8(output.stdout).unwrap();
    let prefix = |sequence: u64, pattern: &str, start: u64, end: u64| {
        format!("{sequence}\t{pattern}\tclass6.conllu\t0\t0\t0\t{start}\t{end}")
    };
    let sentence = "坂を一つ上った";
    assert_eq!(
        rows,
        [
            format!(
                "{}\t1\tn\ttrue\t坂\t坂\t\tNOUN\t名詞\t4\tobj\t{sentence}",
                prefix(0, "npv", 0, 2)
            ),
            format!(
                "{}\t2\tp\ttrue\tを\tを\t\tADP\t助詞\t1\tcase\t{sentence}",
                prefix(0, "npv", 0, 2)
            ),
            format!(
                "{}\t4\tv\tfalse\t上った\t上る\t\tVERB\t動詞\t0\troot\t{sentence}",
                prefix(0, "npv", 0, 2)
            ),
            format!(
                "{}\t1\tn\tfalse\t坂\t坂\t\tNOUN\t名詞\t4\tobj\t{sentence}",
                prefix(1, "pv", 1, 2)
            ),
            format!(
                "{}\t2\tp\ttrue\tを\tを\t\tADP\t助詞\t1\tcase\t{sentence}",
                prefix(1, "pv", 1, 2)
            ),
            format!(
                "{}\t4\tv\tfalse\t上った\t上る\t\tVERB\t動詞\t0\troot\t{sentence}",
                prefix(1, "pv", 1, 2)
            ),
        ]
        .map(|row| row + "\n")
        .concat()
    );

    let default = run(&["matches", "--tsv"], &catalog, &input);
    assert_eq!(
        String::from_utf8(default.stdout).unwrap(),
        "0\tnpv\tclass6.conllu\t0\t0\t0\t0\t2\t\t坂を\t\n\
         1\tpv\tclass6.conllu\t0\t0\t0\t1\t2\t\tを\t\n"
    );
}

#[test]
fn evidence_requires_a_machine_readable_output() {
    let directory = TempDir::new().unwrap();
    let catalog = write(directory.path(), "class6.lx", CATALOG);
    let input = write(directory.path(), "class6.conllu", &nonadjacent());
    let output = lingrep()
        .args(["matches", "--evidence", "--patterns"])
        .arg(&catalog)
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
