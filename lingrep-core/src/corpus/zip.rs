//! ZIP container values shared by the archive readers.
//!
//! Two formats ship as ZIP archives and need the same two facts about a
//! container: which entries it holds, in one deterministic order, and whether
//! the file that was hashed is still the file being parsed. Both derivations
//! live here so the BCCWJ and Soranoha readers cannot disagree about either.

use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::os::unix::fs::MetadataExt;

use zip::ZipArchive;

/// The `(dev, ino, len, mtime)` identity of one ZIP file — the discovery
/// `(dev, ino)` file identity extended with the fields an in-place edit
/// changes. Compared at stream end to refuse an archive that is not
/// byte-stable between its identity hash and its parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipFileStat {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
}

pub(crate) fn zip_file_stat_from_metadata(metadata: &fs::Metadata) -> ZipFileStat {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default();
    ZipFileStat {
        dev: metadata.dev(),
        ino: metadata.ino(),
        len: metadata.len(),
        mtime_secs: mtime.as_secs() as i64,
        mtime_nanos: mtime.subsec_nanos(),
    }
}

/// Stat one open handle for the ZIP-integrity comparison.
///
/// # Errors
///
/// When the handle cannot be stat-ed.
pub fn zip_file_stat(file: &File) -> io::Result<ZipFileStat> {
    Ok(zip_file_stat_from_metadata(&file.metadata()?))
}

/// The file entries of one ZIP as byte-sorted names (directory entries
/// dropped). The one derivation of entry order, so the event stream and the
/// differential harness cannot disagree about which entry is which.
#[must_use]
pub fn zip_entry_names(zip: &ZipArchive<impl Read + Seek>) -> Vec<String> {
    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|index| zip.name_for_index(index).map(str::to_owned))
        // Directory entries (present in the BCCWJ VARIABLE zips) are skipped;
        // a file entry never ends with a separator.
        .filter(|name| !name.ends_with('/'))
        .collect();
    names.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    names
}
