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
//! `matches` publishes post-policy records by default, and raw on request.
//!
//! Policy applies by default on every execution surface, while the raw
//! canonical stream remains requestable.
//! What is asserted is complete record identity — pattern *and* span — because
//! `output_name` changes a record's identity while leaving the record count
//! untouched, so a count assertion here would pass while publishing the wrong
//! names.

use std::path::PathBuf;
use std::process::Output;

use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};
use tempfile::TempDir;

use crate::support::{build_single_sentence_artifact, lingrep_without_dictionary, parse_jsonl};

const CROSS_SURFACE_CATALOG: &str = r#"
fragment COPULA := [lemma=だ].

output_name: "published"
trimmed := [text=研究] [lemma=だ].

final_only := [text=研究].

plain := [text=研究].

policy
  detach @trimmed with: @COPULA, relation: at_end, evidence: raw.
  admit @final_only terminal: {exact: true, anchor: sentence_end}.
end
"#;

struct PreparedPolicyFixture {
    _directory: TempDir,
    patterns: PathBuf,
    artifact: PathBuf,
}

impl PreparedPolicyFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("policy fixture directory");
        let patterns = directory.path().join("policy.lx");
        let artifact = directory.path().join("policy.lingcorpus");
        std::fs::write(&patterns, CROSS_SURFACE_CATALOG).expect("write policy catalog");

        let mut research = TokenInfo::default();
        research.set(TokenAttributeIndex::Orth, "研究".into());
        research.set(TokenAttributeIndex::Lemma, "研究".into());
        let mut copula = TokenInfo::default();
        copula.set(TokenAttributeIndex::Orth, "だ".into());
        copula.set(TokenAttributeIndex::Lemma, "だ".into());
        let mut punctuation = TokenInfo::default();
        punctuation.set(TokenAttributeIndex::Orth, "。".into());
        punctuation.set(TokenAttributeIndex::Lemma, "。".into());

        tokio::runtime::Runtime::new()
            .expect("policy fixture runtime")
            .block_on(build_single_sentence_artifact(
                &artifact,
                "研究だ。",
                &[research, copula, punctuation],
            ));

        Self {
            _directory: directory,
            patterns,
            artifact,
        }
    }

    fn run(&self, subcommand: &[&str]) -> Output {
        let mut command = lingrep_without_dictionary();
        command.args(subcommand);
        if !subcommand.contains(&"--tsv") {
            command.arg("--json");
        }
        command.arg("--patterns").arg(&self.patterns);
        command
            .arg("--corpus")
            .arg(&self.artifact)
            .output()
            .expect("ling runs")
    }
}

fn assert_success(output: &Output, surface: &str) {
    assert!(
        output.status.success(),
        "{surface} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn match_records(output: &Output) -> Vec<(String, u64, u64)> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|record| record["type"] == "match")
        .map(|record| {
            let data = &record["data"];
            (
                data["pattern"].as_str().expect("pattern").to_owned(),
                data["span"]["start"].as_u64().expect("start"),
                data["span"]["end"].as_u64().expect("end"),
            )
        })
        .collect()
}

fn summary_names(output: &Output) -> Vec<String> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|record| record["type"] == "summary")
        .map(|record| {
            record["data"]["pattern"]
                .as_str()
                .expect("summary pattern")
                .to_owned()
        })
        .collect()
}

#[test]
fn publication_surfaces_share_one_policy_projection() {
    let fixture = PreparedPolicyFixture::new();

    let matches = fixture.run(&["matches"]);
    assert_success(&matches, "matches");
    let mut published = match_records(&matches);
    published.sort();
    assert_eq!(
        published,
        [("plain".to_owned(), 0, 1), ("published".to_owned(), 0, 1),]
    );

    let raw = fixture.run(&["matches", "--raw-matches"]);
    assert_success(&raw, "matches --raw-matches");
    let mut raw_records = match_records(&raw);
    raw_records.sort();
    assert_eq!(
        raw_records,
        [
            ("final_only".to_owned(), 0, 1),
            ("plain".to_owned(), 0, 1),
            ("trimmed".to_owned(), 0, 2),
        ],
        "raw output keeps authored identity and span while stripping synthesized evidence"
    );

    let summarize = fixture.run(&["summarize"]);
    assert_success(&summarize, "summarize");
    let mut summarized = summary_names(&summarize);
    summarized.sort();
    assert_eq!(summarized, ["plain", "published"]);

    let sentence_final = fixture.run(&["sentence-final"]);
    assert_success(&sentence_final, "sentence-final");
    assert!(
        !sentence_final
            .stdout
            .windows(b"__policy__".len())
            .any(|window| window == b"__policy__"),
        "synthesized evidence reached sentence-final: {}",
        String::from_utf8_lossy(&sentence_final.stdout)
    );
}

#[test]
fn a_misspelled_policy_key_warns_through_check_without_failing() {
    // `output_namme` is inert because it is not a recognized key,
    // and inert-but-plausible is invisible by reading, which is the whole
    // reason the diagnostic exists. It must reach the researcher through the
    // command they already run, and it must not fail the catalog: the pattern
    // is well-formed and matches correctly.
    let directory = tempfile::tempdir().expect("temp dir");
    let catalog_path = directory.path().join("typo.lx");
    std::fs::write(
        &catalog_path,
        "output_namme: \"x\"\ntypo := [text=\u{7814}].\n",
    )
    .expect("write catalog");

    let output = lingrep_without_dictionary()
        .arg("check")
        .arg("--json")
        .arg("--patterns")
        .arg(&catalog_path)
        .output()
        .expect("ling runs");
    assert!(
        output.status.success(),
        "a suspected typo must not fail check: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = parse_jsonl(&output.stdout);
    let warnings = records
        .iter()
        .find(|record| record["type"] == "check")
        .map(|record| record["data"]["warnings"].clone())
        .expect("check emits a result record");
    let warnings = warnings.as_array().expect("warnings is an array");
    let typo = warnings
        .iter()
        .find(|warning| warning["code"] == "SuspiciousPolicyKey")
        .unwrap_or_else(|| panic!("no SuspiciousPolicyKey warning in {warnings:#?}"));
    let message = typo["message"].as_str().expect("message");
    assert!(
        message.contains("output_namme") && message.contains("output_name"),
        "the warning must name both the typo and the suggestion: {message}"
    );
}

/// A transform's unit, key, and tags reach JSONL, and
/// the attributes they read are cached even though no predicate names them.
///
/// Deliberately over a *prepared* artifact. A prepared run projects only the
/// attributes the run declares it needs, and that set used to be derived from
/// the compiled automaton alone — so a catalog matching on `orth` while keying
/// on `lemma`, guarding on `pos`, and rendering `text` published correct
/// records from a text corpus and empty ones from an artifact, with no error
/// either way.
const DERIVING_CATALOG: &str = r#"
key: [$noun.lemma]
transform: [{op: "drop_edge", edge: "trailing", drop: "[upos=AUX]"}, {op: "join"}]
tags: {kind: "nominal"}
pattern derived := (?<noun> [text=研究]) (?<cop> [text=だ]).
"#;

struct DerivingFixture {
    _directory: TempDir,
    patterns: PathBuf,
    artifact: PathBuf,
}

/// One statement that acts and one that cannot, over the same sentence.
///
/// The `detach` narrows `derived` off the trailing AUX; the `resolve` sees a
/// single candidate, so a contest it wins changes nothing. Both are needed:
/// the first proves a statement's own numbers are reported, the second that a
/// statement reports at all when its numbers are zero.
///
/// Separate from [`DERIVING_CATALOG`] rather than bolted onto it. A statement
/// that acts changes published records by construction, and that catalog's
/// other tests assert on the records it publishes today.
const STATEMENT_CATALOG: &str = r#"
fragment COP := [upos=AUX].
pattern derived := [text=研究] [text=だ].
policy
  resolve all rank: [longest, start, end, pattern].
  detach @derived with: @COP, relation: at_end, evidence: raw.
end
"#;

impl DerivingFixture {
    fn new() -> Self {
        Self::with_catalog(DERIVING_CATALOG)
    }

    fn with_catalog(catalog: &str) -> Self {
        let directory = tempfile::tempdir().expect("deriving fixture directory");
        let patterns = directory.path().join("deriving.lx");
        let artifact = directory.path().join("deriving.lingcorpus");
        std::fs::write(&patterns, catalog).expect("write deriving catalog");

        let token = |orth: &str, lemma: &str, pos: &str| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Orth, orth.into());
            token.set(TokenAttributeIndex::Text, orth.into());
            token.set(TokenAttributeIndex::Lemma, lemma.into());
            token.set(TokenAttributeIndex::Pos, pos.into());
            token
        };

        tokio::runtime::Runtime::new()
            .expect("deriving fixture runtime")
            .block_on(build_single_sentence_artifact(
                &artifact,
                "研究だ。",
                &[
                    token("研究", "研究", "NOUN"),
                    token("だ", "だ", "AUX"),
                    token("。", "。", "PUNCT"),
                ],
            ));

        Self {
            _directory: directory,
            patterns,
            artifact,
        }
    }

    fn run(&self, subcommand: &[&str]) -> Output {
        let mut command = lingrep_without_dictionary();
        command.args(subcommand);
        if !subcommand.contains(&"--tsv") {
            command.arg("--json");
        }
        command.arg("--patterns").arg(&self.patterns);
        command
            .arg("--corpus")
            .arg(&self.artifact)
            .output()
            .expect("ling runs")
    }
}

#[test]
fn a_transformed_record_publishes_its_unit_key_and_tags() {
    let fixture = DerivingFixture::new();

    let output = fixture.run(&["matches"]);
    assert_success(&output, "matches");
    let records = parse_jsonl(&output.stdout);
    let matched = records
        .iter()
        .find(|record| record["type"] == "match")
        .unwrap_or_else(|| panic!("no match record in {records:#?}"));
    let data = &matched["data"];

    assert_eq!(
        (data["span"]["start"].as_u64(), data["span"]["end"].as_u64()),
        (Some(0), Some(1)),
        "drop_edge removed the trailing AUX from the published span"
    );
    assert_eq!(data["unit"], "研究", "join rendered the shortened span");
    assert_eq!(
        data["key"],
        serde_json::json!([{
            "kind": "scalar",
            "name": "$noun.lemma",
            "value": "研究"
        }])
    );
    assert_eq!(
        data["tags"],
        serde_json::json!([{"name": "kind", "value": "nominal"}])
    );
    // The capture survived the rewrite, so it is still published — and its span
    // is the post-rewrite one, which here equals the raw one.
    assert_eq!(
        data["captures"],
        serde_json::json!([{
            "name": "noun",
            "hygienic_path": "derived/noun",
            "span": {"start": 0, "end": 1}
        }])
    );

    let complete = records
        .iter()
        .find(|record| record["type"] == "complete")
        .unwrap_or_else(|| panic!("no complete record in {records:#?}"));
    assert_eq!(
        complete["data"]["counters"]["records_unkeyed"],
        serde_json::Value::Null,
        "every record was keyable, so the counter stays absent"
    );
}

#[test]
fn the_raw_stream_carries_no_derived_fields_at_all() {
    // `--raw-matches` requests raw spans and captures, so it must publish
    // neither transform output nor
    // fields whose absence would otherwise read as "the catalog declared none".
    let fixture = DerivingFixture::new();

    let output = fixture.run(&["matches", "--raw-matches"]);
    assert_success(&output, "matches --raw-matches");
    let records = parse_jsonl(&output.stdout);
    let matched = records
        .iter()
        .find(|record| record["type"] == "match")
        .unwrap_or_else(|| panic!("no match record in {records:#?}"));
    let data = &matched["data"];

    assert_eq!(
        (data["span"]["start"].as_u64(), data["span"]["end"].as_u64()),
        (Some(0), Some(2)),
        "the raw span keeps the token the transform would have dropped"
    );
    for absent in ["unit", "key", "tags"] {
        assert_eq!(
            data[absent],
            serde_json::Value::Null,
            "{absent} must be absent from the raw stream"
        );
    }
}

/// A guard attribute no predicate mentions is still cached for a prepared run.
///
/// The eager prepared path reconstructs exactly the attributes the run asked
/// for, and that set came from the compiled automaton alone. A `drop_edge`
/// guard on `pos` over a catalog matching on `orth` therefore saw tokens with
/// no `pos` at all — and a token lacking an attribute satisfies neither
/// polarity of a predicate, so the guard silently admitted nothing and the
/// transform became a no-op. No error, no counter, just an untransformed span.
///
/// TSV rather than JSONL because the JSONL path defers completion and fills
/// every attribute of the matched span before publication, which hides the
/// defect on exactly the format that would otherwise show it.
#[test]
fn a_prepared_run_caches_the_attributes_only_the_policy_reads() {
    let fixture = DerivingFixture::new();

    let output = fixture.run(&["matches", "--tsv"]);
    assert_success(&output, "matches --tsv");
    let row = String::from_utf8(output.stdout).expect("tsv is utf-8");
    let fields: Vec<&str> = row.trim_end().split('\t').collect();
    assert_eq!(
        (fields[6], fields[7]),
        ("0", "1"),
        "the trailing AUX must be dropped; an unprojected `pos` would leave 0..2 in {row:?}"
    );
    assert_eq!(
        fields[9], "研究",
        "and the published surface follows the transformed span"
    );
}

/// The run reports what it knows.
///
/// It no longer counts suppressions by category — nothing suppresses by
/// category any more — so it no longer prints three zeroes claiming it does.
/// The per-statement replacement now reaches this same channel, one line per
/// authored statement; this catalog declares no `policy` block, so the
/// capture figure is the whole of what it has to say.
#[test]
fn the_policy_progress_line_reports_only_dropped_captures() {
    let fixture = DerivingFixture::new();

    // `cop` covers the trailing AUX, which the `drop_edge` transform removes —
    // so the capture leaves the published span and is dropped. A `detach`
    // would not serve: it counts into `ProgramStats` per statement, and
    // `PolicyStats::captures_dropped` is the transforms' counter.
    let output = fixture.run(&["matches", "--progress", "always"]);
    assert_success(&output, "matches --progress always");
    let progress = String::from_utf8_lossy(&output.stderr);

    assert!(
        progress.contains("policy dropped 1 capture"),
        "the policy line must report the dropped capture: {progress}"
    );
    assert!(
        !progress.contains("suppressed"),
        "no category of suppression is counted any more: {progress}"
    );
}

/// Each statement reports what it did, named by its own source text.
///
/// A researcher asking "did my `detach` do anything" is asking about one
/// statement, and a per-run total cannot answer it. The label comes from the
/// catalog rather than from this formatter, so a statement the author rewords
/// reports under the new text without this command being touched.
#[test]
fn the_progress_output_names_each_statement_and_what_it_did() {
    let fixture = DerivingFixture::with_catalog(STATEMENT_CATALOG);

    let output = fixture.run(&["matches", "--progress", "always"]);
    assert_success(&output, "matches --progress always");
    let progress = String::from_utf8_lossy(&output.stderr);

    assert!(
        progress.contains("detach @derived with: @COP, relation: at_end, evidence: raw."),
        "the statement's own text: {progress}"
    );
    assert!(
        progress.contains("narrowed 1"),
        "what the detach did: {progress}"
    );
}

/// A statement that never fired still reports its line.
///
/// "Ran and did nothing" and "was never reached" are the same observation only
/// if the line is missing, and the first is the answer a researcher debugging
/// an inert statement needs. `ProgramStats::for_program` sizes from the program
/// for exactly this reason, and a formatter that filtered on activity would
/// undo it.
#[test]
fn an_inert_statement_still_reports_its_line() {
    let fixture = DerivingFixture::with_catalog(STATEMENT_CATALOG);

    let output = fixture.run(&["matches", "--progress", "always"]);
    assert_success(&output, "matches --progress always");
    let progress = String::from_utf8_lossy(&output.stderr);

    // The `resolve` contests a field of one, so it can drop nothing.
    assert!(
        progress.contains("resolve all rank: [longest, start, end, pattern]."),
        "the inert statement is still named: {progress}"
    );
    assert!(
        progress.contains("dropped 0"),
        "and reports zeroes rather than vanishing: {progress}"
    );
}

/// Two records from one sentence, so a capacity of one leaves something out.
///
/// [`STATEMENT_CATALOG`] matches once, and a trace that collected everything it
/// was offered can never reach its bound.
const TWO_RECORD_CATALOG: &str = r#"
fragment COP := [upos=AUX].
pattern derived := [text=研究] [text=だ].
pattern noun := [text=研究].
policy
  detach @derived with: @COP, relation: at_end, evidence: raw.
end
"#;

/// The trace reaches the progress channel, and the record stream is untouched.
///
/// A JSONL or TSV consumer's contract is the record. Putting an explanation
/// into it would change a published contract to serve a debugging affordance,
/// and every downstream reader would have to learn to skip it. Both halves are
/// asserted: the explanation appears on stderr, and stdout is byte-for-byte
/// what the same run publishes without the flag.
///
/// Run without `--progress always`, deliberately. The per-statement counters
/// are unrequested run facts and stay behind that flag; `--trace-policy` was
/// typed out loud, and a flag that answered nothing unless a second flag agreed
/// would be a flag that looks broken.
#[test]
fn the_trace_goes_to_progress_and_never_to_the_record_stream() {
    let fixture = DerivingFixture::with_catalog(STATEMENT_CATALOG);

    let traced = fixture.run(&["matches", "--trace-policy", "10"]);
    assert_success(&traced, "matches --trace-policy 10");
    let progress = String::from_utf8_lossy(&traced.stderr);

    assert!(
        progress.contains("policy trace (1 record)"),
        "the trace must reach the progress channel: {progress}"
    );
    assert!(
        progress.contains("detach @derived with: @COP, relation: at_end, evidence: raw."),
        "named by the statement's own text, as the counters are: {progress}"
    );
    assert!(
        progress.contains("→ 0..1"),
        "and reporting the span the detach left: {progress}"
    );

    let untraced = fixture.run(&["matches"]);
    assert_success(&untraced, "matches");
    // The match records only. A JSONL run also publishes a `run` and a
    // `complete` record, and those carry a fresh `run_id` and an elapsed time,
    // so comparing whole streams would compare the clock.
    let published = |output: &Output| {
        parse_jsonl(&output.stdout)
            .into_iter()
            .filter(|record| record["type"] == "match")
            .collect::<Vec<_>>()
    };
    assert_eq!(
        published(&traced),
        published(&untraced),
        "explaining a run must not change what it publishes"
    );
    assert!(
        !published(&traced).is_empty(),
        "the comparison must have something to compare"
    );
}

/// A statement too long for one line is elided at display, never at storage.
///
/// The definition name is padded to push the statement past the display width.
/// What it matches is irrelevant — the statement selects it and that is enough
/// to put its label on a trace line.
///
/// The fixture exceeds the display width by enough that a small width change
/// cannot accidentally turn this into a boundary test.
const LONG_STATEMENT_CATALOG: &str = r#"
fragment COP := [upos=AUX].
pattern derived_with_a_deliberately_long_authored_name := [text=研究] [text=だ].
policy
  detach @derived_with_a_deliberately_long_authored_name with: @COP, relation: at_end, evidence: raw.
end
"#;

/// A long statement is shortened on the line, and only on the line.
///
/// The stored label is what a compile diagnostic quotes back to the author, so
/// shortening it there would send them looking for text their file does not
/// contain. Shortening it here keeps a trace readable at a terminal width.
#[test]
fn a_long_statement_is_elided_on_its_trace_line() {
    let fixture = DerivingFixture::with_catalog(LONG_STATEMENT_CATALOG);

    let output = fixture.run(&["matches", "--trace-policy", "10"]);
    assert_success(&output, "matches --trace-policy 10");
    let progress = String::from_utf8_lossy(&output.stderr);

    assert!(
        progress.contains(
            "detach @derived_with_a_deliberately_long_authored_name with: @COP, relation: at_end, evi…"
        ),
        "the line carries a prefix and an ellipsis: {progress}"
    );
    assert!(
        !progress.contains("evidence: raw."),
        "and not the whole statement: {progress}"
    );
}

/// Truncation is reported, not implied.
///
/// A trace that stopped collecting and did not say so reads as a complete
/// account of the run — the silent cap this workspace refuses everywhere else.
#[test]
fn a_truncated_trace_says_so() {
    let fixture = DerivingFixture::with_catalog(TWO_RECORD_CATALOG);

    let output = fixture.run(&["matches", "--trace-policy", "1"]);
    assert_success(&output, "matches --trace-policy 1");
    let progress = String::from_utf8_lossy(&output.stderr);

    assert!(
        progress.contains("policy trace truncated at 1 records"),
        "the cap was reached and must be reported: {progress}"
    );

    // The same catalog under a capacity that fits must not claim truncation,
    // or the assertion above would pass for a trace that always says so.
    let complete = fixture.run(&["matches", "--trace-policy", "10"]);
    assert_success(&complete, "matches --trace-policy 10");
    let progress = String::from_utf8_lossy(&complete.stderr);
    assert!(
        progress.contains("policy trace (2 records)"),
        "both records are explained: {progress}"
    );
    assert!(
        !progress.contains("truncated"),
        "and nothing was left out: {progress}"
    );
}

/// Every command that publishes through the block can say what the block did.
///
/// `matches` could from the start; `summarize` and `sentence-final` could not,
/// and they are the two that need it most. Both aggregate — into per-pattern
/// counts and into pattern × surface counts — so a total that came out lower
/// than expected has no visible cause in the output itself. The reasoning that
/// deferred them (`matches` publishes records one at a time, which is the
/// granularity a per-record trace reports at) turned out to be an argument
/// about where the trace is *easiest* to read, not where it is needed.
///
/// Driven over all three surfaces from one table rather than three near-copies,
/// because the claim is about the set: a fourth publishing command added
/// without a report is the failure this is guarding, and three separate tests
/// would each pass while it happened.
#[test]
fn every_publishing_surface_reports_what_policy_did() {
    let fixture = DerivingFixture::with_catalog(STATEMENT_CATALOG);

    for surface in ["matches", "summarize", "sentence-final"] {
        let counters = fixture.run(&[surface, "--progress", "always"]);
        assert_success(&counters, &format!("{surface} --progress always"));
        let progress = String::from_utf8_lossy(&counters.stderr);
        assert!(
            progress.contains("detach @derived with: @COP, relation: at_end, evidence: raw."),
            "{surface} names the statement by its own source text: {progress}"
        );
        assert!(
            progress.contains("resolve all rank: [longest, start, end, pattern]."),
            "{surface} names the inert statement too: {progress}"
        );

        // And the per-record explanation, which is a separate flag on each.
        let traced = fixture.run(&[surface, "--trace-policy", "10"]);
        assert_success(&traced, &format!("{surface} --trace-policy 10"));
        let progress = String::from_utf8_lossy(&traced.stderr);
        assert!(
            progress.contains("policy trace ("),
            "{surface} explains individual records: {progress}"
        );
        assert!(
            progress.contains("→ not selected") || progress.contains("→ kept"),
            "{surface} reports per-statement outcomes: {progress}"
        );
    }
}

/// The trace is printed without `--progress always`, on every surface.
///
/// The per-statement counters are unrequested run facts and stay behind that
/// flag. `--trace-policy` was typed out loud, and a flag that answered nothing
/// unless a second flag agreed would be a flag that looks broken. Asserted per
/// surface because each wires its own progress gate, and `summarize` and
/// `sentence-final` gate their counters on `ProgressMode::Always` a few lines
/// from where they print the trace unconditionally.
#[test]
fn the_trace_does_not_wait_for_progress_always() {
    let fixture = DerivingFixture::with_catalog(STATEMENT_CATALOG);

    for surface in ["matches", "summarize", "sentence-final"] {
        let output = fixture.run(&[surface, "--trace-policy", "10"]);
        assert_success(&output, &format!("{surface} --trace-policy 10"));
        let progress = String::from_utf8_lossy(&output.stderr);
        assert!(
            progress.contains("policy trace ("),
            "{surface} traces without --progress always: {progress}"
        );
        // The counters are the half that must stay quiet, and they are
        // recognizable by the phrasing only they use.
        assert!(
            !progress.contains("→ dropped 0, narrowed"),
            "{surface} must not print unrequested run facts: {progress}"
        );
    }
}
