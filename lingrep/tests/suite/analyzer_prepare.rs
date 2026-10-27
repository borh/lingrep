use crate::support::lingrep_without_dictionary;

#[test]
fn cpu_preparation_requires_an_output_and_stanza_selection() {
    for args in [
        vec![
            "ling", "analyzer", "prepare", "ginza", "--output", "analyzer",
        ],
        vec![
            "ling",
            "analyzer",
            "prepare",
            "stanza",
            "--output",
            "analyzer",
            "--language",
            "fr",
            "--package",
            "tokenize=gsd",
        ],
    ] {
        assert!(lingrep::Cli::try_parse_from(args).is_ok());
    }
    for args in [
        vec!["ling", "analyzer", "prepare", "ginza"],
        vec![
            "ling", "analyzer", "prepare", "stanza", "--output", "analyzer",
        ],
    ] {
        assert!(lingrep::Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn preparation_does_not_replace_an_existing_directory() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("analyzer");
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("keep"), b"original").unwrap();
    let result = lingrep_without_dictionary()
        .args(["analyzer", "prepare", "ginza", "--output"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("incomplete analyzer directory"));
    assert_eq!(std::fs::read(output.join("keep")).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
}

#[test]
fn failed_preparation_removes_only_its_new_directory() {
    use sha2::{Digest, Sha256};
    for corrupt in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let wheels = directory.path().join("wheels");
        std::fs::create_dir(&wheels).unwrap();
        let mut sums = String::new();
        for package in ["lingrep_adapter_protocol", "lingrep_spacy_adapter"] {
            let name = format!("{package}-{}-py3-none-any.whl", env!("CARGO_PKG_VERSION"));
            let content = b"fixture wheel";
            std::fs::write(wheels.join(&name), content).unwrap();
            let digest = if corrupt {
                "0".repeat(64)
            } else {
                Sha256::digest(content)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            };
            sums.push_str(&format!("{digest}  ./{name}\n"));
        }
        std::fs::write(wheels.join("SHA256SUMS"), sums).unwrap();
        let output = directory.path().join("analyzer");
        let result = lingrep_without_dictionary()
            .args(["analyzer", "prepare", "ginza", "--output"])
            .arg(&output)
            .arg("--wheels")
            .arg(&wheels)
            .env("PATH", "")
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        let expected = if corrupt {
            "checksum mismatch"
        } else {
            "cannot run uv"
        };
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(&wheels).unwrap().count(), 3);
    }
}

#[test]
fn invalid_reuse_receipts_fail_before_python_without_changing_files() {
    use serde_json::json;
    #[derive(Clone, Copy, Debug)]
    enum Case {
        Release,
        Selection,
        WheelSet,
        WheelHash,
    }
    for case in [
        Case::Release,
        Case::Selection,
        Case::WheelSet,
        Case::WheelHash,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join("wheels")).unwrap();
        let mut receipt = json!({
            "request": {"release": env!("CARGO_PKG_VERSION"), "provider": "ginza", "language": null, "packages": [], "probe_text": "猫が走る。"},
            "wheels": {}, "semantic_identity": vec![0; 32]
        });
        let expected = match case {
            Case::Release => {
                receipt["request"]["release"] = json!("0.4.0");
                "prepared with ling 0.4.0"
            }
            Case::Selection => {
                receipt["request"]["probe_text"] = json!("different");
                "different preparation"
            }
            Case::WheelSet => "invalid wheel set",
            Case::WheelHash => {
                for package in ["lingrep_adapter_protocol", "lingrep_spacy_adapter"] {
                    let name = format!("{package}-{}-py3-none-any.whl", env!("CARGO_PKG_VERSION"));
                    std::fs::write(root.join("wheels").join(&name), b"changed").unwrap();
                    receipt["wheels"][name] = json!("0".repeat(64));
                }
                "checksum mismatch"
            }
        };
        let bytes = serde_json::to_vec(&receipt).unwrap();
        std::fs::write(root.join("prepare.json"), &bytes).unwrap();
        let result = lingrep_without_dictionary()
            .args(["analyzer", "prepare", "ginza", "--output"])
            .arg(root)
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{case:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(std::fs::read(root.join("prepare.json")).unwrap(), bytes);
        assert!(!root.join("analyzer.toml").exists());
    }
}

#[test]
#[ignore = "downloads Python and models; requires uv and LINGREP_ADAPTER_WHEELS"]
fn analyzer_directories_survive_cache_removal() {
    for (provider, selection, text) in [
        ("ginza", vec![], "猫が走る。\n"),
        (
            "stanza",
            vec![
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
            ],
            "Je mange du pain.\n",
        ),
    ] {
        let wheels = std::env::var_os("LINGREP_ADAPTER_WHEELS").expect("release wheel directory");
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let output = root.join("analyzer");
        let cache = root.join("cache");
        let command = || {
            let mut command = lingrep_without_dictionary();
            command
                .env("UV_CACHE_DIR", &cache)
                .env("HF_HOME", root.join("shared-hf"))
                .env("HF_HUB_CACHE", root.join("shared-hub"))
                .env("HF_XET_CACHE", root.join("shared-xet"))
                .env_remove("UV_OFFLINE");
            command
        };
        let prepare = |destination: &std::path::Path| {
            let mut command = command();
            command
                .args(["analyzer", "prepare", provider, "--output"])
                .arg(destination)
                .args(&selection)
                .arg("--wheels")
                .arg(&wheels);
            command
        };
        let initial = prepare(&output).output().unwrap();
        assert!(
            initial.status.success(),
            "{}",
            String::from_utf8_lossy(&initial.stderr)
        );
        for name in ["shared-hf", "shared-hub", "shared-xet"] {
            assert!(
                !root.join(name).exists(),
                "external model cache was created: {name}"
            );
        }
        assert!(!std::fs::read_dir(&output).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("downloads-")
        }));
        let receipt = std::fs::read(output.join("prepare.json")).unwrap();
        let second = root.join("second analyzer");
        let repeated = prepare(&second).output().unwrap();
        assert!(
            repeated.status.success(),
            "{}",
            String::from_utf8_lossy(&repeated.stderr)
        );
        let first_receipt: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
        let second_receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(second.join("prepare.json")).unwrap()).unwrap();
        assert_eq!(
            first_receipt["semantic_identity"], second_receipt["semantic_identity"],
            "{provider}: analyzer identity depends on output location"
        );
        std::fs::remove_dir_all(&second).unwrap();
        let input = root.join("input.txt");
        std::fs::write(&input, text).unwrap();
        let query = || {
            let result = command()
                .args(["matches", "--json", "--analyzer-config"])
                .arg(output.join("analyzer.toml"))
                .args(["--pattern", "word := []."])
                .arg(&input)
                .env("PATH", "")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let records = result
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert!(records.iter().any(|record| record["type"] == "complete"));
            records
                .into_iter()
                .filter(|record| record["type"] == "match")
                .collect::<Vec<_>>()
        };
        let before = query();
        assert!(!before.is_empty());
        std::fs::remove_dir_all(&cache).unwrap();
        let reused = prepare(&output)
            .env("PATH", "")
            .env("UV_OFFLINE", "1")
            .output()
            .unwrap();
        assert!(
            reused.status.success(),
            "{}",
            String::from_utf8_lossy(&reused.stderr)
        );
        assert_eq!(reused.stdout, initial.stdout);
        assert_eq!(std::fs::read(output.join("prepare.json")).unwrap(), receipt);
        assert_eq!(query(), before);
    }
}
