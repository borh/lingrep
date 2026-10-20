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
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use lingrep_web::{RequestStore, handlers::editor_assets};
use sha2::{Digest, Sha256};
use std::process::Command;
use tower::ServiceExt;

/// The `tree-sitter-lingrep` revision `Cargo.lock` pins.
///
/// Read rather than hardcoded: the vendored WASM defines what an lingrep patterns
/// definition and reference *are* for the editor's navigation, so a manifest
/// that drifts from the compiler's own grammar is a correctness problem, not a
/// bookkeeping one. Asserting the manifest against a literal copy of itself —
/// which this test did — cannot detect that.
fn pinned_grammar_revision() -> String {
    let lock_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path).expect("workspace Cargo.lock must be readable");
    let mut in_grammar_package = false;
    for line in lock.lines() {
        if line == "[[package]]" {
            in_grammar_package = false;
        } else if line == r#"name = "tree-sitter-lingrep""# {
            in_grammar_package = true;
        } else if in_grammar_package && let Some(rest) = line.strip_prefix(r#"source = ""#) {
            let revision = rest
                .trim_end_matches('"')
                .rsplit('#')
                .next()
                .unwrap_or_default();
            assert_eq!(
                revision.len(),
                40,
                "expected a 40-character git revision, got {revision:?}"
            );
            return revision.to_owned();
        }
    }
    panic!(
        "tree-sitter-lingrep has no pinned git revision in {}",
        lock_path.display()
    );
}

#[tokio::test]
async fn embedded_vendor_assets_match_the_pinned_manifest() {
    let app = editor_assets::routes().with_state(RequestStore::default());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/vendor/manifest.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-cache");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        manifest["grammar_revision"].as_str().unwrap(),
        pinned_grammar_revision(),
        "vendored grammar has drifted from the revision Cargo.lock pins; \
         run `nix develop --command bash lingrep-web/assets/editor/regenerate.sh`"
    );
    assert_eq!(manifest["runtime_version"], "0.26.11");

    for (name, hash) in manifest["sha256"].as_object().unwrap() {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/vendor/{name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{name}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            hash.as_str().unwrap()
        );
    }

    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping lingrep patterns WASM/query compatibility check: node unavailable");
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/editor/vendor");
    let script = format!(
        r#"import fs from 'node:fs/promises'; import {{Parser,Language,Query}} from '{}'; await Parser.init({{locateFile:()=> '{}'}}); const language=await Language.load('{}'); const source=await fs.readFile('{}','utf8'); new Query(language,source);"#,
        root.join("web-tree-sitter.js").display(),
        root.join("web-tree-sitter.wasm").display(),
        root.join("tree-sitter-lingrep.wasm").display(),
        root.join("highlights.scm").display()
    );
    let output = Command::new("node")
        .args(["--input-type=module", "-e", &script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "full highlight query must construct: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
