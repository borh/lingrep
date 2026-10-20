// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use clap::CommandFactory as _;

fn public_paths(command: &clap::Command, prefix: &str, paths: &mut Vec<String>) {
    paths.push(prefix.to_owned());
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
    {
        let path = if prefix.is_empty() {
            child.get_name().to_owned()
        } else {
            format!("{prefix} {}", child.get_name())
        };
        public_paths(child, &path, paths);
    }
}

fn page_name(path: &str) -> String {
    if path.is_empty() {
        "ling.1".to_owned()
    } else {
        format!("ling-{}.1", path.replace(' ', "-"))
    }
}

fn assert_public_arguments_have_help(command: &clap::Command, path: &str) {
    for argument in command
        .get_arguments()
        .filter(|argument| !argument.is_hide_set())
    {
        assert!(
            argument.get_help().is_some() || argument.get_long_help().is_some(),
            "public argument `{}` on `{path}` has no help text",
            argument.get_id()
        );
    }
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set() && child.get_name() != "help")
    {
        let child_path = if path.is_empty() {
            child.get_name().to_owned()
        } else {
            format!("{path} {}", child.get_name())
        };
        assert_public_arguments_have_help(child, &child_path);
    }
}

#[test]
fn every_public_argument_has_standalone_help_text() {
    assert_public_arguments_have_help(&lingrep::Cli::command(), "ling");
}

#[test]
fn check_help_contains_the_pattern_language_reference() {
    let output = crate::support::lingrep_without_dictionary()
        .args(["check", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for spelling in [
        "lingrep patterns LANGUAGE REFERENCE",
        "fragment name := EXPR.",
        "ATTR not has any SET",
        "same_as($name, by=ATTR)",
        "any_order((?<a> EXPR), ...)",
        "not_followed_by(EXPR)",
        "-[enhanced nsubj|obj|obl:* as $role]->",
        "basic_head($dependent) -[enhanced *]-> $dependent",
        "not $head -[enhanced obj|obl:*]-> $dependent",
        "$edge.rel",
        "analyzer: {family:",
        "transform: [{op:",
        "PipeInBareValue",
        "QuotedAttributeValue",
        "resolve SUBJECT partition:",
        "admit SUBJECT terminal:",
    ] {
        assert!(
            stdout.contains(spelling),
            "missing `{spelling}` from:\n{stdout}"
        );
    }
}

#[test]
fn check_short_help_points_to_the_complete_reference() {
    let output = crate::support::lingrep_without_dictionary()
        .args(["check", "-h"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            "Run `ling check --help` for the complete lingrep patterns language reference."
        ),
        "{stdout}"
    );
    assert!(
        !stdout.contains("lingrep patterns LANGUAGE REFERENCE"),
        "{stdout}"
    );
}

#[test]
fn manpages_cover_root_and_every_public_nested_command() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("pages");
    let output = crate::support::lingrep_without_dictionary()
        .args(["manpages", "--output"])
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut paths = Vec::new();
    public_paths(&lingrep::Cli::command(), "", &mut paths);
    let mut actual = std::fs::read_dir(&output_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = paths.iter().map(|path| page_name(path)).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected);

    for name in [
        "ling.1",
        "ling-check.1",
        "ling-matches.1",
        "ling-corpus-build.1",
    ] {
        let page = std::fs::read_to_string(output_dir.join(name)).unwrap();
        assert!(page.contains(".SH NAME"), "{name}");
        assert!(page.contains(".SH SYNOPSIS"), "{name}");
        assert!(page.contains("OPTIONS"), "{name}");
        assert!(page.contains(".SH EXAMPLES"), "{name}");
        if name == "ling.1" {
            assert!(page.contains(r"ling check \-\-help"), "{name}");
        }
        if name == "ling-check.1" {
            assert!(
                page.contains("lingrep patterns LANGUAGE REFERENCE"),
                "{name}"
            );
            assert!(page.contains("not_followed_by"), "{name}");
            assert!(
                page.contains(r"\-[enhanced nsubj|obj|obl:* as $role]\->"),
                "{name}"
            );
            assert!(
                page.contains(r"basic_head($dependent) \-[enhanced *]\-> $dependent"),
                "{name}"
            );
            assert!(
                page.contains(r"not $head \-[enhanced obj|obl:*]\-> $dependent"),
                "{name}"
            );
            assert!(page.contains("transform:"), "{name}");
            assert!(page.contains("PipeInBareValue"), "{name}");
        }
        if name == "ling-matches.1" {
            assert!(page.contains(r"\fB\-\-align\-hits\fR"), "{name}");
            assert!(
                page.contains(r"\fB\-\-morpheme\-separator\fR \fI<TEXT>\fR"),
                "{name}"
            );
            assert!(
                page.contains(r"\fB\-\-context\-color\-depth\fR \fI<N>\fR"),
                "{name}"
            );
            assert!(
                page.contains(r"\fB\-\-context\-color\-theme\fR \fI<THEME>\fR"),
                "{name}"
            );
            assert!(!page.contains(r"\fB\-\-context\-colors\fR"), "{name}");
        }
    }
}

#[test]
fn manpages_reject_an_existing_directory_without_changing_it() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("pages");
    std::fs::create_dir(&output_dir).unwrap();
    let sentinel = output_dir.join("keep");
    std::fs::write(&sentinel, b"owned by caller").unwrap();
    let output = crate::support::lingrep_without_dictionary()
        .args(["manpages", "--output"])
        .arg(&output_dir)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"owned by caller");
    assert_eq!(std::fs::read_dir(&output_dir).unwrap().count(), 1);
}

#[test]
fn manpages_json_reports_only_committed_pages_in_order() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("pages");
    let output = crate::support::lingrep_without_dictionary()
        .args(["manpages", "--output"])
        .arg(&output_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let records = crate::support::parse_jsonl(&output.stdout);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["schema"], "lingrep.manpages/v1");
    assert_eq!(records[0]["type"], "manpages");
    let pages = records[0]["data"]["pages"].as_array().unwrap();
    let mut paths = Vec::new();
    public_paths(&lingrep::Cli::command(), "", &mut paths);
    assert_eq!(
        pages
            .iter()
            .map(|page| page["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        paths.iter().map(|path| page_name(path)).collect::<Vec<_>>()
    );
    assert!(pages.iter().all(|page| page["bytes"].as_u64().unwrap() > 0));
}
