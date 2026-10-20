// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;

use lingrep::examples::{CommandExample, all_examples};

fn materialize(example: &CommandExample, values: &BTreeMap<&str, String>) -> Vec<String> {
    example
        .argv
        .iter()
        .map(|token| {
            token
                .strip_prefix('{')
                .and_then(|value| value.strip_suffix('}'))
                .map_or_else(|| (*token).to_owned(), |name| values[name].clone())
        })
        .collect()
}

#[test]
fn every_published_example_parses_and_executes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("one.md"), "猫。\n").unwrap();
    let artifact = temp.path().join("prepared");
    tokio::runtime::Runtime::new().unwrap().block_on(
        crate::support::build_single_sentence_artifact(&artifact, "猫。", &[]),
    );
    let artifact_output = temp.path().join("built-by-example");
    let manpage_output = temp.path().join("manpages-by-example");
    let values = BTreeMap::from([
        (
            "patterns",
            crate::support::fixture("one.lx").display().to_string(),
        ),
        (
            "dictionary",
            crate::support::dict_path().display().to_string(),
        ),
        ("root", root.display().to_string()),
        ("artifact", artifact.display().to_string()),
        ("artifact_output", artifact_output.display().to_string()),
        ("manpage_output", manpage_output.display().to_string()),
    ]);

    for example in all_examples() {
        let argv = materialize(example, &values);
        let mut parse_argv = vec!["ling".to_owned()];
        parse_argv.extend(argv.clone());
        lingrep::Cli::try_parse_from(parse_argv)
            .unwrap_or_else(|error| panic!("{} did not parse: {error}", example.id));
        let mut command = if example
            .placeholders
            .iter()
            .any(|item| item.name == "dictionary")
        {
            crate::support::lingrep()
        } else {
            crate::support::lingrep_without_dictionary()
        };
        let output = command.args(&argv).output().unwrap();
        assert!(
            output.status.success(),
            "{}: {}",
            example.id,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(artifact_output.exists());
    assert!(manpage_output.join("ling.1").exists());
}
