use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::search_scope::SearchScope;
use crate::state::RequestStore;

const OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const ERROR_BYTES: u64 = 1024 * 1024;

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/summary", post(summary))
}

#[derive(Deserialize)]
struct Request {
    source: String,
    definition: String,
    corpus: Option<String>,
    search_scope: Option<String>,
}

fn failure(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({"error": code}))).into_response()
}

async fn summary(State(store): State<RequestStore>, Json(request): Json<Request>) -> Response {
    let Some(permit) = store.coordinators.try_acquire_scan_permit() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "summary_busy");
    };
    let Ok(compiled) = crate::patterns::compile_web_pattern(&request.source) else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source");
    };
    if !compiled
        .capture_key_edits
        .iter()
        .any(|edit| edit.definition == request.definition)
    {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "unsupported_key");
    }
    let Ok(scope) = SearchScope::parse(request.search_scope.as_deref()) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_scope");
    };
    let Some(slot) = store.corpora.resolve(request.corpus.as_deref()) else {
        return failure(StatusCode::NOT_FOUND, "unknown_corpus");
    };
    slot.runtime.start_or_observe().await;
    let Some(ready) = slot.runtime.ready().await else {
        return failure(StatusCode::CONFLICT, "corpus_not_ready");
    };
    let clause = if scope.is_unrestricted() {
        None
    } else {
        let Ok(provenance) = ready.provenance().await else {
            return failure(StatusCode::CONFLICT, "corpus_unavailable");
        };
        let Ok(labels) = provenance
            .section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
            .await
        else {
            return failure(StatusCode::CONFLICT, "corpus_unavailable");
        };
        let all = labels
            .into_iter()
            .map(|label| label.label)
            .collect::<Vec<_>>();
        match scope_clause(&scope, &all) {
            Ok(clause) => clause,
            Err(()) => return failure(StatusCode::UNPROCESSABLE_ENTITY, "unsupported_scope"),
        }
    };
    let Ok(artifact) = std::fs::canonicalize(ready.artifact.directory()) else {
        return failure(StatusCode::CONFLICT, "corpus_unavailable");
    };
    let identity = lingrep_corpus::format_identity(ready.artifact.analysis_identity().as_bytes());
    let source_identity =
        lingrep_corpus::format_identity(ready.artifact.source_identity().as_bytes());
    let fingerprint = lingrep_core::fsm::catalog_fingerprint(&request.source);
    let Ok(program) = std::env::current_exe().and_then(|path| program(&path)) else {
        return failure(StatusCode::BAD_GATEWAY, "summary_failed");
    };
    match summarize_admitted(
        permit,
        program,
        request.source,
        artifact,
        clause,
        identity,
        source_identity,
        fingerprint,
    )
    .await
    {
        Ok(output) => (
            [
                (header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            output,
        )
            .into_response(),
        _ => failure(StatusCode::BAD_GATEWAY, "summary_failed"),
    }
}

async fn summarize_admitted(
    permit: tokio::sync::OwnedSemaphorePermit,
    program: PathBuf,
    source: String,
    artifact: PathBuf,
    clause: Option<String>,
    identity: String,
    source_identity: String,
    fingerprint: String,
) -> Result<Vec<u8>, ()> {
    let _permit = permit;
    run(
        program,
        source,
        artifact,
        clause,
        &identity,
        &source_identity,
        &fingerprint,
    )
    .await
}

fn scope_clause(scope: &SearchScope, all: &[String]) -> Result<Option<String>, ()> {
    fn set(labels: &[String]) -> Result<String, ()> {
        if labels.iter().any(|label| label.contains(['"', '\0']))
            || labels.iter().map(String::len).sum::<usize>() > 65_536
        {
            return Err(());
        }
        Ok(labels
            .iter()
            .map(|label| format!("\"{label}\""))
            .collect::<Vec<_>>()
            .join(", "))
    }
    if scope.is_unrestricted() {
        return Ok(None);
    }
    let selected = scope.included_sections.as_deref().unwrap_or(all);
    if all.is_empty() {
        if scope.include_unlabelled {
            return Ok(None);
        }
        return Ok(Some(
            "@document.__lingrep_empty_scope = x & @document.__lingrep_empty_scope != x".into(),
        ));
    }
    let selected = (!selected.is_empty())
        .then(|| set(selected).map(|values| format!("@section in {{{values}}}")))
        .transpose()?;
    let unlabelled = scope
        .include_unlabelled
        .then(|| set(all).map(|values| format!("@section not in {{{values}}}")))
        .transpose()?;
    Ok(Some(match (selected, unlabelled) {
        (Some(selected), Some(unlabelled)) => format!("({selected} | {unlabelled})"),
        (Some(clause), None) | (None, Some(clause)) => clause,
        (None, None) => {
            "@document.__lingrep_empty_scope = x & @document.__lingrep_empty_scope != x".into()
        }
    }))
}

fn program(executable: &std::path::Path) -> std::io::Result<PathBuf> {
    let sibling = executable.with_file_name(if cfg!(windows) { "ling.exe" } else { "ling" });
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "companion ling CLI is missing",
        ))
    }
}

async fn bounded_read(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: u64,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other("summary output limit"));
    }
    Ok(bytes)
}

async fn run(
    program: PathBuf,
    source: String,
    artifact: PathBuf,
    clause: Option<String>,
    identity: &str,
    source_identity: &str,
    fingerprint: &str,
) -> Result<Vec<u8>, ()> {
    let directory = tempfile::tempdir().map_err(|_| ())?;
    let mut file = std::fs::File::create(directory.path().join("query.lx")).map_err(|_| ())?;
    file.write_all(source.as_bytes()).map_err(|_| ())?;
    drop(file);
    let mut command = tokio::process::Command::new(program);
    command
        .current_dir(directory.path())
        .args([
            "summarize",
            "--json",
            "--patterns",
            "query.lx",
            "--top",
            "50",
            "--workers",
            "8",
            "--max-temp-bytes",
            "1073741824",
            "--corpus",
        ])
        .arg(artifact);
    if let Some(clause) = clause {
        command.args(["--where", &clause]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| ())?;
    let stdout = child.stdout.take().ok_or(())?;
    let stderr = child.stderr.take().ok_or(())?;
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        let (stdout, _) = tokio::try_join!(
            bounded_read(stdout, OUTPUT_BYTES),
            bounded_read(stderr, ERROR_BYTES)
        )
        .map_err(|_| ())?;
        if !child.wait().await.map_err(|_| ())?.success() {
            return Err(());
        }
        validate(&stdout, identity, source_identity, fingerprint)?;
        Ok(stdout)
    })
    .await;
    if let Ok(Ok(bytes)) = result {
        Ok(bytes)
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        Err(())
    }
}

fn validate(
    bytes: &[u8],
    identity: &str,
    source_identity: &str,
    fingerprint: &str,
) -> Result<(), ()> {
    if !bytes.ends_with(b"\n") {
        return Err(());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let mut run = false;
    let mut complete = false;
    let mut results = false;
    for line in text.lines() {
        if complete {
            return Err(());
        }
        let record: Value = serde_json::from_str(line).map_err(|_| ())?;
        if record["schema"] != "lingrep.query/v2" {
            return Err(());
        }
        match record["type"].as_str() {
            Some("run")
                if !run
                    && record["data"]["mode"] == "summarize"
                    && record["data"]["analysis_identity"] == identity
                    && record["data"]["source_identity"] == source_identity
                    && record["data"]["catalog_fingerprint"] == fingerprint
                    && record["data"]["matcher_semantics_identity"]
                        == lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY =>
            {
                run = true
            }
            Some("definition") if run && !results => {}
            Some("summary")
                if run
                    && record["data"]["pattern"].is_string()
                    && record["data"]["count"].as_u64().is_some()
                    && record["data"]["rank"].as_u64().is_some()
                    && record["data"]["group"].is_object() =>
            {
                results = true;
            }
            Some("complete")
                if run
                    && record["data"]["coverage"] == "full"
                    && record["data"]["termination"] == "eof"
                    && record["data"]["analysis_identity"] == identity
                    && record["data"]["source_identity"] == source_identity
                    && record["data"]["counters"].is_object() =>
            {
                complete = true
            }
            _ => return Err(()),
        }
    }
    if complete { Ok(()) } else { Err(()) }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn records(identity: &str, fingerprint: &str) -> Vec<Value> {
        vec![
            json!({"schema":"lingrep.query/v2", "type":"run", "data":{"mode":"summarize", "source_identity":"source", "analysis_identity":identity, "catalog_fingerprint":fingerprint, "matcher_semantics_identity":lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY}}),
            json!({"schema":"lingrep.query/v2", "type":"summary", "data":{"pattern":"p", "group":{"kind":"key","fields":[]}, "count":3, "rank":1}}),
            json!({"schema":"lingrep.query/v2", "type":"complete", "data":{"coverage":"full", "termination":"eof", "source_identity":"source", "analysis_identity":identity, "counters":{}}}),
        ]
    }

    fn wire(records: &[Value]) -> Vec<u8> {
        records
            .iter()
            .map(|value| format!("{value}\n"))
            .collect::<String>()
            .into_bytes()
    }

    #[hegel::test(test_cases = 50, derandomize = true)]
    fn summary_publication_requires_matching_complete_records(tc: hegel::TestCase) {
        use hegel::generators as g;
        let identity = format!("{:064x}", tc.draw(g::integers::<u64>()));
        let fingerprint = format!("{:064x}", tc.draw(g::integers::<u64>()));
        let valid = records(&identity, &fingerprint);
        assert_eq!(
            validate(&wire(&valid), &identity, "source", &fingerprint),
            Ok(())
        );
        for (index, field) in [
            (0, "mode"),
            (0, "analysis_identity"),
            (0, "source_identity"),
            (0, "catalog_fingerprint"),
            (0, "matcher_semantics_identity"),
            (2, "analysis_identity"),
            (2, "source_identity"),
            (2, "coverage"),
            (2, "termination"),
        ] {
            let mut invalid = valid.clone();
            invalid[index]["data"][field] = json!("incompatible");
            assert_eq!(
                validate(&wire(&invalid), &identity, "source", &fingerprint),
                Err(()),
                "{field}"
            );
        }
        for invalid in [
            vec![],
            valid[..2].to_vec(),
            vec![valid[1].clone(), valid[0].clone(), valid[2].clone()],
            vec![valid[0].clone(), valid[2].clone(), valid[2].clone()],
        ] {
            assert_eq!(
                validate(&wire(&invalid), &identity, "source", &fingerprint),
                Err(())
            );
        }
    }

    #[hegel::test(test_cases = 50, derandomize = true)]
    fn section_scope_clauses_preserve_unlabelled_and_overlapping_admission(tc: hegel::TestCase) {
        use hegel::generators as g;
        use lingrep_core::scope::{ScopeClause, Verdict};
        let all = if tc.draw(g::booleans()) {
            vec![]
        } else {
            vec![
                "methods".to_owned(),
                "方法 😀,結果".to_owned(),
                format!("s{}", tc.draw(g::integers::<u64>())),
            ]
        };
        let selected = all
            .iter()
            .filter(|_| tc.draw(g::booleans()))
            .cloned()
            .collect::<Vec<_>>();
        let scope = SearchScope {
            included_sections: tc.draw(g::booleans()).then_some(selected),
            include_unlabelled: tc.draw(g::booleans()),
        };
        let clause = scope_clause(&scope, &all)
            .unwrap()
            .map(|source| ScopeClause::parse(&source).unwrap());
        for mask in 0..(1 << all.len()) {
            let active = all
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, label)| label.clone())
                .collect::<Vec<_>>();
            let expected = if active.is_empty() {
                scope.include_unlabelled
            } else {
                scope
                    .included_sections
                    .as_ref()
                    .is_none_or(|selected| active.iter().any(|label| selected.contains(label)))
            };
            let actual = clause.as_ref().is_none_or(|clause| {
                clause.evaluate_sentence(
                    &Default::default(),
                    &Default::default(),
                    active.as_slice(),
                    &Default::default(),
                ) == Verdict::Admitted
            });
            assert_eq!(actual, expected, "{scope:?} {active:?}");
        }
    }

    #[tokio::test]
    async fn summary_reads_reject_overflow_instead_of_returning_a_prefix() {
        assert_eq!(bounded_read(&b"1234"[..], 4).await.unwrap(), b"1234");
        assert!(bounded_read(&b"12345"[..], 4).await.is_err());
        assert!(bounded_read(&b"1"[..], 0).await.is_err());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn overflowing_summary_children_are_killed_and_reaped() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("writer.py");
        std::fs::write(&program, "#!/usr/bin/env python3\nimport os,pathlib,sys,time\npathlib.Path(__file__).with_suffix('.pid').write_text(str(os.getpid()))\ntry:\n sys.stdout.write('x' * 20000000)\n sys.stdout.flush()\nexcept BrokenPipeError:\n time.sleep(30)\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = std::time::Instant::now();
        assert!(
            run(
                program.clone(),
                "p := [].".into(),
                directory.path().to_path_buf(),
                None,
                "identity",
                "source",
                "fingerprint"
            )
            .await
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(10));
        let pid = std::fs::read_to_string(program.with_extension("pid")).unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_dropped_summary_request_kills_its_child_and_frees_the_scan_permit() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("sleeper.py");
        std::fs::write(&program, "#!/usr/bin/env python3\nimport os,pathlib,time\npathlib.Path(__file__).with_suffix('.pid').write_text(str(os.getpid()))\ntime.sleep(60)\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pid_file = program.with_extension("pid");
        let registry = crate::coordinator::CoordinatorRegistry::new(1);
        let permit = registry.try_acquire_scan_permit().unwrap();
        let request = tokio::spawn(summarize_admitted(
            permit,
            program.clone(),
            "p := [].".into(),
            directory.path().to_path_buf(),
            None,
            "identity".into(),
            "source".into(),
            "fingerprint".into(),
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pid_file.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the summary child did not start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(registry.try_acquire_scan_permit().is_none());
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let permit = tokio::time::timeout(Duration::from_secs(1), registry.acquire_scan_permit())
            .await
            .expect("the scan permit is released once the request is dropped");
        drop(permit);
        let process = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while process.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "summary child {pid} is still running after the request was dropped"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    #[test]
    fn summary_program_requires_the_sibling_cli() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("lingrep-web");
        assert!(program(&executable).is_err());
        let sibling = executable.with_file_name(if cfg!(windows) { "ling.exe" } else { "ling" });
        std::fs::write(&sibling, b"companion").unwrap();
        assert_eq!(program(&executable).unwrap(), sibling);
    }
}
