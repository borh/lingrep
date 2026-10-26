//! The Soranoha archive seam: open one bulk archive, hash it, and stream its
//! works.
//!
//! A Soranoha archive is one discovered source and one `SoranohaEvents`
//! stream, so the seam is a single function rather than a cursor: there is no
//! inventory to walk and no per-zip facts to carry. The direct query stream
//! and the prepared build both reach it through
//! [`crate::direct_source::open_corpus_document`].
//!
//! The archive is drained for its content observation before it is opened as
//! a container, and the file stat taken then is compared at end of stream, so
//! the bytes that were hashed are the bytes that were parsed. That check is
//! the same one the BCCWJ stream makes; a run that stops before the last entry
//! never reaches it, which is the accepted residual there and here.

use std::fs::File;
use std::io::{self, Seek};
use std::path::PathBuf;

use lingrep_core::corpus::SourceDocument;
use lingrep_core::corpus::soranoha::SoranohaEvents;
use lingrep_core::corpus::structured::CorpusEvent;
use lingrep_core::corpus::zip::{ZipFileStat, zip_file_stat};
use lingrep_corpus::SourceObservation;

use crate::direct_source::{HashingReader, OrderedInventoryObservation, identity_io_error};

/// Opens one archive's document stream on one handle: stat, hash for identity,
/// rewind, parse.
///
/// # Errors
///
/// The handle cannot be opened or stat-ed, the identity observation rejects
/// the source, or the archive is not a readable Soranoha archive.
pub(crate) fn open(
    source: &SourceObservation,
    inventory: Option<&OrderedInventoryObservation>,
    document: &SourceDocument,
    record_limit: usize,
    tei: bool,
) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
    let mut file = File::open(&document.path)?;
    let integrity = zip_file_stat(&file)?;
    if let Some(inventory) = inventory {
        inventory
            .observe(&document.logical_name)
            .map_err(identity_io_error)?;
    }
    // Drain the same handle that is parsed, for the content observation.
    let mut hasher = HashingReader::new(&file, source.clone(), &document.logical_name)
        .map_err(identity_io_error)?;
    io::copy(&mut hasher, &mut io::sink())?;
    file.rewind()?;
    let events = if tei {
        SoranohaEvents::new_tei(file, document.logical_name.clone(), record_limit)?
    } else {
        SoranohaEvents::new(file, document.logical_name.clone(), record_limit)?
    };
    Ok(Box::new(IntegrityChecked {
        events,
        path: document.path.clone(),
        name: document.logical_name.clone(),
        expected: integrity,
        verified: false,
    }))
}

/// One archive stream that refuses a file replaced or edited between its
/// identity hash and its parse.
struct IntegrityChecked {
    events: SoranohaEvents<File>,
    path: PathBuf,
    name: String,
    expected: ZipFileStat,
    verified: bool,
}

impl Iterator for IntegrityChecked {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(item) = self.events.next() {
            return Some(item);
        }
        if self.verified {
            return None;
        }
        self.verified = true;
        let after = File::open(&self.path).and_then(|file| zip_file_stat(&file));
        match after {
            Ok(after) if after == self.expected => None,
            Ok(_) => Some(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "soranoha archive {} changed during the run; refusing to publish",
                    self.name
                ),
            ))),
            Err(error) => Some(Err(io::Error::new(
                error.kind(),
                format!(
                    "soranoha archive {} could not be re-checked after the run: {error}",
                    self.name
                ),
            ))),
        }
    }
}
