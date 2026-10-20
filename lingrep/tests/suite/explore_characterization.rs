use lingrep_core::testing::MiniDictionary;

use crate::support::{fixture, lingrep, lingrep_shell_command};

#[test]
fn explore_validates_the_catalog_before_terminal_takeover() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "explore",
            "--pattern",
            "broken := [",
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            fixture("sentence-final.jsonl").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid lingrep patterns syntax"),
        "{stderr}"
    );
    assert!(!stderr.contains("\u{1b}[?1049h"));
}

#[test]
fn explore_requires_an_interactive_terminal_without_publishing_protocol_output() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "explore",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            fixture("sentence-final.jsonl").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("interactive terminal"), "{stderr}");
    assert!(!stderr.contains("\u{1b}[?1049h"));
}

#[test]
fn explore_rejects_single_use_stdin_instead_of_hiding_a_replay_cache() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "explore",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "-",
        ])
        .write_stdin("研究。\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stdin cannot support detail rescans"),
        "{stderr}"
    );
    assert!(!stderr.contains("\u{1b}[?1049h"));
}

#[test]
fn explore_rejects_json_before_terminal_setup() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "--json",
            "explore",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            fixture("sentence-final.jsonl").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let diagnostic: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured diagnostic");
    assert_eq!(diagnostic["schema"], "lingrep.diagnostic/v1");
    assert_eq!(diagnostic["data"]["code"], "unsupported_output");
    assert_eq!(diagnostic["data"]["retry"], "after_change");
}

#[test]
fn explore_preflights_missing_direct_root_before_terminal_takeover() {
    let dictionary = MiniDictionary::new();
    let missing = fixture("definitely-missing-explore-root.jsonl");
    let output = lingrep()
        .args([
            "explore",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            missing.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("interactive terminal"), "{stderr}");
    assert!(!stderr.contains("\u{1b}[?1049h"), "{stderr}");
    assert!(
        stderr.contains("does not exist")
            || stderr.contains("not found")
            || stderr.contains("No such file"),
        "{stderr}"
    );
}

#[test]
fn explore_preflights_corrupt_prepared_artifact_before_terminal_takeover() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("corrupt.lingcorpus");
    std::fs::write(&artifact, b"not a prepared corpus").unwrap();
    let output = lingrep()
        .args([
            "explore",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--corpus",
            artifact.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("interactive terminal"), "{stderr}");
    assert!(!stderr.contains("\u{1b}[?1049h"), "{stderr}");
    assert!(
        stderr.contains("prepared") || stderr.contains("corpus"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn explore_successful_pty_start_and_q_restore_terminal_state() {
    use std::io::Write;
    use std::process::Stdio;

    let patterns = fixture("sentence-final.lx");
    let dictionary = MiniDictionary::new();
    let corpus = fixture("sentence-final.jsonl");
    let invocation = lingrep_shell_command([
        std::ffi::OsStr::new("explore"),
        std::ffi::OsStr::new("--patterns"),
        patterns.as_os_str(),
        std::ffi::OsStr::new("--dict-path"),
        dictionary.path().as_os_str(),
        corpus.as_os_str(),
    ]);
    let mut script = crate::support::pty_script_command(&invocation);
    let mut child = script
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"q").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output
            .stdout
            .windows(8)
            .any(|bytes| bytes == b"\x1b[?1049h")
    );
    assert!(
        output
            .stdout
            .windows(8)
            .any(|bytes| bytes == b"\x1b[?1049l")
    );
    assert!(
        output
            .stdout
            .windows(8)
            .any(|bytes| bytes == b"\x1b[?1000l")
            || output
                .stdout
                .windows(8)
                .any(|bytes| bytes == b"\x1b[?1003l")
    );
}
