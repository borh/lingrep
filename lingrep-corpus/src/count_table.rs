//! Per-(document, entry) count tables for one catalog run over a prepared
//! artifact.
//!
//! A table is a directory of little-endian arrays and a `header.json`. It is
//! stored beside the artifact, not inside it, and is keyed by the artifact's
//! analysis identity, the catalog fingerprint and the matcher revision that
//! produced it. [`CountTable::open`] rejects a table whose key differs from the
//! caller's.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lingrep_core::execution::CancellationFlag;
use lingrep_core::spill::{SortedDrain, SortedRunCounts, SpillPolicy, TempBudget};
use memmap2::{Mmap, MmapMut};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(not(target_endian = "little"))]
compile_error!("count tables map little-endian arrays in place");

/// The `format` value of a count table's header.
pub const COUNT_TABLE_FORMAT: &str = "lingrep.count-table";
/// The header version this build reads and writes.
pub const COUNT_TABLE_VERSION: u32 = 1;

/// Admitted pairs at or below which [`CountTable::top`] sums the admitted
/// documents' own pairs instead of scanning entries.
const DOCUMENT_MAJOR_PAIRS: u64 = 2_000_000;

const HEADER: &str = "header.json";
const VALUE_OFFSETS: &str = "value_offsets.u64";
const VALUES: &str = "values.utf8";
const ENTRY_KEY_OFFSETS: &str = "entry_key_offsets.u64";
const ENTRY_KEYS: &str = "entry_keys.u32";
const ENTRY_PAIR_OFFSETS: &str = "entry_pair_offsets.u64";
const PAIR_DOCUMENTS: &str = "pair_documents.u32";
const PAIR_COUNTS: &str = "pair_counts.u32";
const RANKED_ENTRIES: &str = "ranked_entries.u32";
const RANKED_TOTALS: &str = "ranked_totals.u64";
const DOCUMENT_PAIR_OFFSETS: &str = "document_pair_offsets.u64";
const DOCUMENT_ENTRIES: &str = "document_entries.u32";
const DOCUMENT_COUNTS: &str = "document_counts.u32";
const DOCUMENT_IDS: &str = "document_ids.u64";
const DOCUMENT_SENTENCES: &str = "document_sentences.u64";
const DOCUMENT_TOKENS: &str = "document_tokens.u64";
const ENTRY_TOTALS_SCRATCH: &str = "entry_totals.scratch";

/// Elements a writer pass copies between checks of its cancellation flag.
const CANCELLATION_INTERVAL: usize = 1 << 20;

/// What a table's counts depend on. A reader serves a table only when all
/// three equal the current artifact, catalog and matcher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountTableKey {
    pub analysis_identity: String,
    pub catalog_fingerprint: String,
    pub matcher_revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    Surface,
    Unit,
    Term,
    Key,
    LexicalKey,
}

/// The shape of an entry key: its kind and, for keys, the field names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSchema {
    pub kind: GroupKind,
    pub names: Vec<String>,
}

/// One document with at least one admitted sentence in the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentTotals {
    pub document_id: u64,
    pub sentences: u64,
    pub tokens: u64,
}

/// A decoded entry key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Group<'a> {
    Surface(&'a str),
    Unit(&'a str),
    Term(&'a str),
    /// Fields in catalog order; a lexical key's list field is a `List`.
    Key(Vec<(&'a str, FieldValue<'a>)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue<'a> {
    Scalar(&'a str),
    List(Vec<&'a str>),
}

#[derive(Debug, thiserror::Error)]
pub enum CountTableError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("count table header is not valid: {0}")]
    Header(#[from] serde_json::Error),
    #[error(
        "{path} is a {found} version {version} table, not {COUNT_TABLE_FORMAT} version {COUNT_TABLE_VERSION}"
    )]
    UnsupportedFormat {
        path: PathBuf,
        found: String,
        version: u32,
    },
    #[error(
        "count table {path} was written for {field} {found}, but the current one is {expected}; rebuild it with `ling summarize --write-table`"
    )]
    KeyMismatch {
        path: PathBuf,
        field: &'static str,
        expected: String,
        found: String,
    },
    #[error("count table is corrupt: {0}")]
    Corrupt(String),
    #[error("count table writer: {0}")]
    Writer(String),
    #[error("destination already exists at {0}")]
    DestinationExists(PathBuf),
    #[error("count table writing was cancelled")]
    Cancelled,
}

fn corrupt(message: impl Into<String>) -> CountTableError {
    CountTableError::Corrupt(message.into())
}

fn writer_error(message: impl Into<String>) -> CountTableError {
    CountTableError::Writer(message.into())
}

/// In a debug build, waits at the writer phase that
/// `LINGREP_TEST_PAUSE_COUNT_TABLE` names until `cancellation` is raised or
/// 30 seconds pass, so a test can deliver a signal inside that phase.
#[cfg(debug_assertions)]
fn pause_for_test(phase: &str, cancellation: &CancellationFlag) {
    if std::env::var("LINGREP_TEST_PAUSE_COUNT_TABLE").is_ok_and(|named| named == phase) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !cancellation.is_cancelled() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(not(debug_assertions))]
fn pause_for_test(_phase: &str, _cancellation: &CancellationFlag) {}

fn check_cancellation(cancellation: &CancellationFlag) -> Result<(), CountTableError> {
    if cancellation.is_cancelled() {
        Err(CountTableError::Cancelled)
    } else {
        Ok(())
    }
}

/// A directory that is removed on drop unless it was renamed into place.
struct StagingDirectory {
    path: PathBuf,
    published: bool,
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PatternHeader {
    name: String,
    /// The pattern's entries, `[start, end)` in entry order.
    entries: [u64; 2],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    version: u32,
    key: CountTableKey,
    patterns: Vec<PatternHeader>,
    schemas: Vec<GroupSchema>,
    values: u64,
    value_bytes: u64,
    entries: u64,
    key_words: u64,
    pairs: u64,
    documents: u64,
}

/// Only the fields a reader checks before trusting the rest.
#[derive(Deserialize)]
struct HeaderVersion {
    format: String,
    version: u32,
}

/// Words a key of `schema` occupies at the start of `words`, after the
/// schema word, or `None` when `words` does not hold a valid key of it.
fn key_payload_len(schema: &GroupSchema, words: &[u32], values: u64) -> Option<usize> {
    let rank_ok = |rank: u32| u64::from(rank) < values;
    match schema.kind {
        GroupKind::Surface | GroupKind::Unit | GroupKind::Term => {
            words.first().copied().filter(|rank| rank_ok(*rank))?;
            Some(1)
        }
        GroupKind::Key => {
            let fields = words.get(..schema.names.len())?;
            fields
                .iter()
                .all(|rank| rank_ok(*rank))
                .then_some(fields.len())
        }
        GroupKind::LexicalKey => {
            let mut at = 0usize;
            for _ in &schema.names {
                let marker = *words.get(at)?;
                let width = if marker == 0 { 1 } else { marker as usize - 1 };
                let ranks = words.get(at + 1..at + 1 + width)?;
                if !ranks.iter().all(|rank| rank_ok(*rank)) {
                    return None;
                }
                at += 1 + width;
            }
            Some(at)
        }
    }
}

struct Staged {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl Staged {
    fn create(directory: &Path, name: &str) -> Result<Self, CountTableError> {
        let path = directory.join(name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            path,
            writer: BufWriter::with_capacity(1 << 20, file),
        })
    }

    fn u32(&mut self, value: u32) -> Result<(), CountTableError> {
        Ok(self.writer.write_all(&value.to_le_bytes())?)
    }

    fn u64(&mut self, value: u64) -> Result<(), CountTableError> {
        Ok(self.writer.write_all(&value.to_le_bytes())?)
    }

    fn finish(self) -> Result<PathBuf, CountTableError> {
        let file = self
            .writer
            .into_inner()
            .map_err(|error| CountTableError::Io(error.into_error()))?;
        file.sync_all()?;
        Ok(self.path)
    }
}

struct OpenEntry {
    last_document: Option<u32>,
    total: u64,
}

/// The arrays written while entries stream in.
struct Streams {
    entry_keys: Staged,
    entry_key_offsets: Staged,
    entry_pair_offsets: Staged,
    pair_documents: Staged,
    pair_counts: Staged,
    entry_totals: Staged,
}

/// Writes one table from a stream of entries in (pattern, key) order, each
/// followed by its documents in ascending order.
///
/// The table becomes visible at its destination only when
/// [`StagedCountTable::publish`] succeeds; dropping the writer or the staged
/// table earlier removes the staging directory.
pub struct CountTableWriter {
    staging: StagingDirectory,
    destination: PathBuf,
    key: CountTableKey,
    patterns: Vec<PatternHeader>,
    schemas: Vec<GroupSchema>,
    values: u64,
    value_bytes: u64,
    documents: u64,
    streams: Option<Streams>,
    current_pattern: Option<usize>,
    document_pairs: Vec<u64>,
    entries: u64,
    key_words: u64,
    pairs: u64,
    open: Option<OpenEntry>,
}

impl CountTableWriter {
    /// Starts a table at `destination`, which must not exist.
    ///
    /// `values` is the vocabulary that key words index, in the order whose
    /// ranks the keys use. `documents` lists each document with an admitted
    /// sentence, with strictly ascending document IDs; a pair's document is an
    /// index into this list.
    pub fn create<S: AsRef<str>>(
        destination: &Path,
        key: CountTableKey,
        patterns: Vec<String>,
        schemas: Vec<GroupSchema>,
        values: &[S],
        documents: &[DocumentTotals],
    ) -> Result<Self, CountTableError> {
        if destination.exists() {
            return Err(CountTableError::DestinationExists(
                destination.to_path_buf(),
            ));
        }
        if u32::try_from(documents.len()).is_err() {
            return Err(writer_error("more than 2^32-1 documents"));
        }
        if documents
            .windows(2)
            .any(|pair| pair[0].document_id >= pair[1].document_id)
        {
            return Err(writer_error("document IDs are not strictly ascending"));
        }
        if u32::try_from(values.len()).is_err() {
            return Err(writer_error("more than 2^32-1 values"));
        }
        let name = destination
            .file_name()
            .ok_or_else(|| writer_error("destination has no file name"))?
            .to_string_lossy();
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let staging = StagingDirectory {
            path: parent.join(format!(".{name}.staging-{}-{nanos}", std::process::id())),
            published: false,
        };
        fs::create_dir(&staging.path)?;
        // From here on, dropping `staging` removes the directory on any early return.
        let staging_path = staging.path.clone();
        let staging_path = staging_path.as_path();
        let mut value_offsets = Staged::create(staging_path, VALUE_OFFSETS)?;
        let mut value_text = Staged::create(staging_path, VALUES)?;
        let mut value_bytes = 0u64;
        value_offsets.u64(0)?;
        for value in values {
            let value = value.as_ref().as_bytes();
            value_text.writer.write_all(value)?;
            value_bytes += value.len() as u64;
            value_offsets.u64(value_bytes)?;
        }
        value_offsets.finish()?;
        value_text.finish()?;
        let mut ids = Staged::create(staging_path, DOCUMENT_IDS)?;
        let mut sentences = Staged::create(staging_path, DOCUMENT_SENTENCES)?;
        let mut tokens = Staged::create(staging_path, DOCUMENT_TOKENS)?;
        for document in documents {
            ids.u64(document.document_id)?;
            sentences.u64(document.sentences)?;
            tokens.u64(document.tokens)?;
        }
        ids.finish()?;
        sentences.finish()?;
        tokens.finish()?;
        let mut streams = Streams {
            entry_keys: Staged::create(staging_path, ENTRY_KEYS)?,
            entry_key_offsets: Staged::create(staging_path, ENTRY_KEY_OFFSETS)?,
            entry_pair_offsets: Staged::create(staging_path, ENTRY_PAIR_OFFSETS)?,
            pair_documents: Staged::create(staging_path, PAIR_DOCUMENTS)?,
            pair_counts: Staged::create(staging_path, PAIR_COUNTS)?,
            entry_totals: Staged::create(staging_path, ENTRY_TOTALS_SCRATCH)?,
        };
        streams.entry_key_offsets.u64(0)?;
        streams.entry_pair_offsets.u64(0)?;
        Ok(Self {
            streams: Some(streams),
            current_pattern: None,
            staging,
            destination: destination.to_path_buf(),
            key,
            patterns: patterns
                .into_iter()
                .map(|name| PatternHeader {
                    name,
                    entries: [0, 0],
                })
                .collect(),
            schemas,
            values: values.len() as u64,
            value_bytes,
            documents: documents.len() as u64,
            document_pairs: vec![0; documents.len()],
            entries: 0,
            key_words: 0,
            pairs: 0,
            open: None,
        })
    }

    fn streams(&mut self) -> Result<&mut Streams, CountTableError> {
        self.streams
            .as_mut()
            .ok_or_else(|| writer_error("writer already finished"))
    }

    /// Marks the patterns from the last started one up to `before` as empty
    /// at the current entry position.
    fn skip_patterns(&mut self, before: usize) {
        let first = self.current_pattern.map_or(0, |current| current + 1);
        for skipped in first..before {
            self.patterns[skipped].entries = [self.entries, self.entries];
        }
    }

    /// Starts the next entry. Entries arrive grouped by ascending pattern
    /// index, and within a pattern in the order ties are ranked.
    ///
    /// `words` encodes the key after its schema: one value rank for a surface,
    /// unit or term; one rank per field for a key; and for a lexical key, per
    /// field `0, rank` for a scalar or `n + 1` followed by `n` ranks for a list.
    pub fn begin_entry(
        &mut self,
        pattern: usize,
        schema: u32,
        words: &[u32],
    ) -> Result<(), CountTableError> {
        self.close_entry()?;
        let current = self.current_pattern;
        if pattern >= self.patterns.len() || current.is_some_and(|current| pattern < current) {
            return Err(writer_error(format!(
                "entry for pattern {pattern} is out of order"
            )));
        }
        let shape = self
            .schemas
            .get(schema as usize)
            .ok_or_else(|| writer_error(format!("schema {schema} is undeclared")))?;
        if key_payload_len(shape, words, self.values) != Some(words.len()) {
            return Err(writer_error(format!(
                "key words do not form a key of schema {schema}"
            )));
        }
        if u32::try_from(self.entries).is_err() {
            return Err(writer_error("more than 2^32-1 entries"));
        }
        if current != Some(pattern) {
            self.skip_patterns(pattern + 1);
            self.current_pattern = Some(pattern);
        }
        self.patterns[pattern].entries[1] = self.entries + 1;
        self.key_words += 1 + words.len() as u64;
        let key_words = self.key_words;
        let streams = self.streams()?;
        streams.entry_keys.u32(schema)?;
        for word in words {
            streams.entry_keys.u32(*word)?;
        }
        streams.entry_key_offsets.u64(key_words)?;
        self.entries += 1;
        self.open = Some(OpenEntry {
            last_document: None,
            total: 0,
        });
        Ok(())
    }

    /// Adds the open entry's count in one document, given as its index in the
    /// `documents` passed to [`Self::create`].
    pub fn add_pair(&mut self, document: u32, count: u64) -> Result<(), CountTableError> {
        let open = self
            .open
            .as_mut()
            .ok_or_else(|| writer_error("pair before any entry"))?;
        if open.last_document.is_some_and(|last| document <= last) {
            return Err(writer_error("documents of an entry are not ascending"));
        }
        if u64::from(document) >= self.documents {
            return Err(writer_error(format!("document {document} is undeclared")));
        }
        let count32 = u32::try_from(count)
            .ok()
            .filter(|count| *count > 0)
            .ok_or_else(|| writer_error(format!("pair count {count} is outside 1..2^32")))?;
        open.last_document = Some(document);
        open.total = open
            .total
            .checked_add(count)
            .ok_or_else(|| writer_error("entry total overflow"))?;
        let streams = self.streams()?;
        streams.pair_documents.u32(document)?;
        streams.pair_counts.u32(count32)?;
        self.document_pairs[document as usize] += 1;
        self.pairs += 1;
        Ok(())
    }

    fn close_entry(&mut self) -> Result<(), CountTableError> {
        if let Some(open) = self.open.take() {
            if open.last_document.is_none() {
                return Err(writer_error("entry without a pair"));
            }
            let pairs = self.pairs;
            let streams = self.streams()?;
            streams.entry_totals.u64(open.total)?;
            streams.entry_pair_offsets.u64(pairs)?;
        }
        Ok(())
    }

    /// Writes the count order, the document-major copy and the header into
    /// the staging directory. Once `cancellation` is raised this fails with
    /// [`CountTableError::Cancelled`] within a bounded amount of work, and
    /// the staging directory is removed.
    pub fn finish(
        mut self,
        cancellation: &CancellationFlag,
    ) -> Result<StagedCountTable, CountTableError> {
        self.close_entry()?;
        self.skip_patterns(self.patterns.len());
        let streams = self
            .streams
            .take()
            .ok_or_else(|| writer_error("writer already finished"))?;
        for file in [
            streams.entry_keys,
            streams.entry_key_offsets,
            streams.entry_pair_offsets,
            streams.pair_documents,
            streams.pair_counts,
            streams.entry_totals,
        ] {
            file.finish()?;
        }
        pause_for_test("ranking", cancellation);
        check_cancellation(cancellation)?;
        self.write_ranking(cancellation)?;
        pause_for_test("transpose", cancellation);
        self.write_document_major(cancellation)?;
        check_cancellation(cancellation)?;
        let header = Header {
            format: COUNT_TABLE_FORMAT.to_owned(),
            version: COUNT_TABLE_VERSION,
            key: self.key.clone(),
            patterns: self.patterns.clone(),
            schemas: self.schemas.clone(),
            values: self.values,
            value_bytes: self.value_bytes,
            entries: self.entries,
            key_words: self.key_words,
            pairs: self.pairs,
            documents: self.documents,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.staging.path.join(HEADER))?;
        serde_json::to_writer_pretty(&mut file, &header)?;
        file.sync_all()?;
        Ok(StagedCountTable {
            staging: self.staging,
            destination: self.destination,
            stats: CountTableStats {
                entries: self.entries,
                pairs: self.pairs,
                documents: self.documents,
                values: self.values,
                bytes: 0,
            },
        })
    }

    /// Each pattern's entries by descending total, ties by entry order.
    fn write_ranking(&self, cancellation: &CancellationFlag) -> Result<(), CountTableError> {
        let totals_file = File::open(self.staging.path.join(ENTRY_TOTALS_SCRATCH))?;
        let totals_map = map_file(&totals_file)?;
        let totals: &[u64] = cast(totals_map.as_ref(), ENTRY_TOTALS_SCRATCH)?;
        let ranked_file = sized_file(&self.staging.path.join(RANKED_ENTRIES), self.entries * 4)?;
        if self.entries > 0 {
            // SAFETY: the file was created by this writer in its private
            // staging directory and nothing else maps or truncates it.
            let mut ranked_map = unsafe { MmapMut::map_mut(&ranked_file)? };
            let ranked: &mut [u32] = bytemuck::try_cast_slice_mut(&mut ranked_map[..])
                .map_err(|error| writer_error(format!("ranked entries: {error}")))?;
            ranked
                .par_iter_mut()
                .enumerate()
                .for_each(|(entry, slot)| *slot = entry as u32);
            for pattern in &self.patterns {
                check_cancellation(cancellation)?;
                let [start, end] = pattern.entries;
                ranked[start as usize..end as usize].par_sort_unstable_by(|left, right| {
                    totals[*right as usize]
                        .cmp(&totals[*left as usize])
                        .then(left.cmp(right))
                });
            }
            let mut ranked_totals = Staged::create(&self.staging.path, RANKED_TOTALS)?;
            for chunk in ranked.chunks(CANCELLATION_INTERVAL) {
                check_cancellation(cancellation)?;
                for entry in chunk {
                    ranked_totals.u64(totals[*entry as usize])?;
                }
            }
            ranked_totals.finish()?;
            ranked_map.flush()?;
        } else {
            Staged::create(&self.staging.path, RANKED_TOTALS)?.finish()?;
        }
        ranked_file.sync_all()?;
        drop(totals_map);
        fs::remove_file(self.staging.path.join(ENTRY_TOTALS_SCRATCH))?;
        Ok(())
    }

    /// The pairs again, grouped by document, each document's in entry order.
    fn write_document_major(&self, cancellation: &CancellationFlag) -> Result<(), CountTableError> {
        let mut offsets = Vec::with_capacity(self.document_pairs.len() + 1);
        let mut running = 0u64;
        offsets.push(0);
        for pairs in &self.document_pairs {
            running += pairs;
            offsets.push(running);
        }
        let mut offsets_file = Staged::create(&self.staging.path, DOCUMENT_PAIR_OFFSETS)?;
        for offset in &offsets {
            offsets_file.u64(*offset)?;
        }
        offsets_file.finish()?;
        let entries_file = sized_file(&self.staging.path.join(DOCUMENT_ENTRIES), self.pairs * 4)?;
        let counts_file = sized_file(&self.staging.path.join(DOCUMENT_COUNTS), self.pairs * 4)?;
        if self.pairs > 0 {
            let entry_offsets_file = File::open(self.staging.path.join(ENTRY_PAIR_OFFSETS))?;
            let documents_file = File::open(self.staging.path.join(PAIR_DOCUMENTS))?;
            let counts_in_file = File::open(self.staging.path.join(PAIR_COUNTS))?;
            let (entry_offsets_map, documents_map, counts_in_map) = (
                map_file(&entry_offsets_file)?,
                map_file(&documents_file)?,
                map_file(&counts_in_file)?,
            );
            let entry_offsets: &[u64] = cast(entry_offsets_map.as_ref(), ENTRY_PAIR_OFFSETS)?;
            let documents: &[u32] = cast(documents_map.as_ref(), PAIR_DOCUMENTS)?;
            let counts_in: &[u32] = cast(counts_in_map.as_ref(), PAIR_COUNTS)?;
            // SAFETY: both files were created by this writer in its private
            // staging directory and nothing else maps or truncates them.
            let (mut entries_map, mut counts_map) = unsafe {
                (
                    MmapMut::map_mut(&entries_file)?,
                    MmapMut::map_mut(&counts_file)?,
                )
            };
            let out_entries: &mut [u32] = bytemuck::try_cast_slice_mut(&mut entries_map[..])
                .map_err(|error| writer_error(format!("document entries: {error}")))?;
            let out_counts: &mut [u32] = bytemuck::try_cast_slice_mut(&mut counts_map[..])
                .map_err(|error| writer_error(format!("document counts: {error}")))?;
            let mut cursor = offsets[..self.document_pairs.len()].to_vec();
            for (entry, range) in entry_offsets.windows(2).enumerate() {
                if entry % CANCELLATION_INTERVAL == 0 {
                    check_cancellation(cancellation)?;
                }
                for pair in range[0] as usize..range[1] as usize {
                    let document = documents[pair] as usize;
                    let at = cursor[document] as usize;
                    cursor[document] += 1;
                    out_entries[at] = entry as u32;
                    out_counts[at] = counts_in[pair];
                }
            }
            entries_map.flush()?;
            counts_map.flush()?;
        }
        entries_file.sync_all()?;
        counts_file.sync_all()?;
        Ok(())
    }
}

/// A complete table in its staging directory, not yet visible at its
/// destination. Dropping it removes the staging directory.
pub struct StagedCountTable {
    staging: StagingDirectory,
    destination: PathBuf,
    stats: CountTableStats,
}

impl StagedCountTable {
    /// Renames the staged table to its destination without replacing
    /// anything there.
    pub fn publish(mut self) -> Result<CountTableStats, CountTableError> {
        crate::rename_noreplace(&self.staging.path, &self.destination).map_err(|error| {
            if matches!(error.raw_os_error(), Some(libc::EEXIST | libc::ENOTEMPTY)) {
                CountTableError::DestinationExists(self.destination.clone())
            } else {
                CountTableError::Io(error)
            }
        })?;
        self.staging.published = true;
        if let Some(parent) = self.destination.parent()
            && let Ok(directory) = File::open(parent)
        {
            directory.sync_all()?;
        }
        self.stats.bytes = fs::read_dir(&self.destination)?
            .map(|entry| Ok(entry?.metadata()?.len()))
            .sum::<std::io::Result<u64>>()?;
        Ok(self.stats)
    }
}

/// What [`StagedCountTable::publish`] published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountTableStats {
    pub entries: u64,
    pub pairs: u64,
    pub documents: u64,
    pub values: u64,
    /// Bytes of every file in the published directory.
    pub bytes: u64,
}

fn sized_file(path: &Path, bytes: u64) -> Result<File, CountTableError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    file.set_len(bytes)?;
    Ok(file)
}

/// A read-only map of `file`, or `None` for an empty file, which cannot be
/// mapped.
fn map_file(file: &File) -> Result<Option<Mmap>, CountTableError> {
    if file.metadata()?.len() == 0 {
        return Ok(None);
    }
    // SAFETY: a published table is immutable for every open reader, as an
    // artifact's components are (crate documentation).
    Ok(Some(unsafe { Mmap::map(file)? }))
}

fn cast<'a, T: bytemuck::Pod>(
    map: Option<&'a Mmap>,
    name: &str,
) -> Result<&'a [T], CountTableError> {
    match map {
        None => Ok(&[]),
        Some(map) => {
            bytemuck::try_cast_slice(&map[..]).map_err(|error| corrupt(format!("{name}: {error}")))
        }
    }
}

struct Section {
    map: Option<Mmap>,
    name: &'static str,
}

/// A count table opened for queries.
pub struct CountTable {
    path: PathBuf,
    header: Header,
    value_offsets: Section,
    values: Section,
    entry_key_offsets: Section,
    entry_keys: Section,
    entry_pair_offsets: Section,
    pair_documents: Section,
    pair_counts: Section,
    ranked_entries: Section,
    ranked_totals: Section,
    document_pair_offsets: Section,
    document_entries: Section,
    document_counts: Section,
    document_ids: Vec<u64>,
    document_sentences: Vec<u64>,
    document_tokens: Vec<u64>,
}

/// The documents a query admits, indexed like the table's documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentMask {
    admitted: Vec<bool>,
}

/// One entry's counts under a mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopRow {
    pub entry: u32,
    pub count: u64,
    /// Admitted documents with at least one occurrence of the entry.
    pub works: u64,
}

/// One pattern's rows, by descending count and then entry order, which is
/// the order and ranking of `ling summarize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternTop {
    pub pattern: usize,
    pub rows: Vec<TopRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopResult {
    pub patterns: Vec<PatternTop>,
    pub documents_admitted: u64,
    pub sentences_admitted: u64,
    pub tokens_admitted: u64,
}

/// What a mask admits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission {
    pub documents_admitted: u64,
    pub sentences_admitted: u64,
    pub tokens_admitted: u64,
}

/// A row in descending count and then entry order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct RowOrder {
    inverted_count: u64,
    entry: u32,
    works: u64,
}

/// The rows of [`CountTable::rows`]: `(pattern, row)` pairs.
pub struct TableRows<'a> {
    table: &'a CountTable,
    mask: &'a DocumentMask,
    policy: SpillPolicy,
    budget: Arc<TempBudget>,
    admission: Admission,
    next_pattern: usize,
    current: Option<(usize, SortedDrain<RowOrder>)>,
    failed: bool,
}

impl TableRows<'_> {
    pub fn admission(&self) -> Admission {
        self.admission
    }
}

impl Iterator for TableRows<'_> {
    type Item = Result<(usize, TopRow), CountTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.failed {
            if let Some((pattern, drain)) = self.current.as_mut() {
                match drain.next_entry() {
                    Ok(Some((row, _))) => {
                        return Some(Ok((
                            *pattern,
                            TopRow {
                                entry: row.entry,
                                count: u64::MAX - row.inverted_count,
                                works: row.works,
                            },
                        )));
                    }
                    Ok(None) => self.current = None,
                    Err(error) => {
                        self.failed = true;
                        return Some(Err(error.into()));
                    }
                }
            }
            let pattern = self.next_pattern;
            if pattern >= self.table.header.patterns.len() {
                return None;
            }
            self.next_pattern += 1;
            match self
                .table
                .pattern_rows(pattern, self.mask, self.policy, &self.budget)
            {
                Ok(drain) => self.current = Some((pattern, drain)),
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
        None
    }
}

impl CountTable {
    /// Opens the table at `path` and rejects it unless its key equals
    /// `expected`. Checks the format version and every section's length; the
    /// contents are checked by [`Self::verify`].
    pub fn open(path: &Path, expected: &CountTableKey) -> Result<Self, CountTableError> {
        let header_bytes = fs::read(path.join(HEADER))?;
        let version: HeaderVersion = serde_json::from_slice(&header_bytes)?;
        if version.format != COUNT_TABLE_FORMAT || version.version != COUNT_TABLE_VERSION {
            return Err(CountTableError::UnsupportedFormat {
                path: path.to_path_buf(),
                found: version.format,
                version: version.version,
            });
        }
        let header: Header = serde_json::from_slice(&header_bytes)?;
        for (field, expected, found) in [
            (
                "analysis identity",
                &expected.analysis_identity,
                &header.key.analysis_identity,
            ),
            (
                "catalog fingerprint",
                &expected.catalog_fingerprint,
                &header.key.catalog_fingerprint,
            ),
            (
                "matcher revision",
                &expected.matcher_revision,
                &header.key.matcher_revision,
            ),
        ] {
            if expected != found {
                return Err(CountTableError::KeyMismatch {
                    path: path.to_path_buf(),
                    field,
                    expected: expected.clone(),
                    found: found.clone(),
                });
            }
        }
        let section = |name: &'static str, bytes: u64| -> Result<Section, CountTableError> {
            let file = File::open(path.join(name))?;
            if file.metadata()?.len() != bytes {
                return Err(corrupt(format!("{name} is not {bytes} bytes")));
            }
            Ok(Section {
                map: map_file(&file)?,
                name,
            })
        };
        let small = |name: &'static str| -> Result<Vec<u64>, CountTableError> {
            let bytes = fs::read(path.join(name))?;
            if bytes.len() as u64 != header.documents * 8 {
                return Err(corrupt(format!("{name} is not one u64 per document")));
            }
            Ok(bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|chunk| u64::from_le_bytes(*chunk))
                .collect())
        };
        let table = Self {
            value_offsets: section(VALUE_OFFSETS, (header.values + 1) * 8)?,
            values: section(VALUES, header.value_bytes)?,
            entry_key_offsets: section(ENTRY_KEY_OFFSETS, (header.entries + 1) * 8)?,
            entry_keys: section(ENTRY_KEYS, header.key_words * 4)?,
            entry_pair_offsets: section(ENTRY_PAIR_OFFSETS, (header.entries + 1) * 8)?,
            pair_documents: section(PAIR_DOCUMENTS, header.pairs * 4)?,
            pair_counts: section(PAIR_COUNTS, header.pairs * 4)?,
            ranked_entries: section(RANKED_ENTRIES, header.entries * 4)?,
            ranked_totals: section(RANKED_TOTALS, header.entries * 8)?,
            document_pair_offsets: section(DOCUMENT_PAIR_OFFSETS, (header.documents + 1) * 8)?,
            document_entries: section(DOCUMENT_ENTRIES, header.pairs * 4)?,
            document_counts: section(DOCUMENT_COUNTS, header.pairs * 4)?,
            document_ids: small(DOCUMENT_IDS)?,
            document_sentences: small(DOCUMENT_SENTENCES)?,
            document_tokens: small(DOCUMENT_TOKENS)?,
            path: path.to_path_buf(),
            header,
        };
        let mut next = 0;
        for pattern in &table.header.patterns {
            let [start, end] = pattern.entries;
            if start != next || end < start {
                return Err(corrupt("pattern entry ranges are not contiguous"));
            }
            next = end;
        }
        if next != table.header.entries {
            return Err(corrupt("pattern entry ranges do not cover the entries"));
        }
        Ok(table)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn key(&self) -> &CountTableKey {
        &self.header.key
    }

    /// Pattern names, indexed as [`PatternTop::pattern`].
    pub fn pattern_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.header
            .patterns
            .iter()
            .map(|pattern| pattern.name.as_str())
    }

    pub fn entry_count(&self) -> u64 {
        self.header.entries
    }

    pub fn pair_count(&self) -> u64 {
        self.header.pairs
    }

    /// The document ID of each table document, ascending.
    pub fn document_ids(&self) -> &[u64] {
        &self.document_ids
    }

    /// The mask admitting each document whose ID `admit` accepts.
    pub fn mask(&self, mut admit: impl FnMut(u64) -> bool) -> DocumentMask {
        DocumentMask {
            admitted: self.document_ids.iter().map(|id| admit(*id)).collect(),
        }
    }

    fn u32s<'a>(&'a self, section: &'a Section) -> Result<&'a [u32], CountTableError> {
        cast(section.map.as_ref(), section.name)
    }

    fn u64s<'a>(&'a self, section: &'a Section) -> Result<&'a [u64], CountTableError> {
        cast(section.map.as_ref(), section.name)
    }

    fn value(&self, rank: u32) -> Result<&str, CountTableError> {
        let offsets = self.u64s(&self.value_offsets)?;
        let start = *offsets
            .get(rank as usize)
            .ok_or_else(|| corrupt("value rank out of range"))?;
        let end = *offsets
            .get(rank as usize + 1)
            .ok_or_else(|| corrupt("value rank out of range"))?;
        let bytes = self
            .values
            .map
            .as_ref()
            .and_then(|map| map.get(start as usize..end as usize))
            .ok_or_else(|| corrupt("value offsets out of range"))?;
        std::str::from_utf8(bytes).map_err(|_| corrupt("value is not UTF-8"))
    }

    /// The key of `entry`.
    pub fn group(&self, entry: u32) -> Result<Group<'_>, CountTableError> {
        let offsets = self.u64s(&self.entry_key_offsets)?;
        let (start, end) = (
            *offsets
                .get(entry as usize)
                .ok_or_else(|| corrupt("entry out of range"))?,
            *offsets
                .get(entry as usize + 1)
                .ok_or_else(|| corrupt("entry out of range"))?,
        );
        let words = self
            .u32s(&self.entry_keys)?
            .get(start as usize..end as usize)
            .ok_or_else(|| corrupt("entry key offsets out of range"))?;
        let (&schema, words) = words
            .split_first()
            .ok_or_else(|| corrupt("empty entry key"))?;
        let shape = self
            .header
            .schemas
            .get(schema as usize)
            .ok_or_else(|| corrupt("entry key schema out of range"))?;
        if key_payload_len(shape, words, self.header.values) != Some(words.len()) {
            return Err(corrupt("entry key does not match its schema"));
        }
        Ok(match shape.kind {
            GroupKind::Surface => Group::Surface(self.value(words[0])?),
            GroupKind::Unit => Group::Unit(self.value(words[0])?),
            GroupKind::Term => Group::Term(self.value(words[0])?),
            GroupKind::Key => Group::Key(
                shape
                    .names
                    .iter()
                    .zip(words)
                    .map(|(name, rank)| Ok((name.as_str(), FieldValue::Scalar(self.value(*rank)?))))
                    .collect::<Result<_, CountTableError>>()?,
            ),
            GroupKind::LexicalKey => {
                let mut fields = Vec::with_capacity(shape.names.len());
                let mut at = 0usize;
                for name in &shape.names {
                    let marker = words[at];
                    if marker == 0 {
                        fields.push((
                            name.as_str(),
                            FieldValue::Scalar(self.value(words[at + 1])?),
                        ));
                        at += 2;
                    } else {
                        let width = marker as usize - 1;
                        let list = words[at + 1..at + 1 + width]
                            .iter()
                            .map(|rank| self.value(*rank))
                            .collect::<Result<_, _>>()?;
                        fields.push((name.as_str(), FieldValue::List(list)));
                        at += 1 + width;
                    }
                }
                Group::Key(fields)
            }
        })
    }

    /// Each pattern's `limit` highest-count entries under `mask`, or all of
    /// its entries with a nonzero count when it has fewer. [`Self::rows`]
    /// streams every entry instead.
    pub fn top(&self, mask: &DocumentMask, limit: usize) -> Result<TopResult, CountTableError> {
        self.top_with_crossover(mask, limit, DOCUMENT_MAJOR_PAIRS)
    }

    /// [`Self::top`], summing the admitted documents' pairs when they number
    /// at most `document_major_pairs` and scanning entries otherwise. The
    /// result does not depend on the crossover.
    pub fn top_with_crossover(
        &self,
        mask: &DocumentMask,
        limit: usize,
        document_major_pairs: u64,
    ) -> Result<TopResult, CountTableError> {
        let (admission, admitted_pairs) = self.admission(mask)?;
        let mut result = TopResult {
            patterns: Vec::with_capacity(self.header.patterns.len()),
            documents_admitted: admission.documents_admitted,
            sentences_admitted: admission.sentences_admitted,
            tokens_admitted: admission.tokens_admitted,
        };
        if limit == 0 || admitted_pairs == 0 {
            result.patterns = (0..self.header.patterns.len())
                .map(|pattern| PatternTop {
                    pattern,
                    rows: Vec::new(),
                })
                .collect();
            return Ok(result);
        }
        result.patterns = if admitted_pairs <= document_major_pairs {
            self.top_by_document(mask, limit)?
        } else {
            (0..self.header.patterns.len())
                .map(|pattern| {
                    Ok(PatternTop {
                        pattern,
                        rows: self.top_by_rank(pattern, mask, limit)?,
                    })
                })
                .collect::<Result<_, CountTableError>>()?
        };
        Ok(result)
    }

    /// The documents, sentences and tokens `mask` admits, and the pairs its
    /// documents hold.
    fn admission(&self, mask: &DocumentMask) -> Result<(Admission, u64), CountTableError> {
        if mask.admitted.len() != self.document_ids.len() {
            return Err(corrupt("mask was built for a different table"));
        }
        let document_offsets = self.u64s(&self.document_pair_offsets)?;
        let mut admission = Admission {
            documents_admitted: 0,
            sentences_admitted: 0,
            tokens_admitted: 0,
        };
        let mut admitted_pairs = 0u64;
        for (document, admitted) in mask.admitted.iter().enumerate() {
            if *admitted {
                admission.documents_admitted += 1;
                admission.sentences_admitted += self.document_sentences[document];
                admission.tokens_admitted += self.document_tokens[document];
                admitted_pairs += document_offsets[document + 1]
                    .checked_sub(document_offsets[document])
                    .ok_or_else(|| corrupt("document pair offsets decrease"))?;
            }
        }
        Ok((admission, admitted_pairs))
    }

    /// Every entry with a nonzero count under `mask`, pattern by pattern and
    /// within a pattern in [`PatternTop`]'s order. Each pattern's rows are
    /// ordered through sorted runs that hold at most
    /// `policy.max_pending_keys` rows in memory and reserve their files from
    /// `budget`.
    pub fn rows<'a>(
        &'a self,
        mask: &'a DocumentMask,
        policy: SpillPolicy,
        budget: Arc<TempBudget>,
    ) -> Result<TableRows<'a>, CountTableError> {
        let (admission, _) = self.admission(mask)?;
        Ok(TableRows {
            table: self,
            mask,
            policy,
            budget,
            admission,
            next_pattern: 0,
            current: None,
            failed: false,
        })
    }

    /// One pattern's rows with a nonzero count, ordered as [`RowOrder`].
    fn pattern_rows(
        &self,
        pattern: usize,
        mask: &DocumentMask,
        policy: SpillPolicy,
        budget: &Arc<TempBudget>,
    ) -> Result<SortedDrain<RowOrder>, CountTableError> {
        let (start, end) = self.pattern_range(pattern);
        let mut ordered = SortedRunCounts::with_policy(policy, Arc::clone(budget))?;
        let mut at = start;
        while at < end {
            let stop = (at + CANCELLATION_INTERVAL).min(end);
            let rows = (at..stop)
                .into_par_iter()
                .with_min_len(4096)
                .map(|entry| self.conditioned(entry as u32, &mask.admitted))
                .filter(|row| row.as_ref().map_or(true, |row| row.count > 0))
                .collect::<Result<Vec<_>, _>>()?;
            for row in rows {
                ordered.add(
                    RowOrder {
                        inverted_count: u64::MAX - row.count,
                        entry: row.entry,
                        works: row.works,
                    },
                    1,
                )?;
            }
            at = stop;
        }
        Ok(ordered.into_sorted_drain()?)
    }

    /// One entry's count and works under `mask`.
    fn conditioned(&self, entry: u32, mask: &[bool]) -> Result<TopRow, CountTableError> {
        let offsets = self.u64s(&self.entry_pair_offsets)?;
        let start = *offsets
            .get(entry as usize)
            .ok_or_else(|| corrupt("entry out of range"))? as usize;
        let end = *offsets
            .get(entry as usize + 1)
            .ok_or_else(|| corrupt("entry out of range"))? as usize;
        let documents = self
            .u32s(&self.pair_documents)?
            .get(start..end)
            .ok_or_else(|| corrupt("entry pair offsets out of range"))?;
        let counts = self
            .u32s(&self.pair_counts)?
            .get(start..end)
            .ok_or_else(|| corrupt("entry pair offsets out of range"))?;
        let mut row = TopRow {
            entry,
            count: 0,
            works: 0,
        };
        for (document, count) in documents.iter().zip(counts) {
            if mask.get(*document as usize).copied() == Some(true) {
                row.count += u64::from(*count);
                row.works += 1;
            }
        }
        Ok(row)
    }

    fn pattern_range(&self, pattern: usize) -> (usize, usize) {
        let [start, end] = self.header.patterns[pattern].entries;
        (start as usize, end as usize)
    }

    /// Scans the pattern's entries by descending total and stops once an
    /// entry's total is below the `limit`-th count found, which no later entry
    /// can exceed.
    fn top_by_rank(
        &self,
        pattern: usize,
        mask: &DocumentMask,
        limit: usize,
    ) -> Result<Vec<TopRow>, CountTableError> {
        let (start, end) = self.pattern_range(pattern);
        let order = self
            .u32s(&self.ranked_entries)?
            .get(start..end)
            .ok_or_else(|| corrupt("ranked entries out of range"))?;
        let totals = self
            .u64s(&self.ranked_totals)?
            .get(start..end)
            .ok_or_else(|| corrupt("ranked totals out of range"))?;
        let limit = limit.min(order.len());
        let mut best: Vec<TopRow> = Vec::with_capacity(limit.saturating_mul(2).min(order.len()));
        let mut at = 0usize;
        let mut block = 64usize;
        while at < order.len() {
            let floor = if best.len() >= limit {
                best[limit - 1].count
            } else {
                0
            };
            if floor > 0 && totals[at] < floor {
                break;
            }
            let stop = (at + block).min(order.len());
            let found = order[at..stop]
                .par_iter()
                .with_min_len(1024)
                .map(|entry| self.conditioned(*entry, &mask.admitted))
                .filter(|row| {
                    row.as_ref()
                        .map_or(true, |row| row.count > 0 && row.count >= floor)
                })
                .collect::<Result<Vec<_>, _>>()?;
            best.extend(found);
            best = ranked(best, limit);
            at = stop;
            block = (block * 2).min(1 << 20);
        }
        Ok(best)
    }

    /// Sums the admitted documents' own pairs: cost proportional to the
    /// pairs those documents hold.
    fn top_by_document(
        &self,
        mask: &DocumentMask,
        limit: usize,
    ) -> Result<Vec<PatternTop>, CountTableError> {
        let offsets = self.u64s(&self.document_pair_offsets)?;
        let entries = self.u32s(&self.document_entries)?;
        let counts = self.u32s(&self.document_counts)?;
        let mut sums: rustc_hash::FxHashMap<u32, (u64, u64)> = rustc_hash::FxHashMap::default();
        for (document, admitted) in mask.admitted.iter().enumerate() {
            if !*admitted {
                continue;
            }
            let range = offsets[document] as usize..offsets[document + 1] as usize;
            let (Some(entries), Some(counts)) = (entries.get(range.clone()), counts.get(range))
            else {
                return Err(corrupt("document pair offsets out of range"));
            };
            for (entry, count) in entries.iter().zip(counts) {
                let sum = sums.entry(*entry).or_insert((0, 0));
                sum.0 += u64::from(*count);
                sum.1 += 1;
            }
        }
        let mut per_pattern: Vec<Vec<TopRow>> = vec![Vec::new(); self.header.patterns.len()];
        for (entry, (count, works)) in sums {
            let pattern = self
                .header
                .patterns
                .partition_point(|pattern| pattern.entries[1] <= u64::from(entry));
            per_pattern
                .get_mut(pattern)
                .ok_or_else(|| corrupt("document entry out of range"))?
                .push(TopRow {
                    entry,
                    count,
                    works,
                });
        }
        Ok(per_pattern
            .into_iter()
            .enumerate()
            .map(|(pattern, rows)| PatternTop {
                pattern,
                rows: ranked(rows, limit),
            })
            .collect())
    }

    /// Checks every offset, document index, count and key against the
    /// header; reads the whole table.
    pub fn verify(&self) -> Result<(), CountTableError> {
        let monotonic = |offsets: &[u64], last: u64, name: &str| {
            if offsets.first() != Some(&0)
                || offsets.last() != Some(&last)
                || offsets.windows(2).any(|pair| pair[0] > pair[1])
            {
                Err(corrupt(format!(
                    "{name} are not ascending from 0 to {last}"
                )))
            } else {
                Ok(())
            }
        };
        monotonic(
            self.u64s(&self.value_offsets)?,
            self.header.value_bytes,
            "value offsets",
        )?;
        monotonic(
            self.u64s(&self.entry_key_offsets)?,
            self.header.key_words,
            "entry key offsets",
        )?;
        monotonic(
            self.u64s(&self.entry_pair_offsets)?,
            self.header.pairs,
            "entry pair offsets",
        )?;
        monotonic(
            self.u64s(&self.document_pair_offsets)?,
            self.header.pairs,
            "document pair offsets",
        )?;
        for rank in 0..self.header.values as u32 {
            self.value(rank)?;
        }
        if self.document_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(corrupt("document IDs are not ascending"));
        }
        let entry_offsets = self.u64s(&self.entry_pair_offsets)?;
        let documents = self.u32s(&self.pair_documents)?;
        let counts = self.u32s(&self.pair_counts)?;
        let mut totals = Vec::with_capacity(self.header.entries as usize);
        for (entry, range) in entry_offsets.windows(2).enumerate() {
            self.group(entry as u32)?;
            let range = range[0] as usize..range[1] as usize;
            if range.is_empty() {
                return Err(corrupt(format!("entry {entry} has no pair")));
            }
            let documents = &documents[range.clone()];
            if documents.windows(2).any(|pair| pair[0] >= pair[1])
                || documents
                    .last()
                    .is_some_and(|last| u64::from(*last) >= self.header.documents)
            {
                return Err(corrupt(format!("entry {entry} has invalid documents")));
            }
            if counts[range.clone()].contains(&0) {
                return Err(corrupt(format!("entry {entry} has a zero count")));
            }
            totals.push(
                counts[range]
                    .iter()
                    .map(|count| u64::from(*count))
                    .sum::<u64>(),
            );
        }
        let order = self.u32s(&self.ranked_entries)?;
        let ranked_totals = self.u64s(&self.ranked_totals)?;
        for pattern in 0..self.header.patterns.len() {
            let (start, end) = self.pattern_range(pattern);
            let mut seen = order[start..end].to_vec();
            seen.sort_unstable();
            if seen.iter().copied().ne(start as u32..end as u32) {
                return Err(corrupt(
                    "ranked entries are not a permutation of the pattern",
                ));
            }
            for position in start..end {
                let entry = order[position] as usize;
                if ranked_totals[position] != totals[entry]
                    || (position + 1 < end && {
                        let next = order[position + 1] as usize;
                        (totals[next], std::cmp::Reverse(next))
                            > (totals[entry], std::cmp::Reverse(entry))
                    })
                {
                    return Err(corrupt("ranked entries are out of order"));
                }
            }
        }
        let document_offsets = self.u64s(&self.document_pair_offsets)?;
        let document_entries = self.u32s(&self.document_entries)?;
        let document_counts = self.u32s(&self.document_counts)?;
        let mut cursor = document_offsets.to_vec();
        for (entry, range) in entry_offsets.windows(2).enumerate() {
            for pair in range[0] as usize..range[1] as usize {
                let document = documents[pair] as usize;
                let at = cursor[document] as usize;
                cursor[document] += 1;
                if cursor[document] > document_offsets[document + 1]
                    || document_entries.get(at) != Some(&(entry as u32))
                    || document_counts.get(at) != Some(&counts[pair])
                {
                    return Err(corrupt(
                        "document-major pairs differ from entry-major pairs",
                    ));
                }
            }
        }
        if cursor[..self.document_ids.len()] != document_offsets[1..] {
            return Err(corrupt(
                "document pair offsets differ from each document's pairs",
            ));
        }
        Ok(())
    }
}

/// `rows` by descending count and then entry, truncated to `limit`.
fn ranked(mut rows: Vec<TopRow>, limit: usize) -> Vec<TopRow> {
    let order = |left: &TopRow, right: &TopRow| {
        right
            .count
            .cmp(&left.count)
            .then(left.entry.cmp(&right.entry))
    };
    if rows.len() > limit {
        rows.select_nth_unstable_by(limit - 1, order);
        rows.truncate(limit);
    }
    rows.sort_unstable_by(order);
    rows
}
