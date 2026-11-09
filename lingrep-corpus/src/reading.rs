//! Reading sidecars: each document of a Markdown artifact rendered to HTML,
//! with a map from every stored sentence to the rendered text it came from.
//!
//! A sidecar is a directory beside the artifact, not inside it. It is keyed
//! by the artifact's analysis identity, its source identity and
//! [`MARKDOWN_RENDERER_REVISION`], and [`ReadingSidecar::open`] rejects a
//! sidecar whose key differs from the caller's. Its contents are a function
//! of the key alone, so two builds write identical bytes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use lingrep_core::analysis::NormalizationPolicy;
use lingrep_core::corpus::markdown_dialect::read_markdown_sentence_lines;
use lingrep_core::corpus::markdown_render::{
    MARKDOWN_RENDERER_REVISION, RenderedDocument, render_markdown,
};
use lingrep_core::corpus::{CorpusFormat, CorpusReadConfig, MarkdownReaderConfig};
use lingrep_core::execution::CancellationFlag;
use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::publication::StagingDirectory;
use crate::{
    CorpusArtifact, CorpusError, DocumentId, ObservedSources, ReaderIdentity, SentenceId,
    SourceObservation, format_identity,
};

pub const READING_SIDECAR_FORMAT: &str = "lingrep.reading-sidecar";
pub const READING_SIDECAR_VERSION: u32 = 1;

const HEADER: &str = "header.json";
const DOCUMENT_OFFSETS: &str = "document_offsets.u64";
const DOCUMENT_SENTENCES: &str = "document_sentences.u64";
const RECORDS: &str = "records.zst";
const ZSTD_LEVEL: i32 = 9;
const DOCUMENT_BATCH: usize = 64;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_WINDOW_LOG: u32 = 26;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadingSidecarKey {
    pub analysis_identity: String,
    pub source_identity: String,
    pub renderer_revision: String,
}

impl ReadingSidecarKey {
    pub fn for_artifact(artifact: &CorpusArtifact) -> Self {
        Self {
            analysis_identity: format_identity(artifact.analysis_identity().as_bytes()),
            source_identity: format_identity(artifact.source_identity().as_bytes()),
            renderer_revision: MARKDOWN_RENDERER_REVISION.to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadingSidecarError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("reading sidecar header is not valid: {0}")]
    Header(#[from] serde_json::Error),
    #[error(transparent)]
    Corpus(#[from] CorpusError),
    #[error(
        "{path} is a {found} version {version} sidecar, not {READING_SIDECAR_FORMAT} version {READING_SIDECAR_VERSION}; rebuild it with `ling corpus render`"
    )]
    UnsupportedFormat {
        path: PathBuf,
        found: String,
        version: u32,
    },
    #[error(
        "reading sidecar {path} was written for {field} {found}, but the current one is {expected}; rebuild it with `ling corpus render`"
    )]
    KeyMismatch {
        path: PathBuf,
        field: &'static str,
        expected: String,
        found: String,
    },
    #[error(
        "these Markdown reader options have reader identity {found}, but the artifact was read with {expected}; pass the --md-* options the artifact was built with"
    )]
    ReaderMismatch { expected: String, found: String },
    #[error(
        "the source tree has source identity {found}, but the artifact records {expected}; render from the tree the artifact was built from"
    )]
    SourceMismatch { expected: String, found: String },
    #[error("re-reading {source_name} does not reproduce the stored sentences: {detail}")]
    ReplayMismatch { source_name: String, detail: String },
    #[error("reading sidecar {component} record exceeds {limit} bytes")]
    RecordLimit {
        component: &'static str,
        limit: usize,
    },
    #[error("reading sidecar is corrupt: {0}")]
    Corrupt(String),
    #[error("{0}")]
    OutOfRange(String),
    #[error("destination already exists at {0}")]
    DestinationExists(PathBuf),
    #[error("reading sidecar writing was cancelled")]
    Cancelled,
}

fn corrupt(message: impl Into<String>) -> ReadingSidecarError {
    ReadingSidecarError::Corrupt(message.into())
}

fn record_limit(component: &'static str) -> ReadingSidecarError {
    ReadingSidecarError::RecordLimit {
        component,
        limit: MAX_RECORD_BYTES,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    version: u32,
    key: ReadingSidecarKey,
    documents: u64,
    sentences: u64,
    records_bytes: u64,
    records_blake3: String,
}

#[derive(Deserialize)]
struct HeaderVersion {
    format: String,
    version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Segment(u32, u32, u32, u32, u32);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockRecord {
    html: String,
    regions: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SentenceRecord {
    block: u32,
    segments: Vec<Segment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentRecord {
    source_name: String,
    regions: Vec<String>,
    blocks: Vec<BlockRecord>,
    container_blocks: Vec<u32>,
    sentences: Vec<SentenceRecord>,
}

#[derive(Clone, Copy, Debug)]
struct RenderedCharacter {
    character: char,
    container: u32,
    utf16: u32,
}

fn rendered_characters(document: &RenderedDocument, line: usize) -> Vec<RenderedCharacter> {
    let mut characters = Vec::new();
    for run in document.runs.iter().filter(|run| run.line == line) {
        let mut utf16 = run.utf16_start;
        for character in run.text.chars() {
            characters.push(RenderedCharacter {
                character,
                container: run.container,
                utf16,
            });
            utf16 += character.len_utf16() as u32;
        }
    }
    characters
}

fn align(stored: &str, rendered: &[RenderedCharacter]) -> Vec<Segment> {
    let stored_characters: Vec<char> = stored.chars().collect();
    let rendered_characters: Vec<char> = rendered.iter().map(|r| r.character).collect();
    let pairs: Vec<(usize, usize)> = if let Some(start) = rendered_characters
        .windows(stored_characters.len().max(1))
        .position(|window| window == stored_characters.as_slice())
        .filter(|_| !stored_characters.is_empty())
    {
        (0..stored_characters.len())
            .map(|index| (index, start + index))
            .collect()
    } else {
        similar::capture_diff_slices(
            similar::Algorithm::Myers,
            &stored_characters,
            &rendered_characters,
        )
        .into_iter()
        .filter_map(|operation| match operation {
            similar::DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => Some((0..len).map(move |offset| (old_index + offset, new_index + offset))),
            _ => None,
        })
        .flatten()
        .collect()
    };
    let mut segments: Vec<Segment> = Vec::new();
    let mut previous: Option<(usize, usize)> = None;
    for (stored_index, rendered_index) in pairs {
        let character = rendered[rendered_index];
        let units = character.character.len_utf16() as u32;
        let extends =
            previous
                .zip(segments.last())
                .is_some_and(|((last_stored, last_rendered), segment)| {
                    let last = rendered[last_rendered];
                    stored_index == last_stored + 1
                        && rendered_index == last_rendered + 1
                        && character.container == segment.2
                        && units == 1
                        && segment.1 == segment.4
                        && character.utf16 == last.utf16 + 1
                });
        match segments.last_mut() {
            Some(segment) if extends => {
                segment.1 += 1;
                segment.4 += 1;
            }
            _ => segments.push(Segment(
                stored_index as u32,
                1,
                character.container,
                character.utf16,
                units,
            )),
        }
        previous = Some((stored_index, rendered_index));
    }
    segments
}

fn covered(segments: &[Segment], chars: &Range<u32>) -> u32 {
    segments
        .iter()
        .map(|segment| {
            let end = segment.0 + segment.1;
            chars
                .end
                .min(end)
                .saturating_sub(chars.start.max(segment.0))
        })
        .sum()
}

/// A rendered stretch of one `data-c` element's text content, in UTF-16
/// units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightRange {
    pub container: u32,
    pub utf16_start: u32,
    pub utf16_end: u32,
}

fn highlight(segments: &[Segment], chars: &Range<u32>) -> Vec<HighlightRange> {
    segments
        .iter()
        .filter_map(|segment| {
            let start = chars.start.max(segment.0);
            let end = chars.end.min(segment.0 + segment.1);
            if start >= end {
                return None;
            }
            Some(if segment.1 == segment.4 {
                HighlightRange {
                    container: segment.2,
                    utf16_start: segment.3 + (start - segment.0),
                    utf16_end: segment.3 + (end - segment.0),
                }
            } else {
                HighlightRange {
                    container: segment.2,
                    utf16_start: segment.3,
                    utf16_end: segment.3 + segment.4,
                }
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadingSidecarStats {
    pub documents: u64,
    pub sentences: u64,
    /// Sentences whose every character maps to rendered text.
    pub sentences_rendered_in_full: u64,
    /// Sentences none of whose characters map to rendered text.
    pub sentences_without_rendered_text: u64,
    /// Total size of published files, including the header.
    pub bytes: u64,
}

pub struct ReadingSidecarWriter<'a> {
    artifact: &'a CorpusArtifact,
    reader: MarkdownReaderConfig,
    normalization: NormalizationPolicy,
    destination: PathBuf,
}

impl<'a> ReadingSidecarWriter<'a> {
    /// Starts a sidecar at `destination`, which must not exist, and rejects
    /// `reader` unless it is the reading the artifact records.
    pub fn new(
        artifact: &'a CorpusArtifact,
        reader: MarkdownReaderConfig,
        destination: &Path,
    ) -> Result<Self, ReadingSidecarError> {
        if destination.exists() {
            return Err(ReadingSidecarError::DestinationExists(
                destination.to_path_buf(),
            ));
        }
        let normalization = match artifact.manifest.analyzer.normalization.as_str() {
            "nfc" => NormalizationPolicy::Nfc,
            _ => NormalizationPolicy::None,
        };
        let found = ReaderIdentity::derive(
            &CorpusReadConfig::new(CorpusFormat::Markdown, reader.clone()),
            normalization,
        );
        let expected = artifact.analysis_inputs()?.reader();
        if found != expected {
            return Err(ReadingSidecarError::ReaderMismatch {
                expected: format_identity(expected.as_bytes()),
                found: format_identity(found.as_bytes()),
            });
        }
        Ok(Self {
            artifact,
            reader,
            normalization,
            destination: destination.to_path_buf(),
        })
    }

    /// Renders every document and stages the sidecar.
    ///
    /// `observed` must have the artifact's source identity. `read_source`
    /// returns source bytes for each logical document name; the bytes returned
    /// across all calls must reproduce that identity. Once `cancellation` is raised
    /// this fails with [`ReadingSidecarError::Cancelled`] and removes the
    /// staging directory.
    pub async fn write(
        self,
        observed: &ObservedSources,
        read_source: impl Fn(&str) -> io::Result<Vec<u8>> + Sync,
        cancellation: &CancellationFlag,
    ) -> Result<StagedReadingSidecar, ReadingSidecarError> {
        let artifact = self.artifact;
        if observed.source_identity() != artifact.source_identity() {
            return Err(ReadingSidecarError::SourceMismatch {
                expected: format_identity(artifact.source_identity().as_bytes()),
                found: format_identity(observed.source_identity().as_bytes()),
            });
        }
        let view = artifact.provenance_view().await?;
        let ranges: std::collections::HashMap<u64, (u64, u64)> = view
            .document_sentence_ranges()
            .await?
            .into_iter()
            .map(|(id, first, count)| (id, (first, count)))
            .collect();
        let mut documents = Vec::new();
        let mut next_sentence = 0;
        for ordinal in 0..artifact.document_count() {
            let document = artifact.locations.document(ordinal)?;
            let (first, count) = ranges.get(&ordinal).copied().unwrap_or((next_sentence, 0));
            if document.document_id.get() != ordinal || first != next_sentence {
                return Err(corrupt(format!(
                    "document {ordinal} is not the next dense, contiguous document"
                )));
            }
            next_sentence = first + count;
            documents.push((document.source_name, first..next_sentence));
        }
        if next_sentence != artifact.sentence_count() {
            return Err(corrupt("documents do not cover the artifact's sentences"));
        }

        let staging = StagingDirectory::create_beside(&self.destination)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
        })?;
        let mut records = staged_file(&staging.path, RECORDS)?;
        let mut hasher = blake3::Hasher::new();
        let mut offsets = vec![0_u64];
        let mut stats = ReadingSidecarStats {
            documents: documents.len() as u64,
            sentences: next_sentence,
            ..ReadingSidecarStats::default()
        };
        let rendered_sources = SourceObservation::new();
        for batch in documents.chunks(DOCUMENT_BATCH) {
            if cancellation.is_cancelled() {
                return Err(ReadingSidecarError::Cancelled);
            }
            let mut stored = Vec::with_capacity(batch.len());
            for (source_name, sentences) in batch {
                let bytes = read_source(source_name)?;
                rendered_sources
                    .begin_source(source_name)
                    .map_err(CorpusError::from)?;
                rendered_sources
                    .update_source_bytes(&bytes)
                    .map_err(CorpusError::from)?;
                rendered_sources.end_source().map_err(CorpusError::from)?;
                let mut texts = Vec::with_capacity((sentences.end - sentences.start) as usize);
                for sentence in sentences.clone() {
                    texts.push(
                        view.sentence_analysis(SentenceId::new(sentence))
                            .await?
                            .text,
                    );
                }
                stored.push((texts, bytes));
            }
            let frames: Vec<Result<(Vec<u8>, DocumentStats), ReadingSidecarError>> = batch
                .par_iter()
                .zip(stored)
                .map(|((source_name, _), (texts, bytes))| {
                    let record = self.document_record(source_name, &texts, &bytes)?;
                    let stats = DocumentStats::of(&record, &texts);
                    validate_rendered_size(&record)?;
                    let json = serde_json::to_vec(&record)?;
                    if json.len() > MAX_RECORD_BYTES {
                        return Err(record_limit("decompressed"));
                    }
                    let frame = zstd::bulk::compress(&json, ZSTD_LEVEL)?;
                    if frame.len() > MAX_RECORD_BYTES {
                        return Err(record_limit("compressed"));
                    }
                    Ok((frame, stats))
                })
                .collect();
            for frame in frames {
                let (frame, document) = frame?;
                records.write_all(&frame)?;
                hasher.update(&frame);
                offsets.push(offsets.last().copied().unwrap_or(0) + frame.len() as u64);
                stats.sentences_rendered_in_full += document.rendered_in_full;
                stats.sentences_without_rendered_text += document.without_rendered_text;
            }
        }
        let rendered_sources = rendered_sources.finish().map_err(CorpusError::from)?;
        if rendered_sources.source_identity() != artifact.source_identity() {
            return Err(ReadingSidecarError::SourceMismatch {
                expected: format_identity(artifact.source_identity().as_bytes()),
                found: format_identity(rendered_sources.source_identity().as_bytes()),
            });
        }
        finish_file(records)?;
        write_u64s(&staging.path, DOCUMENT_OFFSETS, &offsets)?;
        let mut sentence_starts: Vec<u64> = documents
            .iter()
            .map(|(_, sentences)| sentences.start)
            .collect();
        sentence_starts.push(next_sentence);
        write_u64s(&staging.path, DOCUMENT_SENTENCES, &sentence_starts)?;
        let header = Header {
            format: READING_SIDECAR_FORMAT.to_owned(),
            version: READING_SIDECAR_VERSION,
            key: ReadingSidecarKey::for_artifact(artifact),
            documents: stats.documents,
            sentences: stats.sentences,
            records_bytes: offsets.last().copied().unwrap_or(0),
            records_blake3: hasher.finalize().to_hex().to_string(),
        };
        let mut file = staged_file(&staging.path, HEADER)?;
        file.write_all(&serde_json::to_vec_pretty(&header)?)?;
        finish_file(file)?;
        if cancellation.is_cancelled() {
            return Err(ReadingSidecarError::Cancelled);
        }
        Ok(StagedReadingSidecar {
            staging,
            destination: self.destination,
            stats,
        })
    }

    fn document_record(
        &self,
        source_name: &str,
        stored: &[String],
        bytes: &[u8],
    ) -> Result<DocumentRecord, ReadingSidecarError> {
        let replay = |detail: String| ReadingSidecarError::ReplayMismatch {
            source_name: source_name.to_owned(),
            detail,
        };
        let source = std::str::from_utf8(bytes).map_err(|error| replay(error.to_string()))?;
        let lines = read_markdown_sentence_lines(io::Cursor::new(source), self.reader.clone())
            .map_err(|error| replay(error.to_string()))?;
        if lines.len() != stored.len() {
            return Err(replay(format!(
                "the reader admits {} sentences, and the artifact stores {}",
                lines.len(),
                stored.len()
            )));
        }
        let rendered = render_markdown(source);
        if rendered.blocks.is_empty() && !stored.is_empty() {
            return Err(replay("the document renders no block".into()));
        }
        let mut sentences = Vec::with_capacity(stored.len());
        for (index, (line, text)) in lines.iter().zip(stored).enumerate() {
            if !self.normalization.normalizes(&line.text, text) {
                return Err(replay(format!(
                    "sentence {index} on line {} differs from the stored text",
                    line.line
                )));
            }
            let segments = align(text, &rendered_characters(&rendered, line.line));
            let block = segments.first().map_or_else(
                || {
                    rendered
                        .blocks
                        .partition_point(|block| block.first_line <= line.line)
                        .saturating_sub(1) as u32
                },
                |segment| rendered.container_blocks[segment.2 as usize],
            );
            sentences.push(SentenceRecord { block, segments });
        }
        Ok(DocumentRecord {
            source_name: source_name.to_owned(),
            regions: rendered.regions,
            blocks: rendered
                .blocks
                .into_iter()
                .map(|block| BlockRecord {
                    html: block.html,
                    regions: block.regions,
                })
                .collect(),
            container_blocks: rendered.container_blocks,
            sentences,
        })
    }
}

struct DocumentStats {
    rendered_in_full: u64,
    without_rendered_text: u64,
}

impl DocumentStats {
    fn of(record: &DocumentRecord, texts: &[String]) -> Self {
        let mut stats = Self {
            rendered_in_full: 0,
            without_rendered_text: 0,
        };
        for (sentence, text) in record.sentences.iter().zip(texts) {
            let length = text.chars().count() as u32;
            let mapped = covered(&sentence.segments, &(0..length));
            stats.rendered_in_full += u64::from(mapped == length);
            stats.without_rendered_text += u64::from(mapped == 0 && length > 0);
        }
        stats
    }
}

fn staged_file(directory: &Path, name: &str) -> io::Result<io::BufWriter<File>> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(name))?;
    Ok(io::BufWriter::with_capacity(1 << 20, file))
}

fn finish_file(writer: io::BufWriter<File>) -> io::Result<()> {
    writer
        .into_inner()
        .map_err(io::IntoInnerError::into_error)?
        .sync_all()
}

fn write_u64s(directory: &Path, name: &str, values: &[u64]) -> io::Result<()> {
    let mut file = staged_file(directory, name)?;
    for value in values {
        file.write_all(&value.to_le_bytes())?;
    }
    finish_file(file)
}

/// A complete sidecar in its staging directory, not yet visible at its
/// destination. Dropping it removes the staging directory.
pub struct StagedReadingSidecar {
    staging: StagingDirectory,
    destination: PathBuf,
    stats: ReadingSidecarStats,
}

impl StagedReadingSidecar {
    /// Renames the staged sidecar to its destination without replacing
    /// anything there.
    pub fn publish(mut self) -> Result<ReadingSidecarStats, ReadingSidecarError> {
        crate::rename_noreplace(&self.staging.path, &self.destination).map_err(|error| {
            if matches!(error.raw_os_error(), Some(libc::EEXIST | libc::ENOTEMPTY)) {
                ReadingSidecarError::DestinationExists(self.destination.clone())
            } else {
                ReadingSidecarError::Io(error)
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
            .sum::<io::Result<u64>>()?;
        Ok(self.stats)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageRequest {
    /// The block that holds this sentence, and the blocks around it.
    Around(SentenceId),
    /// This block of the document and the blocks after it.
    From(DocumentId, u32),
    /// The blocks of the document before this block.
    Before(DocumentId, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingDocument {
    pub document: DocumentId,
    pub source_name: String,
    pub block_count: u32,
    /// The document's stored sentences, as corpus sentence ordinals.
    pub sentences: Range<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceAnchor {
    pub sentence: SentenceId,
    pub ranges: Vec<HighlightRange>,
}

/// Consecutive blocks of one document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingPage {
    pub document: DocumentId,
    pub blocks: Range<u32>,
    pub html: String,
    /// Every sentence with rendered text on the page, in sentence order.
    pub anchors: Vec<SentenceAnchor>,
    pub previous: Option<PageRequest>,
    pub next: Option<PageRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OccurrenceAnchor {
    pub document: DocumentId,
    /// The block that holds the occurrence's sentence.
    pub block: u32,
    pub ranges: Vec<HighlightRange>,
    /// Whether every character of the occurrence maps to rendered text.
    pub exact: bool,
}

/// Document reads accept at most 64 MiB each of compressed data, decoded
/// JSON and expanded HTML, with a Zstd window of at most 64 MiB.
pub struct ReadingSidecar {
    path: PathBuf,
    header: Header,
    document_offsets: Vec<u64>,
    document_sentences: Vec<u64>,
    records: Option<Mmap>,
}

fn read_u64s(path: &Path, count: u64) -> Result<Vec<u64>, ReadingSidecarError> {
    let expected = count
        .checked_mul(8)
        .filter(|bytes| *bytes <= isize::MAX as u64)
        .ok_or_else(|| {
            corrupt(format!(
                "{} dimensions exceed the addressable size",
                path.display()
            ))
        })?;
    let file = File::open(path)?;
    if file.metadata()?.len() != expected {
        return Err(corrupt(format!(
            "{} is not {count} u64 values",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(expected + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected {
        return Err(corrupt(format!(
            "{} is not {count} u64 values",
            path.display()
        )));
    }
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect())
}

fn read_header(path: &Path) -> Result<Header, ReadingSidecarError> {
    let header_bytes = fs::read(path.join(HEADER))?;
    let version: HeaderVersion = serde_json::from_slice(&header_bytes)?;
    if version.format != READING_SIDECAR_FORMAT || version.version != READING_SIDECAR_VERSION {
        return Err(ReadingSidecarError::UnsupportedFormat {
            path: path.to_path_buf(),
            found: version.format,
            version: version.version,
        });
    }
    Ok(serde_json::from_slice(&header_bytes)?)
}

fn ascending(values: &[u64], last: u64) -> bool {
    values.first() == Some(&0)
        && values.last() == Some(&last)
        && values.windows(2).all(|pair| pair[0] <= pair[1])
}

fn select_blocks(costs: &[usize], start: Selection, max_bytes: usize) -> Range<usize> {
    let (mut low, mut high, forward, backward) = match start {
        Selection::Around(block) => (block, block + 1, true, true),
        Selection::From(block) => (block, block + 1, true, false),
        Selection::Before(block) => (block - 1, block, false, true),
    };
    let mut used = costs[low];
    loop {
        let mut moved = false;
        if forward && high < costs.len() && used + costs[high] <= max_bytes {
            used += costs[high];
            high += 1;
            moved = true;
        }
        if backward && low > 0 && used + costs[low - 1] <= max_bytes {
            low -= 1;
            used += costs[low];
            moved = true;
        }
        if !moved {
            return low..high;
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Selection {
    Around(usize),
    From(usize),
    Before(usize),
}

fn region_open(regions: &[String], region: u32) -> String {
    let classes = regions
        .get(region as usize)
        .map(String::as_str)
        .unwrap_or_default();
    let mut tag = String::from("<div class=\"");
    for character in classes.chars() {
        match character {
            '&' => tag.push_str("&amp;"),
            '<' => tag.push_str("&lt;"),
            '>' => tag.push_str("&gt;"),
            '"' => tag.push_str("&quot;"),
            other => tag.push(other),
        }
    }
    tag.push_str(&format!("\" data-region=\"{region}\">"));
    tag
}

const REGION_CLOSE: &str = "</div>";

fn validate_rendered_size(record: &DocumentRecord) -> Result<(), ReadingSidecarError> {
    let add = |total: usize, bytes: usize| {
        total
            .checked_add(bytes)
            .filter(|total| *total <= MAX_RECORD_BYTES)
            .ok_or_else(|| record_limit("rendered"))
    };
    let regions = record
        .regions
        .iter()
        .enumerate()
        .map(|(index, classes)| {
            classes.bytes().try_fold(
                region_open(&[], index as u32).len() + REGION_CLOSE.len(),
                |total, byte| {
                    add(
                        total,
                        match byte {
                            b'&' => 5,
                            b'<' | b'>' => 4,
                            b'"' => 6,
                            _ => 1,
                        },
                    )
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    record.blocks.iter().try_fold(0, |total, block| {
        block
            .regions
            .iter()
            .try_fold(add(total, block.html.len())?, |total, region| {
                let bytes = regions
                    .get(*region as usize)
                    .ok_or_else(|| corrupt("region index out of range"))?;
                add(total, *bytes)
            })
    })?;
    Ok(())
}

fn block_cost(record: &DocumentRecord, block: &BlockRecord) -> usize {
    block.html.len()
        + block
            .regions
            .iter()
            .map(|region| region_open(&record.regions, *region).len() + REGION_CLOSE.len())
            .sum::<usize>()
}

fn assemble(record: &DocumentRecord, blocks: Range<usize>) -> String {
    let mut html = String::new();
    let mut open: Vec<u32> = Vec::new();
    for block in &record.blocks[blocks] {
        let shared = open
            .iter()
            .zip(&block.regions)
            .take_while(|(open, wanted)| open == wanted)
            .count();
        for _ in shared..open.len() {
            html.push_str(REGION_CLOSE);
        }
        open.truncate(shared);
        for region in &block.regions[shared..] {
            html.push_str(&region_open(&record.regions, *region));
            open.push(*region);
        }
        html.push_str(&block.html);
    }
    for _ in &open {
        html.push_str(REGION_CLOSE);
    }
    html
}

impl ReadingSidecar {
    /// Opens the sidecar at `path` and rejects it unless its key equals
    /// `expected`. Checks the format version and the index files; the
    /// records are checked by [`Self::verify`].
    pub fn open(path: &Path, expected: &ReadingSidecarKey) -> Result<Self, ReadingSidecarError> {
        let header = read_header(path)?;
        for (field, expected, found) in [
            (
                "analysis identity",
                &expected.analysis_identity,
                &header.key.analysis_identity,
            ),
            (
                "source identity",
                &expected.source_identity,
                &header.key.source_identity,
            ),
            (
                "renderer revision",
                &expected.renderer_revision,
                &header.key.renderer_revision,
            ),
        ] {
            if expected != found {
                return Err(ReadingSidecarError::KeyMismatch {
                    path: path.to_path_buf(),
                    field,
                    expected: expected.clone(),
                    found: found.clone(),
                });
            }
        }
        let index_count = header
            .documents
            .checked_add(1)
            .ok_or_else(|| corrupt("document count exceeds the index format limit"))?;
        let document_offsets = read_u64s(&path.join(DOCUMENT_OFFSETS), index_count)?;
        let document_sentences = read_u64s(&path.join(DOCUMENT_SENTENCES), index_count)?;
        if !ascending(&document_offsets, header.records_bytes)
            || !ascending(&document_sentences, header.sentences)
        {
            return Err(corrupt(
                "document offsets or sentence ranges are not ascending",
            ));
        }
        if header.records_bytes > isize::MAX as u64 {
            return Err(corrupt("records exceed the addressable size"));
        }
        let file = File::open(path.join(RECORDS))?;
        if file.metadata()?.len() != header.records_bytes {
            return Err(corrupt(format!(
                "{RECORDS} is not {} bytes",
                header.records_bytes
            )));
        }
        // SAFETY: a published sidecar is immutable for every open reader, as
        // an artifact's components are (crate documentation).
        let records = (header.records_bytes > 0)
            .then(|| unsafe { Mmap::map(&file) })
            .transpose()?;
        Ok(Self {
            path: path.to_path_buf(),
            header,
            document_offsets,
            document_sentences,
            records,
        })
    }

    /// The key of the sidecar at `path`, read from its header without opening
    /// its index or records.
    pub fn read_key(path: &Path) -> Result<ReadingSidecarKey, ReadingSidecarError> {
        Ok(read_header(path)?.key)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn key(&self) -> &ReadingSidecarKey {
        &self.header.key
    }

    pub fn document_count(&self) -> u64 {
        self.header.documents
    }

    fn document_index(&self, document: DocumentId) -> Result<usize, ReadingSidecarError> {
        if document.get() < self.header.documents {
            Ok(document.get() as usize)
        } else {
            Err(ReadingSidecarError::OutOfRange(format!(
                "document {} is not among the sidecar's {} documents",
                document.get(),
                self.header.documents
            )))
        }
    }

    fn document_of(&self, sentence: SentenceId) -> Result<usize, ReadingSidecarError> {
        if sentence.get() >= self.header.sentences {
            return Err(ReadingSidecarError::OutOfRange(format!(
                "sentence {} is not among the sidecar's {} sentences",
                sentence.get(),
                self.header.sentences
            )));
        }
        Ok(self
            .document_sentences
            .partition_point(|first| *first <= sentence.get())
            - 1)
    }

    fn record(&self, index: usize) -> Result<DocumentRecord, ReadingSidecarError> {
        let start = self.document_offsets[index] as usize;
        let end = self.document_offsets[index + 1] as usize;
        let frame = self
            .records
            .as_ref()
            .and_then(|records| records.get(start..end))
            .unwrap_or_default();
        if frame.len() > MAX_RECORD_BYTES {
            return Err(record_limit("compressed"));
        }
        let decode_error = |error| corrupt(format!("document {index}: {error}"));
        let mut decoder = zstd::stream::read::Decoder::new(frame).map_err(decode_error)?;
        decoder
            .window_log_max(MAX_WINDOW_LOG)
            .map_err(decode_error)?;
        let mut json = Vec::new();
        decoder
            .by_ref()
            .take(MAX_RECORD_BYTES as u64)
            .read_to_end(&mut json)
            .map_err(decode_error)?;
        if decoder.read(&mut [0]).map_err(decode_error)? != 0 {
            return Err(record_limit("decompressed"));
        }
        let record: DocumentRecord = serde_json::from_slice(&json)
            .map_err(|error| corrupt(format!("document {index}: {error}")))?;
        let sentences = self.document_sentences[index + 1] - self.document_sentences[index];
        let blocks = record.blocks.len() as u32;
        let containers = record.container_blocks.len() as u32;
        let regions = record.regions.len() as u32;
        if record.sentences.len() as u64 != sentences
            || record.container_blocks.iter().any(|block| *block >= blocks)
            || record
                .blocks
                .iter()
                .flat_map(|block| &block.regions)
                .any(|region| *region >= regions)
            || record.sentences.iter().any(|sentence| {
                sentence.block >= blocks
                    || sentence
                        .segments
                        .iter()
                        .any(|segment| segment.2 >= containers || segment.1 == 0)
            })
        {
            return Err(corrupt(format!(
                "document {index} does not match the index"
            )));
        }
        validate_rendered_size(&record)?;
        Ok(record)
    }

    pub fn document(&self, document: DocumentId) -> Result<ReadingDocument, ReadingSidecarError> {
        let index = self.document_index(document)?;
        let record = self.record(index)?;
        Ok(ReadingDocument {
            document,
            source_name: record.source_name,
            block_count: record.blocks.len() as u32,
            sentences: self.document_sentences[index]..self.document_sentences[index + 1],
        })
    }

    /// Consecutive blocks whose HTML totals at most `max_html_bytes`, unless
    /// the first block alone is larger.
    pub fn page(
        &self,
        request: PageRequest,
        max_html_bytes: usize,
    ) -> Result<ReadingPage, ReadingSidecarError> {
        let index = match request {
            PageRequest::Around(sentence) => self.document_of(sentence)?,
            PageRequest::From(document, _) | PageRequest::Before(document, _) => {
                self.document_index(document)?
            }
        };
        let record = self.record(index)?;
        Ok(self.page_of(&record, index, request, max_html_bytes)?.0)
    }

    /// The page around `sentence`, as [`Self::page`] gives it, and where the
    /// characters `chars` of its stored text are rendered on that page. The
    /// anchor is not exact when some of those characters are rendered on no
    /// block of the page.
    pub fn page_at(
        &self,
        sentence: SentenceId,
        chars: Range<u32>,
        max_html_bytes: usize,
    ) -> Result<(ReadingPage, OccurrenceAnchor), ReadingSidecarError> {
        let index = self.document_of(sentence)?;
        let record = self.record(index)?;
        let (page, containers) = self.page_of(
            &record,
            index,
            PageRequest::Around(sentence),
            max_html_bytes,
        )?;
        let mut occurrence = self.occurrence(&record, index, sentence, &chars);
        let ranges = occurrence.ranges.len();
        occurrence
            .ranges
            .retain(|range| containers.binary_search(&range.container).is_ok());
        occurrence.exact &= occurrence.ranges.len() == ranges;
        Ok((page, occurrence))
    }

    /// The page `request` selects from the document `record`, and the
    /// containers of its blocks in ascending order.
    fn page_of(
        &self,
        record: &DocumentRecord,
        index: usize,
        request: PageRequest,
        max_html_bytes: usize,
    ) -> Result<(ReadingPage, Vec<u32>), ReadingSidecarError> {
        let first_sentence = self.document_sentences[index];
        let selection = match request {
            PageRequest::Around(sentence) => {
                let local = (sentence.get() - first_sentence) as usize;
                Selection::Around(record.sentences[local].block as usize)
            }
            PageRequest::From(_, block) => Selection::From(block as usize),
            PageRequest::Before(_, block) => Selection::Before(block as usize),
        };
        let count = record.blocks.len();
        let valid = match selection {
            Selection::Around(block) | Selection::From(block) => block < count,
            Selection::Before(block) => block > 0 && block <= count,
        };
        if !valid {
            return Err(ReadingSidecarError::OutOfRange(format!(
                "{request:?} is outside the document's {count} blocks"
            )));
        }
        let costs: Vec<usize> = record
            .blocks
            .iter()
            .map(|block| block_cost(record, block))
            .collect();
        let blocks = select_blocks(&costs, selection, max_html_bytes);
        let html = assemble(record, blocks.clone());
        let containers: Vec<u32> = (0..record.container_blocks.len() as u32)
            .filter(|container| {
                blocks.contains(&(record.container_blocks[*container as usize] as usize))
            })
            .collect();
        let anchors = record
            .sentences
            .iter()
            .enumerate()
            .filter_map(|(local, sentence)| {
                let ranges: Vec<HighlightRange> = highlight(&sentence.segments, &(0..u32::MAX))
                    .into_iter()
                    .filter(|range| containers.binary_search(&range.container).is_ok())
                    .collect();
                (!ranges.is_empty()).then(|| SentenceAnchor {
                    sentence: SentenceId::new(first_sentence + local as u64),
                    ranges,
                })
            })
            .collect();
        let document = DocumentId::new(index as u64);
        let page = ReadingPage {
            document,
            previous: (blocks.start > 0)
                .then_some(PageRequest::Before(document, blocks.start as u32)),
            next: (blocks.end < count).then_some(PageRequest::From(document, blocks.end as u32)),
            blocks: blocks.start as u32..blocks.end as u32,
            html,
            anchors,
        };
        Ok((page, containers))
    }

    /// Where the characters `chars` of `sentence`'s stored text are rendered.
    pub fn anchor(
        &self,
        sentence: SentenceId,
        chars: Range<u32>,
    ) -> Result<OccurrenceAnchor, ReadingSidecarError> {
        let index = self.document_of(sentence)?;
        let record = self.record(index)?;
        Ok(self.occurrence(&record, index, sentence, &chars))
    }

    fn occurrence(
        &self,
        record: &DocumentRecord,
        index: usize,
        sentence: SentenceId,
        chars: &Range<u32>,
    ) -> OccurrenceAnchor {
        let local = (sentence.get() - self.document_sentences[index]) as usize;
        let stored = &record.sentences[local];
        let mapped = covered(&stored.segments, chars);
        OccurrenceAnchor {
            document: DocumentId::new(index as u64),
            block: stored.block,
            ranges: highlight(&stored.segments, chars),
            exact: mapped == chars.end.saturating_sub(chars.start),
        }
    }

    /// Checks the records digest; reads the whole records file once.
    pub fn verify_digest(&self) -> Result<(), ReadingSidecarError> {
        let digest = blake3::hash(self.records.as_deref().unwrap_or_default());
        if digest.to_hex().as_str() != self.header.records_blake3 {
            return Err(corrupt("records digest differs from the header"));
        }
        Ok(())
    }

    /// Checks the records digest and decodes every record; reads the whole
    /// sidecar.
    pub fn verify(&self) -> Result<(), ReadingSidecarError> {
        self.verify_digest()?;
        for index in 0..self.header.documents as usize {
            self.record(index)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn characters(text: &str, container: u32) -> Vec<RenderedCharacter> {
        let mut utf16 = 0;
        text.chars()
            .map(|character| {
                let rendered = RenderedCharacter {
                    character,
                    container,
                    utf16,
                };
                utf16 += character.len_utf16() as u32;
                rendered
            })
            .collect()
    }

    fn record_fixture(frame: &[u8], sentences: u64) -> (tempfile::TempDir, ReadingSidecar) {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path();
        let key = ReadingSidecarKey {
            analysis_identity: "analysis".into(),
            source_identity: "source".into(),
            renderer_revision: MARKDOWN_RENDERER_REVISION.into(),
        };
        let header = Header {
            format: READING_SIDECAR_FORMAT.into(),
            version: READING_SIDECAR_VERSION,
            key: key.clone(),
            documents: 1,
            sentences,
            records_bytes: frame.len() as u64,
            records_blake3: blake3::hash(frame).to_hex().to_string(),
        };
        fs::write(
            path.join(HEADER),
            serde_json::to_vec(&header).expect("JSON"),
        )
        .expect("header");
        fs::write(
            path.join(DOCUMENT_OFFSETS),
            [0u64.to_le_bytes(), (frame.len() as u64).to_le_bytes()].concat(),
        )
        .expect("offsets");
        fs::write(
            path.join(DOCUMENT_SENTENCES),
            [0u64.to_le_bytes(), sentences.to_le_bytes()].concat(),
        )
        .expect("sentences");
        fs::write(path.join(RECORDS), frame).expect("records");
        let sidecar = ReadingSidecar::open(path, &key).expect("open");
        (directory, sidecar)
    }

    fn paged_record(blocks: u32) -> DocumentRecord {
        DocumentRecord {
            source_name: "a.md".into(),
            regions: Vec::new(),
            blocks: (0..blocks)
                .map(|index| BlockRecord {
                    html: format!("<p data-c=\"{index}\">猫𝑥</p>"),
                    regions: Vec::new(),
                })
                .collect(),
            container_blocks: (0..blocks).collect(),
            sentences: (0..blocks)
                .map(|index| SentenceRecord {
                    block: index,
                    segments: vec![Segment(0, 1, index, 0, 1), Segment(1, 1, index, 1, 2)],
                })
                .collect(),
        }
    }

    #[test]
    fn tiny_reads_reject_high_compression_records_before_json_decoding() {
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).expect("encoder");
        for _ in 0..8193 {
            encoder.write_all(&[b' '; 8192]).expect("padding");
        }
        encoder
            .write_all(&serde_json::to_vec(&paged_record(1)).expect("JSON"))
            .expect("record");
        let frame = encoder.finish().expect("frame");
        assert!(frame.len() < 65536);
        let (_directory, sidecar) = record_fixture(&frame, 1);
        for error in [
            sidecar.document(DocumentId::new(0)).err(),
            sidecar
                .page(PageRequest::From(DocumentId::new(0), 0), 1)
                .err(),
            sidecar.anchor(SentenceId::new(0), 0..1).err(),
            sidecar.verify().err(),
        ] {
            assert!(
                error
                    .is_some_and(|error| error.to_string().contains("decompressed record exceeds"))
            );
        }
    }

    #[test]
    fn oversized_compressed_records_are_rejected_before_decoding() {
        let frame = vec![0; 64 * 1024 * 1024 + 1];
        let (_directory, sidecar) = record_fixture(&frame, 1);
        let error = sidecar
            .document(DocumentId::new(0))
            .expect_err("reject frame");
        assert!(
            error.to_string().contains("compressed record exceeds"),
            "{error}"
        );
    }

    #[test]
    fn small_records_with_oversized_decoder_windows_are_rejected() {
        let json = serde_json::to_vec(&paged_record(1)).expect("JSON");
        let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0, 0x88];
        let block = ((json.len() as u32) << 3) | 1;
        frame.extend_from_slice(&block.to_le_bytes()[..3]);
        frame.extend_from_slice(&json);
        assert_eq!(
            zstd::bulk::decompress(&frame, json.len()).expect("valid frame"),
            json
        );
        let (_directory, sidecar) = record_fixture(&frame, 1);
        assert!(matches!(
            sidecar.document(DocumentId::new(0)),
            Err(ReadingSidecarError::Corrupt(_))
        ));
    }

    #[test]
    fn escaped_regions_are_bounded_before_page_assembly() {
        let mut record = paged_record(1);
        record.regions.push("\"".repeat(12 * 1024 * 1024));
        record.blocks[0].regions.push(0);
        let frame =
            zstd::bulk::compress(&serde_json::to_vec(&record).expect("JSON"), 1).expect("compress");
        let (_directory, sidecar) = record_fixture(&frame, 1);
        let error = sidecar
            .page(PageRequest::From(DocumentId::new(0), 0), 1)
            .expect_err("reject expanded HTML");
        assert!(
            error.to_string().contains("rendered record exceeds"),
            "{error}"
        );
    }

    #[test]
    fn multiple_pages_preserve_html_and_unicode_anchors() {
        let record = paged_record(100);
        let frame =
            zstd::bulk::compress(&serde_json::to_vec(&record).expect("JSON"), 1).expect("compress");
        let (_directory, sidecar) = record_fixture(&frame, 100);
        sidecar.verify().expect("verify");
        assert_eq!(
            sidecar
                .document(DocumentId::new(0))
                .expect("document")
                .block_count,
            100
        );
        let mut request = Some(PageRequest::From(DocumentId::new(0), 0));
        let mut html = String::new();
        let mut pages = 0;
        while let Some(next) = request {
            let page = sidecar.page(next, 100).expect("page");
            assert!(page.html.len() <= 100);
            for anchor in &page.anchors {
                let index = anchor.sentence.get() as u32;
                let occurrence = sidecar.anchor(anchor.sentence, 1..2).expect("anchor");
                assert!(occurrence.exact);
                assert_eq!(
                    occurrence.ranges,
                    [HighlightRange {
                        container: index,
                        utf16_start: 1,
                        utf16_end: 3
                    }]
                );
            }
            html.push_str(&page.html);
            pages += 1;
            request = page.next;
        }
        assert!(pages > 20);
        assert_eq!(html, assemble(&record, 0..100));
    }

    #[test]
    fn an_occurrence_opened_with_its_page_keeps_only_the_ranges_the_page_shows() {
        let mut record = paged_record(3);
        record.sentences[1].segments = vec![Segment(0, 1, 1, 0, 1), Segment(1, 1, 2, 0, 1)];
        let frame =
            zstd::bulk::compress(&serde_json::to_vec(&record).expect("JSON"), 1).expect("compress");
        let (_directory, sidecar) = record_fixture(&frame, 3);
        let sentence = SentenceId::new(1);
        let (page, occurrence) = sidecar.page_at(sentence, 0..2, 1).expect("page");
        assert_eq!(
            page,
            sidecar
                .page(PageRequest::Around(sentence), 1)
                .expect("page")
        );
        assert_eq!(page.blocks, 1..2);
        assert_eq!(
            occurrence,
            OccurrenceAnchor {
                document: DocumentId::new(0),
                block: 1,
                ranges: vec![HighlightRange {
                    container: 1,
                    utf16_start: 0,
                    utf16_end: 1
                }],
                exact: false,
            }
        );
        let (_, whole) = sidecar.page_at(sentence, 0..2, 1024).expect("page");
        assert!(whole.exact);
        assert_eq!(whole.ranges.len(), 2);
    }

    #[test]
    fn overflowing_document_dimensions_return_corruption_errors() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path();
        let key = ReadingSidecarKey {
            analysis_identity: "analysis".into(),
            source_identity: "source".into(),
            renderer_revision: MARKDOWN_RENDERER_REVISION.into(),
        };
        let mut header = Header {
            format: READING_SIDECAR_FORMAT.into(),
            version: READING_SIDECAR_VERSION,
            key: key.clone(),
            documents: 0,
            sentences: 0,
            records_bytes: 0,
            records_blake3: blake3::hash(&[]).to_hex().to_string(),
        };
        fs::write(path.join(DOCUMENT_OFFSETS), 0u64.to_le_bytes()).expect("offsets");
        fs::write(path.join(DOCUMENT_SENTENCES), 0u64.to_le_bytes()).expect("sentences");
        fs::write(path.join(RECORDS), []).expect("records");
        for documents in [0, u64::MAX, u64::MAX / 8, u64::MAX / 8 + 1] {
            header.documents = documents;
            fs::write(
                path.join(HEADER),
                serde_json::to_vec(&header).expect("JSON"),
            )
            .expect("header");
            let result = ReadingSidecar::open(path, &key);
            if documents == 0 {
                assert!(result.is_ok());
            } else {
                assert!(
                    matches!(result, Err(ReadingSidecarError::Corrupt(_))),
                    "documents = {documents}"
                );
            }
        }
    }

    #[test]
    fn alignment_maps_each_stored_character_to_its_rendered_character() {
        let rendered = characters("前の𝑥と<sup>2</sup>後", 3);
        let segments = align("前の𝑥と2後", &rendered);
        assert_eq!(
            segments,
            [
                Segment(0, 2, 3, 0, 2),
                Segment(2, 1, 3, 2, 2),
                Segment(3, 1, 3, 4, 1),
                Segment(4, 1, 3, 10, 1),
                Segment(5, 1, 3, 17, 1),
            ]
        );
        assert_eq!(
            highlight(&segments, &(1..3)),
            [
                HighlightRange {
                    container: 3,
                    utf16_start: 1,
                    utf16_end: 2
                },
                HighlightRange {
                    container: 3,
                    utf16_start: 2,
                    utf16_end: 4
                },
            ]
        );
        assert_eq!(covered(&segments, &(0..7)), 6);
    }

    #[hegel::test(test_cases = 200, derandomize = true)]
    fn a_page_stays_within_its_bound_unless_it_is_one_block(tc: hegel::TestCase) {
        use hegel::generators;
        let costs = tc.draw(
            generators::vecs(generators::integers::<usize>().min_value(1).max_value(400))
                .min_size(1)
                .max_size(40),
        );
        let bound = tc.draw(generators::integers::<usize>().max_value(1500));
        let start = tc.draw(generators::integers::<usize>().max_value(costs.len() - 1));
        let selection = match tc.draw(generators::integers::<u8>().max_value(2)) {
            0 => Selection::Around(start),
            1 => Selection::From(start),
            _ => Selection::Before(start + 1),
        };
        let blocks = select_blocks(&costs, selection, bound);
        let used: usize = costs[blocks.clone()].iter().sum();
        assert!(
            used <= bound || blocks.len() == 1,
            "{blocks:?} uses {used} of {bound}"
        );
        assert!(blocks.contains(&start), "{blocks:?} holds block {start}");
        match selection {
            Selection::From(_) => assert_eq!(blocks.start, start),
            Selection::Before(_) => assert_eq!(blocks.end, start + 1),
            Selection::Around(_) => {}
        }
        let grows_forward = !matches!(selection, Selection::Before(_))
            && blocks.end < costs.len()
            && used + costs[blocks.end] <= bound;
        let grows_backward = !matches!(selection, Selection::From(_))
            && blocks.start > 0
            && used + costs[blocks.start - 1] <= bound;
        assert!(!grows_forward && !grows_backward, "{blocks:?} is maximal");
    }

    #[test]
    fn assembled_regions_cost_no_more_than_their_blocks() {
        let record = DocumentRecord {
            source_name: "a.md".into(),
            regions: vec!["outer".into(), "inner".into()],
            blocks: vec![
                BlockRecord {
                    html: "<p>a</p>".into(),
                    regions: vec![0],
                },
                BlockRecord {
                    html: "<p>b</p>".into(),
                    regions: vec![0, 1],
                },
                BlockRecord {
                    html: "<p>c</p>".into(),
                    regions: vec![],
                },
            ],
            container_blocks: Vec::new(),
            sentences: Vec::new(),
        };
        let html = assemble(&record, 0..3);
        assert_eq!(
            html,
            "<div class=\"outer\" data-region=\"0\"><p>a</p><div class=\"inner\" data-region=\"1\"><p>b</p></div></div><p>c</p>"
        );
        let cost: usize = record
            .blocks
            .iter()
            .map(|block| block_cost(&record, block))
            .sum();
        assert!(html.len() <= cost);
    }
}
