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
//! Text files written by Japanese Windows tools: Notepad and Excel prefix
//! UTF-8 with a byte-order mark and end lines with CRLF.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

use lingrep_core::testing::MiniDictionary;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{SORANOHA_CATALOG, fixture, lingrep, parse_jsonl};

const BOM: &str = "\u{feff}";

/// Every one-token match in `input`, so each record carries a token's
/// attributes and its character offsets in the sentence.
fn token_matches(dictionary: &MiniDictionary, input: &Path, extra: &[&str]) -> Vec<Value> {
    let output = lingrep()
        .args(["--json", "matches", "--pattern", "w := [].", "--dict-path"])
        .arg(dictionary.path())
        .args(extra)
        .arg(input)
        .output()
        .unwrap();
    records(&output, "match")
}

fn records(output: &Output, kind: &str) -> Vec<Value> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| event["type"] == kind)
        .map(|event| event["data"].clone())
        .collect()
}

/// Writes `content` as `name` in a fresh directory under `root`, so two
/// variants of one input share a source name.
fn write_named(root: &Path, variant: &str, name: &str, content: &[u8]) -> PathBuf {
    let directory = root.join(variant);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

fn soranoha_archive(path: &Path, member: &str) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (entry, content) in [
        ("catalog.csv", SORANOHA_CATALOG),
        ("kumo.txt", member),
        ("hari.txt", "犬が走る。\r\n"),
    ] {
        writer
            .start_file(entry, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

#[test]
fn a_leading_byte_order_mark_yields_the_same_matches_in_every_text_format() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let conllu = std::fs::read_to_string(fixture("ud-ja-gsd-sample.conllu")).unwrap();
    let cases: [(&str, String, &[&str]); 6] = [
        (
            "notepad.txt",
            crlf("猫が来る。犬が走る。\n猫が寝る。\n"),
            &[],
        ),
        (
            "notepad.md",
            crlf("---\ntitle: 猫\n---\n\n猫が来る。\n"),
            &[],
        ),
        (
            "notepad.jsonl",
            crlf("{\"title\":\"猫\",\"text\":\"猫が来る。\"}\n"),
            &[],
        ),
        ("notepad.conllu", crlf(&conllu), &[]),
        (
            "notepad.xml",
            crlf(
                "<TEI xmlns=\"http://www.tei-c.org/ns/1.0\"><text><body><p>猫が来る。</p></body></text></TEI>\n",
            ),
            &["--input-format", "tei"],
        ),
        (
            "notepad.txt",
            crlf("猫が来る。\n"),
            &["--input-format", "plain"],
        ),
    ];
    for (name, text, extra) in cases {
        let without = write_named(directory.path(), "without", name, text.as_bytes());
        let with = write_named(
            directory.path(),
            "with",
            name,
            format!("{BOM}{text}").as_bytes(),
        );
        let expected = token_matches(&dictionary, &without, extra);
        assert!(!expected.is_empty(), "{name}");
        assert_eq!(
            token_matches(&dictionary, &with, extra),
            expected,
            "{name} {extra:?}"
        );
    }

    let without = directory.path().join("without.zip");
    let with = directory.path().join("with.zip");
    soranoha_archive(&without, "猫が文を鳴く。\r\n");
    soranoha_archive(&with, &format!("{BOM}猫が文を鳴く。\r\n"));
    let soranoha = ["--input-format", "soranoha"];
    assert_eq!(
        token_matches(&dictionary, &with, &soranoha),
        token_matches(&dictionary, &without, &soranoha),
    );
}

#[test]
fn a_corpus_built_from_a_notepad_file_stores_the_records_of_the_same_text_without_a_mark() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let text = crlf("吾輩は猫である。\n名前はまだ無い。\n");
    let records = |variant: &str, content: String| {
        let input = write_named(directory.path(), variant, "notepad.txt", content.as_bytes());
        let artifact = directory.path().join(format!("{variant}.lingcorpus"));
        lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("--output")
            .arg(&artifact)
            .arg(&input)
            .assert()
            .success();
        let output = lingrep()
            .args(["--json", "tokenize", "--corpus"])
            .arg(&artifact)
            .output()
            .unwrap();
        records(&output, "sentence")
    };
    assert_eq!(
        records("with", format!("{BOM}{text}")),
        records("without", text)
    );
}

/// `ascii\r\n猫が～①髙を見た。\r\n` as Windows saves it in CP932. ～ (81 60),
/// ① (87 40) and 髙 (FB FC) are CP932 extensions, so a strict JIS X 0208
/// Shift_JIS decoder rejects this file and the WHATWG Shift_JIS decoder
/// accepts it.
const CP932: &[u8] =
    b"ascii\r\n\x94\x4c\x82\xaa\x81\x60\x87\x40\xfb\xfc\x82\xf0\x8c\xa9\x82\xbd\x81\x42\r\n";

fn failure(output: &Output) -> String {
    assert!(!output.status.success(), "{output:?}");
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn assert_shift_jis_hint(stderr: &str, position: &str) {
    for expected in [
        position,
        "looks like Shift_JIS (CP932)",
        "iconv -f CP932 -t UTF-8 INPUT > OUTPUT",
        "nkf -w INPUT > OUTPUT",
    ] {
        assert!(stderr.contains(expected), "{expected}: {stderr}");
    }
}

#[test]
fn a_cp932_source_names_its_first_invalid_byte_and_the_conversion() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    for name in ["sjis.txt", "sjis.md"] {
        let input = write_named(directory.path(), "cp932", name, CP932);
        let matches = lingrep()
            .args(["matches", "--pattern", "w := [].", "--dict-path"])
            .arg(dictionary.path())
            .arg(&input)
            .output()
            .unwrap();
        let stderr = failure(&matches);
        assert!(stderr.contains(name), "{stderr}");
        assert_shift_jis_hint(&stderr, "line 2, byte offset 7");

        let build = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("--output")
            .arg(directory.path().join(format!("{name}.lingcorpus")))
            .arg(&input)
            .output()
            .unwrap();
        assert_shift_jis_hint(&failure(&build), "line 2, byte offset 7");
    }
}

#[test]
fn invalid_utf8_after_utf8_text_names_its_position_without_a_shift_jis_hint() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let mut bytes = "猫が来る。\n犬が".as_bytes().to_vec();
    bytes.extend_from_slice(b"\x94\x4c\n");
    let input = write_named(directory.path(), "mixed", "mixed.txt", &bytes);
    let stderr = failure(
        &lingrep()
            .args(["matches", "--pattern", "w := [].", "--dict-path"])
            .arg(dictionary.path())
            .arg(&input)
            .output()
            .unwrap(),
    );
    assert!(stderr.contains("line 2, byte offset 22"), "{stderr}");
    assert!(!stderr.contains("Shift_JIS"), "{stderr}");
}

#[test]
fn a_cp932_catalog_names_its_first_invalid_byte_and_the_conversion() {
    let directory = TempDir::new().unwrap();
    let mut catalog = b"# \x94\x4c\r\n".to_vec();
    catalog.extend_from_slice(b"n := [upos=NOUN].\r\n");
    let path = write_named(directory.path(), "cp932", "sjis.lx", &catalog);
    let stderr = failure(
        &lingrep()
            .args(["check", "--patterns"])
            .arg(&path)
            .output()
            .unwrap(),
    );
    assert!(stderr.contains("sjis.lx"), "{stderr}");
    assert_shift_jis_hint(&stderr, "line 1, byte offset 2");
}

#[test]
fn a_zip_read_as_plain_text_says_to_unzip_it_or_read_it_as_soranoha() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let archive = directory.path().join("works.zip");
    soranoha_archive(&archive, "猫が文を鳴く。\r\n");
    let stderr = failure(
        &lingrep()
            .args([
                "matches",
                "--pattern",
                "w := [].",
                "--input-format",
                "plain",
                "--dict-path",
            ])
            .arg(dictionary.path())
            .arg(&archive)
            .output()
            .unwrap(),
    );
    for expected in [
        "works.zip",
        "ZIP archive",
        "unzip",
        "--input-format soranoha",
    ] {
        assert!(stderr.contains(expected), "{expected}: {stderr}");
    }
}
