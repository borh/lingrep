// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::unwrap_used)]
//! Fixture builders shared by more than one web test module.

use std::path::PathBuf;
use std::sync::Arc;

use lingrep_core::token_match::TokenInfo;
use lingrep_web::server_context::ServerContext;

/// A server context over `dict_path`, for tests that do not care which one
/// they get. Three runtime modules had their own copy.
pub fn test_context(dict_path: impl Into<PathBuf>) -> Arc<ServerContext> {
    Arc::new(ServerContext::new(dict_path).unwrap())
}

/// One token whose only attribute is its surface.
pub fn token(orth: &str) -> TokenInfo {
    TokenInfo::default().with_orth(orth)
}

/// One token with a surface and a part of speech.
pub fn tok(orth: &str, pos: &str) -> TokenInfo {
    TokenInfo::default().with_orth(orth).with_pos(pos)
}
