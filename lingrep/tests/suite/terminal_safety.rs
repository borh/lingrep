use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::process::Stdio;

use lingrep_core::testing::MiniDictionary;
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;

use lingrep::output::{KwicLine, format_kwic_aligned_line, sanitize_terminal};

use crate::support::{lingrep, lingrep_std, parse_jsonl};

#[test]
fn terminal_controls_are_visible_before_width_measurement() {
    let hostile = "左\u{1b}[2J\u{0085}\u{202e}右";
    let safe = sanitize_terminal(hostile);
    assert_eq!(safe, "左\\u{001b}[2J\\u{0085}\\u{202e}右");
    assert_eq!(UnicodeWidthStr::width(safe.as_ref()), 31);
    assert!(!safe.contains('\u{1b}'));
    assert!(!safe.contains('\u{202e}'));
}

#[test]
fn aligned_output_sanitizes_every_dynamic_surface_before_layout() {
    let line = KwicLine {
        pre: "left\u{1b}[2J".into(),
        key: "\u{202e}key".into(),
        post: "\u{0085}right".into(),
        match_start: 0,
        match_end: 1,
    };
    let rendered = format_kwic_aligned_line(&line, 80);
    assert!(!rendered.contains('\u{1b}'));
    assert!(!rendered.contains('\u{202e}'));
    assert!(!rendered.contains('\u{0085}'));
    assert!(rendered.contains("\\u{001b}[2J"));
}

#[test]
fn aligned_zwj_sequences_are_never_split() {
    let line = KwicLine {
        pre: "before".into(),
        key: "👩‍💻".into(),
        post: "after".into(),
        match_start: 0,
        match_end: 1,
    };
    let rendered = format_kwic_aligned_line(&line, 12);
    assert!(rendered.contains("👩‍💻") || rendered.contains('…'));
    assert!(!rendered.contains("👩‍…"));
}

#[test]
fn aligned_truncation_never_emits_half_of_a_flag_grapheme() {
    let line = KwicLine {
        pre: String::new(),
        key: "K".into(),
        post: "🇯🇵🇯🇵".into(),
        match_start: 0,
        match_end: 1,
    };
    let rendered = format_kwic_aligned_line(&line, 11);
    assert!(!rendered.contains('🇯'));
    assert!(rendered.contains('…'));
}

#[test]
fn prepared_jsonl_output_failure_after_a_committed_record_is_typed() {
    let dictionary = MiniDictionary::new();
    {
        let directory = TempDir::new().unwrap();
        let corpus = directory.path().join("many.txt");
        fs::write(&corpus, "猫。\n".repeat(2_000)).unwrap();
        let patterns = directory.path().join("patterns.lx");
        fs::write(&patterns, "pattern cat := [text=猫].\n").unwrap();
        let artifact = directory.path().join("corpus.lingcorpus");
        let build = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("--output")
            .arg(&artifact)
            .arg(&corpus)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );

        let mut child = lingrep_std()
            .args(["--json", "matches", "--patterns"])
            .arg(&patterns)
            .args(["--corpus"])
            .arg(&artifact)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut run_id = None;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            let event = parse_jsonl(line.as_bytes()).pop().unwrap();
            match event["type"].as_str().unwrap() {
                "run" => run_id = event["data"]["run_id"].as_str().map(str::to_owned),
                "match" => {
                    assert_eq!(event["data"]["sequence"], 0);
                    break;
                }
                _ => {}
            }
        }
        drop(reader);

        let status = child.wait().unwrap();
        let mut stderr = Vec::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        assert_eq!(status.code(), Some(1));
        let diagnostic = parse_jsonl(&stderr);
        assert_eq!(diagnostic[0]["data"]["code"], "broken_pipe");
        assert_eq!(diagnostic[0]["data"]["stage"], "output");
        assert_eq!(diagnostic[0]["data"]["run_id"], run_id.unwrap());
    }
}

#[test]
fn human_stdout_broken_pipe_is_successful_display_termination() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("many.txt");
    fs::write(&corpus, "猫。\n".repeat(2_000)).unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern cat := [text=猫].\n").unwrap();

    let mut child = lingrep_std()
        .args(["matches", "--dict-path"])
        .arg(dictionary.path())
        .args(["--patterns"])
        .arg(&patterns)
        .arg(&corpus)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    assert_ne!(reader.read_line(&mut first).unwrap(), 0);
    drop(reader);

    let status = child.wait().unwrap();
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(!String::from_utf8_lossy(&stderr).contains("broken_pipe"));
}

#[test]
fn closed_progress_stderr_does_not_truncate_result_coverage() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("many.txt");
    fs::write(&corpus, "猫。\n".repeat(2_000)).unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern cat := [text=猫].\n").unwrap();
    let output_path = directory.path().join("matches.jsonl");
    let output_file = fs::File::create(&output_path).unwrap();

    let mut child = lingrep_std()
        .args(["matches", "--json", "--progress", "always"])
        .args(["--batch-sentences", "1", "--dict-path"])
        .arg(dictionary.path())
        .args(["--patterns"])
        .arg(&patterns)
        .arg(&corpus)
        .stdout(Stdio::from(output_file))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut progress = BufReader::new(child.stderr.take().unwrap());
    let mut first = String::new();
    assert_ne!(progress.read_line(&mut first).unwrap(), 0);
    drop(progress);

    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let events = parse_jsonl(&fs::read(output_path).unwrap());
    let complete = events.last().unwrap();
    assert_eq!(complete["type"], "complete");
    assert_eq!(complete["data"]["counters"]["sentences_matched"], 2_000);
}

#[test]
fn matches_exits_one_when_writing_a_warning_to_closed_stderr_fails() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.conllu");
    fs::write(
        &corpus,
        "1\t猫\t猫\tNOUN\t名詞\t_\t0\troot\t_\t_\n2\t。\t。\tPUNCT\t補助記号\t_\t1\tpunct\t_\t_\n\n",
    )
    .unwrap();
    // The CoNLL-U token schema lacks `unidic.pos1`, so `ling` writes
    // `warning[attribute_unavailable]` for `b` before it writes `a`'s match.
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "a := [upos=NOUN].\nb := [unidic.pos1=名詞].\n").unwrap();

    for format in [None, Some("--tsv"), Some("--json")] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = lingrep_std()
            .args(["matches", "--progress", "never", "--patterns"])
            .arg(&patterns)
            .arg(&corpus)
            .args(format)
            .stderr(writer)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{format:?}");
        if format == Some("--json") {
            assert!(
                parse_jsonl(&output.stdout)
                    .iter()
                    .all(|event| event["type"] != "complete"),
                "{format:?}"
            );
        }
    }
}

#[test]
fn matches_exits_one_when_writing_a_requested_policy_report_to_closed_stderr_fails() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.conllu");
    fs::write(&corpus, "1\tdone\tdo\tVERB\tVBN\t_\t0\troot\t_\t_\n\n").unwrap();

    for format in [None, Some("--tsv"), Some("--json")] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = lingrep_std()
            .args(["matches", "--pattern", "[].", "--trace-policy", "0"])
            .arg(&corpus)
            .args(format)
            .stderr(writer)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{format:?}");
    }
}

#[cfg(unix)]
fn open_pty() -> (fs::File, fs::File) {
    use std::os::fd::FromRawFd;

    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    unsafe { (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave)) }
}

/// Everything the child wrote to a pty, with the line discipline's `\r\n`
/// mapped back to `\n`. Reading ends with `EIO` once no slave is open.
#[cfg(unix)]
fn read_pty(mut master: fs::File) -> String {
    let mut bytes = Vec::new();
    let _ = master.read_to_end(&mut bytes);
    String::from_utf8(bytes)
        .expect("pty output is UTF-8")
        .replace("\r\n", "\n")
}

/// A stdout stream with its run id and elapsed times removed.
fn without_volatile_fields(stdout: &str) -> String {
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                map.remove("run_id");
                map.remove("elapsed_millis");
                map.values_mut().for_each(strip);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    stdout
        .lines()
        .map(|line| {
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) {
                strip(&mut value);
                value.to_string()
            } else if let Some((head, _)) = line.split_once("completed in ") {
                format!("{head}completed in <elapsed>")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
#[test]
fn progress_output_follows_mode_format_delivery_and_terminals_without_changing_stdout() {
    const ERASE: &str = "\r\x1b[2K";
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.conllu");
    fs::write(&corpus, "1\tdone\tdo\tVERB\tVBN\t_\t0\troot\t_\t_\n\n").unwrap();
    let artifact = directory.path().join("one.lingcorpus");
    let built = lingrep_std()
        .args(["corpus", "build", "--output"])
        .arg(&artifact)
        .arg(&corpus)
        .output()
        .unwrap();
    assert!(built.status.success(), "{built:?}");
    let corpus = corpus.to_str().unwrap();
    let artifact = artifact.to_str().unwrap();

    // (arguments, results deferred until the run ends, counter line)
    let commands: [(&[&str], bool, &str); 4] = [
        (
            &["matches", "--pattern", "[].", corpus],
            false,
            "matched 1 sentences, found 1 matches",
        ),
        (
            &["summarize", "--pattern", "[].", corpus],
            true,
            "matched 1 sentences, found 1 matches",
        ),
        (
            &["coverage", corpus],
            true,
            "observed 1 sentences, 1 tokens",
        ),
        (
            &["tokenize", "--corpus", artifact],
            false,
            "tokenized 1 prepared sentences, emitted 1 tokens",
        ),
    ];
    let terminals = [
        (false, false, "xterm"),
        (true, false, "xterm"),
        (false, true, "xterm"),
        (true, true, "xterm"),
        (true, true, "dumb"),
    ];
    let mut stdout_by_case = std::collections::HashMap::new();
    for (arguments, deferred, counter) in commands {
        for format in [None, Some("--json"), Some("--tsv")] {
            for (stdout_terminal, stderr_terminal, term) in terminals {
                for mode in ["auto", "always", "never"] {
                    let label = format!(
                        "{arguments:?} {format:?} stdout={stdout_terminal} \
                         stderr={stderr_terminal} TERM={term} --progress {mode}"
                    );
                    let mut command = lingrep_std();
                    command
                        .args(arguments)
                        .args(format)
                        .args(["--progress", mode])
                        .env("TERM", term)
                        .env("NO_COLOR", "1");
                    let stdout_pty = stdout_terminal.then(|| {
                        let (master, slave) = open_pty();
                        command.stdout(slave);
                        master
                    });
                    let stderr_pty = stderr_terminal.then(|| {
                        let (master, slave) = open_pty();
                        command.stderr(slave);
                        master
                    });
                    let output = command.output().unwrap();
                    drop(command);
                    assert!(output.status.success(), "{label}: {output:?}");
                    let stdout = stdout_pty.map_or_else(
                        || String::from_utf8(output.stdout.clone()).unwrap(),
                        read_pty,
                    );
                    let stderr = stderr_pty.map_or_else(
                        || String::from_utf8(output.stderr.clone()).unwrap(),
                        read_pty,
                    );

                    let transient = format.is_none()
                        && deferred
                        && stdout_terminal
                        && stderr_terminal
                        && term != "dumb";
                    match mode {
                        "always" if transient => {
                            assert!(
                                stderr.contains(&format!("{ERASE}{counter}")),
                                "{label}: {stderr:?}"
                            );
                            assert!(stderr.ends_with(ERASE), "{label}: {stderr:?}");
                        }
                        "always" => {
                            assert!(
                                stderr.contains(&format!("{counter}\n")),
                                "{label}: {stderr:?}"
                            );
                            assert!(!stderr.contains('\x1b'), "{label}: {stderr:?}");
                        }
                        // The run ends before auto's one-second delay.
                        _ => assert_eq!(stderr, "", "{label}"),
                    }

                    let key = (arguments[0], format, stdout_terminal, term);
                    let stdout = without_volatile_fields(&stdout);
                    let expected = stdout_by_case.entry(key).or_insert_with(|| stdout.clone());
                    assert_eq!(&stdout, expected, "{label}");
                }
            }
        }
    }
}

#[test]
fn never_suppresses_progress_but_not_a_warning() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.conllu");
    fs::write(&corpus, "1\tdone\tdo\tVERB\tVBN\t_\t0\troot\t_\t_\n\n").unwrap();
    let output = lingrep_std()
        .args([
            "matches",
            "--progress",
            "never",
            "--pattern",
            "[unidic.pos1=名詞].",
        ])
        .arg(&corpus)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("warning[attribute_unavailable]: "),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}

#[test]
fn a_closed_stderr_does_not_fail_a_run_that_writes_only_progress() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.conllu");
    fs::write(&corpus, "1\tdone\tdo\tVERB\tVBN\t_\t0\troot\t_\t_\n\n").unwrap();
    for command in ["matches", "summarize"] {
        for format in [None, Some("--json"), Some("--tsv")] {
            let (reader, writer) = std::io::pipe().unwrap();
            drop(reader);
            let output = lingrep_std()
                .args([command, "--progress", "always", "--pattern", "[]."])
                .arg(&corpus)
                .args(format)
                .stderr(writer)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(0), "{command} {format:?}");
            assert!(!output.stdout.is_empty(), "{command} {format:?}");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_build_warning_written_during_transient_status_starts_on_an_erased_line() {
    const ERASE: &str = "\r\x1b[2K";
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.conllu");
    fs::write(&corpus, "1\tdone\tdo\tVERB\tVBN\t_\t0\troot\t_\t_\n\n").unwrap();
    // An empty directory at a canonical artifact path is a sibling artifact
    // that cannot be read.
    fs::create_dir(lingrep_corpus::canonical_artifact_path(
        &directory.path().join("other.conllu"),
        &lingrep_corpus::AnalysisIdentity::from_bytes([0xab; 32]),
        lingrep_corpus::TrustClass::LocallyExecuted,
    ))
    .unwrap();
    let (stdout_master, stdout_slave) = open_pty();
    let (stderr_master, stderr_slave) = open_pty();
    let mut command = lingrep_std();
    command
        .args(["corpus", "build", "--progress", "always"])
        .arg(&corpus)
        .env("TERM", "xterm")
        .env("NO_COLOR", "1")
        .stdout(stdout_slave)
        .stderr(stderr_slave);
    let output = command.output().unwrap();
    drop(command);
    assert!(output.status.success(), "{output:?}");
    drop(read_pty(stdout_master));
    let stderr = read_pty(stderr_master);
    assert!(
        stderr.starts_with(&format!(
            "{ERASE}identity pass: read 1 sources{ERASE}warning: ignoring unreadable artifact at "
        )),
        "{stderr:?}"
    );
}
