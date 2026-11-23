//! Raw-byte identity hashing, stdin observation, and ordered inventory checks.

use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender};

use blake3::Hasher;
use lingrep_core::corpus::DiscoveryReport;
use lingrep_core::execution::ExecutionError;
use lingrep_corpus::{IdentityError, SourceObservation};

/// A reader that observes the exact raw chunks consumed from one logical file.
pub struct HashingReader<R> {
    reader: R,
    observation: SourceObservation,
    ended: bool,
}

impl<R: Read> HashingReader<R> {
    pub fn new(
        reader: R,
        observation: SourceObservation,
        logical_name: &str,
    ) -> Result<Self, IdentityError> {
        observation.begin_source(logical_name)?;
        Ok(Self {
            reader,
            observation,
            ended: false,
        })
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return self.reader.read(buffer);
        }
        let read = self.reader.read(buffer)?;
        if read == 0 {
            if !self.ended {
                self.observation.end_source().map_err(identity_io_error)?;
                self.ended = true;
            }
        } else {
            self.observation
                .update_source_bytes(&buffer[..read])
                .map_err(identity_io_error)?;
        }
        Ok(read)
    }
}

/// Run-local observation of stdin bytes. It cannot produce a durable corpus
/// identity because stdin has no reopenable logical source name.
pub struct StdinObservation(Receiver<blake3::Hash>);

impl StdinObservation {
    /// Returns the digest only after an observed EOF.
    pub fn observed_content_digest(self) -> Option<String> {
        self.0
            .try_recv()
            .ok()
            .map(|digest| encode_digest(digest.as_bytes()))
    }
}

pub struct StdinReader<R> {
    reader: R,
    digest: Hasher,
    completion: Option<Sender<blake3::Hash>>,
}

impl<R: Read> StdinReader<R> {
    pub fn new(reader: R) -> (Self, StdinObservation) {
        let (sender, receiver) = mpsc::channel();
        (
            Self {
                reader,
                digest: Hasher::new(),
                completion: Some(sender),
            },
            StdinObservation(receiver),
        )
    }
}

impl<R: Read> Read for StdinReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return self.reader.read(buffer);
        }
        let read = self.reader.read(buffer)?;
        if read == 0 {
            if let Some(completion) = self.completion.take() {
                let _ = completion.send(self.digest.finalize());
            }
        } else {
            self.digest.update(&buffer[..read]);
        }
        Ok(read)
    }
}

#[derive(Default)]
pub struct OrderedInventoryObservation {
    digest: Hasher,
    documents: u64,
}

impl OrderedInventoryObservation {
    pub fn observe(&mut self, logical_name: &str) -> Result<(), IdentityError> {
        let length =
            u64::try_from(logical_name.len()).map_err(|_| IdentityError::LengthOverflow)?;
        self.documents = self
            .documents
            .checked_add(1)
            .ok_or(IdentityError::LengthOverflow)?;
        self.digest.update(&length.to_le_bytes());
        self.digest.update(logical_name.as_bytes());
        Ok(())
    }
}

/// Proves the sources a run actually read are the sources discovery found,
/// in the same order.
///
/// The two digests must be *independently* derived for this to mean
/// anything: `observed` accumulates during the read, and
/// `discovery.ordered_inventory_digest` comes from the traversal that
/// enumerated the inventory. Feeding it the observation's own digest turns
/// this into a tautology — see the BCCWJ arm of `FilesystemCorpusEvents`.
pub fn validate_ordered_inventory(
    observed: &OrderedInventoryObservation,
    discovery: &DiscoveryReport,
) -> Result<(), ExecutionError> {
    if observed.documents != discovery.inventory.documents
        || observed.digest.finalize().as_bytes() != &discovery.ordered_inventory_digest
    {
        Err(ExecutionError::InventoryMismatch)
    } else {
        Ok(())
    }
}

pub(crate) fn identity_io_error(error: IdentityError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

pub fn encode_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
