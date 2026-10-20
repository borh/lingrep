use axum::{
    Json, Router,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use lingrep_core::fsm::{
    CompilerStats, Diagnostic, DiagnosticCode, DiagnosticSeverity, FormatError,
};
use serde::{Deserialize, Serialize};

use crate::patterns::{WEB_PATTERN_SOURCE_NAME, compile_web_pattern, format_web_pattern};
use crate::state::RequestStore;

pub fn routes() -> Router<RequestStore> {
    Router::new()
        .route("/check", post(check))
        .route("/format", post(format))
}

#[derive(Deserialize)]
struct EditorRequest {
    source: String,
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
    message: &'static str,
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
        diagnostics: Vec<EditorDiagnostic>,
        stats: EditorStats,
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

async fn check(request: Result<Json<EditorRequest>, JsonRejection>) -> Response {
    let Ok(Json(request)) = request else {
        return (StatusCode::BAD_REQUEST, Json(CheckResponse::BadRequest)).into_response();
    };
    match compile_web_pattern(&request.source) {
        Ok(output) => (
            StatusCode::OK,
            Json(CheckResponse::Valid {
                diagnostics: editor_diagnostics(&output.diagnostics),
                stats: EditorStats::from(&output.stats),
            }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(CheckResponse::Invalid {
                diagnostics: editor_diagnostics(&error.diagnostics),
            }),
        )
            .into_response(),
    }
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
                    message: "Pattern syntax is invalid.",
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
                    message: "Pattern is too large.",
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
            let (category, message) = diagnostic_message(diagnostic.code());
            let location = diagnostic.location();
            EditorDiagnostic {
                code: diagnostic.code().as_str().into(),
                category,
                severity: match diagnostic.severity() {
                    DiagnosticSeverity::Error => "error",
                    DiagnosticSeverity::Warning => "warning",
                },
                message,
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

fn diagnostic_message(code: DiagnosticCode) -> (&'static str, &'static str) {
    match code {
        DiagnosticCode::Syntax | DiagnosticCode::UnknownNode => {
            ("syntax", "Pattern syntax is invalid.")
        }
        DiagnosticCode::UndefinedReference
        | DiagnosticCode::ReferenceCycle
        | DiagnosticCode::DuplicateDefinition => ("reference", "Pattern references are invalid."),
        DiagnosticCode::InvalidRegex => ("regex", "A regular expression is invalid."),
        DiagnosticCode::UnusedFragment => ("lint", "A fragment is never referenced."),
        DiagnosticCode::UnusedSet => ("lint", "A set is never referenced."),
        DiagnosticCode::UnusedTest => ("lint", "A test is never referenced."),
        // A syntax refusal, and its own arm rather than falling through to
        // `Syntax`, because the specific message is the whole value of the
        // code: it names the two recoveries. This was a lint while the
        // compiler accepted the document and only warned that the value it
        // built was almost certainly not the one written.
        DiagnosticCode::PipeInBareValue => (
            "syntax",
            "An unquoted value contains `|`, which does not \
             separate operands.",
        ),
        // Same reason as the arm above: the generic syntax text would name no
        // recovery, and the unquoted spelling is the whole content of this
        // one.
        DiagnosticCode::QuotedAttributeValue => (
            "syntax",
            "A token-test value is wrapped in quotes, which are part of the \
             literal.",
        ),
        DiagnosticCode::UnverifiableExamples => {
            ("lint", "A documented example cannot be verified.")
        }
        DiagnosticCode::LimitExceeded
        | DiagnosticCode::TokenTestTooComplex
        | DiagnosticCode::TokenTestExpansionTooLarge => {
            ("limit", "Pattern exceeds a compiler limit.")
        }
        _ => ("compile", "Pattern is invalid."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_diagnostic_categories_have_a_safe_fallback() {
        assert_eq!(
            diagnostic_message(DiagnosticCode::NullablePattern),
            ("compile", "Pattern is invalid.")
        );
    }

    #[test]
    fn unused_set_is_a_lint_not_an_invalid_pattern() {
        assert_eq!(
            diagnostic_message(DiagnosticCode::UnusedSet),
            ("lint", "A set is never referenced.")
        );
    }

    #[test]
    fn unused_test_is_a_lint_not_an_invalid_pattern() {
        assert_eq!(
            diagnostic_message(DiagnosticCode::UnusedTest),
            ("lint", "A test is never referenced.")
        );
    }

    /// It keeps its own message after the promotion: the generic syntax text
    /// names no recovery, and naming both is why this code exists at all.
    #[test]
    fn an_unspaced_pipe_is_a_syntax_error_that_keeps_its_own_message() {
        let (category, message) = diagnostic_message(DiagnosticCode::PipeInBareValue);
        assert_eq!(category, "syntax");
        assert_ne!(message, diagnostic_message(DiagnosticCode::Syntax).1);
    }

    /// For the same reason: the recovery is the message.
    #[test]
    fn a_quoted_value_is_a_syntax_error_that_keeps_its_own_message() {
        let (category, message) = diagnostic_message(DiagnosticCode::QuotedAttributeValue);
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
            ("limit", "Pattern exceeds a compiler limit.")
        );
        assert_eq!(
            diagnostic_message(DiagnosticCode::TokenTestExpansionTooLarge),
            ("limit", "Pattern exceeds a compiler limit.")
        );
    }
}
