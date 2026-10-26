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

use std::fs::{self, File};
use std::process::{Output, Stdio};

use lingrep::protocol::validate_sources_jsonl;
use tempfile::TempDir;

use crate::support::{event_types, lingrep, lingrep_std, parse_jsonl};

fn run_sources(root: &std::path::Path, extra: &[&str]) -> Output {
    let mut command = lingrep();
    command
        .arg("--json")
        .arg("corpus")
        .arg("inventory")
        .args(extra)
        .arg(root)
        .output()
        .unwrap()
}

#[cfg(unix)]
fn sharded_alias_fixture() -> TempDir {
    let directory = TempDir::new().unwrap();
    fs::create_dir(directory.path().join("a")).unwrap();
    fs::write(directory.path().join("a/z.md"), "nested").unwrap();
    fs::write(directory.path().join("a.md"), "a").unwrap();
    fs::hard_link(directory.path().join("a.md"), directory.path().join("z.md")).unwrap();
    directory
}

#[cfg(unix)]
#[test]
fn sources_human_and_tsv_are_distinct_bounded_projections() {
    let corpus = sharded_alias_fixture();
    let human = lingrep()
        .args(["corpus", "inventory"])
        .arg(corpus.path())
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.starts_with("corpus inventory (live)\n"));
    assert!(human.contains("complete: 3 source(s), 2 distinct target(s)"));

    let tsv = lingrep()
        .args(["corpus", "inventory", "--tsv"])
        .arg(corpus.path())
        .output()
        .unwrap();
    assert!(tsv.status.success());
    let rows = String::from_utf8(tsv.stdout).unwrap();
    assert_eq!(
        rows,
        "0\ta/z.md\t0\tfalse\t\n1\ta.md\t1\tfalse\t\n2\tz.md\t1\tfalse\t\n"
    );
}

#[cfg(unix)]
#[test]
fn sources_streams_documents_and_publishes_final_alias_statistics() {
    let corpus = sharded_alias_fixture();
    let output = run_sources(
        corpus.path(),
        &[
            "--max-traversal-depth",
            "7",
            "--max-source-entries",
            "99",
            "--max-directory-entries",
            "11",
            "--max-documents",
            "9",
            "--max-source-index-bytes",
            "100000",
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    validate_sources_jsonl(&output.stdout).unwrap();
    let events = parse_jsonl(&output.stdout);
    assert_eq!(
        event_types(&events),
        ["run", "source", "source", "source", "complete"]
    );
    assert_eq!(events[0]["data"]["source_kind"], "live");
    assert_eq!(events[0]["data"]["aggregate_state"]["state"], "pending");
    assert_eq!(
        events[0]["data"]["traversal_limits"],
        serde_json::json!({
            "max_depth": 7,
            "max_source_entries": 99,
            "max_directory_entries": 11,
            "max_documents": 9,
            "max_index_bytes": 100000,
        })
    );
    for field in ["reader_identity", "admission_policy_identity"] {
        assert_eq!(
            events[0]["data"][field].as_str().unwrap().len(),
            64,
            "{field}"
        );
    }

    let sources = &events[1..4];
    assert_eq!(
        sources
            .iter()
            .map(|event| event["data"]["logical_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a/z.md", "a.md", "z.md"]
    );
    assert_eq!(
        sources
            .iter()
            .map(|event| event["data"]["ordinal"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(
        sources
            .iter()
            .map(|event| event["data"]["target_group_ordinal"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 1]
    );
    assert!(
        sources
            .iter()
            .all(|event| event["data"]["external_target"] == false)
    );
    assert!(
        sources
            .iter()
            .all(|event| event["data"]["target_multiplicity"].is_null()),
        "live traversal cannot publish future multiplicity"
    );

    let complete = &events[4]["data"];
    assert_eq!(complete["coverage"], "full");
    assert_eq!(complete["termination"], "eof");
    assert_eq!(complete["documents"], 3);
    assert_eq!(complete["distinct_targets"], 2);
    assert_eq!(complete["alias_documents"], 1);
    assert_eq!(complete["max_target_multiplicity"], 2);
    assert_eq!(complete["external_targets"], 0);
    assert_eq!(complete["traversal_high_water"]["max_depth"], 2);
    assert_eq!(complete["traversal_high_water"]["examined_entries"], 4);
    assert_eq!(complete["traversal_high_water"]["max_directory_entries"], 3);
    assert!(
        complete["traversal_high_water"]["index_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        complete["ordered_inventory_digest"],
        "0dd3dc5d682e526423d27fbacc12059a0458f084bdcca6bd46015c6d5f2548c4"
    );
}

#[cfg(unix)]
#[test]
fn dangling_link_preserves_admitted_source_prefix_without_complete() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("a.md"), "a").unwrap();
    fs::create_dir(directory.path().join("nested")).unwrap();
    symlink("absent.md", directory.path().join("nested/z.md")).unwrap();

    let output = run_sources(directory.path(), &[]);

    assert_eq!(output.status.code(), Some(1));
    let events = parse_jsonl(&output.stdout);
    assert_eq!(event_types(&events), ["run", "source"]);
    assert_eq!(events[1]["data"]["logical_name"], "a.md");
    assert!(matches!(
        validate_sources_jsonl(&output.stdout),
        Err(lingrep::protocol::ProtocolError::MissingComplete)
    ));
    let diagnostics = parse_jsonl(&output.stderr);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["data"]["code"], "dangling_link");
    assert_eq!(diagnostics[0]["data"]["stage"], "input");
    assert_eq!(
        diagnostics[0]["data"]["location"]["source_name"],
        "nested/z.md"
    );
    assert_eq!(
        diagnostics[0]["data"]["run_id"],
        events[0]["data"]["run_id"]
    );
}

#[cfg(unix)]
#[test]
fn sources_reports_external_targets_without_exposing_resolved_paths() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().unwrap();
    let external = TempDir::new().unwrap();
    fs::write(external.path().join("outside.md"), "outside").unwrap();
    symlink(
        external.path().join("outside.md"),
        directory.path().join("linked.md"),
    )
    .unwrap();

    let output = run_sources(directory.path(), &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    assert_eq!(event_types(&events), ["run", "source", "complete"]);
    assert_eq!(events[1]["data"]["logical_name"], "linked.md");
    assert_eq!(events[1]["data"]["external_target"], true);
    assert_eq!(events[2]["data"]["external_targets"], 1);
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(&external.path().display().to_string())
    );
}

#[test]
fn invalid_root_is_rejected_before_run_publication() {
    let directory = TempDir::new().unwrap();
    let missing = directory.path().join("missing");

    let output = run_sources(&missing, &[]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostics = parse_jsonl(&output.stderr);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["data"]["code"], "discovery_io");
    assert_eq!(diagnostics[0]["data"]["stage"], "input");
    assert!(diagnostics[0]["data"]["run_id"].is_null());
    assert_eq!(diagnostics[0]["data"]["location"]["source_name"], ".");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&missing.display().to_string()));
}

#[cfg(unix)]
#[test]
fn stdout_failure_is_typed_and_cannot_publish_complete() {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("a.md"), "a").unwrap();
    let Some(full) = crate::support::always_failing_stdout() else {
        return;
    };

    let output = lingrep_std()
        .arg("--json")
        .arg("corpus")
        .arg("inventory")
        .arg(directory.path())
        .stdout(Stdio::from(full))
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let diagnostics = parse_jsonl(&output.stderr);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["data"]["code"], "output");
    assert_eq!(diagnostics[0]["data"]["stage"], "output");
    assert!(diagnostics[0]["data"]["run_id"].as_str().is_some());
}

/// A BCCWJ distribution's sources are its zips: one streamed record per
/// inventory zip, each its own target, and the completion counts exactly the
/// records that were streamed.
///
/// Before the fix the cursor streamed one record per zip *entry* while the
/// completion reported the zip count — two figures for one run that disagree
/// by four orders of magnitude on a real distribution — and filed every zip
/// in target group 0, so `documents - 1` of them were reported as aliases of
/// one file.
#[test]
fn bccwj_sources_are_the_zips_and_carry_no_aliases() {
    use std::io::Write;

    let directory = TempDir::new().unwrap();
    let root = directory.path().join("dist");
    let doc = root.join("DOC");
    fs::create_dir_all(&doc).unwrap();
    fs::write(doc.join("Joined_info.txt"), crate::support::BCCWJ_JOINED).unwrap();
    fs::write(doc.join("BCCWJ-NDC.txt"), "a\tb\n").unwrap();
    fs::write(doc.join("CopyRight_Annotation.txt"), "a\tb\n").unwrap();
    // Two zips, and two entries inside one of them: the entry count and the
    // zip count differ, so a report that confuses them cannot pass.
    for (logical, entries) in [
        (
            "C-XML/FIXED/LB/LB.zip",
            &["LBa/one.xml", "LBa/solo.xml"] as &[&str],
        ),
        ("C-XML/VARIABLE/PN/PN.zip", &["PNb/solo.xml"]),
    ] {
        let path = root.join(logical);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(File::create(&path).unwrap()));
        for entry in entries {
            writer
                .start_file(
                    (*entry).to_owned(),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            writer
                .write_all(crate::support::BCCWJ_ENTRY_SOLO.as_bytes())
                .unwrap();
        }
        writer.finish().unwrap();
    }

    let output = run_sources(&root, &["--input-format", "bccwj"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    validate_sources_jsonl(&output.stdout).unwrap();
    let events = parse_jsonl(&output.stdout);

    let names: Vec<&str> = events
        .iter()
        .filter(|event| event["type"] == "source")
        .map(|event| event["data"]["logical_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["C-XML/FIXED/LB/LB.zip", "C-XML/VARIABLE/PN/PN.zip"],
        "the zips are the sources, byte-sorted"
    );

    let complete = events
        .iter()
        .find(|event| event["type"] == "complete")
        .expect("a completion record");
    let inventory = &complete["data"];
    assert_eq!(inventory["documents"], names.len(), "{inventory}");
    assert_eq!(inventory["distinct_targets"], names.len(), "{inventory}");
    assert_eq!(inventory["alias_documents"], 0, "{inventory}");
    assert_eq!(inventory["max_target_multiplicity"], 1, "{inventory}");
    assert_eq!(inventory["external_targets"], 0, "{inventory}");
}

#[test]
fn human_prepared_corpus_output_names_the_location_size_and_verification_result() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.conllu");
    fs::write(&input, "# newdoc id = first\n1\t猫\t猫\tNOUN\t_\t_\t0\troot\t_\t_\n\n# newdoc id = second\n1\t犬\t犬\tNOUN\t_\t_\t0\troot\t_\t_\n\n").unwrap();
    let corpus = temp.path().join("saved.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--output"])
        .arg(&corpus)
        .arg(&input)
        .output()
        .unwrap();
    assert!(built.status.success());
    let built = String::from_utf8(built.stdout).unwrap();
    assert!(built.contains(&corpus.display().to_string()), "{built}");
    assert!(
        built.contains("2 document(s), 2 sentence(s), 2 token(s)"),
        "{built}"
    );
    assert!(built.contains("complete: 1 source(s)"), "{built}");
    let inspect = lingrep()
        .args(["corpus", "inspect", "--verify"])
        .arg(&corpus)
        .output()
        .unwrap();
    assert!(inspect.status.success());
    let inspect = String::from_utf8(inspect.stdout).unwrap();
    assert!(inspect.contains("content verified"), "{inspect}");
}
