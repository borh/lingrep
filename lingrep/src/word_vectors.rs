//! Reports undeclared or mismatched word-vector files for catalog `near` tests.

use std::path::PathBuf;

use lingrep_core::fsm::CompiledCatalog;
use lingrep_corpus::word_vectors::{VectorsBinding, vectors_binding};

/// The word-vector file used to resolve a catalog's `near` tests.
pub(crate) struct LoadedVectors {
    pub(crate) path: PathBuf,
    pub(crate) provenance: Option<lingrep_corpus::word_vectors::VectorProvenance>,
    /// The BLAKE3 digest of the file.
    pub(crate) blake3: blake3::Hash,
}

/// Returns a notice unless a `vectors:` entry declares the loaded file's digest.
///
/// `authored_file` is false for an inline `--pattern`, which has no file to
/// declare a table in, so undeclared-vector notices are omitted.
pub(crate) fn binding_notice(
    matcher: &CompiledCatalog,
    loaded: &LoadedVectors,
    authored_file: bool,
) -> Option<(&'static str, String)> {
    match vectors_binding(matcher.vectors(), &loaded.path, loaded.blake3.to_hex().as_str(), loaded.provenance.as_ref()) {
        VectorsBinding::Declared => None,
        VectorsBinding::Undeclared { declaration } => authored_file.then(|| {
            (
                "vectors_undeclared",
                format!(
                    "the catalog has no word-vector declaration; to declare {}, add `{declaration}`",
                    loaded.path.display(),
                ),
            )
        }),
        VectorsBinding::Mismatch { declared } => Some((
            "vectors_binding_mismatch",
            format!(
                "the catalog declares word vectors {}, but {} has BLAKE3 {}; \
                 matches may differ at the same threshold",
                declared
                    .iter()
                    .map(|(resource, blake3)| format!("`{resource}` (BLAKE3 {blake3})"))
                    .collect::<Vec<_>>()
                    .join(" or "),
                loaded.path.display(),
                loaded.blake3
            ),
        )),
    }
}
