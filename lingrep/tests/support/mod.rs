// Exempt test/bench/example code from the workspace panic deny-floor (see
// docs/lint-policy.md).
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
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use lingrep_core::corpus::{
    AdmittedCorpusEvent, AdmittedSentence, DiscoveryReport, DocumentId, InventoryStats, Metadata,
    SentenceId, SourceDocument, TraversalHighWater, TraversalLimits,
};
use lingrep_core::execution::ExecutionError;
use lingrep_core::spill::TempBudget;
use lingrep_core::token_match::TokenInfo;
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusBuilder, DictionaryFingerprint,
    ReaderIdentity,
};

/// Construct the package binary through Cargo's integration-test contract.
#[allow(dead_code)]
pub fn lingrep() -> assert_cmd::Command {
    assert_cmd::Command::from_std(lingrep_std())
}

/// Construct the package binary for a command proven not to load a dictionary.
///
/// Prepared-corpus queries and commands such as `check` do not need the
/// process-wide residency lock used by [`lingrep`]. Direct-text commands
/// belong on the locked constructor.
#[allow(dead_code)]
pub fn lingrep_without_dictionary() -> assert_cmd::Command {
    assert_cmd::cargo::cargo_bin_cmd!("ling")
}

/// Resolve the unwrapped package binary for shell-completion runtimes.
#[allow(dead_code)]
pub fn lingrep_binary_path() -> PathBuf {
    let binary = assert_cmd::cargo::cargo_bin_cmd!("ling");
    PathBuf::from(binary.get_program())
}

/// Start a shell without Cargo's hyphenated binary-path variable.
///
/// Fish rejects that otherwise valid process environment name before it can
/// load the isolated completion-test configuration.
#[allow(dead_code)]
pub fn shell_without_lingrep_binary_env(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env_remove("CARGO_BIN_EXE_ling");
    command
}

/// Construct the package binary for tests that need a live child process.
#[allow(dead_code)]
pub fn lingrep_std() -> Command {
    let binary = assert_cmd::cargo::cargo_bin_cmd!("ling");
    let binary_path = PathBuf::from(binary.get_program());
    let lock_path = binary_path.with_extension("integration-test.lock");

    // A dictionary-backed ling process can consume several GiB. Libtest
    // otherwise starts enough of them concurrently for earlyoom to terminate a
    // healthy child nondeterministically. `--no-fork` makes flock exec the real
    // binary, retaining its PID so the cancellation and signal tests remain
    // faithful to production behavior.
    let mut command = Command::new("flock");
    command
        .args(["--exclusive", "--no-fork"])
        .arg(lock_path)
        .arg(binary_path);
    command
}

/// Whether the `script(1)` on PATH is util-linux's rather than BSD's.
///
/// Asked of the binary, not the platform. nixpkgs' darwin `util-linux` ships
/// `flock` but not `script`, so a dev shell there finds `/usr/bin/script` — but
/// that is a packaging fact which could change, and a `cfg(target_os)` here
/// would silently pick the wrong syntax if it did.
#[cfg(unix)]
fn script_is_util_linux() -> bool {
    static KIND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *KIND.get_or_init(|| {
        Command::new("script")
            .arg("--version")
            .output()
            .is_ok_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).contains("util-linux")
            })
    })
}

/// A `script(1)` invocation running `invocation` under a pseudo-terminal.
///
/// The two implementations disagree on flags *and* operand order. util-linux
/// passes the command to `-c` and takes the typescript file last; BSD takes the
/// typescript file first and the command as trailing argv, so it needs an
/// explicit `sh -c` to accept one rendered string. Both attach a real pty —
/// checked on each platform rather than assumed.
#[cfg(unix)]
#[allow(dead_code)]
pub fn pty_script_command(invocation: &str) -> Command {
    let mut command = Command::new("script");
    if script_is_util_linux() {
        command.args(["-qec", invocation, "/dev/null"]);
    } else {
        command.args(["-q", "/dev/null", "sh", "-c", invocation]);
    }
    command
}

/// A stdout handle whose every write fails, or `None` where none exists.
///
/// `/dev/full` answers each write with `ENOSPC`, which is how these tests
/// provoke a stdout failure without involving a pipe reader — a closed pipe is
/// a *different* contract here, with its own tests. It is a Linux device:
/// darwin has none, so `cfg(unix)` selected a platform that cannot supply it
/// and the tests died on `ENOENT` at the fixture rather than skipping.
///
/// `/dev/full` has no substitute, so a host without it must skip rather than
/// swap in another descriptor. Every route to a bad fd 1 — a read-only
/// reopen, `1>&-` — fails with `EBADF`, and `Stdout` passes writes through
/// std's `handle_ebadf`, which reports `EBADF` as a *successful full write*
/// (`library/std/src/io/stdio.rs`). The write syscall does fail either way;
/// std chooses not to surface this one, so no caller can observe it and the
/// CLI exits 0. `ENOSPC` is the failure it does surface, and the exit-1 path
/// under test is reachable only through it.
///
/// The notice goes to the `stderr()` handle rather than `eprintln!`, which
/// libtest captures and replays only for a failing test; from a passing one it
/// would be discarded, making an absent assertion look like a satisfied one.
#[cfg(unix)]
#[allow(dead_code)]
pub fn always_failing_stdout() -> Option<std::fs::File> {
    match std::fs::File::options().write(true).open("/dev/full") {
        Ok(full) => Some(full),
        Err(error) => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "note: skipping the stdout-failure assertions — no /dev/full on this system ({error})"
            );
            None
        }
    }
}

/// Render the shared wrapper for `script -c`, preserving every argument.
#[cfg(unix)]
#[allow(dead_code)]
pub fn lingrep_shell_command<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = lingrep_std();
    command.args(args);
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(unix)]
#[allow(dead_code)]
fn shell_quote(word: &std::ffi::OsStr) -> String {
    format!("'{}'", word.to_string_lossy().replace('\'', "'\\''"))
}

/// Decode a complete JSONL byte stream for executable protocol assertions.
#[allow(dead_code)]
pub fn parse_jsonl(bytes: &[u8]) -> Vec<serde_json::Value> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("valid JSONL record"))
        .collect()
}

/// Resolve a path below this package's integration-test fixtures.
#[allow(dead_code)]
pub fn fixture(relative: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(relative)
}

/// Resolve the shared development-shell dictionary fixture.
#[allow(dead_code)]
pub fn dict_path() -> PathBuf {
    std::env::var_os("LINGREP_DICTIONARY_PATH")
        .map(PathBuf::from)
        .expect("LINGREP_DICTIONARY_PATH is unset — run tests inside `nix develop`")
}

/// Build the smallest valid prepared corpus used by command-semantics tests.
///
/// Tests of artifact identity, inventory, encoding, or publication construct
/// their own artifact instead; this helper deliberately fixes those details so
/// callers state only the sentence and token evidence relevant to the command.
#[allow(dead_code)]
pub async fn build_single_sentence_artifact(destination: &Path, text: &str, tokens: &[TokenInfo]) {
    let source = SourceDocument {
        ordinal: 0,
        logical_name: "a.md".into(),
        path: PathBuf::from("/must/not/be/opened/a.md"),
        target_group_ordinal: 0,
        external_target: false,
    };
    let mut builder = CorpusBuilder::begin(
        BuildConfig {
            analysis: AnalysisInputs::new(
                ReaderIdentity::from_bytes([1; 32]),
                AdmissionIdentity::from_bytes([2; 32]),
                &lingrep_corpus::vibrato_fixture_descriptor(),
            ),
            analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
            dictionary: DictionaryFingerprint::from_bytes([3; 32]),
            trust: lingrep_corpus::TrustClass::LocallyExecuted,
            temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
            sentence_metadata_keys: Vec::new(),
        },
        destination,
    )
    .await
    .expect("begin prepared test artifact");
    builder
        .register_source_document(&source)
        .expect("register prepared test source");
    let observation = builder.source_observation();
    observation
        .begin_source("a.md")
        .expect("begin prepared test observation");
    observation
        .update_source_bytes(b"raw")
        .expect("observe prepared test bytes");
    observation
        .end_source()
        .expect("end prepared test observation");
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
            id: DocumentId::new(0),
            source_name: "a.md".into(),
            metadata: empty_metadata(),
        })
        .await
        .expect("start prepared test document");
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DivisionStart(std::sync::Arc::new(
            lingrep_core::corpus::Division {
                source_id: None,
                id: lingrep_core::corpus::DivisionId(0),
                document_id: DocumentId::new(0),
                kind: "paragraph".into(),
                parent_id: None,
                metadata: empty_metadata(),
            },
        )))
        .await
        .expect("start prepared test paragraph");
    let location = builder
        .push_corpus_event(&AdmittedCorpusEvent::Sentence(AdmittedSentence {
            structure: Default::default(),
            id: SentenceId::new(0),
            document_id: DocumentId::new(0),
            document_sentence_ordinal: 0,
            text: text.into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        }))
        .await
        .expect("append prepared test sentence")
        .expect("sentence has a location");
    builder
        .push_tokenized_sentence(&location, tokens)
        .await
        .expect("append prepared test tokens");
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DivisionEnd {
            id: lingrep_core::corpus::DivisionId(0),
        })
        .await
        .expect("end prepared test paragraph");
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DocumentEnd {
            id: DocumentId::new(0),
        })
        .await
        .expect("end prepared test document");
    builder
        .finish(DiscoveryReport {
            inventory: InventoryStats {
                documents: 1,
                distinct_targets: 1,
                alias_documents: 0,
                max_target_multiplicity: 1,
                external_targets: 0,
            },
            configured_limits: TraversalLimits::default(),
            observed: TraversalHighWater {
                max_depth: 0,
                examined_entries: 1,
                max_directory_entries: 1,
                index_bytes: 128,
            },
            ordered_inventory_digest: inventory_digest(&["a.md"]),
        })
        .await
        .expect("finish prepared test artifact");
}

fn empty_metadata() -> Metadata {
    Box::new([])
}

fn inventory_digest(names: &[&str]) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    for name in names {
        digest.update(&(name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
    }
    *digest.finalize().as_bytes()
}

/// The synthetic `Joined_info` table for [`bccwj_distribution`] (20 columns
/// per the reference; spec §3.2). All values are self-authored (§2): no
/// BCCWJ content is recorded in any tracked artifact.
pub const BCCWJ_JOINED: &str = "a	b	c	d	e	f	g	h	i	j	k	l	m	n	o	p	q	r	s
one	book	題一		vol	resp	社一	2000	isbn	page							著一			LB
solo	book	題二		vol2	resp2	社二	2001	isbn2	page2							著二			PN
";

/// The entry the `one` zip carries (FIXED/LB): one `speech` paragraph and
/// one plain paragraph. Self-authored text.
pub const BCCWJ_ENTRY_ONE: &str = "<article>\n  <speech><paragraph><sentence><orth>猫</orth><orth>。</orth></sentence></paragraph></speech>\n  <paragraph><sentence><orth>犬</orth><orth>。</orth></sentence></paragraph>\n</article>\n";

/// The entry the `solo` zip carries (VARIABLE/PN). Self-authored text.
pub const BCCWJ_ENTRY_SOLO: &str = "<article><paragraph><sentence><orth>猫</orth><orth>。</orth></sentence></paragraph></article>\n";

/// Writes a synthetic two-zip BCCWJ distribution under `root` (creating it):
/// `C-XML/FIXED/LB/LB.zip` (entry `LBa/one.xml`, the `one` metadata row) and
/// `C-XML/VARIABLE/PN/PN.zip` (entry `PNb/solo.xml`, the `solo` row), plus
/// the three `DOC/` context tables. Returns the root path.
///
/// Sentence census: three sentences total — two in `one` (the `speech`
/// sentence and the body sentence, subcorpus `書籍`, sampling `fixed`) and
/// one in `solo` (subcorpus `新聞`, sampling `variable`).
pub fn bccwj_distribution(root: &Path) -> PathBuf {
    use std::io::Write;

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(
            std::fs::File::create(path).unwrap(),
        ));
        for (entry, content) in entries {
            writer
                .start_file(entry.to_owned(), zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    let doc = root.join("DOC");
    std::fs::create_dir_all(&doc).unwrap();
    std::fs::write(doc.join("Joined_info.txt"), BCCWJ_JOINED).unwrap();
    std::fs::write(doc.join("BCCWJ-NDC.txt"), "a\tb\n").unwrap();
    std::fs::write(doc.join("CopyRight_Annotation.txt"), "a\tb\n").unwrap();
    let one = format!("<BCCWJ version=\"2.00\"><BCCWJ_Text>{BCCWJ_ENTRY_ONE}</BCCWJ_Text></BCCWJ>");
    let solo =
        format!("<BCCWJ version=\"2.00\"><BCCWJ_Text>{BCCWJ_ENTRY_SOLO}</BCCWJ_Text></BCCWJ>");
    write_zip(
        &root.join("C-XML/FIXED/LB/LB.zip"),
        &[("LBa/one.xml", &one)],
    );
    write_zip(
        &root.join("C-XML/VARIABLE/PN/PN.zip"),
        &[("PNb/solo.xml", &solo)],
    );
    root.to_path_buf()
}

/// The `catalog.csv` of the synthetic Soranoha archive below: the pinned
/// header and two self-authored works. Every field is quoted, as the exporter
/// writes them; `body_character_count` counts the non-whitespace characters of
/// each work's text.
pub const SORANOHA_CATALOG: &str = "\u{feff}\"identifier\",\"title\",\"subtitle\",\"title_reading\",\"author\",\"orthographic_style\",\"ndc\",\"first_published\",\"body_character_count\",\"source_edition_title\",\"source_edition_publisher\",\"source_edition_year\",\"filename\",\"source_content_hash\",\"url\",\"release\",\"doi\"\r\n\"000092_000879\",\"蜘蛛の糸\",\"\",\"くものいと\",\"芥川 竜之介\",\"新字新仮名\",\"NDC 913\",\"「赤い鳥」1918（大正7）年7月\",\"7\",\"或阿呆の一生・侏儒の言葉\",\"角川文庫、角川書店\",\"1969\",\"kumo.txt\",\"sha256:0000000000000000000000000000000000000000000000000000000000000000\",\"https://soranoha.org/works/000092_000879/\",\"c373bb085971c6a88d8aa1c164788c53ebf304ab501a6f5188c3077ac5482916\",\"10.5281/zenodo.23469644\"\r\n\"000013_000879\",\"十本の針\",\"\",\"じっぽんのはり\",\"別 作者\",\"新字新仮名\",\"NDC 914\",\"「文藝春秋」1927（昭和2）年9月\",\"5\",\"或阿呆の一生・侏儒の言葉\",\"角川文庫、角川書店\",\"1969\",\"hari.txt\",\"sha256:1111111111111111111111111111111111111111111111111111111111111111\",\"https://soranoha.org/works/000013_000879/\",\"c373bb085971c6a88d8aa1c164788c53ebf304ab501a6f5188c3077ac5482916\",\"10.5281/zenodo.23469644\"\r\n";

/// Writes a synthetic Soranoha bulk plaintext archive at `path` (creating its
/// parent): `catalog.csv` plus two works, `kumo.txt` (one sentence) and
/// `hari.txt` (one sentence). Both texts use words the miniature dictionary
/// knows, so a test can match them without a real dictionary. Returns the archive path.
///
/// The texts are self-authored, the release is one value across the archive,
/// and each `body_character_count` equals the work's non-whitespace character
/// count, so a test that checks the length formula is checking the fixture and
/// the reader together.
pub fn soranoha_archive(path: &Path) -> PathBuf {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(
        std::fs::File::create(path).unwrap(),
    ));
    for (entry, content) in [
        ("catalog.csv", SORANOHA_CATALOG),
        ("kumo.txt", "猫が文を鳴く。\n"),
        ("hari.txt", "犬が走る。\n"),
    ] {
        writer
            .start_file(entry.to_owned(), zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
    path.to_path_buf()
}

/// Runs `corpus build --input-format bccwj` over `root` and returns the
/// published canonical artifact path beside it.
pub fn bccwj_build_artifact(
    root: &Path,
    dictionary: &lingrep_core::testing::MiniDictionary,
) -> PathBuf {
    let build = lingrep()
        .args(["corpus", "build", "--input-format", "bccwj", "--dict-path"])
        .arg(dictionary.path())
        .arg(root)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let prefix = format!(
        "{root}.",
        root = root.file_name().unwrap().to_string_lossy()
    );
    std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".lingcorpus"))
        })
        .expect("the canonical artifact beside the root")
}

/// The `type` field of each JSONL event, in stream order.
pub fn event_types(events: &[serde_json::Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect()
}

/// Assert a child exited successfully, printing both streams when it did not.
pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The `(source, sentence ordinal, token index)` a missing-attribute refusal
/// names.
pub fn missing_attribute_location(error: &ExecutionError) -> (&str, u64, usize) {
    match error {
        ExecutionError::MissingRequiredAttribute {
            source_name,
            corpus_sentence_ordinal,
            token_index,
            ..
        } => (source_name, *corpus_sentence_ordinal, *token_index),
        error => panic!("expected missing attribute error, got {error:?}"),
    }
}
