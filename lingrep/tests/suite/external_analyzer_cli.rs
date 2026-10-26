#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use crate::support::lingrep_std;
use std::process::Command;

#[test]
fn analyzer_file_runs_tokens_matches_and_prepared_builds() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    std::fs::copy(
        repository.join("lingrep-core/tests/fixtures/external_echo_adapter.py"),
        root.join("adapter.py"),
    )
    .unwrap();
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let python = String::from_utf8(python.stdout).unwrap();
    std::fs::write(root.join("analyzer.toml"), format!("version = 1\nkind = 'protocol'\npython = '{}'\nentrypoint = 'adapter.py'\nlanguage = 'en'\nschema = [{{ key = 'text', kind = 'scalar' }}]\n", python.trim())).unwrap();
    std::fs::write(root.join("input.txt"), "hello\nworld\n").unwrap();
    std::fs::write(root.join("query.lx"), "word := [text=hello].\n").unwrap();
    let mut direct_matches = Vec::new();
    for command in ["tokenize", "matches", "matches", "summarize", "coverage"] {
        let mut run = lingrep_std();
        run.current_dir(root).args([
            command,
            "--analyzer-config",
            "analyzer.toml",
            "--json",
            "--normalization",
            "none",
            "--batch-sentences",
            "1",
            "--batch-raw-bytes",
            "1",
        ]);
        if matches!(command, "matches" | "summarize") {
            run.args(["--patterns", "query.lx"]);
        }
        let output = run.arg("input.txt").output().unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let records = String::from_utf8(output.stdout).unwrap();
        assert!(records.contains(if command == "coverage" {
            "text"
        } else {
            "hello"
        }));
        if command != "coverage" {
            assert!(records.contains("complete"));
        }
        if command == "matches" {
            direct_matches.push(
                records
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .filter(|record| record["type"] == "match")
                    .collect::<Vec<_>>(),
            );
        }
    }
    assert_eq!(direct_matches[0], direct_matches[1]);
    let output = lingrep_std()
        .current_dir(root)
        .args([
            "corpus",
            "build",
            "--batch-raw-bytes",
            "1",
            "--analyzer-config",
            "analyzer.toml",
            "--output",
            "artifact",
            "input.txt",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_file(root.join("adapter.py")).unwrap();
    let output = lingrep_std()
        .current_dir(root)
        .args([
            "matches",
            "--corpus",
            "artifact",
            "--patterns",
            "query.lx",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout).unwrap().contains("hello"));
}

#[test]
fn invalid_analyzer_files_and_protocol_failures_publish_no_completion() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    std::fs::copy(
        repository.join("lingrep-core/tests/fixtures/external_echo_adapter.py"),
        root.join("adapter.py"),
    )
    .unwrap();
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let python = String::from_utf8(python.stdout).unwrap();
    std::fs::write(root.join("input.txt"), "hello\n").unwrap();
    for (mode, extra, expected) in [
        ("malformed", "", 1),
        ("nonzero", "", 1),
        ("echo", "unknown_option = true\n", 2),
        ("echo", "", 0),
    ] {
        let config = format!(
            "version = 1\nkind = 'protocol'\npython = '{}'\nentrypoint = 'adapter.py'\nlanguage = 'en'\nschema = [{{ key = 'text', kind = 'scalar' }}]\narguments = ['{mode}']\n{extra}",
            python.trim()
        );
        std::fs::write(root.join("analyzer.toml"), config).unwrap();
        let output = lingrep_std()
            .current_dir(root)
            .args([
                "tokenize",
                "--analyzer-config",
                "analyzer.toml",
                "--json",
                "input.txt",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let records = String::from_utf8(output.stdout).unwrap();
        assert_eq!(records.contains("\"type\":\"complete\""), expected == 0);
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }
}

#[test]
fn signals_cancel_an_external_adapter_without_successful_completion() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    for (signal, expected) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let adapter = std::fs::read_to_string(repository.join("lingrep-core/tests/fixtures/external_echo_adapter.py")).unwrap().replace(
            "        if mode in {\"silent\", \"ignore_term_silent\"}:\n            time.sleep(60)",
            "        if mode in {\"silent\", \"ignore_term_silent\"}:\n            sys.stderr.write('ADAPTER_WAITING\\n')\n            sys.stderr.flush()\n            time.sleep(60)",
        );
        std::fs::write(root.join("adapter.py"), adapter).unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        let python = String::from_utf8(python.stdout).unwrap();
        std::fs::write(root.join("analyzer.toml"), format!("version = 1\nkind = 'protocol'\npython = '{}'\nentrypoint = 'adapter.py'\nlanguage = 'en'\nschema = [{{ key = 'text', kind = 'scalar' }}]\narguments = ['silent']\n", python.trim())).unwrap();
        std::fs::write(root.join("input.txt"), "hello\n").unwrap();
        let mut child = lingrep_std()
            .current_dir(root)
            .args([
                "tokenize",
                "--analyzer-config",
                "analyzer.toml",
                "--json",
                "input.txt",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                if line.contains("ADAPTER_WAITING") {
                    let _ = sender.send(());
                }
            }
        });
        if receiver.recv_timeout(Duration::from_secs(10)).is_err() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("adapter did not reach the blocked batch");
        }
        assert_eq!(
            unsafe { libc::kill(i32::try_from(child.id()).unwrap(), signal) },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("external analyzer ignored cancellation");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        reader.join().unwrap();
        assert_eq!(output.status.code(), Some(expected));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("\"type\":\"complete\""));
    }
}

#[test]
fn analyzer_file_preserves_the_selected_python_virtual_environment() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(
        Command::new("python3")
            .args(["-m", "venv", "--without-pip"])
            .arg(root.join("venv"))
            .status()
            .unwrap()
            .success()
    );
    let python = root.join("venv/bin/python");
    let site = Command::new(&python)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('purelib'))",
        ])
        .output()
        .unwrap();
    let site = std::path::PathBuf::from(String::from_utf8(site.stdout).unwrap().trim());
    std::fs::write(site.join("cli_probe.py"), "VALUE = 1\n").unwrap();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let adapter = std::fs::read_to_string(
        repository.join("lingrep-core/tests/fixtures/external_echo_adapter.py"),
    )
    .unwrap();
    std::fs::write(
        root.join("adapter.py"),
        format!("import cli_probe\n{adapter}"),
    )
    .unwrap();
    std::fs::write(root.join("analyzer.toml"), "version = 1\nkind = 'protocol'\npython = 'venv/bin/python'\nentrypoint = 'adapter.py'\nlanguage = 'en'\nschema = [{ key = 'text', kind = 'scalar' }]\n").unwrap();
    std::fs::write(root.join("input.txt"), "hello\n").unwrap();
    let output = lingrep_std()
        .current_dir(root)
        .args([
            "tokenize",
            "--analyzer-config",
            "analyzer.toml",
            "--json",
            "input.txt",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
}

#[test]
fn stanza_file_loads_its_sealed_schema_and_supported_provider_settings() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join("adapter/bin")).unwrap();
    std::fs::create_dir_all(root.join("adapter/src")).unwrap();
    std::fs::create_dir_all(root.join("adapter-protocol/src")).unwrap();
    std::fs::write(
        root.join("adapter-protocol/pyproject.toml"),
        "protocol fixture",
    )
    .unwrap();
    std::fs::write(root.join("adapter-protocol/src/__init__.py"), "").unwrap();
    std::fs::create_dir_all(root.join("model")).unwrap();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    std::fs::copy(
        repository.join("lingrep-core/tests/fixtures/external_echo_adapter.py"),
        root.join("adapter/bin/lingrep-stanza-adapter"),
    )
    .unwrap();
    for path in [
        "adapter/pyproject.toml",
        "adapter/uv.lock",
        "adapter/src/__init__.py",
    ] {
        std::fs::write(root.join(path), "").unwrap();
    }
    let profile = serde_json::json!({
        "language": "fr",
        "selections": { "morphology/input": { "schema": [{ "key": "text", "kind": "scalar" }], "boundary_policy": "input-v1" } },
        "selection_digests": { "morphology/input": "00".repeat(32) }
    });
    std::fs::write(
        root.join("model/lingrep-stanza-model.json"),
        serde_json::to_vec(&profile).unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("model/resources.json"), "{}").unwrap();
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let python = String::from_utf8(python.stdout).unwrap();
    std::fs::write(root.join("analyzer.toml"), format!("version = 1\nkind = 'stanza'\npython = '{}'\nentrypoint = 'adapter/bin/lingrep-stanza-adapter'\nmodel = 'model'\n", python.trim())).unwrap();
    std::fs::write(root.join("input.txt"), "bonjour\n").unwrap();
    let output = lingrep_std()
        .current_dir(root)
        .args([
            "tokenize",
            "--analyzer-config",
            "analyzer.toml",
            "--json",
            "input.txt",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = String::from_utf8(output.stdout).unwrap();
    assert!(records.contains("bonjour"));
    assert!(records.contains("\"family\":\"stanza\""));
}

#[test]
fn missing_analyzer_file_reports_its_path_before_reading_stdin() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-analyzer.toml");
    let output = lingrep_std()
        .args(["tokenize", "--analyzer-config"])
        .arg(&missing)
        .args(["--json", "-"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing-analyzer.toml"));
    assert!(output.stdout.is_empty());
}

#[test]
#[ignore = "requires installed spaCy adapter wheel, protocol wheel and ja_ginza"]
fn installed_spacy_setup_produces_a_reusable_verified_analyzer() {
    installed_setup_case(
        "spacy",
        &["--model", "ja_ginza", "--probe-text", "猫が走る。"],
        "猫が走る。\n",
    );
}

#[test]
#[ignore = "requires installed Stanza adapter wheel and downloaded French GSD resources"]
fn installed_stanza_setup_produces_a_reusable_verified_analyzer() {
    installed_setup_case(
        "stanza",
        &[
            "--language",
            "fr",
            "--package",
            "tokenize=gsd",
            "--package",
            "mwt=gsd",
            "--package",
            "pos=gsd_nocharlm",
            "--package",
            "lemma=gsd_nocharlm",
            "--probe-text",
            "Je mange du pain.",
        ],
        "Je mange du pain.\n",
    );
}

fn installed_setup_case(kind: &str, arguments: &[&str], input: &str) {
    let variable = format!("LINGREP_{}_ADAPTER_PYTHON", kind.to_uppercase());
    let python = std::env::var(&variable).expect("installed-wheel Python environment");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let bundle = root.join("analyzer");
    let setup = || {
        Command::new(&python).current_dir(root)
            .args(["-c", &format!("from lingrep_adapter_protocol.setup import setup; import sys; raise SystemExit(setup('{kind}', sys.argv[1:]))")])
            .args(arguments).arg("--output").arg(&bundle).output().unwrap()
    };
    let first = setup();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let profile = std::fs::read(bundle.join(format!("model/lingrep-{kind}-model.json"))).unwrap();
    let second = setup();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(
        profile,
        std::fs::read(bundle.join(format!("model/lingrep-{kind}-model.json"))).unwrap()
    );
    std::fs::write(root.join("input.txt"), input).unwrap();
    let query = |command: &str| {
        lingrep_std()
            .current_dir(root)
            .args([command, "--json", "--analyzer-config"])
            .arg(bundle.join("analyzer.toml"))
            .args(["--pattern", "word := [upos=NOUN].", "input.txt"])
            .output()
            .unwrap()
    };
    let mut identity = None;
    for command in ["matches", "summarize"] {
        let output = query(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("\"complete\""));
        let run: serde_json::Value =
            serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
        identity = Some(run["data"]["analyzer"]["semantic_identity"].clone());
    }
    let package = bundle.join(format!(
        "{kind}-adapter/src/lingrep_{kind}_adapter/adapter.py"
    ));
    let original = std::fs::read(&package).unwrap();
    let mut changed = original.clone();
    changed.extend_from_slice(b"\n");
    std::fs::write(&package, changed).unwrap();
    let altered = query("matches");
    assert!(
        altered.status.success(),
        "{}",
        String::from_utf8_lossy(&altered.stderr)
    );
    let run: serde_json::Value =
        serde_json::from_slice(altered.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_ne!(
        Some(run["data"]["analyzer"]["semantic_identity"].clone()),
        identity
    );
    std::fs::write(&package, original).unwrap();
}

#[test]
fn tei_source_annotations_compose_with_external_tokens_and_change_analysis_identity() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    std::fs::copy(
        repository.join("lingrep-core/tests/fixtures/external_echo_adapter.py"),
        root.join("adapter.py"),
    )
    .unwrap();
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let python = String::from_utf8(python.stdout).unwrap();
    std::fs::write(root.join("analyzer.toml"), format!("version = 1\nkind = 'protocol'\npython = '{}'\nentrypoint = 'adapter.py'\nlanguage = 'en'\nschema = [{{ key = 'text', kind = 'scalar' }}]\n", python.trim())).unwrap();
    let mut results = Vec::new();
    let mut identities = Vec::new();
    for reading in ["hello", "greeting"] {
        std::fs::write(root.join("input.xml"), format!("<TEI xmlns='http://www.tei-c.org/ns/1.0'><text><body><p><ruby><rb>hello</rb><rt>{reading}</rt></ruby></p></body></text></TEI>")).unwrap();
        let output = lingrep_std()
            .current_dir(root)
            .args([
                "matches",
                "--json",
                "--analyzer-config",
                "analyzer.toml",
                "--input-format",
                "tei",
                "--pattern",
                "p := (?<whole> [tei.ruby has any {hello,greeting}]).",
                "input.xml",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let records = events
            .iter()
            .filter(|e| e["type"] == "match")
            .map(|e| e["data"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            1,
            "reading={reading}; {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(records[0]["surface"], "hello");
        assert_eq!(
            records[0]["char_span"],
            serde_json::json!({"start":0,"end":5})
        );
        let complete = &events.last().unwrap()["data"];
        identities.push((
            complete["source_identity"].clone(),
            complete["analysis_identity"].clone(),
        ));
        results.push(records);
        if reading == "hello" {
            let output = lingrep_std()
                .current_dir(root)
                .args([
                    "corpus",
                    "build",
                    "--analyzer-config",
                    "analyzer.toml",
                    "--input-format",
                    "tei",
                    "--output",
                    "artifact",
                    "input.xml",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert_ne!(identities[0].0, identities[1].0);
    assert_ne!(identities[0].1, identities[1].1);
    let output = lingrep_std()
        .current_dir(root)
        .args([
            "matches",
            "--json",
            "--corpus",
            "artifact",
            "--pattern",
            "p := (?<whole> [tei.ruby has hello]).",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>()
        .into_iter()
        .filter(|e| e["type"] == "match")
        .map(|e| e["data"].clone())
        .collect::<Vec<_>>();
    assert_eq!(results[0], records);
}
