// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
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
//! `--where '@section …'` end to end: refusals, warnings, and accounting.
//!
//! The Markdown fixture is one document with three labelled sections —
//! `methods` (two sentences), `results` (one), and `references` (one). The last
//! is excluded by the default read-time policy, which is the interaction the
//! `--md-*` flags exist to make escapable.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use lingrep_core::testing::MiniDictionary;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{fixture, lingrep, lingrep_without_dictionary, parse_jsonl};

fn run_on(input: &str, extra: &[&str]) -> Output {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep();
    command
        .arg("--json")
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg("[].")
        .arg("--limit")
        .arg("all");
    command.args(extra).arg(fixture(input)).output().unwrap()
}

fn scope_of(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .find(|record| record["type"] == "complete")
        .expect("a completion record")["data"]["scope"]
        .clone()
}

/// A JSONL corpus records no sections at all. Answering with a confident zero
/// and a clean four-bucket accounting would be a false denominator, which is
/// what the scope report exists to prevent.
#[test]
fn a_section_clause_over_jsonl_is_refused_before_processing() {
    let output = run_on("scope-fixture.jsonl", &["--where", "@section = methods"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("records no section labels"),
        "the refusal must say why: {stderr}"
    );
    assert!(
        stderr.contains("jsonl"),
        "the refusal must name the format: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing may be published before the clause is refused"
    );
}

#[test]
fn a_section_clause_selects_sentences_from_markdown() {
    let output = run_on("labelled", &["--where", "@section = methods"]);
    let scope = scope_of(&output);

    assert_eq!(scope["clause"], "@section = methods");
    assert_eq!(scope["sentences"]["admitted"], 2);
    assert_eq!(scope["sentences"]["excluded_by_predicate"], 1);
    // A section predicate is two-valued: these two buckets are unreachable
    // from it, and only a metadata predicate can ever fill them.
    assert_eq!(scope["sentences"]["excluded_missing_field"], 0);
    assert_eq!(scope["sentences"]["excluded_shape_mismatch"], 0);
}

/// The document was in scope; none of its sentences were. Both numbers are
/// true, and the report shows both rather than reconciling them.
#[test]
fn a_document_is_admitted_even_when_no_sentence_survives() {
    let output = run_on("labelled", &["--where", "@section = absent_label"]);
    let scope = scope_of(&output);

    assert_eq!(scope["documents"]["admitted"], 1);
    assert_eq!(scope["sentences"]["admitted"], 0);
}

/// A label absent from *this* corpus is a warning, not an error: one catalog
/// legitimately runs over several corpora and only some may record it.
#[test]
fn a_label_the_corpus_never_records_warns_and_completes() {
    let output = run_on("labelled", &["--where", "@section = absent_label"]);

    assert!(output.status.success(), "a warning must not fail the run");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("absent_label"),
        "the warning must name the label: {stderr}"
    );
    assert!(
        stderr.contains("methods"),
        "the warning must name what the corpus does record: {stderr}"
    );
}

/// The trap the flags exist to make escapable: `references` is excluded at
/// *read* time by the default policy, so the predicate can only ever be empty,
/// and the warning has to point at the flag that changes it.
#[test]
fn a_default_excluded_label_warns_and_names_the_flag() {
    let output = run_on("labelled", &["--where", "@section = references"]);

    assert_eq!(scope_of(&output)["sentences"]["admitted"], 0);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--md-exclude-sections"),
        "the warning must name the flag that admits it: {stderr}"
    );
}

/// …and passing that flag makes the same clause answerable. This pair is the
/// whole argument for shipping the flags and the predicates together.
#[test]
fn disabling_exclusion_makes_a_default_excluded_section_queryable() {
    let output = run_on(
        "labelled",
        &[
            "--md-exclude-sections",
            "",
            "--where",
            "@section = references",
        ],
    );

    assert_eq!(scope_of(&output)["sentences"]["admitted"], 1);
}

#[test]
fn an_empty_include_list_fails_before_the_run() {
    let output = run_on("labelled", &["--md-include-sections", ""]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("admits nothing"), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn naming_a_self_label_warns_and_points_at_the_self_label_set() {
    let output = run_on("labelled", &["--md-exclude-sections", "title"]);

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--md-self-labels"), "{stderr}");
}

/// Two documents, three section labels, and a `year` that separates them —
/// enough for a clause to be decided at the document grain for one document and
/// at the sentence grain for the other.
struct AgreementFixture {
    _directory: TempDir,
    root: PathBuf,
    artifact: PathBuf,
    dictionary: MiniDictionary,
}

impl AgreementFixture {
    fn new() -> Self {
        let dictionary = MiniDictionary::new();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("corpus");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("recent.md"),
            // `discussion` sits *between* the two selected sections on
            // purpose. The admitted set is then {0, 1, 3} rather than a
            // prefix, which is what makes the direct path's renumbering
            // observable — see
            // `the_two_paths_number_the_same_sentences_differently`.
            "---\nyear: 2020\n---\n\n\
             # Methods {.methods}\n\n猫が鳥を見る。\n犬も鳥を見る。\n\n\
             # Discussion {.discussion}\n\n一と二と三。\n\n\
             # Results {.results}\n\n猫は犬である。\n",
        )
        .unwrap();
        fs::write(
            root.join("old.md"),
            "---\nyear: 2005\n---\n\n\
             # Methods {.methods}\n\n犬が鳥を見る。\n",
        )
        .unwrap();
        let artifact = directory.path().join("agreement.lingcorpus");
        let build = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("--output")
            .arg(&artifact)
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        Self {
            _directory: directory,
            root,
            artifact,
            dictionary,
        }
    }

    fn direct(&self, clause: &str) -> Output {
        lingrep()
            .arg("--json")
            .args(["matches", "--pattern", "[].", "--limit", "all"])
            .args(["--dict-path"])
            .arg(self.dictionary.path())
            .args(["--where", clause])
            .arg(&self.root)
            .output()
            .unwrap()
    }

    fn prepared(&self, clause: &str) -> Output {
        lingrep_without_dictionary()
            .arg("--json")
            .args(["matches", "--pattern", "[].", "--limit", "all"])
            .args(["--where", clause, "--corpus"])
            .arg(&self.artifact)
            .output()
            .unwrap()
    }
}

/// Every match record, as `(document, sentence ordinal, span start, surface)`.
fn records(output: &Output) -> Vec<(String, u64, u64, String)> {
    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut rows: Vec<(String, u64, u64, String)> = parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|record| record["type"] == "match")
        .map(|record| {
            let data = &record["data"];
            let name = Path::new(data["source_name"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (
                name,
                data["document_sentence_ordinal"].as_u64().unwrap(),
                data["span"]["start"].as_u64().unwrap(),
                data["surface"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// The published sentences, by content rather than by number.
///
/// The ordinal fields are dropped before comparison. Under a scope clause the direct
/// path renumbers over admitted sentences while a prepared artifact keeps the
/// numbering it was built with, so the same sentence carries different ordinals
/// on the two paths — documented behaviour, pinned by
/// `the_two_paths_number_the_same_sentences_differently`.
///
/// The pattern is `[].`, so one admitted sentence yields one record per token,
/// and reassembling them in span order recovers the sentence.
fn published(output: &Output) -> Vec<(String, String)> {
    let mut sentences: std::collections::BTreeMap<(String, u64), Vec<(u64, String)>> =
        std::collections::BTreeMap::new();
    for (name, ordinal, start, surface) in records(output) {
        sentences
            .entry((name, ordinal))
            .or_default()
            .push((start, surface));
    }
    let mut published: Vec<(String, String)> = sentences
        .into_iter()
        .map(|((name, _), mut tokens)| {
            tokens.sort();
            (
                name,
                tokens.into_iter().map(|(_, surface)| surface).collect(),
            )
        })
        .collect();
    published.sort();
    published
}

/// One evaluator, two label sources. If the direct and prepared paths ever
/// disagree about which sentences a clause admits, one of them has grown a
/// second copy of the clause semantics — which is the failure `ActiveLabels`
/// exists to prevent.
///
/// The clause mixes metadata and sentence evaluation: `old.md` is decided from
/// metadata alone and never reaches pass two, while `recent.md` is decided per
/// sentence. A discrepancy in either grain causes a difference here.
#[test]
fn the_direct_and_prepared_paths_admit_the_same_sentences() {
    let fixture = AgreementFixture::new();
    let clause = "@section in {methods, results} & @document.year >= 2010";

    let direct = published(&fixture.direct(clause));
    let prepared = published(&fixture.prepared(clause));

    assert_eq!(direct, prepared, "the two paths disagreed");
    // Three of the five sentences: two `methods` and one `results`, all from
    // the 2020 document. `discussion` is out of scope and `old.md` is out of
    // range.
    assert_eq!(
        direct,
        // Sorted, so this is codepoint order rather than corpus order.
        vec![
            ("recent.md".to_owned(), "犬も鳥を見る。".to_owned()),
            ("recent.md".to_owned(), "猫が鳥を見る。".to_owned()),
            ("recent.md".to_owned(), "猫は犬である。".to_owned()),
        ]
    );
}

/// The two paths give an admitted sentence the same in-document ordinal.
///
/// Direct input compacts corpus ordinals over the admitted sentences, but an
/// excluded sentence still counts toward the in-document ordinal, as on a
/// prepared artifact; a `within=document` lookaround window ends at that gap.
#[test]
fn the_two_paths_give_the_same_sentences_the_same_document_ordinals() {
    let fixture = AgreementFixture::new();
    let clause = "@section in {methods, results} & @document.year >= 2010";

    let ordinals = |output: &Output| {
        records(output)
            .into_iter()
            .map(|(_, ordinal, _, _)| ordinal)
            .collect::<std::collections::BTreeSet<_>>()
    };

    // The `results` sentence is the document's fourth; `discussion` precedes
    // it and is not admitted.
    assert_eq!(
        ordinals(&fixture.direct(clause)),
        std::collections::BTreeSet::from([0, 1, 3])
    );
    assert_eq!(
        ordinals(&fixture.prepared(clause)),
        std::collections::BTreeSet::from([0, 1, 3])
    );
}

/// The accounting agrees with the other path *and* with its own stream.
///
/// The second half is not redundant. Mutating the prepared path's admission
/// grain leaves this report entirely correct — it is computed per sentence
/// either way — while the run publishes sentences the report counted as
/// excluded. A report that is right about a stream it does not describe is the
/// worst of the failures available here, and only the stream comparison
/// catches it.
#[test]
fn each_path_publishes_exactly_the_sentences_it_reports_admitting() {
    let fixture = AgreementFixture::new();
    let clause = "@section in {methods, results} & @document.year >= 2010";

    for (label, output) in [
        ("direct", fixture.direct(clause)),
        ("prepared", fixture.prepared(clause)),
    ] {
        let scope = scope_of(&output);
        assert_eq!(scope["sentences"]["admitted"], 3, "{label}");
        // `old.md` is decided by `year` alone and never reaches pass two, so
        // one document is admitted and one is excluded — while four of the
        // admitted document's five sentences do reach pass two and one of them
        // is excluded there.
        assert_eq!(scope["documents"]["admitted"], 1, "{label}");
        assert_eq!(scope["documents"]["excluded_by_predicate"], 1, "{label}");
        assert_eq!(scope["sentences"]["excluded_by_predicate"], 2, "{label}");
        assert_eq!(scope["basis"], "corpus", "{label}");
        assert_eq!(
            published(&output).len(),
            scope["sentences"]["admitted"].as_u64().unwrap() as usize,
            "{label} published a different set of sentences than it reported"
        );
    }
}

/// A metadata-only clause must not acquire section behaviour, and in
/// particular must not be rejected against an input that records no sections.
#[test]
fn a_metadata_only_clause_is_unaffected_by_any_of_this() {
    let output = run_on(
        "scope-fixture.jsonl",
        &["--where", "@document.year >= 2010"],
    );
    let scope = scope_of(&output);

    assert_eq!(scope["sentences"]["admitted"], 2);
    assert!(
        String::from_utf8(output.stderr.clone()).unwrap().is_empty(),
        "a metadata-only clause must emit no section diagnostics"
    );
}

/// BCCWJ's paragraph tags ride the sentence `active_labels` (spec D2), so a
/// section clause over a BCCWJ distribution selects exactly the tagged
/// sentences — over the live input and the prepared artifact alike.
fn bccwj_section_scope(
    clause: &str,
    root: &Path,
    artifact: &Path,
    dictionary: &MiniDictionary,
    prepared: bool,
) -> Value {
    let mut command = lingrep();
    command.args(["--json", "matches", "--pattern", "[]."]);
    if prepared {
        command.arg("--corpus").arg(artifact);
    } else {
        command
            .arg("--dict-path")
            .arg(dictionary.path())
            .args(["--input-format", "bccwj"])
            .arg(root);
    }
    command.args(["--where", clause]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .find(|record| record["type"] == "complete")
        .expect("a completion record")["data"]["scope"]
        .clone()
}

#[test]
fn a_section_clause_selects_the_tagged_bccwj_sentences_only() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = crate::support::bccwj_distribution(&directory.path().join("dist"));
    let artifact = crate::support::bccwj_build_artifact(&root, &dictionary);

    let live = bccwj_section_scope("@section = speech", &root, &artifact, &dictionary, false);
    assert_eq!(live["sentences"]["admitted"], 1, "{live}");
    assert_eq!(live["sentences"]["excluded_by_predicate"], 2, "{live}");

    // The clause means the same thing over the prepared artifact: the tags
    // survived into the artifact, which is why they live in active_labels
    // rather than paragraph metadata.
    let prepared = bccwj_section_scope("@section = speech", &root, &artifact, &dictionary, true);
    assert_eq!(prepared["sentences"]["admitted"], 1, "{prepared}");
    assert_eq!(
        prepared["sentences"]["excluded_by_predicate"], 2,
        "{prepared}"
    );
}

/// Builds a two-document Markdown tree whose second document carries `trailing`
/// after its last sentence, and returns the root.
///
/// Two documents, not one: the diagnostics below claim to name *which* file
/// failed, and a single-document tree cannot tell a real lookup from a constant.
fn markdown_tree(directory: &Path, trailing: &str) -> PathBuf {
    let root = directory.join("tree");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("00001.md"), "猫。\n犬。\n").unwrap();
    fs::write(root.join("00002.md"), format!("鳥。\n{trailing}")).unwrap();
    root
}

fn build_tree(root: &Path, output: &Path, extra: &[&str]) -> Output {
    let dictionary = MiniDictionary::new();
    lingrep()
        .args(["corpus", "build", "--input-format", "markdown"])
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--output")
        .arg(output)
        .args(extra)
        .arg(root)
        .output()
        .unwrap()
}

/// A reader has a line number and nothing else — it was handed one stream,
/// not a tree. The build path has no execution layer to supply the rest, so
/// without the document name this failure would arrive as a bare `line 3:`.
#[test]
fn a_strict_build_names_the_document_whose_div_never_closed() {
    let directory = TempDir::new().unwrap();
    let output = build_tree(
        &markdown_tree(directory.path(), "::: {.notes}\n魚。\n"),
        &directory.path().join("out.lingcorpus"),
        &[],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("unclosed pandoc div"),
        "the refusal must say what failed: {stderr}"
    );
    assert!(
        stderr.contains("00002.md"),
        "the refusal must name the document that failed: {stderr}"
    );
}

/// The same tree builds when the caller opts into recovery. Before the flag
/// existed there was nothing to pass: `lingrep-web` recovered and the CLI
/// rejected the tree, and one unclosed div in one file made a whole tree
/// unbuildable.
#[test]
fn recovery_admits_a_tree_a_strict_read_refuses() {
    let directory = TempDir::new().unwrap();
    let root = markdown_tree(directory.path(), "::: {.notes}\n魚。\n");
    let output = build_tree(
        &root,
        &directory.path().join("recovered.lingcorpus"),
        &["--md-recover-conversion-artifacts"],
    );

    assert!(
        output.status.success(),
        "recovery must admit what the strict read refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(directory.path().join("recovered.lingcorpus").is_dir());
}

/// Recovery reaches `reader_tag`, so the two reads name two artifacts rather
/// than one serving the other's cache. That is the mechanism working, and it is
/// what makes exposing the flag safe for every artifact already on disk.
#[test]
fn a_recovered_read_is_a_different_corpus_from_a_strict_one() {
    let directory = TempDir::new().unwrap();
    let root = markdown_tree(directory.path(), "");
    let strict = directory.path().join("strict.lingcorpus");
    let recovered = directory.path().join("recovered.lingcorpus");

    assert!(build_tree(&root, &strict, &[]).status.success());
    assert!(
        build_tree(&root, &recovered, &["--md-recover-conversion-artifacts"])
            .status
            .success()
    );

    let manifest = |artifact: &Path| -> Value {
        serde_json::from_str(&fs::read_to_string(artifact.join("manifest.json")).unwrap()).unwrap()
    };
    let (strict, recovered) = (manifest(&strict), manifest(&recovered));

    assert_ne!(
        strict["reader_identity"], recovered["reader_identity"],
        "recovery must reach the reader identity"
    );
    assert_ne!(
        strict["analysis_identity"], recovered["analysis_identity"],
        "and thence the analysis identity, or one read serves the other's cache"
    );
    // The difference is the reader and nothing else: same bytes, same
    // admission. Without this the assertions above would pass on any
    // accidental difference between the two runs.
    assert_eq!(strict["source_identity"], recovered["source_identity"]);
    assert_eq!(
        strict["admission_identity"],
        recovered["admission_identity"]
    );
}

fn manifest_of(artifact: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(artifact.join("manifest.json")).unwrap()).unwrap()
}

#[test]
fn a_legacy_truncated_artifact_is_never_reused_as_complete() {
    let directory = TempDir::new().unwrap();
    let root = markdown_tree(directory.path(), "猫。\n");
    let build = |extra: &[&str]| {
        let dictionary = MiniDictionary::new();
        lingrep()
            .args([
                "corpus",
                "build",
                "--input-format",
                "markdown",
                "--dict-path",
            ])
            .arg(dictionary.path())
            .args(extra)
            .arg(&root)
            .output()
            .unwrap()
    };
    assert!(build(&[]).status.success());
    let artifact = fs::read_dir(directory.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "lingcorpus"))
        .unwrap();
    let mut manifest = manifest_of(&artifact);
    manifest["sentence_truncation"] = serde_json::json!({"count": 1, "limit": 128});
    fs::write(
        artifact.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let query = lingrep()
        .args(["--json", "matches", "--pattern", "[].", "--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(!query.status.success());
    assert!(String::from_utf8_lossy(&query.stderr).contains("truncated sentence"));
    let inspection = lingrep()
        .args(["corpus", "inspect"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(inspection.status.success());
    for extra in [&[][..], &["--max-sentence-bytes", "128"][..]] {
        let output = build(extra);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("rebuild the complete corpus"));
    }
}

#[test]
fn complete_builds_do_not_record_truncation() {
    let directory = TempDir::new().unwrap();
    let artifact = directory.path().join("clean.lingcorpus");
    assert!(
        build_tree(&markdown_tree(directory.path(), ""), &artifact, &[])
            .status
            .success()
    );

    assert_eq!(manifest_of(&artifact)["sentence_truncation"], Value::Null);
}

/// Writes `files` into a fresh corpus root and builds a prepared artifact from
/// `input`, a path relative to that root.
fn prepared_artifact(
    directory: &Path,
    name: &str,
    files: &[(&str, String)],
    input: &str,
) -> PathBuf {
    let dictionary = MiniDictionary::new();
    let root = directory.join(name);
    fs::create_dir_all(&root).unwrap();
    for (file, contents) in files {
        fs::write(root.join(file), contents).unwrap();
    }
    let artifact = directory.join(format!("{name}.lingcorpus"));
    let build = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(root.join(input))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    artifact
}

/// Pins which sentences each clause admits from a prepared artifact, as
/// complete records: every admitted sentence's source, corpus and document
/// ordinals and text, and the whole scope accounting.
///
/// In the Markdown artifact `sections.md` carries `year` and three labelled
/// sections and `bare.md` has no frontmatter, so every `@document` predicate
/// meets a missing field there. In the JSONL artifact the first document
/// carries `year`, `tags` and two authored sections, and the second carries
/// only `year`.
#[test]
fn a_prepared_artifact_admits_the_pinned_sentences_for_each_clause() {
    let directory = tempfile::tempdir().unwrap();
    let markdown = prepared_artifact(
        directory.path(),
        "markdown",
        &[
            (
                "sections.md",
                "---\nyear: 2020\n---\n\n\
                 # Methods {.methods}\n\n猫が鳥を見る。\n犬も鳥を見る。\n\n\
                 # Discussion {.discussion}\n\n一と二と三。\n\n\
                 # Results {.results}\n\n猫は犬である。\n"
                    .to_owned(),
            ),
            ("bare.md", "犬が鳥を見る。\n".to_owned()),
        ],
        ".",
    );
    let documents = [
        serde_json::json!({
            "metadata": {"year": 2005, "tags": ["a", "b"]},
            "paragraphs": [
                {"sentences": [{"text": "猫。"}, {"text": "犬。"}]},
                {"sentences": [{"text": "鳥。"}]}
            ],
            "structure": {"observed_kinds": ["section"], "divisions": [
                {"id": "m", "kind": "section", "start": 0, "end": 2, "metadata": {"role": "methods"}},
                {"id": "r", "kind": "section", "start": 2, "end": 3, "metadata": {"role": "results"}}
            ]}
        }),
        serde_json::json!({
            "metadata": {"year": 2020},
            "paragraphs": [{"sentences": [{"text": "魚。"}]}],
            "structure": {"observed_kinds": ["section"], "divisions": []}
        }),
    ];
    let jsonl = prepared_artifact(
        directory.path(),
        "jsonl",
        &[(
            "structure.jsonl",
            documents
                .iter()
                .map(|document| format!("{document}\n"))
                .collect(),
        )],
        "structure.jsonl",
    );
    for (artifact, clause, expected_sentences, expected_scope) in [
        (
            &markdown,
            "@document.year >= 2010",
            vec![
                ("sections.md", 1, 0, "猫が鳥を見る。"),
                ("sections.md", 2, 1, "犬も鳥を見る。"),
                ("sections.md", 3, 2, "一と二と三。"),
                ("sections.md", 4, 3, "猫は犬である。"),
            ],
            r#"{"basis":"corpus","clause":"@document.year >= 2010","documents":{"admitted":1,"excluded_by_predicate":0,"excluded_missing_field":1,"excluded_shape_mismatch":0},"sentences":{"admitted":4,"excluded_by_predicate":0,"excluded_missing_field":1,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &markdown,
            "@document.year != 2020",
            vec![],
            r#"{"basis":"corpus","clause":"@document.year != 2020","documents":{"admitted":0,"excluded_by_predicate":1,"excluded_missing_field":1,"excluded_shape_mismatch":0},"sentences":{"admitted":0,"excluded_by_predicate":4,"excluded_missing_field":1,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &markdown,
            "@section = methods",
            vec![
                ("sections.md", 1, 0, "猫が鳥を見る。"),
                ("sections.md", 2, 1, "犬も鳥を見る。"),
            ],
            r#"{"basis":"corpus","clause":"@section = methods","documents":{"admitted":2,"excluded_by_predicate":0,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":2,"excluded_by_predicate":3,"excluded_missing_field":0,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &markdown,
            "@section != methods",
            vec![
                ("bare.md", 0, 0, "犬が鳥を見る。"),
                ("sections.md", 3, 2, "一と二と三。"),
                ("sections.md", 4, 3, "猫は犬である。"),
            ],
            r#"{"basis":"corpus","clause":"@section != methods","documents":{"admitted":2,"excluded_by_predicate":0,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":3,"excluded_by_predicate":2,"excluded_missing_field":0,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &markdown,
            "@section in {methods, results} & @document.year >= 2010",
            vec![
                ("sections.md", 1, 0, "猫が鳥を見る。"),
                ("sections.md", 2, 1, "犬も鳥を見る。"),
                ("sections.md", 4, 3, "猫は犬である。"),
            ],
            r#"{"basis":"corpus","clause":"@section in {methods, results} & @document.year >= 2010","documents":{"admitted":1,"excluded_by_predicate":0,"excluded_missing_field":1,"excluded_shape_mismatch":0},"sentences":{"admitted":3,"excluded_by_predicate":1,"excluded_missing_field":1,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &markdown,
            "(@document.year = 2005 | @section = results)",
            vec![("sections.md", 4, 3, "猫は犬である。")],
            r#"{"basis":"corpus","clause":"(@document.year = 2005 | @section = results)","documents":{"admitted":2,"excluded_by_predicate":0,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":1,"excluded_by_predicate":3,"excluded_missing_field":1,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &jsonl,
            "@document.year < 2010",
            vec![
                ("structure.jsonl line 1", 0, 0, "猫。"),
                ("structure.jsonl line 1", 1, 1, "犬。"),
                ("structure.jsonl line 1", 2, 2, "鳥。"),
            ],
            r#"{"basis":"corpus","clause":"@document.year < 2010","documents":{"admitted":1,"excluded_by_predicate":1,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":3,"excluded_by_predicate":1,"excluded_missing_field":0,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &jsonl,
            "@document.tags contains a",
            vec![
                ("structure.jsonl line 1", 0, 0, "猫。"),
                ("structure.jsonl line 1", 1, 1, "犬。"),
                ("structure.jsonl line 1", 2, 2, "鳥。"),
            ],
            r#"{"basis":"corpus","clause":"@document.tags contains a","documents":{"admitted":1,"excluded_by_predicate":0,"excluded_missing_field":1,"excluded_shape_mismatch":0},"sentences":{"admitted":3,"excluded_by_predicate":0,"excluded_missing_field":1,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &jsonl,
            "@document.tags not contains a",
            vec![],
            r#"{"basis":"corpus","clause":"@document.tags not contains a","documents":{"admitted":0,"excluded_by_predicate":1,"excluded_missing_field":1,"excluded_shape_mismatch":0},"sentences":{"admitted":0,"excluded_by_predicate":3,"excluded_missing_field":1,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &jsonl,
            "@document.year in {2005, 2020}",
            vec![
                ("structure.jsonl line 1", 0, 0, "猫。"),
                ("structure.jsonl line 1", 1, 1, "犬。"),
                ("structure.jsonl line 1", 2, 2, "鳥。"),
                ("structure.jsonl line 2", 3, 0, "魚。"),
            ],
            r#"{"basis":"corpus","clause":"@document.year in {2005, 2020}","documents":{"admitted":2,"excluded_by_predicate":0,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":4,"excluded_by_predicate":0,"excluded_missing_field":0,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &jsonl,
            "within(section, role=methods)",
            vec![
                ("structure.jsonl line 1", 0, 0, "猫。"),
                ("structure.jsonl line 1", 1, 1, "犬。"),
            ],
            r#"{"basis":"corpus","clause":"within(section, role=methods)","documents":{"admitted":2,"excluded_by_predicate":0,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":2,"excluded_by_predicate":2,"excluded_missing_field":0,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &jsonl,
            "not within(section, role=methods)",
            vec![
                ("structure.jsonl line 1", 2, 2, "鳥。"),
                ("structure.jsonl line 2", 3, 0, "魚。"),
            ],
            r#"{"basis":"corpus","clause":"not within(section, role=methods)","documents":{"admitted":2,"excluded_by_predicate":0,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":2,"excluded_by_predicate":2,"excluded_missing_field":0,"excluded_shape_mismatch":0}}"#,
        ),
        (
            &jsonl,
            "within(section, role=results) & @document.year = 2005",
            vec![("structure.jsonl line 1", 2, 2, "鳥。")],
            r#"{"basis":"corpus","clause":"within(section, role=results) & @document.year = 2005","documents":{"admitted":1,"excluded_by_predicate":1,"excluded_missing_field":0,"excluded_shape_mismatch":0},"sentences":{"admitted":1,"excluded_by_predicate":3,"excluded_missing_field":0,"excluded_shape_mismatch":0}}"#,
        ),
    ] {
        let output = lingrep_without_dictionary()
            .arg("--json")
            .args(["matches", "--pattern", "p := ^ []+ $.", "--limit", "all"])
            .args(["--where", clause, "--corpus"])
            .arg(artifact)
            .output()
            .unwrap();
        let scope = scope_of(&output);
        let admitted: Vec<(String, u64, u64, String)> = parse_jsonl(&output.stdout)
            .into_iter()
            .filter(|record| record["type"] == "match")
            .map(|record| {
                let data = &record["data"];
                (
                    Path::new(data["source_name"].as_str().unwrap())
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    data["corpus_sentence_ordinal"].as_u64().unwrap(),
                    data["document_sentence_ordinal"].as_u64().unwrap(),
                    data["surface"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let expected_sentences: Vec<(String, u64, u64, String)> = expected_sentences
            .into_iter()
            .map(|(source, corpus, document, text)| {
                (source.to_owned(), corpus, document, text.to_owned())
            })
            .collect();
        assert_eq!(admitted, expected_sentences, "{clause}");
        assert_eq!(
            scope,
            serde_json::from_str::<Value>(expected_scope).unwrap(),
            "{clause}"
        );
    }
}
