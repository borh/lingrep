//! Typed, block-compressed V1 prepared morphology storage.

use rustc_hash::FxHashMap;

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::JoinHandle,
};

use compact_str::CompactString;
use lingrep_core::{
    corpus::conllu::EnhancedGraphObservation,
    execution::{
        BoundSentenceInput, CancellationFlag, EnhancedGraphInput, ExecutionBatch, ExecutionError,
        ExecutionLimits, ExecutionSentence, SentenceAdmission, SentenceBatchSource,
        TokenizedAdmission, estimate_token_info_bytes,
    },
    spill::TempBudget,
    token_match::{
        AttributeKind, AttributeOrdinal, AttributeProvider, AttributeValueRef, BoundProjection,
        BoundTokenSentence, CharSpan, ProjectedAttributeValue, SchemaAttribute,
        TokenAttributeIndex, TokenInfo, TokenSchema, TokenSequence,
    },
};
use memmap2::{Mmap, MmapOptions};

use crate::{
    AnalysisIdentity, CorpusError, TokenSchemaIdentity, enhanced::EnhancedReader,
    locations::MappedLocations, scratch::BudgetedScratch,
};

const COMPONENT: &str = "tokens.v1.zst";
const MAGIC: &[u8; 8] = b"LINGTOK1";
const BLOCK_MAGIC: &[u8; 8] = b"LGTBLK02";
const FOOTER_MAGIC: &[u8; 8] = b"LGTEND01";
pub(crate) const FORMAT_VERSION: u32 = 2;
const HEADER_LEN: u64 = 128;
const FOOTER_LEN: u64 = 48;
const DIRECTORY_ENTRY_LEN: u64 = 80;
const RESIDENT_ENTRY_LEN: u64 = 8;
const BLOCK_HEADER_LEN: u64 = 32;
const SENTENCE_ENTRY_LEN: u64 = 16;
const BLOCK_SENTENCE_LIMIT: usize = 256;
const MAX_BLOCK_RAW_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct DirectoryEntry {
    pub(crate) first_sentence: u64,
    pub(crate) sentence_count: u64,
    file_offset: u64,
    pub(crate) stored_len: u64,
    raw_len: u64,
    token_count: u64,
    checksum: [u8; 32],
}

/// Accumulates sentences into blocks, encodes each finished block on a pool,
/// and hands the results to a [`BlockRecorder`] that writes them in order.
///
/// Three stages, split where the obligations differ:
///
/// 1. **Deciding the block**, here, on the caller's thread, because the
///    decision needs the running size bound.
/// 2. **Encoding it** — residency values, the typed encoding, compression and
///    the checksum — on [`BLOCK_ENCODERS`] threads. This is
///    [`encode_recorded_block`], a pure function of the block: it reads no
///    writer state and advances no position, so several blocks can be in it at
///    once.
/// 3. **Writing it**, on the recorder thread, one block at a time, in order,
///    because the file offset and the directory are append-only positions.
///
/// Measured on a 36 MB corpus, stage 2 is what made stage 3 the bound once the
/// caller's own work had been cut: the recorder was the busiest thread in the
/// build at 4.42 s against the caller's 4.15 s, and moving encoding off it took
/// it to 0.51 s (`docs/results/2026-08-27-provenance-and-encode-fan-out`).
///
/// **Ordering is not reconstructed, it is inherent.** The caller sends the
/// recorder one result channel per block, in block order, over a FIFO queue.
/// Waiting on the front of that queue *is* waiting for the next block in
/// sequence, so there is no reorder buffer and no sequence number to get
/// wrong. Blocks are written in the order they were cut and each block's
/// sentences in the order they were pushed, so the residency file, the
/// directory and the token file are byte-for-byte what the single-threaded
/// writer produced.
pub(crate) struct TokenWriter {
    schema: TokenSchema,
    /// Shared with every in-flight encoder, which is why it is behind an `Arc`.
    full_projection: Arc<BoundProjection>,
    buffered: Vec<BoundTokenSentence>,
    buffered_upper_bound: u64,
    encoders: rayon::ThreadPool,
    recorder: RecorderHandle,
}

/// Blocks encoded, or being encoded, ahead of the writer.
///
/// This depth was **one** while a single recorder thread did the encoding as
/// well as the writing, on the measurement that the recorder cost ~2.0 ms per
/// block against ~5.2 ms of caller work — a queue that was empty almost
/// always, where depth could only defer a throughput mismatch that was not
/// there. Two later changes removed enough caller work to invert that: the
/// recorder became the busiest thread in the build, at 4.42 s against the
/// caller's 4.15 s, and the queue became permanently full. Depth is now what
/// keeps [`BLOCK_ENCODERS`] supplied, and it bounds residency: at most
/// `BLOCK_QUEUE_DEPTH + 1` blocks are alive, each one a resident
/// `Vec<BoundTokenSentence>` plus its compressed bytes. A block is bounded by
/// [`MAX_BLOCK_RAW_BYTES`] *encoded*, and a resident `BoundTokenSentence` is
/// wider than its encoding, so the worst case this admits is bounded but not
/// small; on real corpora a block is three orders of magnitude under that cap.
const BLOCK_QUEUE_DEPTH: usize = 4;

/// Threads encoding and compressing blocks.
///
/// The fan-out exists to stop the writer being the rate limiter, not to use
/// every core: past the point where the caller becomes the bound again, more
/// encoders buy nothing, and each one costs a thread for the life of the
/// build. Four takes the writer thread from 4.42 s of a build to 0.51 s, which
/// is already well under the caller's own 3.96 s.
const BLOCK_ENCODERS: usize = 4;

/// One block, encoded, compressed and checksummed, awaiting its turn.
struct EncodedBlock {
    residency: Vec<u64>,
    stored: Vec<u8>,
    raw_len: u64,
    sentence_count: u64,
    token_count: u64,
    checksum: [u8; 32],
}

/// Turns one complete block into the bytes and counts the writer will append.
///
/// Pure in its arguments, which is the whole reason it can run anywhere: it
/// reads no writer state and advances no position. Everything order-dependent
/// — where the bytes land, which sentences they cover — is decided by
/// [`BlockRecorder::write`], on one thread, in order.
fn encode_recorded_block(
    buffered: &[BoundTokenSentence],
    projection: &BoundProjection,
) -> Result<EncodedBlock, CorpusError> {
    let mut residency = Vec::with_capacity(buffered.len());
    for sentence in buffered {
        residency.push(
            u64::try_from(
                sentence
                    .resident_bytes()
                    .map_err(|_| invalid("sentence residency overflow"))?,
            )
            .map_err(|_| invalid("sentence residency exceeds u64"))?,
        );
    }
    let raw = encode_block(buffered, projection)?;
    if raw.len() as u64 > MAX_BLOCK_RAW_BYTES {
        return Err(invalid("encoded block exceeds its raw bound"));
    }
    let stored = zstd::bulk::compress(&raw, 3)
        .map_err(|error| invalid(format!("block compression failed: {error}")))?;
    Ok(EncodedBlock {
        residency,
        checksum: *blake3::hash(&stored).as_bytes(),
        raw_len: raw.len() as u64,
        sentence_count: buffered.len() as u64,
        token_count: buffered.iter().map(BoundTokenSentence::len).sum::<usize>() as u64,
        stored,
    })
}

/// One block on its way from an encoder to the writer.
type PendingBlock = Receiver<Result<EncodedBlock, CorpusError>>;

/// Owns every append-only output of the token component and advances them one
/// block at a time.
struct BlockRecorder {
    file: File,
    directory: BudgetedScratch,
    residency: BudgetedScratch,
    schema: TokenSchema,
    directory_count: u64,
    sentence_count: u64,
    token_count: u64,
}

impl BlockRecorder {
    /// Appends one encoded block to every output it owns.
    fn write(&mut self, block: EncodedBlock) -> Result<(), CorpusError> {
        for resident in &block.residency {
            self.residency.append(&resident.to_le_bytes())?;
        }
        let entry = DirectoryEntry {
            first_sentence: self.sentence_count,
            sentence_count: block.sentence_count,
            file_offset: self.file.stream_position()?,
            stored_len: block.stored.len() as u64,
            raw_len: block.raw_len,
            token_count: block.token_count,
            checksum: block.checksum,
        };
        self.file.write_all(&block.stored)?;
        self.directory.append(&encode_directory_entry(entry))?;
        self.directory_count += 1;
        self.sentence_count += block.sentence_count;
        self.token_count += block.token_count;
        Ok(())
    }

    /// Writes the directory, the residency table, the footer and the header.
    ///
    /// This runs on the caller's thread after the recorder thread has been
    /// joined, because it needs every block's contribution to be final.
    fn finish(mut self, analysis: AnalysisIdentity) -> Result<(), CorpusError> {
        let directory_offset = self.file.stream_position()?;
        self.directory.sync_all()?;
        self.directory.rewind()?;
        let mut metadata_hash = blake3::Hasher::new();
        let mut entry = [0_u8; DIRECTORY_ENTRY_LEN as usize];
        for _ in 0..self.directory_count {
            self.directory.read_exact(&mut entry)?;
            self.file.write_all(&entry)?;
            metadata_hash.update(&entry);
        }
        let residency_offset = self.file.stream_position()?;
        self.residency.sync_all()?;
        self.residency.rewind()?;
        let mut resident = [0_u8; RESIDENT_ENTRY_LEN as usize];
        for _ in 0..self.sentence_count {
            self.residency.read_exact(&mut resident)?;
            self.file.write_all(&resident)?;
            metadata_hash.update(&resident);
        }
        let metadata_len = self
            .directory_count
            .checked_mul(DIRECTORY_ENTRY_LEN)
            .and_then(|bytes| bytes.checked_add(self.sentence_count * RESIDENT_ENTRY_LEN))
            .ok_or_else(|| invalid("metadata length overflow"))?;
        let mut footer = Vec::with_capacity(FOOTER_LEN as usize);
        footer.extend_from_slice(&metadata_len.to_le_bytes());
        footer.extend_from_slice(metadata_hash.finalize().as_bytes());
        footer.extend_from_slice(FOOTER_MAGIC);
        self.file.write_all(&footer)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&encode_header(
            analysis,
            &self.schema,
            self.sentence_count,
            self.token_count,
            self.directory_count,
            directory_offset,
            residency_offset,
        )?)?;
        self.file.sync_all()?;
        Ok(())
    }
}

/// The caller's end of the recorder thread.
///
/// A recording failure ends the thread, which disconnects the channel, so the
/// next send fails. That is the only failure path: [`Self::send`] answers a
/// disconnect by joining, which surfaces the error the recorder actually hit
/// rather than a channel-shaped stand-in. No error is shared through a lock,
/// because the thread's own return value already carries exactly one.
struct RecorderHandle {
    blocks: Option<SyncSender<PendingBlock>>,
    thread: Option<JoinHandle<Result<BlockRecorder, CorpusError>>>,
}

impl RecorderHandle {
    fn spawn(recorder: BlockRecorder) -> Self {
        let (blocks, jobs) = sync_channel::<PendingBlock>(BLOCK_QUEUE_DEPTH);
        let thread = std::thread::Builder::new()
            .name("lingrep-block-recorder".into())
            .spawn(move || {
                let mut recorder = recorder;
                // Ordering is still inherent. The queue is FIFO and each entry
                // is that block's own result channel, so waiting on the front
                // one *is* waiting for the next block in sequence — there is no
                // reorder buffer and no sequence number to get wrong.
                for pending in jobs {
                    let encoded = pending
                        .recv()
                        .map_err(|_| invalid("a block encoder ended without reporting"))?;
                    recorder.write(encoded?)?;
                }
                Ok(recorder)
            })
            .expect("the block recorder thread must start");
        Self {
            blocks: Some(blocks),
            thread: Some(thread),
        }
    }

    fn send(&mut self, block: PendingBlock) -> Result<(), CorpusError> {
        let sender = self
            .blocks
            .as_ref()
            .ok_or_else(|| invalid("block recorder is already finished"))?;
        if sender.send(block).is_err() {
            // Disconnected means the recorder returned; join to learn why.
            self.join()?;
            return Err(invalid("block recorder stopped without reporting an error"));
        }
        Ok(())
    }

    fn join(&mut self) -> Result<BlockRecorder, CorpusError> {
        drop(self.blocks.take());
        self.thread
            .take()
            .ok_or_else(|| invalid("block recorder is already joined"))?
            .join()
            .map_err(|_| invalid("block recorder thread panicked"))?
    }
}

impl Drop for RecorderHandle {
    /// A `TokenWriter` dropped without `finish` — a cancelled or failed build —
    /// must not leave the recorder thread holding the staging file. Dropping
    /// the sender ends its loop; the join discards whatever it returns, because
    /// the build that would have reported it is already over.
    fn drop(&mut self) {
        drop(self.blocks.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl TokenWriter {
    pub(crate) fn create(
        path: &Path,
        schema: TokenSchema,
        temp_budget: Arc<TempBudget>,
    ) -> Result<Self, CorpusError> {
        let full_projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .map_err(|error| invalid(format!("stored schema cannot bind itself: {error}")))?;
        let directory =
            BudgetedScratch::create(&scratch_path(path, ".directory.scratch"), &temp_budget)?;
        let residency =
            BudgetedScratch::create(&scratch_path(path, ".residency.scratch"), &temp_budget)?;
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
        file.write_all(&vec![0; HEADER_LEN as usize])?;
        let schema_bytes = encode_schema(&schema)?;
        file.write_all(&schema_bytes)?;
        let encoders = rayon::ThreadPoolBuilder::new()
            .num_threads(BLOCK_ENCODERS)
            .thread_name(|index| format!("lingrep-block-encode-{index}"))
            .build()
            .map_err(|error| invalid(format!("block encoder pool: {error}")))?;
        Ok(Self {
            schema: schema.clone(),
            full_projection: Arc::new(full_projection),
            buffered: Vec::with_capacity(BLOCK_SENTENCE_LIMIT),
            buffered_upper_bound: BLOCK_HEADER_LEN,
            encoders,
            recorder: RecorderHandle::spawn(BlockRecorder {
                file,
                directory,
                residency,
                schema,
                directory_count: 0,
                sentence_count: 0,
                token_count: 0,
            }),
        })
    }

    /// Takes the sentence by value because it is buffered until the block
    /// flushes and every caller already owns it; borrowing would require a deep
    /// clone.
    pub(crate) fn push_bound_sentence(
        &mut self,
        sentence: BoundTokenSentence,
    ) -> Result<(), CorpusError> {
        if sentence.schema_digest() != self.schema.digest()
            || sentence.width() != self.full_projection.len()
        {
            return Err(invalid(
                "sentence does not carry the complete stored schema",
            ));
        }
        let upper = encoded_sentence_upper_bound(&sentence, &self.full_projection)?;
        if BLOCK_HEADER_LEN
            .checked_add(upper)
            .is_none_or(|size| size > MAX_BLOCK_RAW_BYTES)
        {
            return Err(CorpusError::TokenBlockCapacity {
                minimum: BLOCK_HEADER_LEN.saturating_add(upper),
                limit: MAX_BLOCK_RAW_BYTES,
            });
        }
        if self
            .buffered_upper_bound
            .checked_add(upper)
            .is_none_or(|size| size > MAX_BLOCK_RAW_BYTES)
        {
            self.flush_block()?;
        }
        self.buffered_upper_bound = self
            .buffered_upper_bound
            .checked_add(upper)
            .ok_or_else(|| invalid("block-size accounting overflow"))?;
        self.buffered.push(sentence);
        if self.buffered.len() == BLOCK_SENTENCE_LIMIT {
            self.flush_block()?;
        }
        Ok(())
    }

    pub fn push_sentence(&mut self, tokens: &[TokenInfo]) -> Result<(), CorpusError> {
        let sentence = BoundTokenSentence::project(&self.full_projection, tokens)
            .map_err(|_| invalid("physical token row cannot project into the stored schema"))?;
        self.push_bound_sentence(sentence)
    }

    pub(crate) fn finish(mut self, analysis: AnalysisIdentity) -> Result<(), CorpusError> {
        self.flush_block()?;
        self.recorder.join()?.finish(analysis)
    }

    /// Hands the buffered block to the recorder. Returns as soon as the queue
    /// accepts it, which is the whole point; the error a previous block hit
    /// arrives here rather than at the block that caused it.
    fn flush_block(&mut self) -> Result<(), CorpusError> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let block = std::mem::replace(&mut self.buffered, Vec::with_capacity(BLOCK_SENTENCE_LIMIT));
        self.buffered_upper_bound = BLOCK_HEADER_LEN;
        let (encoded, pending) = sync_channel(1);
        let projection = Arc::clone(&self.full_projection);
        self.encoders.spawn(move || {
            // A closed receiver means the writer is already gone, which the
            // caller learns from its own send below.
            let _ = encoded.send(encode_recorded_block(&block, &projection));
        });
        self.recorder.send(pending)
    }
}

pub(crate) struct TokenReader {
    _path: PathBuf,
    map: Mmap,
    schema: TokenSchema,
    directory_offset: u64,
    residency_offset: u64,
    block_count: u64,
    sentence_count: u64,
    token_count: u64,
    decoded_blocks: AtomicUsize,
    decoded_sentences: AtomicUsize,
    verified_blocks: AtomicUsize,
}

impl TokenReader {
    pub(crate) fn open(
        path: &Path,
        expected_analysis: &AnalysisIdentity,
        expected_schema: &TokenSchemaIdentity,
    ) -> Result<Self, CorpusError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len < HEADER_LEN + FOOTER_LEN {
            return Err(truncated(len));
        }
        let map = unsafe { MmapOptions::new().map(&file)? };
        if slice(&map, 0, 8)? != MAGIC {
            return Err(invalid("wrong V1 token magic"));
        }
        let version = read_u32(&map, 8)?;
        if version != FORMAT_VERSION {
            return Err(CorpusError::UnsupportedSchema {
                component: COMPONENT,
                version,
            });
        }
        if read_u32(&map, 12)? != HEADER_LEN as u32 {
            return Err(invalid("wrong V1 header length"));
        }
        let attribute_count = read_u32(&map, 16)? as usize;
        let schema_len = read_u32(&map, 20)? as u64;
        let analysis = AnalysisIdentity::from_bytes(read_array(&map, 24)?);
        if &analysis != expected_analysis {
            return Err(CorpusError::AnalysisIdentityMismatch {
                component: COMPONENT,
            });
        }
        let recorded_schema = TokenSchemaIdentity::from_bytes(read_array(&map, 56)?);
        if &recorded_schema != expected_schema {
            return Err(CorpusError::VerificationMismatch {
                component: COMPONENT,
                sentence_ordinal: None,
                message: "token schema identity differs from the manifest".into(),
            });
        }
        let schema = decode_schema(slice(&map, HEADER_LEN, schema_len)?)?;
        if schema.attributes().len() != attribute_count
            || schema.digest().as_bytes() != expected_schema.as_bytes()
        {
            return Err(invalid("stored schema bytes differ from their identity"));
        }
        let sentence_count = read_u64(&map, 88)?;
        let token_count = read_u64(&map, 96)?;
        let block_count = read_u64(&map, 104)?;
        let directory_offset = read_u64(&map, 112)?;
        let residency_offset = read_u64(&map, 120)?;
        let footer_offset = len - FOOTER_LEN;
        let footer = slice(&map, footer_offset, FOOTER_LEN)?;
        if &footer[40..] != FOOTER_MAGIC {
            return Err(invalid("wrong V1 footer magic"));
        }
        let directory_len = block_count
            .checked_mul(DIRECTORY_ENTRY_LEN)
            .ok_or_else(|| invalid("directory length overflow"))?;
        let residency_len = sentence_count
            .checked_mul(RESIDENT_ENTRY_LEN)
            .ok_or_else(|| invalid("residency length overflow"))?;
        if directory_offset < HEADER_LEN + schema_len
            || directory_offset.checked_add(directory_len) != Some(residency_offset)
            || residency_offset.checked_add(residency_len) != Some(footer_offset)
            || read_u64(footer, 0)? != directory_len + residency_len
        {
            return Err(invalid("invalid V1 directory bounds"));
        }
        let metadata = slice(&map, directory_offset, directory_len + residency_len)?;
        if blake3::hash(metadata).as_bytes() != &footer[8..40] {
            return Err(invalid("V1 directory checksum mismatch"));
        }
        Ok(Self {
            _path: path.to_path_buf(),
            map,
            schema,
            directory_offset,
            residency_offset,
            block_count,
            sentence_count,
            token_count,
            decoded_blocks: AtomicUsize::new(0),
            decoded_sentences: AtomicUsize::new(0),
            verified_blocks: AtomicUsize::new(0),
        })
    }

    pub fn sentence_count(&self) -> u64 {
        self.sentence_count
    }
    pub fn token_count(&self) -> u64 {
        self.token_count
    }
    pub fn decoded_block_count(&self) -> usize {
        self.decoded_blocks.load(Ordering::Relaxed)
    }
    pub fn decoded_sentence_count(&self) -> usize {
        self.decoded_sentences.load(Ordering::Relaxed)
    }
    pub fn verified_block_count(&self) -> usize {
        self.verified_blocks.load(Ordering::Relaxed)
    }

    pub(crate) fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn projection_for_physical(
        &self,
        attributes: &[TokenAttributeIndex],
    ) -> Result<BoundProjection, CorpusError> {
        self.schema
            .bind_projection(
                attributes
                    .iter()
                    .copied()
                    .map(lingrep_core::token_match::AttributeKey::from),
            )
            .map_err(|error| invalid(format!("requested projection is unavailable: {error}")))
    }

    fn resident_size(&self, ordinal: u64) -> Result<usize, CorpusError> {
        if ordinal >= self.sentence_count {
            return Err(invalid("sentence ordinal is outside the V1 component"));
        }
        usize::try_from(read_u64(&self.map, self.residency_offset + ordinal * 8)?)
            .map_err(|_| invalid("resident size exceeds usize"))
    }

    fn directory_entry(&self, index: u64) -> Result<DirectoryEntry, CorpusError> {
        if index >= self.block_count {
            return Err(invalid("block index is outside the V1 directory"));
        }
        decode_directory_entry(slice(
            &self.map,
            self.directory_offset + index * DIRECTORY_ENTRY_LEN,
            DIRECTORY_ENTRY_LEN,
        )?)
    }

    pub(crate) fn block_for_sentence(
        &self,
        ordinal: u64,
    ) -> Result<(u64, DirectoryEntry), CorpusError> {
        if ordinal >= self.sentence_count {
            return Err(invalid("sentence ordinal is outside the V1 component"));
        }
        let mut low = 0;
        let mut high = self.block_count;
        while low < high {
            let middle = low + (high - low) / 2;
            if self.directory_entry(middle)?.first_sentence <= ordinal {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let index = low.checked_sub(1).ok_or_else(|| invalid("directory gap"))?;
        let entry = self.directory_entry(index)?;
        if ordinal >= entry.first_sentence + entry.sentence_count {
            return Err(invalid("directory gap"));
        }
        Ok((index, entry))
    }

    fn load_block(&self, entry: DirectoryEntry) -> Result<Vec<u8>, CorpusError> {
        let stored = slice(&self.map, entry.file_offset, entry.stored_len)?;
        if blake3::hash(stored).as_bytes() != &entry.checksum {
            return Err(CorpusError::VerificationMismatch {
                component: COMPONENT,
                sentence_ordinal: Some(entry.first_sentence),
                message: "V1 block checksum mismatch".into(),
            });
        }
        let raw = zstd::bulk::decompress(
            stored,
            usize::try_from(entry.raw_len)
                .map_err(|_| invalid("raw block length exceeds usize"))?,
        )
        .map_err(|error| invalid(format!("block decompression failed: {error}")))?;
        if raw.len() as u64 != entry.raw_len {
            return Err(invalid("decompressed block length mismatch"));
        }
        self.decoded_blocks.fetch_add(1, Ordering::Relaxed);
        Ok(raw)
    }

    pub(crate) fn decode_bound_ordinals(
        &self,
        ordinals: &[u64],
        projection: &BoundProjection,
        char_offsets: bool,
    ) -> Result<Vec<BoundTokenSentence>, CorpusError> {
        if projection.schema_digest() != self.schema.digest() {
            return Err(invalid("requested projection belongs to another schema"));
        }
        let mut output = Vec::with_capacity(ordinals.len());
        let mut cursor = 0;
        while cursor < ordinals.len() {
            let (_, entry) = self.block_for_sentence(ordinals[cursor])?;
            let end = entry.first_sentence + entry.sentence_count;
            let start = cursor;
            while cursor < ordinals.len() && ordinals[cursor] < end {
                if ordinals[cursor] < entry.first_sentence {
                    return Err(invalid("sentence ordinals must be nondecreasing"));
                }
                cursor += 1;
            }
            let locals = ordinals[start..cursor]
                .iter()
                .map(|ordinal| {
                    usize::try_from(*ordinal - entry.first_sentence)
                        .map_err(|_| invalid("block-local sentence ordinal exceeds usize"))
                })
                .collect::<Result<Vec<_>, CorpusError>>()?;
            let raw = self.load_block(entry)?;
            output.extend(decode_block(
                &raw,
                entry,
                &self.schema,
                projection,
                &locals,
                char_offsets,
            )?);
        }
        self.decoded_sentences
            .fetch_add(output.len(), Ordering::Relaxed);
        Ok(output)
    }

    pub fn decode_sentences_with_attributes(
        &self,
        sentence_indices: &[u64],
        attributes: &[TokenAttributeIndex],
    ) -> Result<Vec<Option<Vec<TokenInfo>>>, CorpusError> {
        let projection = self.projection_for_physical(attributes)?;
        let mut ordinals = sentence_indices
            .iter()
            .copied()
            .filter(|ordinal| *ordinal < self.sentence_count)
            .collect::<Vec<_>>();
        ordinals.sort_unstable();
        ordinals.dedup();
        let decoded = self.decode_bound_ordinals(&ordinals, &projection, false)?;
        let projected = ordinals
            .into_iter()
            .zip(decoded)
            .map(|(ordinal, sentence)| {
                bound_to_physical(&sentence, &projection).map(|sentence| (ordinal, sentence))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(sentence_indices
            .iter()
            .map(|ordinal| projected.get(ordinal).cloned())
            .collect())
    }

    pub fn verify_integrity(&self) -> Result<(), CorpusError> {
        let mut next_sentence = 0;
        let mut tokens = 0;
        for index in 0..self.block_count {
            let entry = self.directory_entry(index)?;
            if entry.first_sentence != next_sentence {
                return Err(invalid("non-contiguous V1 directory"));
            }
            let raw = self.load_block(entry)?;
            validate_block(&raw, entry, &self.schema)?;
            next_sentence += entry.sentence_count;
            tokens += entry.token_count;
            self.verified_blocks.fetch_add(1, Ordering::Relaxed);
        }
        if next_sentence != self.sentence_count || tokens != self.token_count {
            return Err(invalid("V1 directory totals differ from the header"));
        }
        Ok(())
    }
}

enum SourceProjection {
    Physical(BoundProjection),
    Bound {
        logical: BoundProjection,
        physical: Box<BoundProjection>,
        graph_requested: bool,
    },
}

pub struct PreparedSentenceSource {
    reader: Arc<TokenReader>,
    enhanced: Option<Arc<EnhancedReader>>,
    locations: MappedLocations,
    projection: SourceProjection,
    next_ordinal: u64,
    terminal_failure: Option<String>,
    admission: SentenceAdmission,
    cancel: Option<CancellationFlag>,
    tokenized_admission: TokenizedAdmission,
    source_metadata: Option<lingrep_core::corpus::SourceMetadataProjection>,
    provenance: Option<Arc<crate::BlockingProvenance>>,
    char_offsets: bool,
    block_sample: Option<lingrep_core::execution::CorpusSample>,
    pub(crate) candidates: Option<crate::postings::Candidates>,
    filter_stats: crate::PreparedFilterStats,
}

impl PreparedSentenceSource {
    pub(crate) fn new(
        reader: TokenReader,
        locations: MappedLocations,
        attributes: Vec<TokenAttributeIndex>,
    ) -> Result<Self, CorpusError> {
        let projection = reader.projection_for_physical(&attributes)?;
        Self::finish_new(
            reader,
            None,
            locations,
            SourceProjection::Physical(projection),
        )
    }

    pub(crate) fn new_bound_deferred(
        reader: TokenReader,
        enhanced: Option<Arc<EnhancedReader>>,
        locations: MappedLocations,
        logical_schema: &TokenSchema,
        logical: BoundProjection,
    ) -> Result<Self, CorpusError> {
        if logical.schema_digest() != logical_schema.digest() {
            return Err(invalid(
                "bound projection belongs to another logical schema",
            ));
        }
        let physical = reader
            .schema()
            .bind_projection(
                logical
                    .keys()
                    .iter()
                    .zip(logical.providers())
                    .filter(|(_, provider)| **provider != AttributeProvider::EnhancedGraph)
                    .map(|(key, _)| key.clone()),
            )
            .map_err(|error| invalid(format!("physical projection is unavailable: {error}")))?;
        let graph_requested = logical
            .providers()
            .contains(&AttributeProvider::EnhancedGraph);
        if graph_requested && enhanced.is_none() {
            return Err(invalid(
                "logical graph projection has no enhanced graph component",
            ));
        }
        Self::finish_new(
            reader,
            enhanced,
            locations,
            SourceProjection::Bound {
                logical,
                physical: Box::new(physical),
                graph_requested,
            },
        )
    }

    fn finish_new(
        reader: TokenReader,
        enhanced: Option<Arc<EnhancedReader>>,
        locations: MappedLocations,
        projection: SourceProjection,
    ) -> Result<Self, CorpusError> {
        if reader.sentence_count() != locations.sentence_count() {
            return Err(CorpusError::SentenceCountMismatch {
                component: COMPONENT,
            });
        }
        Ok(Self {
            reader: Arc::new(reader),
            enhanced,
            locations,
            projection,
            next_ordinal: 0,
            terminal_failure: None,
            admission: SentenceAdmission::All,
            cancel: None,
            tokenized_admission: TokenizedAdmission::Fail,
            source_metadata: None,
            provenance: None,
            char_offsets: true,
            block_sample: None,
            candidates: None,
            filter_stats: crate::PreparedFilterStats::default(),
        })
    }
    /// Returns a live counter shared with this source, including after it reaches EOF.
    pub fn filter_stats(&self) -> crate::PreparedFilterStats {
        self.filter_stats.clone()
    }

    /// Draws physical token blocks by their first corpus sentence ordinal, before decoding.
    #[must_use]
    pub fn with_block_sample(
        mut self,
        sample: Option<lingrep_core::execution::CorpusSample>,
    ) -> Self {
        self.block_sample =
            sample.filter(|sample| sample.rate_per_million() < lingrep_core::execution::RATE_SCALE);
        self
    }

    #[must_use]
    pub fn without_char_offsets(mut self) -> Self {
        self.char_offsets = false;
        self
    }

    pub(crate) fn with_source_metadata(
        mut self,
        reader: Arc<crate::BlockingProvenance>,
        projection: lingrep_core::corpus::SourceMetadataProjection,
    ) -> Self {
        self.provenance = Some(reader);
        self.source_metadata = Some(projection);
        self
    }

    #[must_use]
    pub fn with_tokenized_admission(mut self, admission: TokenizedAdmission) -> Self {
        self.tokenized_admission = admission;
        self
    }
    #[must_use]
    pub fn with_admission(mut self, admission: SentenceAdmission) -> Self {
        self.admission = admission;
        self
    }
    #[must_use]
    pub fn with_cancellation(mut self, cancel: CancellationFlag) -> Self {
        self.cancel = Some(cancel);
        self
    }
    pub fn decoded_block_count(&self) -> usize {
        self.reader.decoded_block_count()
    }
    pub fn decoded_sentence_count(&self) -> usize {
        self.reader.decoded_sentence_count()
    }
    /// Returns the number of enhanced blocks decoded by this source.
    #[must_use]
    pub fn enhanced_decoded_block_count(&self) -> usize {
        self.enhanced
            .as_ref()
            .map_or(0, |reader| reader.decoded_block_count())
    }
    pub fn verified_block_count(&self) -> usize {
        self.reader.verified_block_count()
    }
    pub fn sentence_count(&self) -> u64 {
        self.reader.sentence_count()
    }

    pub fn decode_sentences_with_attributes(
        &self,
        _pool: &rayon::ThreadPool,
        sentence_indices: &[u64],
        attributes: &[TokenAttributeIndex],
    ) -> Result<Vec<Option<Vec<TokenInfo>>>, CorpusError> {
        self.reader
            .decode_sentences_with_attributes(sentence_indices, attributes)
    }

    pub fn decode_sentences(
        &self,
        pool: &rayon::ThreadPool,
        sentence_indices: &[u64],
    ) -> Result<Vec<Option<Vec<TokenInfo>>>, CorpusError> {
        let attributes = TokenAttributeIndex::ALL
            .into_iter()
            .filter(|attribute| self.reader.schema.keys().contains(&(*attribute).into()))
            .collect::<Vec<_>>();
        self.decode_sentences_with_attributes(pool, sentence_indices, &attributes)
    }

    fn admitted(&self, ordinal: u64) -> Result<bool, CorpusError> {
        if self.admission.is_unrestricted() {
            return Ok(true);
        }
        Ok(self.admission.admits(&self.locations.sentence(ordinal)?))
    }

    fn fail(&mut self, ordinal: u64, error: CorpusError) -> ExecutionError {
        let message = error.to_string();
        self.terminal_failure = Some(message.clone());
        ExecutionError::Source { ordinal, message }
    }
}

impl SentenceBatchSource for PreparedSentenceSource {
    fn wants_lookahead(&self) -> bool {
        true
    }

    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        if let Some(message) = &self.terminal_failure {
            return Err(ExecutionError::Source {
                ordinal: self.next_ordinal,
                message: format!("previous V1 token decode failure: {message}"),
            });
        }
        limits.validate()?;
        while self.next_ordinal < self.reader.sentence_count() {
            if self
                .cancel
                .as_ref()
                .is_some_and(CancellationFlag::is_cancelled)
            {
                return Ok(None);
            }
            let mut ordinals = Vec::new();
            let mut indexed_bytes = 0_usize;
            let start = self.next_ordinal;
            // Bound a graph-bearing pull to the native token/enhanced block
            // granularity. This avoids retaining several decoded graph blocks
            // in one source batch while still amortizing ordinary dense runs.
            let source_batch_sentences = if matches!(
                &self.projection,
                SourceProjection::Bound {
                    graph_requested: true,
                    ..
                }
            ) {
                limits.max_stage_sentences.min(BLOCK_SENTENCE_LIMIT)
            } else {
                limits.max_stage_sentences
            };
            while self.next_ordinal < self.reader.sentence_count()
                && ordinals.len() < source_batch_sentences
            {
                let ordinal = self.next_ordinal;
                if let Some(sample) = self.block_sample {
                    let (_, block) = self
                        .reader
                        .block_for_sentence(ordinal)
                        .map_err(|error| self.fail(ordinal, error))?;
                    if !sample.admits(block.first_sentence) {
                        self.next_ordinal = block.first_sentence + block.sentence_count;
                        continue;
                    }
                }
                self.next_ordinal += 1;
                let admitted = self
                    .admitted(ordinal)
                    .map_err(|error| self.fail(ordinal, error))?;
                if !admitted {
                    continue;
                }
                let resident = self
                    .reader
                    .resident_size(ordinal)
                    .map_err(|error| self.fail(ordinal, error))?;
                if resident > limits.max_tokenized_bytes
                    && matches!(&self.projection, SourceProjection::Physical(_))
                {
                    match &self.tokenized_admission {
                        TokenizedAdmission::Fail => {
                            return Err(ExecutionError::TokenizedSentenceTooLarge {
                                ordinal,
                                actual: resident,
                                limit: limits.max_tokenized_bytes,
                            });
                        }
                        TokenizedAdmission::Omit(omissions) => {
                            omissions
                                .lock()
                                .map_err(|_| ExecutionError::Source {
                                    ordinal,
                                    message: "oversized omission lock poisoned".into(),
                                })?
                                .record(
                                    ordinal,
                                    self.locations
                                        .sentence(ordinal)
                                        .ok()
                                        .map(|row| row.source_name),
                                    resident,
                                    limits.max_tokenized_bytes,
                                );
                            continue;
                        }
                    }
                }
                if !ordinals.is_empty()
                    && indexed_bytes
                        .checked_add(resident)
                        .is_none_or(|total| total > limits.max_tokenized_bytes)
                {
                    self.next_ordinal = ordinal;
                    break;
                }
                indexed_bytes += resident;
                ordinals.push(ordinal);
            }
            if ordinals.is_empty() {
                if self.next_ordinal == start {
                    return Err(ExecutionError::Source {
                        ordinal: start,
                        message: "V1 source made no progress".into(),
                    });
                }
                continue;
            }
            if self.candidates.is_some() {
                let mut retained = Vec::with_capacity(ordinals.len());
                for ordinal in ordinals {
                    let resident = self
                        .reader
                        .resident_size(ordinal)
                        .map_err(|error| self.fail(ordinal, error))?;
                    if resident > limits.max_tokenized_bytes {
                        retained.push(ordinal);
                        continue;
                    }
                    let result = self
                        .candidates
                        .as_mut()
                        .expect("candidate source")
                        .contains(ordinal);
                    if result.map_err(|error| self.fail(ordinal, error))? {
                        retained.push(ordinal);
                    } else {
                        self.filter_stats.record(1);
                    }
                }
                ordinals = retained;
                if ordinals.is_empty() {
                    continue;
                }
            }
            let (projection, logical_projection, graph_requested) = match &self.projection {
                SourceProjection::Physical(projection) => (projection.clone(), None, false),
                SourceProjection::Bound {
                    logical,
                    physical,
                    graph_requested,
                } => (
                    (**physical).clone(),
                    Some(logical.clone()),
                    *graph_requested,
                ),
            };
            let mut decoded = self
                .reader
                .decode_bound_ordinals(&ordinals, &projection, self.char_offsets)
                .map_err(|error| self.fail(ordinals[0], error))?;
            let mut graph_observations = None;
            if let Some(logical) = &logical_projection {
                if graph_requested {
                    let observations = self
                        .enhanced
                        .as_ref()
                        .expect("graph-bound source construction required a component")
                        .observations(&ordinals)
                        .map_err(|error| self.fail(ordinals[0], error))?;
                    decoded = decoded
                        .into_iter()
                        .zip(&observations)
                        .map(|(physical, observation)| {
                            merge_graph_projection(&physical, logical, observation)
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| self.fail(ordinals[0], error))?;
                    graph_observations = Some(observations);
                } else if logical.schema_digest() != projection.schema_digest() {
                    decoded = decoded
                        .iter()
                        .map(|sentence| sentence.project_onto_schema(self.reader.schema(), logical))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| {
                            self.fail(
                                ordinals[0],
                                invalid("logical projection reconstruction failed"),
                            )
                        })?;
                }
            }
            let mut locations = ordinals
                .iter()
                .map(|ordinal| self.locations.sentence(*ordinal))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| self.fail(ordinals[0], error))?;
            if let Some(metadata) = &self.source_metadata {
                let reader = self
                    .provenance
                    .as_ref()
                    .expect("metadata source has provenance");
                for page in locations.chunks_mut(crate::MAX_PROVENANCE_PAGE_SIZE) {
                    let values = reader
                        .block_on(reader.view().projected_source_metadata(page, metadata))
                        .map_err(|error| ExecutionError::Source {
                            ordinal: page[0].corpus_sentence_ordinal,
                            message: error.to_string(),
                        })?;
                    for (location, value) in page.iter_mut().zip(values) {
                        location.source_metadata = Some(value);
                    }
                }
            }
            return if logical_projection.is_some() {
                let mut charges = decoded
                    .iter()
                    .enumerate()
                    .map(|(index, words)| {
                        let words = words.resident_bytes().map_err(|_| ())?;
                        let graph = graph_observations
                            .as_ref()
                            .map_or(Some(0), |observations| observations[index].resident_bytes())
                            .ok_or(())?;
                        words.checked_add(graph).ok_or(())
                    })
                    .collect::<Result<Vec<_>, ()>>()
                    .map_err(|_| ExecutionError::Source {
                        ordinal: ordinals[0],
                        message: "V1 projected residency overflow".into(),
                    })?;
                let oversized = charges
                    .iter()
                    .enumerate()
                    .filter_map(|(index, charge)| {
                        (*charge > limits.max_tokenized_bytes).then_some((index, *charge))
                    })
                    .collect::<Vec<_>>();
                if let Some(&(index, actual)) = oversized.first() {
                    let omissions = match &self.tokenized_admission {
                        TokenizedAdmission::Fail => {
                            return Err(ExecutionError::TokenizedSentenceTooLarge {
                                ordinal: ordinals[index],
                                actual,
                                limit: limits.max_tokenized_bytes,
                            });
                        }
                        TokenizedAdmission::Omit(omissions) => omissions,
                    };
                    let mut omissions = omissions.lock().map_err(|_| ExecutionError::Source {
                        ordinal: ordinals[index],
                        message: "oversized omission lock poisoned".into(),
                    })?;
                    for &(index, actual) in &oversized {
                        let ordinal = ordinals[index];
                        omissions.record(
                            ordinal,
                            self.locations
                                .sentence(ordinal)
                                .ok()
                                .map(|row| row.source_name),
                            actual,
                            limits.max_tokenized_bytes,
                        );
                    }
                    drop(omissions);

                    let keep = charges
                        .iter()
                        .map(|charge| *charge <= limits.max_tokenized_bytes)
                        .collect::<Vec<_>>();
                    locations = locations
                        .into_iter()
                        .zip(&keep)
                        .filter_map(|(location, keep)| keep.then_some(location))
                        .collect();
                    decoded = decoded
                        .into_iter()
                        .zip(&keep)
                        .filter_map(|(sentence, keep)| keep.then_some(sentence))
                        .collect();
                    if let Some(observations) = graph_observations.take() {
                        graph_observations = Some(
                            observations
                                .into_iter()
                                .zip(&keep)
                                .filter_map(|(observation, keep)| keep.then_some(observation))
                                .collect(),
                        );
                    }
                    charges = charges
                        .into_iter()
                        .zip(&keep)
                        .filter_map(|(charge, keep)| keep.then_some(charge))
                        .collect();
                    if charges.is_empty() {
                        continue;
                    }
                }
                Ok(Some(ExecutionBatch::bound_tokenized_with_charges(
                    locations
                        .into_iter()
                        .zip(decoded.into_iter().enumerate())
                        .map(|(location, (index, words))| ExecutionSentence {
                            location,
                            value: BoundSentenceInput::new(
                                words,
                                graph_observations.as_ref().map_or(
                                    EnhancedGraphInput::NotRequested,
                                    |observations| {
                                        EnhancedGraphInput::Observation(observations[index].clone())
                                    },
                                ),
                            ),
                        })
                        .collect(),
                    charges,
                )))
            } else {
                let tokens = decoded
                    .iter()
                    .map(|sentence| bound_to_physical(sentence, &projection))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| ExecutionError::ResourceAccounting)?;
                let charges = tokens
                    .iter()
                    .map(|sentence| estimate_token_info_bytes(sentence))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Some(ExecutionBatch::tokenized_with_charges(
                    locations
                        .into_iter()
                        .zip(tokens)
                        .map(|(location, value)| ExecutionSentence { location, value })
                        .collect(),
                    charges,
                )))
            };
        }
        Ok(None)
    }
}

fn merge_graph_projection(
    physical: &BoundTokenSentence,
    logical: &BoundProjection,
    observation: &EnhancedGraphObservation,
) -> Result<BoundTokenSentence, CorpusError> {
    let adjacency = observation
        .word_adjacency_values(physical.len())
        .map_err(|message| {
            invalid(format!(
                "enhanced projection reconstruction failed: {message}"
            ))
        })?;
    let mut values = Vec::with_capacity(physical.len() * logical.len());
    for (token_index, token_adjacency) in adjacency.iter().enumerate() {
        for (key, (kind, provider)) in logical
            .keys()
            .iter()
            .zip(logical.kinds().iter().zip(logical.providers()))
        {
            let value = if *provider == AttributeProvider::EnhancedGraph {
                ProjectedAttributeValue::string_set(token_adjacency.iter().cloned())
            } else {
                match TokenSequence::value_key(physical, token_index, key) {
                    Some(AttributeValueRef::Scalar(value)) => {
                        ProjectedAttributeValue::scalar(value)
                    }
                    Some(AttributeValueRef::StringSet(value)) => {
                        ProjectedAttributeValue::string_set(value.iter().cloned())
                    }
                    None => match kind {
                        AttributeKind::Scalar => ProjectedAttributeValue::missing_scalar(),
                        AttributeKind::StringSet => {
                            ProjectedAttributeValue::string_set(std::iter::empty::<CompactString>())
                        }
                    },
                }
            };
            values.push(value);
        }
    }
    let offsets = (0..physical.len())
        .map(|token| physical.char_span(token))
        .collect::<Option<Vec<_>>>();
    BoundTokenSentence::from_projected_attribute_values(logical, physical.len(), values)
        .map(|sentence| sentence.with_char_offsets(offsets.map(Arc::new)))
        .map_err(|error| {
            invalid(format!(
                "enhanced projection reconstruction failed: {error}"
            ))
        })
}

fn bound_to_physical(
    sentence: &BoundTokenSentence,
    projection: &BoundProjection,
) -> Result<Vec<TokenInfo>, CorpusError> {
    let mut tokens = Vec::with_capacity(sentence.len());
    for token_index in 0..sentence.len() {
        let source = sentence
            .token(token_index)
            .ok_or_else(|| invalid("token index"))?;
        let mut token = TokenInfo::default();
        for (ordinal, key) in projection.keys().iter().enumerate() {
            let Some(physical) = TokenAttributeIndex::from_attribute_key(key) else {
                continue;
            };
            if let Some(value) = source.get(AttributeOrdinal::new(ordinal as u8)) {
                token.set(physical, value.into());
            }
        }
        tokens.push(token);
    }
    Ok(tokens)
}

fn encode_schema(schema: &TokenSchema) -> Result<Vec<u8>, CorpusError> {
    let mut bytes = Vec::new();
    for attribute in schema.attributes() {
        bytes.push(match attribute.kind() {
            AttributeKind::Scalar => 0,
            AttributeKind::StringSet => 1,
        });
        bytes.push(
            u8::try_from(attribute.key().as_bytes().len())
                .map_err(|_| invalid("schema key too long"))?,
        );
        bytes.extend_from_slice(attribute.key().as_bytes());
    }
    Ok(bytes)
}

fn decode_schema(bytes: &[u8]) -> Result<TokenSchema, CorpusError> {
    let mut cursor = 0;
    let mut attributes = Vec::new();
    while cursor < bytes.len() {
        let kind = match *bytes
            .get(cursor)
            .ok_or_else(|| invalid("truncated schema kind"))?
        {
            0 => AttributeKind::Scalar,
            1 => AttributeKind::StringSet,
            _ => return Err(invalid("unknown schema value kind")),
        };
        let len = usize::from(
            *bytes
                .get(cursor + 1)
                .ok_or_else(|| invalid("truncated schema key length"))?,
        );
        cursor += 2;
        let key = std::str::from_utf8(
            bytes
                .get(cursor..cursor + len)
                .ok_or_else(|| invalid("truncated schema key"))?,
        )
        .map_err(|_| invalid("schema key is not UTF-8"))?;
        attributes.push(SchemaAttribute::new(key, kind).map_err(|_| {
            // Name the one cause a researcher will actually hit. `form` was
            // the portable surface key until it was renamed `text`, so an
            // artifact published before that carries a key this build does
            // not accept. This does not make it readable — it says why, in
            // place of a decoder detail that reads like corruption.
            if key == "form" {
                invalid(
                    "artifact stores the retired portable key `form`; it predates the \
                     `text` rename and must be rebuilt from source",
                )
            } else {
                invalid(format!("invalid schema key `{key}`"))
            }
        })?);
        cursor += len;
    }
    TokenSchema::new(attributes).map_err(|error| invalid(format!("invalid schema: {error}")))
}

fn encode_header(
    analysis: AnalysisIdentity,
    schema: &TokenSchema,
    sentences: u64,
    tokens: u64,
    blocks: u64,
    directory: u64,
    residency: u64,
) -> Result<Vec<u8>, CorpusError> {
    let schema_len = encode_schema(schema)?.len();
    let mut bytes = Vec::with_capacity(HEADER_LEN as usize);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    bytes.extend_from_slice(&(schema.attributes().len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(schema_len as u32).to_le_bytes());
    bytes.extend_from_slice(analysis.as_bytes());
    bytes.extend_from_slice(schema.digest().as_bytes());
    bytes.extend_from_slice(&sentences.to_le_bytes());
    bytes.extend_from_slice(&tokens.to_le_bytes());
    bytes.extend_from_slice(&blocks.to_le_bytes());
    bytes.extend_from_slice(&directory.to_le_bytes());
    bytes.extend_from_slice(&residency.to_le_bytes());
    Ok(bytes)
}

fn encode_block(
    sentences: &[BoundTokenSentence],
    projection: &BoundProjection,
) -> Result<Vec<u8>, CorpusError> {
    let token_count = sentences.iter().map(BoundTokenSentence::len).sum::<usize>();
    // Not the default hasher: every token attribute value in the block is
    // looked up here, which made SipHash 16.5% of a `corpus build` profile
    // (`docs/results/2026-08-22-uncached-analyzer-path`). The keys are token
    // values from the corpus being prepared by the same process, so there is
    // no untrusted-input boundary for a hash flood to cross, and the string
    // table's order is insertion order rather than iteration order — the
    // encoded block is byte-identical either way.
    let mut ids = FxHashMap::<String, u32>::default();
    let mut strings = Vec::<String>::new();
    // One traversal, not two. Interning assigns the id and the cell is written
    // with it immediately, so each value is hashed once and each token row is
    // walked once. The cells trail the string table in the encoded block, so
    // they are built here and appended after it below; the bytes are the same
    // either order.
    let mut cells = Vec::<u8>::with_capacity(token_count * projection.len() * 4);
    for sentence in sentences {
        for token_index in 0..sentence.len() {
            let token = sentence
                .token(token_index)
                .ok_or_else(|| invalid("token index"))?;
            for ordinal in 0..projection.len() {
                let ordinal = AttributeOrdinal::new(ordinal as u8);
                let kind = projection
                    .kind(ordinal)
                    .ok_or_else(|| invalid("projection kind"))?;
                match (kind, token.value(ordinal)) {
                    (AttributeKind::Scalar, Some(AttributeValueRef::Scalar(value))) => {
                        let id = intern(value, &mut ids, &mut strings)?;
                        cells.extend_from_slice(&id.to_le_bytes());
                    }
                    (AttributeKind::Scalar, None) => {
                        cells.extend_from_slice(&0_u32.to_le_bytes());
                    }
                    (AttributeKind::StringSet, Some(AttributeValueRef::StringSet(values))) => {
                        cells.extend_from_slice(&(values.len() as u32).to_le_bytes());
                        for value in values {
                            let id = intern(value, &mut ids, &mut strings)?;
                            cells.extend_from_slice(&id.to_le_bytes());
                        }
                    }
                    (AttributeKind::StringSet, None) => {
                        cells.extend_from_slice(&0_u32.to_le_bytes());
                    }
                    (AttributeKind::Scalar, Some(AttributeValueRef::StringSet(_))) => {
                        return Err(invalid("scalar/set mismatch"));
                    }
                    (AttributeKind::StringSet, Some(AttributeValueRef::Scalar(_))) => {
                        return Err(invalid("set/scalar mismatch"));
                    }
                }
            }
        }
    }
    let mut string_bytes = Vec::new();
    string_bytes.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    for value in &strings {
        string_bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
        string_bytes.extend_from_slice(value.as_bytes());
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(BLOCK_MAGIC);
    bytes.extend_from_slice(&(projection.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(sentences.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(token_count as u64).to_le_bytes());
    bytes.extend_from_slice(&(string_bytes.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&string_bytes);
    let mut start = 0_u64;
    for sentence in sentences {
        bytes.extend_from_slice(&start.to_le_bytes());
        bytes.extend_from_slice(&(sentence.len() as u64).to_le_bytes());
        start += sentence.len() as u64;
    }
    for sentence in sentences {
        let present = sentence.char_span(0).is_some();
        bytes.extend_from_slice(&u32::from(present).to_le_bytes());
        if present {
            let mut previous = None;
            for token in 0..sentence.len() {
                let span = sentence
                    .char_span(token)
                    .ok_or_else(|| invalid("incomplete offsets"))?;
                validate_char_span(previous, span)?;
                bytes.extend_from_slice(&span.start.to_le_bytes());
                bytes.extend_from_slice(&span.end.to_le_bytes());
                previous = Some(span);
            }
        }
    }
    bytes.extend_from_slice(&cells);
    Ok(bytes)
}

fn validate_block(
    raw: &[u8],
    entry: DirectoryEntry,
    schema: &TokenSchema,
) -> Result<(), CorpusError> {
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .map_err(|error| invalid(error.to_string()))?;
    let locals = (0..entry.sentence_count as usize).collect::<Vec<_>>();
    let _ = decode_block(raw, entry, schema, &projection, &locals, true)?;
    Ok(())
}

fn decode_block(
    raw: &[u8],
    entry: DirectoryEntry,
    schema: &TokenSchema,
    projection: &BoundProjection,
    locals: &[usize],
    char_offsets: bool,
) -> Result<Vec<BoundTokenSentence>, CorpusError> {
    if slice(raw, 0, 8)? != BLOCK_MAGIC {
        return Err(invalid("wrong V1 block magic"));
    }
    let width = read_u32(raw, 8)? as usize;
    let sentence_count = read_u32(raw, 12)? as usize;
    let token_count = read_u64(raw, 16)? as usize;
    let strings_len = read_u64(raw, 24)?;
    if width != schema.attributes().len()
        || sentence_count as u64 != entry.sentence_count
        || token_count as u64 != entry.token_count
    {
        return Err(invalid("V1 block shape differs from its directory/schema"));
    }
    let strings_bytes = slice(raw, BLOCK_HEADER_LEN, strings_len)?;
    let strings = decode_strings(strings_bytes)?;
    let sentence_offset = BLOCK_HEADER_LEN + strings_len;
    let sentence_bytes = slice(
        raw,
        sentence_offset,
        entry.sentence_count * SENTENCE_ENTRY_LEN,
    )?;
    let cells_offset = sentence_offset + entry.sentence_count * SENTENCE_ENTRY_LEN;
    let mut ranges = Vec::with_capacity(sentence_count);
    let mut next = 0_u64;
    for index in 0..sentence_count {
        let start = read_u64(sentence_bytes, index as u64 * 16)?;
        let count = read_u64(sentence_bytes, index as u64 * 16 + 8)?;
        if start != next || start + count > entry.token_count {
            return Err(invalid("non-canonical sentence table"));
        }
        ranges.push(start as usize..(start + count) as usize);
        next = start + count;
    }
    if next != entry.token_count || locals.iter().any(|local| *local >= sentence_count) {
        return Err(invalid("invalid sentence selection"));
    }
    // Every caller already supplies these in ascending order — `decode_bound_ordinals`
    // refuses a decreasing corpus ordinal and `validate_block` passes `0..count`.
    // The cell loop's owner cursor depends on it, so it is checked rather than assumed.
    if locals.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid("sentence selection must be strictly increasing"));
    }
    let mut requested = vec![None; schema.attributes().len()];
    for (ordinal, key) in projection.keys().iter().enumerate() {
        let stored = schema
            .keys()
            .iter()
            .position(|candidate| candidate == key)
            .ok_or_else(|| invalid("projection key absent"))?;
        requested[stored] = Some(ordinal);
    }
    let mut selected = locals
        .iter()
        .map(|local| {
            let cells = ranges[*local]
                .len()
                .checked_mul(projection.len())
                .ok_or_else(|| invalid("projected cell count overflow"))?;
            Ok((
                ranges[*local].clone(),
                vec![ProjectedAttributeValue::missing_scalar(); cells],
            ))
        })
        .collect::<Result<Vec<_>, CorpusError>>()?;
    for (selected_index, (_, values)) in selected.iter_mut().enumerate() {
        for ordinal in 0..projection.len() {
            if projection.kind(AttributeOrdinal::new(ordinal as u8))
                == Some(AttributeKind::StringSet)
            {
                for token in 0..ranges[locals[selected_index]].len() {
                    values[token * projection.len() + ordinal] =
                        ProjectedAttributeValue::string_set(std::iter::empty::<CompactString>());
                }
            }
        }
    }
    let mut cursor = cells_offset as usize;
    let mut offsets = Vec::with_capacity(locals.len());
    let mut selected_sentence = 0;
    for (local, range) in ranges.iter().enumerate() {
        let present = match read_u32_at(raw, &mut cursor)? {
            0 => false,
            1 => true,
            _ => return Err(invalid("invalid offset availability")),
        };
        let selected = locals.get(selected_sentence) == Some(&local);
        let span_bytes = if present {
            let bytes = slice(raw, cursor as u64, range.len() as u64 * 8)?;
            cursor += bytes.len();
            bytes
        } else {
            &[]
        };
        let mut spans = if selected && present && char_offsets {
            Vec::with_capacity(range.len())
        } else {
            Vec::new()
        };
        if present {
            let mut previous = None;
            for &[a, b, c, d, e, f, g, h] in span_bytes.as_chunks::<8>().0 {
                let span = CharSpan::new(
                    u32::from_le_bytes([a, b, c, d]),
                    u32::from_le_bytes([e, f, g, h]),
                );
                validate_char_span(previous, span)?;
                if selected && char_offsets {
                    spans.push(span);
                }
                previous = Some(span);
            }
        }
        if selected {
            offsets.push((present && char_offsets).then(|| Arc::new(spans)));
            selected_sentence += 1;
        }
    }
    // The owning selected range is found by advancing a cursor rather than by
    // scanning `selected` from the start at every cell. Both orders that make
    // this sound are already established: the sentence table is canonical, so
    // `ranges` ascends and its entries are disjoint, and `locals` is checked
    // strictly increasing above. `token` only increases, so the owner can only
    // move forward.
    let mut owner_cursor = 0_usize;
    for token in 0..token_count {
        while owner_cursor < selected.len() && selected[owner_cursor].0.end <= token {
            owner_cursor += 1;
        }
        let owner = (owner_cursor < selected.len() && selected[owner_cursor].0.start <= token)
            .then_some(owner_cursor);
        for (stored, attribute) in schema.attributes().iter().enumerate() {
            match attribute.kind() {
                AttributeKind::Scalar => {
                    let id = read_u32_at(raw, &mut cursor)?;
                    if id as usize > strings.len() {
                        return Err(invalid("invalid scalar string id"));
                    }
                    if let Some(projected) = requested[stored]
                        && let Some(local_index) = owner
                    {
                        let start = selected[local_index].0.start;
                        selected[local_index].1[(token - start) * projection.len() + projected] =
                            if id == 0 {
                                ProjectedAttributeValue::missing_scalar()
                            } else {
                                ProjectedAttributeValue::scalar(strings[id as usize - 1])
                            };
                    }
                }
                AttributeKind::StringSet => {
                    let count = read_u32_at(raw, &mut cursor)? as usize;
                    let mut set = Vec::with_capacity(count);
                    for _ in 0..count {
                        let id = read_u32_at(raw, &mut cursor)? as usize;
                        if id == 0 || id > strings.len() {
                            return Err(invalid("invalid set string id"));
                        }
                        set.push(CompactString::new(strings[id - 1]));
                    }
                    if let Some(projected) = requested[stored]
                        && let Some(local_index) = owner
                    {
                        let start = selected[local_index].0.start;
                        selected[local_index].1[(token - start) * projection.len() + projected] =
                            ProjectedAttributeValue::StringSet(set);
                    }
                }
            }
        }
    }
    if cursor != raw.len() {
        return Err(invalid("trailing V1 block bytes"));
    }
    selected
        .into_iter()
        .zip(offsets)
        .map(|((range, values), offsets)| {
            BoundTokenSentence::from_projected_attribute_values(projection, range.len(), values)
                .map(|sentence| sentence.with_char_offsets(offsets))
                .map_err(|_| invalid("decoded values disagree with projection"))
        })
        .collect()
}

fn validate_char_span(previous: Option<CharSpan>, span: CharSpan) -> Result<(), CorpusError> {
    if span.start >= span.end
        || previous.is_some_and(|previous| previous != span && previous.end > span.start)
    {
        return Err(invalid("invalid or unordered character offsets"));
    }
    Ok(())
}

/// Returns the id this value already carries, assigning the next one when the
/// block has not seen it before. Ids start at 1; 0 is the absent-value marker.
fn intern(
    value: &str,
    ids: &mut FxHashMap<String, u32>,
    strings: &mut Vec<String>,
) -> Result<u32, CorpusError> {
    if let Some(&id) = ids.get(value) {
        return Ok(id);
    }
    let id = u32::try_from(strings.len() + 1).map_err(|_| invalid("too many interned strings"))?;
    ids.insert(value.to_owned(), id);
    strings.push(value.to_owned());
    Ok(id)
}

fn decode_strings(bytes: &[u8]) -> Result<Vec<&str>, CorpusError> {
    let count = read_u32(bytes, 0)? as usize;
    let mut cursor = 4;
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_u32_at(bytes, &mut cursor)? as usize;
        let value = std::str::from_utf8(
            bytes
                .get(cursor..cursor + len)
                .ok_or_else(|| invalid("truncated string"))?,
        )
        .map_err(|_| invalid("string is not UTF-8"))?;
        strings.push(value);
        cursor += len;
    }
    if cursor != bytes.len() {
        return Err(invalid("trailing string table bytes"));
    }
    Ok(strings)
}

fn encoded_sentence_upper_bound(
    sentence: &BoundTokenSentence,
    projection: &BoundProjection,
) -> Result<u64, CorpusError> {
    let mut bytes = SENTENCE_ENTRY_LEN + 4;
    if sentence.char_span(0).is_some() {
        bytes = bytes
            .checked_add(
                (sentence.len() as u64)
                    .checked_mul(8)
                    .ok_or_else(|| invalid("offset size overflow"))?,
            )
            .ok_or_else(|| invalid("offset size overflow"))?;
    }
    for token_index in 0..sentence.len() {
        let token = sentence
            .token(token_index)
            .ok_or_else(|| invalid("token index"))?;
        for ordinal in 0..projection.len() {
            let ordinal = AttributeOrdinal::new(ordinal as u8);
            bytes = bytes
                .checked_add(4)
                .ok_or_else(|| invalid("sentence size overflow"))?;
            match token.value(ordinal) {
                Some(AttributeValueRef::Scalar(value)) => {
                    bytes = bytes
                        .checked_add(4 + value.len() as u64)
                        .ok_or_else(|| invalid("sentence size overflow"))?
                }
                Some(AttributeValueRef::StringSet(values)) => {
                    for value in values {
                        bytes = bytes
                            .checked_add(4 + value.len() as u64)
                            .ok_or_else(|| invalid("sentence size overflow"))?;
                    }
                }
                None => {}
            }
        }
    }
    Ok(bytes)
}

fn encode_directory_entry(entry: DirectoryEntry) -> [u8; 80] {
    let mut bytes = [0; 80];
    for (index, value) in [
        entry.first_sentence,
        entry.sentence_count,
        entry.file_offset,
        entry.stored_len,
        entry.raw_len,
        entry.token_count,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes[48..].copy_from_slice(&entry.checksum);
    bytes
}

fn decode_directory_entry(bytes: &[u8]) -> Result<DirectoryEntry, CorpusError> {
    Ok(DirectoryEntry {
        first_sentence: read_u64(bytes, 0)?,
        sentence_count: read_u64(bytes, 8)?,
        file_offset: read_u64(bytes, 16)?,
        stored_len: read_u64(bytes, 24)?,
        raw_len: read_u64(bytes, 32)?,
        token_count: read_u64(bytes, 40)?,
        checksum: read_array(bytes, 48)?,
    })
}

fn read_u32(bytes: &[u8], offset: u64) -> Result<u32, CorpusError> {
    Ok(u32::from_le_bytes(
        slice(bytes, offset, 4)?
            .try_into()
            .map_err(|_| invalid("u32 width"))?,
    ))
}
fn read_u64(bytes: &[u8], offset: u64) -> Result<u64, CorpusError> {
    Ok(u64::from_le_bytes(
        slice(bytes, offset, 8)?
            .try_into()
            .map_err(|_| invalid("u64 width"))?,
    ))
}
fn read_u32_at(bytes: &[u8], cursor: &mut usize) -> Result<u32, CorpusError> {
    let value = u32::from_le_bytes(
        bytes
            .get(*cursor..*cursor + 4)
            .ok_or_else(|| invalid("truncated cell"))?
            .try_into()
            .map_err(|_| invalid("cell width"))?,
    );
    *cursor += 4;
    Ok(value)
}
fn read_array<const N: usize>(bytes: &[u8], offset: u64) -> Result<[u8; N], CorpusError> {
    slice(bytes, offset, N as u64)?
        .try_into()
        .map_err(|_| invalid("fixed field width"))
}
fn slice(bytes: &[u8], offset: u64, len: u64) -> Result<&[u8], CorpusError> {
    let start = usize::try_from(offset).map_err(|_| invalid("offset exceeds usize"))?;
    let len = usize::try_from(len).map_err(|_| invalid("length exceeds usize"))?;
    bytes
        .get(
            start
                ..start
                    .checked_add(len)
                    .ok_or_else(|| invalid("range overflow"))?,
        )
        .ok_or(CorpusError::Truncated {
            component: COMPONENT,
            offset,
        })
}
fn scratch_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
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
mod tests {
    use std::sync::Arc;

    use lingrep_core::{
        spill::TempBudget,
        token_match::{
            AttributeKey, AttributeKind, AttributeOrdinal, AttributeValueRef, BoundTokenSentence,
            ProjectedAttributeValue, SchemaAttribute,
        },
    };

    use super::*;

    #[test]
    fn block_samples_use_real_boundaries_and_leave_excluded_blocks_lazy() {
        use crate::locations::LocationWriter;
        use lingrep_core::execution::CorpusSample;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tokens");
        let location_path = directory.path().join("locations");
        let schema = TokenSchema::from_keys(["text"]).unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let identity = AnalysisIdentity::from_bytes([15; 32]);
        let schema_id = TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes());
        let budget = Arc::new(TempBudget::new(16 * 1024 * 1024));
        let mut writer = TokenWriter::create(&path, schema.clone(), Arc::clone(&budget)).unwrap();
        let mut locations = LocationWriter::create(&location_path, budget).unwrap();
        let mut ordinal = 0;
        for count in [3, 256, 7, 256, 253] {
            for _ in 0..count {
                writer
                    .push_bound_sentence(
                        BoundTokenSentence::from_projected_values(
                            &projection,
                            1,
                            vec![Some(format!("word-{ordinal}").into())],
                        )
                        .unwrap(),
                    )
                    .unwrap();
                locations
                    .append(&crate::SentenceLocation {
                        source_metadata: None,
                        corpus_sentence_ordinal: ordinal,
                        document_id: crate::DocumentId::new(0),
                        document_ordinal: 0,
                        document_sentence_ordinal: ordinal,
                        source_name: Arc::from("source"),
                    })
                    .unwrap();
                ordinal += 1;
            }
            writer.flush_block().unwrap();
        }
        writer.finish(identity).unwrap();
        locations.finish(identity).unwrap();
        let reader = TokenReader::open(&path, &identity, &schema_id).unwrap();
        let blocks = (0..reader.block_count)
            .map(|i| reader.directory_entry(i).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            blocks.iter().map(|b| b.first_sentence).collect::<Vec<_>>(),
            [0, 3, 259, 266, 522]
        );
        drop(reader);
        let sample = CorpusSample::new(500_000, 7).unwrap();
        let expected = blocks
            .iter()
            .filter(|b| sample.admits(b.first_sentence))
            .flat_map(|b| b.first_sentence..b.first_sentence + b.sentence_count)
            .collect::<Vec<_>>();
        assert!(!expected.is_empty() && expected.len() < ordinal as usize);
        let open = |sample| {
            PreparedSentenceSource::new_bound_deferred(
                TokenReader::open(&path, &identity, &schema_id).unwrap(),
                None,
                MappedLocations::open(&location_path, &identity).unwrap(),
                &schema,
                projection.clone(),
            )
            .unwrap()
            .with_block_sample(sample)
        };
        let drain = |source: &mut PreparedSentenceSource, batch_size| {
            let limits = ExecutionLimits {
                max_stage_sentences: batch_size,
                ..ExecutionLimits::default()
            };
            let mut ordinals = Vec::new();
            while let Some(batch) = source.next_batch(&limits).unwrap() {
                let ExecutionBatch::BoundTokenized { sentences, .. } = batch else {
                    panic!("bound source");
                };
                assert!(!sentences.is_empty());
                ordinals.extend(sentences.iter().map(|s| s.location.corpus_sentence_ordinal));
            }
            ordinals
        };
        for batch_size in [1, 17, 1024] {
            let mut source = open(Some(sample));
            assert_eq!(drain(&mut source, batch_size), expected);
            if batch_size == 1024 {
                assert_eq!(
                    source.decoded_block_count(),
                    blocks
                        .iter()
                        .filter(|b| sample.admits(b.first_sentence))
                        .count()
                );
                assert_eq!(source.decoded_sentence_count(), expected.len());
            }
            let scope = (0..ordinal).filter(|id| id % 3 == 0).collect();
            let mut source = open(Some(sample)).with_admission(SentenceAdmission::Sentences(scope));
            assert_eq!(
                drain(&mut source, batch_size),
                expected
                    .iter()
                    .copied()
                    .filter(|id| id % 3 == 0)
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(
            drain(&mut open(CorpusSample::new(1_000_000, 7)), 1024),
            (0..ordinal).collect::<Vec<_>>()
        );
        assert!(drain(&mut open(CorpusSample::new(1, 7)), 1024).is_empty());
        let original = std::fs::read(&path).unwrap();
        for selected in [false, true] {
            let block = blocks
                .iter()
                .find(|b| sample.admits(b.first_sentence) == selected)
                .unwrap();
            let mut bytes = original.clone();
            bytes[(block.file_offset + block.stored_len / 2) as usize] ^= 1;
            std::fs::write(&path, &bytes).unwrap();
            let mut source = open(Some(sample));
            if selected {
                assert!(source.next_batch(&ExecutionLimits::default()).is_err());
                assert!(source.next_batch(&ExecutionLimits::default()).is_err());
            } else {
                assert_eq!(drain(&mut source, 1024), expected);
            }
            assert!(
                TokenReader::open(&path, &identity, &schema_id)
                    .unwrap()
                    .verify_integrity()
                    .is_err()
            );
        }
    }

    #[test]
    fn typed_mixed_rows_round_trip_deterministically_and_support_subsets() {
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
            SchemaAttribute::new("spacy.morph.reading", AttributeKind::StringSet).unwrap(),
        ])
        .unwrap();
        let complete = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            &complete,
            2,
            vec![
                ProjectedAttributeValue::scalar("cats"),
                ProjectedAttributeValue::string_set(["Sing", "Plur", "Sing"]),
                ProjectedAttributeValue::string_set(["キャッツ"]),
                ProjectedAttributeValue::scalar("run"),
                ProjectedAttributeValue::string_set(std::iter::empty::<&str>()),
                ProjectedAttributeValue::string_set(["ラン"]),
            ],
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.v1");
        let second = directory.path().join("second.v1");
        let analysis = AnalysisIdentity::from_bytes([9; 32]);
        for path in [&first, &second] {
            let mut writer =
                TokenWriter::create(path, schema.clone(), Arc::new(TempBudget::new(1024 * 1024)))
                    .unwrap();
            writer.push_bound_sentence(sentence.clone()).unwrap();
            writer.finish(analysis).unwrap();
        }
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap()
        );

        let schema_identity = TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes());
        let reader = TokenReader::open(&first, &analysis, &schema_identity).unwrap();
        let subset = schema
            .bind_projection([
                AttributeKey::new("text").unwrap(),
                AttributeKey::new("feat.Number").unwrap(),
            ])
            .unwrap();
        let decoded = reader.decode_bound_ordinals(&[0], &subset, true).unwrap();
        let token = decoded[0].token(0).unwrap();
        assert_eq!(
            token.value(AttributeOrdinal::new(0)),
            Some(AttributeValueRef::Scalar("cats"))
        );
        assert_eq!(
            token.value(AttributeOrdinal::new(1)),
            Some(AttributeValueRef::StringSet(&[
                "Plur".into(),
                "Sing".into()
            ]))
        );
        assert_eq!(
            decoded[0].token(1).unwrap().value(AttributeOrdinal::new(1)),
            None
        );
        reader.verify_integrity().unwrap();
    }

    #[test]
    fn typed_offsets_preserve_selected_spans_and_refuse_malformed_tables() {
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
        ])
        .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let words =
            BoundTokenSentence::from_projected_values(&projection, 3, vec![Some("字".into()); 6])
                .unwrap();
        let spans = vec![
            CharSpan::new(1, 3),
            CharSpan::new(1, 3),
            CharSpan::new(5, 6),
        ];
        let sentences = vec![
            words
                .clone()
                .with_char_offsets(Some(Arc::new(spans.clone()))),
            words.clone(),
            words.with_char_offsets(Some(Arc::new(spans.clone()))),
        ];
        let raw = encode_block(&sentences, &projection).unwrap();
        let entry = DirectoryEntry {
            first_sentence: 0,
            sentence_count: 3,
            file_offset: 0,
            stored_len: 0,
            raw_len: raw.len() as u64,
            token_count: 9,
            checksum: [0; 32],
        };
        let projected = schema
            .bind_projection([AttributeKey::new("text").unwrap()])
            .unwrap();
        for char_offsets in [true, false] {
            for locals in [vec![0, 1, 2], vec![0, 2], vec![1], vec![2]] {
                let decoded =
                    decode_block(&raw, entry, &schema, &projected, &locals, char_offsets).unwrap();
                for (sentence, local) in decoded.iter().zip(locals) {
                    for token in 0..3 {
                        assert_eq!(
                            sentence.char_span(token),
                            if char_offsets {
                                sentences[local].char_span(token)
                            } else {
                                None
                            }
                        );
                    }
                }
            }
        }
        let offset_start = (BLOCK_HEADER_LEN
            + read_u64(&raw, 24).unwrap()
            + entry.sentence_count * SENTENCE_ENTRY_LEN) as usize;
        for (position, value) in [
            (offset_start, 2u32),
            (offset_start + 8, 0),
            (offset_start + 12, 2),
            (offset_start + 20, 0),
        ] {
            let mut malformed = raw.clone();
            malformed[position..position + 4].copy_from_slice(&value.to_le_bytes());
            for char_offsets in [true, false] {
                assert!(
                    decode_block(&malformed, entry, &schema, &projected, &[1], char_offsets)
                        .is_err()
                );
            }
        }
        assert!(
            decode_block(
                &raw[..offset_start + 9],
                entry,
                &schema,
                &projected,
                &[0],
                true
            )
            .is_err()
        );
        let bad = sentences[0]
            .clone()
            .with_char_offsets(Some(Arc::new(vec![CharSpan::new(2, 1); 3])));
        assert!(encode_block(&[bad], &projection).is_err());
    }

    /// `encode_block` walks each token row once, interning a value and writing
    /// the cell that refers to it in the same step. Two properties follow, and
    /// an artifact's identity is its bytes, so a future restructuring that
    /// breaks either one renames every prepared artifact rather than merely
    /// encoding them differently:
    ///
    /// * the string table is ordered by **first encounter** along that single
    ///   traversal — sentence, then token, then attribute ordinal, then set
    ///   member;
    /// * a repeat of a value already seen — later in the same set, under
    ///   another attribute, or in a later token — refers to the existing id
    ///   rather than adding a second entry.
    ///
    /// The row below mixes every case the traversal distinguishes: a scalar, a
    /// set, an absent scalar, an empty set, and one value shared between a set
    /// member and a later scalar. The set is authored `["Sing", "Plur",
    /// "Sing"]` and reaches the encoder as `["Plur", "Sing"]`, because the
    /// dense row deduplicates and orders set members on construction — which
    /// is why `Plur` is interned first.
    #[test]
    fn typed_block_interns_in_first_encounter_order_along_one_traversal() {
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
        ])
        .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            &projection,
            3,
            vec![
                ProjectedAttributeValue::scalar("走る"),
                ProjectedAttributeValue::scalar("走る"),
                ProjectedAttributeValue::string_set(["Sing", "Plur", "Sing"]),
                ProjectedAttributeValue::scalar("犬"),
                ProjectedAttributeValue::missing_scalar(),
                ProjectedAttributeValue::string_set(std::iter::empty::<&str>()),
                ProjectedAttributeValue::scalar("Plur"),
                ProjectedAttributeValue::scalar("走る"),
                ProjectedAttributeValue::string_set(std::iter::empty::<&str>()),
            ],
        )
        .unwrap();
        let raw = encode_block(std::slice::from_ref(&sentence), &projection).unwrap();

        let strings_len = read_u64(&raw, 24).unwrap() as usize;
        let table = decode_strings(slice(&raw, 32, strings_len as u64).unwrap()).unwrap();
        assert_eq!(table, ["走る", "Plur", "Sing", "犬"]);

        // The cells refer to that table, so decoding reproduces the row.
        let entry = DirectoryEntry {
            first_sentence: 0,
            sentence_count: 1,
            file_offset: 0,
            stored_len: 0,
            raw_len: raw.len() as u64,
            token_count: 3,
            checksum: [0; 32],
        };
        let decoded = decode_block(&raw, entry, &schema, &projection, &[0], true).unwrap();
        assert_eq!(decoded[0], sentence);
    }

    /// Blocks cross a channel to the recorder thread, so the property that
    /// used to be a consequence of single-threaded control flow — the file
    /// holds blocks in push order, and each block its sentences in push order
    /// — is now a consequence of the channel being FIFO with one producer and
    /// one consumer. This writes **three** blocks rather than one, because a
    /// single-block artifact cannot observe a reordering at all.
    #[test]
    fn typed_multi_block_writes_are_ordered_and_byte_identical() {
        let schema = TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
        ])
        .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let sentences = (0..BLOCK_SENTENCE_LIMIT * 2 + 7)
            .map(|index| {
                BoundTokenSentence::from_projected_attribute_values(
                    &projection,
                    2,
                    vec![
                        ProjectedAttributeValue::scalar(format!("surface-{index}")),
                        ProjectedAttributeValue::scalar("共通"),
                        ProjectedAttributeValue::scalar(format!("surface-{index}-b")),
                        ProjectedAttributeValue::missing_scalar(),
                    ],
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let directory = tempfile::tempdir().unwrap();
        let analysis = AnalysisIdentity::from_bytes([11; 32]);
        let paths = ["first.v1", "second.v1"].map(|name| directory.path().join(name));
        for path in &paths {
            let mut writer =
                TokenWriter::create(path, schema.clone(), Arc::new(TempBudget::new(1 << 20)))
                    .unwrap();
            for sentence in &sentences {
                writer.push_bound_sentence(sentence.clone()).unwrap();
            }
            writer.finish(analysis).unwrap();
        }
        assert_eq!(
            std::fs::read(&paths[0]).unwrap(),
            std::fs::read(&paths[1]).unwrap(),
        );

        let identity = TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes());
        let reader = TokenReader::open(&paths[0], &analysis, &identity).unwrap();
        let locals = (0..sentences.len() as u64).collect::<Vec<_>>();
        let decoded = reader
            .decode_bound_ordinals(&locals, &projection, true)
            .unwrap();
        assert_eq!(decoded, sentences);
        reader.verify_integrity().unwrap();
    }

    /// A failure inside the recorder has to reach the caller. It cannot be
    /// returned by the push that caused it — that push has already gone — so
    /// it surfaces at the next block boundary or at `finish`, whichever comes
    /// first. What must not happen is a silent success or a hang.
    ///
    /// The trigger is the temporary-storage budget, which the recorder charges
    /// for every residency entry it appends.
    #[test]
    fn typed_a_recorder_failure_reaches_the_caller() {
        let schema =
            TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar).unwrap()])
                .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            &projection,
            1,
            vec![ProjectedAttributeValue::scalar("語")],
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tokens.v1.zst");
        // Room for two residency entries, against a block of 256 sentences.
        let mut writer = TokenWriter::create(&path, schema, Arc::new(TempBudget::new(16))).unwrap();
        let mut outcome = Ok(());
        for _ in 0..=BLOCK_SENTENCE_LIMIT {
            outcome = writer.push_bound_sentence(sentence.clone());
            if outcome.is_err() {
                break;
            }
        }
        let outcome = outcome.and_then(|()| writer.finish(AnalysisIdentity::from_bytes([5; 32])));
        let error = outcome.expect_err("the exhausted budget must reach the caller");
        assert!(
            error.to_string().contains("temporary limit exceeded"),
            "expected the budget error, got: {error}"
        );
    }

    #[test]
    fn typed_reader_refuses_a_schema_identity_from_another_kind() {
        let schema =
            TokenSchema::new([
                SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
            ])
            .unwrap();
        let scalar_schema =
            TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::Scalar).unwrap()])
                .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tokens.v1.zst");
        let writer =
            TokenWriter::create(&path, schema, Arc::new(TempBudget::new(1024 * 1024))).unwrap();
        writer
            .finish(AnalysisIdentity::from_bytes([3; 32]))
            .unwrap();
        let wrong = TokenSchemaIdentity::from_bytes(*scalar_schema.digest().as_bytes());
        assert!(TokenReader::open(&path, &AnalysisIdentity::from_bytes([3; 32]), &wrong).is_err());
    }
}
