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

use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::Path;
use std::process::{Child, Output, Stdio};

use lingrep::direct_source::{
    BoundedRecordError, BoundedRecordReader, HashingReader, OrderedInventoryObservation,
    StdinObservation, matched_orth_tokens, matched_surface, project_context, source_from_events,
    validate_ordered_inventory,
};
use lingrep::protocol::validate_query_jsonl;
use lingrep_core::corpus::{AdmittedCorpusEvent, AdmittedSentence, DocumentId, SentenceId};
use lingrep_core::execution::{
    ExecutionError, ExecutionLimits as MatcherLimits, SentenceBatchSource, SentenceLocation,
};
use lingrep_core::testing::MiniDictionary;
use lingrep_core::testing::token;
use lingrep_core::token_match::TokenInfo;
use lingrep_corpus::SourceObservation;
use serde_json::Value;
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr as _;

use crate::support::{event_types, lingrep, lingrep_std, parse_jsonl};

fn orths(tokens: &[TokenInfo]) -> Vec<&str> {
    tokens.iter().map(|token| token.orth().unwrap()).collect()
}

fn assert_post_run_diagnostic(output: &Output, code: &str, stage: &str) -> Value {
    let events = parse_jsonl(&output.stdout);
    let diagnostic = parse_jsonl(&output.stderr);
    assert_eq!(
        diagnostic.len(),
        1,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(diagnostic[0]["data"]["code"], code);
    assert_eq!(diagnostic[0]["data"]["stage"], stage);
    assert_eq!(
        diagnostic[0]["data"]["run_id"],
        events
            .iter()
            .find(|event| event["type"] == "run")
            .expect("post-run failure emitted run")["data"]["run_id"]
    );
    diagnostic.into_iter().next().unwrap()
}

fn run_direct(root: &std::path::Path, extra: &[&str]) -> Output {
    run_direct_with_pattern(root, "[].", extra)
}

fn run_direct_with_pattern(root: &std::path::Path, pattern: &str, extra: &[&str]) -> Output {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep();
    if !extra.contains(&"--tsv") {
        command.arg("--json");
    }
    command
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg(pattern);
    command.args(extra).arg(root).output().unwrap()
}

fn run_human_with_pattern(root: &std::path::Path, pattern: &str, extra: &[&str]) -> Output {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep();
    command
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg(pattern);
    command.args(extra).arg(root).output().unwrap()
}

fn delimiter_free_line(bytes: usize) -> String {
    let mut line = "関".repeat(bytes / "関".len());
    line.push_str(&"x".repeat(bytes % "関".len()));
    assert_eq!(line.len(), bytes);
    line
}

fn match_events(output: &Output) -> Vec<Value> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| event["type"] == "match")
        .collect()
}

fn run_event(output: &Output) -> Value {
    parse_jsonl(&output.stdout)
        .into_iter()
        .find(|event| event["type"] == "run")
        .expect("every run publishes a run record")
}

/// The offsets address the sentence's own text, and the run says whether it
/// has any.
///
/// The slice-equals-surface assertion is the whole feature stated as one
/// property, and it is asserted against the sentence text rather than against
/// `surface`: `surface` concatenates token text with no separator, so for any
/// space-separated language it is *not* the original substring and comparing
/// against it would pass while the offsets were wrong.
#[test]
fn char_offsets_address_the_sentence_text_and_the_run_declares_them() {
    const SENTENCE: &str = "\u{5148}\u{751f}\u{306e}\u{7406}\u{60f3}\u{306f}\u{9650}\u{308a}\u{306a}\u{304f}\u{9ad8}\u{3044}\u{3002}";
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("ja_gsd-ud-test.conllu");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ud-ja-gsd-sample.conllu"),
        &input,
    )
    .unwrap();
    let artifact = directory.path().join("ja-gsd.lingcorpus");
    let pattern = "pattern nominal := (?<n> [upos=NOUN]) (?<c> [upos=ADP]).";

    let direct = lingrep()
        .args(["--json", "matches", "--pattern", pattern, "--limit", "all"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert_eq!(
        run_event(&direct)["data"]["char_offsets"],
        Value::Bool(true),
        "a direct CoNLL-U run derives offsets and must say so"
    );

    let text: Vec<char> = SENTENCE.chars().collect();
    let slice = |span: &Value| -> String {
        let start = span["start"].as_u64().expect("start is a number") as usize;
        let end = span["end"].as_u64().expect("end is a number") as usize;
        text[start..end].iter().collect()
    };
    let matches = match_events(&direct);
    assert!(!matches.is_empty(), "the fixture matches this pattern");
    for event in &matches {
        let data = &event["data"];
        assert_eq!(
            slice(&data["char_span"]),
            data["surface"].as_str().expect("surface is a string"),
            "the record's offsets must select the record's own text"
        );
        for capture in data["captures"].as_array().expect("captures is an array") {
            let span = &capture["char_span"];
            assert!(
                !span.is_null(),
                "a record with offsets gives every surviving capture one"
            );
            // One token each, so the capture's slice is that token's surface.
            assert!(SENTENCE.contains(&slice(span)));
        }
    }

    let built = lingrep()
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
    let prepared = lingrep()
        .args([
            "--json",
            "matches",
            "--pattern",
            pattern,
            "--limit",
            "all",
            "--corpus",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert_eq!(
        run_event(&prepared)["data"]["char_offsets"],
        Value::Bool(true)
    );
    assert_eq!(match_events(&prepared), match_events(&direct));
}

#[test]
fn conllu_queries_dependencies_without_analyzer_and_prepares_the_same_tokens() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("ja_gsd-ud-test.conllu");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ud-ja-gsd-sample.conllu"),
        &input,
    )
    .unwrap();
    let artifact = directory.path().join("ja-gsd.lingcorpus");
    let pattern = "pattern root := [dep.head=0 & dep.rel=root & upos=ADJ].";

    let direct = lingrep()
        .args(["--json", "matches", "--pattern", pattern, "--limit", "all"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let direct_matches = match_events(&direct);
    assert_eq!(direct_matches.len(), 1);
    assert_eq!(direct_matches[0]["data"]["surface"], "高い");

    let built = lingrep()
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
    let prepared = lingrep()
        .args([
            "--json",
            "matches",
            "--pattern",
            pattern,
            "--limit",
            "all",
            "--corpus",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert_eq!(match_events(&prepared), direct_matches);

    let summarized = lingrep()
        .args(["--json", "summarize", "--pattern", pattern, "--top", "all"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        summarized.status.success(),
        "{}",
        String::from_utf8_lossy(&summarized.stderr)
    );
    let summaries = parse_jsonl(&summarized.stdout)
        .into_iter()
        .filter(|event| event["type"] == "summary")
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["data"]["count"], 1);
}

#[test]
fn enhanced_graph_assignments_are_identical_direct_and_prepared() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("enhanced.conllu");
    std::fs::write(
        &input,
        concat!(
            "# sent_id = enhanced-1\n",
            "# text = They do\n",
            "1\tThey\tthey\tPRON\tPRP\t_\t2\tnsubj\t2:nsubj|2.1:nsubj\t_\n",
            "2\tdo\tdo\tVERB\tVBP\t_\t0\troot\t0:root\t_\n",
            "2.1\tdid\tdo\tVERB\tVBD\t_\t_\t_\t0:root\t_\n\n",
        ),
    )
    .unwrap();
    let artifact = directory.path().join("enhanced.lingcorpus");
    let pattern = concat!(
        "key: [$predicate.node.id, $role.rel, $argument.lemma]\n",
        "pattern elided := (?<argument> [upos=PRON]) ",
        "where (?<predicate> [node.kind=empty]) ",
        "-[enhanced nsubj as $role]-> $argument.\n",
    );

    let direct = lingrep()
        .args(["--json", "matches", "--pattern", pattern, "--limit", "all"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );

    let built = lingrep()
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
    let prepared = lingrep()
        .args([
            "--json",
            "matches",
            "--pattern",
            pattern,
            "--limit",
            "all",
            "--corpus",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );

    let direct_matches = match_events(&direct);
    assert_eq!(match_events(&prepared), direct_matches);
    assert_eq!(direct_matches.len(), 1);
    let data = &direct_matches[0]["data"];
    assert_eq!(data["surface"], "They");
    assert_eq!(data["graph_bindings"][0]["name"], "predicate");
    assert_eq!(data["graph_bindings"][0]["node"]["node_kind"], "empty");
    assert_eq!(data["graph_bindings"][0]["node"]["boundary"], 2);
    assert_eq!(data["graph_bindings"][0]["node"]["subindex"], 1);
    assert_eq!(data["graph_bindings"][1]["name"], "role");
    assert_eq!(data["graph_bindings"][1]["relation"], "nsubj");
    assert_eq!(data["key"][0]["value"], "2.1");
}

#[test]
fn where_clause_catalog_runs_direct_and_prepared_identically() {
    // The tracked exemplar catalog over the pinned GSD sentence
    // 先生の理想は限りなく高い。
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("ja_gsd-ud-test.conllu");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ud-ja-gsd-sample.conllu"),
        &input,
    )
    .unwrap();
    let catalog = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("patterns/ud-dependency.lx");
    let artifact = directory.path().join("ja-gsd.lingcorpus");

    let direct = lingrep()
        .args(["--json", "matches", "--limit", "all", "--patterns"])
        .arg(&catalog)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let direct_matches = match_events(&direct);
    let summary: Vec<(String, String)> = direct_matches
        .iter()
        .map(|event| {
            (
                event["data"]["pattern"].as_str().unwrap().to_owned(),
                event["data"]["surface"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("case_marked_nominal", "先生の"),
            ("genitive_modifier", "先生の理想"),
            ("case_marked_nominal", "理想は"),
            ("adjectival_predicate_subject", "理想は限りなく高い"),
        ]
        .map(|(pattern, surface)| (pattern.to_owned(), surface.to_owned())),
        "the edge-constrained witnesses, in canonical (start, end, pattern) order"
    );

    let built = lingrep()
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
    let prepared = lingrep()
        .args(["--json", "matches", "--limit", "all", "--patterns"])
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert_eq!(match_events(&prepared), direct_matches);
}

#[test]
fn conllu_sentence_identity_scopes_direct_and_prepared_identically() {
    // Three sentences over two `# newdoc` documents, each with a `# sent_id`:
    // `--where 'sent_id = …'` selects one cited sentence at sentence grain,
    // and `--where 'newdoc_id = …'` selects one document as ordinary document
    // metadata. Both paths must publish the same sentences for each clause.
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("identity.conllu");
    let sentence = |sent_id: &str, noun: &str, comments: &str| {
        format!(
            "{comments}# sent_id = {sent_id}\n# text = {noun}がいる。\n\
             1\t{noun}\t{noun}\tNOUN\t名詞\t_\t3\tnsubj\t_\tSpaceAfter=No\n\
             2\tが\tが\tADP\t助詞\t_\t1\tcase\t_\tSpaceAfter=No\n\
             3\tいる\tいる\tVERB\t動詞\t_\t0\troot\t_\tSpaceAfter=No\n\
             4\t。\t。\tPUNCT\t補助記号\t_\t3\tpunct\t_\tSpaceAfter=No\n\n"
        )
    };
    std::fs::write(
        &input,
        [
            sentence("s1", "猫", "# newdoc id = doc-1\n"),
            sentence("s2", "犬", ""),
            sentence("s3", "鳥", "# newdoc id = doc-2\n"),
        ]
        .concat(),
    )
    .unwrap();
    let artifact = directory.path().join("identity.lingcorpus");
    let built = lingrep()
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

    let pattern = "pattern noun := [upos=NOUN].";
    let surfaces = |output: &Output| {
        match_events(output)
            .iter()
            .map(|event| event["data"]["surface"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    for (clause, expected) in [
        ("@sentence.sent_id = s2", vec!["犬"]),
        ("@sentence.sent_id in {s1, s3}", vec!["猫", "鳥"]),
        ("@document.newdoc_id = doc-2", vec!["鳥"]),
        (
            "@document.newdoc_id = doc-1 & @sentence.sent_id = s2",
            vec!["犬"],
        ),
    ] {
        let direct = lingrep()
            .args([
                "--json",
                "matches",
                "--pattern",
                pattern,
                "--limit",
                "all",
                "--where",
                clause,
            ])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            direct.status.success(),
            "`{clause}` direct: {}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(surfaces(&direct), expected, "`{clause}` direct");

        let prepared = lingrep()
            .args([
                "--json",
                "matches",
                "--pattern",
                pattern,
                "--limit",
                "all",
                "--where",
                clause,
                "--corpus",
            ])
            .arg(&artifact)
            .output()
            .unwrap();
        assert!(
            prepared.status.success(),
            "`{clause}` prepared: {}",
            String::from_utf8_lossy(&prepared.stderr)
        );
        assert_eq!(surfaces(&prepared), expected, "`{clause}` prepared");
    }
}

#[test]
fn conllu_treebank_directory_is_discovered_recursively_for_direct_and_prepared_queries() {
    let directory = TempDir::new().unwrap();
    let treebank = directory.path().join("UD_Japanese-GSD");
    std::fs::create_dir_all(treebank.join("split")).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ud-ja-gsd-sample.conllu");
    std::fs::copy(&fixture, treebank.join("ja_gsd-ud-dev.conllu")).unwrap();
    std::fs::copy(&fixture, treebank.join("split/ja_gsd-ud-test.CONLLU")).unwrap();
    std::fs::write(treebank.join("README.md"), "not a CoNLL-U document").unwrap();
    let artifact = directory.path().join("ja-gsd.lingcorpus");
    let pattern = "pattern root := [dep.head=0 & dep.rel=root & upos=ADJ].";

    let direct = lingrep()
        .args([
            "--json",
            "matches",
            "--input-format",
            "conllu",
            "--pattern",
            pattern,
            "--limit",
            "all",
        ])
        .arg(&treebank)
        .output()
        .unwrap();
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let direct_matches = match_events(&direct);
    assert_eq!(direct_matches.len(), 2);
    assert_eq!(
        direct_matches[0]["data"]["source_name"],
        "ja_gsd-ud-dev.conllu"
    );
    assert_eq!(
        direct_matches[1]["data"]["source_name"],
        "split/ja_gsd-ud-test.CONLLU"
    );

    let built = lingrep()
        .args(["corpus", "build", "--input-format", "conllu", "--output"])
        .arg(&artifact)
        .arg(&treebank)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let prepared = lingrep()
        .args([
            "--json",
            "matches",
            "--pattern",
            pattern,
            "--limit",
            "all",
            "--corpus",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert_eq!(match_events(&prepared), direct_matches);
}

/// A catalog referencing an attribute this corpus's token schema does not
/// carry runs to completion, publishes its zero matches, and outputs a warning to
/// stderr — on both execution paths.
#[test]
fn an_unavailable_attribute_warns_and_the_run_still_completes() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("sample.conllu");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ud-ja-gsd-sample.conllu"),
        &input,
    )
    .unwrap();
    let pattern = "pattern p := [unidic.pos1=名詞].";

    let assert_warned_zero_match_run = |output: &Output| {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("warning[attribute_unavailable]:"),
            "{stderr}"
        );
        assert!(
            stderr.contains("`unidic.pos1` is referenced by `p` (inline.lx:1)"),
            "{stderr}"
        );
        assert!(stderr.contains("(pretokenized syntax)"), "{stderr}");
        assert_eq!(match_events(output).len(), 0);
    };

    let direct = lingrep()
        .args(["--json", "matches", "--pattern", pattern, "--limit", "all"])
        .arg(&input)
        .output()
        .unwrap();
    assert_warned_zero_match_run(&direct);

    let artifact = directory.path().join("sample.lingcorpus");
    let built = lingrep()
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
    let prepared = lingrep()
        .args([
            "--json",
            "matches",
            "--pattern",
            pattern,
            "--limit",
            "all",
            "--corpus",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert_warned_zero_match_run(&prepared);
}

/// The one absence that stays an error, framed as what it is: a dependency
/// `where` clause cannot mean anything on a morphology-only corpus, and the
/// refusal is an invalid invocation (exit 2), not a source failure.
#[test]
fn a_where_catalog_on_a_morphology_corpus_is_refused_as_invalid_invocation() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("input.txt");
    std::fs::write(&input, "猫が走る。\n").unwrap();

    let output = run_direct_with_pattern(
        &input,
        "pattern p := (?<s> [text=猫]) (?<v> [text=走る]) where $v -[nsubj]-> $s.",
        &[],
    );

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot bind to this corpus's token schema (vibrato morphology)"),
        "{stderr}"
    );
    assert!(stderr.contains("dep.head, dep.rel"), "{stderr}");
    assert!(
        !stderr.contains("source failed before sentence"),
        "the refusal must not be framed as a read failure: {stderr}"
    );
}

#[test]
#[ignore = "requires the fixed Sudachi core dictionary and resources"]
fn sudachi_portable_catalog_publishes_nonzero_context_without_unidic_fields() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("input.txt");
    std::fs::write(&input, "猫が走る。\n").unwrap();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let output = lingrep()
        .arg("--json")
        .arg("matches")
        .arg("--analyzer")
        .arg("sudachi")
        .arg("--sudachi-dict")
        .arg(std::env::var_os("LINGREP_SUDACHI_DICT_CORE").unwrap())
        .arg("--sudachi-resources")
        .arg(std::env::var_os("LINGREP_SUDACHI_RESOURCES").unwrap())
        .arg("--sudachi-split")
        .arg("c")
        .arg("--profile")
        .arg("morphology")
        .arg("--patterns")
        .arg(repository.join("patterns/portable-content-words.lx"))
        .arg("--context")
        .arg("1")
        .arg("--progress")
        .arg("never")
        .arg(&input)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let matches = match_events(&output);
    assert_eq!(matches.len(), 2);
    assert_eq!(
        matches[0]["data"]["right_context"][0]["attributes"]["text"],
        "が"
    );
    assert_eq!(
        matches[1]["data"]["left_context"][0]["attributes"]["text"],
        "が"
    );
}

/// A Sudachi artifact is queryable, and its records are the direct run's.
///
/// The end-to-end half of `AnalysisBinding`. A unit test can pin the
/// executor's contract, but not that the CLI hands it the artifact's schema
/// rather than a constant — which is the mistake that shipped: `corpus build
/// --analyzer sudachi` published a well-formed artifact that every read of
/// failed with `bound lingrep patterns schema does not match the analyzer schema`, because
/// a prepared run with no analyzer session assumed Vibrato's.
///
/// It also asserts stderr is silent. The `featurization_policy_mismatch`
/// notice compared every artifact against one global schema — Vibrato's — so
/// a Sudachi artifact this build had just written was reported as stale and
/// told to rebuild, which would have produced the identical artifact.
#[test]
#[ignore = "requires the fixed Sudachi core dictionary and resources"]
fn a_sudachi_artifact_is_queryable_and_agrees_with_the_direct_run() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("input.txt");
    std::fs::write(&input, "猫が走る。\n国立国語研究所で研究をしている。\n").unwrap();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let catalog = repository.join("patterns/portable-content-words.lx");
    let sudachi = |mut command: assert_cmd::Command| {
        command
            .arg("--analyzer")
            .arg("sudachi")
            .arg("--sudachi-dict")
            .arg(std::env::var_os("LINGREP_SUDACHI_DICT_CORE").unwrap())
            .arg("--sudachi-resources")
            .arg(std::env::var_os("LINGREP_SUDACHI_RESOURCES").unwrap())
            .arg("--sudachi-split")
            .arg("c")
            .arg("--progress")
            .arg("never")
            .output()
            .unwrap()
    };

    let artifact = directory.path().join("sudachi.lingcorpus");
    let mut build = lingrep();
    build
        .args(["corpus", "build", "--output"])
        .arg(&artifact)
        .arg(&input);
    let built = sudachi(build);
    assert!(
        built.status.success(),
        "corpus build --analyzer sudachi failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let mut direct = lingrep();
    direct
        .args(["--json", "matches", "--patterns"])
        .arg(&catalog)
        .args(["--limit", "all"])
        .arg(&input);
    let direct = sudachi(direct);
    let mut prepared = lingrep();
    prepared
        .args(["--json", "matches", "--patterns"])
        .arg(&catalog)
        .args(["--limit", "all", "--corpus"])
        .arg(&artifact);
    let prepared = sudachi(prepared);

    for (label, output) in [("direct", &direct), ("prepared", &prepared)] {
        assert!(
            output.status.success(),
            "the {label} run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let direct_matches = match_events(&direct);
    assert!(
        !direct_matches.is_empty(),
        "the fixture must produce matches"
    );
    assert_eq!(
        match_events(&prepared),
        direct_matches,
        "a prepared Sudachi run must publish exactly what the direct run does"
    );
    let stderr = String::from_utf8_lossy(&prepared.stderr);
    assert!(
        !stderr.contains("featurization_policy_mismatch"),
        "an artifact this build just wrote must draw no rebuild notice, got {stderr}"
    );
}

#[test]
fn markdown_matches_reports_two_omissions_after_publishing_later_evidence() {
    let directory = TempDir::new().unwrap();
    let root = directory.path().join("corpus");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("a.md"),
        format!("猫。\n{}\n", delimiter_free_line(187_965)),
    )
    .unwrap();
    std::fs::write(
        root.join("b.md"),
        format!("{}\n犬。\n", delimiter_free_line(187_560)),
    )
    .unwrap();

    let output = run_direct_with_pattern(
        &root,
        "[text in {猫, 犬}].",
        &["--max-sentence-bytes", "65536"],
    );

    assert_eq!(output.status.code(), Some(1));
    let matches = match_events(&output);
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["data"]["corpus_sentence_ordinal"], 0);
    assert_eq!(matches[1]["data"]["corpus_sentence_ordinal"], 3);
    assert!(
        !parse_jsonl(&output.stdout)
            .iter()
            .any(|event| event["type"] == "complete")
    );
    let diagnostic = assert_post_run_diagnostic(&output, "oversized_sentences_skipped", "input");
    assert_eq!(diagnostic["data"]["location"]["source_name"], "a.md");
    assert_eq!(diagnostic["data"]["location"]["corpus_sentence_ordinal"], 1);
    assert!(
        diagnostic["data"]["message"]
            .as_str()
            .unwrap()
            .contains("count=2")
    );
}

#[test]
fn human_and_tsv_publish_later_matches_before_the_degraded_exit() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("degraded.md");
    std::fs::write(&corpus, format!("{}\n猫。\n", delimiter_free_line(187_965))).unwrap();

    let human = run_human_with_pattern(
        &corpus,
        "[text=猫].",
        &[
            "--limit",
            "all",
            "--progress",
            "always",
            "--max-sentence-bytes",
            "65536",
        ],
    );
    assert_eq!(human.status.code(), Some(1));
    assert!(String::from_utf8(human.stdout).unwrap().contains('猫'));
    let human_stderr = String::from_utf8(human.stderr).unwrap();
    assert!(human_stderr.contains("matched 1 sentences, found 1 matches"));
    assert!(human_stderr.contains("count=1"));

    let tsv = run_direct_with_pattern(
        &corpus,
        "[text=猫].",
        &["--tsv", "--max-sentence-bytes", "65536"],
    );
    assert_eq!(tsv.status.code(), Some(1));
    assert!(String::from_utf8(tsv.stdout).unwrap().contains('猫'));
}

#[test]
fn non_markdown_matches_and_sibling_commands_keep_terminal_oversize_errors() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let oversized = delimiter_free_line(65_537);

    let jsonl = directory.path().join("oversized.jsonl");
    std::fs::write(
        &jsonl,
        format!(
            "{{\"sentences\":[{}]}}\n",
            serde_json::to_string(&oversized).unwrap()
        ),
    )
    .unwrap();
    let jsonl_output = run_direct(&jsonl, &["--max-sentence-bytes", "65536"]);

    let stdin_output = lingrep()
        .arg("--json")
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg("[].")
        .arg("--max-sentence-bytes")
        .arg("65536")
        .arg("-")
        .write_stdin(format!("{oversized}\n"))
        .output()
        .unwrap();

    let markdown = directory.path().join("oversized.md");
    std::fs::write(&markdown, format!("{oversized}\n")).unwrap();
    let summarize_output = lingrep()
        .arg("--json")
        .arg("summarize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg("[].")
        .arg("--max-sentence-bytes")
        .arg("65536")
        .arg(&markdown)
        .output()
        .unwrap();

    for output in [&jsonl_output, &stdin_output, &summarize_output] {
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = assert_post_run_diagnostic(output, "sentence_too_large", "input");
        assert_ne!(diagnostic["data"]["code"], "oversized_sentences_skipped");
    }
}

#[test]
fn scoped_out_markdown_oversize_is_not_an_omission() {
    let directory = TempDir::new().unwrap();
    let document_root = directory.path().join("documents");
    std::fs::create_dir(&document_root).unwrap();
    std::fs::write(
        document_root.join("a.md"),
        format!("---\nyear: 2000\n---\n{}\n", delimiter_free_line(187_965)),
    )
    .unwrap();
    std::fs::write(document_root.join("b.md"), "---\nyear: 2020\n---\n猫。\n").unwrap();
    let document = run_direct(
        &document_root,
        &[
            "--where",
            "@document.year >= 2010",
            "--max-sentence-bytes",
            "65536",
        ],
    );
    assert!(
        document.status.success(),
        "{}",
        String::from_utf8_lossy(&document.stderr)
    );

    let section = directory.path().join("sections.md");
    std::fs::write(
        &section,
        format!(
            "## Hidden {{.hidden}}\n{}\n## Methods {{.methods}}\n猫。\n",
            delimiter_free_line(187_965)
        ),
    )
    .unwrap();
    let section = run_direct(
        &section,
        &[
            "--where",
            "@section = methods",
            "--max-sentence-bytes",
            "65536",
        ],
    );
    assert!(
        section.status.success(),
        "{}",
        String::from_utf8_lossy(&section.stderr)
    );
}

#[test]
fn result_limit_distinguishes_unread_and_observed_omissions() {
    let directory = TempDir::new().unwrap();
    let unread = directory.path().join("unread.md");
    std::fs::write(&unread, format!("猫。\n{}\n", delimiter_free_line(187_965))).unwrap();
    let unread = run_direct_with_pattern(
        &unread,
        "[text=猫].",
        &[
            "--limit",
            "1",
            "--batch-sentences",
            "1",
            "--max-sentence-bytes",
            "65536",
        ],
    );
    assert!(
        unread.status.success(),
        "{}",
        String::from_utf8_lossy(&unread.stderr)
    );
    let unread_events = parse_jsonl(&unread.stdout);
    let complete = unread_events
        .iter()
        .find(|event| event["type"] == "complete")
        .unwrap();
    assert_eq!(complete["data"]["coverage"], "prefix");

    let observed = directory.path().join("observed.md");
    std::fs::write(
        &observed,
        format!("{}\n猫。\n", delimiter_free_line(187_965)),
    )
    .unwrap();
    let observed = run_direct_with_pattern(
        &observed,
        "[text=猫].",
        &[
            "--limit",
            "1",
            "--batch-sentences",
            "1",
            "--max-sentence-bytes",
            "65536",
        ],
    );
    assert_eq!(observed.status.code(), Some(1));
    assert_eq!(match_events(&observed).len(), 1);
    assert!(
        !parse_jsonl(&observed.stdout)
            .iter()
            .any(|event| event["type"] == "complete")
    );
    assert_post_run_diagnostic(&observed, "oversized_sentences_skipped", "input");
}

fn run_omission_capacity(count: usize) -> Output {
    let directory = TempDir::new().unwrap();
    let root = directory.path().join("corpus");
    std::fs::create_dir(&root).unwrap();
    let line = delimiter_free_line(65_537);
    for ordinal in 0..count {
        std::fs::write(root.join(format!("{ordinal:04}.md")), format!("{line}\n")).unwrap();
    }
    run_direct_with_pattern(&root, "[text=猫].", &["--max-sentence-bytes", "65536"])
}

#[test]
#[ignore = "capacity control run explicitly for recovery evidence"]
fn one_markdown_omission_capacity_control() {
    let output = run_omission_capacity(1);
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = assert_post_run_diagnostic(&output, "oversized_sentences_skipped", "input");
    assert!(
        diagnostic["data"]["message"]
            .as_str()
            .unwrap()
            .contains("count=1")
    );
}

#[test]
#[ignore = "capacity stress run explicitly for recovery evidence"]
fn one_thousand_markdown_omissions_retain_one_sample() {
    let output = run_omission_capacity(1_000);
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = assert_post_run_diagnostic(&output, "oversized_sentences_skipped", "input");
    assert!(
        diagnostic["data"]["message"]
            .as_str()
            .unwrap()
            .contains("count=1000")
    );
    assert_eq!(diagnostic["data"]["location"]["source_name"], "0000.md");
}

#[test]
fn human_matches_use_quiet_default_and_custom_morpheme_boundaries() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "私猫犬鳥。\n").unwrap();
    let pattern = "[text=猫] [text=犬].";

    let plain = run_human_with_pattern(&corpus, pattern, &["--color", "never", "--context", "1"]);
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert!(plain.contains("私 ⟦猫犬⟧ 鳥"), "{plain:?}");
    assert!(!plain.contains('│'));

    let separated = run_human_with_pattern(
        &corpus,
        pattern,
        &[
            "--color",
            "never",
            "--context",
            "1",
            "--morpheme-separator",
            "|",
        ],
    );
    assert!(
        separated.status.success(),
        "{}",
        String::from_utf8_lossy(&separated.stderr)
    );
    assert!(
        String::from_utf8(separated.stdout)
            .unwrap()
            .contains("⟦猫|犬⟧")
    );
}

#[test]
fn human_context_coloring_follows_depth_and_explicit_environment() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("colors.txt");
    std::fs::write(&corpus, "一二三私彼研究実験調査猫犬鳥文をがはも。\n").unwrap();
    let dictionary = MiniDictionary::new();

    let run = |context: &str,
               depth: &str,
               theme: &str,
               colorterm: Option<&str>,
               term: &str,
               colorfgbg: Option<&str>| {
        let mut command = lingrep();
        command
            .env_remove("NO_COLOR")
            .env_remove("COLORTERM")
            .env_remove("COLORFGBG")
            .env("TERM", term);
        if let Some(value) = colorterm {
            command.env("COLORTERM", value);
        }
        if let Some(value) = colorfgbg {
            command.env("COLORFGBG", value);
        }
        command
            .args(["--color", "always", "matches", "--dict-path"])
            .arg(dictionary.path())
            .args([
                "--pattern",
                "[text=猫].",
                "--context",
                context,
                "--context-color-depth",
                depth,
                "--context-color-theme",
                theme,
            ])
            .arg(&corpus)
            .output()
            .unwrap()
    };

    let truecolor = run(
        "8",
        "8",
        "dark",
        Some("truecolor"),
        "xterm-256color",
        Some("15;0"),
    );
    assert!(
        truecolor.status.success(),
        "{}",
        String::from_utf8_lossy(&truecolor.stderr)
    );
    let truecolor = String::from_utf8(truecolor.stdout).unwrap();
    let escape = |token| foreground_escape_before(&truecolor, token);
    assert_eq!(escape("調査"), "\x1b[38;2;32;208;176m");
    assert_eq!(escape("犬"), escape("調査"));
    assert_eq!(escape("実験"), "\x1b[38;2;255;255;48m");
    assert_eq!(escape("鳥"), escape("実験"));
    assert_eq!(escape("研究"), "\x1b[38;2;255;16;255m");
    assert_eq!(escape("文"), escape("研究"));
    assert_eq!(escape("彼"), "\x1b[38;2;224;192;16m");
    assert_eq!(escape("を"), escape("彼"));
    assert_eq!(escape("私"), "\x1b[38;2;144;255;224m");
    assert_eq!(escape("が"), escape("私"));
    assert_eq!(escape("三"), "\x1b[38;2;176;144;0m");
    assert_eq!(escape("は"), escape("三"));
    assert_eq!(escape("二"), escape("調査"));
    assert_eq!(escape("も"), escape("調査"));
    assert!(truecolor.contains("\x1b[1m猫"), "{truecolor:?}");
    assert!(!truecolor.contains('⟦'));
    assert!(!truecolor.contains('⟧'));

    let context_two = run(
        "2",
        "8",
        "dark",
        Some("truecolor"),
        "xterm-256color",
        Some("15;0"),
    );
    assert!(
        context_two.status.success(),
        "{}",
        String::from_utf8_lossy(&context_two.stderr)
    );
    let context_two = String::from_utf8(context_two.stdout).unwrap();
    assert_eq!(
        foreground_escape_before(&truecolor, "実験"),
        foreground_escape_before(&context_two, "実験")
    );

    let ansi256 = run("8", "8", "dark", None, "xterm-256color", Some("15;0"));
    assert!(
        ansi256.status.success(),
        "{}",
        String::from_utf8_lossy(&ansi256.stderr)
    );
    let ansi256 = String::from_utf8(ansi256.stdout).unwrap();
    for index in [43, 11, 13, 178, 122, 136] {
        assert!(
            ansi256.contains(&format!("\x1b[38;5;{index}m")),
            "{ansi256:?}"
        );
    }
    assert!(!ansi256.contains("\x1b[38;2;"), "{ansi256:?}");

    let light = run(
        "8",
        "8",
        "light",
        Some("truecolor"),
        "xterm-256color",
        Some("0;15"),
    );
    assert!(
        light.status.success(),
        "{}",
        String::from_utf8_lossy(&light.stderr)
    );
    let light = String::from_utf8(light.stdout).unwrap();
    assert!(light.contains("\x1b[38;2;160;80;144m"), "{light:?}");

    let auto = run("8", "8", "auto", Some("truecolor"), "xterm-256color", None);
    assert!(
        auto.status.success(),
        "{}",
        String::from_utf8_lossy(&auto.stderr)
    );
    let auto = String::from_utf8(auto.stdout).unwrap();
    assert!(auto.contains("\x1b[38;2;32;208;176m"), "{auto:?}");

    let depth_zero = run(
        "8",
        "0",
        "dark",
        Some("truecolor"),
        "xterm-256color",
        Some("15;0"),
    );
    assert!(
        depth_zero.status.success(),
        "{}",
        String::from_utf8_lossy(&depth_zero.stderr)
    );
    let depth_zero = String::from_utf8(depth_zero.stdout).unwrap();
    assert!(depth_zero.contains("\x1b[2m"), "{depth_zero:?}");
    assert!(!depth_zero.contains("\x1b[38;2;"), "{depth_zero:?}");
    assert!(!depth_zero.contains("\x1b[38;5;"), "{depth_zero:?}");
}

fn foreground_escape_before<'a>(output: &'a str, token: &str) -> &'a str {
    let token_offset = output.find(token).unwrap();
    let prefix = &output[..token_offset];
    let escape_offset = prefix.rfind("\x1b[38;").unwrap();
    let escape = &prefix[escape_offset..];
    &escape[..=escape.find('m').unwrap()]
}

#[test]
fn aligned_human_matches_share_one_hit_column() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("two.txt");
    std::fs::write(&corpus, "私猫犬鳥。猫犬鳥。\n").unwrap();

    let output = run_human_with_pattern(
        &corpus,
        "[text=猫] [text=犬].",
        &["--color", "never", "--context", "1", "--align-hits"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    let rows = output.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{output:?}");
    let offsets = rows
        .iter()
        .map(|row| {
            let key = row.find('猫').unwrap();
            row.get(..key)
                .expect("find returns a UTF-8 boundary")
                .width()
        })
        .collect::<Vec<_>>();
    assert_eq!(offsets[0], offsets[1], "{output:?}");
    assert!(rows.iter().all(|row| !row.ends_with(' ')));
}

#[test]
fn direct_dictionary_environment_is_fallback_with_explicit_override_and_typed_missing_error() {
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "猫。\n").unwrap();
    let fallback_dictionary = MiniDictionary::new();
    let explicit_dictionary = MiniDictionary::new();

    let fallback = lingrep()
        .env("LINGREP_DICTIONARY_PATH", fallback_dictionary.path())
        .args(["matches", "--pattern", "[]."])
        .arg(&corpus)
        .output()
        .unwrap();
    assert!(
        fallback.status.success(),
        "{}",
        String::from_utf8_lossy(&fallback.stderr)
    );

    let explicit = lingrep()
        .env(
            "LINGREP_DICTIONARY_PATH",
            directory.path().join("missing-env-dictionary"),
        )
        .args(["matches", "--dict-path"])
        .arg(explicit_dictionary.path())
        .args(["--pattern", "[]."])
        .arg(&corpus)
        .output()
        .unwrap();
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );

    let missing = lingrep()
        .env_remove("LINGREP_DICTIONARY_PATH")
        .args(["--json", "matches", "--pattern", "[]."])
        .arg(&corpus)
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());
    assert_eq!(
        parse_jsonl(&missing.stderr)[0]["data"]["code"],
        "invalid_invocation"
    );
}

#[test]
fn empty_read_buffers_do_not_claim_source_eof() {
    let stdin = StdinObservation::default();
    let mut stdin_reader = stdin.reader(Cursor::new(b"stdin"));
    assert_eq!(stdin_reader.read(&mut []).unwrap(), 0);
    assert_eq!(stdin.observed_content_digest(), None);
    let mut bytes = Vec::new();
    stdin_reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"stdin");
    assert!(stdin.observed_content_digest().is_some());

    let filesystem = SourceObservation::new();
    let mut filesystem_reader =
        HashingReader::new(Cursor::new(b"file"), filesystem.clone(), "a.md").unwrap();
    assert_eq!(filesystem_reader.read(&mut []).unwrap(), 0);
    let mut bytes = Vec::new();
    filesystem_reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"file");
    assert_eq!(filesystem.finish().unwrap().document_count(), 1);
}

#[test]
fn bounded_record_reader_rejects_unterminated_records_without_retaining_them() {
    for source_name in ["plain.txt", "document.md", "documents.jsonl"] {
        let input = vec![b'x'; 64];
        let reader = BufReader::with_capacity(3, Cursor::new(input));
        let (mut bounded, stats) = BoundedRecordReader::new(reader, 8, source_name);
        let mut exposed = String::new();

        let error = bounded.read_to_string(&mut exposed).unwrap_err();
        let error = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<BoundedRecordError>())
            .expect("typed bounded-record error");

        assert_eq!(error.source_name, source_name);
        assert_eq!(error.record_ordinal, 0);
        assert_eq!(error.actual_at_least, 9);
        assert_eq!(error.limit, 8);
        assert!(exposed.is_empty());
        assert!(stats.max_retained_bytes() <= 9);
    }
}

#[test]
fn bounded_record_reader_preserves_normal_multi_sentence_records() {
    let input = "猫。犬。\n鳥。\n";
    let reader = BufReader::with_capacity(2, Cursor::new(input.as_bytes()));
    let (mut bounded, stats) = BoundedRecordReader::new(reader, 16, "plain.txt");
    let mut output = String::new();

    bounded.read_to_string(&mut output).unwrap();

    assert_eq!(output, input);
    assert!(stats.max_retained_bytes() <= 17);
}

#[test]
fn completed_identity_requires_the_exact_ordered_logical_inventory_digest() {
    let observation = OrderedInventoryObservation::default();
    observation.observe("a.md").unwrap();
    let discovery = lingrep_core::corpus::DiscoveryReport {
        inventory: lingrep_core::corpus::InventoryStats {
            documents: 1,
            ..Default::default()
        },
        configured_limits: Default::default(),
        observed: Default::default(),
        ordered_inventory_digest: [0; 32],
    };

    assert!(matches!(
        validate_ordered_inventory(&observation, &discovery),
        Err(ExecutionError::InventoryMismatch)
    ));
}

#[test]
fn direct_formats_reject_oversized_physical_records_before_parsing() {
    let directory = TempDir::new().unwrap();
    let dictionary = MiniDictionary::new();
    let fixtures = [
        ("plain.txt", "x".repeat(64)),
        ("document.md", "x".repeat(64)),
        (
            "documents.jsonl",
            format!("{{\"sentences\":[\"{}\"]}}", "x".repeat(64)),
        ),
    ];

    for (name, contents) in fixtures {
        let path = directory.path().join(name);
        std::fs::write(&path, contents).unwrap();
        let output = lingrep()
            .arg("--json")
            .arg("matches")
            .arg("--dict-path")
            .arg(dictionary.path())
            .arg("--pattern")
            .arg("[].")
            .arg("--max-sentence-bytes")
            .arg("32")
            .arg("--max-record-bytes")
            .arg("32")
            .arg(&path)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1), "{name}");
        let diagnostic = parse_jsonl(&output.stderr);
        assert_eq!(
            diagnostic[0]["data"]["code"],
            "input_record_too_large",
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(diagnostic[0]["data"]["location"]["source_name"], name);
        assert_eq!(
            diagnostic[0]["data"]["location"]["corpus_sentence_ordinal"],
            0
        );
        assert_eq!(diagnostic[0]["data"]["stage"], "input");
        let events = parse_jsonl(&output.stdout);
        assert_eq!(diagnostic[0]["data"]["run_id"], events[0]["data"]["run_id"]);
        assert!(!event_types(&parse_jsonl(&output.stdout)).contains(&"complete"));
    }
}

#[test]
fn oversized_record_diagnostic_separates_physical_record_and_global_sentence_ordinals() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("records.txt");
    std::fs::write(&path, format!("猫。犬。\n{}", "x".repeat(64))).unwrap();

    let output = run_direct(
        &path,
        &[
            "--json",
            "--max-sentence-bytes",
            "32",
            "--max-record-bytes",
            "32",
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let diagnostic = assert_post_run_diagnostic(&output, "input_record_too_large", "input");
    assert_eq!(diagnostic["data"]["location"]["corpus_sentence_ordinal"], 2);
    assert!(
        diagnostic["data"]["message"]
            .as_str()
            .unwrap()
            .contains("input record 1")
    );
}

#[test]
fn direct_markdown_bounds_cumulative_frontmatter_by_the_raw_stage_limit() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("metadata.md");
    let mut contents = String::from("---\n");
    for _ in 0..20 {
        contents.push_str("# x\n");
    }
    contents.push_str("---\n本文。\n");
    std::fs::write(&path, contents).unwrap();

    let output = run_direct(
        &path,
        &[
            "--json",
            "--max-sentence-bytes",
            "64",
            "--batch-raw-bytes",
            "64",
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    let diagnostic = assert_post_run_diagnostic(&output, "input", "input");
    assert_eq!(diagnostic["data"]["location"]["source_name"], "metadata.md");
    assert_eq!(diagnostic["data"]["location"]["corpus_sentence_ordinal"], 0);
    assert!(
        diagnostic["data"]["message"]
            .as_str()
            .unwrap()
            .contains("frontmatter")
    );
    assert!(!event_types(&parse_jsonl(&output.stdout)).contains(&"complete"));
}

#[test]
fn context_is_bounded_and_does_not_change_the_match() {
    let tokens = ["左2", "左1", "鍵", "右1", "右2"]
        .into_iter()
        .map(token)
        .collect::<Vec<_>>();

    let (left, right) = project_context(&tokens, 2..3, 1);

    assert_eq!(orths(&left), ["左1"]);
    assert_eq!(orths(&right), ["右1"]);
    assert_eq!(orths(&tokens[2..3]), ["鍵"]);
}

#[test]
fn matched_surface_rejects_a_missing_orth_with_sentence_and_token_location() {
    let mut tokens = vec![token("有"), TokenInfo::default()];
    tokens[1].set_text("fallback-must-not-be-used");
    let location = SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: 7,
        document_id: lingrep_core::corpus::DocumentId::new(2),
        document_ordinal: 2,
        document_sentence_ordinal: 3,
        source_name: "a.md".into(),
    };

    assert_eq!(
        matched_orth_tokens(&[token("猫"), token("犬")], 0..2, &location).unwrap(),
        ["猫", "犬"]
    );
    let error = matched_surface(&tokens, 0..2, &location).unwrap_err();
    assert_eq!(
        matched_orth_tokens(&tokens, 0..2, &location).unwrap_err(),
        error
    );

    assert_eq!(error.attribute, "text");
    assert_eq!(error.corpus_sentence_ordinal, 7);
    assert_eq!(error.token_index, 1);
    assert_eq!(error.source_name, "a.md");
    assert!(matches!(
        ExecutionError::from(error),
        ExecutionError::MissingRequiredAttribute {
            attribute: "text",
            corpus_sentence_ordinal: 7,
            token_index: 1,
            ..
        }
    ));
}

#[test]
fn a_deferred_oversized_sentence_retains_its_typed_resource_failure() {
    let document = DocumentId::new(0);
    let events = vec![
        Ok(AdmittedCorpusEvent::DocumentStart {
            id: document,
            source_name: "a.md".into(),
            metadata: Box::new([]),
        }),
        Ok(AdmittedCorpusEvent::Sentence(AdmittedSentence {
            structure: Default::default(),
            id: SentenceId::new(0),
            document_id: document,
            document_sentence_ordinal: 0,
            text: "a".into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        })),
        Ok(AdmittedCorpusEvent::Sentence(AdmittedSentence {
            structure: Default::default(),
            id: SentenceId::new(1),
            document_id: document,
            document_sentence_ordinal: 1,
            text: "oversized".into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        })),
    ];
    let mut source = source_from_events(events.into_iter());
    let limits = MatcherLimits {
        max_sentence_bytes: 4,
        max_input_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        max_stage_sentences: 8,
        max_stage_input_bytes: 8,
        max_tokenized_bytes: 1024,
    };

    assert!(source.next_batch(&limits).unwrap().is_some());
    assert!(matches!(
        source.next_batch(&limits),
        Err(ExecutionError::SentenceTooLarge {
            ordinal: 1,
            actual: 9,
            limit: 4,
            ..
        })
    ));
}

#[test]
fn deferred_source_errors_use_the_global_next_sentence_ordinal() {
    let document = DocumentId::new(0);
    let events = vec![
        Ok(AdmittedCorpusEvent::DocumentStart {
            id: document,
            source_name: "a.md".into(),
            metadata: Box::new([]),
        }),
        Ok(AdmittedCorpusEvent::Sentence(AdmittedSentence {
            structure: Default::default(),
            id: SentenceId::new(0),
            document_id: document,
            document_sentence_ordinal: 0,
            text: "a".into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        })),
        Ok(AdmittedCorpusEvent::DocumentEnd { id: document }),
        Err(std::io::Error::other("later read failure")),
    ];
    let mut source = source_from_events(events.into_iter());

    assert!(
        source
            .next_batch(&MatcherLimits::default())
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        source.next_batch(&MatcherLimits::default()),
        Err(ExecutionError::Input { ordinal: 1, .. })
    ));
}

#[test]
fn completed_stdin_reports_stream_digest_but_no_corpus_identity() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .arg("--json")
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg("[].")
        .arg("-")
        .write_stdin("猫。\n")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    validate_query_jsonl(&output.stdout).unwrap();
    assert_eq!(events[0]["type"], "run");
    assert_eq!(events[0]["data"]["corpus_kind"], "stdin");
    assert!(events[0]["data"]["stream_id"].as_str().is_some());
    let complete = events.last().unwrap();
    assert_eq!(complete["type"], "complete");
    assert_eq!(complete["data"]["coverage"], "full");
    assert_eq!(complete["data"]["termination"], "eof");
    assert!(
        complete["data"]["observed_content_digest"]
            .as_str()
            .is_some()
    );
    assert!(complete["data"].get("source_identity").is_none());
    assert!(complete["data"].get("analysis_identity").is_none());
}

#[test]
fn stdin_requires_the_explicit_dash_root() {
    fn run(root: Option<&str>) -> Output {
        let dictionary = MiniDictionary::new();
        let mut command = lingrep();
        command
            .arg("--json")
            .arg("matches")
            .arg("--dict-path")
            .arg(dictionary.path())
            .arg("--pattern")
            .arg("[].");
        if let Some(root) = root {
            command.arg(root);
        }
        command.write_stdin("猫。\n").output().unwrap()
    }

    let omitted = run(None);
    let dash = run(Some("-"));
    assert_eq!(omitted.status.code(), Some(2));
    assert!(dash.status.success());
    assert!(omitted.stdout.is_empty());
    validate_query_jsonl(&dash.stdout).unwrap();
}

#[test]
fn completed_filesystem_run_moves_identity_from_pending_to_known() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "猫。\n").unwrap();

    let output = run_direct(&corpus, &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    assert_eq!(events[0]["data"]["corpus_kind"], "direct");
    assert_eq!(events[0]["data"]["state"], "pending");
    assert!(events[0]["data"].get("source_identity").is_none());
    let complete = events.last().unwrap();
    assert_eq!(complete["data"]["corpus_kind"], "direct");
    assert_eq!(complete["data"]["state"], "known");
    assert!(complete["data"]["source_identity"].as_str().is_some());
    assert!(complete["data"]["analysis_identity"].as_str().is_some());
    assert_eq!(complete["data"]["inventory"]["documents"], 1);
    assert_eq!(complete["data"]["counters"]["documents_admitted"], 1);
}

#[test]
fn direct_pattern_events_and_analysis_identity_are_invariant_to_worker_and_batch_choices() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("sentences.txt");
    std::fs::write(&corpus, "猫。犬。鳥。\n").unwrap();

    let one = run_direct(
        &corpus,
        &[
            "--workers",
            "1",
            "--batch-sentences",
            "1",
            "--batch-raw-bytes",
            "65536",
        ],
    );
    let four = run_direct(
        &corpus,
        &[
            "--workers",
            "4",
            "--batch-sentences",
            "3",
            "--batch-raw-bytes",
            "196608",
        ],
    );

    for output in [&one, &four] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        validate_query_jsonl(&output.stdout).unwrap();
    }
    let pattern_events = |bytes: &[u8]| {
        parse_jsonl(bytes)
            .into_iter()
            .filter(|event| event["type"] == "definition")
            .collect::<Vec<_>>()
    };
    assert_eq!(pattern_events(&one.stdout), pattern_events(&four.stdout));
    let one = parse_jsonl(&one.stdout);
    let four = parse_jsonl(&four.stdout);
    assert_eq!(
        one.last().unwrap()["data"]["analysis_identity"],
        four.last().unwrap()["data"]["analysis_identity"]
    );
    let run = &four[0]["data"];
    assert_eq!(run["worker_count"], 4);
    assert_eq!(run["execution_limits"]["max_stage_sentences"], 3);
    assert_eq!(run["execution_limits"]["max_stage_input_bytes"], 196_608);
}

#[test]
fn markdown_semantics_and_identities_do_not_depend_on_an_unused_raw_batch_bound() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("document.md");
    std::fs::write(&corpus, "---\ntitle: bounded identity\n---\n\n猫。犬。\n").unwrap();

    let smaller = run_direct(
        &corpus,
        &[
            "--workers",
            "1",
            "--batch-sentences",
            "1",
            "--batch-raw-bytes",
            "65536",
        ],
    );
    let larger = run_direct(
        &corpus,
        &[
            "--workers",
            "4",
            "--batch-sentences",
            "3",
            "--batch-raw-bytes",
            "196608",
        ],
    );

    for output in [&smaller, &larger] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        validate_query_jsonl(&output.stdout).unwrap();
    }
    let result_lines = |bytes: &[u8]| {
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .filter(|line| {
                matches!(
                    serde_json::from_slice::<Value>(line).unwrap()["type"].as_str(),
                    Some("definition" | "match")
                )
            })
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>()
    };
    assert_eq!(result_lines(&smaller.stdout), result_lines(&larger.stdout));
    let smaller = parse_jsonl(&smaller.stdout);
    let larger = parse_jsonl(&larger.stdout);
    assert_eq!(
        smaller[0]["data"]["reader_identity"],
        larger[0]["data"]["reader_identity"]
    );
    for field in ["source_identity", "analysis_identity"] {
        assert_eq!(
            smaller.last().unwrap()["data"][field],
            larger.last().unwrap()["data"][field]
        );
    }
}

#[test]
fn a_global_limit_stops_after_one_match_across_two_patterns() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "猫。\n").unwrap();
    let catalog = directory.path().join("two.lx");
    std::fs::write(&catalog, "a := [].\nb := [].\n").unwrap();
    let dictionary = MiniDictionary::new();

    let output = lingrep()
        .arg("--json")
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--patterns")
        .arg(&catalog)
        .arg("--limit")
        .arg("1")
        .arg(&corpus)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    assert_eq!(
        event_types(&events),
        ["run", "definition", "definition", "match", "complete"]
    );
    assert_eq!(events[3]["data"]["sequence"], 0);
    assert_eq!(events[4]["data"]["coverage"], "prefix");
    assert_eq!(events[4]["data"]["state"], "prefix_unavailable");
    assert_eq!(events[4]["data"]["counters"]["records_emitted"], 1);
    assert_eq!(events[4]["data"]["termination"], "result_limit");
}

#[test]
fn inline_and_equivalent_file_catalogs_share_the_semantic_fingerprint() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "猫。\n").unwrap();
    let catalog = directory.path().join("one.lx");
    std::fs::write(&catalog, "inline := [].\n").unwrap();

    let inline = run_direct(&corpus, &[]);
    let dictionary = MiniDictionary::new();
    let file = lingrep()
        .arg("--json")
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--patterns")
        .arg(&catalog)
        .arg(&corpus)
        .output()
        .unwrap();

    assert!(inline.status.success());
    assert!(file.status.success());
    let fingerprint = |output: &Output| {
        parse_jsonl(&output.stdout)[0]["data"]["catalog_fingerprint"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(fingerprint(&inline), fingerprint(&file));
}

#[cfg(unix)]
fn corpus_with_match_then_dangling_link() -> TempDir {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().unwrap();
    let nested = directory.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("a.md"), "猫。\n").unwrap();
    symlink(nested.join("missing.md"), nested.join("z.md")).unwrap();
    directory
}

#[cfg(unix)]
#[test]
fn later_discovery_failure_preserves_only_the_committed_prefix() {
    let fixture = corpus_with_match_then_dangling_link();
    let output = run_direct_with_pattern(fixture.path(), "[text=猫].", &["--json"]);

    assert_eq!(output.status.code(), Some(1));
    let events = parse_jsonl(&output.stdout);
    assert_eq!(event_types(&events), ["run", "definition", "match"]);
    assert_eq!(events[2]["data"]["source_name"], "nested/a.md");
    let diagnostic = assert_post_run_diagnostic(&output, "dangling_link", "input");
    assert_eq!(diagnostic["data"]["location"]["source_name"], "nested/z.md");

    let limited = run_direct_with_pattern(fixture.path(), "[text=猫].", &["--limit", "1"]);
    assert!(
        limited.status.success(),
        "{}",
        String::from_utf8_lossy(&limited.stderr)
    );
    let events = parse_jsonl(&limited.stdout);
    assert_eq!(events.last().unwrap()["type"], "complete");
    assert_eq!(events.last().unwrap()["data"]["coverage"], "prefix");
    assert_eq!(
        events.last().unwrap()["data"]["state"],
        "prefix_unavailable"
    );
}

#[test]
fn match_tsv_is_the_exact_eleven_column_projection() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "左。鍵。右。\n").unwrap();

    let output = run_direct(&corpus, &["--tsv", "--context", "1"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for line in output.stdout.split(|byte| *byte == b'\n') {
        if !line.is_empty() {
            assert_eq!(line.split(|byte| *byte == b'\t').count(), 11);
        }
    }
    assert!(
        !output
            .stdout
            .windows(10)
            .any(|window| window == b"\"complete\"")
    );
}

#[test]
fn closed_stdout_is_failure_and_never_publishes_complete() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("many.txt");
    std::fs::write(&corpus, "猫。\n".repeat(20_000)).unwrap();
    let (_dictionary, mut child) = raw_long_running_command(&corpus, &[]);
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.contains("\"type\":\"run\""));
    drop(reader);

    let status = child.wait().unwrap();
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();

    assert_eq!(status.code(), Some(1));
    let diagnostic = parse_jsonl(&stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "broken_pipe");
    assert_eq!(diagnostic[0]["data"]["stage"], "output");
    assert_eq!(
        diagnostic[0]["data"]["run_id"],
        parse_jsonl(first.as_bytes())[0]["data"]["run_id"]
    );
}

#[cfg(unix)]
fn raw_long_running_command(root: &std::path::Path, extra: &[&str]) -> (MiniDictionary, Child) {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep_std();
    command
        .arg("--json")
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg("[].")
        .arg("--batch-sentences")
        .arg("1");
    command
        .args(extra)
        .arg(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    (dictionary, child)
}

#[test]
fn post_run_resource_and_parser_failures_are_typed_and_located() {
    let directory = TempDir::new().unwrap();

    let oversized = directory.path().join("oversized.txt");
    std::fs::write(&oversized, "猫。\n").unwrap();
    let output = run_direct(&oversized, &["--json", "--max-sentence-bytes", "2"]);
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = assert_post_run_diagnostic(&output, "sentence_too_large", "input");
    assert_eq!(
        diagnostic["data"]["location"]["source_name"],
        "oversized.txt"
    );
    assert_eq!(diagnostic["data"]["location"]["corpus_sentence_ordinal"], 0);

    let malformed = directory.path().join("malformed.jsonl");
    std::fs::write(&malformed, "{not-json}\n").unwrap();
    let output = run_direct(&malformed, &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = assert_post_run_diagnostic(&output, "input", "input");
    assert_eq!(
        diagnostic["data"]["location"]["source_name"],
        "malformed.jsonl"
    );
    assert_eq!(diagnostic["data"]["location"]["corpus_sentence_ordinal"], 0);

    let corpus = directory.path().join("corpus");
    std::fs::create_dir(&corpus).unwrap();
    std::fs::write(corpus.join("a.md"), "猫。\n").unwrap();
    std::fs::write(corpus.join("b.md"), "犬。\n").unwrap();
    let output = run_direct(&corpus, &["--json", "--max-documents", "1"]);
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = assert_post_run_diagnostic(&output, "traversal_limit", "input");
    assert_eq!(diagnostic["data"]["location"]["source_name"], "b.md");
}

#[test]
fn root_index_limit_diagnostic_uses_logical_dot_without_the_absolute_root() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("corpus");
    std::fs::create_dir(&corpus).unwrap();
    std::fs::write(corpus.join("a.md"), "猫。\n").unwrap();

    let output = run_direct(&corpus, &["--json", "--max-source-index-bytes", "1"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "root validation must precede run publication: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let diagnostics = parse_jsonl(&output.stderr);
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic["data"]["code"], "traversal_limit");
    assert_eq!(diagnostic["data"]["stage"], "input");
    assert!(diagnostic["data"]["run_id"].is_null());
    assert_eq!(diagnostic["data"]["location"]["source_name"], ".");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&corpus.display().to_string()));
}

#[cfg(unix)]
#[test]
fn dangling_and_missing_roots_use_discovery_classification_at_logical_dot() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().unwrap();
    let dangling = directory.path().join("dangling.md");
    symlink("absent.md", &dangling).unwrap();
    let missing = directory.path().join("ordinary-missing.md");

    for (root, code) in [(&dangling, "dangling_link"), (&missing, "discovery_io")] {
        let output = run_direct(root, &["--json"]);

        assert_eq!(output.status.code(), Some(1));
        assert!(
            output.stdout.is_empty(),
            "root validation must precede run publication: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let diagnostics = parse_jsonl(&output.stderr);
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic["data"]["code"], code);
        assert_eq!(diagnostic["data"]["stage"], "input");
        assert!(diagnostic["data"]["run_id"].is_null());
        assert_eq!(diagnostic["data"]["location"]["source_name"], ".");
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&root.display().to_string()));
    }
}

#[cfg(unix)]
fn signal_after_first_line(signal: i32, extra: &[&str]) -> Output {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("many.txt");
    std::fs::write(&corpus, "猫。\n".repeat(100_000)).unwrap();
    let (_dictionary, mut child) = raw_long_running_command(&corpus, extra);
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.contains("\"type\":\"run\""));
    let result = unsafe { libc::kill(child.id() as i32, signal) };
    assert_eq!(result, 0);
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    let status = child.wait().unwrap();
    let mut stdout = first.into_bytes();
    stdout.extend(rest);
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    Output {
        status,
        stdout,
        stderr,
    }
}

#[cfg(unix)]
#[test]
fn first_sigint_cancels_between_pushes_and_exits_130_without_complete() {
    for extra in [&[][..], &["--retain", "5"][..]] {
        let output = signal_after_first_line(libc::SIGINT, extra);
        assert_eq!(output.status.code(), Some(130));
        let events = parse_jsonl(&output.stdout);
        assert!(!event_types(&events).contains(&"complete"));
        if !extra.is_empty() {
            assert!(!event_types(&events).contains(&"match"));
        }
    }
}

#[cfg(unix)]
#[test]
fn first_sigterm_cancels_between_pushes_and_exits_143_without_complete() {
    for extra in [&[][..], &["--retain", "5"][..]] {
        let output = signal_after_first_line(libc::SIGTERM, extra);
        assert_eq!(output.status.code(), Some(143));
        let events = parse_jsonl(&output.stdout);
        assert!(!event_types(&events).contains(&"complete"));
        if !extra.is_empty() {
            assert!(!event_types(&events).contains(&"match"));
        }
    }
}

/// Bytes the child has written to its stdout pipe that nobody has read.
///
/// `FIONREAD` on the read end is the only way to see the *writer's* state from
/// here without consuming the very backpressure being established.
#[cfg(unix)]
fn buffered_on_pipe(reader: &BufReader<std::process::ChildStdout>) -> usize {
    use std::os::fd::AsRawFd as _;

    let mut pending: libc::c_int = 0;
    let result = unsafe {
        libc::ioctl(
            reader.get_ref().as_raw_fd(),
            libc::FIONREAD as _,
            &raw mut pending,
        )
    };
    assert_eq!(result, 0, "FIONREAD on the child's stdout pipe");
    pending.try_into().unwrap()
}

/// Block until the child is parked in `write` on a full stdout pipe.
///
/// A pipe whose buffered byte count stops growing while this side reads nothing
/// is a pipe whose writer is blocked, and a blocked writer cannot reach
/// `TerminalArbiter::try_claim_completion`. That is a *state*, not a moment: it
/// persists until this test chooses to drain, which is what lets the caller
/// order a signal against it.
#[cfg(unix)]
fn wait_until_parked_on_backpressure(reader: &BufReader<std::process::ChildStdout>) {
    let mut previous = 0;
    let mut stable = 0;
    for _ in 0..1_000 {
        let pending = buffered_on_pipe(reader);
        stable = if pending > 0 && pending == previous {
            stable + 1
        } else {
            0
        };
        if stable == 3 {
            return;
        }
        previous = pending;
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("child never filled its stdout pipe");
}

/// A configured `--limit` must not let a cancelled run claim completion.
///
/// The ordering here is established by backpressure rather than by racing the
/// signal against the run, because the race has no correct answer: a run that
/// genuinely finishes before the signal lands is *entitled* to exit 0, so a
/// test that signals a short run and asserts 130 pins an interleaving and not a
/// contract. It failed exactly that way — `--limit 1` over a matching corpus
/// completes in the time it takes to deliver a signal.
///
/// So the child is parked mid-publication instead: its result set is far larger
/// than the pipe can hold, it blocks writing, and it stays blocked until this
/// test drains. Cancellation is therefore observed first by construction, and
/// the assertion is about `TerminalArbiter`'s wiring rather than about which
/// thread the scheduler happened to pick.
///
/// The limit is reachable — 100,000 sentences supply 200,000 single-token
/// matches — so the run is on course to claim completion and must not. Whether
/// the limit is reached *simultaneously* with the signal is not separately
/// reachable, and is not a distinct contract: the arbiter is one CAS, pinned by
/// `terminal::tests::first_signal_atomically_wins_over_completion_and_later_signals`.
#[cfg(unix)]
#[test]
fn cancellation_wins_over_a_configured_result_limit_in_the_process_protocol() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("many.txt");
    std::fs::write(&corpus, "猫。\n".repeat(100_000)).unwrap();
    let (_dictionary, mut child) = raw_long_running_command(&corpus, &["--limit", "100000"]);
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.contains("\"type\":\"run\""));

    wait_until_parked_on_backpressure(&reader);

    let result = unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    assert_eq!(result, 0, "the parked child must still be alive");

    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    let status = child.wait().unwrap();
    let mut stdout = first.into_bytes();
    stdout.extend(rest);

    assert_eq!(status.code(), Some(130));
    assert!(!event_types(&parse_jsonl(&stdout)).contains(&"complete"));
}

#[cfg(unix)]
#[test]
fn completion_claim_wins_over_a_signal_observed_after_the_complete_event() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("short.txt");
    std::fs::write(&corpus, "猫。\n").unwrap();
    let (_dictionary, mut child) = raw_long_running_command(&corpus, &[]);
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut captured = Vec::new();

    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "missing complete event"
        );
        let is_complete = line.contains("\"type\":\"complete\"");
        captured.extend_from_slice(line.as_bytes());
        if is_complete {
            break;
        }
    }

    let kill_result = unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    if kill_result != 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    reader.read_to_end(&mut captured).unwrap();
    let status = child.wait().unwrap();
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();

    assert_eq!(
        status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        event_types(&parse_jsonl(&captured))
            .into_iter()
            .filter(|event| *event == "complete")
            .count(),
        1
    );
}

/// Two runs of one binary over one input differ **only** in the fields
/// [`lingrep::protocol::query::RUN_VARIANT_FIELDS`] names.
///
/// This is what makes that constant trustworthy rather than merely plausible.
/// Every performance campaign discharges the output-parity requirement by
/// comparing two binaries' streams, and an incomplete list reports a difference
/// that is not there. That is not hypothetical: `elapsed_millis` was found only
/// after a parity comparison had already been misread as a semantic difference,
/// and the check that settled it was running one binary twice — this test.
///
/// The un-normalized comparison is asserted first so the normalized one cannot
/// pass vacuously: `run_id` is fresh per run, so the raw streams always differ.
#[test]
fn two_runs_of_one_binary_differ_only_in_the_named_run_variant_fields() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("corpus.txt");
    std::fs::write(&corpus, "猫。\n".repeat(64)).unwrap();
    let patterns = directory.path().join("patterns.lx");
    std::fs::write(&patterns, "pattern cat := [text=猫].\n").unwrap();

    let run = || {
        let output = lingrep_std()
            .args(["--json", "matches", "--dict-path"])
            .arg(dictionary.path())
            .args(["--patterns"])
            .arg(&patterns)
            .arg(&corpus)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse_jsonl(&output.stdout)
    };

    let raw_first = run();
    let raw_second = run();
    assert!(
        raw_first.iter().any(|event| event["type"] == "match"),
        "the fixture must publish matches, or parity is asserted over nothing"
    );
    assert!(
        raw_first.iter().any(|event| event["type"] == "complete"),
        "the fixture must complete, or elapsed_millis is never exercised"
    );
    assert_ne!(
        raw_first, raw_second,
        "run_id is fresh per run, so the raw streams must differ"
    );

    let normalize = |events: Vec<Value>| {
        events
            .into_iter()
            .map(|mut event| {
                lingrep::protocol::query::normalize_run_variance(&mut event);
                event
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(normalize(raw_first), normalize(raw_second));
}

/// A label set collapses an *n*-pattern enumeration into one pattern without
/// losing which label matched: the matched label is the dependent capture's
/// own `dep.rel`, so an ordered key reports it. This is the claim the
/// 2026-08-29 design round rests on, end to end through publication.
#[test]
fn a_label_set_reports_which_member_matched_through_an_ordered_key() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ud-en-subtyped-sample.conllu");
    let observed = |labels: &str| {
        let output = lingrep()
            .args([
                "--json",
                "matches",
                "--limit",
                "all",
                "--pattern",
                &format!(
                    "key: [$d.dep.rel]\n\
                     pattern p := (?<h> [upos=VERB]) []* (?<d> [upos=NOUN]) \
                     where $h -[{labels}]-> $d."
                ),
            ])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut keys: Vec<String> = match_events(&output)
            .iter()
            .map(|event| {
                let key = event["data"]["key"].as_array().unwrap();
                assert_eq!(key.len(), 1, "one projection was authored");
                assert_eq!(key[0]["name"], "$d.dep.rel");
                key[0]["value"].as_str().unwrap().to_owned()
            })
            .collect();
        keys.sort();
        keys
    };

    // `obl` and `obl:tmod` are two labels: the wildcard takes the subtyped
    // one only, and the union spelling takes both.
    assert_eq!(observed("obl"), ["obl"]);
    assert_eq!(observed("obl:*"), ["obl:tmod"]);
    assert_eq!(observed("obl|obl:*"), ["obl", "obl:tmod"]);
    // A `@set` term means what the inline spelling means, and the set may be
    // declared after the pattern that references it.
    let named = {
        let output = lingrep()
            .args([
                "--json",
                "matches",
                "--limit",
                "all",
                "--pattern",
                "key: [$d.dep.rel]\n\
                 pattern p := (?<h> [upos=VERB]) []* (?<d> [upos=NOUN]) \
                 where $h -[@OBLIQUE]-> $d.\n\
                 set OBLIQUE := {obl, obl:*}.",
            ])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut keys: Vec<String> = match_events(&output)
            .iter()
            .map(|event| {
                event["data"]["key"][0]["value"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(named, observed("obl|obl:*"));
}

/// The two refusals a label set adds are invocation errors that say what to
/// change, not silent zero-match catalogs.
#[test]
fn contradictory_and_repeated_label_sets_are_refused_by_name() {
    for (pattern, code) in [
        (
            "pattern p := (?<a> []) (?<c> []) where $a -[obj]-> $c, $a -[obl]-> $c.",
            "WhereContradictoryLabels",
        ),
        (
            "pattern p := (?<a> []) (?<c> []) where $a -[obj|obj]-> $c.",
            "DuplicateRelationLabel",
        ),
        (
            "pattern p := ((?<a> []) | (?<b> [])) where $a -[obj]-> $b.",
            "WhereExclusiveCaptures",
        ),
    ] {
        let output = lingrep()
            .args(["--json", "check", "--pattern", pattern])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{pattern}");
        let reported = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(reported.contains(code), "{pattern}: {reported}");
    }
}

#[test]
fn sparse_sentence_metadata_scopes_cross_prepared_pages_without_changing_counts() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("paged.conllu");
    let mut text = String::new();
    for ordinal in 0..600 {
        if ordinal % 2 == 0 {
            text.push_str(&format!("# newdoc id = d{}\n", ordinal / 2));
        }
        if ordinal % 5 != 0 {
            text.push_str(&format!("# sent_id = s{ordinal}\n"));
        }
        text.push_str(&format!(
            "1\tx{ordinal}\tx{ordinal}\tNOUN\t_\t_\t0\troot\t_\t_\n\n"
        ));
    }
    std::fs::write(&input, text).unwrap();
    let artifact = directory.path().join("paged.lingcorpus");
    let built = lingrep()
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
    for clause in [
        "@sentence.sent_id in {s1, s321, s599}",
        "@document.newdoc_id = d1 & @sentence.sent_id != s599",
    ] {
        let mut outputs = Vec::new();
        for prepared in [false, true] {
            let mut command = lingrep();
            command.args([
                "--json",
                "matches",
                "--pattern",
                "n := [upos=NOUN].",
                "--limit",
                "all",
                "--where",
                clause,
            ]);
            if prepared {
                command.arg("--corpus").arg(&artifact);
            } else {
                command.arg(&input);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let surfaces = match_events(&output)
                .iter()
                .map(|event| event["data"]["surface"].clone())
                .collect::<Vec<_>>();
            let records = parse_jsonl(&output.stdout);
            let scope = records
                .iter()
                .find(|record| record["type"] == "complete")
                .unwrap()["data"]["scope"]
                .clone();
            outputs.push((surfaces, scope));
        }
        assert_eq!(outputs[0], outputs[1], "{clause}");
        assert!(!outputs[0].0.is_empty());
    }
}

#[test]
fn source_metadata_projection_remains_joinable_after_scope_and_sampling() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("captions.jsonl");
    let documents = (0..40).map(|cue| serde_json::json!({"text":if cue % 4 == 0 {"cat"} else {"猫"}, "metadata":{"video_id":format!("video-{cue}"), "cue_index":cue, "start_ms":0, "empty":"", "flag":false, "channel":if cue % 2 == 0 {"keep"} else {"drop"}, "private":"omit"}}).to_string()).collect::<Vec<_>>().join("\n");
    let documents = documents + "\n";
    std::fs::write(&input, &documents).unwrap();
    let artifact = directory.path().join("captions.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    for mode in ["direct", "prepared", "stdin"] {
        let mut command = lingrep();
        command.args(["--json", "matches"]).args([
            "--pattern",
            "hit := (?<word> []).",
            "--where",
            "@document.channel = keep",
            "--sample",
            "0.5",
            "--sample-seed",
            "7",
            "--limit",
            "all",
        ]);
        for key in [
            "@document.video_id",
            "@document.cue_index",
            "@document.start_ms",
            "@document.empty",
            "@document.flag",
            "@document.missing",
        ] {
            command.args(["--source-metadata", key]);
        }
        if mode == "prepared" {
            command.arg("--corpus").arg(&artifact);
        } else {
            command
                .arg("--dict-path")
                .arg(dictionary.path())
                .arg(&input);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        validate_query_jsonl(&output.stdout).unwrap();
        let records = parse_jsonl(&output.stdout);
        let run = &records
            .iter()
            .find(|record| record["type"] == "run")
            .unwrap()["data"];
        assert_eq!(
            run["published_source_metadata"],
            serde_json::json!([
                "@document.cue_index",
                "@document.empty",
                "@document.flag",
                "@document.missing",
                "@document.start_ms",
                "@document.video_id"
            ])
        );
        let matches = match_events(&output);
        assert!(!matches.is_empty());
        for record in matches {
            let data = &record["data"];
            let metadata = &data["source_metadata"]["document"];
            let cue = metadata["cue_index"].as_u64().unwrap();
            assert_eq!(cue % 2, 0);
            assert_eq!(metadata["video_id"], format!("video-{cue}"));
            assert_eq!(metadata["start_ms"], 0);
            assert_eq!(metadata["empty"], "");
            assert_eq!(metadata["flag"], false);
            assert!(metadata.get("missing").is_none());
            assert!(metadata.get("private").is_none());
            assert_eq!(data["captures"][0]["name"], "word");
        }
    }
}

#[test]
fn a_soranoha_archive_queries_and_publishes_its_catalog_bibliography() {
    let directory = TempDir::new().unwrap();
    let archive = crate::support::soranoha_archive(&directory.path().join("akutagawa.zip"));

    // One author's work, selected by bibliography, publishes the citation
    // fields the catalog carries.
    let output = run_direct_with_pattern(
        &archive,
        "kumo := [text=猫].",
        &[
            "--input-format",
            "soranoha",
            "--where",
            "@document.author = \"芥川 竜之介\"",
            "--source-metadata",
            "@document.identifier",
            "--source-metadata",
            "@document.title",
            "--source-metadata",
            "@document.body_character_count",
            "--source-metadata",
            "@document.release",
            "--limit",
            "all",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    validate_query_jsonl(&output.stdout).unwrap();
    let matches = match_events(&output);
    assert_eq!(matches.len(), 1);
    let metadata = &matches[0]["data"]["source_metadata"]["document"];
    assert_eq!(metadata["identifier"], "000092_000879");
    assert_eq!(metadata["title"], "蜘蛛の糸");
    assert_eq!(metadata["body_character_count"], 7);
    assert_eq!(
        metadata["release"],
        "c373bb085971c6a88d8aa1c164788c53ebf304ab501a6f5188c3077ac5482916"
    );

    // The catalog length is typed as a number, so an ordered comparison
    // selects by it rather than comparing digits as text.
    let output = run_direct_with_pattern(
        &archive,
        "hit := [text=犬].",
        &[
            "--input-format",
            "soranoha",
            "--where",
            "@document.body_character_count < 9",
            "--limit",
            "all",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let matches = match_events(&output);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["data"]["source_name"], "hari.txt");
}

#[test]
fn jsonl_sentence_metadata_scopes_and_projections_agree_for_direct_and_prepared() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("sentence-metadata.jsonl");
    let documents = [
        serde_json::json!({"metadata":{"id":"document", "role":"default", "video_id":"v0"}, "sentences":[{"text":"猫", "metadata":{"id":"s0", "role":"hit", "start_ms":0}}, "犬", {"text":"鳥", "id":"s2", "role":"other", "start_ms":20}]}),
        serde_json::json!({"metadata":{"id":"d1", "role":"other", "video_id":"v1"}, "sentences":["猫"]}),
    ];
    std::fs::write(
        &input,
        documents.map(|value| value.to_string()).join("\n") + "\n",
    )
    .unwrap();
    let artifact = directory.path().join("sentence-metadata.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    for (clause, expected) in [
        ("@sentence.id = s0", vec!["猫"]),
        ("@document.id = document", vec!["猫", "犬", "鳥"]),
        ("@document.role = default", vec!["猫", "犬", "鳥"]),
        ("@sentence.role = hit", vec!["猫"]),
        ("@sentence.start_ms >= 0", vec!["猫", "鳥"]),
        ("@sentence.role != hit", vec!["鳥"]),
        ("(@sentence.id = s0 | @document.id = d1)", vec!["猫", "猫"]),
    ] {
        let mut results = Vec::new();
        for prepared in [false, true] {
            let mut command = lingrep();
            command.args([
                "--json",
                "matches",
                "--pattern",
                "hit := (?<word> []).",
                "--where",
                clause,
                "--limit",
                "all",
            ]);
            for key in [
                "@sentence.id",
                "@sentence.role",
                "@sentence.start_ms",
                "@document.video_id",
            ] {
                command.args(["--source-metadata", key]);
            }
            if prepared {
                command.arg("--corpus").arg(&artifact);
            } else {
                command
                    .arg("--dict-path")
                    .arg(dictionary.path())
                    .arg(&input);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{clause}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            validate_query_jsonl(&output.stdout).unwrap();
            let records = parse_jsonl(&output.stdout);
            let matches = match_events(&output);
            assert_eq!(
                matches
                    .iter()
                    .map(|record| record["data"]["surface"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                expected,
                "{clause}"
            );
            let matches = matches
                .iter()
                .map(|record| {
                    (
                        record["data"]["surface"].clone(),
                        record["data"]["source_metadata"].clone(),
                        record["data"]["captures"].clone(),
                    )
                })
                .collect::<Vec<_>>();
            let scope = records
                .iter()
                .find(|record| record["type"] == "complete")
                .unwrap()["data"]["scope"]
                .clone();
            results.push((matches, scope));
        }
        assert_eq!(results[0], results[1], "{clause}");
    }
}

#[test]
fn prepared_metadata_discovery_reports_raw_grains_and_missing_values() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("metadata.conllu");
    std::fs::write(&input, "# newdoc id = document\n# sent_id = first\n1\ta\ta\tNOUN\t_\t_\t0\troot\t_\t_\n\n1\tb\tb\tNOUN\t_\t_\t0\troot\t_\t_\n\n").unwrap();
    let artifact = directory.path().join("metadata.lingcorpus");
    let built = lingrep()
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
    let output = lingrep()
        .args(["--json", "corpus", "inspect", "--metadata"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let complete = records.last().unwrap();
    let metadata = &complete["data"]["metadata"];
    assert_eq!(metadata["documents"]["units"], 1);
    assert_eq!(metadata["sentences"]["units"], 2);
    assert_eq!(metadata["sentences"]["fields"]["sent_id"]["present"], 1);
    assert_eq!(metadata["sentences"]["fields"]["sent_id"]["missing"], 1);
    assert_eq!(
        metadata["sentences"]["fields"]["sent_id"]["shapes"]["string"],
        1
    );
    assert!(metadata["sentences"]["fields"].get("newdoc_id").is_none());
}

#[test]
fn structural_metadata_has_identical_owner_values_and_scope_counts_in_direct_and_prepared_queries()
{
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("structure.jsonl");
    let value = serde_json::json!({
        "metadata":{"speaker":"Document","year":2026},
        "paragraphs":[
            {"metadata":{"speaker":"A"},"sentences":[{"text":"猫。","metadata":{"sid":"first","speaker":"Sentence"}},{"text":"犬。","metadata":{"sid":"second"}}]},
            {"metadata":{"speaker":"B"},"sentences":[{"text":"鳥。","metadata":{"sid":"third"}}]}
        ],
        "structure":{"observed_kinds":["part","chapter","section","region"],"divisions":[
            {"id":"p","kind":"part","start":0,"end":3,"metadata":{"number":1}},
            {"id":"c","kind":"chapter","parent":"p","start":0,"end":3,"metadata":{"title":"Chapter"}},
            {"id":"results","kind":"section","start":0,"end":2,"metadata":{"role":"results","reviewer":"A"}},
            {"id":"methods-first","kind":"section","start":0,"end":1,"metadata":{"role":"methods","reviewer":"B"}},
            {"id":"methods-second","kind":"section","start":1,"end":2,"metadata":{"role":"methods","reviewer":"A"}},
            {"id":"quote","kind":"region","start":1,"end":3,"metadata":{"role":"quote"}},
            {"id":"empty","kind":"chapter","parent":"c","start":2,"end":2,"metadata":{"title":"Empty"}}
        ]}
    });
    std::fs::write(&input, format!("{value}\n")).unwrap();
    let artifact = directory.path().join("structure.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    for (clause, expected) in [
        (None, vec!["first", "second", "third"]),
        (
            Some("within(section, role=methods & reviewer=A)"),
            vec!["second"],
        ),
        (
            Some("within(section, role=methods) & within(section, reviewer=A)"),
            vec!["first", "second"],
        ),
        (Some("within(paragraph, speaker=B)"), vec!["third"]),
        (
            Some("@document.speaker=Document"),
            vec!["first", "second", "third"],
        ),
        (Some("@sentence.speaker=Document"), vec![]),
        (Some("not within(section, absent=value)"), vec!["third"]),
        (
            Some("within(part, number=1)"),
            vec!["first", "second", "third"],
        ),
        (Some("within(chapter, title=Empty)"), vec![]),
    ] {
        let mut compared = Vec::new();
        for prepared in [false, true] {
            let mut command = lingrep();
            command.args([
                "--json",
                "matches",
                "--pattern",
                "p := ^ []+ $.",
                "--limit",
                "all",
            ]);
            for key in [
                "@document.speaker",
                "@sentence.sid",
                "@sentence.speaker",
                "@division(paragraph).speaker",
                "@division(part).number",
                "@division(section).role",
            ] {
                command.args(["--source-metadata", key]);
            }
            if let Some(clause) = clause {
                command.args(["--where", clause]);
            }
            if prepared {
                command.arg("--corpus").arg(&artifact);
            } else {
                command
                    .arg("--dict-path")
                    .arg(dictionary.path())
                    .arg(&input);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{clause:?} prepared={prepared}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            validate_query_jsonl(&output.stdout).unwrap();
            let records = parse_jsonl(&output.stdout);
            let mut data = Vec::new();
            let mut sids = Vec::new();
            for record in records.iter().filter(|record| record["type"] == "match") {
                let mut matched = record["data"].clone();
                sids.push(
                    matched["source_metadata"]["sentence"]["sid"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
                assert_eq!(
                    matched["source_metadata"]["document"]["speaker"],
                    "Document"
                );
                let divisions = matched["source_metadata"]["divisions"].as_array().unwrap();
                assert!(divisions.iter().any(|division| division["kind"] == "part"
                    && division["source_id"] == "p"
                    && division["metadata"]["number"] == 1));
                matched
                    .as_object_mut()
                    .unwrap()
                    .remove("corpus_sentence_ordinal");
                matched
                    .as_object_mut()
                    .unwrap()
                    .remove("document_sentence_ordinal");
                data.push(matched);
            }
            assert_eq!(sids, expected, "{clause:?}");
            let mut scope = records
                .iter()
                .find(|record| record["type"] == "complete")
                .unwrap()["data"]["scope"]
                .clone();
            if let Some(scope) = scope.as_object_mut() {
                scope.remove("basis");
            }
            compared.push((data, scope));
        }
        assert_eq!(compared[0], compared[1], "{clause:?}");
    }
    let inspected = lingrep()
        .args(["--json", "corpus", "inspect", "--metadata", "--verify"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let records = parse_jsonl(&inspected.stdout);
    let divisions = &records.last().unwrap()["data"]["metadata"]["divisions"];
    assert_eq!(divisions["paragraph"]["units"], 2);
    assert_eq!(divisions["section"]["units"], 3);
    assert_eq!(divisions["chapter"]["units"], 2);
    assert_eq!(divisions["chapter"]["empty_units"], 1);
    assert_eq!(divisions["paragraph"]["fields"]["speaker"]["present"], 2);
    assert_eq!(divisions["paragraph"]["fields"]["speaker"]["missing"], 0);
}

#[test]
fn normalized_analyzer_offsets_are_identical_when_prepared() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("offsets.jsonl");
    std::fs::write(
        &input,
        serde_json::json!({"sentences": ["は\u{3099} 猫😀"]}).to_string(),
    )
    .unwrap();
    let artifact = directory.path().join("offsets.lingcorpus");
    let pattern = "p := ^ (?<whole> []+) $.";
    let direct = lingrep()
        .args(["--json", "matches", "--dict-path"])
        .arg(dictionary.path())
        .args(["--pattern", pattern, "--limit", "all"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let prepared = crate::support::lingrep_without_dictionary()
        .args(["--json", "matches", "--corpus"])
        .arg(&artifact)
        .args(["--pattern", pattern, "--limit", "all"])
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let matches = match_events(&direct);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches, match_events(&prepared));
    let data = &matches[0]["data"];
    assert_eq!(data["char_span"], serde_json::json!({"start":0,"end":4}));
    assert_eq!(data["captures"][0]["char_span"], data["char_span"]);
}

#[test]
fn prepared_multiword_offsets_preserve_enclosures_and_sentence_wide_absence() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("mwt.conllu");
    let rows = "1-2\tcan't\t_\t_\t_\t_\t_\t_\t_\t_\n1\tca\tcan\tAUX\t_\t_\t3\taux\t_\t_\n2\tn't\tnot\tPART\t_\t_\t3\tadvmod\t_\tSpaceAfter=No\n3\tgo\tgo\tVERB\t_\t_\t0\troot\t_\t_\n\n";
    std::fs::write(&input, format!("# text = can't go\n{rows}{rows}# text = mismatch\n1\tword\tword\tNOUN\t_\t_\t0\troot\t_\t_\n\n")).unwrap();
    let artifact = directory.path().join("mwt.lingcorpus");
    let pattern = "p := (?<word> []).";
    let direct = crate::support::lingrep_without_dictionary()
        .args(["--json", "matches", "--pattern", pattern, "--limit", "all"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let built = crate::support::lingrep_without_dictionary()
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
    let prepared = crate::support::lingrep_without_dictionary()
        .args(["--json", "matches", "--corpus"])
        .arg(&artifact)
        .args(["--pattern", pattern, "--limit", "all"])
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let matches = match_events(&direct);
    assert_eq!(matches, match_events(&prepared));
    assert_eq!(matches.len(), 7);
    for (index, matched) in matches.iter().enumerate().take(6) {
        let expected = if index % 3 == 2 {
            serde_json::json!({"start":6,"end":8})
        } else {
            serde_json::json!({"start":0,"end":5})
        };
        assert_eq!(matched["data"]["char_span"], expected);
        assert_eq!(matched["data"]["captures"][0]["char_span"], expected);
    }
    assert!(matches[6]["data"]["char_span"].is_null());
    for output in [&direct, &prepared] {
        let complete = parse_jsonl(&output.stdout).pop().unwrap();
        assert_eq!(complete["data"]["counters"]["offsets_omitted"], 1);
    }
}
