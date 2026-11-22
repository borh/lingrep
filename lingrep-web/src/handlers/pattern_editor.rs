use std::borrow::Cow;

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use lingrep_core::fsm::{
    CatalogAnalysis, CompileOutput, CompilerStats, Diagnostic, DiagnosticCode, DiagnosticLocation,
    DiagnosticSeverity, FormatError, FullWidthCharacter,
};
use lingrep_core::token_match::AttributeKey;
use serde::{Deserialize, Serialize};

use crate::patterns::{WEB_PATTERN_SOURCE_NAME, compile_web_pattern, format_web_pattern};
use crate::state::RequestStore;

pub fn routes() -> Router<RequestStore> {
    Router::new()
        .merge(super::live_summary::routes())
        .route("/check", post(check))
        .route("/format", post(format))
        .route("/diagram", post(diagram))
}

#[derive(Deserialize)]
struct DiagramRequest {
    source: String,
    definition: String,
    #[serde(default)]
    expand: bool,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum DiagramResponse {
    /// The `DiagramCatalog` rendering `ling check --diagram` produces.
    Rendered {
        svg: String,
    },
    /// The source does not compile.
    Invalid,
    /// The source has no pattern or fragment of that name, or its diagram
    /// exceeds a rendering limit.
    Unavailable {
        reason: String,
    },
    BadRequest,
}

const DIAGRAM_COLUMNS: usize = 100;

async fn diagram(
    State(store): State<RequestStore>,
    request: Result<Json<DiagramRequest>, JsonRejection>,
) -> Response {
    use lingrep_core::fsm::{CompileLimits, DiagramCatalog, DiagramError, PatternSource};
    let Ok(Json(request)) = request else {
        return (StatusCode::BAD_REQUEST, Json(DiagramResponse::BadRequest)).into_response();
    };
    let rendered = store
        .coordinators
        .pattern_task(move || {
            DiagramCatalog::parse(
                PatternSource::named(WEB_PATTERN_SOURCE_NAME, &request.source),
                CompileLimits::default(),
            )?
            .diagram(&request.definition, request.expand)?
            .render_embedded_svg(DIAGRAM_COLUMNS)
        })
        .await;
    match rendered {
        Err(error) => super::pattern_task_error(error),
        Ok(Ok(svg)) => Json(DiagramResponse::Rendered { svg }).into_response(),
        Ok(Err(DiagramError::Compile(_))) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(DiagramResponse::Invalid),
        )
            .into_response(),
        Ok(Err(error)) => Json(DiagramResponse::Unavailable {
            reason: error.to_string(),
        })
        .into_response(),
    }
}

#[derive(Deserialize)]
struct EditorRequest {
    source: String,
    /// The key of a served corpus. When it names a ready corpus, `check` also
    /// binds the source to that corpus's token schema and reports what the
    /// binding finds, as a run would.
    #[serde(default)]
    corpus: Option<String>,
}

#[derive(Serialize)]
struct EditorLocation {
    source_name: String,
    line: usize,
    column: usize,
    byte_range: Option<EditorByteRange>,
}

#[derive(Serialize)]
struct EditorByteRange {
    start: usize,
    end: usize,
}

#[derive(Serialize)]
struct EditorDiagnostic {
    code: String,
    category: &'static str,
    severity: &'static str,
    message: Cow<'static, str>,
    message_key: &'static str,
    /// Interpolation arguments for `message_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<serde_json::Value>,
    location: Option<EditorLocation>,
}

/// The compile facts the editor shows. A deliberate subset of `CompilerStats`:
/// these are the numbers a catalog author acts on. The budget counters
/// (`generated_owned_bytes`, `expansion_work`) describe compiler internals and
/// would read as noise above a query box.
#[derive(Serialize)]
struct EditorStats {
    definitions: usize,
    expressions: usize,
    references: usize,
    depth: usize,
    nfa_states: usize,
    transitions: usize,
}

impl From<&CompilerStats> for EditorStats {
    fn from(stats: &CompilerStats) -> Self {
        Self {
            definitions: stats.definitions(),
            expressions: stats.expression_nodes(),
            references: stats.reference_edges(),
            depth: stats.max_depth(),
            nfa_states: stats.nfa_states(),
            transitions: stats.transitions(),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CheckResponse {
    Valid {
        capture_key_edits: Vec<lingrep_core::fsm::CaptureKeyEdit>,
        diagnostics: Vec<EditorDiagnostic>,
        stats: EditorStats,
        /// The token attribute keys of the requested corpus, when it is ready.
        #[serde(skip_serializing_if = "Option::is_none")]
        token_keys: Option<Vec<String>>,
    },
    Invalid {
        diagnostics: Vec<EditorDiagnostic>,
    },
    BadRequest,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum FormatResponse {
    Formatted { source: String, changed: bool },
    Invalid { diagnostics: Vec<EditorDiagnostic> },
    BadRequest,
    ServerError,
}

async fn check(
    State(store): State<RequestStore>,
    request: Result<Json<EditorRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = request else {
        return (StatusCode::BAD_REQUEST, Json(CheckResponse::BadRequest)).into_response();
    };
    let source = request.source.clone();
    let compiled = match store
        .coordinators
        .pattern_task(move || compile_web_pattern(&source))
        .await
    {
        Ok(compiled) => compiled,
        Err(error) => return super::pattern_task_error(error),
    };
    match compiled {
        Ok(output) => {
            let mut diagnostics = editor_diagnostics(&request.source, &output.diagnostics);
            let mut token_keys = None;
            if let Some((schema, schema_name)) =
                corpus_schema(&store, request.corpus.as_deref()).await
            {
                token_keys = Some(
                    schema
                        .keys()
                        .iter()
                        .map(|key| key.as_str().to_owned())
                        .collect(),
                );
                match binding_diagnostics(&request.source, &output, &schema, &schema_name) {
                    Ok(found) => diagnostics.extend(found),
                    Err(refusal) => {
                        return (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(CheckResponse::Invalid {
                                diagnostics: vec![*refusal],
                            }),
                        )
                            .into_response();
                    }
                }
            }
            (
                StatusCode::OK,
                Json(CheckResponse::Valid {
                    capture_key_edits: output.capture_key_edits,
                    diagnostics,
                    stats: EditorStats::from(&output.stats),
                    token_keys,
                }),
            )
                .into_response()
        }
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(CheckResponse::Invalid {
                diagnostics: editor_diagnostics(&request.source, &error.diagnostics),
            }),
        )
            .into_response(),
    }
}

/// The token schema of the served corpus `key` and its name as run notices
/// give it (`family profile`); `None` when no key is given, the key names no
/// served corpus, or the corpus is not ready.
pub(super) async fn corpus_schema(
    store: &RequestStore,
    key: Option<&str>,
) -> Option<(lingrep_core::token_match::TokenSchema, String)> {
    let slot = store.corpora.resolve(Some(key?))?;
    let ready = slot.runtime.ready().await?;
    let schema = ready.artifact.token_schema().ok()?;
    let analyzer = ready.artifact.analyzer();
    Some((schema, format!("{} {}", analyzer.family, analyzer.profile)))
}

/// What binding `output` to `schema` finds, as a run reports it: a warning per
/// definition that references an attribute the schema does not carry, or the
/// refusal a run fails with (a dependency `where` without its edge keys, or a
/// value-kind conflict).
fn binding_diagnostics(
    source: &str,
    output: &CompileOutput,
    schema: &lingrep_core::token_match::TokenSchema,
    schema_name: &str,
) -> Result<Vec<EditorDiagnostic>, Box<EditorDiagnostic>> {
    let bound = output
        .matcher
        .bind_with_projection(schema, output.matcher.execution_attributes())
        .map_err(|error| {
            Box::new(EditorDiagnostic {
            code: "SchemaBind".into(),
            category: "schema",
            severity: "error",
            message: Cow::Owned(format!(
                "The pattern cannot bind to this corpus's token schema ({schema_name}): {error}."
            )),
            message_key: "editor.diag_schema_bind",
            args: Some(serde_json::json!({ "schema": schema_name, "error": error.to_string() })),
            location: None,
        })
        })?;
    Ok(bound
        .unavailable_attributes()
        .iter()
        .flat_map(|attribute| {
            unavailable_diagnostics(source, &output.analysis, attribute, schema_name)
        })
        .collect())
}

/// One warning per definition referencing `attribute`, placed on its first
/// mention at or after the definition's declaration; one unplaced warning when
/// only the catalog configuration references it.
fn unavailable_diagnostics(
    source: &str,
    analysis: &CatalogAnalysis,
    attribute: &AttributeKey,
    schema_name: &str,
) -> Vec<EditorDiagnostic> {
    let sites: Vec<_> = analysis.references_to(attribute).collect();
    let definitions = sites
        .iter()
        .map(|site| site.definition.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let diagnostic = |location: Option<EditorLocation>| {
        let referenced_by = if definitions.is_empty() {
            "the catalog configuration"
        } else {
            &definitions
        };
        EditorDiagnostic {
            code: "attribute_unavailable".into(),
            category: "schema",
            severity: "warning",
            message: Cow::Owned(format!(
                "This corpus's token schema ({schema_name}) does not carry `{attribute}`, which \
                 {referenced_by} references; tests over it match nothing, and only `!(…)` \
                 complements hold."
            )),
            message_key: "editor.diag_attribute_unavailable",
            args: Some(serde_json::json!({
                "attribute": attribute.as_str(),
                "definitions": referenced_by,
                "schema": schema_name,
            })),
            location,
        }
    };
    if sites.is_empty() {
        return vec![diagnostic(None)];
    }
    sites
        .iter()
        .map(|site| diagnostic(mention(source, site.declaration_line, attribute.as_str())))
        .collect()
}

/// The first occurrence of `needle` at or after the start of 1-based `line`.
fn mention(source: &str, line: usize, needle: &str) -> Option<EditorLocation> {
    let line_start = source
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let start = line_start + source.get(line_start..)?.find(needle)?;
    let before = source.get(..start)?;
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |text| text.chars().count())
        + 1;
    Some(EditorLocation {
        source_name: WEB_PATTERN_SOURCE_NAME.into(),
        line,
        column,
        byte_range: Some(EditorByteRange {
            start,
            end: start + needle.len(),
        }),
    })
}

async fn format(request: Result<Json<EditorRequest>, JsonRejection>) -> Response {
    let Ok(Json(request)) = request else {
        return (StatusCode::BAD_REQUEST, Json(FormatResponse::BadRequest)).into_response();
    };
    match format_web_pattern(&request.source) {
        Ok(output) => (
            StatusCode::OK,
            Json(FormatResponse::Formatted {
                source: output.source,
                changed: output.changed,
            }),
        )
            .into_response(),
        Err(FormatError::Syntax { line, column }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(FormatResponse::Invalid {
                diagnostics: vec![EditorDiagnostic {
                    code: "Syntax".into(),
                    category: "syntax",
                    severity: "error",
                    message: Cow::Borrowed("Pattern syntax is invalid."),
                    message_key: "editor.diag_syntax",
                    args: None,
                    location: Some(EditorLocation {
                        source_name: WEB_PATTERN_SOURCE_NAME.into(),
                        line,
                        column,
                        byte_range: None,
                    }),
                }],
            }),
        )
            .into_response(),
        Err(FormatError::SourceTooLarge { .. }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(FormatResponse::Invalid {
                diagnostics: vec![EditorDiagnostic {
                    code: "LimitExceeded".into(),
                    category: "limit",
                    severity: "error",
                    message: Cow::Borrowed("Pattern is too large."),
                    message_key: "editor.diag_too_large",
                    args: None,
                    location: None,
                }],
            }),
        )
            .into_response(),
        Err(FormatError::Parser) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FormatResponse::ServerError),
        )
            .into_response(),
    }
}

fn editor_diagnostics(source: &str, diagnostics: &[Diagnostic]) -> Vec<EditorDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let (category, message, mut message_key) = diagnostic_message(diagnostic.code());
            let mut location = diagnostic.location();
            let mut args = None;
            if let Some((note, character)) = full_width_note(source, diagnostic) {
                location = note;
                if character.ascii == ' ' {
                    message_key = "editor.diag_full_width_space";
                    args = Some(serde_json::json!({ "found": character.found }));
                } else {
                    message_key = "editor.diag_full_width";
                    args = Some(serde_json::json!({
                        "found": character.found,
                        "ascii": character.ascii,
                    }));
                }
            }
            EditorDiagnostic {
                code: diagnostic.code().as_str().into(),
                category,
                severity: match diagnostic.severity() {
                    DiagnosticSeverity::Error => "error",
                    DiagnosticSeverity::Warning => "warning",
                },
                message: Cow::Borrowed(message),
                message_key,
                args,
                location: Some(EditorLocation {
                    source_name: location.source_name().into(),
                    line: location.line(),
                    column: location.column(),
                    byte_range: Some(EditorByteRange {
                        start: location.byte_range().start,
                        end: location.byte_range().end,
                    }),
                }),
            }
        })
        .collect()
}

/// The full-width character a syntax diagnostic's note points at, and the
/// note's location.
fn full_width_note<'a>(
    source: &str,
    diagnostic: &'a Diagnostic,
) -> Option<(&'a DiagnosticLocation, FullWidthCharacter)> {
    if diagnostic.code() != DiagnosticCode::Syntax {
        return None;
    }
    diagnostic.related_locations().iter().find_map(|related| {
        let location = related.location();
        let character = FullWidthCharacter::first_in(source.get(location.byte_range())?)?;
        Some((location, character))
    })
}

fn diagnostic_message(code: DiagnosticCode) -> (&'static str, &'static str, &'static str) {
    match code {
        DiagnosticCode::Syntax | DiagnosticCode::UnknownNode => {
            ("syntax", "Pattern syntax is invalid.", "editor.diag_syntax")
        }
        DiagnosticCode::UndefinedReference
        | DiagnosticCode::ReferenceCycle
        | DiagnosticCode::DuplicateDefinition => (
            "reference",
            "Pattern references are invalid.",
            "editor.diag_reference",
        ),
        DiagnosticCode::InvalidRegex => (
            "regex",
            "A regular expression is invalid.",
            "editor.diag_regex",
        ),
        DiagnosticCode::UnusedFragment => (
            "lint",
            "A fragment is never referenced.",
            "editor.diag_unused_fragment",
        ),
        DiagnosticCode::UnusedSet => (
            "lint",
            "A set is never referenced.",
            "editor.diag_unused_set",
        ),
        DiagnosticCode::UnusedTest => (
            "lint",
            "A test is never referenced.",
            "editor.diag_unused_test",
        ),
        DiagnosticCode::NormalizedLiteral => (
            "lint",
            "Matching uses the NFC form of this literal.",
            "editor.diag_normalized_literal",
        ),
        DiagnosticCode::RegexNotNfc => (
            "lint",
            "This regular expression is used as written and may miss NFC-normalized text.",
            "editor.diag_regex_not_nfc",
        ),
        DiagnosticCode::FoldedLiteral => (
            "lint",
            "This `text` literal matches as its folded form. Use `surface` for the exact spelling.",
            "editor.diag_folded_literal",
        ),
        DiagnosticCode::RegexNotFolded => (
            "lint",
            "This regular expression is used as written, but a folded `text` never contains some of its characters. Use `surface` for the exact spelling.",
            "editor.diag_regex_not_folded",
        ),
        DiagnosticCode::PipeInBareValue => (
            "syntax",
            "An unquoted value contains `|`, which does not \
             separate operands.",
            "editor.diag_pipe",
        ),
        // Same reason as the arm above: the generic syntax text would name no
        // recovery, and the unquoted spelling is the whole content of this
        // one.
        DiagnosticCode::QuotedAttributeValue => (
            "syntax",
            "A token-test value is wrapped in quotes, which are part of the \
             literal.",
            "editor.diag_quoted",
        ),
        DiagnosticCode::UnverifiableExamples => (
            "lint",
            "A documented example cannot be verified.",
            "editor.diag_examples",
        ),
        DiagnosticCode::LimitExceeded
        | DiagnosticCode::TokenTestTooComplex
        | DiagnosticCode::TokenTestExpansionTooLarge => (
            "limit",
            "Pattern exceeds a compiler limit.",
            "editor.diag_limit",
        ),
        _ => ("compile", "Pattern is invalid.", "editor.diag_invalid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn check_rejects_work_when_pattern_workers_are_occupied() {
        let store = RequestStore::default();
        let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let mut releases = Vec::new();
        let mut workers = Vec::new();
        for _ in 0..2 {
            let registry = store.coordinators.clone();
            let started = started.clone();
            let (release, wait) = std::sync::mpsc::channel();
            releases.push(release);
            workers.push(tokio::spawn(async move {
                registry
                    .pattern_task(move || {
                        started.send(()).unwrap();
                        wait.recv_timeout(std::time::Duration::from_secs(5))
                            .expect("pattern task was not released");
                        compile_web_pattern("p := [].").unwrap()
                    })
                    .await
                    .unwrap()
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), starts.recv())
            .await
            .expect("pattern task did not start")
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), starts.recv())
            .await
            .expect("pattern task did not start")
            .unwrap();
        let response = check(
            State(store.clone()),
            Ok(Json(EditorRequest {
                source: "p := [].".into(),
                corpus: None,
            })),
        )
        .await;
        use tower::ServiceExt;
        let summary = routes()
            .with_state(store.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/summary")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"source":"p := [].","definition":"p"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        for release in releases {
            release.send(()).unwrap();
        }
        for worker in workers {
            worker.await.unwrap();
        }
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(summary.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(summary.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
            "summary_busy"
        );
        assert!(store.coordinators.try_acquire_scan_permit().is_some());
        let response = check(
            State(store),
            Ok(Json(EditorRequest {
                source: "p := [].".into(),
                corpus: None,
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    async fn check_body(source: &str) -> serde_json::Value {
        let response = check(
            State(RequestStore::default()),
            Ok(Json(EditorRequest {
                source: source.into(),
                corpus: None,
            })),
        )
        .await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn a_syntax_error_at_full_width_punctuation_names_it_and_its_ascii_character() {
        for (source, args, key, start, end) in [
            (
                "n := ［upos=NOUN］.",
                serde_json::json!({"found": "［", "ascii": "["}),
                "editor.diag_full_width",
                5,
                8,
            ),
            (
                "n := [upos=NOUN]\u{3000}[upos=ADP].",
                serde_json::json!({"found": "\u{3000}"}),
                "editor.diag_full_width_space",
                16,
                19,
            ),
        ] {
            assert_eq!(
                check_body(source).await,
                serde_json::json!({
                    "status": "invalid",
                    "diagnostics": [{
                        "code": "Syntax",
                        "category": "syntax",
                        "severity": "error",
                        "message": "Pattern syntax is invalid.",
                        "message_key": key,
                        "args": args,
                        "location": {
                            "source_name": WEB_PATTERN_SOURCE_NAME,
                            "line": 1,
                            "column": start + 1,
                            "byte_range": {"start": start, "end": end},
                        },
                    }],
                }),
                "{source}"
            );
        }
    }

    #[tokio::test]
    async fn an_nfd_value_is_reported_as_searched_for_in_its_nfc_form() {
        let body = check_body("g := [text=\u{30ab}\u{3099}].").await;
        assert_eq!(body["status"], "valid", "{body}");
        assert_eq!(
            body["diagnostics"],
            serde_json::json!([{
                "code": "NormalizedLiteral",
                "category": "lint",
                "severity": "warning",
                "message": "Matching uses the NFC form of this literal.",
                "message_key": "editor.diag_normalized_literal",
                "location": {
                    "source_name": WEB_PATTERN_SOURCE_NAME,
                    "line": 1,
                    "column": 12,
                    "byte_range": {"start": 11, "end": 17},
                },
            }])
        );
    }

    #[tokio::test]
    async fn a_text_literal_or_regex_the_key_fold_changes_is_a_lint() {
        for (source, code, message_key, end) in [
            (
                "g := [text=ＮＨＫ].",
                "FoldedLiteral",
                "editor.diag_folded_literal",
                20,
            ),
            (
                "g := [text~/～/].",
                "RegexNotFolded",
                "editor.diag_regex_not_folded",
                16,
            ),
        ] {
            let body = check_body(source).await;
            assert_eq!(body["status"], "valid", "{body}");
            let diagnostics = body["diagnostics"].as_array().unwrap();
            assert_eq!(diagnostics.len(), 1, "{body}");
            let diagnostic = &diagnostics[0];
            assert_eq!(diagnostic["code"], code);
            assert_eq!(diagnostic["category"], "lint");
            assert_eq!(diagnostic["severity"], "warning");
            assert_eq!(diagnostic["message_key"], message_key);
            assert_eq!(
                diagnostic["location"]["byte_range"],
                serde_json::json!({"start": 11, "end": end})
            );
        }
    }

    #[test]
    fn unknown_diagnostic_categories_have_a_safe_fallback() {
        assert_eq!(
            diagnostic_message(DiagnosticCode::NullablePattern),
            ("compile", "Pattern is invalid.", "editor.diag_invalid")
        );
    }

    #[test]
    fn unused_set_is_a_lint_not_an_invalid_pattern() {
        assert_eq!(
            diagnostic_message(DiagnosticCode::UnusedSet),
            (
                "lint",
                "A set is never referenced.",
                "editor.diag_unused_set"
            )
        );
    }

    #[test]
    fn unused_test_is_a_lint_not_an_invalid_pattern() {
        assert_eq!(
            diagnostic_message(DiagnosticCode::UnusedTest),
            (
                "lint",
                "A test is never referenced.",
                "editor.diag_unused_test"
            )
        );
    }

    /// It keeps its own message after the promotion: the generic syntax text
    /// names no recovery, and naming both is why this code exists at all.
    #[test]
    fn an_unspaced_pipe_is_a_syntax_error_that_keeps_its_own_message() {
        let (category, message, _) = diagnostic_message(DiagnosticCode::PipeInBareValue);
        assert_eq!(category, "syntax");
        assert_ne!(message, diagnostic_message(DiagnosticCode::Syntax).1);
    }

    /// For the same reason: the recovery is the message.
    #[test]
    fn a_quoted_value_is_a_syntax_error_that_keeps_its_own_message() {
        let (category, message, _) = diagnostic_message(DiagnosticCode::QuotedAttributeValue);
        assert_eq!(category, "syntax");
        assert_ne!(message, diagnostic_message(DiagnosticCode::Syntax).1);
        assert_ne!(
            message,
            diagnostic_message(DiagnosticCode::PipeInBareValue).1
        );
    }

    #[test]
    fn both_token_test_budgets_are_a_limit_not_an_invalid_pattern() {
        assert_eq!(
            diagnostic_message(DiagnosticCode::TokenTestTooComplex),
            (
                "limit",
                "Pattern exceeds a compiler limit.",
                "editor.diag_limit"
            )
        );
        assert_eq!(
            diagnostic_message(DiagnosticCode::TokenTestExpansionTooLarge),
            (
                "limit",
                "Pattern exceeds a compiler limit.",
                "editor.diag_limit"
            )
        );
    }
}
