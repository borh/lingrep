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

use std::fs;

use tempfile::TempDir;

use crate::support::{assert_success, lingrep};

/// A UD v2 parse: `obj`, `nsubj:pass` and `aux:pass` are outside ClearNLP.
const UD_PARSE: &str = "\
1\tShe\tshe\tPRON\tPRP\t_\t2\tnsubj\t_\t_
2\treads\tread\tVERB\tVBZ\t_\t0\troot\t_\t_
3\tbooks\tbook\tNOUN\tNNS\t_\t2\tobj\t_\t_

1\tIt\tit\tPRON\tPRP\t_\t3\tnsubj:pass\t_\t_
2\twas\tbe\tAUX\tVBD\t_\t3\taux:pass\t_\t_
3\tread\tread\tVERB\tVBN\t_\t0\troot\t_\t_
4\tthen\tthen\tADV\tRB\t_\t3\tobj\t_\t_

";

#[test]
fn a_run_reports_corpus_values_outside_the_declared_label_set() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("ud.conllu");
    fs::write(&corpus, UD_PARSE).unwrap();
    let artifact = directory.path().join("ud.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--output"])
        .arg(&artifact)
        .arg(&corpus)
        .output()
        .unwrap();
    assert_success(&built);
    let clearnlp = directory.path().join("clearnlp.lx");
    fs::write(
        &clearnlp,
        "labels: { dep.rel: \"clearnlp\", upos: \"ud-v2\" }\n\
         object := (?<v> [upos=VERB]) (?<o> []) where $v -[dobj]-> $o.\n",
    )
    .unwrap();
    let union = directory.path().join("union.lx");
    fs::write(
        &union,
        "labels: { dep.rel: [\"clearnlp\", \"ud-v2\"] }\n\
         object := (?<v> [upos=VERB]) (?<o> []) where $v -[dobj | obj]-> $o.\n",
    )
    .unwrap();

    for command in ["matches", "summarize"] {
        for prepared in [false, true] {
            for (catalog, expected) in [
                (
                    &clearnlp,
                    Some(
                        "warning[label_outside_declared_set]: the corpus has `dep.rel` values \
                         outside the label set `clearnlp` declared at",
                    ),
                ),
                (&union, None),
            ] {
                let mut run = lingrep();
                run.args([command, "--patterns"]).arg(catalog);
                if prepared {
                    run.arg("--corpus").arg(&artifact);
                } else {
                    run.arg(&corpus);
                }
                let output = run.output().unwrap();
                let case = format!("{command} prepared={prepared} {}", catalog.display());
                assert_success(&output);
                let stderr = String::from_utf8(output.stderr).unwrap();
                let warnings: Vec<&str> = stderr
                    .lines()
                    .filter(|line| line.starts_with("warning[label_outside_declared_set]"))
                    .collect();
                match expected {
                    Some(prefix) => {
                        assert_eq!(warnings.len(), 1, "{case}: {stderr}");
                        assert!(warnings[0].starts_with(prefix), "{case}: {stderr}");
                        assert!(
                            warnings[0].ends_with(
                                ": `obj` (2), `aux:pass` (1), `nsubj:pass` (1); the catalog's \
                                 literals may not match this corpus's annotation scheme"
                            ),
                            "{case}: {stderr}"
                        );
                    }
                    None => assert!(warnings.is_empty(), "{case}: {stderr}"),
                }
            }
        }
    }
}

#[test]
fn out_of_set_counts_cover_untested_attributes_and_only_the_sentences_read() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("ud.conllu");
    let (first, second) = UD_PARSE.trim_end().split_once("\n\n").unwrap();
    let mut parse = format!("{first}\n\n");
    for _ in 0..3000 {
        parse.push_str(second);
        parse.push_str("\n\n");
    }
    fs::write(&corpus, parse).unwrap();
    let artifact = directory.path().join("ud.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--output"])
        .arg(&artifact)
        .arg(&corpus)
        .output()
        .unwrap();
    assert_success(&built);
    let catalog = directory.path().join("book.lx");
    fs::write(
        &catalog,
        "labels: { lemma: {book, read}, upos: {VERB} }\np := [lemma=book].\n",
    )
    .unwrap();

    for command in ["matches", "summarize"] {
        for prepared in [false, true] {
            let mut run = lingrep();
            run.args([command, "--patterns"]).arg(&catalog);
            if prepared {
                run.arg("--corpus").arg(&artifact);
            } else {
                run.arg(&corpus);
            }
            let output = run.output().unwrap();
            let case = format!("{command} prepared={prepared}");
            assert_success(&output);
            let stderr = String::from_utf8(output.stderr).unwrap();
            let counts: Vec<&str> = stderr
                .lines()
                .filter(|line| line.starts_with("warning[label_outside_declared_set]"))
                .map(|line| {
                    let (_, values) = line.rsplit_once(".lx:1: ").unwrap();
                    values.split_once(';').unwrap().0
                })
                .collect();
            // `summarize --corpus` reads only the sentence that the posting
            // index selects for `lemma=book`.
            let expected = if command == "summarize" && prepared {
                ["`she` (1)", "`NOUN` (1), `PRON` (1)"]
            } else {
                [
                    "`be` (3000), `it` (3000), `then` (3000), `she` (1)",
                    "`PRON` (3001), `ADV` (3000), `AUX` (3000), `NOUN` (1)",
                ]
            };
            assert_eq!(counts, expected, "{case}: {stderr}");
        }
    }
}
