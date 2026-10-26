//! BCCWJ distribution reader: the pure value layer behind the `bccwj` corpus
//! format.
//!
//! A BCCWJ distribution is a directory holding two trees — `C-XML/` (zipped
//! C-XML document entries) and `DOC/` (tab-separated context tables). The
//! reader is split into a value layer and a lowering:
//!
//! - [`xml::parse_entry`] parses one C-XML entry into [`xml::BccwjDocument`]
//!   values (paragraphs with tag sets and sentences). It is pure and total
//!   over well-formed entries: a `&[u8]` in, a value out, no I/O.
//! - [`metadata::BccwjMetadata`] is the join table over the `DOC/` context
//!   tables, built once per stream.
//! - [`discovery::BccwjInventory`] enumerates the zips and the context files
//!   in byte-sorted order, and [`discovery::BccwEvents`] lowers documents to
//!   [`super::CorpusEvent`]s over a caller-owned open handle.
//!
//! The `DOC/` context files are part of the *source identity* (they change
//! what the reader emits for every document), so the seams observe them
//! through `lingrep_corpus::SourceObservation`.

pub mod discovery;
pub mod metadata;
pub mod xml;

pub use crate::corpus::zip::{zip_entries, zip_entry_names, zip_file_stat};

/// The seam-facing reader values, re-exported so a consumer never reaches
/// into the module layout.
pub use discovery::{BccwEvents, BccwjInventory, BccwjMetadataLock, BccwjZip, basename_from_entry};
pub use metadata::{BccwjBuildNotes, BccwjMetadata, DocMetadata, Sampling, Subcorpus};
pub use xml::{BccwjDocument, parse_entry};

/// Opens the zip container over an already-open handle, under the same
/// container rules as the event stream.
///
/// The event stream opens its own container; this supports callers such as the
/// differential harness that list and read entries directly.
pub fn open_zip_archive(
    file: std::fs::File,
) -> std::io::Result<zip::ZipArchive<std::io::BufReader<std::fs::File>>> {
    zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|err| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bccwj zip: {err}"))
    })
}

use std::collections::BTreeMap;

/// The NDC9 classification table: notation → ordered label list.
///
/// # Provenance
///
/// Source: NDC9 (日本十進分類法第9版) Linked Data, produced by the JLA
/// classification committee and NDL joint research and published under
/// CC-BY (https://www.jla.or.jp/committees/bunrui/ndc-data/). It reached this
/// tree via the `corpus-utils` checkout's `data/ndc9.edn.xz` (itself derived
/// from the JLA `ndc9.ttl`). NDC9 is NDL/JLA's classification *scheme*, not
/// BCCWJ text, so it is checked in as a build artifact with stated
/// provenance rather than a runtime data dependency.
///
/// # Regeneration
///
/// ```text
/// xz -dc <corpus-utils>/data/ndc9.edn.xz \
///   | cargo run -p lingrep-core --example generate-ndc9-tsv -- - \
///       lingrep-core/src/corpus/bccwj/data/ndc9.tsv
/// ```
///
/// The generator (that example) is the validation: it refuses to write on
/// any malformed input (non-string values, duplicate or empty notations,
/// tab/newline in a field, or an empty label outside the four known
/// astronomy heading gaps 440.12–440.14 and 440.19). Pinned provenance of
/// the checked-in table:
///
/// | fact | value |
/// |---|---|
/// | source (decompressed EDN) BLAKE3 | `19bd146ee793b346284990f7a888aa0809579377a1815fdc61d0c8983eaf9295` |
/// | output TSV BLAKE3 | `4068ab3c01d08c2366e5b837790f3691cd96598fcda77b38069d7c58865bbb0f` |
/// | rows | 12,387 |
/// | labels | 46,232 |
///
/// A regeneration that changes any of those numbers is a visible diff, not a
/// silent re-derivation.
static NDC9_TSV: &str = include_str!("data/ndc9.tsv");

/// The parsed NDC9 table, loaded once. The generator validates every row
/// (one notation, one or more labels, no tabs or newlines, empty labels
/// limited to the known heading gaps), so parsing cannot fail at run time;
/// the defensive `if let` keeps this total without a panic path. Empty
/// labels are preserved verbatim: the reference map carries them, and a
/// genre value that equals a gap notation would otherwise derive a
/// different `category`.
static NDC9: std::sync::LazyLock<BTreeMap<String, Vec<String>>> = std::sync::LazyLock::new(|| {
    NDC9_TSV
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let notation = fields.next()?;
            let labels = fields.map(String::from).collect();
            (!notation.is_empty()).then_some((notation.to_owned(), labels))
        })
        .collect()
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_in_table_matches_the_pinned_shape() {
        assert_eq!(NDC9.len(), 12_387);
        // The plan's spot checks: 913 expands to three labels, 91 to one.
        assert_eq!(NDC9["913"], vec!["日本文学", "小説", "物語"]);
        assert_eq!(NDC9["91"], vec!["日本文学（綱目）"]);
        // A known heading gap carries an empty label verbatim (five fields,
        // the third empty), matching the reference map exactly.
        assert_eq!(NDC9["440.14"].len(), 5);
        assert!(NDC9["440.14"][2].is_empty());
    }
}
