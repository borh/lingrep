//! Bounded exact counting backed by leveled sorted temporary runs.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tempfile::{NamedTempFile, TempDir};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempBudgetSnapshot {
    limit_bytes: u64,
    used_bytes: u64,
    peak_used_bytes: u64,
}

impl TempBudgetSnapshot {
    pub fn limit_bytes(self) -> u64 {
        self.limit_bytes
    }

    pub fn used_bytes(self) -> u64 {
        self.used_bytes
    }

    pub fn peak_used_bytes(self) -> u64 {
        self.peak_used_bytes
    }

    pub fn headroom_bytes(self) -> u64 {
        self.limit_bytes.saturating_sub(self.used_bytes)
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "temporary limit exceeded: requested-additional-bytes={requested_additional_bytes} \
     used-bytes={} limit-bytes={}",
    budget.used_bytes(),
    budget.limit_bytes()
)]
pub struct TempLimitExceeded {
    requested_additional_bytes: u64,
    budget: TempBudgetSnapshot,
}

impl TempLimitExceeded {
    pub fn requested_additional_bytes(&self) -> u64 {
        self.requested_additional_bytes
    }

    pub fn budget(&self) -> TempBudgetSnapshot {
        self.budget
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TempCapacityError {
    #[error(transparent)]
    LimitExceeded(#[from] TempLimitExceeded),
    #[error(
        "temporary budget accounting overflow: requested-additional-bytes={requested_additional_bytes} \
         used-bytes={} limit-bytes={}",
        budget.used_bytes(),
        budget.limit_bytes()
    )]
    AccountingOverflow {
        requested_additional_bytes: u64,
        budget: TempBudgetSnapshot,
    },
}

impl TempCapacityError {
    pub fn requested_additional_bytes(&self) -> u64 {
        match self {
            Self::LimitExceeded(error) => error.requested_additional_bytes(),
            Self::AccountingOverflow {
                requested_additional_bytes,
                ..
            } => *requested_additional_bytes,
        }
    }

    pub fn budget(&self) -> TempBudgetSnapshot {
        match self {
            Self::LimitExceeded(error) => error.budget(),
            Self::AccountingOverflow { budget, .. } => *budget,
        }
    }
}

fn capacity_io_error(error: TempCapacityError) -> io::Error {
    io::Error::new(io::ErrorKind::StorageFull, error)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillCounter {
    PendingBytes,
    RunBytes,
    InitialRunBytesWritten,
    TotalRunBytesWritten,
}

#[derive(Debug, thiserror::Error)]
pub enum SpillAccountingOverflow {
    #[error(
        "spill accounting overflow for {counter:?}: current={current} \
         requested-additional={requested_additional}"
    )]
    Counter {
        counter: SpillCounter,
        current: u64,
        requested_additional: u64,
    },
    #[error(
        "spill estimated-key byte accounting overflow: serialized={serialized_key_bytes} \
         key-inline={key_inline_bytes} count-inline={count_inline_bytes} \
         overhead={accounting_overhead_bytes}"
    )]
    EstimatedKeyBytes {
        serialized_key_bytes: usize,
        key_inline_bytes: usize,
        count_inline_bytes: usize,
        accounting_overhead_bytes: usize,
    },
}

impl SpillAccountingOverflow {
    pub fn counter(&self) -> Option<SpillCounter> {
        match self {
            Self::Counter { counter, .. } => Some(*counter),
            Self::EstimatedKeyBytes { .. } => None,
        }
    }

    pub fn current(&self) -> Option<u64> {
        match self {
            Self::Counter { current, .. } => Some(*current),
            Self::EstimatedKeyBytes { .. } => None,
        }
    }

    pub fn requested_additional(&self) -> Option<u64> {
        match self {
            Self::Counter {
                requested_additional,
                ..
            } => Some(*requested_additional),
            Self::EstimatedKeyBytes { .. } => None,
        }
    }

    pub fn estimated_key_components(&self) -> Option<(usize, usize, usize, usize)> {
        match self {
            Self::Counter { .. } => None,
            Self::EstimatedKeyBytes {
                serialized_key_bytes,
                key_inline_bytes,
                count_inline_bytes,
                accounting_overhead_bytes,
            } => Some((
                *serialized_key_bytes,
                *key_inline_bytes,
                *count_inline_bytes,
                *accounting_overhead_bytes,
            )),
        }
    }
}

fn spill_counter_overflow(
    counter: SpillCounter,
    current: u64,
    requested_additional: u64,
) -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        SpillAccountingOverflow::Counter {
            counter,
            current,
            requested_additional,
        },
    )
}

fn estimated_key_bytes_overflow(
    serialized_key_bytes: usize,
    key_inline_bytes: usize,
    count_inline_bytes: usize,
    accounting_overhead_bytes: usize,
) -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        SpillAccountingOverflow::EstimatedKeyBytes {
            serialized_key_bytes,
            key_inline_bytes,
            count_inline_bytes,
            accounting_overhead_bytes,
        },
    )
}

#[derive(Debug, thiserror::Error)]
#[error("spill count overflow: current={current} requested-additional={requested_additional}")]
pub struct SpillCountOverflow {
    current: usize,
    requested_additional: usize,
}

impl SpillCountOverflow {
    pub fn current(&self) -> usize {
        self.current
    }

    pub fn requested_additional(&self) -> usize {
        self.requested_additional
    }
}

fn count_overflow(current: usize, requested_additional: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        SpillCountOverflow {
            current,
            requested_additional,
        },
    )
}

#[derive(Debug, thiserror::Error)]
#[error("spill record is too large: record-bytes={record_bytes}")]
pub struct SpillRecordTooLarge {
    record_bytes: usize,
}

impl SpillRecordTooLarge {
    pub fn new(record_bytes: usize) -> Self {
        Self { record_bytes }
    }

    pub fn record_bytes(&self) -> usize {
        self.record_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillCodecOperation {
    EstimateKey,
    EncodeRecord,
    DecodeRecord,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "spill codec {operation:?} failed at run={run_id:?} level={run_level:?} \
     byte-offset={byte_offset:?}: {source}"
)]
pub struct SpillCodecError {
    operation: SpillCodecOperation,
    run_id: Option<u64>,
    run_level: Option<u32>,
    /// Offset within the **decoded** record stream, not the compressed file.
    /// Runs are stored as zstd streams; a compressed offset would not locate
    /// a record.
    byte_offset: Option<u64>,
    #[source]
    source: SpillCodecFailure,
}

/// What went wrong inside the run codec.
///
/// Runs are framed as `[u32 length][MessagePack record]`, so a corrupt run
/// surfaces either as a malformed frame — which is an [`io::Error`] before
/// `rmp_serde` is reached — or as a decode failure on the payload.
#[derive(Debug, thiserror::Error)]
pub enum SpillCodecFailure {
    #[error(transparent)]
    Encode(#[from] rmp_serde::encode::Error),
    #[error(transparent)]
    Decode(#[from] rmp_serde::decode::Error),
}

impl SpillCodecError {
    fn without_run(operation: SpillCodecOperation, source: impl Into<SpillCodecFailure>) -> Self {
        Self {
            operation,
            run_id: None,
            run_level: None,
            byte_offset: None,
            source: source.into(),
        }
    }

    fn for_run(
        operation: SpillCodecOperation,
        run_id: u64,
        run_level: u32,
        byte_offset: u64,
        source: impl Into<SpillCodecFailure>,
    ) -> Self {
        Self {
            operation,
            run_id: Some(run_id),
            run_level: Some(run_level),
            byte_offset: Some(byte_offset),
            source: source.into(),
        }
    }

    pub fn operation(&self) -> SpillCodecOperation {
        self.operation
    }

    pub fn run_id(&self) -> Option<u64> {
        self.run_id
    }

    pub fn run_level(&self) -> Option<u32> {
        self.run_level
    }

    pub fn byte_offset(&self) -> Option<u64> {
        self.byte_offset
    }
}

fn codec_io_error(error: SpillCodecError) -> io::Error {
    io::Error::other(error)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillOperation {
    Flush,
    Compact,
    Finalize,
    Visit,
}

#[derive(Debug, thiserror::Error)]
#[error("{operation:?} spill operation failed: {source}")]
pub struct SpillOperationError {
    operation: SpillOperation,
    spill: SpillSnapshot,
    budget: TempBudgetSnapshot,
    #[source]
    source: io::Error,
}

impl SpillOperationError {
    pub fn operation(&self) -> SpillOperation {
        self.operation
    }

    pub fn spill(&self) -> SpillSnapshot {
        self.spill
    }

    pub fn budget(&self) -> TempBudgetSnapshot {
        self.budget
    }

    pub fn source_error(&self) -> &io::Error {
        &self.source
    }
}

fn contains_operation_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<SpillOperationError>().is_some()
        || error
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            .is_some_and(|source| contains_operation_error(source))
        || error.source().is_some_and(contains_operation_error)
}

fn operation_io_error(
    operation: SpillOperation,
    spill: SpillSnapshot,
    budget: TempBudgetSnapshot,
    source: io::Error,
) -> io::Error {
    if contains_operation_error(&source) {
        return source;
    }
    let kind = source.kind();
    io::Error::new(
        kind,
        SpillOperationError {
            operation,
            spill,
            budget,
            source,
        },
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct Record<K> {
    key: K,
    count: usize,
}

/// Shared byte budget for private temporary artifacts.
#[derive(Debug)]
pub struct TempBudget {
    limit: u64,
    used: AtomicU64,
    peak_used: AtomicU64,
}

impl TempBudget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: AtomicU64::new(0),
            peak_used: AtomicU64::new(0),
        }
    }

    pub fn used_bytes(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    pub fn peak_used_bytes(&self) -> u64 {
        self.peak_used.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> TempBudgetSnapshot {
        TempBudgetSnapshot {
            limit_bytes: self.limit,
            used_bytes: self.used.load(Ordering::Acquire),
            peak_used_bytes: self.peak_used.load(Ordering::Acquire),
        }
    }

    fn try_reserve_additional(&self, bytes: u64) -> Result<(), TempCapacityError> {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let snapshot = TempBudgetSnapshot {
                limit_bytes: self.limit,
                used_bytes: used,
                peak_used_bytes: self.peak_used.load(Ordering::Acquire),
            };
            let next = used
                .checked_add(bytes)
                .ok_or(TempCapacityError::AccountingOverflow {
                    requested_additional_bytes: bytes,
                    budget: snapshot,
                })?;
            if next > self.limit {
                return Err(TempLimitExceeded {
                    requested_additional_bytes: bytes,
                    budget: snapshot,
                }
                .into());
            }
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.peak_used.fetch_max(next, Ordering::AcqRel);
                    return Ok(());
                }
                Err(actual) => used = actual,
            }
        }
    }

    pub fn try_reserve(self: &Arc<Self>, bytes: u64) -> Result<TempReservation, TempCapacityError> {
        self.try_reserve_additional(bytes)?;
        Ok(TempReservation {
            budget: self.clone(),
            held_bytes: bytes,
        })
    }

    pub fn reserve(self: &Arc<Self>, bytes: u64) -> io::Result<TempReservation> {
        self.try_reserve(bytes).map_err(capacity_io_error)
    }
}

#[derive(Debug)]
pub struct TempReservation {
    budget: Arc<TempBudget>,
    held_bytes: u64,
}

impl TempReservation {
    pub fn try_grow(&mut self, bytes: u64) -> Result<(), TempCapacityError> {
        if bytes == 0 {
            return Ok(());
        }
        let next =
            self.held_bytes
                .checked_add(bytes)
                .ok_or(TempCapacityError::AccountingOverflow {
                    requested_additional_bytes: bytes,
                    budget: self.budget.snapshot(),
                })?;
        self.budget.try_reserve_additional(bytes)?;
        self.held_bytes = next;
        Ok(())
    }

    pub fn grow(&mut self, bytes: u64) -> io::Result<()> {
        self.try_grow(bytes).map_err(capacity_io_error)
    }

    pub fn held_bytes(&self) -> u64 {
        self.held_bytes
    }
}

impl Drop for TempReservation {
    fn drop(&mut self) {
        self.budget
            .used
            .fetch_sub(self.held_bytes, Ordering::AcqRel);
    }
}

/// Charges a [`TempBudget`] for every byte before that byte reaches `inner`.
///
/// This is the single adapter that enforces the temporary-storage bound. The
/// bound is only as strong as this type, so there must be exactly one of it:
/// two implementations can drift, and the weaker one silently becomes the
/// real limit.
///
/// Placing this writer *underneath* a compressing encoder is what lets a
/// stream codec keep the reserve-before-write invariant. The encoder hands
/// its compressed bytes here before they reach the file, so there is no
/// window in which unreserved bytes exist on disk.
#[derive(Debug)]
pub struct BudgetedWriter<W> {
    inner: W,
    reservation: TempReservation,
}

impl<W> BudgetedWriter<W> {
    pub fn new(inner: W, reservation: TempReservation) -> Self {
        Self { inner, reservation }
    }

    pub fn reservation(&self) -> &TempReservation {
        &self.reservation
    }

    pub fn into_parts(self) -> (W, TempReservation) {
        (self.inner, self.reservation)
    }
}

impl<W: Write> Write for BudgetedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.reservation.grow(bytes.len() as u64)?;
        self.inner.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpillSnapshot {
    buffered_keys: usize,
    run_count: usize,
    peak_buffered_keys: usize,
    peak_run_count: usize,
    runs_created_total: u64,
    compaction_merge_count: u64,
    max_run_level: u32,
    records_written_total: u64,
    records_read_total: u64,
    initial_run_bytes_written: u64,
    total_run_bytes_written: u64,
}

impl SpillSnapshot {
    pub fn buffered_keys(self) -> usize {
        self.buffered_keys
    }

    pub fn run_count(self) -> usize {
        self.run_count
    }

    pub fn peak_buffered_keys(self) -> usize {
        self.peak_buffered_keys
    }

    pub fn peak_run_count(self) -> usize {
        self.peak_run_count
    }

    pub fn runs_created_total(self) -> u64 {
        self.runs_created_total
    }

    pub fn compaction_merge_count(self) -> u64 {
        self.compaction_merge_count
    }

    pub fn max_run_level(self) -> u32 {
        self.max_run_level
    }

    pub fn records_written_total(self) -> u64 {
        self.records_written_total
    }

    pub fn records_read_total(self) -> u64 {
        self.records_read_total
    }

    pub fn initial_run_bytes_written(self) -> u64 {
        self.initial_run_bytes_written
    }

    pub fn total_run_bytes_written(self) -> u64 {
        self.total_run_bytes_written
    }
}

#[derive(Debug, Default)]
struct SpillMetrics {
    peak_buffered_keys: usize,
    peak_run_count: usize,
    runs_created_total: u64,
    compaction_merge_count: u64,
    max_run_level: u32,
    records_written_total: u64,
    records_read_total: u64,
    initial_run_bytes_written: u64,
    total_run_bytes_written: u64,
}

impl SpillMetrics {
    fn completed_run_id(&mut self, level: u32) -> u64 {
        self.runs_created_total += 1;
        self.max_run_level = self.max_run_level.max(level);
        self.runs_created_total
    }

    fn snapshot(&self, buffered_keys: usize, run_count: usize) -> SpillSnapshot {
        SpillSnapshot {
            buffered_keys,
            run_count,
            peak_buffered_keys: self.peak_buffered_keys.max(buffered_keys),
            peak_run_count: self.peak_run_count.max(run_count),
            runs_created_total: self.runs_created_total,
            compaction_merge_count: self.compaction_merge_count,
            max_run_level: self.max_run_level,
            records_written_total: self.records_written_total,
            records_read_total: self.records_read_total,
            initial_run_bytes_written: self.initial_run_bytes_written,
            total_run_bytes_written: self.total_run_bytes_written,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SpillPolicy {
    pub max_pending_bytes: usize,
    pub max_pending_keys: usize,
    pub merge_fan_in: usize,
}

#[derive(Debug)]
struct RunFile {
    id: u64,
    file: NamedTempFile,
    reservation: TempReservation,
    level: u32,
    bytes: u64,
}

fn add_total_run_bytes(metrics: &mut SpillMetrics, bytes: u64) -> io::Result<()> {
    let current = metrics.total_run_bytes_written;
    metrics.total_run_bytes_written = current.checked_add(bytes).ok_or_else(|| {
        spill_counter_overflow(SpillCounter::TotalRunBytesWritten, current, bytes)
    })?;
    Ok(())
}

/// A `finish` failure, carrying the budget snapshot taken *before* the run's
/// reservation was released.
///
/// `RunWriter::finish` consumes the writer, so any early return drops the
/// `TempReservation` and credits its bytes back to the budget. A caller that
/// snapshotted afterwards would report post-drop usage for the failure. This
/// type preserves the pre-drop evidence that [`SpillOperationError`] promises.
#[derive(Debug)]
struct RunFinishFailure {
    source: io::Error,
    budget: TempBudgetSnapshot,
}

/// Reads one sorted run, carrying the identity needed to locate a decode
/// failure. `byte_offset` is an offset within the **decoded** record stream,
/// not the compressed file: a compressed offset would not locate a record.
///
/// No `Debug` derive: `zstd::stream::read::Decoder` does not implement it.
struct RunReader {
    reader: BufReader<zstd::stream::read::Decoder<'static, BufReader<File>>>,
    id: u64,
    level: u32,
    byte_offset: u64,
    /// Reused across records: one frame's payload, never handed out.
    payload: Vec<u8>,
}

impl RunReader {
    fn open(run: &RunFile) -> io::Result<Self> {
        Ok(Self {
            reader: BufReader::new(zstd::stream::read::Decoder::new(run.file.reopen()?)?),
            id: run.id,
            level: run.level,
            byte_offset: 0,
            payload: Vec::new(),
        })
    }

    fn next<K>(&mut self, metrics: &mut SpillMetrics) -> io::Result<Option<Record<K>>>
    where
        K: DeserializeOwned,
    {
        let record_offset = self.byte_offset;
        // A run ends exactly on a frame boundary, so an empty stream here is
        // the end of the run and a partial length prefix is a truncated one.
        // `read_exact` cannot tell those apart after the fact; count first.
        let mut header = [0_u8; RECORD_LENGTH_PREFIX_LEN];
        let mut filled = 0;
        while filled < header.len() {
            match self.reader.read(&mut header[filled..])? {
                0 => break,
                read => filled += read,
            }
        }
        if filled == 0 {
            return Ok(None);
        }
        if filled < header.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                truncated_run_frame(self.id, self.level, record_offset),
            ));
        }
        let payload_len = u32::from_le_bytes(header) as usize;
        self.payload.resize(payload_len, 0);
        self.reader.read_exact(&mut self.payload)?;
        let record = rmp_serde::from_slice(&self.payload).map_err(|source| {
            codec_io_error(SpillCodecError::for_run(
                SpillCodecOperation::DecodeRecord,
                self.id,
                self.level,
                record_offset,
                source,
            ))
        })?;
        self.byte_offset = record_offset
            .saturating_add(RECORD_LENGTH_PREFIX_LEN as u64)
            .saturating_add(payload_len as u64);
        metrics.records_read_total += 1;
        Ok(Some(record))
    }
}

/// Compression level for spilled runs, matching the level used elsewhere in
/// this crate for cache artifacts.
const RUN_COMPRESSION_LEVEL: i32 = 3;

/// Bytes of little-endian length prefix in front of each record's payload.
///
/// Runs are private temporary files that never outlive the process that wrote
/// them, so the framing carries no version tag and no compatibility promise.
const RECORD_LENGTH_PREFIX_LEN: usize = 4;

fn truncated_run_frame(run_id: u64, run_level: u32, byte_offset: u64) -> String {
    format!(
        "spilled run ended inside a record frame: \
         run={run_id} level={run_level} byte-offset={byte_offset}"
    )
}

/// Writes one sorted run: owns its temporary file, budget reservation, and
/// on-disk encoding. Charges every byte before it reaches the file.
///
/// No `Debug` derive: `zstd::stream::write::Encoder` does not implement it.
struct RunWriter {
    encoder: zstd::stream::write::Encoder<'static, BudgetedWriter<NamedTempFile>>,
    /// Reused across records: one framed record, length prefix included.
    frame: Vec<u8>,
}

impl RunWriter {
    fn create(directory: &TempDir, budget: &Arc<TempBudget>) -> io::Result<Self> {
        let file = NamedTempFile::new_in(directory.path())?;
        let reservation = budget.reserve(0)?;
        let encoder = zstd::stream::write::Encoder::new(
            BudgetedWriter::new(file, reservation),
            RUN_COMPRESSION_LEVEL,
        )?;
        Ok(Self {
            encoder,
            frame: Vec::new(),
        })
    }

    fn push(&mut self, record: &impl Serialize, metrics: &mut SpillMetrics) -> io::Result<()> {
        self.frame.clear();
        self.frame
            .extend_from_slice(&[0_u8; RECORD_LENGTH_PREFIX_LEN]);
        rmp_serde::encode::write(&mut self.frame, record).map_err(|source| {
            codec_io_error(SpillCodecError::without_run(
                SpillCodecOperation::EncodeRecord,
                source,
            ))
        })?;
        let payload_len = self.frame.len() - RECORD_LENGTH_PREFIX_LEN;
        let prefix = u32::try_from(payload_len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                SpillRecordTooLarge::new(payload_len),
            )
        })?;
        self.frame[..RECORD_LENGTH_PREFIX_LEN].copy_from_slice(&prefix.to_le_bytes());
        // Deliberately no `Encoder::flush` here: it would end a block and
        // cost compression ratio for no benefit.
        self.encoder.write_all(&self.frame)?;
        metrics.records_written_total += 1;
        Ok(())
    }

    fn finish(
        self,
        budget: &Arc<TempBudget>,
        metrics: &mut SpillMetrics,
        level: u32,
    ) -> Result<RunFile, RunFinishFailure> {
        // The frame epilogue is charged like any other byte, so this can fail
        // with StorageFull after every record was already accepted.
        //
        // `Encoder::finish` is `try_finish().map_err(|(_, err)| err)` — it
        // DISCARDS the encoder on error, dropping the BudgetedWriter and its
        // reservation and crediting the budget before we can snapshot it.
        // `try_finish` hands the encoder back, so the snapshot is pre-drop.
        let budgeted = match self.encoder.try_finish() {
            Ok(budgeted) => budgeted,
            Err((encoder, source)) => {
                let failure = RunFinishFailure {
                    source,
                    budget: budget.snapshot(),
                };
                // `write` panics after a failed `try_finish`; never reuse it.
                drop(encoder);
                return Err(failure);
            }
        };
        let (file, reservation) = budgeted.into_parts();
        let bytes = reservation.held_bytes();
        // No `sync_all`: runs are process-private, never recovered, and
        // deleted with their TempDir. `metadata` reports the written length
        // without a durability barrier.
        debug_assert_eq!(
            bytes,
            file.as_file()
                .metadata()
                .expect("temporary run metadata is readable")
                .len(),
            "charged bytes must equal bytes on disk"
        );
        let id = metrics.completed_run_id(level);
        Ok(RunFile {
            id,
            file,
            reservation,
            level,
            bytes,
        })
    }
}

/// Exact counts for ordered keys while retaining only bounded pending state.
#[derive(Debug)]
pub struct SortedRunCounts<K> {
    pending: BTreeMap<K, usize>,
    pending_bytes: usize,
    runs: Vec<RunFile>,
    directory: TempDir,
    budget: Arc<TempBudget>,
    policy: SpillPolicy,
    metrics: SpillMetrics,
}

/// A pull-style reader over a finalized container's sorted stream.
///
/// Owning the container keeps its run files and budget reservations alive for
/// exactly as long as the stream is being read.
pub struct SortedDrain<K> {
    counts: SortedRunCounts<K>,
    reader: Option<RunReader>,
}

impl<K> SortedDrain<K>
where
    K: Clone + Ord + Serialize + DeserializeOwned,
{
    /// The next record in sorted order, or `None` at the end of the stream.
    ///
    /// A drain over a container that never spilled has no reader: it pops the
    /// resident buffer directly, in the same order, releasing each entry's
    /// memory as it goes — which is what keeps the ordering phase's transient
    /// footprint at one buffer rather than two.
    pub fn next_entry(&mut self) -> io::Result<Option<(K, usize)>> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(self.counts.pending.pop_first());
        };
        match reader.next(&mut self.counts.metrics) {
            Ok(Some(record)) => Ok(Some((record.key, record.count))),
            Ok(None) => {
                self.reader = None;
                Ok(None)
            }
            Err(source) => Err(self.counts.operation_error(SpillOperation::Visit, source)),
        }
    }
}

impl<K> SortedRunCounts<K>
where
    K: Clone + Ord + Serialize + DeserializeOwned,
{
    /// Compatibility constructor: `max_runs` is now the leveled merge fan-in.
    pub fn new(max_pending_keys: usize, max_runs: usize) -> io::Result<Self> {
        Self::new_budgeted(
            max_pending_keys,
            max_runs,
            Arc::new(TempBudget::new(u64::MAX)),
        )
    }

    /// Compatibility constructor: `max_runs` is now the leveled merge fan-in.
    pub fn new_budgeted(
        max_pending_keys: usize,
        max_runs: usize,
        budget: Arc<TempBudget>,
    ) -> io::Result<Self> {
        Self::with_policy(
            SpillPolicy {
                max_pending_bytes: usize::MAX,
                max_pending_keys,
                merge_fan_in: max_runs,
            },
            budget,
        )
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "spill policy invariants are checked before construction returns"
    )]
    pub fn with_policy(policy: SpillPolicy, budget: Arc<TempBudget>) -> io::Result<Self> {
        assert!(
            policy.max_pending_bytes > 0,
            "spill byte buffer must be non-zero"
        );
        assert!(
            policy.max_pending_keys > 0,
            "spill key buffer must be non-zero"
        );
        assert!(
            policy.merge_fan_in > 1,
            "spill merge fan-in must exceed one"
        );
        Ok(Self {
            pending: BTreeMap::new(),
            pending_bytes: 0,
            runs: Vec::new(),
            directory: TempDir::new()?,
            budget,
            policy,
            metrics: SpillMetrics::default(),
        })
    }

    pub fn add(&mut self, key: K, count: usize) -> io::Result<()> {
        // A byte-capped policy must know what a new key will cost *before* it
        // decides whether to flush, and the only honest answer is to serialize
        // it — one encode per distinct key. An uncapped policy cannot act on
        // that number, so it does not pay for it, and one `entry` lookup then
        // replaces the `contains_key`/`entry` pair.
        if self.policy.max_pending_bytes == usize::MAX {
            return self.add_key_bounded(key, count);
        }
        let is_new = !self.pending.contains_key(&key);
        let estimated = if is_new {
            self.estimate_pending_key_bytes(&key)?
        } else {
            0
        };
        if is_new
            && !self.pending.is_empty()
            && (self.pending.len() >= self.policy.max_pending_keys
                || self
                    .pending_bytes
                    .checked_add(estimated)
                    .is_none_or(|bytes| bytes > self.policy.max_pending_bytes))
        {
            self.flush()?;
        }

        let pending = self.pending.entry(key).or_insert(0);
        let current = *pending;
        *pending = current
            .checked_add(count)
            .ok_or_else(|| count_overflow(current, count))?;
        if is_new {
            let current = self.pending_bytes;
            self.pending_bytes = self.pending_bytes.checked_add(estimated).ok_or_else(|| {
                spill_counter_overflow(SpillCounter::PendingBytes, current as u64, estimated as u64)
            })?;
        }
        if self.pending.len() >= self.policy.max_pending_keys
            || self.pending_bytes >= self.policy.max_pending_bytes
        {
            self.flush()?;
        }
        Ok(())
    }

    /// [`Self::add`] for a policy bounded by key count alone.
    ///
    /// The general path flushes *before* inserting a key that would exceed the
    /// cap; this one flushes after. The runs are the same either way, because
    /// the trailing check fires at exactly `max_pending_keys` and so the buffer
    /// never grows past it — `pre_insert_and_post_insert_flush_produce_equal_runs`
    /// pins that.
    fn add_key_bounded(&mut self, key: K, count: usize) -> io::Result<()> {
        let pending = self.pending.entry(key).or_insert(0);
        let current = *pending;
        *pending = current
            .checked_add(count)
            .ok_or_else(|| count_overflow(current, count))?;
        if self.pending.len() >= self.policy.max_pending_keys {
            self.flush()?;
        }
        Ok(())
    }

    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    pub fn buffered_keys(&self) -> usize {
        self.pending.len()
    }

    pub fn initial_run_bytes_written(&self) -> u64 {
        self.metrics.initial_run_bytes_written
    }

    pub fn total_run_bytes_written(&self) -> u64 {
        self.metrics.total_run_bytes_written
    }

    pub fn snapshot(&self) -> SpillSnapshot {
        self.metrics.snapshot(self.pending.len(), self.runs.len())
    }

    fn operation_error(&self, operation: SpillOperation, source: io::Error) -> io::Error {
        operation_io_error(operation, self.snapshot(), self.budget.snapshot(), source)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut writer = RunWriter::create(&self.directory, &self.budget)
            .map_err(|source| self.operation_error(SpillOperation::Flush, source))?;
        for (key, count) in &self.pending {
            if let Err(source) = writer.push(&Record { key, count: *count }, &mut self.metrics) {
                return Err(self.operation_error(SpillOperation::Flush, source));
            }
        }
        let run = match writer.finish(&self.budget, &mut self.metrics, 0) {
            Ok(run) => run,
            Err(failure) => {
                // Use the pre-drop budget snapshot, not a fresh one.
                return Err(operation_io_error(
                    SpillOperation::Flush,
                    self.snapshot(),
                    failure.budget,
                    failure.source,
                ));
            }
        };
        let initial_before = self.metrics.initial_run_bytes_written;
        self.metrics.initial_run_bytes_written =
            if let Some(total) = initial_before.checked_add(run.bytes) {
                total
            } else {
                let source = spill_counter_overflow(
                    SpillCounter::InitialRunBytesWritten,
                    initial_before,
                    run.bytes,
                );
                return Err(self.operation_error(SpillOperation::Flush, source));
            };
        if let Err(source) = add_total_run_bytes(&mut self.metrics, run.bytes) {
            return Err(self.operation_error(SpillOperation::Flush, source));
        }
        self.metrics.peak_buffered_keys = self.metrics.peak_buffered_keys.max(self.pending.len());
        self.pending.clear();
        self.pending_bytes = 0;
        self.runs.push(run);
        self.metrics.peak_run_count = self.metrics.peak_run_count.max(self.runs.len());
        self.compact_full_levels()
    }

    pub fn finalize(&mut self) -> io::Result<()> {
        self.flush()?;
        self.finalize_runs(SpillOperation::Finalize)
    }

    pub fn visit_sorted(
        &mut self,
        mut visitor: impl FnMut(&K, usize) -> io::Result<()>,
    ) -> io::Result<()> {
        // Nothing ever spilled: the resident buffer already is the sorted
        // stream, and writing it out to read it straight back would round the
        // records through the codec and the disk for nothing.
        if self.runs.is_empty() {
            for (key, count) in &self.pending {
                visitor(key, *count)?;
            }
            return Ok(());
        }
        self.flush()?;
        self.finalize_runs(SpillOperation::Visit)?;
        let Some(run) = self.runs.first() else {
            return Ok(());
        };
        debug_assert_eq!(run.reservation.held_bytes(), run.bytes);
        let mut reader = match RunReader::open(run) {
            Ok(reader) => reader,
            Err(source) => {
                return Err(self.operation_error(SpillOperation::Visit, source));
            }
        };
        loop {
            let record = match reader.next(&mut self.metrics) {
                Ok(record) => record,
                Err(source) => {
                    return Err(self.operation_error(SpillOperation::Visit, source));
                }
            };
            let Some(record) = record else {
                break;
            };
            if let Err(source) = visitor(&record.key, record.count) {
                return Err(self.operation_error(SpillOperation::Visit, source));
            }
        }
        Ok(())
    }

    /// Converts into a pull-style consumer of the same stream
    /// [`Self::visit_sorted`] would push.
    ///
    /// A caller merging several containers' streams needs one record in hand
    /// per container, which a push visitor cannot give it. Draining compacts
    /// to a single run exactly as a visit does, so the records and their
    /// order are identical to a visit's.
    pub fn into_sorted_drain(mut self) -> io::Result<SortedDrain<K>> {
        // Nothing ever spilled: drain the resident buffer directly (see
        // [`SortedDrain::next_entry`]), skipping the codec and the disk.
        if self.runs.is_empty() {
            return Ok(SortedDrain {
                counts: self,
                reader: None,
            });
        }
        self.flush()?;
        self.finalize_runs(SpillOperation::Visit)?;
        let reader = match self.runs.first() {
            Some(run) => {
                debug_assert_eq!(run.reservation.held_bytes(), run.bytes);
                match RunReader::open(run) {
                    Ok(reader) => Some(reader),
                    Err(source) => {
                        return Err(self.operation_error(SpillOperation::Visit, source));
                    }
                }
            }
            None => None,
        };
        Ok(SortedDrain {
            counts: self,
            reader,
        })
    }

    /// Estimates **uncompressed resident** bytes, for the pending-flush
    /// threshold only.
    ///
    /// This is unrelated to `initial_run_bytes_written` and
    /// `total_run_bytes_written`, which count **compressed stored** bytes:
    /// this one measures a key that is still resident, those measure records
    /// already through the run codec and zstd. Do not compare them.
    ///
    /// Only a byte-capped policy calls this. [`Self::add`] takes a path that
    /// skips it entirely when `max_pending_bytes` is `usize::MAX`, because an
    /// uncapped policy cannot act on the answer.
    fn estimate_pending_key_bytes(&self, key: &K) -> io::Result<usize> {
        let serialized_key_bytes = rmp_serde::to_vec(key)
            .map_err(|source| {
                codec_io_error(SpillCodecError::without_run(
                    SpillCodecOperation::EstimateKey,
                    source,
                ))
            })?
            .len();
        let key_inline_bytes = std::mem::size_of::<K>();
        let count_inline_bytes = std::mem::size_of::<usize>();
        let accounting_overhead_bytes = 64;
        serialized_key_bytes
            .checked_add(key_inline_bytes)
            .and_then(|bytes| bytes.checked_add(count_inline_bytes))
            .and_then(|bytes| bytes.checked_add(accounting_overhead_bytes))
            .ok_or_else(|| {
                estimated_key_bytes_overflow(
                    serialized_key_bytes,
                    key_inline_bytes,
                    count_inline_bytes,
                    accounting_overhead_bytes,
                )
            })
    }

    fn compact_full_levels(&mut self) -> io::Result<()> {
        loop {
            let Some(level) = self.runs.iter().map(|run| run.level).find(|&level| {
                self.runs.iter().filter(|run| run.level == level).count()
                    >= self.policy.merge_fan_in
            }) else {
                return Ok(());
            };
            let mut selected = Vec::with_capacity(self.policy.merge_fan_in);
            let mut retained = Vec::with_capacity(self.runs.len() - self.policy.merge_fan_in + 1);
            for run in self.runs.drain(..) {
                if run.level == level && selected.len() < self.policy.merge_fan_in {
                    selected.push(run);
                } else {
                    retained.push(run);
                }
            }
            let merged = Self::merge_runs(
                SpillOperation::Compact,
                selected,
                retained.len(),
                &self.directory,
                &self.budget,
                &mut self.metrics,
                level.saturating_add(1),
            )?;
            if let Err(source) = add_total_run_bytes(&mut self.metrics, merged.bytes) {
                return Err(operation_io_error(
                    SpillOperation::Compact,
                    self.metrics.snapshot(0, retained.len() + 1),
                    self.budget.snapshot(),
                    source,
                ));
            }
            retained.push(merged);
            self.runs = retained;
            self.metrics.peak_run_count = self.metrics.peak_run_count.max(self.runs.len());
        }
    }

    fn finalize_runs(&mut self, operation: SpillOperation) -> io::Result<()> {
        if self.runs.len() <= 1 {
            return Ok(());
        }
        let level = self
            .runs
            .iter()
            .map(|run| run.level)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let merged = Self::merge_runs(
            operation,
            std::mem::take(&mut self.runs),
            0,
            &self.directory,
            &self.budget,
            &mut self.metrics,
            level,
        )?;
        if let Err(source) = add_total_run_bytes(&mut self.metrics, merged.bytes) {
            return Err(operation_io_error(
                operation,
                self.metrics.snapshot(0, 1),
                self.budget.snapshot(),
                source,
            ));
        }
        self.runs.push(merged);
        self.metrics.peak_run_count = self.metrics.peak_run_count.max(self.runs.len());
        Ok(())
    }

    fn merge_runs(
        operation: SpillOperation,
        runs: Vec<RunFile>,
        retained_run_count: usize,
        directory: &TempDir,
        budget: &Arc<TempBudget>,
        metrics: &mut SpillMetrics,
        level: u32,
    ) -> io::Result<RunFile> {
        let live_run_count = retained_run_count + runs.len();
        let merge_error = |metrics: &SpillMetrics, source| {
            operation_io_error(
                operation,
                metrics.snapshot(0, live_run_count),
                budget.snapshot(),
                source,
            )
        };
        for run in &runs {
            debug_assert_eq!(run.reservation.held_bytes(), run.bytes);
        }
        let mut readers = Vec::with_capacity(runs.len());
        for run in &runs {
            match RunReader::open(run) {
                Ok(reader) => readers.push(reader),
                Err(source) => return Err(merge_error(metrics, source)),
            }
        }
        let mut current: Vec<Option<Record<K>>> = Vec::with_capacity(readers.len());
        for reader in &mut readers {
            match reader.next(metrics) {
                Ok(record) => current.push(record),
                Err(source) => return Err(merge_error(metrics, source)),
            }
        }
        let mut writer = match RunWriter::create(directory, budget) {
            Ok(writer) => writer,
            Err(source) => return Err(merge_error(metrics, source)),
        };

        while let Some(next_index) = current
            .iter()
            .enumerate()
            .filter_map(|(index, record)| record.as_ref().map(|record| (index, record)))
            .min_by(|(_, left), (_, right)| left.key.cmp(&right.key))
            .map(|(index, _)| index)
        {
            let mut record = current[next_index].take().expect("selected record exists");
            current[next_index] = match readers[next_index].next(metrics) {
                Ok(record) => record,
                Err(source) => return Err(merge_error(metrics, source)),
            };
            for index in 0..current.len() {
                while current[index]
                    .as_ref()
                    .is_some_and(|candidate| candidate.key == record.key)
                {
                    let duplicate = current[index].take().expect("duplicate exists");
                    let count_before = record.count;
                    record.count = if let Some(count) = record.count.checked_add(duplicate.count) {
                        count
                    } else {
                        let source = count_overflow(count_before, duplicate.count);
                        return Err(merge_error(metrics, source));
                    };
                    current[index] = match readers[index].next(metrics) {
                        Ok(record) => record,
                        Err(source) => return Err(merge_error(metrics, source)),
                    };
                }
            }
            if let Err(source) = writer.push(&record, metrics) {
                return Err(merge_error(metrics, source));
            }
        }
        // `finish` is fallible, so the merge counter must advance only after
        // it succeeds; incrementing first would record a completed compaction
        // for a merge that failed.
        let run = match writer.finish(budget, metrics, level) {
            Ok(run) => run,
            Err(failure) => {
                // Pre-drop snapshot; `merge_error` would take a fresh one.
                return Err(operation_io_error(
                    operation,
                    metrics.snapshot(0, live_run_count),
                    failure.budget,
                    failure.source,
                ));
            }
        };
        metrics.compaction_merge_count += 1;
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cmp::Ordering as CmpOrdering;

    use serde::de::{Deserializer, Error as _};
    use serde::ser::{Error as _, Serializer};

    use super::*;

    #[derive(Debug)]
    struct FailingKey {
        value: String,
        serializations_before_error: Cell<usize>,
    }

    impl FailingKey {
        fn new(value: &str, serializations_before_error: usize) -> Self {
            Self {
                value: value.to_owned(),
                serializations_before_error: Cell::new(serializations_before_error),
            }
        }
    }

    impl Clone for FailingKey {
        fn clone(&self) -> Self {
            Self::new(&self.value, self.serializations_before_error.get())
        }
    }

    impl PartialEq for FailingKey {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    impl Eq for FailingKey {}

    impl PartialOrd for FailingKey {
        fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for FailingKey {
        fn cmp(&self, other: &Self) -> CmpOrdering {
            self.value.cmp(&other.value)
        }
    }

    impl Serialize for FailingKey {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let remaining = self.serializations_before_error.get();
            if remaining == 0 {
                return Err(S::Error::custom("injected key serialization failure"));
            }
            self.serializations_before_error.set(remaining - 1);
            self.value.serialize(serializer)
        }
    }

    impl<'de> Deserialize<'de> for FailingKey {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            String::deserialize(deserializer).map(|value| Self::new(&value, usize::MAX))
        }
    }

    #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct DecodeFailingKey(String);

    impl<'de> Deserialize<'de> for DecodeFailingKey {
        fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            Err(D::Error::custom("injected key decode failure"))
        }
    }

    fn codec_source<'a>(error: &'a (dyn std::error::Error + 'static)) -> &'a SpillCodecError {
        if let Some(source) = error.downcast_ref::<SpillCodecError>() {
            return source;
        }
        if let Some(source) = error
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            && let Some(codec) = codec_source_opt(source)
        {
            return codec;
        }
        error
            .source()
            .and_then(codec_source_opt)
            .expect("typed spill codec source")
    }

    fn codec_source_opt<'a>(
        error: &'a (dyn std::error::Error + 'static),
    ) -> Option<&'a SpillCodecError> {
        if let Some(source) = error.downcast_ref::<SpillCodecError>() {
            return Some(source);
        }
        if let Some(source) = error
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            && let Some(codec) = codec_source_opt(source)
        {
            return Some(codec);
        }
        error.source().and_then(codec_source_opt)
    }

    #[test]
    fn estimation_and_record_encode_failures_have_typed_codec_operations() {
        // `EstimateKey` is reachable only under a byte cap, because only a byte
        // cap estimates. Under the key-bounded policy every production caller
        // builds, the same unserializable key surfaces as `EncodeRecord` at the
        // flush instead — later, but with the same typed operation and the same
        // run identity a caller needs to locate it.
        let budget = Arc::new(TempBudget::new(1 << 20));
        let mut estimate = SortedRunCounts::with_policy(
            SpillPolicy {
                max_pending_bytes: 1 << 20,
                max_pending_keys: 1,
                merge_fan_in: 4,
            },
            Arc::clone(&budget),
        )
        .unwrap();
        let error = estimate.add(FailingKey::new("estimate", 0), 1).unwrap_err();
        assert_eq!(
            codec_source(&error).operation(),
            SpillCodecOperation::EstimateKey
        );

        // Byte-capped: the estimate consumes the first serialization, so the
        // second one — the flush encode — is the one that fails.
        let mut encode = SortedRunCounts::with_policy(
            SpillPolicy {
                max_pending_bytes: 1 << 20,
                max_pending_keys: 1,
                merge_fan_in: 4,
            },
            Arc::clone(&budget),
        )
        .unwrap();
        let error = encode.add(FailingKey::new("encode", 1), 1).unwrap_err();
        assert_eq!(
            codec_source(&error).operation(),
            SpillCodecOperation::EncodeRecord
        );

        // Key-bounded: no estimate, so the flush encode is the *first*
        // serialization the key ever sees.
        let mut uncapped = SortedRunCounts::new_budgeted(1, 4, budget).unwrap();
        let error = uncapped.add(FailingKey::new("uncapped", 0), 1).unwrap_err();
        assert_eq!(
            codec_source(&error).operation(),
            SpillCodecOperation::EncodeRecord
        );
    }

    #[test]
    fn decode_failure_carries_completed_run_identity_and_offset() {
        let mut counts = SortedRunCounts::new(1, 4).unwrap();
        counts
            .add(DecodeFailingKey("decode".to_owned()), 1)
            .unwrap();
        let error = counts.visit_sorted(|_, _| Ok(())).unwrap_err();
        let source = codec_source(&error);
        assert_eq!(source.operation(), SpillCodecOperation::DecodeRecord);
        assert_eq!(source.run_id(), Some(1));
        assert_eq!(source.run_level(), Some(0));
        assert_eq!(source.byte_offset(), Some(0));
    }

    #[test]
    fn failed_record_write_does_not_advance_completed_counters() {
        // Retargeted twice. Originally this drove the deleted `write_record`
        // free function through an injected always-failing writer. It now
        // drives `RunWriter::push`, which owns the same ordering: the record
        // counter advances only after `write_all` returns successfully.
        //
        // A single small record cannot fail under a stream codec — the
        // encoder buffers it and no byte reaches the budget — so the failure
        // is provoked where it genuinely occurs, when a block is emitted
        // under a tight budget. The counter must then equal exactly the
        // number of pushes that succeeded, never one more.
        let directory = TempDir::new().unwrap();
        let budget = Arc::new(TempBudget::new(1024));
        let mut writer = RunWriter::create(&directory, &budget).unwrap();
        let mut metrics = SpillMetrics::default();

        let mut succeeded = 0u64;
        let mut failure = None;
        for index in 0..100_000u32 {
            let key = format!("{}{index:08}", "spill-codec-ordering-".repeat(4));
            match writer.push(&Record { key, count: 1 }, &mut metrics) {
                Ok(()) => succeeded += 1,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }

        let error = failure.expect("a 1 KiB budget cannot hold 100,000 records");
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert_eq!(
            metrics.records_written_total, succeeded,
            "the failed push must not be counted"
        );
        assert!(
            budget.used_bytes() <= 1024,
            "no byte is written without being charged first"
        );
    }

    #[test]
    fn every_spill_created_storage_full_has_a_typed_source() {
        let mut errors = vec![capacity_io_error(TempCapacityError::AccountingOverflow {
            requested_additional_bytes: 1,
            budget: TempBudgetSnapshot {
                limit_bytes: u64::MAX,
                used_bytes: u64::MAX,
                peak_used_bytes: u64::MAX,
            },
        })];
        for counter in [
            SpillCounter::PendingBytes,
            SpillCounter::RunBytes,
            SpillCounter::InitialRunBytesWritten,
            SpillCounter::TotalRunBytesWritten,
        ] {
            errors.push(spill_counter_overflow(counter, u64::MAX, 1));
        }
        errors.push(estimated_key_bytes_overflow(
            usize::MAX,
            usize::MAX,
            usize::MAX,
            usize::MAX,
        ));

        for error in errors {
            assert_eq!(error.kind(), io::ErrorKind::StorageFull);
            let source = error.get_ref().expect("typed source");
            assert!(
                source.downcast_ref::<TempCapacityError>().is_some()
                    || source.downcast_ref::<SpillAccountingOverflow>().is_some()
            );
        }
    }

    #[test]
    fn spill_accounting_helpers_preserve_site_identity() {
        for counter in [
            SpillCounter::PendingBytes,
            SpillCounter::RunBytes,
            SpillCounter::InitialRunBytesWritten,
            SpillCounter::TotalRunBytesWritten,
        ] {
            let error = spill_counter_overflow(counter, 17, 3);
            let source = error
                .get_ref()
                .and_then(|source| source.downcast_ref::<SpillAccountingOverflow>())
                .expect("typed spill accounting source");
            assert_eq!(source.counter(), Some(counter));
            assert_eq!(source.current(), Some(17));
            assert_eq!(source.requested_additional(), Some(3));
        }

        let error = estimated_key_bytes_overflow(11, 22, 33, 44);
        let source = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<SpillAccountingOverflow>())
            .expect("typed estimated-key source");
        assert_eq!(source.estimated_key_components(), Some((11, 22, 33, 44)));
    }

    #[test]
    fn defensive_record_too_large_type_has_structured_identity() {
        let error = SpillRecordTooLarge::new(usize::MAX);
        assert_eq!(error.record_bytes(), usize::MAX);
    }
}
