//! A word2vec text file as a table of word vectors.
//!
//! Each row is a value, one space, and the value's vector as decimals. A
//! first line of two integers (`count dimensions`) is skipped, as are blank
//! lines. A row whose vector is all zeros has no direction and is near
//! nothing. A value on several rows takes its vector from the first of them
//! as a query word, and is near a word when any of its rows is.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use compact_str::CompactString;
use lingrep_core::fsm::{CatalogVectors, NearQuery, WordVectors};
use memmap2::Mmap;
use rayon::prelude::*;

/// A cosine similarity in millionths, from -1,000,000 to 1,000,000.
///
/// Similarities are rounded to this precision before comparison.
pub type Millionths = i32;

const ONE: f64 = 1_000_000.0;

#[derive(Debug, thiserror::Error)]
pub enum VectorTableError {
    #[error("invalid vector query: {0}")]
    InvalidQuery(String),
    #[error("word-vector file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("cannot read word-vector file {}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("word-vector file {}: line {line}: {detail}", .path.display())]
    Malformed {
        path: PathBuf,
        line: u64,
        detail: String,
    },
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
    digest: OnceLock<blake3::Hash>,
    answered: Mutex<HashMap<NearQuery, Option<Arc<[CompactString]>>>>,
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
    /// Maps the file at `path`.
    ///
    /// # Errors
    ///
    /// [`VectorTableError::NotFound`] or [`VectorTableError::Io`].
    pub fn open(path: &Path) -> Result<Self, VectorTableError> {
        let io_error = |source: io::Error| {
            if source.kind() == io::ErrorKind::NotFound {
                VectorTableError::NotFound(path.to_owned())
            } else {
                VectorTableError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        };
        let file = File::open(path).map_err(io_error)?;
        // SAFETY: callers must keep the mapped file unchanged, as required by VectorTable.
        let map = unsafe { Mmap::map(&file) }.map_err(io_error)?;
        Ok(Self {
            path: path.to_owned(),
            map,
            digest: OnceLock::new(),
            answered: Mutex::new(HashMap::new()),
        })
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
            path: self.path.clone(),
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
        let mut answered = self.answered.lock().unwrap_or_else(PoisonError::into_inner);
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
                answered.insert((*query).clone(), Some(Arc::from(values)));
            }
            for query in unanswered {
                answered.entry(query.clone()).or_insert(None);
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

/// How a catalog's `vectors:` entries relate to the file that answered its
/// `near` tests.
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
pub fn vectors_binding(declared: &[CatalogVectors], path: &Path, blake3: &str) -> VectorsBinding {
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
                if label.is_empty() { "vectors" } else { label },
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
