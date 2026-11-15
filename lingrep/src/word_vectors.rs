//! The notices about the word-vector file a catalog's `near` tests were
//! answered from.

use std::path::PathBuf;

use lingrep_core::fsm::CompiledCatalog;
use lingrep_corpus::word_vectors::{VectorsBinding, vectors_binding};

/// The word-vector file that answered a catalog's `near` tests.
pub(crate) struct LoadedVectors {
    pub(crate) path: PathBuf,
    /// The BLAKE3 digest of the file, as lowercase hex.
    pub(crate) blake3: String,
}

/// The notice to publish about the table a catalog's `near` tests were
/// answered from, or `None` when a `vectors:` entry carries its digest.
///
/// `authored_file` is false for an inline `--pattern`, which has no file to
/// declare a table in and so gets no notice for declaring none.
pub(crate) fn binding_notice(
    matcher: &CompiledCatalog,
    loaded: &LoadedVectors,
    authored_file: bool,
) -> Option<(&'static str, String)> {
    match vectors_binding(matcher.vectors(), &loaded.path, &loaded.blake3) {
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
