use crate::{AnalysisIdentity, CorpusError, TokenSchemaIdentity, tokens::TokenReader};
use lingrep_core::{
    fsm::SentenceFilter,
    spill::{SortedRunCounts, SpillPolicy, TempBudget},
    token_match::{
        AttributeKey, AttributeKind, AttributeOrdinal, BoundProjection, BoundTokenSentence,
    },
};
use memmap2::{Mmap, MmapOptions};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Seek, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub(crate) const NAME: &str = "postings.v1";
const MAGIC: &[u8; 8] = b"LGPOST01";
const MAX_DIRECTORY: usize = 64 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const ATTRIBUTES: [&str; 2] = ["lemma", "unidic.pos1"];

#[derive(Clone, Default)]
pub struct PreparedFilterStats(Arc<AtomicU64>);
impl PreparedFilterStats {
    #[must_use]
    pub fn sentences_proven_absent(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
    pub(crate) fn record(&self, count: usize) {
        self.0.fetch_add(count as u64, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct Chunk {
    offset: u64,
    stored: u64,
    raw: u64,
    count: u64,
    first: u64,
    last: u64,
    hash: [u8; 32],
}
struct Entry {
    frequency: u64,
    covered_bytes: u64,
    chunks: Vec<Chunk>,
}
pub(crate) struct PostingIndex {
    map: Mmap,
    entries: Vec<((String, String), Entry)>,
    coverage: Vec<String>,
    total_bytes: u64,
    sentences: u64,
}
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
struct Key {
    attribute: u8,
    value: String,
    sentence: u64,
}

fn invalid(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: NAME,
        message: message.into(),
    }
}
fn put(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn text(out: &mut Vec<u8>, value: &str) {
    put(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}
fn chunk(
    file: &mut File,
    raw: &mut Vec<u8>,
    count: u64,
    first: u64,
    last: u64,
) -> std::io::Result<Chunk> {
    let bytes = zstd::bulk::compress(raw, 3)?;
    let value = Chunk {
        offset: file.stream_position()?,
        stored: bytes.len() as u64,
        raw: raw.len() as u64,
        count,
        first,
        last,
        hash: *blake3::hash(&bytes).as_bytes(),
    };
    file.write_all(&bytes)?;
    raw.clear();
    Ok(value)
}
fn delta(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

pub(crate) struct PostingsBuilder {
    coverage: Vec<String>,
    attributes: Vec<AttributeOrdinal>,
    sorted: SortedRunCounts<Key>,
    sentences: u64,
}

impl PostingsBuilder {
    pub(crate) fn new(
        projection: &BoundProjection,
        budget: Arc<TempBudget>,
    ) -> Result<Option<Self>, CorpusError> {
        let mut coverage = Vec::new();
        let mut attributes = Vec::new();
        for key in ATTRIBUTES {
            if let Some(ordinal) =
                projection.ordinal_for_key(&AttributeKey::new(key).expect("fixed attribute key"))
                && projection.kind(ordinal) == Some(AttributeKind::Scalar)
            {
                coverage.push(key.to_owned());
                attributes.push(ordinal);
            }
        }
        if coverage.is_empty() {
            return Ok(None);
        }
        let sorted = SortedRunCounts::with_policy(
            SpillPolicy {
                max_pending_bytes: 16 * 1024 * 1024,
                max_pending_keys: 65_536,
                merge_fan_in: 16,
            },
            budget,
        )?;
        Ok(Some(Self {
            coverage,
            attributes,
            sorted,
            sentences: 0,
        }))
    }

    pub(crate) fn push(&mut self, sentence: &BoundTokenSentence) -> Result<(), CorpusError> {
        let mut values = Vec::with_capacity(sentence.len());
        for (attribute, ordinal) in self.attributes.iter().enumerate() {
            values.clear();
            values.extend((0..sentence.len()).filter_map(|token| {
                sentence
                    .token(token)
                    .expect("token index is in range")
                    .get(*ordinal)
            }));
            values.sort_unstable();
            values.dedup();
            for &value in &values {
                self.sorted.add(
                    Key {
                        attribute: u8::try_from(attribute).expect("two indexed attributes fit u8"),
                        value: value.into(),
                        sentence: self.sentences,
                    },
                    1,
                )?;
            }
        }
        self.sentences = self
            .sentences
            .checked_add(1)
            .ok_or_else(|| invalid("sentence count overflow"))?;
        Ok(())
    }

    pub(crate) fn finish(
        self,
        root: &Path,
        reader: &TokenReader,
        analysis: &AnalysisIdentity,
        schema: &TokenSchemaIdentity,
        token_hash: &str,
        should_continue: &mut impl FnMut() -> bool,
    ) -> Result<bool, CorpusError> {
        let Self {
            coverage,
            mut sorted,
            sentences,
            ..
        } = self;
        if sentences != reader.sentence_count() {
            return Err(invalid("captured sentence count differs from token file"));
        }
        let projection = reader
            .schema()
            .bind_projection(std::iter::empty::<AttributeKey>())
            .map_err(|error| invalid(error.to_string()))?;
        let mut start = 0;
        let mut total_bytes = 0;
        while start < reader.sentence_count() {
            if !should_continue() {
                return Ok(false);
            }
            let (_, entry) = reader.block_for_sentence(start)?;
            total_bytes += entry.stored_len;
            let ordinals = (start..start + entry.sentence_count).collect::<Vec<_>>();
            reader.decode_bound_ordinals(&ordinals, &projection, false)?;
            start += entry.sentence_count;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(NAME))?;
        file.write_all(MAGIC)?;
        let mut entries = Vec::<((String, String), Entry)>::new();
        let mut raw = Vec::new();
        let mut previous = 0_u64;
        let mut first = 0;
        let mut count = 0;
        let mut block_end = None;
        let mut directory_charge = 256;
        let sorted_result = sorted.visit_sorted(|key, _| {
            if !should_continue() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "postings build cancelled",
                ));
            }
            let attribute = coverage.get(usize::from(key.attribute)).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "posting attribute is outside coverage",
                )
            })?;
            if !entries
                .last()
                .is_some_and(|(group, _)| group.0 == *attribute && group.1 == key.value)
            {
                if count != 0 {
                    entries
                        .last_mut()
                        .expect("current entry")
                        .1
                        .chunks
                        .push(chunk(&mut file, &mut raw, count, first, previous)?);
                }
                let group = (attribute.clone(), key.value.clone());
                directory_charge += group.0.len() + group.1.len() + 128;
                if directory_charge > MAX_DIRECTORY {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        "postings directory exceeds 64 MiB",
                    ));
                }
                entries.push((
                    group,
                    Entry {
                        frequency: 0,
                        covered_bytes: 0,
                        chunks: Vec::new(),
                    },
                ));
                previous = 0;
                count = 0;
                block_end = None;
            }
            let entry = &mut entries.last_mut().expect("current entry").1;
            if raw.len() >= CHUNK {
                entry
                    .chunks
                    .push(chunk(&mut file, &mut raw, count, first, previous)?);
                directory_charge += 80;
                if directory_charge > MAX_DIRECTORY {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        "postings directory exceeds 64 MiB",
                    ));
                }
                previous = 0;
                count = 0;
            }
            if count == 0 {
                first = key.sentence;
            }
            delta(&mut raw, key.sentence - previous);
            previous = key.sentence;
            count += 1;
            entry.frequency += 1;
            if block_end.is_none_or(|end| key.sentence >= end) {
                let (_, block_entry) = reader
                    .block_for_sentence(key.sentence)
                    .map_err(std::io::Error::other)?;
                block_end = Some(block_entry.first_sentence + block_entry.sentence_count);
                entry.covered_bytes += block_entry.stored_len;
            }
            Ok(())
        });
        if let Err(error) = sorted_result {
            if error.kind() == std::io::ErrorKind::FileTooLarge {
                drop(file);
                std::fs::remove_file(root.join(NAME))?;
                return Ok(false);
            }
            return Err(error.into());
        }
        if count != 0 {
            entries
                .last_mut()
                .expect("current entry")
                .1
                .chunks
                .push(chunk(&mut file, &mut raw, count, first, previous)?);
        }
        let mut directory = Vec::new();
        directory.extend_from_slice(analysis.as_bytes());
        directory.extend_from_slice(schema.as_bytes());
        directory.extend_from_slice(
            blake3::Hash::from_hex(token_hash)
                .map_err(|e| invalid(e.to_string()))?
                .as_bytes(),
        );
        put(&mut directory, reader.sentence_count());
        put(&mut directory, total_bytes);
        put(&mut directory, coverage.len() as u64);
        for key in coverage {
            text(&mut directory, &key);
        }
        put(&mut directory, entries.len() as u64);
        for ((attribute, value), entry) in entries {
            text(&mut directory, &attribute);
            text(&mut directory, &value);
            put(&mut directory, entry.frequency);
            put(&mut directory, entry.covered_bytes);
            put(&mut directory, entry.chunks.len() as u64);
            for c in entry.chunks {
                for n in [c.offset, c.stored, c.raw, c.count, c.first, c.last] {
                    put(&mut directory, n);
                }
                directory.extend_from_slice(&c.hash);
            }
            if directory.len() > MAX_DIRECTORY {
                drop(file);
                std::fs::remove_file(root.join(NAME))?;
                return Ok(false);
            }
        }
        file.write_all(&directory)?;
        file.write_all(&(directory.len() as u64).to_le_bytes())?;
        file.write_all(blake3::hash(&directory).as_bytes())?;
        file.write_all(MAGIC)?;
        file.sync_all()?;
        Ok(true)
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], CorpusError> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| invalid("directory range overflow"))?;
        let value = self
            .data
            .get(self.at..end)
            .ok_or_else(|| invalid("truncated directory"))?;
        self.at = end;
        Ok(value)
    }
    fn number(&mut self) -> Result<u64, CorpusError> {
        Ok(u64::from_le_bytes(
            self.bytes(8)?.try_into().map_err(|_| invalid("integer"))?,
        ))
    }
    fn string(&mut self) -> Result<String, CorpusError> {
        let n = self.number()?;
        if n > MAX_DIRECTORY as u64 {
            return Err(invalid("directory string too large"));
        }
        Ok(std::str::from_utf8(self.bytes(n as usize)?)
            .map_err(|_| invalid("directory UTF-8"))?
            .into())
    }
}
impl PostingIndex {
    pub(crate) fn open(
        path: &Path,
        analysis: &AnalysisIdentity,
        schema: &TokenSchemaIdentity,
        token_hash: &str,
        sentences: u64,
    ) -> Result<Self, CorpusError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len < 56 {
            return Err(invalid("truncated index"));
        }
        let map = unsafe { MmapOptions::new().map(&file)? };
        if &map[..8] != MAGIC || &map[map.len() - 8..] != MAGIC {
            return Err(invalid("postings magic"));
        }
        let footer = map.len() - 48;
        let size = u64::from_le_bytes(
            map[footer..footer + 8]
                .try_into()
                .map_err(|_| invalid("footer"))?,
        );
        if size > MAX_DIRECTORY as u64 || size > footer as u64 - 8 {
            return Err(invalid("directory bound"));
        }
        let offset = footer - size as usize;
        let data = &map[offset..footer];
        if blake3::hash(data).as_bytes() != &map[footer + 8..footer + 40] {
            return Err(invalid("directory checksum"));
        }
        let mut c = Cursor { data, at: 0 };
        if c.bytes(32)? != analysis.as_bytes()
            || c.bytes(32)? != schema.as_bytes()
            || c.bytes(32)?
                != blake3::Hash::from_hex(token_hash)
                    .map_err(|e| invalid(e.to_string()))?
                    .as_bytes()
            || c.number()? != sentences
        {
            return Err(invalid("postings identity differs from tokens"));
        }
        let total_bytes = c.number()?;
        let n = c.number()?;
        if n == 0 || n > ATTRIBUTES.len() as u64 {
            return Err(invalid("coverage count"));
        }
        let mut coverage = Vec::new();
        for _ in 0..n {
            let key = c.string()?;
            if !ATTRIBUTES.contains(&key.as_str()) || coverage.contains(&key) {
                return Err(invalid("coverage"));
            }
            coverage.push(key);
        }
        let count = c.number()?;
        if count > MAX_DIRECTORY as u64 / 40 {
            return Err(invalid("entry count"));
        }
        let mut entries = Vec::<((String, String), Entry)>::new();
        let mut next_payload = 8;
        for _ in 0..count {
            let key = (c.string()?, c.string()?);
            if !coverage.contains(&key.0)
                || entries.last().is_some_and(|(previous, _)| previous >= &key)
            {
                return Err(invalid("vocabulary ordering/coverage"));
            }
            let frequency = c.number()?;
            let covered_bytes = c.number()?;
            let n = c.number()?;
            if frequency == 0
                || frequency > sentences
                || covered_bytes == 0
                || covered_bytes > total_bytes
                || n == 0
                || n > frequency
            {
                return Err(invalid("frequency or list count"));
            }
            let mut chunks = Vec::new();
            let mut seen = 0_u64;
            let mut last = None;
            for _ in 0..n {
                let chunk = Chunk {
                    offset: c.number()?,
                    stored: c.number()?,
                    raw: c.number()?,
                    count: c.number()?,
                    first: c.number()?,
                    last: c.number()?,
                    hash: c.bytes(32)?.try_into().map_err(|_| invalid("chunk hash"))?,
                };
                if chunk.offset != next_payload
                    || chunk.stored == 0
                    || chunk.stored > (2 * CHUNK) as u64
                    || chunk.raw > (CHUNK + 10) as u64
                    || chunk.count == 0
                    || chunk.count > chunk.raw
                    || chunk.first > chunk.last
                    || chunk.last >= sentences
                    || last.is_some_and(|last| last >= chunk.first)
                {
                    return Err(invalid("chunk range/count"));
                }
                next_payload = chunk
                    .offset
                    .checked_add(chunk.stored)
                    .ok_or_else(|| invalid("chunk offset overflow"))?;
                if next_payload > offset as u64 {
                    return Err(invalid("chunk outside payload"));
                }
                last = Some(chunk.last);
                seen = seen
                    .checked_add(chunk.count)
                    .ok_or_else(|| invalid("posting count overflow"))?;
                chunks.push(chunk);
            }
            if seen != frequency {
                return Err(invalid("incomplete posting list"));
            }
            entries.push((
                key,
                Entry {
                    frequency,
                    covered_bytes,
                    chunks,
                },
            ));
        }
        if c.at != data.len() || next_payload != offset as u64 {
            return Err(invalid("unaccounted index bytes"));
        }
        Ok(Self {
            map,
            entries,
            coverage,
            total_bytes,
            sentences,
        })
    }
    fn entry(&self, key: &(String, String)) -> Option<&Entry> {
        self.entries
            .binary_search_by(|(candidate, _)| candidate.cmp(key))
            .ok()
            .map(|index| &self.entries[index].1)
    }
    fn decode(&self, chunk: &Chunk) -> Result<Vec<u64>, CorpusError> {
        let bytes = &self.map[chunk.offset as usize..(chunk.offset + chunk.stored) as usize];
        if blake3::hash(bytes).as_bytes() != &chunk.hash {
            return Err(invalid("posting chunk checksum"));
        }
        let raw = zstd::bulk::decompress(bytes, chunk.raw as usize)?;
        if raw.len() as u64 != chunk.raw {
            return Err(invalid("posting raw length"));
        }
        let mut values = Vec::new();
        let mut at = 0;
        let mut previous = 0_u64;
        while at < raw.len() {
            let mut value = 0_u64;
            let mut shift = 0;
            loop {
                let b = *raw.get(at).ok_or_else(|| invalid("truncated delta"))?;
                at += 1;
                if shift > 63 || (shift == 63 && b > 1) {
                    return Err(invalid("delta overflow"));
                }
                value |= u64::from(b & 127) << shift;
                if b & 128 == 0 {
                    break;
                }
                shift += 7;
            }
            let ordinal = previous
                .checked_add(value)
                .ok_or_else(|| invalid("ordinal overflow"))?;
            if ordinal >= self.sentences || (!values.is_empty() && ordinal <= previous) {
                return Err(invalid("posting order"));
            }
            values.push(ordinal);
            previous = ordinal;
        }
        if values.len() as u64 != chunk.count
            || values.first() != Some(&chunk.first)
            || values.last() != Some(&chunk.last)
        {
            return Err(invalid("posting chunk coverage"));
        }
        Ok(values)
    }
    pub(crate) fn candidates(
        self: &Arc<Self>,
        filter: &SentenceFilter,
    ) -> Result<Option<Candidates>, CorpusError> {
        let mut selected = Vec::new();
        let mut total = 0_u64;
        for clause in filter.clauses() {
            let mut best = None;
            for term in clause {
                if !self.coverage.iter().any(|a| a == term.attribute().as_str()) {
                    return Ok(None);
                }
                let key = (
                    term.attribute().as_str().to_owned(),
                    term.value().to_owned(),
                );
                let Some(entry) = self.entry(&key) else {
                    best = Some((key, 0));
                    break;
                };
                if best
                    .as_ref()
                    .is_none_or(|(_, cost)| entry.covered_bytes < *cost)
                {
                    best = Some((key, entry.covered_bytes));
                }
            }
            let Some((key, cost)) = best else {
                return Ok(None);
            };
            total = total.saturating_add(cost);
            selected.push(key);
        }
        if self.total_bytes != 0 && total >= self.total_bytes.div_ceil(2) {
            return Ok(None);
        }
        selected.sort();
        selected.dedup();
        let lists = selected
            .into_iter()
            .filter_map(|key| self.entry(&key))
            .map(|e| ListCursor {
                chunks: e.chunks.clone(),
                chunk: 0,
                values: Vec::new(),
                at: 0,
            })
            .collect();
        Ok(Some(Candidates {
            index: Arc::clone(self),
            lists,
        }))
    }
    pub(crate) fn verify(&self) -> Result<(), CorpusError> {
        for (_, entry) in &self.entries {
            for chunk in &entry.chunks {
                self.decode(chunk)?;
            }
        }
        Ok(())
    }
}
struct ListCursor {
    chunks: Vec<Chunk>,
    chunk: usize,
    values: Vec<u64>,
    at: usize,
}
pub(crate) struct Candidates {
    index: Arc<PostingIndex>,
    lists: Vec<ListCursor>,
}
impl Candidates {
    pub(crate) fn contains(&mut self, ordinal: u64) -> Result<bool, CorpusError> {
        let mut found = false;
        for list in &mut self.lists {
            loop {
                while list.at < list.values.len() && list.values[list.at] < ordinal {
                    list.at += 1;
                }
                if list.at < list.values.len() {
                    found |= list.values[list.at] == ordinal;
                    break;
                }
                if list.chunk == list.chunks.len() {
                    break;
                }
                list.values = self.index.decode(&list.chunks[list.chunk])?;
                list.chunk += 1;
                list.at = 0;
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::TokenWriter;
    use lingrep_core::{
        fsm::{BoundCatalog, FsmBuilder, PatternSource},
        token_match::{BoundTokenSentence, TokenSchema},
    };

    fn query(schema: &TokenSchema, body: &str) -> BoundCatalog {
        let mut builder = FsmBuilder::new();
        builder
            .add_document(PatternSource::named("query.lx", body))
            .unwrap();
        builder.build().unwrap().matcher.bind(schema).unwrap()
    }

    #[test]
    fn postings_bytes_cover_empty_sentences_spilled_runs_and_chunk_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tokens");
        let schema = TokenSchema::from_keys(["lemma", "text", "unidic.pos1", "upos"]).unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let analysis = AnalysisIdentity::from_bytes([9; 32]);
        let schema_id = TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes());
        let budget = Arc::new(TempBudget::new(16 * 1024 * 1024));
        let mut writer = TokenWriter::create_indexed(&path, schema, Arc::clone(&budget)).unwrap();
        for ordinal in 0..70_000 {
            let tokens = if ordinal == 0 { 0 } else { 2 };
            let lemma = match ordinal {
                1 => None,
                100 => Some("長".repeat(50).into()),
                257 => Some("rare".into()),
                _ => Some("common".into()),
            };
            let values = (0..tokens)
                .flat_map(|_| {
                    projection.keys().iter().map(|key| match key.as_str() {
                        "lemma" => lemma.clone(),
                        "unidic.pos1" => Some("名詞".into()),
                        "text" => Some("日本語".into()),
                        _ => None,
                    })
                })
                .collect();
            writer
                .push_bound_sentence(
                    BoundTokenSentence::from_projected_values(&projection, tokens, values).unwrap(),
                )
                .unwrap();
        }
        let postings = writer.finish(analysis).unwrap().unwrap();
        let reader = TokenReader::open(&path, &analysis, &schema_id).unwrap();
        let hash = blake3::hash(&std::fs::read(&path).unwrap())
            .to_hex()
            .to_string();
        assert!(
            postings
                .finish(
                    root.path(),
                    &reader,
                    &analysis,
                    &schema_id,
                    &hash,
                    &mut || true
                )
                .unwrap()
        );
        assert!(budget.peak_used_bytes() > 0);
        assert_eq!(budget.used_bytes(), 0);
        let index = PostingIndex::open(
            &root.path().join(NAME),
            &analysis,
            &schema_id,
            &hash,
            70_000,
        )
        .unwrap();
        index.verify().unwrap();
        let common = index.entry(&("lemma".into(), "common".into())).unwrap();
        assert_eq!(common.frequency, 69_996);
        assert_eq!(common.chunks.len(), 2);
        assert_eq!(common.covered_bytes, index.total_bytes);
        assert_eq!(
            blake3::hash(&std::fs::read(root.path().join(NAME)).unwrap())
                .to_hex()
                .as_str(),
            "ca455404f27cc0af840a76531dbe524345deca42a88e48eee7003987a259f7a3"
        );
    }

    #[test]
    fn indexed_attributes_keep_their_names_across_projection_subsets() {
        for keys in [
            vec!["unidic.pos1"],
            vec!["lemma"],
            vec!["unidic.pos1", "lemma"],
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("tokens");
            let schema = TokenSchema::from_keys(keys.iter().copied().chain(["text"])).unwrap();
            let projection = schema
                .bind_projection(schema.keys().iter().cloned())
                .unwrap();
            let analysis = AnalysisIdentity::from_bytes([9; 32]);
            let schema_id = TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes());
            let budget = Arc::new(TempBudget::new(16 * 1024 * 1024));
            let mut writer =
                TokenWriter::create_indexed(&path, schema.clone(), Arc::clone(&budget)).unwrap();
            let values = projection
                .keys()
                .iter()
                .map(|key| Some(key.as_str().into()))
                .collect();
            writer
                .push_bound_sentence(
                    BoundTokenSentence::from_projected_values(&projection, 1, values).unwrap(),
                )
                .unwrap();
            let postings = writer.finish(analysis).unwrap().unwrap();
            let reader = TokenReader::open(&path, &analysis, &schema_id).unwrap();
            let hash = blake3::hash(&std::fs::read(&path).unwrap())
                .to_hex()
                .to_string();
            assert!(
                postings
                    .finish(
                        root.path(),
                        &reader,
                        &analysis,
                        &schema_id,
                        &hash,
                        &mut || true
                    )
                    .unwrap()
            );
            let index =
                PostingIndex::open(&root.path().join(NAME), &analysis, &schema_id, &hash, 1)
                    .unwrap();
            index.verify().unwrap();
            assert_eq!(index.entries.len(), keys.len());
            for key in keys {
                assert_eq!(index.entry(&(key.into(), key.into())).unwrap().frequency, 1);
            }
        }
    }

    #[test]
    fn lists_preserve_rare_or_candidates_and_common_terms_fall_back_before_decode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tokens");
        let schema = TokenSchema::from_keys(["lemma"]).unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let analysis = AnalysisIdentity::from_bytes([4; 32]);
        let schema_id = TokenSchemaIdentity::from_bytes(*schema.digest().as_bytes());
        let budget = Arc::new(TempBudget::new(16 * 1024 * 1024));
        let mut writer =
            TokenWriter::create_indexed(&path, schema.clone(), Arc::clone(&budget)).unwrap();
        for ordinal in 0..2048 {
            let value = match ordinal {
                17 => "rare",
                1800 => "other",
                _ => "common",
            };
            writer
                .push_bound_sentence(
                    BoundTokenSentence::from_projected_values(
                        &projection,
                        if ordinal == 17 { 3 } else { 1 },
                        vec![Some(value.into()); if ordinal == 17 { 3 } else { 1 }],
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let postings = writer.finish(analysis).unwrap().unwrap();
        let reader = TokenReader::open(&path, &analysis, &schema_id).unwrap();
        let hash = blake3::hash(&std::fs::read(&path).unwrap())
            .to_hex()
            .to_string();
        assert!(
            postings
                .finish(
                    root.path(),
                    &reader,
                    &analysis,
                    &schema_id,
                    &hash,
                    &mut || true
                )
                .unwrap()
        );
        let index = Arc::new(
            PostingIndex::open(&root.path().join(NAME), &analysis, &schema_id, &hash, 2048)
                .unwrap(),
        );
        index.verify().unwrap();
        let rare_key = ("lemma".to_owned(), "rare".to_owned());
        assert_eq!(index.entry(&rare_key).unwrap().frequency, 1);
        let mut damaged = std::fs::read(root.path().join(NAME)).unwrap();
        damaged[index.entry(&rare_key).unwrap().chunks[0].offset as usize] ^= 1;
        std::fs::write(root.path().join("damaged-selected"), damaged).unwrap();
        let selected_index = Arc::new(
            PostingIndex::open(
                &root.path().join("damaged-selected"),
                &analysis,
                &schema_id,
                &hash,
                2048,
            )
            .unwrap(),
        );
        let rare = query(&schema, "p := [lemma=rare].");
        let mut selected = selected_index
            .candidates(rare.sentence_filter().unwrap())
            .unwrap()
            .unwrap();
        assert!(selected.contains(0).is_err());
        let bound = query(&schema, "p := [lemma=rare] | [lemma=other].");
        let mut candidates = index
            .candidates(bound.sentence_filter().unwrap())
            .unwrap()
            .unwrap();
        let selected = (0..2048)
            .filter(|ordinal| candidates.contains(*ordinal).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(selected, [17, 1800]);
        let bound = query(&schema, "p := [lemma=absent].");
        let mut candidates = index
            .candidates(bound.sentence_filter().unwrap())
            .unwrap()
            .unwrap();
        assert!(!(0..2048).any(|ordinal| candidates.contains(ordinal).unwrap()));
        drop(candidates);
        let mut damaged = std::fs::read(root.path().join(NAME)).unwrap();
        damaged[8] ^= 1;
        std::fs::write(root.path().join("damaged"), damaged).unwrap();
        let index = Arc::new(
            PostingIndex::open(
                &root.path().join("damaged"),
                &analysis,
                &schema_id,
                &hash,
                2048,
            )
            .unwrap(),
        );
        let bound = query(&schema, "p := [lemma=common].");
        assert!(
            index
                .candidates(bound.sentence_filter().unwrap())
                .unwrap()
                .is_none()
        );
        assert!(index.verify().is_err());
        let mut damaged = std::fs::read(root.path().join(NAME)).unwrap();
        let at = damaged.len() - 49;
        damaged[at] ^= 1;
        std::fs::write(root.path().join("bad-directory"), damaged).unwrap();
        assert!(
            PostingIndex::open(
                &root.path().join("bad-directory"),
                &analysis,
                &schema_id,
                &hash,
                2048
            )
            .is_err()
        );
        assert!(
            PostingIndex::open(
                &root.path().join(NAME),
                &AnalysisIdentity::from_bytes([5; 32]),
                &schema_id,
                &hash,
                2048
            )
            .is_err()
        );
    }
    #[test]
    fn open_refuses_an_oversized_compressed_chunk_before_query_selection() {
        let root = tempfile::tempdir().unwrap();
        let analysis = AnalysisIdentity::from_bytes([7; 32]);
        let schema = TokenSchemaIdentity::from_bytes([8; 32]);
        let digest = blake3::hash(b"tokens");
        let payload = vec![0; 2 * CHUNK + 1];
        let mut directory = Vec::new();
        directory.extend_from_slice(analysis.as_bytes());
        directory.extend_from_slice(schema.as_bytes());
        directory.extend_from_slice(digest.as_bytes());
        for value in [1, 1, 1] {
            put(&mut directory, value);
        }
        text(&mut directory, "lemma");
        put(&mut directory, 1);
        text(&mut directory, "lemma");
        text(&mut directory, "value");
        for value in [1, 1, 1, 8, payload.len() as u64, 1, 1, 0, 0] {
            put(&mut directory, value);
        }
        directory.extend_from_slice(blake3::hash(&payload).as_bytes());
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&directory);
        bytes.extend_from_slice(&(directory.len() as u64).to_le_bytes());
        bytes.extend_from_slice(blake3::hash(&directory).as_bytes());
        bytes.extend_from_slice(MAGIC);
        let path = root.path().join(NAME);
        std::fs::write(&path, bytes).unwrap();
        assert!(
            matches!(PostingIndex::open(&path, &analysis, &schema, digest.to_hex().as_ref(), 1), Err(CorpusError::InvalidFormat { message, .. }) if message == "chunk range/count")
        );
    }
}
