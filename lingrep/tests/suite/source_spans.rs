// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! Vibrato match records over variant spellings: `char_span` addresses the
//! admitted sentence text in code points, and `surface` is that slice. The
//! default admits the source unchanged; `--normalization nfc` admits its NFC
//! form.

use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::{assert_success, lingrep, lingrep_without_dictionary, parse_jsonl};

/// One sentence per line: an ideographic variation sequence, half-width
/// katakana, decomposed が, 臭氣 spelled with U+FA5C as UniDic lists it, and
/// U+FF5E between kana, and a letter followed by two combining macrons.
const LINES: [&str; 6] = [
    "葛\u{E0100}城市に住む。",
    "ｶﾞﾗｽの靴。",
    "か\u{3099}まんする。",
    "\u{FA5C}氣がする。",
    "長～い道。",
    "U\u{0304}\u{0304}",
];

/// A token's sentence, its code-point span in the admitted sentence, and its
/// `text` key.
type Token = (u64, (u64, u64), &'static str);

/// Every token under the default: spans address the source, so every variant
/// spelling stays inside one token whose `surface` is the source slice.
const SOURCE_TOKENS: [Token; 21] = [
    (0, (0, 3), "葛城"),
    (0, (3, 4), "市"),
    (0, (4, 5), "に"),
    (0, (5, 7), "住む"),
    (0, (7, 8), "。"),
    (1, (0, 4), "ガラス"),
    (1, (4, 5), "の"),
    (1, (5, 6), "靴"),
    (1, (6, 7), "。"),
    (2, (0, 4), "がまん"),
    (2, (4, 6), "する"),
    (2, (6, 7), "。"),
    (3, (0, 2), "臭氣"),
    (3, (2, 3), "が"),
    (3, (3, 5), "する"),
    (3, (5, 6), "。"),
    (4, (0, 3), "長〜い"),
    (4, (3, 4), "道"),
    (4, (4, 5), "。"),
    (5, (0, 2), "Ū"),
    (5, (2, 3), "\u{0304}"),
];

/// Every token under `--normalization nfc`: が is one code point, and U+FA5C
/// becomes U+81ED before analysis, which UniDic does not list with 氣.
const NFC_TOKENS: [Token; 22] = [
    (0, (0, 3), "葛城"),
    (0, (3, 4), "市"),
    (0, (4, 5), "に"),
    (0, (5, 7), "住む"),
    (0, (7, 8), "。"),
    (1, (0, 4), "ガラス"),
    (1, (4, 5), "の"),
    (1, (5, 6), "靴"),
    (1, (6, 7), "。"),
    (2, (0, 3), "がまん"),
    (2, (3, 5), "する"),
    (2, (5, 6), "。"),
    (3, (0, 1), "臭"),
    (3, (1, 2), "氣"),
    (3, (2, 3), "が"),
    (3, (3, 5), "する"),
    (3, (5, 6), "。"),
    (4, (0, 3), "長〜い"),
    (4, (3, 4), "道"),
    (4, (4, 5), "。"),
    (5, (0, 1), "Ū"),
    (5, (1, 2), "\u{0304}"),
];

/// The complete record `t := [].` publishes for each token, with `surface`
/// sliced from `sentences` by the token's span.
fn expected(sentences: &[String], tokens: &[Token]) -> Vec<Value> {
    let mut index = 0;
    tokens
        .iter()
        .enumerate()
        .map(|(sequence, &(sentence, (start, end), text))| {
            if sequence > 0 && tokens[sequence - 1].0 != sentence {
                index = 0;
            }
            let surface: String = sentences[sentence as usize]
                .chars()
                .skip(start as usize)
                .take((end - start) as usize)
                .collect();
            let record = json!({
                "sequence": sequence,
                "pattern": "t",
                "span": {"start": index, "end": index + 1},
                "char_span": {"start": start, "end": end},
                "captures": [],
                "corpus_sentence_ordinal": sentence,
                "source_name": "corpus.txt",
                "document_ordinal": 0,
                "document_sentence_ordinal": sentence,
                "surface": surface,
                "tokens": [{"index": index, "attributes": {"text": text}}],
                "left_context": null,
                "right_context": null,
            });
            index += 1;
            record
        })
        .collect()
}

fn records(output: &std::process::Output) -> Vec<Value> {
    assert_success(output);
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| event["type"] == "match")
        .map(|event| event["data"].clone())
        .collect()
}

fn direct(corpus: &Path, normalization: &[&str]) -> Vec<Value> {
    records(
        &lingrep()
            .args(["--json", "matches", "--input-format", "plain"])
            .args(normalization)
            .args(["--pattern", "t := []."])
            .arg(corpus)
            .output()
            .unwrap(),
    )
}

fn prepared(directory: &Path, corpus: &Path, normalization: &[&str]) -> Vec<Value> {
    let artifact = directory.join(format!("{}.lingcorpus", normalization.concat()));
    assert_success(
        &lingrep()
            .args(["corpus", "build", "--input-format", "plain"])
            .args(normalization)
            .arg("--output")
            .arg(&artifact)
            .arg(corpus)
            .output()
            .unwrap(),
    );
    records(
        &lingrep_without_dictionary()
            .args(["--json", "matches", "--pattern", "t := [].", "--corpus"])
            .arg(&artifact)
            .output()
            .unwrap(),
    )
}

#[test]
fn char_span_addresses_the_source_by_default_and_the_nfc_text_when_asked() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("corpus.txt");
    std::fs::write(&corpus, LINES.join("\n") + "\n").unwrap();
    let source: Vec<String> = LINES.iter().map(|line| (*line).to_owned()).collect();
    let mut nfc = source.clone();
    nfc[2] = "がまんする。".to_owned();
    nfc[3] = "\u{81ED}氣がする。".to_owned();

    nfc[5] = "Ū\u{0304}".to_owned();

    for (normalization, sentences, tokens) in [
        (&[][..], &source, &SOURCE_TOKENS[..]),
        (
            &["--normalization", "none"][..],
            &source,
            &SOURCE_TOKENS[..],
        ),
        (&["--normalization", "nfc"][..], &nfc, &NFC_TOKENS[..]),
    ] {
        let expected = expected(sentences, tokens);
        assert_eq!(
            direct(&corpus, normalization),
            expected,
            "direct {normalization:?}"
        );
        assert_eq!(
            prepared(directory.path(), &corpus, normalization),
            expected,
            "prepared {normalization:?}"
        );
    }
}
