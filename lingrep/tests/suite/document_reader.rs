#![allow(clippy::panic, clippy::unwrap_used, clippy::string_slice)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::support::{Server, assert_success, get, lingrep, serve};
use lingrep_core::corpus::DocumentId;
use lingrep_core::corpus::markdown_render::render_markdown;
use lingrep_core::testing::MiniDictionary;
use lingrep_corpus::{CorpusArtifact, ReadingSidecar, ReadingSidecarKey};

const PAPER: &str = "---\ntitle: 一\n---\n# 序論 {.introduction}\n\n最初の文です。\n本文の**太字**です。\n\n| 項目 | 値 |\n|---|---|\n| 甲 | 一 |\n表の後の文です。\n\n::: {.note}\n注の文です。[^1]\n:::\n\n## 方法\n\n- 一つ目の項目です。\n- 二つ目の項目です。\n\n> 引用の文です。\n\n### 細目\n\n文中の<b>生の</b>記号です。\n\n[^1]: 脚注の文です。\n";

/// A document whose rendering spans several reader pages in each direction
/// from its middle.
fn long_paper() -> String {
    let mut text = String::from("# 長い文書\n\n");
    for index in 0..3000 {
        if index % 40 == 0 {
            text.push_str(&format!("## 第{index}節\n\n"));
        }
        text.push_str(&format!(
            "これは{index}番目の段落にある、読み手が順に読むための長めの文です。\n\n"
        ));
    }
    text
}

struct Fixture {
    directory: tempfile::TempDir,
    dictionary: MiniDictionary,
    root: PathBuf,
    artifact: PathBuf,
    sidecar: PathBuf,
}

fn build(directory: &Path, dictionary: &MiniDictionary, input: &Path, name: &str) -> PathBuf {
    let artifact = directory.join(format!("{name}.lingcorpus"));
    assert_success(
        &lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("--output")
            .arg(&artifact)
            .arg(input)
            .output()
            .unwrap(),
    );
    artifact
}

fn render(artifact: &Path, root: &Path, sidecar: &Path) {
    assert_success(
        &lingrep()
            .args(["corpus", "render"])
            .arg(artifact)
            .arg("--source-root")
            .arg(root)
            .arg("--output")
            .arg(sidecar)
            .output()
            .unwrap(),
    );
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("papers");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), PAPER).unwrap();
    fs::write(root.join("b.md"), long_paper()).unwrap();
    fs::write(root.join("c.md"), "# 見出しだけ\n").unwrap();
    let dictionary = MiniDictionary::new();
    let artifact = build(directory.path(), &dictionary, &root, "papers");
    let sidecar = directory.path().join("papers.reading");
    render(&artifact, &root, &sidecar);
    Fixture {
        directory,
        dictionary,
        root,
        artifact,
        sidecar,
    }
}

fn served(fixture: &Fixture, extra: &[&dyn AsRef<std::ffi::OsStr>]) -> Server {
    ready(
        serve_with(fixture, extra)
            .unwrap_or_else(|(code, stderr)| panic!("exit {code:?}: {stderr}")),
    )
}

/// `server` once its default corpus answers reader requests.
fn ready(server: Server) -> Server {
    for _ in 0..500 {
        if get(&server, "/api/read/default/0/0/1/document").0 != 503 {
            return server;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("the corpus did not become ready");
}

fn serve_with(
    fixture: &Fixture,
    extra: &[&dyn AsRef<std::ffi::OsStr>],
) -> Result<Server, (Option<i32>, String)> {
    let mut args: Vec<OsString> = vec!["--input".into(), fixture.artifact.clone().into()];
    args.extend(extra.iter().map(|part| part.as_ref().to_owned()));
    serve(fixture.directory.path(), fixture.dictionary.path(), &args)
}

/// Every `<p>` and `<h1>`–`<h6>` element of `html`, opening tag and content,
/// in document order.
fn headings_and_paragraphs(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = html[at..].find('<') {
        let start = at + open;
        let name: String = html[start + 1..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        at = start + 1;
        if matches!(name.as_str(), "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            let close = format!("</{name}>");
            let end = start + html[start..].find(&close).unwrap() + close.len();
            found.push(html[start..end].to_owned());
            at = end;
        }
    }
    found
}

fn control(fragment: &str, marker: &str) -> Option<String> {
    let at = fragment.find(marker)?;
    let rest = &fragment[at..];
    let url = &rest[rest.find("hx-get=\"")? + 8..];
    Some(url[..url.find('"')?].to_owned())
}

/// The reader's headings and paragraphs for the document holding
/// `sentence`, from the page it opens on and every page before and after.
fn reader_structure(server: &Server, sentence: u64) -> (Vec<String>, usize) {
    let (status, opened) = get(
        server,
        &format!("/api/read/default/{sentence}/0/1/document"),
    );
    assert_eq!(status, 200, "{opened}");
    assert!(opened.contains("data-doc-reader"), "{opened}");
    let text = &opened[opened.find("class=\"doc-reader-text\"").unwrap()..];
    let mut earlier = Vec::new();
    let mut cursor = control(text, "data-reader-earlier");
    while let Some(url) = cursor {
        let (status, page) = get(server, &format!("/{url}"));
        assert_eq!(status, 200, "{page}");
        assert!(control(&page, "data-reader-later").is_none());
        cursor = control(&page, "data-reader-earlier");
        earlier.push(headings_and_paragraphs(&page));
    }
    let mut pages = 1 + earlier.len();
    let mut structure: Vec<String> = earlier.into_iter().rev().flatten().collect();
    structure.extend(headings_and_paragraphs(text));
    let mut cursor = control(text, "data-reader-later");
    while let Some(url) = cursor {
        let (status, page) = get(server, &format!("/{url}"));
        assert_eq!(status, 200, "{page}");
        assert!(control(&page, "data-reader-earlier").is_none());
        cursor = control(&page, "data-reader-later");
        structure.extend(headings_and_paragraphs(&page));
        pages += 1;
    }
    (structure, pages)
}

/// For each document with a sentence, whether the reader opened at its
/// middle sentence shows the headings and paragraphs that rendering its
/// source shows: documents compared, documents equal, and the most pages
/// one document took.
fn compare_documents(
    server: &Server,
    sidecar: &Path,
    artifact: &Path,
    root: &Path,
) -> (u64, u64, usize) {
    let artifact = CorpusArtifact::open(artifact).unwrap();
    let sidecar =
        ReadingSidecar::open(sidecar, &ReadingSidecarKey::for_artifact(&artifact)).unwrap();
    let (mut compared, mut equal, mut most) = (0, 0, 0);
    for document in 0..sidecar.document_count() {
        let document = sidecar.document(DocumentId::new(document)).unwrap();
        if document.sentences.is_empty() {
            continue;
        }
        let middle =
            document.sentences.start + (document.sentences.end - document.sentences.start) / 2;
        let source = fs::read_to_string(root.join(&document.source_name)).unwrap();
        let rendered: String = render_markdown(&source)
            .blocks
            .into_iter()
            .map(|block| block.html)
            .collect();
        let expected = headings_and_paragraphs(&rendered);
        let (shown, pages) = reader_structure(server, middle);
        compared += 1;
        equal += u64::from(!expected.is_empty() && shown == expected);
        most = most.max(pages);
    }
    (compared, equal, most)
}

#[test]
fn the_reader_shows_the_headings_and_paragraphs_of_the_source_rendering() {
    let fixture = fixture();
    let server = served(&fixture, &[&"--reading-sidecar", &fixture.sidecar]);
    let (compared, equal, most) =
        compare_documents(&server, &fixture.sidecar, &fixture.artifact, &fixture.root);
    assert_eq!((compared, equal), (2, 2));
    assert!(most >= 5, "the long document took {most} pages");
}

/// Over the local corpus that `docs/testing-guide.md`'s
/// `LINGREP_READER_*` variables name; prints counts only.
#[test]
#[ignore = "needs a local Markdown corpus and its sidecar"]
fn the_reader_matches_the_source_rendering_on_a_local_corpus() {
    let path = |name: &str| {
        PathBuf::from(
            std::env::var_os(name)
                .unwrap_or_else(|| panic!("{name} is not set; see docs/testing-guide.md")),
        )
    };
    let artifact = path("LINGREP_READER_ARTIFACT");
    let sidecar = path("LINGREP_READER_SIDECAR");
    let root = path("LINGREP_READER_SOURCE_ROOT");
    let directory = tempfile::tempdir().unwrap();
    let args: Vec<OsString> = vec![
        "--input".into(),
        artifact.clone().into(),
        "--reading-sidecar".into(),
        sidecar.clone().into(),
    ];
    let server = ready(
        serve(directory.path(), &crate::support::dict_path(), &args)
            .unwrap_or_else(|(code, stderr)| panic!("exit {code:?}: {stderr}")),
    );
    let (compared, equal, most) = compare_documents(&server, &sidecar, &artifact, &root);
    println!(
        "documents compared: {compared}, headings and paragraphs equal: {equal}, most pages: {most}"
    );
    assert_eq!(compared, equal);
}

fn copy_sidecar(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[test]
fn a_sidecar_that_matches_no_served_corpus_stops_startup() {
    let fixture = fixture();
    let other_root = fixture.directory.path().join("other");
    fs::create_dir(&other_root).unwrap();
    fs::write(other_root.join("x.md"), "# 別\n\n別の文です。\n").unwrap();
    let other = build(
        fixture.directory.path(),
        &fixture.dictionary,
        &other_root,
        "other",
    );
    let other_sidecar = fixture.directory.path().join("other.reading");
    render(&other, &other_root, &other_sidecar);
    let revised = fixture.directory.path().join("revised.reading");
    copy_sidecar(&fixture.sidecar, &revised);
    let header = fs::read_to_string(revised.join("header.json")).unwrap();
    fs::write(
        revised.join("header.json"),
        header.replace(
            "lingrep-core/markdown-render/1",
            "lingrep-core/markdown-render/0",
        ),
    )
    .unwrap();

    let corrupted = fixture.directory.path().join("corrupted.reading");
    copy_sidecar(&fixture.sidecar, &corrupted);
    let mut records = fs::read(corrupted.join("records.zst")).unwrap();
    let last = records.len() - 1;
    records[last] ^= 0xff;
    fs::write(corrupted.join("records.zst"), records).unwrap();
    let rendered = ReadingSidecar::read_key(&other_sidecar)
        .unwrap()
        .analysis_identity;
    let served = ReadingSidecarKey::for_artifact(&CorpusArtifact::open(&fixture.artifact).unwrap())
        .analysis_identity;
    let unmatched = [
        "matches no served corpus".to_owned(),
        format!("renders analysis identity {rendered}"),
        format!("corpus \"default\" has analysis identity {served}"),
        "ling corpus render".to_owned(),
    ];
    for (sidecars, expected) in [
        (vec![&other_sidecar], unmatched.to_vec()),
        (vec![&revised], vec!["renderer revision".to_owned()]),
        (vec![&corrupted], vec!["records digest".to_owned()]),
        (
            vec![&fixture.sidecar, &fixture.sidecar],
            vec!["both render corpus \"default\"".to_owned()],
        ),
    ] {
        let mut extra: Vec<&dyn AsRef<std::ffi::OsStr>> = Vec::new();
        for sidecar in &sidecars {
            extra.push(&"--reading-sidecar");
            extra.push(*sidecar);
        }
        let (code, stderr) = serve_with(&fixture, &extra)
            .err()
            .unwrap_or_else(|| panic!("{sidecars:?} started"));
        assert_eq!(code, Some(2), "{stderr}");
        assert!(stderr.contains("--reading-sidecar"), "{stderr}");
        for expected in expected {
            assert!(stderr.contains(&expected), "{expected}: {stderr}");
        }
    }

    // A corpus lingrep-web prepares from a source tree has no artifact a
    // sidecar can bind to.
    let (code, stderr) = serve(
        fixture.directory.path(),
        fixture.dictionary.path(),
        &[
            "--input".into(),
            fixture.root.clone().into(),
            "--reading-sidecar".into(),
            fixture.sidecar.clone().into(),
        ],
    )
    .err()
    .unwrap_or_else(|| panic!("a source tree with a sidecar started"));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("no served corpus is a prepared .lingcorpus artifact"),
        "{stderr}"
    );
}

#[test]
fn without_a_sidecar_the_reader_shows_the_stored_sentences() {
    let fixture = fixture();
    let server = served(&fixture, &[]);
    let (status, page) = get(&server, "/api/read/default/1/0/1/document");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("data-reading-page"), "{page}");
    assert!(!page.contains("data-doc-reader"), "{page}");
    assert_eq!(page.matches("<mark>").count(), 1, "{page}");

    let input = fixture.directory.path().join("plain.txt");
    fs::write(
        &input,
        "一つ目の文です。\n二つ目の文です。\n三つ目の文です。\n",
    )
    .unwrap();
    let plain = build(
        fixture.directory.path(),
        &fixture.dictionary,
        &input,
        "plain",
    );
    let server = serve(
        fixture.directory.path(),
        fixture.dictionary.path(),
        &["--input".into(), plain.into()],
    )
    .map(ready)
    .unwrap();
    let (status, page) = get(&server, "/api/read/default/1/0/1/document");
    assert_eq!(status, 200, "{page}");
    assert_eq!(page.matches("class=\"reading-text\"").count(), 1, "{page}");
    let run = &page[page.find("class=\"reading-text\"").unwrap()..];
    let sentences: Vec<&str> = run
        .split("data-reading-sentence=\"")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect();
    assert_eq!(sentences, ["1", "2"], "{page}");
    assert!(!run.contains("data-reading-section"), "{page}");
    assert_eq!(run.matches("<mark>").count(), 1, "{page}");
}

#[test]
fn reader_requests_outside_the_corpus_are_rejected_and_leave_it_served() {
    let fixture = fixture();
    let server = served(&fixture, &[&"--reading-sidecar", &fixture.sidecar]);
    for path in [
        "/api/read/default/1000000/0/1/document",
        "/api/read/default/0/0/1000/document",
        "/api/read/default/0/1/1/document",
        "/api/read/default/0/0/1/section",
        "/api/read/default/0/0/1/document?start=0",
        "/api/read/default/0/0/1/document?blocks_before=0",
        "/api/read/default/0/0/1/document?blocks_from=1000",
        "/api/read/default/0/0/1/document?blocks_from=0&blocks_before=1",
        "/api/read/default/0/0/1/paragraph?blocks_from=0",
    ] {
        let (status, body) = get(&server, path);
        assert_eq!(status, 400, "{path}: {body}");
    }
    assert_eq!(get(&server, "/api/read/other/0/0/1/document").0, 404);
    let (status, page) = get(&server, "/api/read/default/0/0/1/document");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("data-doc-reader"), "{page}");
}
