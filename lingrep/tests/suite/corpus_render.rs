#![allow(clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use crate::support::{assert_success, lingrep};
use lingrep_core::corpus::{DocumentId, SentenceId};
use lingrep_core::testing::MiniDictionary;
use lingrep_corpus::{
    CorpusArtifact, PageRequest, ReadingSidecar, ReadingSidecarError, ReadingSidecarKey,
};

const FIRST: &str = "---\ntitle: 一\n---\n# 序論 {.introduction}\n\n最初の文です。\n本文<b>太字</b>です。\n\n| 項目 | 値 |\n|---|---|\n| 甲 | 一 | 余り |\n表の後の文です。\n\n::: {.note}\n注の文です。[^1]\n:::\n\n[^1]: 脚注の文です。\n";
const SECOND: &str = "# 方法\n\n- 一つ目の項目です。\n- 二つ目の項目です。\n\n> 引用の文です。\n\n<!-- 注釈 --> 注釈の後の文です。\n";

struct Fixture {
    _directory: tempfile::TempDir,
    _dictionary: MiniDictionary,
    root: PathBuf,
    artifact: PathBuf,
    scratch: PathBuf,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("papers");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), FIRST).unwrap();
    fs::write(root.join("b.md"), SECOND).unwrap();
    fs::write(root.join("c.md"), "# 見出しだけ\n").unwrap();
    let dictionary = MiniDictionary::new();
    let artifact = directory.path().join("papers.lingcorpus");
    assert_success(
        &lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("--output")
            .arg(&artifact)
            .arg(&root)
            .output()
            .unwrap(),
    );
    Fixture {
        scratch: directory.path().to_path_buf(),
        _directory: directory,
        _dictionary: dictionary,
        root,
        artifact,
    }
}

fn render(fixture: &Fixture, output: &Path, extra: &[&str]) -> std::process::Output {
    lingrep()
        .args(["corpus", "render"])
        .arg(&fixture.artifact)
        .arg("--source-root")
        .arg(&fixture.root)
        .arg("--output")
        .arg(output)
        .args(extra)
        .output()
        .unwrap()
}

fn files(directory: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

fn stored_texts(artifact: &CorpusArtifact) -> Vec<String> {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let view = artifact.provenance_view().await.unwrap();
        let mut texts = Vec::new();
        for sentence in 0..artifact.sentence_count() {
            texts.push(
                view.sentence_analysis(SentenceId::new(sentence))
                    .await
                    .unwrap()
                    .text,
            );
        }
        texts
    })
}

#[test]
fn two_renders_are_identical_and_every_sentence_anchors_in_its_page() {
    let fixture = fixture();
    let first = fixture.scratch.join("one.reading");
    let second = fixture.scratch.join("two.reading");
    assert_success(&render(&fixture, &first, &[]));
    assert_success(&render(&fixture, &second, &[]));
    assert_eq!(files(&first), files(&second));

    let artifact = CorpusArtifact::open(&fixture.artifact).unwrap();
    let sidecar =
        ReadingSidecar::open(&first, &ReadingSidecarKey::for_artifact(&artifact)).unwrap();
    sidecar.verify().unwrap();
    let texts = stored_texts(&artifact);
    assert_eq!(
        texts,
        [
            "最初の文です。",
            "本文太字です。",
            "表の後の文です。",
            "注の文です。[^1]",
            "一つ目の項目です。",
            "二つ目の項目です。",
            "引用の文です。",
            "注釈の後の文です。",
        ]
    );
    for (sentence, text) in texts.iter().enumerate() {
        let sentence = SentenceId::new(sentence as u64);
        let length = text.chars().count() as u32;
        let anchor = sidecar.anchor(sentence, 0..length).unwrap();
        assert_eq!(
            anchor.exact,
            !text.contains('|') && !text.contains("[^1]"),
            "{text}"
        );
        let page = sidecar
            .page(PageRequest::Around(sentence), 1 << 20)
            .unwrap();
        assert_eq!(page.document, anchor.document);
        assert!(page.blocks.contains(&anchor.block));
        let anchored = page.anchors.iter().find(|found| found.sentence == sentence);
        assert_eq!(
            anchored.map(|found| &found.ranges),
            (!anchor.ranges.is_empty()).then_some(&anchor.ranges)
        );
    }

    let page = sidecar
        .page(PageRequest::From(DocumentId::new(0), 0), 1 << 20)
        .unwrap();
    assert!(
        page.html.contains("本文&lt;b&gt;太字&lt;/b&gt;です。"),
        "{}",
        page.html
    );
    assert!(!page.html.contains("<b>"));
    assert!(page.html.contains("\">余り</td></tr>"), "{}", page.html);
    let after_table = page.html.split("</table>").nth(1).unwrap();
    assert!(after_table.starts_with("<p data-c="), "{}", page.html);
    assert!(
        after_table.contains("表の後の文です。</p>"),
        "{}",
        page.html
    );
    assert!(
        page.html
            .contains("<div class=\"note\" data-region=\"0\"><p")
    );
    assert_eq!((page.previous, page.next), (None, None));
}

#[test]
fn pages_cover_every_block_once_within_their_byte_bound() {
    let fixture = fixture();
    let output = fixture.scratch.join("papers.reading");
    assert_success(&render(&fixture, &output, &[]));
    let artifact = CorpusArtifact::open(&fixture.artifact).unwrap();
    let sidecar =
        ReadingSidecar::open(&output, &ReadingSidecarKey::for_artifact(&artifact)).unwrap();
    for document in 0..sidecar.document_count() {
        let document = DocumentId::new(document);
        let blocks = sidecar.document(document).unwrap().block_count;
        for bound in [0, 40, 120, 400] {
            let mut forward = Vec::new();
            let mut request = Some(PageRequest::From(document, 0));
            while let Some(next) = request {
                let page = sidecar.page(next, bound).unwrap();
                assert!(page.html.len() <= bound || page.blocks.len() == 1);
                forward.extend(page.blocks.clone());
                request = page.next;
            }
            assert_eq!(forward, (0..blocks).collect::<Vec<_>>(), "bound {bound}");
            let mut backward = Vec::new();
            let mut request = Some(PageRequest::Before(document, blocks));
            while let Some(previous) = request {
                let page = sidecar.page(previous, bound).unwrap();
                assert!(page.html.len() <= bound || page.blocks.len() == 1);
                backward.splice(0..0, page.blocks.clone());
                request = page.previous;
            }
            assert_eq!(backward, forward, "bound {bound}");
        }
    }
}

#[test]
fn open_rejects_a_key_that_differs_and_names_the_field() {
    let fixture = fixture();
    let output = fixture.scratch.join("papers.reading");
    assert_success(&render(&fixture, &output, &[]));
    let artifact = CorpusArtifact::open(&fixture.artifact).unwrap();
    let key = ReadingSidecarKey::for_artifact(&artifact);
    for (field, changed) in [
        (
            "analysis identity",
            ReadingSidecarKey {
                analysis_identity: "0".repeat(64),
                ..key.clone()
            },
        ),
        (
            "source identity",
            ReadingSidecarKey {
                source_identity: "0".repeat(64),
                ..key.clone()
            },
        ),
        (
            "renderer revision",
            ReadingSidecarKey {
                renderer_revision: "other".into(),
                ..key.clone()
            },
        ),
    ] {
        let Err(error) = ReadingSidecar::open(&output, &changed) else {
            panic!("{field} differs, and open accepted the sidecar");
        };
        assert!(
            matches!(&error, ReadingSidecarError::KeyMismatch { field: found, .. } if *found == field),
            "{error}"
        );
        assert!(error.to_string().contains(field), "{error}");
    }
}

#[test]
fn render_rejects_another_source_tree_or_other_reader_options() {
    let fixture = fixture();
    let output = fixture.scratch.join("papers.reading");

    let options = render(&fixture, &output, &["--md-exclude-sections", ""]);
    assert_eq!(options.status.code(), Some(2));
    let diagnostic = String::from_utf8_lossy(&options.stderr);
    assert!(
        diagnostic.contains("Markdown reader settings do not match the corpus"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("cannot recover"), "{diagnostic}");
    assert!(diagnostic.contains("NEW_ARTIFACT"), "{diagnostic}");

    fs::write(
        fixture.root.join("b.md"),
        format!("{SECOND}追加の文です。\n"),
    )
    .unwrap();
    let changed = render(&fixture, &output, &[]);
    assert!(!changed.status.success());
    assert!(
        String::from_utf8_lossy(&changed.stderr).contains("source identity"),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    assert!(!output.exists());
    assert_eq!(
        fs::read_dir(&fixture.scratch)
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("staging"))
            .count(),
        0
    );
}

#[test]
fn render_rejects_source_bytes_changed_after_observation() {
    let fixture = fixture();
    let artifact = CorpusArtifact::open(&fixture.artifact).unwrap();
    let observation = lingrep_corpus::SourceObservation::new();
    for name in ["a.md", "b.md", "c.md"] {
        observation.begin_source(name).unwrap();
        observation
            .update_source_bytes(&fs::read(fixture.root.join(name)).unwrap())
            .unwrap();
        observation.end_source().unwrap();
    }
    let observed = observation.finish().unwrap();
    assert_eq!(observed.source_identity(), artifact.source_identity());
    for name in ["a.md", "c.md"] {
        let output = fixture.scratch.join(format!("changed-{name}.reading"));
        let writer = lingrep_corpus::ReadingSidecarWriter::new(
            &artifact,
            lingrep_cli_common::MarkdownReaderArgs::default()
                .resolve()
                .unwrap(),
            &output,
        )
        .unwrap();
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(writer.write(
                &observed,
                |requested| {
                    let bytes = fs::read(fixture.root.join(requested))?;
                    if requested == name {
                        Ok(String::from_utf8(bytes)
                            .unwrap()
                            .replace("# ", "# changed ")
                            .into_bytes())
                    } else {
                        Ok(bytes)
                    }
                },
                &lingrep_core::execution::CancellationFlag::new(),
            ));
        assert!(
            matches!(result, Err(ReadingSidecarError::SourceMismatch { .. })),
            "changed {name} was accepted"
        );
        assert!(!output.exists());
        assert!(!fs::read_dir(&fixture.scratch).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("staging")
        }));
    }
}

#[test]
fn math_recovery_and_unprefixed_tables_preserve_prose_and_reading_anchors() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("papers");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("math.md"),
        "$$ x=1\n::: {.note}\nx=2 $$\n本文。\n",
    )
    .unwrap();
    fs::write(
        root.join("table.md"),
        "a | b\n--- | ---\n1 | 2\n表の後。\n後文。\n",
    )
    .unwrap();
    fs::write(
        root.join("unmatched.md"),
        "$$\n::: {.note}\n注。\n:::\n後文。\n",
    )
    .unwrap();
    let dictionary = MiniDictionary::new();
    let artifact = directory.path().join("papers.lingcorpus");
    assert_success(
        &lingrep()
            .args([
                "corpus",
                "build",
                "--md-recover-conversion-artifacts",
                "--dict-path",
            ])
            .arg(dictionary.path())
            .arg("--output")
            .arg(&artifact)
            .arg(&root)
            .output()
            .unwrap(),
    );
    let fixture = Fixture {
        scratch: directory.path().to_path_buf(),
        _directory: directory,
        _dictionary: dictionary,
        root,
        artifact,
    };
    let output = fixture.scratch.join("reading");
    assert_success(&render(
        &fixture,
        &output,
        &["--md-recover-conversion-artifacts"],
    ));
    let artifact = CorpusArtifact::open(&fixture.artifact).unwrap();
    assert_eq!(
        stored_texts(&artifact),
        ["本文。", "表の後。", "後文。", "$$", "注。", "後文。"]
    );
    let sidecar =
        ReadingSidecar::open(&output, &ReadingSidecarKey::for_artifact(&artifact)).unwrap();
    sidecar.verify().unwrap();
    for ordinal in 0..artifact.sentence_count() {
        assert!(
            sidecar
                .anchor(SentenceId::new(ordinal), 0..1)
                .unwrap()
                .exact
        );
    }
    let page = sidecar
        .page(PageRequest::From(DocumentId::new(1), 0), 1 << 20)
        .unwrap();
    assert!(page.html.find("表の後。").unwrap() > page.html.find("</table>").unwrap());
}

/// Unpacks `markdown-0.8.1.zip`: two Markdown trees that ling 0.8.1 built,
/// `plain` and `bom`, whose `a.md` starts with a byte-order mark, and the
/// sidecar 0.8.1 rendered for `plain`.
fn corpora_built_by_0_8_1() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let archive = fs::File::open(crate::support::fixture("markdown-0.8.1.zip")).unwrap();
    zip::ZipArchive::new(archive)
        .unwrap()
        .extract(directory.path())
        .unwrap();
    directory
}

fn render_0_8_1(directory: &Path, corpus: &str, output: &Path) -> std::process::Output {
    lingrep()
        .args(["corpus", "render"])
        .arg(directory.join(corpus).join("papers.lingcorpus"))
        .arg("--source-root")
        .arg(directory.join(corpus).join("papers"))
        .arg("--output")
        .arg(output)
        .output()
        .unwrap()
}

#[test]
fn render_writes_the_records_0_8_1_wrote_for_an_artifact_0_8_1_built() {
    let directory = corpora_built_by_0_8_1();
    let manifest = fs::read_to_string(
        directory
            .path()
            .join("plain/papers.lingcorpus/manifest.json"),
    )
    .unwrap();
    // The Markdown reader identity of ling 0.8.1, reader revision 12.
    assert!(manifest.contains(
        "\"reader_identity\": \"46a19a1b725796633f7ead3edbd095e717caaed27f09d9fa2335b1a408aad381\""
    ));
    let output = directory.path().join("rendered");
    assert_success(&render_0_8_1(directory.path(), "plain", &output));
    // The records are the ones 0.8.1 wrote; only the renderer revision in the
    // key has moved since.
    let expected: Vec<(String, Vec<u8>)> = files(&directory.path().join("plain/papers.reading"))
        .into_iter()
        .map(|(name, bytes)| {
            if name != "header.json" {
                return (name, bytes);
            }
            let header = String::from_utf8(bytes).unwrap().replace(
                "lingrep-core/markdown-render/4",
                lingrep_core::corpus::markdown_render::MARKDOWN_RENDERER_REVISION,
            );
            (name, header.into_bytes())
        })
        .collect();
    assert_eq!(files(&output), expected);
}

#[test]
fn render_rejects_0_8_1_artifacts_when_a_source_starts_with_a_byte_order_mark() {
    let directory = corpora_built_by_0_8_1();
    let output = directory.path().join("rendered");
    let result = render_0_8_1(directory.path(), "bom", &output);
    assert!(!result.status.success());
    assert!(!output.exists());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains(
            "re-reading a.md does not reproduce the stored sentences: the reader admits 1 \
             sentences, and the artifact stores 2"
        ),
        "{stderr}"
    );
}

/// The text of `range` in the page's `data-c` element, which holds one plain
/// paragraph.
fn highlighted(html: &str, range: &lingrep_corpus::HighlightRange) -> String {
    let marker = format!("data-c=\"{}\"", range.container);
    let (_, content) = html.split_once(&marker).unwrap();
    let (_, content) = content.split_once('>').unwrap();
    let (content, _) = content.split_once('<').unwrap();
    let units: Vec<u16> = content.encode_utf16().collect();
    String::from_utf16(&units[range.utf16_start as usize..range.utf16_end as usize]).unwrap()
}

/// Each paragraph's first word is a variant spelling: an ideographic
/// variation sequence, half-width katakana, decomposed が, 臭氣 with U+FA5C,
/// and U+FF5E between kana.
const VARIANT_PARAGRAPHS: [&str; 5] = [
    "葛\u{E0100}城市に住む。",
    "ｶﾞﾗｽの靴。",
    "か\u{3099}まんする。",
    "\u{FA5C}氣がする。",
    "長～い道。",
];

/// The reader highlights exactly the source text of a word and of a whole
/// sentence, whether the corpus stores the source (the default) or its NFC
/// form, whose first word of 臭氣 is U+81ED alone.
#[test]
fn a_variant_spelling_highlights_its_exact_source_text() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("variants");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), VARIANT_PARAGRAPHS.join("\n\n") + "\n").unwrap();
    let source_words = [
        "葛\u{E0100}城",
        "ｶﾞﾗｽ",
        "か\u{3099}まん",
        "\u{FA5C}氣",
        "長～い",
    ];
    let mut nfc_words = source_words;
    nfc_words[3] = "\u{FA5C}";

    for (normalization, words) in [
        (&[][..], source_words),
        (&["--normalization", "nfc"][..], nfc_words),
    ] {
        let name = normalization.concat();
        let artifact = directory.path().join(format!("{name}.lingcorpus"));
        assert_success(
            &lingrep()
                .args(["corpus", "build"])
                .args(normalization)
                .arg("--output")
                .arg(&artifact)
                .arg(&root)
                .output()
                .unwrap(),
        );
        let sidecar_path = directory.path().join(format!("{name}.reading"));
        assert_success(
            &lingrep()
                .args(["corpus", "render"])
                .arg(&artifact)
                .arg("--source-root")
                .arg(&root)
                .arg("--output")
                .arg(&sidecar_path)
                .output()
                .unwrap(),
        );
        let artifact = CorpusArtifact::open(&artifact).unwrap();
        let sidecar =
            ReadingSidecar::open(&sidecar_path, &ReadingSidecarKey::for_artifact(&artifact))
                .unwrap();
        let analyses = tokio::runtime::Runtime::new().unwrap().block_on(async {
            let view = artifact.provenance_view().await.unwrap();
            let mut analyses = Vec::new();
            for sentence in 0..artifact.sentence_count() {
                analyses.push(
                    view.sentence_analysis(SentenceId::new(sentence))
                        .await
                        .unwrap(),
                );
            }
            analyses
        });
        assert_eq!(analyses.len(), VARIANT_PARAGRAPHS.len());
        for (sentence, (analysis, (paragraph, word))) in analyses
            .iter()
            .zip(VARIANT_PARAGRAPHS.iter().zip(words))
            .enumerate()
        {
            let chars = |bytes: std::ops::Range<u32>| {
                let count =
                    |end: u32| analysis.text.get(..end as usize).unwrap().chars().count() as u32;
                count(bytes.start)..count(bytes.end)
            };
            let first = chars(analysis.surfaces[0].source_range.clone());
            let whole = 0..analysis.text.chars().count() as u32;
            for (range, expected) in [(first, word), (whole, *paragraph)] {
                let (page, anchor) = sidecar
                    .page_at(SentenceId::new(sentence as u64), range.clone(), 1 << 20)
                    .unwrap();
                assert!(anchor.exact, "{name} {expected}");
                let shown: String = anchor
                    .ranges
                    .iter()
                    .map(|range| highlighted(&page.html, range))
                    .collect();
                assert_eq!(shown, expected, "{name} sentence {sentence} {range:?}");
            }
        }
    }
}
