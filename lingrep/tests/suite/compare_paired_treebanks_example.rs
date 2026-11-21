// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

//! `examples/compare_paired_treebanks.py` over real `ling matches --json
//! --evidence` streams: the recipe in docs/paired-treebank-comparison.md.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

use crate::dependency_evidence::{conllu_sentence, write};
use crate::support::{assert_success, lingrep};

const UD: &str = "(?<w> [text=を & upos=ADP]) \
    where (?<n> [upos in {NOUN, PRON}]) -[case]-> $w, (?<v> [upos=VERB]) -[obj]-> $n.";
const SUD_DIRECT: &str = "(?<w> [text=を & upos=ADP]) \
    where $w -[comp]-> (?<n> [upos in {NOUN, PRON}]), (?<v> [upos=VERB]) -[comp:obj]-> $w.";
const SUD_VIA: &str = "(?<w> [text=を & upos=ADP]) \
    where $w -[comp]-> (?<p> [upos=ADP]), $p -[comp]-> (?<n> [upos in {NOUN, PRON}]), \
    (?<v> [upos=VERB]) -[comp:obj]-> $w.";

type Row<'a> = (&'a str, &'a str, usize, &'a str);

fn sentence(id: &str, rows: &[Row<'_>]) -> String {
    let words: Vec<_> = rows
        .iter()
        .map(|(form, upos, head, rel)| (*form, *form, *upos, "_", *head, *rel))
        .collect();
    conllu_sentence(id, &words)
}

/// Four sentences as UD: an object, an object with a second particle, an
/// object with a nominalizing suffix, and an oblique.
fn ud() -> String {
    [
        sentence(
            "direct",
            &[
                ("坂", "NOUN", 3, "obj"),
                ("を", "ADP", 1, "case"),
                ("上る", "VERB", 0, "root"),
            ],
        ),
        sentence(
            "via",
            &[
                ("何", "PRON", 4, "obj"),
                ("か", "ADP", 1, "case"),
                ("を", "ADP", 1, "case"),
                ("語る", "VERB", 0, "root"),
            ],
        ),
        sentence(
            "suffix",
            &[
                ("凄惨", "NOUN", 4, "obj"),
                ("さ", "PART", 1, "mark"),
                ("を", "ADP", 1, "case"),
                ("感じる", "VERB", 0, "root"),
            ],
        ),
        sentence(
            "oblique",
            &[
                ("道", "NOUN", 3, "obl"),
                ("を", "ADP", 1, "case"),
                ("歩く", "VERB", 0, "root"),
            ],
        ),
    ]
    .concat()
}

/// The same sentences with function words as heads, in another order.
fn sud(particle: &str) -> String {
    [
        sentence(
            "oblique",
            &[
                ("道", "NOUN", 2, "comp"),
                ("を", "ADP", 3, "comp:obj"),
                ("歩く", "VERB", 0, "root"),
            ],
        ),
        sentence(
            "suffix",
            &[
                ("凄惨", "NOUN", 2, "comp"),
                ("さ", "PART", 3, "comp"),
                ("を", "ADP", 4, "comp:obj"),
                ("感じる", "VERB", 0, "root"),
            ],
        ),
        sentence(
            "via",
            &[
                ("何", "PRON", 2, "comp"),
                (particle, "ADP", 3, "comp"),
                ("を", "ADP", 4, "comp:obj"),
                ("語る", "VERB", 0, "root"),
            ],
        ),
        sentence(
            "direct",
            &[
                ("坂", "NOUN", 2, "comp"),
                ("を", "ADP", 3, "comp:obj"),
                ("上る", "VERB", 0, "root"),
            ],
        ),
    ]
    .concat()
}

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        write(directory.path(), "ud.conllu", &ud());
        write(directory.path(), "sud.conllu", &sud("か"));
        Self { directory }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn matches(&self, name: &str, pattern: &str, conllu: &str, limit: &str) -> PathBuf {
        let output = lingrep()
            .args(["--json", "matches", "--evidence", "--limit", limit])
            .args(["--pattern", pattern])
            .arg(self.path(conllu))
            .output()
            .unwrap();
        assert_success(&output);
        write(
            self.directory.path(),
            name,
            std::str::from_utf8(&output.stdout).unwrap(),
        )
    }

    fn compare(&self, a_conllu: &str, b_conllu: &str, a: &Path, b: &[&Path]) -> Output {
        let mut command = Command::new("python3");
        command
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join("examples/compare_paired_treebanks.py"),
            )
            .arg("--a-conllu")
            .arg(self.path(a_conllu))
            .arg("--b-conllu")
            .arg(self.path(b_conllu))
            .arg("--a")
            .arg(a)
            .args(["--roles", "n,w,v"]);
        for stream in b {
            command.arg("--b").arg(stream);
        }
        command.output().unwrap()
    }
}

fn totals(output: &Output) -> String {
    assert_success(output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.lines().last().unwrap().to_owned()
}

fn rejected(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "{stderr}");
}

#[test]
fn paired_comparison_aligns_by_sent_id_and_word_id_and_pools_b_runs() {
    let fixture = Fixture::new();
    let ud = fixture.matches("ud.jsonl", UD, "ud.conllu", "all");
    let direct = fixture.matches("direct.jsonl", SUD_DIRECT, "sud.conllu", "all");
    let via = fixture.matches("via.jsonl", SUD_VIA, "sud.conllu", "all");

    let output = fixture.compare("ud.conllu", "sud.conllu", &ud, &[&direct, &via]);
    assert_eq!(totals(&output), "shared 2, a_only 1, b_only 1");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "status\tcount_a\tcount_b\tsent_id\tunit\tanalysis_a\tanalysis_b\tsentence\n\
         shared\t1\t1\tdirect\tn=1 w=2 v=3\t\
         n=坂(1,obj→3) w=を(2,case→1) v=上る(3,root→0)\t\
         n=坂(1,comp→2) w=を(2,comp:obj→3) v=上る(3,root→0)\t坂を上る\n\
         shared\t1\t1\tvia\tn=1 w=3 v=4\t\
         n=何(1,obj→4) w=を(3,case→1) v=語る(4,root→0)\t\
         n=何(1,comp→2) p=か(2,comp→3) w=を(3,comp:obj→4) v=語る(4,root→0)\t何かを語る\n\
         a_only\t1\t0\tsuffix\tn=1 w=3 v=4\t\
         n=凄惨(1,obj→4) w=を(3,case→1) v=感じる(4,root→0)\t\
         n=凄惨(1,comp→2) w=を(3,comp:obj→4) v=感じる(4,root→0)\t凄惨さを感じる\n\
         b_only\t0\t1\toblique\tn=1 w=2 v=3\t\
         n=道(1,obl→3) w=を(2,case→1) v=歩く(3,root→0)\t\
         n=道(1,comp→2) w=を(2,comp:obj→3) v=歩く(3,root→0)\t道を歩く\n"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("A: 3 records (ud.jsonl 3), source_identity "),
        "{stderr}"
    );
    assert!(
        stderr.contains("B: 3 records (direct.jsonl 2, via.jsonl 1), source_identity "),
        "{stderr}"
    );

    let unchanged = fixture.compare("ud.conllu", "ud.conllu", &ud, &[&ud]);
    assert_eq!(totals(&unchanged), "shared 3, a_only 0, b_only 0");
}

#[test]
fn paired_comparison_rejects_unaligned_files_and_unusable_runs() {
    let fixture = Fixture::new();
    let ud = fixture.matches("ud.jsonl", UD, "ud.conllu", "all");
    let direct = fixture.matches("direct.jsonl", SUD_DIRECT, "sud.conllu", "all");
    let rejects = |b_conllu: &str, content: &str, message: &str| {
        write(fixture.directory.path(), b_conllu, content);
        let stream = fixture.matches("b.jsonl", SUD_DIRECT, b_conllu, "all");
        rejected(
            &fixture.compare("ud.conllu", b_conllu, &ud, &[&stream]),
            message,
        );
    };

    rejects(
        "token.conllu",
        &sud("かも"),
        "1 sentences differ in word ID, FORM or UPOS, first via",
    );
    let one_fewer = sud("か");
    let (_, without_oblique) = one_fewer.split_once("\n\n").unwrap();
    rejects(
        "missing.conllu",
        without_oblique,
        "1 sent_id values are in only one file, first oblique",
    );
    rejects(
        "duplicate.conllu",
        &[sud("か"), sentence("direct", &[("坂", "NOUN", 0, "root")])].concat(),
        "B: sent_id direct occurs more than once",
    );

    let limited = fixture.matches("limited.jsonl", SUD_DIRECT, "sud.conllu", "1");
    rejected(
        &fixture.compare("ud.conllu", "sud.conllu", &ud, &[&limited]),
        "B limited.jsonl: the run stopped early",
    );
    rejected(
        &fixture.compare("sud.conllu", "sud.conllu", &ud, &[&direct]),
        "A ud.jsonl: record 0 is not from",
    );
}
