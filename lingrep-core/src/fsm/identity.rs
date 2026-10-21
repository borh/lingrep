//! Public identities that bind compiled lingrep patterns semantics across applications.

/// Versions how tree-sitter-lingrep parses catalog source that already parses.
///
/// Catalog fingerprints include this value rather than the grammar's Git
/// revision, so a grammar bump that only adds syntax keeps every existing
/// fingerprint. Advance it when a bump changes the compiled program of existing
/// source, which `repository_catalog_fingerprints_are_stable_across_cst_normalization`
/// reports for the tracked catalogs. The value is the grammar revision lingrep
/// 0.3.0 shipped, so fingerprints recorded since then stay equal.
pub const GRAMMAR_SEMANTICS_REVISION: &str = "b7ac30d8055e0e4e22ce4e4a34711f27f07712fc";

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
