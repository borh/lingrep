// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::io::{Read, Seek, SeekFrom, Write};

use crate::support::{dict_path, lingrep_without_dictionary, parse_jsonl};

#[test]
fn describe_is_derived_from_the_public_parser_surface() {
    let output = lingrep_without_dictionary()
        .args(["describe", "matches", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = &parse_jsonl(&output.stdout)[0];
    assert_eq!(record["schema"], "lingrep.capabilities/v1");
    assert_eq!(record["data"]["command"], "matches");
    assert_eq!(
        record["data"]["output_modes"],
        serde_json::json!(["human", "json", "tsv"])
    );
    let arguments = record["data"]["arguments"].as_array().unwrap();
    assert!(
        arguments
            .iter()
            .any(|arg| arg["name"] == "context_color_depth")
    );
    assert!(arguments.iter().any(|arg| {
        arg["name"] == "context_color_theme"
            && arg["values"] == serde_json::json!(["auto", "dark", "light"])
    }));
    let color_defaults = record["data"]["defaults"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .filter(|value| value.starts_with("context_color_"))
        .collect::<Vec<_>>();
    assert_eq!(
        color_defaults,
        ["context_color_depth=5", "context_color_theme=auto"]
    );
    assert!(arguments.iter().any(|arg| arg["name"] == "align_hits"));
    assert!(
        arguments
            .iter()
            .any(|arg| arg["name"] == "morpheme_separator")
    );
    assert!(arguments.iter().any(|arg| {
        arg["name"] == "progress" && arg["values"] == serde_json::json!(["auto", "always", "never"])
    }));
}

#[test]
fn describe_publishes_typed_argv_examples_and_registry_owned_contracts() {
    let output = lingrep_without_dictionary()
        .args(["describe", "matches", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let records = parse_jsonl(&output.stdout);
    let data = &records[0]["data"];
    let example = &data["examples"][0];
    assert_eq!(example["argv"][0], "matches");
    assert_eq!(example["argv"][2], "{patterns}");
    assert!(example["placeholders"]["patterns"].is_string());
    assert!(example["placeholders"]["dictionary"].is_string());
    assert!(example["placeholders"]["root"].is_string());
    assert_eq!(
        data["schemas"],
        serde_json::json!(["lingrep.invocation/v1", "lingrep.query/v2"])
    );
    assert_eq!(
        data["tsv_contracts"],
        serde_json::json!(["lingrep.matches-tsv/v1"])
    );
}

#[test]
fn human_describe_explains_example_purpose_and_placeholders() {
    let output = lingrep_without_dictionary()
        .args(["describe", "matches"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("example — Search a direct corpus with a lingrep patterns catalog."));
    assert!(
        stdout.contains("ling matches --patterns '{patterns}' --dict-path '{dictionary}' '{root}'")
    );
    assert!(stdout.contains("patterns: lingrep patterns catalog file"));
    assert!(stdout.contains("dictionary: Vibrato dictionary cache"));
    assert!(stdout.contains("root: direct corpus root"));
    assert!(stdout.contains("options: ling matches --help-all"));
    assert!(!stdout.contains("schemas:"));
    assert!(!stdout.contains("lingrep.matches-tsv/v1"));
}

#[test]
fn human_contract_detail_exposes_tsv_layout() {
    let output = lingrep_without_dictionary()
        .args(["contracts", "lingrep.matches-tsv/v1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("columns: sequence, pattern, source_name, document_ordinal"));
    assert!(stdout.contains("match_start, match_end, left_surface, surface, right_surface"));
    assert!(stdout.contains("header: none"));
    assert!(stdout.contains("escaping: UTF-8, LF-terminated, RFC-4180-style quoted fields"));
}

#[test]
fn explain_reports_the_default_cache_without_traversing_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let blocked = temp.path().join("must-not-be-traversed");
    std::fs::create_dir(&blocked).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
    }

    let output = lingrep_without_dictionary()
        .args([
            "matches",
            "--plan",
            "--json",
            "--pattern",
            "[].",
            "--dict-path",
            dict_path().to_str().unwrap(),
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = &parse_jsonl(&output.stdout)[0];
    assert_eq!(record["schema"], "lingrep.invocation/v1");
    assert_eq!(record["data"]["input_kind"], "direct");
}

#[test]
fn doctor_probe_failure_is_runtime_failure_not_usage() {
    let data_home = tempfile::tempdir().unwrap();
    let output = lingrep_without_dictionary()
        .env_remove("LINGREP_DICTIONARY_PATH")
        .env("XDG_DATA_HOME", data_home.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let record = &parse_jsonl(&output.stdout)[0];
    assert_eq!(record["schema"], "lingrep.doctor/v1");
    assert_eq!(record["data"]["ready"], false);

    let usage = lingrep_without_dictionary()
        .args(["doctor", "unexpected"])
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn doctor_runs_explicit_probes_in_order_after_independent_failures() {
    let temp = tempfile::tempdir().unwrap();
    let artifact = temp.path().join("artifact");
    tokio::runtime::Runtime::new().unwrap().block_on(
        crate::support::build_single_sentence_artifact(&artifact, "猫。", &[]),
    );
    let missing_dictionary = temp.path().join("missing.dic.zst");
    let output = lingrep_without_dictionary()
        .args(["doctor", "--json", "--dict-path"])
        .arg(&missing_dictionary)
        .arg("--patterns")
        .arg(crate::support::fixture("one.lx"))
        .arg("--corpus")
        .arg(&artifact)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let records = parse_jsonl(&output.stdout);
    let probes = records[0]["data"]["probes"].as_array().unwrap();
    // The binding probe is derived: the dictionary failure is independent of
    // it, and its catalog and corpus prerequisites were both admitted.
    assert_eq!(
        probes
            .iter()
            .map(|probe| probe["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["dictionary", "catalog", "corpus", "binding"]
    );
    assert_eq!(
        probes
            .iter()
            .map(|probe| probe["ok"].as_bool().unwrap())
            .collect::<Vec<_>>(),
        [false, true, true, true]
    );
    let invalid_catalog = temp.path().join("invalid.lx");
    std::fs::write(&invalid_catalog, "p := (.").unwrap();
    let missing_corpus = temp.path().join("missing-artifact");
    for (catalog, corpus, admitted) in [
        (
            invalid_catalog.as_path(),
            artifact.as_path(),
            [false, false, true],
        ),
        (
            crate::support::fixture("one.lx").as_path(),
            missing_corpus.as_path(),
            [false, true, false],
        ),
        (
            invalid_catalog.as_path(),
            missing_corpus.as_path(),
            [false, false, false],
        ),
    ] {
        let output = lingrep_without_dictionary()
            .args(["doctor", "--json", "--dict-path"])
            .arg(&missing_dictionary)
            .arg("--patterns")
            .arg(catalog)
            .arg("--corpus")
            .arg(corpus)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let records = parse_jsonl(&output.stdout);
        let probes = records[0]["data"]["probes"].as_array().unwrap();
        assert_eq!(
            probes
                .iter()
                .map(|probe| probe["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["dictionary", "catalog", "corpus"]
        );
        assert_eq!(
            probes
                .iter()
                .map(|probe| probe["ok"].as_bool().unwrap())
                .collect::<Vec<_>>(),
            admitted
        );
    }
}

/// The binding probe reads the corpus's stored schema, so `doctor` names the
/// exact attributes a `matches` run over this pair would warn about.
#[test]
fn doctor_binding_probe_names_unavailable_attributes() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("one.conllu");
    std::fs::write(
        &input,
        "# text = 猫がいる。\n\
         1\t猫\t猫\tNOUN\t名詞\t_\t3\tnsubj\t_\t_\n\
         2\tが\tが\tADP\t助詞\t_\t1\tcase\t_\t_\n\
         3\tいる\tいる\tVERB\t動詞\t_\t0\troot\t_\t_\n\n",
    )
    .unwrap();
    let artifact = temp.path().join("one.lingcorpus");
    let built = crate::support::lingrep()
        .args(["corpus", "build", "--output"])
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let catalog = temp.path().join("unidic.lx");
    std::fs::write(&catalog, "p := [unidic.pos1=名詞].\n").unwrap();

    let output = lingrep_without_dictionary()
        .args(["doctor", "--json", "--dict-path"])
        .arg(dict_path())
        .arg("--patterns")
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = parse_jsonl(&output.stdout);
    let probes = records[0]["data"]["probes"].as_array().unwrap();
    let binding = probes
        .iter()
        .find(|probe| probe["kind"] == "binding")
        .expect("the binding probe runs when both halves are admitted");
    assert_eq!(binding["ok"], true);
    assert_eq!(
        binding["evidence"]["unavailable"][0]["attribute"],
        "unidic.pos1"
    );
    assert_eq!(binding["evidence"]["unavailable"][0]["definitions"][0], "p");
}

/// A dependency `where` clause on a morphology-only artifact makes the pair
/// not ready — the same refusal a `matches` run would report, found first.
#[test]
fn doctor_binding_probe_fails_on_an_unsatisfiable_where_clause() {
    let temp = tempfile::tempdir().unwrap();
    let artifact = temp.path().join("artifact");
    tokio::runtime::Runtime::new().unwrap().block_on(
        crate::support::build_single_sentence_artifact(&artifact, "猫。", &[]),
    );
    let catalog = temp.path().join("dep.lx");
    std::fs::write(
        &catalog,
        "pattern p := (?<s> [text=猫]) (?<v> [text=走る]) where $v -[nsubj]-> $s.\n",
    )
    .unwrap();

    let output = lingrep_without_dictionary()
        .args(["doctor", "--json", "--dict-path"])
        .arg(dict_path())
        .arg("--patterns")
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let records = parse_jsonl(&output.stdout);
    assert_eq!(records[0]["data"]["ready"], false);
    let probes = records[0]["data"]["probes"].as_array().unwrap();
    let binding = probes
        .iter()
        .find(|probe| probe["kind"] == "binding")
        .unwrap();
    assert_eq!(binding["ok"], false);
    assert_eq!(
        binding["evidence"]["required_missing"],
        serde_json::json!(["dep.head", "dep.rel"])
    );
}

#[test]
fn explicit_dictionary_overrides_a_broken_environment_binding() {
    let dictionary = dict_path();
    let output = crate::support::lingrep()
        .env("LINGREP_DICTIONARY_PATH", "/definitely/missing.dic.zst")
        .args(["doctor", "--json", "--dict-path"])
        .arg(&dictionary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = parse_jsonl(&output.stdout);
    let probe = &records[0]["data"]["probes"][0];
    assert_eq!(probe["ok"], true);
    assert_eq!(probe["target"], dictionary.display().to_string());
    assert_eq!(probe["evidence"]["type"], "dictionary");
}

#[test]
fn doctor_corpus_is_bounded_open_not_full_verification() {
    let temp = tempfile::tempdir().unwrap();
    let artifact = temp.path().join("artifact");
    tokio::runtime::Runtime::new().unwrap().block_on(
        crate::support::build_single_sentence_artifact(&artifact, "猫。", &[]),
    );
    let provenance = artifact.join("provenance.sqlite");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&provenance)
        .unwrap();
    let offset = file.metadata().unwrap().len() / 2;
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x01;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&byte).unwrap();
    file.flush().unwrap();

    let doctor = crate::support::lingrep()
        .args(["doctor", "--json", "--dict-path"])
        .arg(dict_path())
        .arg("--corpus")
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );

    let verify = lingrep_without_dictionary()
        .args(["corpus", "inspect", "--verify", "--json"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert_eq!(verify.status.code(), Some(1));
    assert_eq!(
        parse_jsonl(&verify.stderr)[0]["data"]["code"],
        "component_checksum_mismatch"
    );
}

#[test]
fn plans_validate_only_the_resources_of_the_selected_analyzer() {
    let temp = tempfile::tempdir().unwrap();
    let conllu = temp.path().join("input.conllu");
    std::fs::write(&conllu, "1\t猫\t猫\tNOUN\t_\t_\t0\troot\t_\t_\n\n").unwrap();
    for command in ["matches", "summarize", "tokenize", "coverage"] {
        let mut run = lingrep_without_dictionary();
        run.args([command, "--plan", "--json"]);
        if matches!(command, "matches" | "summarize") {
            run.args(["--pattern", "[]."]);
        }
        let output = run
            .arg(&conllu)
            .env("LINGREP_DICTIONARY_PATH", temp.path().join("missing.dic"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let data = &parse_jsonl(&output.stdout)[0]["data"];
        assert!(data["dictionary_path"].is_null());
    }
    let dictionary = temp.path().join("system_core.dic");
    std::fs::write(
        &dictionary,
        "plan checks paths without loading the dictionary",
    )
    .unwrap();
    let text_input = temp.path().join("native.txt");
    std::fs::write(&text_input, "猫。\n").unwrap();
    let sudachi = lingrep_without_dictionary()
        .args([
            "matches",
            "--plan",
            "--json",
            "--pattern",
            "[].",
            "--analyzer",
            "sudachi",
            "--sudachi-dict",
        ])
        .arg(&dictionary)
        .arg(&text_input)
        .env("LINGREP_DICTIONARY_PATH", temp.path().join("missing.dic"))
        .output()
        .unwrap();
    assert!(
        sudachi.status.success(),
        "{}",
        String::from_utf8_lossy(&sudachi.stderr)
    );
    assert_eq!(
        parse_jsonl(&sudachi.stdout)[0]["data"]["dictionary_path"],
        dictionary.display().to_string()
    );
    let human = lingrep_without_dictionary()
        .args(["matches", "--plan", "--pattern", "[]."])
        .arg(&conllu)
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("patterns: inline.lx"), "{human}");
    assert!(human.contains("settings:"), "{human}");
    assert!(!human.contains("limits: {"), "{human}");
    let config = temp.path().join("external.toml");
    std::fs::write(&config, "version = 1\nkind = 'protocol'\npython = 'python3'\nentrypoint = 'adapter.py'\nlanguage = 'en'\nschema = [{key = 'text', kind = 'scalar'}]\n").unwrap();
    let text = temp.path().join("input.txt");
    std::fs::write(&text, "hello\n").unwrap();
    let output = lingrep_without_dictionary()
        .args([
            "matches",
            "--plan",
            "--json",
            "--pattern",
            "[].",
            "--analyzer-config",
        ])
        .arg(&config)
        .arg(&text)
        .env("LINGREP_DICTIONARY_PATH", temp.path().join("missing.dic"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(parse_jsonl(&output.stdout)[0]["data"]["dictionary_path"].is_null());
}
