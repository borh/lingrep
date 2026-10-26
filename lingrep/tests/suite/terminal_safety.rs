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

#[cfg(unix)]
#[test]
fn auto_progress_follows_stderr_terminal_without_changing_stdout() {
    use std::os::fd::FromRawFd;

    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.conllu");
    fs::write(&corpus, "1\tdone\tdo\tVERB\tVBN\t_\t0\troot\t_\t_\n\n").unwrap();
    let mut expected_stdout = None;
    for terminal in [false, true] {
        for mode in ["auto", "always", "never"] {
            let mut command = lingrep_std();
            command
                .args(["matches", "--json", "--pattern", "[].", "--progress", mode])
                .arg(&corpus);
            let mut master = None;
            if terminal {
                let (mut master_fd, mut slave_fd) = (-1, -1);
                assert_eq!(
                    unsafe {
                        libc::openpty(
                            &raw mut master_fd,
                            &raw mut slave_fd,
                            std::ptr::null_mut(),
                            std::ptr::null(),
                            std::ptr::null(),
                        )
                    },
                    0
                );
                master = Some(unsafe { fs::File::from_raw_fd(master_fd) });
                command.stderr(unsafe { fs::File::from_raw_fd(slave_fd) });
            }
            let output = command.output().unwrap();
            assert!(output.status.success());
            let mut stderr = output.stderr;
            if let Some(mut master) = master {
                drop(command);
                let _ = master.read_to_end(&mut stderr);
            }
            let stderr = String::from_utf8(stderr).unwrap();
            assert_eq!(
                stderr.contains("matched 1 sentences"),
                mode == "always" || (mode == "auto" && terminal),
                "{mode} terminal={terminal}: {stderr}"
            );
            let events = parse_jsonl(&output.stdout);
            let semantic_events: Vec<_> = events
                .iter()
                .filter(|event| event["type"] == "match")
                .map(|event| event["data"].clone())
                .collect();
            assert_eq!(semantic_events.len(), 1);
            if let Some(expected) = &expected_stdout {
                assert_eq!(&semantic_events, expected);
            } else {
                expected_stdout = Some(semantic_events);
            }
        }
    }
}
