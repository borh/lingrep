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
//! - `locations.v1` is pinned by its bytes, and `tokens.v1.zst`,
//!   `postings.v1` and `provenance.sqlite` by their decoded content, which
//!   does not depend on the zstd or SQLite version that wrote them.
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

    // Components: locations.v1 is written by lingrep alone, so its bytes are
    // pinned. The other three contain zstd or SQLite output and are pinned by
    // their decoded content in the tests below.
    let components = manifest["components"].as_object().unwrap();
    assert_eq!(
        components.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "locations.v1",
            "postings.v1",
            "provenance.sqlite",
            "tokens.v1.zst"
        ]
    );
    assert_eq!(components["locations.v1"]["length"], 376);
    assert_eq!(
        components["locations.v1"]["blake3"],
        "5f297ddda32ff505c0874eedede8381d0b661834f0fd87d92f2440b5bb6af975"
    );
}

#[test]
fn the_prepared_tokens_decode_to_the_pinned_records() {
    let (_directory, artifact) = build_fixture_artifact();
    let output = lingrep()
        .args(["tokenize", "--json", "--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = Vec::new();
    for record in String::from_utf8(output.stdout).unwrap().lines() {
        let record: serde_json::Value = serde_json::from_str(record).unwrap();
        if record["type"] != "sentence" {
            continue;
        }
        let sentence = &record["data"];
        for token in sentence["tokens"].as_array().unwrap() {
            let attributes = token["attributes"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| format!("{key}={}", value.as_str().unwrap()))
                .collect::<Vec<_>>();
            lines.push(format!(
                "{} {} {}:{} {} {}",
                sentence["corpus_sentence_ordinal"],
                sentence["source_name"].as_str().unwrap(),
                sentence["document_ordinal"],
                sentence["document_sentence_ordinal"],
                token["index"],
                attributes.join(" ")
            ));
        }
    }
    assert_eq!(lines, EXPECTED_TOKENS);

    let bytes = fs::read(artifact.join("tokens.v1.zst")).unwrap();
    assert_eq!(
        decompressed_tokens_digest(&bytes),
        "ac47641caa5f9557f38e92c4de395a4e8345c0711111415b0f0f8d6ab0f5d8b5"
    );
}

const EXPECTED_TOKENS: &[&str] = &[
    "0 a.md 0:0 0 lemma=猫 norm=猫 text=猫 unidic.cform=* unidic.ctype=* unidic.goshu=和 unidic.iform=* unidic.itype=* unidic.lextype=system unidic.lform=ネコ unidic.orthbase=猫 unidic.pos1=名詞 unidic.pos2=普通名詞 unidic.pos3=一般 unidic.pos4=* unidic.pron=ネコ unidic.pronbase=ネコ upos=NOUN xpos=名詞-普通名詞-一般",
    "0 a.md 0:0 1 lemma=が norm=が text=が unidic.cform=* unidic.ctype=* unidic.goshu=和 unidic.iform=* unidic.itype=* unidic.lextype=system unidic.lform=ガ unidic.orthbase=が unidic.pos1=助詞 unidic.pos2=格助詞 unidic.pos3=* unidic.pos4=* unidic.pron=ガ unidic.pronbase=ガ upos=ADP xpos=助詞-格助詞",
    "0 a.md 0:0 2 lemma=来る。 norm=来る。 text=来る。 unidic.cform=* unidic.ctype=* unidic.lextype=unknown unidic.orthbase=来る。 unidic.pos1=名詞 unidic.pos2=普通名詞 unidic.pos3=一般 unidic.pos4=* upos=NOUN xpos=名詞-普通名詞-一般",
    "1 a.md 0:1 0 lemma=猫 norm=猫 text=猫 unidic.cform=* unidic.ctype=* unidic.goshu=和 unidic.iform=* unidic.itype=* unidic.lextype=system unidic.lform=ネコ unidic.orthbase=猫 unidic.pos1=名詞 unidic.pos2=普通名詞 unidic.pos3=一般 unidic.pos4=* unidic.pron=ネコ unidic.pronbase=ネコ upos=NOUN xpos=名詞-普通名詞-一般",
    "1 a.md 0:1 1 lemma=が norm=が text=が unidic.cform=* unidic.ctype=* unidic.goshu=和 unidic.iform=* unidic.itype=* unidic.lextype=system unidic.lform=ガ unidic.orthbase=が unidic.pos1=助詞 unidic.pos2=格助詞 unidic.pos3=* unidic.pos4=* unidic.pron=ガ unidic.pronbase=ガ upos=ADP xpos=助詞-格助詞",
    "1 a.md 0:1 2 lemma=寝る。 norm=寝る。 text=寝る。 unidic.cform=* unidic.ctype=* unidic.lextype=unknown unidic.orthbase=寝る。 unidic.pos1=名詞 unidic.pos2=普通名詞 unidic.pos3=一般 unidic.pos4=* upos=NOUN xpos=名詞-普通名詞-一般",
    "2 b.md 1:0 0 lemma=犬 norm=犬 text=犬 unidic.cform=* unidic.ctype=* unidic.lextype=system unidic.lform=イヌ unidic.pos1=名詞 unidic.pos2=普通名詞 unidic.pos3=一般 unidic.pos4=* upos=NOUN xpos=名詞-普通名詞-一般",
    "2 b.md 1:0 1 lemma=が norm=が text=が unidic.cform=* unidic.ctype=* unidic.goshu=和 unidic.iform=* unidic.itype=* unidic.lextype=system unidic.lform=ガ unidic.orthbase=が unidic.pos1=助詞 unidic.pos2=格助詞 unidic.pos3=* unidic.pos4=* unidic.pron=ガ unidic.pronbase=ガ upos=ADP xpos=助詞-格助詞",
    "2 b.md 1:0 2 lemma=走る norm=走る text=走る unidic.cform=終止形-一般 unidic.ctype=五段-ラ行 unidic.goshu=和 unidic.iform=基本形 unidic.itype=ハ濁 unidic.lextype=system unidic.lform=ハシル unidic.orthbase=走る unidic.pos1=動詞 unidic.pos2=一般 unidic.pos3=* unidic.pos4=* unidic.pron=ハシル unidic.pronbase=ハシル upos=VERB xpos=動詞-一般",
    "2 b.md 1:0 3 lemma=。 norm=。 text=。 unidic.cform=* unidic.ctype=* unidic.goshu=記号 unidic.iform=* unidic.itype=* unidic.lextype=system unidic.lform=* unidic.orthbase=。 unidic.pos1=補助記号 unidic.pos2=句点 unidic.pos3=* unidic.pos4=* unidic.pron=* unidic.pronbase=* upos=PUNCT xpos=補助記号-句点",
];

/// BLAKE3 over `tokens.v1.zst` with each block replaced by its decompressed
/// bytes and the fields that depend on compressed sizes left out: the header's
/// directory and residency offsets, each directory entry's file offset, stored
/// length and checksum, and the footer checksum.
fn decompressed_tokens_digest(bytes: &[u8]) -> String {
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
    assert_eq!(&bytes[..8], b"LINGTOK1");
    let schema_end = 128 + u32_at(20);
    let sentence_count = u64_at(88);
    let block_count = u64_at(104);
    let directory = u64_at(112);
    let residency = u64_at(120);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&bytes[..112]);
    hasher.update(&bytes[128..schema_end]);
    let mut payload_end = schema_end;
    for block in 0..block_count {
        let entry = directory + 80 * block;
        let (offset, stored, raw_len) =
            (u64_at(entry + 16), u64_at(entry + 24), u64_at(entry + 32));
        assert_eq!(offset, payload_end, "block {block} is not contiguous");
        let raw = zstd::bulk::decompress(&bytes[offset..offset + stored], raw_len).unwrap();
        assert_eq!(raw.len(), raw_len);
        hasher.update(&bytes[entry..entry + 16]);
        hasher.update(&bytes[entry + 32..entry + 48]);
        hasher.update(&raw);
        payload_end = offset + stored;
    }
    assert_eq!(directory, payload_end);
    assert_eq!(residency, directory + 80 * block_count);
    let footer = residency + 8 * sentence_count;
    assert_eq!(footer + 48, bytes.len());
    hasher.update(&bytes[residency..footer]);
    hasher.update(&bytes[footer..footer + 8]);
    hasher.update(&bytes[footer + 40..]);
    hasher.finalize().to_hex().to_string()
}

#[test]
fn the_prepared_postings_decode_to_the_pinned_lists() {
    let (_directory, artifact) = build_fixture_artifact();
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(artifact.join("manifest.json")).unwrap()).unwrap();
    let bytes = fs::read(artifact.join("postings.v1")).unwrap();
    assert_eq!(&bytes[..8], b"LGPOST01");
    assert_eq!(&bytes[bytes.len() - 8..], b"LGPOST01");
    let footer = bytes.len() - 48;
    let size = u64::from_le_bytes(bytes[footer..footer + 8].try_into().unwrap()) as usize;
    let mut directory = Cursor {
        data: &bytes[footer - size..footer],
        at: 0,
    };
    let hex = |b: &[u8]| blake3::Hash::from_slice(b).unwrap().to_hex().to_string();
    assert_eq!(hex(directory.take(32)), manifest["analysis_identity"]);
    assert_eq!(hex(directory.take(32)), manifest["token_schema_identity"]);
    assert_eq!(
        hex(directory.take(32)),
        manifest["components"]["tokens.v1.zst"]["blake3"]
    );
    assert_eq!(directory.number(), 3, "sentence count");
    // The total and per-entry covered byte counts sum compressed token blocks.
    let _total_bytes = directory.number();
    let coverage = (0..directory.number())
        .map(|_| directory.string())
        .collect::<Vec<_>>();
    assert_eq!(coverage, ["lemma", "unidic.pos1"]);
    let mut lines = Vec::new();
    let mut payload_end = 8;
    for _ in 0..directory.number() {
        let attribute = directory.string();
        let value = directory.string();
        let frequency = directory.number();
        let _covered_bytes = directory.number();
        let mut sentences = Vec::new();
        for _ in 0..directory.number() {
            let offset = directory.number() as usize;
            let stored = directory.number() as usize;
            let raw_len = directory.number() as usize;
            let (count, first, last) = (directory.number(), directory.number(), directory.number());
            let _checksum = directory.take(32);
            assert_eq!(offset, payload_end);
            payload_end = offset + stored;
            let raw = zstd::bulk::decompress(&bytes[offset..payload_end], raw_len).unwrap();
            assert_eq!(raw.len(), raw_len);
            let chunk = delta_varints(&raw);
            assert_eq!(chunk.len() as u64, count);
            assert_eq!((chunk[0], chunk[chunk.len() - 1]), (first, last));
            sentences.extend(chunk);
        }
        assert_eq!(sentences.len() as u64, frequency);
        lines.push(format!("{attribute} {value} {sentences:?}"));
    }
    assert_eq!(directory.at, size);
    assert_eq!(payload_end, footer - size);
    assert_eq!(lines, EXPECTED_POSTINGS);
}

const EXPECTED_POSTINGS: &[&str] = &[
    "lemma 。 [2]",
    "lemma が [0, 1, 2]",
    "lemma 寝る。 [1]",
    "lemma 来る。 [0]",
    "lemma 犬 [2]",
    "lemma 猫 [0, 1]",
    "lemma 走る [2]",
    "unidic.pos1 助詞 [0, 1, 2]",
    "unidic.pos1 動詞 [2]",
    "unidic.pos1 名詞 [0, 1, 2]",
    "unidic.pos1 補助記号 [2]",
];

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        self.at += n;
        &self.data[self.at - n..self.at]
    }

    fn number(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().unwrap())
    }

    fn string(&mut self) -> String {
        let n = self.number() as usize;
        String::from_utf8(self.take(n).to_vec()).unwrap()
    }
}

/// Sentence ordinals stored as LEB128 deltas from the previous ordinal.
fn delta_varints(raw: &[u8]) -> Vec<u64> {
    let mut values = Vec::new();
    let (mut value, mut shift, mut previous) = (0_u64, 0, 0_u64);
    for byte in raw {
        value |= u64::from(byte & 127) << shift;
        shift += 7;
        if byte & 128 == 0 {
            previous += value;
            values.push(previous);
            (value, shift) = (0, 0);
        }
    }
    assert_eq!(shift, 0, "truncated delta");
    values
}

#[test]
fn the_prepared_provenance_reads_back_the_pinned_rows() {
    use lingrep_core::analysis::{NormalizationPolicy, TextUnitKind};
    use lingrep_core::corpus::{Division, DivisionId, DivisionObservation, StructuralContext};
    use lingrep_corpus::{
        CorpusArtifact, DivisionMetadataRow, DocumentId, DocumentMetadataRow, RichSentenceMetadata,
        SentenceId, SentenceLocation, SentenceProvenance, StoredSentenceAnalysis,
        StoredSurfaceAlignment,
    };
    use std::sync::Arc;

    let (_directory, artifact) = build_fixture_artifact();
    let artifact = CorpusArtifact::open(&artifact).unwrap();
    let reader = artifact.provenance_reader().unwrap();
    let view = reader.view();
    let paragraph = |id: u64| {
        Arc::new(Division {
            source_id: None,
            id: DivisionId(id),
            document_id: DocumentId::new(id),
            kind: "paragraph".to_owned(),
            parent_id: None,
            metadata: Box::default(),
        })
    };
    let documents = [(0, 0, 2, "一"), (1, 2, 1, "二")];
    // (source, document, document sentence, text, surface byte ranges, input identity)
    let sentences: [(&str, u64, u64, &str, &[(u32, u32)], &str); 3] = [
        (
            "a.md",
            0,
            0,
            "猫が来る。",
            &[(0, 3), (3, 6), (6, 15)],
            "030787cc8ecd319bab161d9133d88338460c96515aa4d2612b3b255ff19f7559",
        ),
        (
            "a.md",
            0,
            1,
            "猫が寝る。",
            &[(0, 3), (3, 6), (6, 15)],
            "e5687d832fba4aed5eb769cca8067fa14d33bede6346e18e9c0240bca5e91dcf",
        ),
        (
            "b.md",
            1,
            0,
            "犬が走る。",
            &[(0, 3), (3, 6), (6, 12), (12, 15)],
            "54cef58b6233ebd461f96621deb88885eb353a82be7cca0b5d675653435cad99",
        ),
    ];
    reader.block_on(async {
        assert_eq!(
            view.document_metadata_page(0).await.unwrap(),
            documents.map(
                |(id, first_sentence, sentence_count, title)| DocumentMetadataRow {
                    document_id: DocumentId::new(id),
                    first_sentence,
                    sentence_count,
                    metadata_json: Some(format!(r#"{{"title":"{title}"}}"#)),
                }
            )
        );
        assert_eq!(
            view.division_metadata_page(0).await.unwrap(),
            documents.map(
                |(id, first_sentence, sentence_count, _)| DivisionMetadataRow {
                    division: paragraph(id),
                    first_sentence,
                    end_sentence: first_sentence + sentence_count,
                }
            )
        );
        assert_eq!(
            view.division_observation_counts().await.unwrap(),
            [
                ("paragraph".to_owned(), 2),
                ("region".to_owned(), 2),
                ("section".to_owned(), 2)
            ]
        );
        assert_eq!(
            view.section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
                .await
                .unwrap(),
            []
        );
        assert!(view.sentence_metadata_keys().await.unwrap().is_empty());
        assert_eq!(view.sentence_metadata_page(0).await.unwrap(), []);
        for (ordinal, (source, document, document_sentence, text, surfaces, input)) in
            (0_u64..).zip(sentences)
        {
            let title = documents[document as usize].3;
            assert_eq!(
                artifact.sentence_provenance(view, ordinal).await.unwrap(),
                SentenceProvenance {
                    location: SentenceLocation {
                        corpus_sentence_ordinal: ordinal,
                        document_id: DocumentId::new(document),
                        document_ordinal: document,
                        document_sentence_ordinal: document_sentence,
                        source_name: source.into(),
                        source_metadata: None,
                    },
                    rich: RichSentenceMetadata {
                        structure: Arc::new(StructuralContext {
                            observations: ["paragraph", "region", "section"]
                                .map(|kind| (kind.to_owned(), DivisionObservation::Observed))
                                .into(),
                            divisions: Box::new([paragraph(document)]),
                        }),
                        region_labels: Box::default(),
                        metadata_json: Some(format!(r#"{{"title":"{title}"}}"#)),
                        sentence_metadata_json: None,
                    },
                    active_regions: Box::default(),
                },
                "sentence {ordinal}"
            );
            assert_eq!(
                view.sentence_analysis(SentenceId::new(ordinal))
                    .await
                    .unwrap(),
                StoredSentenceAnalysis {
                    text: text.to_owned(),
                    surfaces: (0_u32..)
                        .zip(surfaces)
                        .map(|(word, &(start, end))| StoredSurfaceAlignment {
                            source_range: start..end,
                            word_range: word..word + 1,
                        })
                        .collect(),
                    input_unit_id: Some(ordinal),
                    input_kind: Some(TextUnitKind::Sentence),
                    normalization: Some(NormalizationPolicy::Nfc),
                    input_identity: Some(*blake3::Hash::from_hex(input).unwrap().as_bytes()),
                    source_range: Some(0..15),
                },
                "sentence {ordinal}"
            );
        }
    });
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
