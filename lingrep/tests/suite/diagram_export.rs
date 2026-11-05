// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Output;

fn export(catalog: &Path, destination: &Path, extra: &[&str]) -> Output {
    crate::support::lingrep_without_dictionary()
        .args([
            "check",
            "--diagram",
            "--diagram-format",
            "svg",
            "--patterns",
        ])
        .arg(catalog)
        .arg("--output-dir")
        .arg(destination)
        .args(extra)
        .output()
        .unwrap()
}

fn single_svg(catalog: &Path, name: &str) -> Vec<u8> {
    let output = crate::support::lingrep_without_dictionary()
        .args([
            "check",
            "--diagram",
            "--diagram-format",
            "svg",
            "--patterns",
        ])
        .arg(catalog)
        .args(["--name", name])
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

fn tree(directory: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let contents = entry
                .file_type()
                .unwrap()
                .is_file()
                .then(|| fs::read(entry.path()).unwrap());
            (entry.file_name().into_string().unwrap(), contents)
        })
        .collect()
}

#[test]
fn output_dir_writes_selected_definitions_in_source_order_with_bounded_unique_names() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = temp.path().join("catalog.lx");
    let long_a = format!("{}_a", "n".repeat(70));
    let long_b = format!("{}_b", "n".repeat(70));
    let japanese = "動".repeat(30);
    fs::write(
        &catalog,
        format!(
            "fragment f := [upos=VERB].\nVerb := @f+.\nverb := [upos=AUX].\n\
             {long_a} := [upos=NOUN].\n{long_b} := [upos=PROPN].\n{japanese} := [upos=ADJ].\n"
        ),
    )
    .unwrap();
    let destination = temp.path().join("diagrams");

    let output = export(&catalog, &destination, &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let truncated = "n".repeat(64);
    let japanese_stem = "動".repeat(21);
    let expected = [
        ("f", "1-f.svg".to_owned()),
        ("Verb", "2-Verb.svg".to_owned()),
        ("verb", "3-verb.svg".to_owned()),
        (long_a.as_str(), format!("4-{truncated}.svg")),
        (long_b.as_str(), format!("5-{truncated}.svg")),
        (japanese.as_str(), format!("6-{japanese_stem}.svg")),
    ];
    let receipt = expected
        .iter()
        .map(|(name, file)| format!("{name}\t{}\n", destination.join(file).display()))
        .collect::<String>();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), receipt);
    let files = expected
        .iter()
        .map(|(name, file)| (file.clone(), Some(single_svg(&catalog, name))))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(tree(&destination), files);
    assert_eq!(
        tree(temp.path()).into_keys().collect::<Vec<_>>(),
        ["catalog.lx", "diagrams"]
    );

    let selected = temp.path().join("selected");
    let output = export(&catalog, &selected, &["--name", "verb", "--name", "f"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "f\t{}\nverb\t{}\n",
            selected.join("1-f.svg").display(),
            selected.join("2-verb.svg").display()
        )
    );
    assert_eq!(
        tree(&selected),
        BTreeMap::from([
            ("1-f.svg".to_owned(), Some(single_svg(&catalog, "f"))),
            ("2-verb.svg".to_owned(), Some(single_svg(&catalog, "verb"))),
        ])
    );
}

#[test]
fn output_dir_rejects_an_existing_destination_and_leaves_it_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = temp.path().join("catalog.lx");
    fs::write(&catalog, "p := [upos=VERB].").unwrap();
    fs::create_dir(temp.path().join("empty")).unwrap();
    fs::create_dir(temp.path().join("full")).unwrap();
    fs::write(temp.path().join("full").join("1-p.svg"), "kept").unwrap();
    fs::write(temp.path().join("file"), "kept").unwrap();
    let before = tree(temp.path());

    for existing in ["empty", "full", "file"] {
        let output = export(&catalog, &temp.path().join(existing), &[]);

        assert_eq!(output.status.code(), Some(1), "{existing}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
        assert_eq!(tree(temp.path()), before);
        assert_eq!(
            tree(&temp.path().join("full")),
            BTreeMap::from([("1-p.svg".to_owned(), Some(b"kept".to_vec()))])
        );
    }
}

#[test]
fn output_dir_failures_publish_nothing_and_leave_no_staging() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = temp.path().join("catalog.lx");
    let oversized = "@f ".repeat(70);
    fs::write(
        &catalog,
        format!(
            "fragment f := {}.\na := [upos=VERB].\nbig := {oversized}.\nz := [upos=NOUN].\n",
            "[] ".repeat(70)
        ),
    )
    .unwrap();
    fs::write(temp.path().join("file"), "kept").unwrap();
    let before = tree(temp.path());

    let render_failure = export(&catalog, &temp.path().join("out"), &["--expand"]);
    assert_eq!(render_failure.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&render_failure.stderr).contains("cannot draw big"));

    let missing_parent = export(&catalog, &temp.path().join("missing").join("out"), &[]);
    assert_eq!(missing_parent.status.code(), Some(1));

    let file_parent = export(&catalog, &temp.path().join("file").join("out"), &[]);
    assert_eq!(file_parent.status.code(), Some(1));

    for output in [render_failure, missing_parent, file_parent] {
        assert!(output.stdout.is_empty());
    }
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn output_dir_requires_svg_diagrams() {
    for extra in [
        vec!["--diagram", "--output-dir", "unused"],
        vec![
            "--diagram",
            "--diagram-format",
            "ascii",
            "--output-dir",
            "unused",
        ],
        vec!["--output-dir", "unused"],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let output = crate::support::lingrep_without_dictionary()
            .current_dir(temp.path())
            .args(["check", "--pattern", "[]."])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(tree(temp.path()).is_empty());
    }
}

#[test]
fn json_output_rejects_diagram_mode() {
    let output = crate::support::lingrep_without_dictionary()
        .args([
            "--json",
            "check",
            "--diagram",
            "--pattern",
            "p := [upos=NOUN].",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let records = crate::support::parse_jsonl(&output.stderr);
    assert_eq!(
        records[0]["data"]["code"], "unsupported_output",
        "{records:?}"
    );
}
