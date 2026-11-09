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
            "| 項目 | 値 |",
            "|---|---|",
            "| 甲 | 一 | 余り |",
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
    assert!(
        String::from_utf8_lossy(&options.stderr).contains("reader identity"),
        "{}",
        String::from_utf8_lossy(&options.stderr)
    );

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
