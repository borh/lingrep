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
//! Golden fingerprint baseline for the prepared-artifact pipeline.
//!
//! Every literal here must remain byte-identical unless the corresponding
//! identity contract changes:
//!
//! - `source_identity` and `ordered_inventory_digest` pin the observation
//!   **order** (two documents swapped produce the same content digest set
//!   and a different `source_identity`);
//! - `analysis_identity` / `reader_identity` / `admission_identity` pin the
//!   identity derivation the seams feed;
//! - every `components` entry pins the per-file content hashes and lengths.
//!
//! The fixture is the smallest multi-document Markdown corpus with
//! frontmatter: two documents, three sentences, built through the real
//! `corpus build` command against the hermetic `MiniDictionary`.

use std::fs;

use lingrep_core::testing::MiniDictionary;

use crate::support::lingrep;

const DOC_A: &str = "---\ntitle: 一\n---\n\n猫が来る。\n猫が寝る。\n";
const DOC_B: &str = "---\ntitle: 二\n---\n\n犬が走る。\n";

fn build_fixture_artifact() -> (tempfile::TempDir, std::path::PathBuf) {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.md"), DOC_A).unwrap();
    fs::write(root.join("b.md"), DOC_B).unwrap();
    let artifact = directory.path().join("corpus.lingcorpus");
    let build = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .args(["--output"])
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    (directory, artifact)
}

#[test]
fn the_prepared_artifact_identity_literals_are_pinned() {
    let (_directory, artifact) = build_fixture_artifact();
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(artifact.join("manifest.json")).unwrap()).unwrap();

    // Identity — the bytes a later run compares against.
    assert_eq!(
        manifest["source_identity"],
        "11cd4420a2791ea4c347beb959291606a297a16b1d9cba6abf0bb34630c7c0da"
    );
    assert_eq!(
        manifest["analysis_identity"],
        "876020ea11414790f9f7b165bb5ac45d25d13471660d268ce1d444ccef6df527"
    );
    assert_eq!(
        manifest["reader_identity"],
        "e6e3551c916214f19813eca5e74f734f70598ba643888cad52b459516ccf9d53"
    );
    assert_eq!(
        manifest["admission_identity"],
        "d2965033d003b48ab4920b1a01a625ec5f1aafbf445cee721bf15d626b544bac"
    );
    assert_eq!(
        manifest["dictionary_fingerprint"],
        "2a29f7210a0a75456987d57d9b8a2a609896b54324bbb7e4e7cfaf14d7710cc0"
    );
    assert_eq!(
        manifest["token_schema_identity"],
        "0258eb71de9f5983c95b8a9fa9fc4bd23a883f0087a16cf26fd9a6f6b9eda7cc"
    );

    // Inventory — observation order made into bytes.
    assert_eq!(
        manifest["ordered_inventory_digest"],
        "fd606b705a567120339acda0eb3db88288a99b85e474171c94f409ad5a93c87b"
    );
    assert_eq!(manifest["document_count"], 2);
    assert_eq!(manifest["sentence_count"], 3);

    // Components — every per-file content hash and length.
    let components = manifest["components"].as_object().unwrap();
    let expected: &[(&str, u64, &str)] = &[
        // (name, length, blake3)
        (
            "locations.v1",
            376,
            "5f297ddda32ff505c0874eedede8381d0b661834f0fd87d92f2440b5bb6af975",
        ),
        (
            "postings.v1",
            1798,
            "4736438eb4e6955869a889f2ac381f890d3812b951f3c32e79b09feace9a6b3f",
        ),
        (
            "provenance.sqlite",
            73728,
            "6f6f25e0515ba18f37bb496935a00a269949295b4a31fadb3f15311db2b35ff9",
        ),
        (
            "tokens.v1.zst",
            1235,
            "25449f7f02475a1bc86f2a32ed73caa848d10c5947f14d1b691d212dc9c0133a",
        ),
    ];
    assert_eq!(components.len(), expected.len());
    for (name, length, blake3) in expected {
        let fact = components.get(*name).unwrap_or_else(|| {
            panic!(
                "component {name:?} missing; have {:?}",
                components.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(fact["length"], *length, "component {name}");
        assert_eq!(fact["blake3"], *blake3, "component {name}");
    }
}

#[test]
fn the_baseline_is_deterministic_across_builds() {
    // The literals above are only meaningful if the pipeline is stable:
    // build twice and compare every identity-bearing field.
    let (_directory_a, artifact_a) = build_fixture_artifact();
    let (_directory_b, artifact_b) = build_fixture_artifact();
    let read = |artifact: &std::path::Path| {
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(artifact.join("manifest.json")).unwrap())
                .unwrap();
        let mut fields: Vec<(String, serde_json::Value)> = Vec::new();
        for key in [
            "source_identity",
            "analysis_identity",
            "reader_identity",
            "admission_identity",
            "dictionary_fingerprint",
            "token_schema_identity",
            "ordered_inventory_digest",
            "admitted_inventory_digest",
            "document_count",
            "sentence_count",
            "token_count",
        ] {
            fields.push((key.to_owned(), manifest[key].clone()));
        }
        for (name, fact) in manifest["components"].as_object().unwrap() {
            fields.push((format!("component {name}"), fact.clone()));
        }
        fields
    };
    assert_eq!(
        read(&artifact_a),
        read(&artifact_b),
        "identity fields are build-stable"
    );
}

/// The literals are what `ling` 0.3.2 records for this fixture, so a CoNLL-U
/// artifact prepared by 0.3.2 is found and read by this build. A change to
/// CoNLL-U decoding replaces `DECODER_REVISION` in `lingrep-core`'s
/// `corpus/conllu.rs`, and then these literals.
#[test]
fn a_conllu_artifact_keeps_the_identity_0_3_2_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("s.conllu");
    fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ud-ja-gsd-sample.conllu"),
        &source,
    )
    .unwrap();
    let artifact = directory.path().join("s.lingcorpus");
    let build = lingrep()
        .args(["corpus", "build", "--output"])
        .arg(&artifact)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(artifact.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["source_identity"],
        "3fd391bb40e8065f0e96107b6b5768a5f92009056c8559c8fb7534e6c08179a6"
    );
    assert_eq!(
        manifest["token_schema_identity"],
        "61e60e9b20279b5e569551d399baf301721974b22ec6213d1403abebb37a6250"
    );
    assert_eq!(
        manifest["analysis_identity"],
        "59c0a3711d4b4669b19310dc3e44c418d76bd009d755ad0fbd422dc1617e9779"
    );
}
