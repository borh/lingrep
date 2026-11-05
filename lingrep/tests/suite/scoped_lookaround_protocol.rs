// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! `within=` lookarounds end to end, over direct input and a prepared
//! artifact built from it.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Output;

use lingrep_core::testing::MiniDictionary;
use tempfile::TempDir;

use crate::support::{lingrep, lingrep_without_dictionary, parse_jsonl};

/// Each pattern matches the last token of a sentence whose next sentence is
/// in the same document, section or paragraph.
const CATALOG: &str = "pattern document := [] $ followed_by([], within=document).\n\
                       pattern section := [] $ followed_by([], within=section).\n\
                       pattern paragraph := [] $ followed_by([], within=paragraph).";

struct Corpus {
    _directory: TempDir,
    options: [&'static str; 4],
    root: PathBuf,
    catalog: PathBuf,
    artifact: PathBuf,
    dictionary: MiniDictionary,
}

impl Corpus {
    fn new(format: &'static str, segmentation: &'static str, text: &str, catalog: &str) -> Self {
        let options = ["--input-format", format, "--segmentation", segmentation];
        let dictionary = MiniDictionary::new();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("corpus");
        fs::write(&root, text).unwrap();
        let catalog_path = directory.path().join("scoped.lx");
        fs::write(&catalog_path, catalog).unwrap();
        let artifact = directory.path().join("corpus.lingcorpus");
        Self {
            _directory: directory,
            options,
            root,
            catalog: catalog_path,
            artifact,
            dictionary,
        }
    }

    fn direct(&self) -> Output {
        lingrep()
            .args(["--json", "matches", "--limit", "all"])
            .args(self.options)
            .arg("--patterns")
            .arg(&self.catalog)
            .arg("--dict-path")
            .arg(self.dictionary.path())
            .arg(&self.root)
            .output()
            .unwrap()
    }

    fn prepared(&self) -> Output {
        let build = lingrep()
            .args(["corpus", "build"])
            .args(self.options)
            .arg("--dict-path")
            .arg(self.dictionary.path())
            .arg("--output")
            .arg(&self.artifact)
            .arg(&self.root)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        lingrep_without_dictionary()
            .args(["--json", "matches", "--limit", "all"])
            .arg("--patterns")
            .arg(&self.catalog)
            .arg("--corpus")
            .arg(&self.artifact)
            .output()
            .unwrap()
    }
}

/// The in-document ordinals of the sentences that each pattern matched.
fn matched(output: &Output) -> BTreeSet<(String, u64)> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|record| record["type"] == "match")
        .map(|record| {
            let data = &record["data"];
            (
                data["pattern"].as_str().unwrap().to_owned(),
                data["document_sentence_ordinal"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn warnings(output: &Output) -> Vec<(String, String)> {
    parse_jsonl(&output.stderr)
        .into_iter()
        .map(|record| {
            let data = &record["data"];
            (
                data["code"].as_str().unwrap().to_owned(),
                data["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Sentences 0 and 1 share a paragraph, sentence 2 ends the first section
/// before its subsection, sentence 3 is alone in that subsection, and
/// sentence 4 is alone in the second section. Under analyzer segmentation,
/// which `corpus build` rejects for Markdown, sentences 0 and 1 are one
/// input line.
#[test]
fn a_division_window_ends_at_the_division_on_both_paths() {
    let expected = BTreeSet::from(
        [
            ("document", 0),
            ("document", 1),
            ("document", 2),
            ("document", 3),
            ("section", 0),
            ("section", 1),
            ("paragraph", 0),
        ]
        .map(|(pattern, ordinal)| (pattern.to_owned(), ordinal)),
    );
    for (segmentation, first_paragraph, prepared) in [
        ("input", "猫が鳥を見る。\n犬も見る。", true),
        ("analyzer", "猫が鳥を見る。犬も見る。", false),
    ] {
        let corpus = Corpus::new(
            "markdown",
            segmentation,
            &format!(
                "# First\n\n{first_paragraph}\n\n鳥が猫を見る。\n\n## Inner\n\n猫が来る。\n\n\
                 # Second\n\n犬が来る。\n"
            ),
            CATALOG,
        );
        let mut outputs = vec![corpus.direct()];
        if prepared {
            outputs.push(corpus.prepared());
        }
        for output in outputs {
            assert_eq!(matched(&output), expected, "{segmentation}");
            assert_eq!(warnings(&output), [], "{segmentation}");
        }
    }
}

/// Plain text does not observe paragraphs, so a `within=paragraph`
/// lookaround matches sentence-locally and the run warns.
#[test]
fn a_division_kind_the_corpus_does_not_observe_warns_on_both_paths() {
    let corpus = Corpus::new(
        "plain",
        "input",
        "猫が鳥を見る。犬も見る。\n",
        "pattern paragraph := [] $ followed_by([], within=paragraph).",
    );
    for output in [corpus.direct(), corpus.prepared()] {
        assert_eq!(matched(&output), BTreeSet::new());
        let warnings = warnings(&output);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].0, "division_unavailable");
        assert!(warnings[0].1.contains("`within=paragraph`"), "{warnings:?}");
    }
}
