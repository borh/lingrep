// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use crate::support::lingrep_without_dictionary;

#[test]
fn retired_cli_spellings_are_rejected_without_aliases() {
    let retired = [
        vec!["--protocol-version", "2", "check", "--pattern", "[]."],
        vec!["--diagnostic-format", "json", "check", "--pattern", "[]."],
        vec!["check", "--format", "human", "--pattern", "[]."],
        vec!["matches", "--format", "jsonl"],
        vec!["matches", "--no-policy"],
        vec!["matches", "--progress", "stderr"],
        vec!["matches", "--cached"],
        vec!["matches", "--ephemeral"],
        vec!["matches", "--no-cache"],
        vec!["matches", "--explain"],
        vec!["attributes"],
        vec!["schema"],
        vec!["corpus", "sources"],
        vec!["tokenize", "--profile", "tokenize"],
    ];

    for args in retired {
        let output = lingrep_without_dictionary().args(&args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "retired invocation unexpectedly parsed: {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
