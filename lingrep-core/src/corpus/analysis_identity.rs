//! Canonical semantic identity inputs shared by direct and prepared execution.

use super::{CorpusFormat, CorpusReadConfig};
use crate::token_match::TokenSchema;

const ADMISSION_DOMAIN: &[u8] = b"lingrep/sentence-admission/v2";
const READER_DOMAIN: &[u8] = b"lingrep/corpus-reader/v1";

/// Structural identity of a reader policy, **without** its normalization.
///
/// Not the value an artifact records. A build's reader identity also binds the
/// normalization policy, because that decides the bytes the reader admits, and
/// the function that composes the two is
/// `lingrep_corpus::ReaderIdentity::derive` — **call that**, not this.
///
/// This is a component of it, exposed because `lingrep-corpus` is a separate crate
/// and has to reach it. Calling it directly to name an artifact is exactly the
/// defect it caused once: `lingrep-web` did, `ling` used the composed value,
/// and the two addressed one JSONL root with two artifacts whose decoded
/// content was identical.
#[must_use]
pub fn reader_identity_digest(config: &CorpusReadConfig) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(READER_DOMAIN);
    digest.update(&config.reader_tag().to_le_bytes());
    digest.update(&[match config.format() {
        CorpusFormat::Plain => 1,
        CorpusFormat::Jsonl => 2,
        CorpusFormat::Markdown => 3,
        CorpusFormat::Bccwj => 4,
        CorpusFormat::Conllu => 5,
        CorpusFormat::Soranoha => 6,
    }]);
    *digest.finalize().as_bytes()
}

#[must_use]
pub fn admission_identity_digest() -> [u8; 32] {
    *blake3::hash(ADMISSION_DOMAIN).as_bytes()
}

/// Structural identity of the **Vibrato** morphology schema.
///
/// An identity pin, not a description of "this build's schema": every Vibrato
/// artifact on disk records this digest, so moving it invalidates all of them.
/// It is deliberately not the answer to "would this build reproduce that
/// artifact's tokens" — that question is per analyzer, and reading a single
/// constant for it reported every Sudachi artifact as stale. Ask
/// [`declared_token_schema`](crate::analysis::declared_token_schema) instead.
#[must_use]
pub fn token_schema_identity_digest() -> [u8; 32] {
    *TokenSchema::unidic_morphology().digest().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest every published V6 artifact records, pinned as a literal.
    ///
    /// The recomputation test below derives its expectation from the same
    /// constants the function reads, so it follows any encoding change rather
    /// than catching one. Only a literal can say "this refactor did not
    /// invalidate artifacts that are already on disk", which is a different
    /// claim and the one a reader of a refactor commit needs.
    ///
    /// Moving this value is legitimate only with a stored-schema change. The
    /// artifact generation is independent of this schema identity; older
    /// readable schemas remain readable but are not reusable as current
    /// analysis.
    #[test]
    fn the_published_token_schema_identity_is_unchanged() {
        assert_eq!(
            hex(&token_schema_identity_digest()),
            "0258eb71de9f5983c95b8a9fa9fc4bd23a883f0087a16cf26fd9a6f6b9eda7cc"
        );
    }

    fn hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
