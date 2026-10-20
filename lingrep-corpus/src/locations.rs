use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use lingrep_core::spill::TempBudget;
use memmap2::{Mmap, MmapOptions};

use crate::{
    AnalysisIdentity, CorpusError, DocumentId, SentenceLocation, scratch::BudgetedScratch,
};

const COMPONENT: &str = "locations.v1";
const LOCATION_MAGIC: &[u8; 8] = b"LINGLOC1";
const LOCATION_SCHEMA_VERSION: u32 = 1;
const HEADER_LEN: u64 = 128;
const RECORD_LEN: u64 = 24;
const BLOCK_ENTRY_LEN: u64 = 56;
const DOCUMENT_ENTRY_LEN: u64 = 56;
const RECORDS_PER_BLOCK: u64 = 1024;
/// The buffered size of one complete checksum block.
const BLOCK_LEN: usize = (RECORDS_PER_BLOCK * RECORD_LEN) as usize;
const DOCUMENT_SCRATCH_HEADER_LEN: usize = 48;

/// Streaming writer for the authoritative dense sentence-location relation.
pub(crate) struct LocationWriter {
    file: File,
    sentence_count: u64,
    document_count: u64,
    distinct_targets: u64,
    last_document_id: Option<u64>,
    active_location_document: Option<DocumentEntry>,
    next_location_document: u64,
    next_location_document_offset: u64,
    documents: BudgetedScratch,
    target_counts: BudgetedScratch,
    blocks: BudgetedScratch,
    block_count: u64,
    /// The records of the block being filled, held until it is complete.
    ///
    /// Writing each 24-byte record directly costs one `write(2)` per sentence,
    /// measured at 2.65 µs each: 1.39 s of a `corpus build` over 523,883
    /// sentences. A block is the checksum unit already, so buffering one
    /// costs 24 KiB and makes both the write and the hash happen once per 1024
    /// records instead of once per record. The bytes and the checksum are unchanged: BLAKE3
    /// over a concatenation is the same value as BLAKE3 fed the pieces.
    current_block: Vec<u8>,
}

#[derive(Clone)]
struct DocumentEntry {
    document_id: u64,
    document_ordinal: u64,
    source_name: Arc<str>,
}

/// One document stored in the dense location directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocationDocument {
    pub(crate) document_id: DocumentId,
    pub(crate) document_ordinal: u64,
    pub(crate) source_name: String,
    pub(crate) target_group_ordinal: u64,
    pub(crate) target_multiplicity: u64,
    pub(crate) external_target: bool,
}

struct BlockEntry {
    start: u64,
    record_offset: u64,
    record_count: u64,
    checksum: [u8; 32],
}

/// Memory-mapped, bounded random-access view of `locations.v1`.
pub(crate) struct MappedLocations {
    map: Mmap,
    sentence_count: u64,
    document_count: u64,
    blocks_offset: u64,
    documents_offset: u64,
    strings_offset: u64,
    directory_checksum: [u8; 32],
    /// One bit per block, set once that block's payload checksum has been
    /// verified by this mapping.
    ///
    /// [`Self::sentence`] is called once per sentence and a block holds 1024 of
    /// them, so checking the payload on every call hashed each block 1024 times
    /// over. The guarantee is per block and does not become stronger by being
    /// re-established: a block is verified before any record is read from it,
    /// and every block a run touches is still verified. This is the same
    /// boundary the token side draws, where `preflight_block` checks each block
    /// a run admits from, once.
    ///
    /// Relaxed ordering is sufficient because the bit guards no data. A racing
    /// pair of readers may both verify the same block, which is idempotent, and
    /// a stale `false` costs a redundant hash rather than an unverified read.
    verified_blocks: Box<[AtomicU64]>,
}

impl LocationWriter {
    /// Creates a new location component, writing records as they arrive.
    pub(crate) fn create(path: &Path, temp_budget: Arc<TempBudget>) -> Result<Self, CorpusError> {
        let documents =
            BudgetedScratch::create(&scratch_path(path, ".documents.scratch"), &temp_budget)?;
        let target_counts =
            BudgetedScratch::create(&scratch_path(path, ".targets.scratch"), &temp_budget)?;
        let blocks = BudgetedScratch::create(&scratch_path(path, ".blocks.scratch"), &temp_budget)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    CorpusError::DestinationExists {
                        path: path.to_path_buf(),
                    }
                } else {
                    CorpusError::Io(error)
                }
            })?;
        file.write_all(&[0; HEADER_LEN as usize])?;
        Ok(Self {
            file,
            sentence_count: 0,
            document_count: 0,
            distinct_targets: 0,
            last_document_id: None,
            active_location_document: None,
            next_location_document: 0,
            next_location_document_offset: 0,
            documents,
            target_counts,
            blocks,
            block_count: 0,
            current_block: Vec::with_capacity(BLOCK_LEN),
        })
    }

    pub(crate) fn register_document_unresolved(
        &mut self,
        document_id: DocumentId,
        document_ordinal: u64,
        source_name: Arc<str>,
        target_group_ordinal: u64,
        external_target: bool,
    ) -> Result<u64, CorpusError> {
        self.register_document_inner(
            document_id,
            document_ordinal,
            source_name,
            target_group_ordinal,
            None,
            external_target,
        )
    }

    fn register_document_inner(
        &mut self,
        document_id: DocumentId,
        document_ordinal: u64,
        source_name: Arc<str>,
        target_group_ordinal: u64,
        declared_multiplicity: Option<u64>,
        external_target: bool,
    ) -> Result<u64, CorpusError> {
        if document_ordinal != self.document_count {
            return Err(invalid(
                "document ordinals must be contiguous and zero-based",
            ));
        }
        let raw_id = document_id.get();
        if self
            .last_document_id
            .is_some_and(|last_id| last_id >= raw_id)
        {
            return Err(invalid("document IDs must increase with document ordinal"));
        }
        let name_len = checked_u64(source_name.len())?;
        let target_multiplicity = if target_group_ordinal == self.distinct_targets {
            self.target_counts.seek(SeekFrom::End(0))?;
            self.target_counts.append(&1_u64.to_le_bytes())?;
            self.distinct_targets = checked_add(self.distinct_targets, 1)?;
            1
        } else if target_group_ordinal < self.distinct_targets {
            let offset = checked_mul(target_group_ordinal, 8)?;
            self.target_counts.seek(SeekFrom::Start(offset))?;
            let mut bytes = [0_u8; 8];
            self.target_counts.read_exact(&mut bytes)?;
            let count = checked_add(u64::from_le_bytes(bytes), 1)?;
            self.target_counts.seek(SeekFrom::Start(offset))?;
            self.target_counts.write_existing(&count.to_le_bytes())?;
            count
        } else {
            return Err(invalid(
                "first-seen target group ordinals must be contiguous and zero-based",
            ));
        };
        let mut header = [0_u8; DOCUMENT_SCRATCH_HEADER_LEN];
        put_u64(&mut header, 0, raw_id);
        put_u64(&mut header, 8, document_ordinal);
        put_u64(&mut header, 16, target_group_ordinal);
        put_u64(&mut header, 24, declared_multiplicity.unwrap_or(0));
        put_u64(&mut header, 32, name_len);
        header[40] = u8::from(external_target);
        self.documents.seek(SeekFrom::End(0))?;
        self.documents.append(&header)?;
        self.documents.append(source_name.as_bytes())?;
        self.document_count = checked_add(self.document_count, 1)?;
        self.last_document_id = Some(raw_id);
        Ok(target_multiplicity)
    }

    /// Appends the next contiguous corpus sentence location.
    pub(crate) fn append(&mut self, location: &SentenceLocation) -> Result<(), CorpusError> {
        if location.corpus_sentence_ordinal != self.sentence_count {
            return Err(invalid(
                "sentence ordinals must be contiguous and zero-based",
            ));
        }
        let document_id = location.document_id.get();
        if location.document_ordinal == self.document_count {
            let _ = self.register_document_inner(
                location.document_id,
                location.document_ordinal,
                Arc::clone(&location.source_name),
                location.document_ordinal,
                None,
                false,
            )?;
        }
        let document_matches = self
            .active_location_document
            .as_ref()
            .is_some_and(|document| {
                document.document_id == document_id
                    && document.document_ordinal == location.document_ordinal
                    && document.source_name.as_ref() == location.source_name.as_ref()
            });
        if !document_matches {
            self.activate_location_document(location)?;
        }

        let mut record = [0; RECORD_LEN as usize];
        put_u64(&mut record, 0, document_id);
        put_u64(&mut record, 8, location.document_ordinal);
        put_u64(&mut record, 16, location.document_sentence_ordinal);
        self.current_block.extend_from_slice(&record);
        self.sentence_count = self
            .sentence_count
            .checked_add(1)
            .ok_or_else(|| invalid("sentence count overflow"))?;

        if self.current_block.len() == BLOCK_LEN {
            self.finish_block()?;
        }
        Ok(())
    }

    fn activate_location_document(
        &mut self,
        location: &SentenceLocation,
    ) -> Result<(), CorpusError> {
        if location.document_ordinal < self.next_location_document
            || location.document_ordinal >= self.document_count
        {
            return Err(invalid(
                "sentence document order differs from registered document order",
            ));
        }
        self.documents
            .seek(SeekFrom::Start(self.next_location_document_offset))?;
        let mut header = [0_u8; DOCUMENT_SCRATCH_HEADER_LEN];
        let mut buffer = [0_u8; 8192];
        while self.next_location_document <= location.document_ordinal {
            self.documents.read_exact(&mut header)?;
            let document_id = read_u64(&header, 0)?;
            let document_ordinal = read_u64(&header, 8)?;
            let name_len = read_u64(&header, 32)?;
            if document_ordinal != self.next_location_document {
                return Err(invalid(
                    "registered document scratch order is non-canonical",
                ));
            }
            if document_ordinal == location.document_ordinal {
                if document_id != location.document_id.get()
                    || name_len != checked_u64(location.source_name.len())?
                {
                    return Err(invalid("one document ID has conflicting location metadata"));
                }
                let mut compared = 0_usize;
                while compared < location.source_name.len() {
                    let length = (location.source_name.len() - compared).min(buffer.len());
                    self.documents.read_exact(&mut buffer[..length])?;
                    if buffer[..length]
                        != location.source_name.as_bytes()[compared..compared + length]
                    {
                        return Err(invalid("one document ID has conflicting location metadata"));
                    }
                    compared += length;
                }
                self.active_location_document = Some(DocumentEntry {
                    document_id,
                    document_ordinal,
                    source_name: Arc::clone(&location.source_name),
                });
            } else {
                self.documents.seek(SeekFrom::Current(
                    i64::try_from(name_len)
                        .map_err(|_| invalid("document name is too large to seek"))?,
                ))?;
            }
            self.next_location_document_offset = checked_add(
                self.next_location_document_offset,
                checked_add(DOCUMENT_SCRATCH_HEADER_LEN as u64, name_len)?,
            )?;
            self.next_location_document = checked_add(self.next_location_document, 1)?;
        }
        if self
            .active_location_document
            .as_ref()
            .is_none_or(|document| document.document_ordinal != location.document_ordinal)
        {
            return Err(invalid(
                "sentence document order differs from registered document order",
            ));
        }
        Ok(())
    }

    /// Finalizes the location table and its header.
    pub(crate) fn finish(mut self, analysis: AnalysisIdentity) -> Result<(), CorpusError> {
        self.finish_block()?;
        let blocks_offset = self.file.stream_position()?;
        let mut directory = blake3::Hasher::new();
        self.blocks.sync_all()?;
        self.blocks.rewind()?;
        let mut block_bytes = [0_u8; BLOCK_ENTRY_LEN as usize];
        for _ in 0..self.block_count {
            self.blocks.read_exact(&mut block_bytes)?;
            let bytes = block_bytes;
            self.file.write_all(&bytes)?;
            directory.update(&bytes);
        }

        let documents_offset = self.file.stream_position()?;
        let documents_len = checked_mul(self.document_count, DOCUMENT_ENTRY_LEN)?;
        let strings_offset = checked_add(documents_offset, documents_len)?;
        let mut next_name_offset = strings_offset;
        self.documents.sync_all()?;
        self.target_counts.sync_all()?;
        self.documents.rewind()?;
        let mut scratch_header = [0_u8; DOCUMENT_SCRATCH_HEADER_LEN];
        for _ in 0..self.document_count {
            self.documents.read_exact(&mut scratch_header)?;
            let document_id = read_u64(&scratch_header, 0)?;
            let document_ordinal = read_u64(&scratch_header, 8)?;
            let target_group_ordinal = read_u64(&scratch_header, 16)?;
            let declared_multiplicity = read_u64(&scratch_header, 24)?;
            let name_len = read_u64(&scratch_header, 32)?;
            let external_target = scratch_header[40];
            self.target_counts
                .seek(SeekFrom::Start(checked_mul(target_group_ordinal, 8)?))?;
            let mut count_bytes = [0_u8; 8];
            self.target_counts.read_exact(&mut count_bytes)?;
            let target_multiplicity = u64::from_le_bytes(count_bytes);
            if declared_multiplicity != 0 && declared_multiplicity != target_multiplicity {
                return Err(invalid(
                    "declared target multiplicity differs from observed documents",
                ));
            }
            let bytes = encode_document_entry(
                document_id,
                document_ordinal,
                next_name_offset,
                name_len,
                target_group_ordinal,
                target_multiplicity,
                match external_target {
                    0 => false,
                    1 => true,
                    _ => return Err(invalid("document external-target flag is invalid")),
                },
            );
            self.file.write_all(&bytes)?;
            directory.update(&bytes);
            self.documents.seek(SeekFrom::Current(
                i64::try_from(name_len)
                    .map_err(|_| invalid("document name is too large to seek"))?,
            ))?;
            next_name_offset = checked_add(next_name_offset, name_len)?;
        }
        self.documents.rewind()?;
        let mut name_buffer = [0_u8; 8192];
        for _ in 0..self.document_count {
            self.documents.read_exact(&mut scratch_header)?;
            let mut remaining = read_u64(&scratch_header, 32)?;
            while remaining > 0 {
                let length = usize::try_from(remaining.min(name_buffer.len() as u64))
                    .expect("bounded name chunk");
                self.documents.read_exact(&mut name_buffer[..length])?;
                self.file.write_all(&name_buffer[..length])?;
                directory.update(&name_buffer[..length]);
                remaining -= length as u64;
            }
        }

        let directory_checksum = *directory.finalize().as_bytes();
        let header = encode_header(
            analysis,
            self.sentence_count,
            self.document_count,
            HEADER_LEN,
            blocks_offset,
            documents_offset,
            strings_offset,
            directory_checksum,
        );
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Writes the buffered block and records its directory entry.
    fn finish_block(&mut self) -> Result<(), CorpusError> {
        if self.current_block.is_empty() {
            return Ok(());
        }
        let record_count = checked_u64(self.current_block.len())? / RECORD_LEN;
        let start = self
            .sentence_count
            .checked_sub(record_count)
            .ok_or_else(|| invalid("block start underflow"))?;
        let records_len = checked_mul(start, RECORD_LEN)?;
        let block = BlockEntry {
            start,
            record_offset: checked_add(HEADER_LEN, records_len)?,
            record_count,
            checksum: *blake3::hash(&self.current_block).as_bytes(),
        };
        self.file.write_all(&self.current_block)?;
        self.blocks.append(&encode_block_entry(&block))?;
        self.block_count = checked_add(self.block_count, 1)?;
        self.current_block.clear();
        Ok(())
    }
}

impl MappedLocations {
    /// Opens a location relation after constant-size header and extent validation.
    pub(crate) fn open(
        path: &Path,
        expected_analysis: &AnalysisIdentity,
    ) -> Result<Self, CorpusError> {
        let file = File::open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < HEADER_LEN {
            return Err(truncated(file_len));
        }
        // The file descriptor remains owned by the mapping after this call; no caller data is
        // dereferenced without a prior checked range validation.
        let map = unsafe { MmapOptions::new().map(&file)? };
        if &map[..8] != LOCATION_MAGIC {
            return Err(invalid("wrong magic"));
        }
        let version = read_u32(&map, 8)?;
        if version != LOCATION_SCHEMA_VERSION {
            return Err(CorpusError::UnsupportedSchema {
                component: COMPONENT,
                version,
            });
        }
        if read_u32(&map, 12)? != HEADER_LEN as u32 {
            return Err(invalid("wrong header length"));
        }
        let analysis = AnalysisIdentity::from_bytes(read_array(&map, 16)?);
        if &analysis != expected_analysis {
            return Err(CorpusError::VerificationMismatch {
                component: COMPONENT,
                sentence_ordinal: None,
                message: "analysis identity differs from the expected artifact identity".into(),
            });
        }
        let sentence_count = read_u64(&map, 48)?;
        let document_count = read_u64(&map, 56)?;
        let records_offset = read_u64(&map, 64)?;
        let blocks_offset = read_u64(&map, 72)?;
        let documents_offset = read_u64(&map, 80)?;
        let strings_offset = read_u64(&map, 88)?;
        let directory_checksum = read_array(&map, 96)?;
        let expected_blocks_offset =
            checked_add(HEADER_LEN, checked_mul(sentence_count, RECORD_LEN)?)?;
        let block_count = block_count(sentence_count)?;
        let expected_documents_offset = checked_add(
            expected_blocks_offset,
            checked_mul(block_count, BLOCK_ENTRY_LEN)?,
        )?;
        let expected_strings_offset = checked_add(
            expected_documents_offset,
            checked_mul(document_count, DOCUMENT_ENTRY_LEN)?,
        )?;
        if records_offset != HEADER_LEN
            || blocks_offset != expected_blocks_offset
            || documents_offset != expected_documents_offset
            || strings_offset != expected_strings_offset
        {
            return Err(invalid("non-canonical section offsets"));
        }
        ensure_range(
            file_len,
            records_offset,
            checked_mul(sentence_count, RECORD_LEN)?,
        )?;
        ensure_range(
            file_len,
            blocks_offset,
            checked_mul(block_count, BLOCK_ENTRY_LEN)?,
        )?;
        ensure_range(
            file_len,
            documents_offset,
            checked_mul(document_count, DOCUMENT_ENTRY_LEN)?,
        )?;
        ensure_range(file_len, strings_offset, 0)?;

        let words = usize::try_from(block_count.div_ceil(64))
            .map_err(|_| invalid("location block count overflow"))?;
        Ok(Self {
            map,
            sentence_count,
            document_count,
            blocks_offset,
            documents_offset,
            strings_offset,
            directory_checksum,
            verified_blocks: (0..words).map(|_| AtomicU64::new(0)).collect(),
        })
    }

    /// Returns the number of addressable sentences.
    pub(crate) fn sentence_count(&self) -> u64 {
        self.sentence_count
    }

    /// Returns the number of documents in the dense document table.
    pub(crate) fn document_count(&self) -> u64 {
        self.document_count
    }

    /// Reports whether this mapping has already checked `block_ordinal`.
    ///
    /// A block ordinal past the bitmap reads as unverified, so an out-of-range
    /// value costs a hash rather than skipping one.
    fn block_already_verified(&self, block_ordinal: u64) -> bool {
        let index = usize::try_from(block_ordinal / 64).ok();
        let bit = 1_u64 << (block_ordinal % 64);
        index
            .and_then(|index| self.verified_blocks.get(index))
            .is_some_and(|word| word.load(Ordering::Relaxed) & bit != 0)
    }

    /// Records that `block_ordinal` passed its payload checksum.
    fn mark_block_verified(&self, block_ordinal: u64) {
        let bit = 1_u64 << (block_ordinal % 64);
        if let Some(word) = usize::try_from(block_ordinal / 64)
            .ok()
            .and_then(|index| self.verified_blocks.get(index))
        {
            word.fetch_or(bit, Ordering::Relaxed);
        }
    }

    /// Returns one authoritative sentence location by corpus ordinal.
    pub(crate) fn sentence(&self, ordinal: u64) -> Result<SentenceLocation, CorpusError> {
        if ordinal >= self.sentence_count {
            return Err(invalid("sentence ordinal is outside the dense relation"));
        }
        let block_ordinal = ordinal / RECORDS_PER_BLOCK;
        let block = self.block(block_ordinal)?;
        let expected_count = (self.sentence_count - block.start).min(RECORDS_PER_BLOCK);
        if block.start != block_ordinal * RECORDS_PER_BLOCK
            || block.record_count != expected_count
            || block.record_offset
                != checked_add(HEADER_LEN, checked_mul(block.start, RECORD_LEN)?)?
        {
            return Err(invalid("invalid block directory entry"));
        }
        let payload_len = checked_mul(block.record_count, RECORD_LEN)?;
        let payload = self.range(block.record_offset, payload_len)?;
        if !self.block_already_verified(block_ordinal) {
            let checksum = *blake3::hash(payload).as_bytes();
            if checksum != block.checksum {
                return Err(CorpusError::VerificationMismatch {
                    component: COMPONENT,
                    sentence_ordinal: Some(ordinal),
                    message: "location block checksum mismatch".into(),
                });
            }
            self.mark_block_verified(block_ordinal);
        }
        let record_offset = checked_mul(ordinal - block.start, RECORD_LEN)?;
        let record = checked_subslice(payload, record_offset, RECORD_LEN)?;
        let document_id = DocumentId::new(read_u64(record, 0)?);
        let document_ordinal = read_u64(record, 8)?;
        let document_sentence_ordinal = read_u64(record, 16)?;
        let source_name = Arc::from(self.document_name(document_id)?);
        Ok(SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: ordinal,
            document_id,
            document_ordinal,
            document_sentence_ordinal,
            source_name,
        })
    }

    /// Looks up a logical document name by its stable document ID.
    pub(crate) fn document_name(&self, document_id: DocumentId) -> Result<&str, CorpusError> {
        let entry = self.document_entry_for_id(document_id)?;
        if entry.name_offset < self.strings_offset {
            return Err(invalid("document name points before string data"));
        }
        let bytes = self.range(entry.name_offset, entry.name_len)?;
        std::str::from_utf8(bytes).map_err(|_| invalid("document name is not UTF-8"))
    }

    /// Returns one stored document by zero-based document ordinal.
    pub(crate) fn document(&self, ordinal: u64) -> Result<LocationDocument, CorpusError> {
        if ordinal >= self.document_count {
            return Err(invalid("document ordinal is outside the dense relation"));
        }
        let entry = self.document_entry(ordinal)?;
        if entry.document_ordinal != ordinal {
            return Err(invalid(
                "document ordinals are not contiguous and zero-based",
            ));
        }
        let source_name = std::str::from_utf8(self.range(entry.name_offset, entry.name_len)?)
            .map_err(|_| invalid("document name is not UTF-8"))?
            .to_owned();
        Ok(LocationDocument {
            document_id: DocumentId::new(entry.document_id),
            document_ordinal: entry.document_ordinal,
            source_name,
            target_group_ordinal: entry.target_group_ordinal,
            target_multiplicity: entry.target_multiplicity,
            external_target: match entry.external_target {
                0 => false,
                1 => true,
                _ => return Err(invalid("document external-target flag is invalid")),
            },
        })
    }

    /// Fully verifies directory, source-name, and lazy block checksums.
    ///
    /// Unlike [`Self::open`], this scans the mapped relation and is therefore intended for
    /// explicit artifact verification rather than the matching hot path.
    pub(crate) fn verify_integrity(&self) -> Result<(), CorpusError> {
        let file_len = checked_u64(self.map.len())?;
        let directory = self.range(
            self.blocks_offset,
            checked_sub(file_len, self.blocks_offset)?,
        )?;
        let checksum = *blake3::hash(directory).as_bytes();
        if checksum != self.directory_checksum {
            return Err(CorpusError::VerificationMismatch {
                component: COMPONENT,
                sentence_ordinal: None,
                message: "location directory checksum mismatch".into(),
            });
        }

        self.verify_document_table(file_len)?;
        for block_ordinal in 0..block_count(self.sentence_count)? {
            let block = self.block(block_ordinal)?;
            let expected_start = checked_mul(block_ordinal, RECORDS_PER_BLOCK)?;
            let expected_count = (self.sentence_count - expected_start).min(RECORDS_PER_BLOCK);
            let expected_offset =
                checked_add(HEADER_LEN, checked_mul(expected_start, RECORD_LEN)?)?;
            if block.start != expected_start
                || block.record_count != expected_count
                || block.record_offset != expected_offset
            {
                return Err(invalid("invalid block directory entry"));
            }
            let payload = self.range(
                block.record_offset,
                checked_mul(block.record_count, RECORD_LEN)?,
            )?;
            let payload_checksum = *blake3::hash(payload).as_bytes();
            if payload_checksum != block.checksum {
                return Err(CorpusError::VerificationMismatch {
                    component: COMPONENT,
                    sentence_ordinal: Some(block.start),
                    message: "location block checksum mismatch".into(),
                });
            }
            for index in 0..block.record_count {
                let record =
                    checked_subslice(payload, checked_mul(index, RECORD_LEN)?, RECORD_LEN)?;
                let document_id = DocumentId::new(read_u64(record, 0)?);
                let document = self.document_entry_for_id(document_id)?;
                if read_u64(record, 8)? != document.document_ordinal {
                    return Err(invalid(
                        "sentence document ordinal differs from document table",
                    ));
                }
            }
        }
        Ok(())
    }

    fn verify_document_table(&self, file_len: u64) -> Result<(), CorpusError> {
        let mut previous_id = None;
        let mut expected_name_offset = self.strings_offset;
        for index in 0..self.document_count {
            let entry = self.document_entry(index)?;
            if previous_id.is_some_and(|previous| previous >= entry.document_id) {
                return Err(invalid("document IDs are not strictly ordered"));
            }
            if entry.document_ordinal != index {
                return Err(invalid(
                    "document ordinals are not contiguous and zero-based",
                ));
            }
            if entry.name_offset != expected_name_offset {
                return Err(invalid(
                    "document name offsets are not canonical and contiguous",
                ));
            }
            if entry.target_multiplicity == 0 {
                return Err(invalid("document target multiplicity is zero"));
            }
            if entry.external_target > 1 {
                return Err(invalid("document external-target flag is invalid"));
            }
            let name = self.range(entry.name_offset, entry.name_len)?;
            std::str::from_utf8(name).map_err(|_| invalid("document name is not UTF-8"))?;
            expected_name_offset = checked_add(expected_name_offset, entry.name_len)?;
            previous_id = Some(entry.document_id);
        }
        if expected_name_offset != file_len {
            return Err(invalid("string data has a non-canonical extent"));
        }
        Ok(())
    }

    fn document_entry_for_id(
        &self,
        document_id: DocumentId,
    ) -> Result<DecodedDocumentEntry, CorpusError> {
        let mut low = 0_u64;
        let mut high = self.document_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let entry = self.document_entry(mid)?;
            match entry.document_id.cmp(&document_id.get()) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Ok(entry),
            }
        }
        Err(invalid("document ID is absent from the document table"))
    }

    fn block(&self, ordinal: u64) -> Result<DecodedBlockEntry, CorpusError> {
        let offset = checked_add(self.blocks_offset, checked_mul(ordinal, BLOCK_ENTRY_LEN)?)?;
        let bytes = self.range(offset, BLOCK_ENTRY_LEN)?;
        Ok(DecodedBlockEntry {
            start: read_u64(bytes, 0)?,
            record_offset: read_u64(bytes, 8)?,
            record_count: read_u64(bytes, 16)?,
            checksum: read_array(bytes, 24)?,
        })
    }

    fn document_entry(&self, ordinal: u64) -> Result<DecodedDocumentEntry, CorpusError> {
        let offset = checked_add(
            self.documents_offset,
            checked_mul(ordinal, DOCUMENT_ENTRY_LEN)?,
        )?;
        let bytes = self.range(offset, DOCUMENT_ENTRY_LEN)?;
        Ok(DecodedDocumentEntry {
            document_id: read_u64(bytes, 0)?,
            document_ordinal: read_u64(bytes, 8)?,
            name_offset: read_u64(bytes, 16)?,
            name_len: read_u64(bytes, 24)?,
            target_group_ordinal: read_u64(bytes, 32)?,
            target_multiplicity: read_u64(bytes, 40)?,
            external_target: read_u64(bytes, 48)?,
        })
    }

    fn range(&self, offset: u64, len: u64) -> Result<&[u8], CorpusError> {
        let end = checked_add(offset, len)?;
        let len = checked_usize(len)?;
        let start = checked_usize(offset)?;
        let end_usize = checked_usize(end)?;
        self.map
            .get(start..end_usize)
            .filter(|bytes| bytes.len() == len)
            .ok_or_else(|| truncated(offset))
    }
}

struct DecodedBlockEntry {
    start: u64,
    record_offset: u64,
    record_count: u64,
    checksum: [u8; 32],
}

struct DecodedDocumentEntry {
    document_id: u64,
    document_ordinal: u64,
    name_offset: u64,
    name_len: u64,
    target_group_ordinal: u64,
    target_multiplicity: u64,
    external_target: u64,
}

fn encode_header(
    analysis: AnalysisIdentity,
    sentence_count: u64,
    document_count: u64,
    records_offset: u64,
    blocks_offset: u64,
    documents_offset: u64,
    strings_offset: u64,
    directory_checksum: [u8; 32],
) -> [u8; HEADER_LEN as usize] {
    let mut header = [0; HEADER_LEN as usize];
    header[..8].copy_from_slice(LOCATION_MAGIC);
    put_u32(&mut header, 8, LOCATION_SCHEMA_VERSION);
    put_u32(&mut header, 12, HEADER_LEN as u32);
    header[16..48].copy_from_slice(analysis.as_bytes());
    put_u64(&mut header, 48, sentence_count);
    put_u64(&mut header, 56, document_count);
    put_u64(&mut header, 64, records_offset);
    put_u64(&mut header, 72, blocks_offset);
    put_u64(&mut header, 80, documents_offset);
    put_u64(&mut header, 88, strings_offset);
    header[96..128].copy_from_slice(&directory_checksum);
    header
}

fn encode_block_entry(entry: &BlockEntry) -> [u8; BLOCK_ENTRY_LEN as usize] {
    let mut bytes = [0; BLOCK_ENTRY_LEN as usize];
    put_u64(&mut bytes, 0, entry.start);
    put_u64(&mut bytes, 8, entry.record_offset);
    put_u64(&mut bytes, 16, entry.record_count);
    bytes[24..56].copy_from_slice(&entry.checksum);
    bytes
}

fn encode_document_entry(
    document_id: u64,
    document_ordinal: u64,
    name_offset: u64,
    name_len: u64,
    target_group_ordinal: u64,
    target_multiplicity: u64,
    external_target: bool,
) -> [u8; DOCUMENT_ENTRY_LEN as usize] {
    let mut bytes = [0; DOCUMENT_ENTRY_LEN as usize];
    put_u64(&mut bytes, 0, document_id);
    put_u64(&mut bytes, 8, document_ordinal);
    put_u64(&mut bytes, 16, name_offset);
    put_u64(&mut bytes, 24, name_len);
    put_u64(&mut bytes, 32, target_group_ordinal);
    put_u64(&mut bytes, 40, target_multiplicity);
    put_u64(&mut bytes, 48, u64::from(external_target));
    bytes
}

fn scratch_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8], offset: u64) -> Result<u32, CorpusError> {
    let bytes = checked_subslice(bytes, offset, 4)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("exactly four bytes"),
    ))
}

fn read_u64(bytes: &[u8], offset: u64) -> Result<u64, CorpusError> {
    let bytes = checked_subslice(bytes, offset, 8)?;
    Ok(u64::from_le_bytes(
        bytes.try_into().expect("exactly eight bytes"),
    ))
}

fn read_array(bytes: &[u8], offset: u64) -> Result<[u8; 32], CorpusError> {
    Ok(checked_subslice(bytes, offset, 32)?
        .try_into()
        .expect("exactly thirty-two bytes"))
}

fn checked_subslice(bytes: &[u8], offset: u64, len: u64) -> Result<&[u8], CorpusError> {
    let start = checked_usize(offset)?;
    let end = checked_usize(checked_add(offset, len)?)?;
    bytes.get(start..end).ok_or_else(|| truncated(offset))
}

fn ensure_range(file_len: u64, offset: u64, len: u64) -> Result<(), CorpusError> {
    if checked_add(offset, len)? > file_len {
        return Err(truncated(offset));
    }
    Ok(())
}

fn block_count(sentence_count: u64) -> Result<u64, CorpusError> {
    if sentence_count == 0 {
        return Ok(0);
    }
    sentence_count
        .checked_sub(1)
        .and_then(|count| count.checked_div(RECORDS_PER_BLOCK))
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| invalid("block count overflow"))
}

fn checked_u64(value: usize) -> Result<u64, CorpusError> {
    u64::try_from(value).map_err(|_| invalid("length does not fit u64"))
}

fn checked_usize(value: u64) -> Result<usize, CorpusError> {
    usize::try_from(value).map_err(|_| invalid("offset does not fit usize"))
}

fn checked_add(left: u64, right: u64) -> Result<u64, CorpusError> {
    left.checked_add(right)
        .ok_or_else(|| invalid("offset overflow"))
}

fn checked_sub(left: u64, right: u64) -> Result<u64, CorpusError> {
    left.checked_sub(right)
        .ok_or_else(|| invalid("offset underflow"))
}

fn checked_mul(left: u64, right: u64) -> Result<u64, CorpusError> {
    left.checked_mul(right)
        .ok_or_else(|| invalid("length overflow"))
}

fn invalid(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: COMPONENT,
        message: message.into(),
    }
}

fn truncated(offset: u64) -> CorpusError {
    CorpusError::Truncated {
        component: COMPONENT,
        offset,
    }
}

#[cfg(test)]
#[path = "locations/structural_tests.rs"]
mod structural_tests;
