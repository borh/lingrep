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

/// File entry names in byte-sorted order, excluding directory entries.
///
/// # Errors
///
/// Returns invalid data when names cannot be decoded or decoded names collide.
pub fn zip_entry_names(zip: &ZipArchive<impl Read + Seek>) -> io::Result<Vec<String>> {
    Ok(zip_entries(zip)?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// File entry names with their archive indices, sorted by name bytes.
/// Directory entries are excluded.
///
/// # Errors
///
/// Returns invalid data when names cannot be decoded or decoded names collide.
pub fn zip_entries(zip: &ZipArchive<impl Read + Seek>) -> io::Result<Vec<(String, usize)>> {
    let mut entries = Vec::new();
    for index in 0..zip.len() {
        let name = zip
            .name_for_index(index)
            .expect("index is within archive bounds")
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !name.ends_with('/') {
            entries.push((name.into_owned(), index));
        }
    }
    entries.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ZIP entries have duplicate decoded names",
        ));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn legacy_archive(duplicate: bool) -> ZipArchive<Cursor<Vec<u8>>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        if duplicate {
            writer
                .start_file("é.txt", SimpleFileOptions::default())
                .expect("UTF-8 name");
        }
        writer
            .start_file("x.txt", SimpleFileOptions::default())
            .expect("legacy name");
        writer.write_all(b"entry").expect("entry bytes");
        let mut bytes = writer.finish().expect("archive").into_inner();
        for index in 0..=bytes.len() - b"x.txt".len() {
            if bytes[index..].starts_with(b"x.txt") {
                bytes[index] = 0x82;
            }
        }
        ZipArchive::new(Cursor::new(bytes)).expect("read archive")
    }

    #[test]
    fn legacy_encoded_entries_preserve_names_and_indices() {
        let mut archive = legacy_archive(false);
        let entries = zip_entries(&archive).expect("decoded entry");
        assert_eq!(entries, [("é.txt".to_owned(), 0)]);
        let mut text = String::new();
        archive
            .by_index(entries[0].1)
            .expect("entry index")
            .read_to_string(&mut text)
            .expect("entry text");
        assert_eq!(text, "entry");
    }

    #[test]
    fn decoded_entry_name_collision_returns_invalid_data() {
        let error = zip_entries(&legacy_archive(true)).expect_err("ambiguous decoded names");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
