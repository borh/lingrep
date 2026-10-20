// Required, not conceded. This module is compiled into two separate test
// crates — the consolidated suites and isolated process-level tests — and each
// sees only its own uses.
//
// Every item here does have a consumer somewhere; checked 2026-07-30. The
// allow cannot tell you that, so verify by hand across `tests/*.rs` rather
// than trusting the build.
#![allow(dead_code)]

use lingrep_core::corpus::{Metadata, metadata_from_json};
use lingrep_core::fsm::{CompileError, DiagnosticCode};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

/// Whether `error` carries a diagnostic with exactly this code.
///
/// Asserting the code rather than the message is what keeps a test honest about
/// *which* refusal fired; three compile-test modules had their own copy.
pub fn refuses_with(error: &CompileError, code: DiagnosticCode) -> bool {
    error
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == code)
}

/// One token carrying exactly the supplied attributes.
pub fn token_with(attributes: &[(TokenAttributeIndex, &str)]) -> TokenInfo {
    let mut token = TokenInfo::default();
    for (attribute, value) in attributes {
        token.set(*attribute, (*value).into());
    }
    token
}

/// Typed metadata parsed from a JSON literal, or a panic naming the fixture.
pub fn metadata(json: serde_json::Value) -> Metadata {
    metadata_from_json(json, "test").expect("metadata types")
}
