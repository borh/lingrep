//! Raw-byte identity hashing, stdin observation, and ordered inventory checks.

use std::io::{self, Read};
use std::sync::{Arc, Mutex};

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
#[derive(Clone, Default)]
pub struct StdinObservation {
    state: Arc<Mutex<StdinObservationState>>,
}

#[derive(Clone, Default)]
struct StdinObservationState {
    digest: Hasher,
    eof: bool,
}

impl StdinObservation {
    pub fn reader<R: Read>(&self, reader: R) -> StdinReader<R> {
        StdinReader {
            reader,
            observation: self.clone(),
        }
    }

    /// Returns the digest only after an observed EOF.
    pub fn observed_content_digest(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        state
            .eof
            .then(|| encode_digest(state.digest.clone().finalize().as_bytes()))
    }
}

pub struct StdinReader<R> {
    reader: R,
    observation: StdinObservation,
}

impl<R: Read> Read for StdinReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return self.reader.read(buffer);
        }
        let read = self.reader.read(buffer)?;
        let mut state = self
            .observation
            .state
            .lock()
            .map_err(|_| io::Error::other("stdin observation lock poisoned"))?;
        if read == 0 {
            state.eof = true;
        } else {
            state.digest.update(&buffer[..read]);
        }
        Ok(read)
    }
}

#[derive(Clone, Default)]
pub struct OrderedInventoryObservation {
    state: Arc<Mutex<(blake3::Hasher, u64)>>,
}

impl OrderedInventoryObservation {
    pub fn observe(&self, logical_name: &str) -> Result<(), IdentityError> {
        let length =
            u64::try_from(logical_name.len()).map_err(|_| IdentityError::LengthOverflow)?;
        let mut state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        state.0.update(&length.to_le_bytes());
        state.0.update(logical_name.as_bytes());
        state.1 = state
            .1
            .checked_add(1)
            .ok_or(IdentityError::LengthOverflow)?;
        Ok(())
    }

    fn snapshot(&self) -> Result<([u8; 32], u64), IdentityError> {
        let state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        Ok((*state.0.clone().finalize().as_bytes(), state.1))
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
    let (digest, documents) = observed
        .snapshot()
        .map_err(|_| ExecutionError::ResourceAccounting)?;
    if documents != discovery.inventory.documents || digest != discovery.ordered_inventory_digest {
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
