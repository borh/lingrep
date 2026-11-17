use std::borrow::Cow;

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use lingrep_core::fsm::{
    CatalogAnalysis, CompileOutput, CompilerStats, Diagnostic, DiagnosticCode, DiagnosticSeverity,
    FormatError,
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
    match compile_web_pattern(&request.source) {
        Ok(output) => {
            let mut diagnostics = editor_diagnostics(&output.diagnostics);
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
                diagnostics: editor_diagnostics(&error.diagnostics),
            }),
        )
            .into_response(),
    }
}

/// The token schema of the served corpus `key` and its name as run notices
/// give it (`family profile`); `None` when no key is given, the key names no
/// served corpus, or the corpus is not ready.
async fn corpus_schema(
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

fn editor_diagnostics(diagnostics: &[Diagnostic]) -> Vec<EditorDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let (category, message, message_key) = diagnostic_message(diagnostic.code());
            let location = diagnostic.location();
            EditorDiagnostic {
                code: diagnostic.code().as_str().into(),
                category,
                severity: match diagnostic.severity() {
                    DiagnosticSeverity::Error => "error",
                    DiagnosticSeverity::Warning => "warning",
                },
                message: Cow::Borrowed(message),
                message_key,
                args: None,
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
