//! Reads word2vec text files, with optional count/dimension headers and blank lines.
//!
//! Each row contains a value, one space, and decimal vector components.
//! Zero vectors never match a similarity query. For duplicate values, queries use
//! the first row; candidate matching accepts any row for that value.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use compact_str::CompactString;
use lingrep_core::fsm::{CatalogVectors, NearQuery, WordVectors};
use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// A cosine similarity in millionths, from -1,000,000 to 1,000,000.
///
/// Similarities are rounded to this precision before comparison.
pub type Millionths = i32;

const ONE: f64 = 1_000_000.0;
const MAX_CACHED_QUERIES: usize = 64;
const MAX_CACHED_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

struct CachedAnswer {
    query: NearQuery,
    values: Option<Arc<[CompactString]>>,
    payload_bytes: usize,
}

struct AnswerCache {
    entries: VecDeque<CachedAnswer>,
    payload_bytes: usize,
}

impl AnswerCache {
    fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(MAX_CACHED_QUERIES),
            payload_bytes: 0,
        }
    }

    fn get(&self, query: &NearQuery) -> Option<&Option<Arc<[CompactString]>>> {
        self.entries
            .iter()
            .find(|entry| &entry.query == query)
            .map(|entry| &entry.values)
    }

    fn insert(&mut self, query: NearQuery, values: Option<Arc<[CompactString]>>) {
        if self.get(&query).is_some() {
            return;
        }
        let heap_bytes = |value: &CompactString| {
            if value.is_heap_allocated() {
                value.capacity()
            } else {
                0
            }
        };
        let mut payload_bytes = heap_bytes(&query.word);
        if let Some(values) = &values {
            payload_bytes = payload_bytes
                .saturating_add(2 * std::mem::size_of::<usize>())
                .saturating_add(std::mem::size_of_val(values.as_ref()));
            for value in values.iter() {
                payload_bytes = payload_bytes.saturating_add(heap_bytes(value));
            }
        }
        if payload_bytes > MAX_CACHED_PAYLOAD_BYTES {
            return;
        }
        while self.entries.len() == MAX_CACHED_QUERIES
            || self.payload_bytes > MAX_CACHED_PAYLOAD_BYTES - payload_bytes
        {
            if let Some(entry) = self.entries.pop_front() {
                self.payload_bytes -= entry.payload_bytes;
            }
        }
        self.payload_bytes += payload_bytes;
        self.entries.push_back(CachedAnswer {
            query,
            values,
            payload_bytes,
        });
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VectorProvenance {
    pub resource: String,
    pub attribute: String,
    pub vocabulary_sources: Vec<String>,
    pub selection: String,
    pub encoding: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    schema: String,
    blake3: String,
    provenance: VectorProvenance,
}

impl BundleManifest {
    fn read(directory: &Path) -> Result<Self, VectorTableError> {
        let path = directory.join("manifest.json");
        let file = File::open(&path).map_err(|source| VectorTableError::io(&path, source))?;
        let invalid = |detail: String| VectorTableError::InvalidBundle {
            path: path.clone(),
            detail,
        };
        let manifest: Self = serde_json::from_reader(io::BufReader::new(file))
            .map_err(|error| invalid(error.to_string()))?;
        if manifest.schema != "lingrep.word-vectors/v1" {
            return Err(invalid(format!("unsupported schema {:?}", manifest.schema)));
        }
        if manifest.blake3.len() != 64
            || !manifest
                .blake3
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(invalid(
                "blake3 must be 64 lowercase hexadecimal digits".into(),
            ));
        }
        let provenance = &manifest.provenance;
        if !provenance
            .resource
            .starts_with(|c: char| c.is_ascii_lowercase())
            || !provenance
                .resource
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        {
            return Err(invalid("resource must match [a-z][a-z0-9_-]*".into()));
        }
        lingrep_core::token_match::AttributeKey::new(&provenance.attribute)
            .map_err(|error| invalid(format!("attribute: {error}")))?;
        if provenance.vocabulary_sources.is_empty()
            || provenance
                .vocabulary_sources
                .iter()
                .any(|source| source.trim().is_empty())
            || provenance.selection.trim().is_empty()
            || provenance.encoding.trim().is_empty()
        {
            return Err(invalid(
                "provenance requires vocabulary sources, selection and encoding".into(),
            ));
        }
        Ok(manifest)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VectorTableError {
    #[error("invalid vector query: {0}")]
    InvalidQuery(String),
    #[error("word-vector file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("cannot read word-vector file {}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid word-vector bundle {}: {detail}", .path.display())]
    InvalidBundle { path: PathBuf, detail: String },
    #[error("word-vector file {}: line {line}: {detail}", .path.display())]
    Malformed {
        path: PathBuf,
        line: u64,
        detail: String,
    },
}

impl VectorTableError {
    fn io(path: &Path, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::NotFound {
            Self::NotFound(path.to_owned())
        } else {
            Self::Io {
                path: path.to_owned(),
                source,
            }
        }
    }
}

pub struct Scan {
    /// Per query, the values that passed, each with its similarity, in file
    /// order and without repeats.
    pub hits: Vec<Vec<(CompactString, Millionths)>>,
    /// Number of rows, including duplicate values and zero vectors.
    pub values: u64,
    /// Number of rows whose components are all zero.
    pub values_without_vector: u64,
}

/// A memory-mapped word-vector file. The file must not be modified while the
/// table is open.
pub struct VectorTable {
    path: PathBuf,
    map: Mmap,
    provenance: Option<VectorProvenance>,
    digest: OnceLock<blake3::Hash>,
    answered: Mutex<AnswerCache>,
}

struct Malformed {
    offset: usize,
    detail: String,
}

struct Chunk {
    hits: Vec<Vec<(CompactString, Millionths)>>,
    values: u64,
    values_without_vector: u64,
}

impl VectorTable {
    /// Opens a word2vec text file or a bundle directory containing `manifest.json`
    /// and `vectors.vec`. A bundle's table digest is verified before returning.
    ///
    /// # Errors
    ///
    /// An unreadable file, invalid manifest, or digest mismatch.
    pub fn open(path: &Path) -> Result<Self, VectorTableError> {
        let manifest = path
            .is_dir()
            .then(|| BundleManifest::read(path))
            .transpose()?;
        let file_path = if manifest.is_some() {
            path.join("vectors.vec")
        } else {
            path.to_owned()
        };
        let file =
            File::open(&file_path).map_err(|source| VectorTableError::io(&file_path, source))?;
        // SAFETY: callers must keep the mapped file unchanged, as required by VectorTable.
        let map = unsafe { Mmap::map(&file) }
            .map_err(|source| VectorTableError::io(&file_path, source))?;
        let mut table = Self {
            path: path.to_owned(),
            map,
            provenance: None,
            digest: OnceLock::new(),
            answered: Mutex::new(AnswerCache::new()),
        };
        if let Some(manifest) = manifest {
            let actual = table.digest().to_hex();
            if actual.as_str() != manifest.blake3 {
                return Err(VectorTableError::InvalidBundle {
                    path: file_path,
                    detail: format!("expected BLAKE3 {}, found {actual}", manifest.blake3),
                });
            }
            table.provenance = Some(manifest.provenance);
        }
        Ok(table)
    }

    #[must_use]
    pub fn provenance(&self) -> Option<&VectorProvenance> {
        self.provenance.as_ref()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The BLAKE3 digest of the file's bytes.
    #[must_use]
    pub fn digest(&self) -> blake3::Hash {
        *self.digest.get_or_init(|| {
            let mut digest = blake3::Hasher::new();
            digest.update_rayon(&self.map);
            digest.finalize()
        })
    }

    /// The unit vector of each word, or `None` for a word that has no row or
    /// whose first row is all zeros.
    ///
    /// # Errors
    ///
    /// [`VectorTableError::Malformed`] when a word's row is malformed or
    /// its components or squared norm exceed the supported floating-point range.
    pub fn unit_vectors(&self, words: &[&str]) -> Result<Vec<Option<Vec<f64>>>, VectorTableError> {
        let found = self
            .chunks()
            .into_par_iter()
            .map(|(start, bytes)| {
                let mut found: Vec<Option<Vec<f64>>> = vec![None; words.len()];
                for (offset, line) in lines(start, bytes) {
                    let (value, numbers) = split(line);
                    for (slot, word) in found.iter_mut().zip(words) {
                        if slot.is_none() && word.as_bytes() == value {
                            let mut vector = Vec::new();
                            parse_numbers(numbers, offset, |x| vector.push(x))?;
                            if !vector.iter().map(|x| x * x).sum::<f64>().is_finite() {
                                return Err(Malformed {
                                    offset,
                                    detail: "vector norm exceeds the supported numeric range"
                                        .into(),
                                });
                            }
                            *slot = Some(vector);
                        }
                    }
                }
                Ok(found)
            })
            .collect::<Result<Vec<_>, Malformed>>()
            .map_err(|error| self.malformed(error))?;
        Ok((0..words.len())
            .map(|index| {
                let mut vector = found.iter().find_map(|chunk| chunk[index].clone())?;
                let norm = vector.iter().map(|x| x * x).sum::<f64>().sqrt();
                if norm == 0.0 {
                    return None;
                }
                for x in &mut vector {
                    *x /= norm;
                }
                Some(vector)
            })
            .collect())
    }

    /// Compares every row with each unit vector in `queries`, keeping for
    /// query `q` the values whose similarity `s` satisfies `keep(q, s)`.
    ///
    /// # Errors
    ///
    /// [`VectorTableError::Malformed`] when a row does not parse or its
    /// vector is not as long as the queries; [`VectorTableError::InvalidQuery`]
    /// for empty, nonfinite, or unequal-length query vectors.
    pub fn scan(
        &self,
        queries: &[&[f64]],
        keep: impl Fn(usize, Millionths) -> bool + Sync,
    ) -> Result<Scan, VectorTableError> {
        let Some(width) = queries.first().map(|query| query.len()) else {
            return Ok(Scan {
                hits: Vec::new(),
                values: 0,
                values_without_vector: 0,
            });
        };
        if width == 0
            || queries
                .iter()
                .any(|query| query.len() != width || query.iter().any(|x| !x.is_finite()))
        {
            return Err(VectorTableError::InvalidQuery(
                "queries must be nonempty finite vectors of equal dimensions".into(),
            ));
        }
        let chunks = self
            .chunks()
            .into_par_iter()
            .map(|(start, bytes)| {
                let mut chunk = Chunk {
                    hits: vec![Vec::new(); queries.len()],
                    values: 0,
                    values_without_vector: 0,
                };
                let mut dots = vec![0.0f64; queries.len()];
                for (offset, line) in lines(start, bytes) {
                    let (value, numbers) = split(line);
                    chunk.values += 1;
                    dots.fill(0.0);
                    let (mut squares, mut count) = (0.0f64, 0usize);
                    parse_numbers(numbers, offset, |x| {
                        if count < width {
                            for (dot, query) in dots.iter_mut().zip(queries) {
                                *dot += x * query[count];
                            }
                        }
                        squares += x * x;
                        count += 1;
                    })?;
                    if count != width {
                        return Err(Malformed {
                            offset,
                            detail: format!("{count} numbers where {width} were expected"),
                        });
                    }
                    if !squares.is_finite() {
                        return Err(Malformed {
                            offset,
                            detail: "vector norm exceeds the supported numeric range".into(),
                        });
                    }
                    if squares == 0.0 {
                        chunk.values_without_vector += 1;
                        continue;
                    }
                    let norm = squares.sqrt();
                    let mut name = None;
                    for (index, dot) in dots.iter().enumerate() {
                        #[allow(clippy::cast_possible_truncation)]
                        let similarity = ((dot / norm).clamp(-1.0, 1.0) * ONE).round() as i32;
                        if keep(index, similarity) {
                            let name = match &name {
                                Some(name) => name,
                                None => name.insert(text(value, offset)?),
                            };
                            chunk.hits[index].push((CompactString::from(*name), similarity));
                        }
                    }
                }
                Ok(chunk)
            })
            .collect::<Result<Vec<_>, Malformed>>()
            .map_err(|error| self.malformed(error))?;

        let mut scan = Scan {
            hits: vec![Vec::new(); queries.len()],
            values: 0,
            values_without_vector: 0,
        };
        let mut seen: Vec<HashSet<CompactString>> = vec![HashSet::new(); queries.len()];
        for chunk in chunks {
            scan.values += chunk.values;
            scan.values_without_vector += chunk.values_without_vector;
            for ((hits, seen), found) in scan.hits.iter_mut().zip(&mut seen).zip(chunk.hits) {
                for hit in found {
                    if seen.insert(hit.0.clone()) {
                        hits.push(hit);
                    }
                }
            }
        }
        Ok(scan)
    }

    fn chunks(&self) -> Vec<(usize, &[u8])> {
        const TARGET: usize = 1 << 22;
        let bytes: &[u8] = &self.map;
        let mut start = match bytes.iter().position(|byte| *byte == b'\n') {
            Some(end) if is_header(&bytes[..end]) => end + 1,
            None if is_header(bytes) => bytes.len(),
            _ => 0,
        };
        let mut chunks = Vec::new();
        while start < bytes.len() {
            let target = (start + TARGET).min(bytes.len());
            let end = bytes[target..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |newline| target + newline + 1);
            chunks.push((start, &bytes[start..end]));
            start = end;
        }
        chunks
    }

    fn malformed(&self, error: Malformed) -> VectorTableError {
        let line = self.map[..error.offset]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u64
            + 1;
        VectorTableError::Malformed {
            path: if self.provenance.is_some() {
                self.path.join("vectors.vec")
            } else {
                self.path.clone()
            },
            line,
            detail: error.detail,
        }
    }
}

impl WordVectors for VectorTable {
    fn similar_values(
        &self,
        queries: &[NearQuery],
    ) -> Result<Vec<Option<Arc<[CompactString]>>>, String> {
        let mut answered: HashMap<&NearQuery, Option<Arc<[CompactString]>>> = {
            let cache = self.answered.lock().unwrap_or_else(PoisonError::into_inner);
            queries
                .iter()
                .filter_map(|query| cache.get(query).map(|answer| (query, answer.clone())))
                .collect()
        };
        let unanswered: Vec<&NearQuery> = queries
            .iter()
            .filter(|query| !answered.contains_key(*query))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if !unanswered.is_empty() {
            let words: Vec<&str> = unanswered.iter().map(|query| query.word.as_str()).collect();
            let vectors = self
                .unit_vectors(&words)
                .map_err(|error| error.to_string())?;
            let present: Vec<(&NearQuery, &[f64])> = unanswered
                .iter()
                .zip(&vectors)
                .filter_map(|(query, vector)| Some((*query, vector.as_deref()?)))
                .collect();
            let unit_vectors: Vec<&[f64]> = present.iter().map(|(_, vector)| *vector).collect();
            let scan = self
                .scan(&unit_vectors, |index, similarity| {
                    i64::from(similarity) >= i64::from(present[index].0.threshold.millionths())
                })
                .map_err(|error| error.to_string())?;
            for ((query, _), hits) in present.iter().zip(scan.hits) {
                let values: Vec<CompactString> = hits.into_iter().map(|(value, _)| value).collect();
                answered.insert(*query, Some(Arc::from(values)));
            }
            let mut cache = self.answered.lock().unwrap_or_else(PoisonError::into_inner);
            for query in unanswered {
                let answer = answered.entry(query).or_insert(None);
                cache.insert(query.clone(), answer.clone());
            }
        }
        Ok(queries
            .iter()
            .map(|query| answered.get(query).cloned().flatten())
            .collect())
    }
}

fn is_header(line: &[u8]) -> bool {
    let Ok(line) = std::str::from_utf8(line) else {
        return false;
    };
    let mut fields = line.split_ascii_whitespace();
    let integers = fields
        .by_ref()
        .take(2)
        .filter(|field| field.parse::<u64>().is_ok())
        .count();
    integers == 2 && fields.next().is_none()
}

fn lines(start: usize, bytes: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut offset = start;
    bytes.split(|byte| *byte == b'\n').filter_map(move |line| {
        let at = offset;
        offset += line.len() + 1;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        (!line.iter().all(u8::is_ascii_whitespace)).then_some((at, line))
    })
}

fn split(line: &[u8]) -> (&[u8], &[u8]) {
    match line.iter().position(|byte| *byte == b' ') {
        Some(space) => (&line[..space], &line[space + 1..]),
        None => (line, &[]),
    }
}

fn text(bytes: &[u8], offset: usize) -> Result<&str, Malformed> {
    std::str::from_utf8(bytes).map_err(|error| Malformed {
        offset,
        detail: error.to_string(),
    })
}

fn parse_numbers(
    numbers: &[u8],
    offset: usize,
    mut each: impl FnMut(f64),
) -> Result<(), Malformed> {
    for field in text(numbers, offset)?.split_ascii_whitespace() {
        let value = field.parse::<f64>().map_err(|error| Malformed {
            offset,
            detail: error.to_string(),
        })?;
        if !value.is_finite() {
            return Err(Malformed {
                offset,
                detail: "vector components must be finite".into(),
            });
        }
        if value != 0.0 && (value * value == 0.0 || !(value * value).is_finite()) {
            return Err(Malformed {
                offset,
                detail: "vector component exceeds the supported numeric range".into(),
            });
        }
        each(value);
    }
    Ok(())
}

/// Whether a catalog declares the digest of the loaded word-vector file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VectorsBinding {
    /// An entry carries the file's digest.
    Declared,
    /// The catalog has no entry; `declaration` is the line that declares the
    /// file.
    Undeclared { declaration: String },
    /// Every entry carries another digest; `declared` holds each entry's
    /// resource label and digest.
    Mismatch { declared: Vec<(String, String)> },
}

/// Compares `declared` with the file at `path` whose BLAKE3 digest is
/// `blake3` in lowercase hex.
#[must_use]
pub fn vectors_binding(
    declared: &[CatalogVectors],
    path: &Path,
    blake3: &str,
    provenance: Option<&VectorProvenance>,
) -> VectorsBinding {
    if declared.iter().any(|vectors| vectors.blake3 == blake3) {
        return VectorsBinding::Declared;
    }
    if declared.is_empty() {
        let label: String = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("vectors")
            .to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let label = label.trim_start_matches(|c: char| !c.is_ascii_lowercase());
        return VectorsBinding::Undeclared {
            declaration: format!(
                "vectors: {{resource: \"{}\", blake3: \"{blake3}\"}}",
                provenance.map_or_else(
                    || if label.is_empty() { "vectors" } else { label },
                    |meta| meta.resource.as_str()
                ),
            ),
        };
    }
    VectorsBinding::Mismatch {
        declared: declared
            .iter()
            .map(|vectors| (vectors.resource.to_string(), vectors.blake3.to_string()))
            .collect(),
    }
}

/// `value` spelled as an unquoted pattern literal: every character that is
/// not a letter, a digit or `_` becomes a `\uXXXX` escape. `None` for an
/// empty value and when such a character lies outside the range the
/// four-digit escape reaches.
#[must_use]
pub fn pattern_literal(value: &str) -> Option<String> {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if u32::from(ch) <= 0xFFFF {
            let _ = write!(out, "\\u{:04X}", u32::from(ch));
        } else {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn table(rows: &str) -> (tempfile::TempDir, VectorTable) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vectors.vec");
        std::fs::write(&path, rows).unwrap();
        let table = VectorTable::open(&path).unwrap();
        (directory, table)
    }

    #[test]
    fn distinct_queries_do_not_accumulate_unbounded_cached_answers() {
        let (_directory, table) = table("walk 1 0\nstroll 0.8 0.6\n");
        for index in 0..65 {
            let query = NearQuery {
                word: format!("missing_{index}").into(),
                threshold: lingrep_core::fsm::SimilarityThreshold::parse("0.6").unwrap(),
            };
            assert_eq!(table.similar_values(&[query]).unwrap(), vec![None]);
        }
        assert!(table.answered.lock().unwrap().entries.len() <= 64);
        let queries: Vec<_> = (0..65)
            .map(|index| NearQuery {
                word: "walk".into(),
                threshold: lingrep_core::fsm::SimilarityThreshold::parse(&format!("0.{index:06}"))
                    .unwrap(),
            })
            .collect();
        for answer in table.similar_values(&queries).unwrap() {
            assert_eq!(
                answer.as_deref().unwrap(),
                &[CompactString::from("walk"), CompactString::from("stroll")]
            );
        }
        assert!(table.answered.lock().unwrap().entries.len() <= 64);
    }

    #[test]
    fn eviction_preserves_query_order_duplicates_and_complete_answers() {
        let (_directory, table) = table("walk 1 0\nstroll 0.8 0.6\nwalk 0 1\nback -1 0\n");
        let query = |word: &str, threshold: &str| NearQuery {
            word: word.into(),
            threshold: lingrep_core::fsm::SimilarityThreshold::parse(threshold).unwrap(),
        };
        let queries = [
            query("walk", "0.6"),
            query("missing", "0"),
            query("walk", "0.6"),
        ];
        let before = table.similar_values(&queries).unwrap();
        assert_eq!(
            before[0].as_deref().unwrap(),
            &[CompactString::from("walk"), CompactString::from("stroll")]
        );
        assert_eq!(before[1], None);
        assert_eq!(before[0], before[2]);
        let first = Arc::downgrade(before[0].as_ref().unwrap());
        for index in 0..MAX_CACHED_QUERIES {
            table
                .similar_values(&[query(&format!("absent_{index}"), "0")])
                .unwrap();
        }
        assert_eq!(Arc::strong_count(before[0].as_ref().unwrap()), 2);
        assert_eq!(table.similar_values(&queries).unwrap(), before);
        drop(before);
        assert!(first.upgrade().is_none());
    }

    #[test]
    fn cached_payload_accounts_for_query_and_value_allocations_and_evicts() {
        let mut cache = AnswerCache::new();
        let query = |word: CompactString| NearQuery {
            word,
            threshold: lingrep_core::fsm::SimilarityThreshold::parse("0").unwrap(),
        };
        let mut value = CompactString::with_capacity(MAX_CACHED_PAYLOAD_BYTES / 2);
        value.push('v');
        let values: Arc<[CompactString]> = Arc::from([value]);
        let first = Arc::downgrade(&values);
        cache.insert(query("first".into()), Some(values));
        let mut word = CompactString::with_capacity(MAX_CACHED_PAYLOAD_BYTES / 2);
        word.push('q');
        cache.insert(query(word), None);
        assert!(first.upgrade().is_none());
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.payload_bytes <= MAX_CACHED_PAYLOAD_BYTES);
        cache.insert(
            query(CompactString::from(
                "x".repeat(MAX_CACHED_PAYLOAD_BYTES + 1),
            )),
            None,
        );
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.entries.capacity() <= MAX_CACHED_QUERIES);
    }

    #[test]
    fn oversized_answers_are_returned_completely_without_cache_retention() {
        let large_word = "v".repeat(MAX_CACHED_PAYLOAD_BYTES + 1);
        let (_directory, table) = table(&format!("walk 1 0\n{large_word} 1 0\n"));
        let query = NearQuery {
            word: "walk".into(),
            threshold: lingrep_core::fsm::SimilarityThreshold::parse("1").unwrap(),
        };
        let answers = table.similar_values(&[query]).unwrap();
        let values = answers[0].as_ref().unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], "walk");
        assert_eq!(values[1], large_word);
        let weak = Arc::downgrade(values);
        drop(answers);
        assert!(weak.upgrade().is_none());
        assert!(table.answered.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn unsupported_components_are_rejected_in_queries_and_candidate_rows() {
        for component in ["NaN", "inf", "-inf", "1e308", "1e-300"] {
            let (_directory, table) = table(&format!("walk 1 0\nbad {component} 1\n"));
            assert!(matches!(
                table.unit_vectors(&["bad"]),
                Err(VectorTableError::Malformed { line: 2, .. })
            ));
            assert!(matches!(
                table.scan(&[&[1.0, 0.0]], |_, _| true),
                Err(VectorTableError::Malformed { line: 2, .. })
            ));
        }
    }

    #[test]
    fn invalid_query_vectors_return_an_error() {
        let (_directory, table) = table("walk 1 0\n");
        for queries in [
            vec![&[1.0, 0.0][..], &[1.0][..]],
            vec![&[][..]],
            vec![&[f64::NAN, 0.0][..]],
        ] {
            assert!(matches!(
                table.scan(&queries, |_, _| true),
                Err(VectorTableError::InvalidQuery(_))
            ));
        }
    }
}
