// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use crate::support::{
    lingrep_binary_path, lingrep_without_dictionary, parse_jsonl, shell_without_lingrep_binary_env,
};
use lingrep::protocol::ContractId;

#[test]
fn every_supported_shell_generates_from_the_production_command() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let output = lingrep_without_dictionary()
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let script = String::from_utf8(output.stdout).unwrap();
        assert!(script.contains("matches"), "{shell}");
        assert!(!script.contains("sentence-final"), "{shell}");
        assert!(script.contains("json"), "{shell}");
        assert!(script.contains("color"), "{shell}");
        assert!(script.contains("align-hits"), "{shell}");
        assert!(script.contains("morpheme-separator"), "{shell}");
        assert!(script.contains("context-color-depth"), "{shell}");
        assert!(script.contains("context-color-theme"), "{shell}");
        assert!(!script.contains("--context-colors"), "{shell}");
        assert!(script.contains("contracts"), "{shell}");
        for id in ContractId::ALL {
            assert!(script.contains(id.as_str()), "{shell}: {}", id.as_str());
        }
    }
}

#[test]
fn json_completion_record_wraps_the_exact_human_script() {
    let human = lingrep_without_dictionary()
        .args(["completions", "bash"])
        .output()
        .unwrap();
    let machine = lingrep_without_dictionary()
        .args(["completions", "bash", "--json"])
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(machine.status.success());
    let record = &parse_jsonl(&machine.stdout)[0];
    assert_eq!(record["schema"], "lingrep.completions/v1");
    assert_eq!(record["data"]["shell"], "bash");
    assert_eq!(
        record["data"]["script"],
        String::from_utf8(human.stdout).unwrap()
    );
}

#[cfg(unix)]
fn completion_script(shell: &str) -> String {
    let output = lingrep_without_dictionary()
        .args(["completions", shell])
        .output()
        .unwrap();
    assert!(output.status.success(), "{shell}");
    String::from_utf8(output.stdout).unwrap()
}

#[cfg(unix)]
fn complete_in_bash(script: &std::path::Path, words: &[&str]) -> String {
    let output = shell_without_lingrep_binary_env("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            r#"
. "$1"
shift
COMP_WORDS=("$@")
COMP_CWORD=$((${#COMP_WORDS[@]} - 1))
cur=${COMP_WORDS[$COMP_CWORD]}
if (( COMP_CWORD > 0 )); then
    prev=${COMP_WORDS[$((COMP_CWORD - 1))]}
else
    prev=
fi
_ling "${COMP_WORDS[0]}" "$cur" "$prev"
complete -p ling
printf '%s\n' "${COMPREPLY[@]}"
"#,
            "ling-completion-test",
        ])
        .arg(script)
        .args(words)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The prompt the Zsh and Elvish pty runtimes configure inside
/// `completest_pty`.
///
/// Its presence on the captured screen is what separates a completion *result*
/// from a harness timeout — see [`complete_after_the_shell_is_ready`].
#[cfg(unix)]
const SHELL_PROMPT: &str = "% ";

/// Complete `input`, retrying while the shell never reached its prompt.
///
/// `completest_pty::comptest` writes the input to the pty and then waits for
/// output to go quiet for 50ms, having no other way to know a shell is done. But
/// the pty echoes those bytes back the instant they are written — before the
/// shell has read its rcfile — so the quiet window can open against shell
/// *startup*. Bash therefore uses [`complete_in_bash`], which invokes its
/// generated function through the shell's completion variables without a
/// guessed wall-clock window. These retries remain for the Zsh and Elvish pty
/// integrations.
///
/// A screen without the prompt therefore says the shell never got far enough to
/// complete anything, which is a fact about the harness and not about the script
/// under test: retry it. A screen *with* the prompt is a real completion result
/// and is returned whatever it says, so a genuinely wrong completion still fails
/// on the first attempt rather than being retried into a green run.
#[cfg(unix)]
fn complete_after_the_shell_is_ready(
    runtime: &mut dyn completest_pty::Runtime,
    input: &str,
    term: &completest_pty::Term,
    shell: &str,
) -> String {
    const ATTEMPTS: usize = 4;

    for _ in 0..ATTEMPTS {
        let screen = runtime.complete(input, term).unwrap();
        if screen.contains(SHELL_PROMPT) {
            return screen;
        }
    }
    panic!(
        "{shell} never reached its prompt in {ATTEMPTS} attempts at {input:?}: \
         the completion harness is timing out during shell startup"
    );
}

#[cfg(unix)]
#[test]
fn generated_scripts_complete_real_commands_in_supported_shells() {
    use completest_pty::{Runtime, Term};

    let temp = tempfile::tempdir().unwrap();
    let binary = lingrep_binary_path();
    let bin_root = binary.parent().unwrap().to_path_buf();
    let bash_script = temp.path().join("ling.bash");
    std::fs::write(&bash_script, completion_script("bash")).unwrap();
    for (words, expected) in [
        (&["ling", "contr"][..], "contracts"),
        (
            &["ling", "contracts", "lingrep.que"][..],
            "lingrep.query/v2",
        ),
        (&["ling", "matches", "--prog"][..], "--progress"),
    ] {
        let output = complete_in_bash(&bash_script, words);
        assert!(
            output.contains("-F _ling ling"),
            "bash did not register the generated completion function: {output:?}"
        );
        assert!(
            output.contains(expected),
            "bash did not complete {words:?} to {expected:?}: {output:?}"
        );
    }

    let mut runtimes: Vec<(&str, Box<dyn Runtime>)> = vec![
        (
            "zsh",
            Box::new(
                completest_pty::ZshRuntime::new(bin_root.clone(), temp.path().join("zsh")).unwrap(),
            ),
        ),
        (
            "elvish",
            Box::new(
                completest_pty::ElvishRuntime::new(bin_root, temp.path().join("elvish")).unwrap(),
            ),
        ),
    ];
    let term = Term::new();

    for (shell, runtime) in &mut runtimes {
        runtime.register("ling", &completion_script(shell)).unwrap();
        for (input, expected) in [
            ("ling contr", "contracts"),
            ("ling contracts lingrep.", "lingrep.query/v2"),
            ("ling matches --p", "--progress"),
        ] {
            let input = format!("{input}\t\t");
            let output = complete_after_the_shell_is_ready(runtime.as_mut(), &input, &term, shell);
            assert!(
                output.contains(expected),
                "{shell} did not complete {input:?} to {expected:?}: {output:?}"
            );
        }
    }

    let fish_script = temp.path().join("ling.fish");
    std::fs::write(&fish_script, completion_script("fish")).unwrap();
    let fish = std::env::var("LINGREP_FISH_PATH").expect("Nix dev shell sets LINGREP_FISH_PATH");
    for (input, expected) in [
        ("ling contr", "contracts"),
        ("ling contracts lingrep.", "lingrep.query/v2"),
        ("ling matches --p", "--progress"),
    ] {
        let output = shell_without_lingrep_binary_env(&fish)
            .args([
                "--no-config",
                "-c",
                "source $argv[1]; complete -C $argv[2]",
                fish_script.to_str().unwrap(),
                input,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.contains(expected),
            "fish did not complete {input:?} to {expected:?}: {output:?}"
        );
    }
}
