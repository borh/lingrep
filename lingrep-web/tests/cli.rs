//! Snapshot tests for the lingrep-web CLI surface (help text etc.).

#[test]
fn cli_snapshots() {
    trycmd::TestCases::new().case("tests/cli/*.trycmd");
}

#[test]
fn server_entry_point_distinguishes_runtime_failures_from_usage_errors() {
    let directory = tempfile::tempdir().unwrap();
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    for (arguments, expected) in [
        (vec!["--input", "missing-source.txt"], 1),
        (vec!["--corpus", "invalid-declaration"], 2),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lingrep-web"))
            .current_dir(directory.path())
            .arg("--dict-path")
            .arg(dictionary.path())
            .args(arguments)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(expected), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
}
