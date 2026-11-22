//! `POST /api/pattern/compare`: the occurrences the editor's pattern and the
//! last searched pattern share over the selected corpus, and those only one
//! of them has.
//!
//! The companion `ling` binary runs each pattern with `ling matches` and
//! compares the two streams with `ling diff`, so the page and the command
//! line report one comparison. Each run stops at [`RECORD_LIMIT`] records;
//! `ling diff` then compares the sentences both runs completed and states
//! that bound.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use askama::Template;
use axum::{Form, Router, extract::State, response::Response, routing::post};
use serde::Deserialize;
use serde_json::Value;

use super::live_summary::{bounded_read, program, scope_clause};
use crate::search_scope::SearchScope;
use crate::state::RequestStore;

const RECORD_LIMIT: &str = "20000";
/// Differing occurrences listed per definition and side.
const EXAMPLES: &str = "20";
const DIFF_BYTES: u64 = 4 * 1024 * 1024;
const ERROR_BYTES: u64 = 1024 * 1024;

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/compare", post(compare))
}

#[derive(Deserialize)]
struct Request {
    /// The pattern of the last search.
    a: String,
    /// The editor's pattern.
    b: String,
    corpus: Option<String>,
    level: Option<String>,
    search_scope: Option<String>,
    overlaps: Option<String>,
}

#[derive(Template)]
#[template(path = "fragments/pattern_compare.html")]
struct CompareFragment {
    outcome: Outcome,
}

enum Outcome {
    /// An i18n key and its English text.
    Failed(&'static str, &'static str),
    Compared(Box<Comparison>),
}

struct Comparison {
    level: &'static str,
    scoped: bool,
    /// Sentences scanned, for full coverage.
    sentences_scanned: Option<u64>,
    /// The corpus sentence a run stopped before, for prefix coverage.
    before_corpus_sentence: Option<u64>,
    pairs: Vec<PairView>,
    a_only_definitions: Vec<(String, u64)>,
    b_only_definitions: Vec<(String, u64)>,
    differences_found: bool,
    a_only: u64,
    b_only: u64,
}

struct PairView {
    name: String,
    shared: u64,
    a_only: u64,
    b_only: u64,
    occurrences: Vec<OccurrenceView>,
}

struct OccurrenceView {
    a_only: bool,
    /// The reader address `sentence.start.end`, when the sentence has a
    /// corpus ordinal.
    read: Option<String>,
    surface: String,
    source_name: String,
    unit: String,
}

fn render(outcome: Outcome) -> Response {
    super::render_or_500(CompareFragment { outcome })
}

fn failed(key: &'static str, text: &'static str) -> Response {
    render(Outcome::Failed(key, text))
}

async fn compare(State(store): State<RequestStore>, Form(request): Form<Request>) -> Response {
    let level = match request.level.as_deref() {
        None | Some("span") => "span",
        Some("roles") => "roles",
        Some("record") => "record",
        Some(_) => return failed("diff.failed", "The comparison could not be run."),
    };
    let Some(permit) = store.coordinators.try_acquire_scan_permit() else {
        return failed(
            "diff.busy",
            "Another scan is running. Compare again when it finishes.",
        );
    };
    let (a, b) = (request.a.clone(), request.b.clone());
    let (permit, compiled) = match store
        .coordinators
        .pattern_task(move || {
            let compiles = crate::patterns::compile_web_pattern(&a).is_ok()
                && crate::patterns::compile_web_pattern(&b).is_ok();
            (permit, compiles)
        })
        .await
    {
        Ok(result) => result,
        Err(_) => {
            return failed(
                "diff.busy",
                "Another scan is running. Compare again when it finishes.",
            );
        }
    };
    if !compiled {
        return failed(
            "diff.invalid",
            "Both patterns must compile before they can be compared.",
        );
    }
    let Ok(scope) = SearchScope::parse(request.search_scope.as_deref()) else {
        return failed("diff.failed", "The comparison could not be run.");
    };
    let not_ready = || {
        failed(
            "diff.corpus_not_ready",
            "The corpus is not ready to be searched.",
        )
    };
    let Some(slot) = store.corpora.resolve(request.corpus.as_deref()) else {
        return not_ready();
    };
    slot.runtime.start_or_observe().await;
    let Some(ready) = slot.runtime.ready().await else {
        return not_ready();
    };
    let Ok(clause) = scope_clause(&ready, &scope).await else {
        return failed(
            "diff.scope",
            "The search scope cannot be applied to this comparison.",
        );
    };
    let (Ok(artifact), Ok(program)) = (
        std::fs::canonicalize(ready.artifact.directory()),
        std::env::current_exe().and_then(|path| program(&path)),
    ) else {
        return failed("diff.failed", "The comparison could not be run.");
    };
    let overlaps = crate::templates::overlaps_value(crate::templates::overlaps_of_form(
        request.overlaps.as_deref(),
    ));
    let scoped = clause.is_some();
    let run = async {
        let _permit = permit;
        run(
            &program, &request.a, &request.b, &artifact, clause, overlaps, level,
        )
        .await
    };
    match tokio::time::timeout(Duration::from_secs(120), run).await {
        Ok(Ok(bytes)) => match comparison(&bytes, level, scoped) {
            Some(comparison) => render(Outcome::Compared(Box::new(comparison))),
            None => failed("diff.failed", "The comparison could not be run."),
        },
        Ok(Err(())) => failed("diff.failed", "The comparison could not be run."),
        Err(_) => failed(
            "diff.timeout",
            "The comparison took too long and was stopped.",
        ),
    }
}

/// Runs both patterns and returns the `ling --json diff` stream.
async fn run(
    program: &Path,
    a: &str,
    b: &str,
    artifact: &Path,
    clause: Option<String>,
    overlaps: Option<&str>,
    level: &str,
) -> Result<Vec<u8>, ()> {
    let directory = tempfile::tempdir().map_err(|_| ())?;
    for (name, source) in [("a", a), ("b", b)] {
        std::fs::write(directory.path().join(format!("{name}.lx")), source).map_err(|_| ())?;
        let output = std::fs::File::create(directory.path().join(format!("{name}.jsonl")))
            .map_err(|_| ())?;
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(directory.path())
            .env("TMPDIR", directory.path())
            .args(["--json", "matches", "--patterns"])
            .arg(format!("{name}.lx"))
            .args([
                "--limit",
                RECORD_LIMIT,
                "--workers",
                "8",
                "--max-temp-bytes",
                "1073741824",
                "--corpus",
            ])
            .arg(artifact);
        if let Some(table) = crate::patterns::word_vectors() {
            command.arg("--vectors").arg(table.path());
        }
        if let Some(clause) = &clause {
            command.args(["--where", clause]);
        }
        if let Some(value) = overlaps {
            command.args(["--overlaps", value]);
        }
        let status = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await
            .map_err(|_| ())?;
        if !status.success() {
            return Err(());
        }
    }
    let mut child = tokio::process::Command::new(program)
        .current_dir(directory.path())
        .args([
            "--json",
            "diff",
            "a.jsonl",
            "b.jsonl",
            "--level",
            level,
            "--examples",
            EXAMPLES,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let (stdout, stderr) = (
        child.stdout.take().ok_or(())?,
        child.stderr.take().ok_or(())?,
    );
    let (bytes, _) = tokio::try_join!(
        bounded_read(stdout, DIFF_BYTES),
        bounded_read(stderr, ERROR_BYTES)
    )
    .map_err(|_| ())?;
    if child.wait().await.map_err(|_| ())?.success() {
        Ok(bytes)
    } else {
        Err(())
    }
}

/// Reads a `lingrep.diff/v1` stream into the view.
fn comparison(bytes: &[u8], level: &'static str, scoped: bool) -> Option<Comparison> {
    let mut view = None;
    let mut pairs: Vec<PairView> = Vec::new();
    for line in std::str::from_utf8(bytes).ok()?.lines() {
        let event: Value = serde_json::from_str(line).ok()?;
        if event["schema"] != "lingrep.diff/v1" {
            return None;
        }
        let data = &event["data"];
        match event["type"].as_str()? {
            "run" => {
                if data["level"] != level {
                    return None;
                }
                view = Some((
                    data["coverage"]["sentences_scanned"].as_u64(),
                    data["coverage"]["before_corpus_sentence"].as_u64(),
                ));
            }
            "definition" => pairs.push(PairView {
                name: data["b"].as_str()?.to_owned(),
                shared: data["shared"].as_u64()?,
                a_only: data["a_only"].as_u64()?,
                b_only: data["b_only"].as_u64()?,
                occurrences: Vec::new(),
            }),
            "occurrence" => pairs.last_mut()?.occurrences.push(OccurrenceView {
                a_only: data["status"] == "a_only",
                read: data["corpus_sentence_ordinal"].as_u64().map(|sentence| {
                    format!(
                        "{sentence}.{}.{}",
                        data["span"]["start"], data["span"]["end"]
                    )
                }),
                surface: data["surface"].as_str()?.to_owned(),
                source_name: data["source_name"].as_str()?.to_owned(),
                unit: data["unit"].as_str()?.to_owned(),
            }),
            "complete" => {
                let (sentences_scanned, before_corpus_sentence) = view?;
                let unpaired = |field: &str| {
                    data[field]
                        .as_array()?
                        .iter()
                        .map(|definition| {
                            Some((
                                definition["name"].as_str()?.to_owned(),
                                definition["records"].as_u64()?,
                            ))
                        })
                        .collect::<Option<Vec<_>>>()
                };
                return Some(Comparison {
                    level,
                    scoped,
                    sentences_scanned,
                    before_corpus_sentence,
                    pairs,
                    a_only_definitions: unpaired("a_only_definitions")?,
                    b_only_definitions: unpaired("b_only_definitions")?,
                    differences_found: data["differences_found"].as_bool()?,
                    a_only: data["a_only"].as_u64()?,
                    b_only: data["b_only"].as_u64()?,
                });
            }
            _ => return None,
        }
    }
    None
}
