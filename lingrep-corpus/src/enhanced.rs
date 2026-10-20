//! Sparse, block-compressed enhanced-dependency graph observations.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
};

use lingrep_core::corpus::conllu::{
    EnhancedEdgeData, EnhancedEmptyNodeData, EnhancedGraph, EnhancedGraphObservation, EnhancedHead,
    EnhancedNodeId,
};
use memmap2::{Mmap, MmapOptions};

use lingrep_core::spill::TempBudget;

use crate::{AnalysisIdentity, CorpusError, TokenSchemaIdentity, scratch::BudgetedScratch};

pub(crate) const ENHANCED_NAME: &str = "enhanced.v1.zst";
pub(crate) const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"LINGENH1";
const BLOCK_MAGIC: &[u8; 8] = b"LGEBLK01";
const FOOTER_MAGIC: &[u8; 8] = b"LGEEND01";
const HEADER_LEN: u64 = 128;
const FOOTER_LEN: u64 = 48;
const DIRECTORY_ENTRY_LEN: u64 = 80;
const BLOCK_HEADER_LEN: u64 = 32;
const SENTENCE_ENTRY_LEN: u64 = 24;
const BLOCK_RECORD_LIMIT: usize = 256;
const MAX_BLOCK_RAW_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn normalization_identity() -> [u8; 32] {
    *blake3::hash(b"lingrep.enhanced-graph-normalization/v1").as_bytes()
}

#[derive(Clone, Copy)]
struct DirectoryEntry {
    first_sentence: u64,
    last_sentence: u64,
    record_count: u64,
    file_offset: u64,
    stored_len: u64,
    raw_len: u64,
    checksum: [u8; 32],
}

#[derive(Clone)]
struct BufferedObservation {
    ordinal: u64,
    word_count: usize,
    observation: EnhancedGraphObservation,
}

pub(crate) struct EnhancedWriter {
    file: File,
    buffered: Vec<BufferedObservation>,
    directory: BudgetedScratch,
    block_count: u64,
    sentence_count: u64,
}

impl EnhancedWriter {
    pub(crate) fn create(path: &Path, budget: &Arc<TempBudget>) -> Result<Self, CorpusError> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&[0; HEADER_LEN as usize])?;
        Ok(Self {
            file,
            buffered: Vec::with_capacity(BLOCK_RECORD_LIMIT),
            directory: BudgetedScratch::create(&path.with_extension("directory.scratch"), budget)?,
            block_count: 0,
            sentence_count: 0,
        })
    }

    pub(crate) fn push(
        &mut self,
        observation: EnhancedGraphObservation,
        word_count: usize,
    ) -> Result<(), CorpusError> {
        if observation
            .graph()
            .is_some_and(|graph| graph.word_count() != word_count)
        {
            return Err(invalid(
                "enhanced observation word count differs from its token sentence",
            ));
        }
        let ordinal = self.sentence_count;
        self.sentence_count = self
            .sentence_count
            .checked_add(1)
            .ok_or_else(|| invalid("enhanced sentence count overflow"))?;
        if matches!(observation, EnhancedGraphObservation::Unavailable) {
            return Ok(());
        }
        self.buffered.push(BufferedObservation {
            ordinal,
            word_count,
            observation,
        });
        if self.buffered.len() == BLOCK_RECORD_LIMIT {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), CorpusError> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let raw = encode_block(&self.buffered)?;
        if raw.len() > MAX_BLOCK_RAW_BYTES {
            return Err(invalid("enhanced raw block exceeds 64 MiB"));
        }
        let stored = zstd::bulk::compress(&raw, 3)
            .map_err(|error| invalid(format!("enhanced block compression failed: {error}")))?;
        let first = self.buffered[0].ordinal;
        let last = self
            .buffered
            .last()
            .expect("a nonempty block has a last record")
            .ordinal;
        let entry = DirectoryEntry {
            first_sentence: first,
            last_sentence: last,
            record_count: self.buffered.len() as u64,
            file_offset: self.file.stream_position()?,
            stored_len: stored.len() as u64,
            raw_len: raw.len() as u64,
            checksum: *blake3::hash(&stored).as_bytes(),
        };
        self.file.write_all(&stored)?;
        let mut bytes = Vec::with_capacity(DIRECTORY_ENTRY_LEN as usize);
        encode_directory_entry(entry, &mut bytes);
        self.directory.append(&bytes)?;
        self.block_count = self
            .block_count
            .checked_add(1)
            .ok_or_else(|| invalid("enhanced block count overflow"))?;
        self.buffered.clear();
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
        analysis: AnalysisIdentity,
        logical_schema: TokenSchemaIdentity,
    ) -> Result<(), CorpusError> {
        self.flush()?;
        let directory_offset = self.file.stream_position()?;
        self.directory.rewind()?;
        let mut directory_hash = blake3::Hasher::new();
        let mut entry = [0_u8; DIRECTORY_ENTRY_LEN as usize];
        for _ in 0..self.block_count {
            self.directory.read_exact(&mut entry)?;
            self.file.write_all(&entry)?;
            directory_hash.update(&entry);
        }
        let directory_len = self
            .block_count
            .checked_mul(DIRECTORY_ENTRY_LEN)
            .ok_or_else(|| invalid("enhanced directory length overflow"))?;
        let mut footer = Vec::with_capacity(FOOTER_LEN as usize);
        footer.extend_from_slice(&directory_len.to_le_bytes());
        footer.extend_from_slice(directory_hash.finalize().as_bytes());
        footer.extend_from_slice(FOOTER_MAGIC);
        self.file.write_all(&footer)?;

        let mut header = vec![0_u8; HEADER_LEN as usize];
        header[0..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        header[16..48].copy_from_slice(analysis.as_bytes());
        header[48..80].copy_from_slice(logical_schema.as_bytes());
        header[80..88].copy_from_slice(&self.sentence_count.to_le_bytes());
        header[88..96].copy_from_slice(&self.block_count.to_le_bytes());
        header[96..104].copy_from_slice(&directory_offset.to_le_bytes());
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)?;
        self.file.sync_all()?;
        Ok(())
    }

    pub(crate) fn sentence_count(&self) -> u64 {
        self.sentence_count
    }
}

pub(crate) struct EnhancedReader {
    _path: PathBuf,
    map: Mmap,
    sentence_count: u64,
    block_count: u64,
    directory_offset: u64,
    decoded_blocks: AtomicUsize,
}

impl EnhancedReader {
    pub(crate) fn open(
        path: &Path,
        expected_analysis: &AnalysisIdentity,
        expected_logical_schema: &TokenSchemaIdentity,
    ) -> Result<Self, CorpusError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len < HEADER_LEN + FOOTER_LEN {
            return Err(truncated(len));
        }
        let map = unsafe { MmapOptions::new().map(&file)? };
        if slice(&map, 0, 8)? != MAGIC {
            return Err(invalid("wrong enhanced component magic"));
        }
        let version = read_u32(&map, 8)?;
        if version != FORMAT_VERSION {
            return Err(CorpusError::UnsupportedSchema {
                component: ENHANCED_NAME,
                version,
            });
        }
        if read_u32(&map, 12)? != HEADER_LEN as u32 {
            return Err(invalid("wrong enhanced header length"));
        }
        if &AnalysisIdentity::from_bytes(read_array(&map, 16)?) != expected_analysis {
            return Err(CorpusError::AnalysisIdentityMismatch {
                component: ENHANCED_NAME,
            });
        }
        if &TokenSchemaIdentity::from_bytes(read_array(&map, 48)?) != expected_logical_schema {
            return Err(CorpusError::VerificationMismatch {
                component: ENHANCED_NAME,
                sentence_ordinal: None,
                message: "enhanced logical schema identity differs from the manifest".into(),
            });
        }
        if slice(&map, 104, 24)?.iter().any(|byte| *byte != 0) {
            return Err(invalid("nonzero enhanced header reserved bytes"));
        }
        let sentence_count = read_u64(&map, 80)?;
        let block_count = read_u64(&map, 88)?;
        let directory_offset = read_u64(&map, 96)?;
        let directory_len = block_count
            .checked_mul(DIRECTORY_ENTRY_LEN)
            .ok_or_else(|| invalid("enhanced directory length overflow"))?;
        let footer_offset = len - FOOTER_LEN;
        if directory_offset < HEADER_LEN
            || directory_offset.checked_add(directory_len) != Some(footer_offset)
        {
            return Err(invalid("invalid enhanced directory bounds"));
        }
        let footer = slice(&map, footer_offset, FOOTER_LEN)?;
        if read_u64(footer, 0)? != directory_len || &footer[40..48] != FOOTER_MAGIC {
            return Err(invalid("invalid enhanced footer"));
        }
        let directory = slice(&map, directory_offset, directory_len)?;
        if blake3::hash(directory).as_bytes() != &footer[8..40] {
            return Err(invalid("enhanced directory checksum mismatch"));
        }
        let reader = Self {
            _path: path.to_path_buf(),
            map,
            sentence_count,
            block_count,
            directory_offset,
            decoded_blocks: AtomicUsize::new(0),
        };
        reader.validate_directory()?;
        Ok(reader)
    }

    pub(crate) fn sentence_count(&self) -> u64 {
        self.sentence_count
    }

    pub(crate) fn decoded_block_count(&self) -> usize {
        self.decoded_blocks.load(Ordering::Relaxed)
    }

    pub(crate) fn observation(
        &self,
        ordinal: u64,
    ) -> Result<EnhancedGraphObservation, CorpusError> {
        if ordinal >= self.sentence_count {
            return Err(invalid(
                "enhanced sentence ordinal is outside the component",
            ));
        }
        let Some(entry) = self.block_for_sentence(ordinal)? else {
            return Ok(EnhancedGraphObservation::Unavailable);
        };
        let records = self.decode_block(entry)?;
        Ok(records
            .into_iter()
            .find_map(|(candidate, observation)| (candidate == ordinal).then_some(observation))
            .unwrap_or(EnhancedGraphObservation::Unavailable))
    }

    pub(crate) fn observations(
        &self,
        ordinals: &[u64],
    ) -> Result<Vec<EnhancedGraphObservation>, CorpusError> {
        let mut output = Vec::with_capacity(ordinals.len());
        let mut cached = None::<(u64, Vec<(u64, EnhancedGraphObservation)>)>;
        for &ordinal in ordinals {
            if ordinal >= self.sentence_count {
                return Err(invalid(
                    "enhanced sentence ordinal is outside the component",
                ));
            }
            let Some(entry) = self.block_for_sentence(ordinal)? else {
                output.push(EnhancedGraphObservation::Unavailable);
                continue;
            };
            if cached
                .as_ref()
                .is_none_or(|(offset, _)| *offset != entry.file_offset)
            {
                cached = Some((entry.file_offset, self.decode_block(entry)?));
            }
            let observation = cached
                .as_ref()
                .expect("the covering block was cached")
                .1
                .iter()
                .find_map(|(candidate, observation)| {
                    (*candidate == ordinal).then(|| observation.clone())
                })
                .unwrap_or(EnhancedGraphObservation::Unavailable);
            output.push(observation);
        }
        Ok(output)
    }

    pub(crate) fn verify_integrity(&self) -> Result<(), CorpusError> {
        for index in 0..self.block_count {
            let entry = self.directory_entry(index)?;
            self.decode_block(entry)?;
        }
        Ok(())
    }

    fn validate_directory(&self) -> Result<(), CorpusError> {
        let mut previous_last = None;
        let mut expected_offset = HEADER_LEN;
        for index in 0..self.block_count {
            let entry = self.directory_entry(index)?;
            if entry.record_count == 0
                || entry.record_count > BLOCK_RECORD_LIMIT as u64
                || entry.first_sentence > entry.last_sentence
                || entry.last_sentence >= self.sentence_count
                || previous_last.is_some_and(|last| last >= entry.first_sentence)
                || entry.file_offset != expected_offset
                || entry.stored_len == 0
                || entry.raw_len == 0
                || entry.raw_len > MAX_BLOCK_RAW_BYTES as u64
            {
                return Err(invalid("noncanonical enhanced directory"));
            }
            previous_last = Some(entry.last_sentence);
            expected_offset = entry
                .file_offset
                .checked_add(entry.stored_len)
                .ok_or_else(|| invalid("enhanced block bounds overflow"))?;
            if expected_offset > self.directory_offset {
                return Err(invalid("enhanced block exceeds its payload section"));
            }
        }
        if expected_offset != self.directory_offset {
            return Err(invalid("enhanced payload and directory are not contiguous"));
        }
        Ok(())
    }

    fn directory_entry(&self, index: u64) -> Result<DirectoryEntry, CorpusError> {
        if index >= self.block_count {
            return Err(invalid("enhanced block index is outside the directory"));
        }
        decode_directory_entry(slice(
            &self.map,
            self.directory_offset + index * DIRECTORY_ENTRY_LEN,
            DIRECTORY_ENTRY_LEN,
        )?)
    }

    fn block_for_sentence(&self, ordinal: u64) -> Result<Option<DirectoryEntry>, CorpusError> {
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
        let Some(index) = low.checked_sub(1) else {
            return Ok(None);
        };
        let entry = self.directory_entry(index)?;
        Ok((ordinal <= entry.last_sentence).then_some(entry))
    }

    fn decode_block(
        &self,
        entry: DirectoryEntry,
    ) -> Result<Vec<(u64, EnhancedGraphObservation)>, CorpusError> {
        let stored = slice(&self.map, entry.file_offset, entry.stored_len)?;
        if blake3::hash(stored).as_bytes() != &entry.checksum {
            return Err(CorpusError::VerificationMismatch {
                component: ENHANCED_NAME,
                sentence_ordinal: Some(entry.first_sentence),
                message: "enhanced block checksum mismatch".into(),
            });
        }
        let raw_len = usize::try_from(entry.raw_len)
            .map_err(|_| invalid("enhanced raw block length exceeds usize"))?;
        if raw_len > MAX_BLOCK_RAW_BYTES {
            return Err(invalid("enhanced raw block exceeds 64 MiB"));
        }
        let raw = zstd::bulk::decompress(stored, raw_len)
            .map_err(|error| invalid(format!("enhanced block decompression failed: {error}")))?;
        if raw.len() != raw_len {
            return Err(invalid("enhanced decompressed length mismatch"));
        }
        self.decoded_blocks.fetch_add(1, Ordering::Relaxed);
        decode_block(&raw, entry)
    }
}

fn encode_block(records: &[BufferedObservation]) -> Result<Vec<u8>, CorpusError> {
    let mut strings = Vec::<String>::new();
    let mut ids = HashMap::<String, u64>::new();
    let mut frames = Vec::new();
    let mut table = Vec::with_capacity(records.len() * SENTENCE_ENTRY_LEN as usize);
    for record in records {
        let offset = frames.len() as u64;
        encode_frame(record, &mut frames, &mut strings, &mut ids)?;
        table.extend_from_slice(&record.ordinal.to_le_bytes());
        table.extend_from_slice(&offset.to_le_bytes());
        table.extend_from_slice(&((frames.len() as u64) - offset).to_le_bytes());
    }
    let mut string_bytes = Vec::new();
    for value in &strings {
        write_varint(value.len() as u64, &mut string_bytes);
        string_bytes.extend_from_slice(value.as_bytes());
    }
    let mut raw = Vec::with_capacity(
        BLOCK_HEADER_LEN as usize + string_bytes.len() + table.len() + frames.len(),
    );
    raw.extend_from_slice(BLOCK_MAGIC);
    raw.extend_from_slice(&(records.len() as u32).to_le_bytes());
    raw.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    raw.extend_from_slice(&(string_bytes.len() as u64).to_le_bytes());
    raw.extend_from_slice(&(table.len() as u64).to_le_bytes());
    raw.extend_from_slice(&string_bytes);
    raw.extend_from_slice(&table);
    raw.extend_from_slice(&frames);
    Ok(raw)
}

fn encode_frame(
    record: &BufferedObservation,
    output: &mut Vec<u8>,
    strings: &mut Vec<String>,
    ids: &mut HashMap<String, u64>,
) -> Result<(), CorpusError> {
    let (tag, graph) = match &record.observation {
        EnhancedGraphObservation::Unavailable => {
            return Err(invalid("unavailable observation entered an enhanced block"));
        }
        EnhancedGraphObservation::Incomplete(graph) => (1_u8, graph),
        EnhancedGraphObservation::Complete(graph) => (2_u8, graph),
    };
    output.push(tag);
    write_varint(record.word_count as u64, output);
    write_varint(graph.empty_nodes().len() as u64, output);
    write_varint(graph.edges().len() as u64, output);
    for node in graph.empty_nodes() {
        let EnhancedNodeId::Empty { boundary, subindex } = node.id() else {
            return Err(invalid("graph empty-node list contains a word"));
        };
        write_varint(boundary as u64, output);
        write_varint(subindex as u64, output);
        for value in [node.form(), node.lemma(), node.upos(), node.xpos()] {
            write_varint(intern_optional(value, strings, ids)?, output);
        }
        write_varint(node.feature_names().len() as u64, output);
        for name in node.feature_names() {
            write_varint(intern(name, strings, ids)?, output);
            let values = node
                .feature_values(name)
                .expect("a feature name came from this node");
            write_varint(values.len() as u64, output);
            for value in values {
                write_varint(intern(value, strings, ids)?, output);
            }
        }
        write_varint(node.raw_features().len() as u64, output);
        for value in node.raw_features() {
            write_varint(intern(value, strings, ids)?, output);
        }
        write_varint(node.misc().len() as u64, output);
        for value in node.misc() {
            write_varint(intern(value, strings, ids)?, output);
        }
    }
    let empty_ids = graph
        .empty_nodes()
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id(), record.word_count + index + 1))
        .collect::<HashMap<_, _>>();
    for edge in graph.edges() {
        let head = match edge.head() {
            EnhancedHead::Root => 0,
            EnhancedHead::Node(node) => encode_node_id(node, record.word_count, &empty_ids)?,
        };
        let dependent = encode_node_id(edge.dependent(), record.word_count, &empty_ids)?;
        write_varint(head as u64, output);
        write_varint(dependent as u64, output);
        write_varint(intern(edge.relation(), strings, ids)?, output);
    }
    Ok(())
}

fn decode_block(
    raw: &[u8],
    entry: DirectoryEntry,
) -> Result<Vec<(u64, EnhancedGraphObservation)>, CorpusError> {
    if raw.len() < BLOCK_HEADER_LEN as usize || &raw[0..8] != BLOCK_MAGIC {
        return Err(invalid("invalid enhanced block header"));
    }
    let record_count = read_u32(raw, 8)? as usize;
    let string_count = read_u32(raw, 12)? as usize;
    let strings_len = read_u64(raw, 16)? as usize;
    let table_len = read_u64(raw, 24)? as usize;
    if record_count as u64 != entry.record_count
        || table_len != record_count * SENTENCE_ENTRY_LEN as usize
    {
        return Err(invalid("enhanced block shape differs from its directory"));
    }
    let strings_start = BLOCK_HEADER_LEN as usize;
    let table_start = strings_start
        .checked_add(strings_len)
        .ok_or_else(|| invalid("enhanced string bounds overflow"))?;
    let frames_start = table_start
        .checked_add(table_len)
        .ok_or_else(|| invalid("enhanced frame bounds overflow"))?;
    if frames_start > raw.len() {
        return Err(invalid("truncated enhanced block tables"));
    }
    let strings = decode_strings(&raw[strings_start..table_start], string_count)?;
    let table = &raw[table_start..frames_start];
    let frames = &raw[frames_start..];
    let mut output = Vec::with_capacity(record_count);
    let mut previous = None;
    let mut next_frame = 0_u64;
    for index in 0..record_count {
        let base = index as u64 * SENTENCE_ENTRY_LEN;
        let ordinal = read_u64(table, base)?;
        let offset = read_u64(table, base + 8)?;
        let len = read_u64(table, base + 16)?;
        if previous.is_some_and(|prior| prior >= ordinal)
            || ordinal < entry.first_sentence
            || ordinal > entry.last_sentence
            || offset != next_frame
            || offset
                .checked_add(len)
                .is_none_or(|end| end > frames.len() as u64)
        {
            return Err(invalid("noncanonical enhanced sentence table"));
        }
        let frame = &frames[offset as usize..(offset + len) as usize];
        output.push((ordinal, decode_frame(frame, &strings)?));
        previous = Some(ordinal);
        next_frame = offset + len;
    }
    if output.first().map(|record| record.0) != Some(entry.first_sentence)
        || output.last().map(|record| record.0) != Some(entry.last_sentence)
        || next_frame != frames.len() as u64
    {
        return Err(invalid(
            "enhanced block endpoints or frames are noncanonical",
        ));
    }
    Ok(output)
}

fn decode_frame(frame: &[u8], strings: &[&str]) -> Result<EnhancedGraphObservation, CorpusError> {
    let Some((&tag, rest)) = frame.split_first() else {
        return Err(invalid("empty enhanced frame"));
    };
    let mut cursor = 0_usize;
    let word_count = read_varint(rest, &mut cursor)? as usize;
    let empty_count = read_varint(rest, &mut cursor)? as usize;
    let edge_count = read_varint(rest, &mut cursor)? as usize;
    let mut empty_nodes = Vec::with_capacity(empty_count);
    for _ in 0..empty_count {
        let boundary = read_varint(rest, &mut cursor)? as usize;
        let subindex = read_varint(rest, &mut cursor)? as usize;
        let form = read_optional_string(rest, &mut cursor, strings)?;
        let lemma = read_optional_string(rest, &mut cursor, strings)?;
        let upos = read_optional_string(rest, &mut cursor, strings)?;
        let xpos = read_optional_string(rest, &mut cursor, strings)?;
        let feature_count = read_varint(rest, &mut cursor)? as usize;
        let mut features = Vec::with_capacity(feature_count);
        for _ in 0..feature_count {
            let name = read_string(rest, &mut cursor, strings)?.to_owned();
            let value_count = read_varint(rest, &mut cursor)? as usize;
            let mut values = Vec::with_capacity(value_count);
            for _ in 0..value_count {
                values.push(read_string(rest, &mut cursor, strings)?.to_owned());
            }
            features.push((name, values));
        }
        let raw_features = read_string_list(rest, &mut cursor, strings)?;
        let misc = read_string_list(rest, &mut cursor, strings)?;
        empty_nodes.push(EnhancedEmptyNodeData {
            id: EnhancedNodeId::Empty { boundary, subindex },
            form,
            lemma,
            upos,
            xpos,
            features,
            raw_features,
            misc,
        });
    }
    let empty_ids = empty_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (word_count + index + 1, node.id))
        .collect::<HashMap<_, _>>();
    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        let head_id = read_varint(rest, &mut cursor)? as usize;
        let dependent_id = read_varint(rest, &mut cursor)? as usize;
        let relation = read_string(rest, &mut cursor, strings)?.to_owned();
        let head = if head_id == 0 {
            EnhancedHead::Root
        } else {
            EnhancedHead::Node(decode_node_id(head_id, word_count, &empty_ids)?)
        };
        let dependent = decode_node_id(dependent_id, word_count, &empty_ids)?;
        edges.push(EnhancedEdgeData {
            head,
            dependent,
            relation,
        });
    }
    if cursor != rest.len() {
        return Err(invalid("trailing enhanced frame bytes"));
    }
    let graph = EnhancedGraph::from_normalized_parts(word_count, empty_nodes, edges)
        .map_err(|message| invalid(format!("invalid normalized enhanced graph: {message}")))?;
    match tag {
        1 => Ok(EnhancedGraphObservation::Incomplete(graph)),
        2 => Ok(EnhancedGraphObservation::Complete(graph)),
        _ => Err(invalid("invalid enhanced observation tag")),
    }
}

fn encode_node_id(
    node: EnhancedNodeId,
    word_count: usize,
    empty_ids: &HashMap<EnhancedNodeId, usize>,
) -> Result<usize, CorpusError> {
    match node {
        EnhancedNodeId::Word { token_index } if token_index < word_count => Ok(token_index + 1),
        EnhancedNodeId::Word { .. } => Err(invalid("enhanced word endpoint exceeds word count")),
        EnhancedNodeId::Empty { .. } => empty_ids
            .get(&node)
            .copied()
            .ok_or_else(|| invalid("enhanced empty endpoint does not resolve")),
    }
}

fn decode_node_id(
    id: usize,
    word_count: usize,
    empty_ids: &HashMap<usize, EnhancedNodeId>,
) -> Result<EnhancedNodeId, CorpusError> {
    if id == 0 {
        return Err(invalid("root cannot be an enhanced dependent"));
    }
    if id <= word_count {
        return Ok(EnhancedNodeId::Word {
            token_index: id - 1,
        });
    }
    empty_ids
        .get(&id)
        .copied()
        .ok_or_else(|| invalid("enhanced stored node ID does not resolve"))
}

fn intern_optional(
    value: Option<&str>,
    strings: &mut Vec<String>,
    ids: &mut HashMap<String, u64>,
) -> Result<u64, CorpusError> {
    value.map_or(Ok(0), |value| intern(value, strings, ids))
}

fn intern(
    value: &str,
    strings: &mut Vec<String>,
    ids: &mut HashMap<String, u64>,
) -> Result<u64, CorpusError> {
    if let Some(id) = ids.get(value) {
        return Ok(*id);
    }
    let id =
        u64::try_from(strings.len() + 1).map_err(|_| invalid("too many enhanced block strings"))?;
    strings.push(value.to_owned());
    ids.insert(value.to_owned(), id);
    Ok(id)
}

fn decode_strings(bytes: &[u8], count: usize) -> Result<Vec<&str>, CorpusError> {
    let mut cursor = 0_usize;
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_varint(bytes, &mut cursor)? as usize;
        let end = cursor
            .checked_add(len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| invalid("truncated enhanced string"))?;
        strings.push(
            std::str::from_utf8(&bytes[cursor..end])
                .map_err(|_| invalid("enhanced string is not UTF-8"))?,
        );
        cursor = end;
    }
    if cursor != bytes.len() {
        return Err(invalid("trailing enhanced string-table bytes"));
    }
    Ok(strings)
}

fn read_optional_string(
    bytes: &[u8],
    cursor: &mut usize,
    strings: &[&str],
) -> Result<Option<String>, CorpusError> {
    let id = read_varint(bytes, cursor)? as usize;
    if id == 0 {
        Ok(None)
    } else {
        strings
            .get(id - 1)
            .map(|value| Some((*value).to_owned()))
            .ok_or_else(|| invalid("enhanced optional string ID is invalid"))
    }
}

fn read_string<'a>(
    bytes: &[u8],
    cursor: &mut usize,
    strings: &'a [&str],
) -> Result<&'a str, CorpusError> {
    let id = read_varint(bytes, cursor)? as usize;
    if id == 0 {
        return Err(invalid("enhanced required string ID is zero"));
    }
    strings
        .get(id - 1)
        .copied()
        .ok_or_else(|| invalid("enhanced string ID is invalid"))
}

fn read_string_list(
    bytes: &[u8],
    cursor: &mut usize,
    strings: &[&str],
) -> Result<Vec<String>, CorpusError> {
    let count = read_varint(bytes, cursor)? as usize;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read_string(bytes, cursor, strings)?.to_owned());
    }
    Ok(values)
}

fn write_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, CorpusError> {
    let mut value = 0_u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes
            .get(*cursor)
            .ok_or_else(|| invalid("truncated enhanced varint"))?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(invalid("enhanced varint overflow"));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            if shift > 0 && byte == 0 {
                return Err(invalid("noncanonical enhanced varint"));
            }
            return Ok(value);
        }
    }
    Err(invalid("enhanced varint overflow"))
}

fn encode_directory_entry(entry: DirectoryEntry, output: &mut Vec<u8>) {
    output.extend_from_slice(&entry.first_sentence.to_le_bytes());
    output.extend_from_slice(&entry.last_sentence.to_le_bytes());
    output.extend_from_slice(&entry.record_count.to_le_bytes());
    output.extend_from_slice(&entry.file_offset.to_le_bytes());
    output.extend_from_slice(&entry.stored_len.to_le_bytes());
    output.extend_from_slice(&entry.raw_len.to_le_bytes());
    output.extend_from_slice(&entry.checksum);
}

fn decode_directory_entry(bytes: &[u8]) -> Result<DirectoryEntry, CorpusError> {
    Ok(DirectoryEntry {
        first_sentence: read_u64(bytes, 0)?,
        last_sentence: read_u64(bytes, 8)?,
        record_count: read_u64(bytes, 16)?,
        file_offset: read_u64(bytes, 24)?,
        stored_len: read_u64(bytes, 32)?,
        raw_len: read_u64(bytes, 40)?,
        checksum: read_array(bytes, 48)?,
    })
}

fn slice(bytes: &[u8], offset: u64, len: u64) -> Result<&[u8], CorpusError> {
    let start = usize::try_from(offset).map_err(|_| invalid("enhanced offset exceeds usize"))?;
    let len = usize::try_from(len).map_err(|_| invalid("enhanced length exceeds usize"))?;
    let end = start
        .checked_add(len)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid("truncated enhanced component"))?;
    Ok(&bytes[start..end])
}

fn read_u32(bytes: &[u8], offset: u64) -> Result<u32, CorpusError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: u64) -> Result<u64, CorpusError> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: u64) -> Result<[u8; N], CorpusError> {
    slice(bytes, offset, N as u64)?
        .try_into()
        .map_err(|_| invalid("truncated enhanced component"))
}

fn invalid(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: ENHANCED_NAME,
        message: message.into(),
    }
}

fn truncated(_actual: u64) -> CorpusError {
    CorpusError::TruncatedComponent {
        component: ENHANCED_NAME,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::corpus::conllu::Reader;

    const COMPLETE: &str = "# text = cannot\n1\tcan\tcan\tAUX\tMD\t_\t0\troot\t0:root\t_\n2\tnot\tnot\tPART\tRB\t_\t1\tadvmod\t1:advmod\t_\n2.1\treally\treally\tADV\tRB\tDegree=Pos\t_\t_\t1:advmod\tSpaceAfter=No\n\n";

    fn observation(source: &str) -> (EnhancedGraphObservation, usize) {
        let sentence = Reader::new(source.as_bytes(), "fixture.conllu")
            .next()
            .unwrap()
            .unwrap();
        (sentence.enhanced_graph().clone(), sentence.len())
    }

    fn budget() -> Arc<TempBudget> {
        Arc::new(TempBudget::new(16 * 1024 * 1024))
    }

    #[test]
    fn sparse_component_round_trips_all_observation_states() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(ENHANCED_NAME);
        let analysis = AnalysisIdentity::from_bytes([7; 32]);
        let schema = TokenSchemaIdentity::from_bytes([8; 32]);
        let (complete, words) = observation(COMPLETE);
        let incomplete_source = COMPLETE.replacen("1:advmod", "_", 1);
        let (incomplete, incomplete_words) = observation(&incomplete_source);
        let mut writer = EnhancedWriter::create(&path, &budget()).unwrap();
        writer
            .push(EnhancedGraphObservation::Unavailable, 1)
            .unwrap();
        writer.push(incomplete.clone(), incomplete_words).unwrap();
        writer
            .push(EnhancedGraphObservation::Unavailable, 2)
            .unwrap();
        writer.push(complete.clone(), words).unwrap();
        writer.finish(analysis, schema).unwrap();

        let reader = EnhancedReader::open(&path, &analysis, &schema).unwrap();
        assert_eq!(reader.sentence_count(), 4);
        assert_eq!(reader.decoded_block_count(), 0, "ordinary open stays lazy");
        assert_eq!(
            reader.observation(0).unwrap(),
            EnhancedGraphObservation::Unavailable
        );
        assert_eq!(
            reader.decoded_block_count(),
            0,
            "a sparse gap decodes nothing"
        );
        assert_eq!(reader.observation(1).unwrap(), incomplete);
        assert_eq!(
            reader.observation(2).unwrap(),
            EnhancedGraphObservation::Unavailable
        );
        assert_eq!(reader.observation(3).unwrap(), complete);
    }

    #[test]
    fn all_unavailable_capable_component_has_only_fixed_framing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(ENHANCED_NAME);
        let analysis = AnalysisIdentity::from_bytes([7; 32]);
        let schema = TokenSchemaIdentity::from_bytes([8; 32]);
        let mut writer = EnhancedWriter::create(&path, &budget()).unwrap();
        for _ in 0..16_236 {
            writer
                .push(EnhancedGraphObservation::Unavailable, 1)
                .unwrap();
        }
        writer.finish(analysis, schema).unwrap();

        assert_eq!(std::fs::metadata(&path).unwrap().len(), 176);
        let reader = EnhancedReader::open(&path, &analysis, &schema).unwrap();
        assert_eq!(reader.sentence_count(), 16_236);
        assert_eq!(reader.decoded_block_count(), 0);
    }

    #[test]
    fn writer_rejects_graphs_for_a_different_token_sentence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(ENHANCED_NAME);
        let (complete, words) = observation(COMPLETE);
        let mut writer = EnhancedWriter::create(&path, &budget()).unwrap();

        let error = writer.push(complete, words + 1).unwrap_err();

        assert!(matches!(
            error,
            CorpusError::InvalidFormat {
                component: ENHANCED_NAME,
                message,
            } if message == "enhanced observation word count differs from its token sentence"
        ));
        assert_eq!(writer.sentence_count, 0);
    }

    #[test]
    fn block_corruption_is_detected_only_when_the_block_is_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(ENHANCED_NAME);
        let analysis = AnalysisIdentity::from_bytes([7; 32]);
        let schema = TokenSchemaIdentity::from_bytes([8; 32]);
        let (complete, words) = observation(COMPLETE);
        let mut writer = EnhancedWriter::create(&path, &budget()).unwrap();
        writer.push(complete, words).unwrap();
        writer.finish(analysis, schema).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[HEADER_LEN as usize] ^= 0x01;
        std::fs::write(&path, bytes).unwrap();
        let reader = EnhancedReader::open(&path, &analysis, &schema).unwrap();
        assert_eq!(reader.decoded_block_count(), 0);
        assert!(matches!(
            reader.observation(0),
            Err(CorpusError::VerificationMismatch {
                component: ENHANCED_NAME,
                sentence_ordinal: Some(0),
                ..
            })
        ));
    }

    #[test]
    fn decoder_rejects_noncanonical_sentence_tables_and_unknown_tags() {
        let (complete, words) = observation(COMPLETE);
        let records = [BufferedObservation {
            ordinal: 4,
            word_count: words,
            observation: complete,
        }];
        let raw = encode_block(&records).unwrap();
        let entry = DirectoryEntry {
            first_sentence: 4,
            last_sentence: 4,
            record_count: 1,
            file_offset: 0,
            stored_len: 0,
            raw_len: raw.len() as u64,
            checksum: [0; 32],
        };
        let strings_len = read_u64(&raw, 16).unwrap() as usize;
        let table_start = BLOCK_HEADER_LEN as usize + strings_len;
        let table_len = read_u64(&raw, 24).unwrap() as usize;
        let frames_start = table_start + table_len;

        let mut noncanonical = raw.clone();
        noncanonical[table_start..table_start + 8].copy_from_slice(&5_u64.to_le_bytes());
        assert!(decode_block(&noncanonical, entry).is_err());

        let mut unknown_tag = raw;
        unknown_tag[frames_start] = 3;
        assert!(decode_block(&unknown_tag, entry).is_err());
    }
}
