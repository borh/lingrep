//! Public identities that bind compiled lingrep patterns semantics across applications.

/// Exact tree-sitter-lingrep revision used by the workspace compiler.
///
/// Catalog identities must include this value so a grammar change cannot
/// retain the same semantic fingerprint.
pub const GRAMMAR_REVISION: &str = "b7ac30d8055e0e4e22ce4e4a34711f27f07712fc";

/// Versions catalog lowering and compilation semantics.
///
/// Advance this identity when a compiler change must invalidate catalog
/// fingerprints while source and grammar remain fixed. The fresh lingrep
/// namespace starts at version 1.
pub const COMPILER_SCHEMA_IDENTITY: &str = "lingrep-core-compiler/v1";

/// Versions publication-policy vocabulary and execution semantics.
///
/// Advance this identity when policy changes can alter published records.
/// Descriptive statement labels are excluded because they change no decision.
pub const POLICY_SEMANTICS_IDENTITY: &str = "lingrep-core-policy/v1";
