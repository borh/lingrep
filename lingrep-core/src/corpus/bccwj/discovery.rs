//! BCCWJ distribution discovery and the entry→`CorpusEvent` lowering.
//!
//! [`BccwjInventory::discover`] validates the distribution root structurally
//! (a `C-XML/` tree and a `DOC/` context table) and enumerates:
//!
//! - the selected `*.zip` files under `C-XML/`, byte-sorted by root-relative
//!   logical name (deterministic document order across runs and platforms),
//!   with `sampling` derived from the `FIXED`/`VARIABLE` tree component and
//!   the subcorpus from the code directory; and
//! - the three `DOC/` context files, byte-sorted, as a distinct list. They are
//!   folded into the ordered inventory digest, so a metadata update changes the
//!   source identity.
//!
//! [`BccwEvents`] is the per-zip stream: it wraps the caller's already-open
//! `Read + Seek` handle in a [`ZipArchive`], iterates the file entries
//! byte-sorted by name, and lowers each parsed [`super::xml::BccwjDocument`]
//! to the `CorpusEvent` sequence — `DocumentStart` (metadata from the
//! [`BccwjMetadata`] join, typed by the single typing rule), then per
//! non-empty paragraph an empty-metadata `ParagraphStart`, each non-empty
//! sentence with the coded tag subset as `active_labels`, `ParagraphEnd`,
//! and finally `DocumentEnd`.
//!
//! A basename with no `Joined_info` row is an **error** carrying the zip and
//! the entry, not a diagnostic: the prepared build has nowhere to return an
//! advisory to, and an advisory there is a silently-degraded artifact that
//! then serves every later run.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use zip::ZipArchive;

use super::metadata::{BccwjBuildNotes, BccwjMetadata, Sampling, Subcorpus};
use super::xml::parse_entry;
use crate::corpus::TraversalLimits;
use crate::corpus::structured::{CorpusEvent, metadata_from_json};
use crate::corpus::zip::{ZipFileStat, zip_entries, zip_file_stat_from_metadata};

/// The `DOC/` context tables, byte-sorted (the order they enter the
/// inventory digest).
const CONTEXT_FILES: [&str; 3] = [
    "BCCWJ-NDC.txt",
    "CopyRight_Annotation.txt",
    "Joined_info.txt",
];

/// A selected zip file of the inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BccwjZip {
    /// The absolute path (what the seam opens).
    pub path: PathBuf,
    /// The root-relative logical name (`C-XML/FIXED/LB/LB.zip`) — the name
    /// the inventory observation and the ordered inventory digest carry.
    pub logical_name: String,
    /// From the `C-XML/{FIXED,VARIABLE}` path component (Decision D3).
    pub sampling: Sampling,
    /// From the code directory under the tree.
    pub subcorpus: Subcorpus,
}

/// The discovered BCCWJ distribution: the selected zips and the context
/// files, both in byte-sorted, reproducible order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BccwjInventory {
    pub zips: Vec<BccwjZip>,
    /// The `DOC/` context files, byte-sorted; part of the source identity.
    pub context_files: Vec<PathBuf>,
}

impl BccwjInventory {
    /// Validates the distribution root and builds the inventory, under the
    /// run's traversal limits.
    ///
    /// The walk is bounded exactly as the generic lexical discovery is:
    /// `max_depth` below `C-XML/`, `max_directory_entries` per directory,
    /// and `max_source_entries` / `max_documents` over the selected zips.
    /// A symlink back to an ancestor is refused rather than enumerated once
    /// per level. Two things secure that, and the depth bound is only one of
    /// them: a link is resolved with an explicit `metadata` call whose error
    /// propagates, because `is_dir()` reports a failed stat as "not a
    /// directory" and so ended the walk silently. Which one fires is a
    /// property of the platform's symlink budget — Linux permits 40 per
    /// resolution, so the depth bound is reached first, while macOS gives up
    /// at 16 with `ELOOP` (measured, not assumed). Both are named refusals;
    /// relying on the bound alone made the guarantee depend on that budget
    /// exceeding it. The non-guarantee about cycles *inside* the bound is
    /// unchanged, and matches the generic walker.
    ///
    /// # Errors
    ///
    /// A missing `C-XML/` or `DOC/` directory, a missing context table, a
    /// zip outside `C-XML/{FIXED,VARIABLE}/<SUB>/` (naming the path), an
    /// exceeded traversal limit (naming the limit and the path), or an
    /// empty zip selection.
    pub fn discover(root: &Path, limits: &TraversalLimits) -> io::Result<Self> {
        let cxml = root.join("C-XML");
        if !cxml.is_dir() {
            return Err(missing(&cxml));
        }
        let doc = root.join("DOC");
        if !doc.is_dir() {
            return Err(missing(&doc));
        }
        let context_files = CONTEXT_FILES
            .iter()
            .map(|name| {
                let path = doc.join(name);
                if path.is_file() {
                    Ok(path)
                } else {
                    Err(missing(&path))
                }
            })
            .collect::<io::Result<Vec<_>>>()?;

        let mut zips = Vec::new();
        collect_zips(root, &cxml, limits, 1, &mut zips)?;
        zips.sort_by(|left, right| {
            left.logical_name
                .as_bytes()
                .cmp(right.logical_name.as_bytes())
        });
        if zips.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no *.zip files under {}", cxml.display()),
            ));
        }
        Ok(Self {
            zips,
            context_files,
        })
    }
}

fn missing(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("bccwj distribution is missing {}", path.display()),
    )
}

/// One exceeded traversal limit, naming the limit, its value and the path
/// that crossed it.
fn limit_exceeded(field: &str, limit: u64, observed: u64, path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "bccwj distribution exceeds {field} ({limit}) at {} (observed {observed})",
            path.display()
        ),
    )
}

fn collect_zips(
    root: &Path,
    dir: &Path,
    limits: &TraversalLimits,
    depth: usize,
    out: &mut Vec<BccwjZip>,
) -> io::Result<()> {
    if depth > limits.max_depth {
        return Err(limit_exceeded(
            "max_depth",
            limits.max_depth as u64,
            depth as u64,
            dir,
        ));
    }
    let mut directory_entries = 0_usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        directory_entries += 1;
        if directory_entries > limits.max_directory_entries {
            return Err(limit_exceeded(
                "max_directory_entries",
                limits.max_directory_entries as u64,
                directory_entries as u64,
                dir,
            ));
        }
        // `Path::is_dir()` returns `false` both for a regular file and for a
        // failed stat call, so a symlink cycle would truncate the walk without
        // reporting an error. Resolve a link explicitly and let the error propagate;
        // an ordinary entry keeps the cheap readdir answer and never stats.
        let file_type = entry.file_type()?;
        let is_directory = if file_type.is_symlink() {
            fs::metadata(&path)?.is_dir()
        } else {
            file_type.is_dir()
        };
        if is_directory {
            collect_zips(root, &path, limits, depth + 1, out)?;
        } else if path.extension().is_some_and(|ext| ext == "zip") {
            let relative = path.strip_prefix(root).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "bccwj zip path {} is not under the root: {err}",
                        path.display()
                    ),
                )
            })?;
            let components: Vec<&str> = relative
                .components()
                .map(|component| {
                    component.as_os_str().to_str().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("non-UTF-8 bccwj zip path {}", path.display()),
                        )
                    })
                })
                .collect::<io::Result<Vec<_>>>()?;
            // C-XML/<TREE>/<SUB>/… : the tree picks the sampling, the first
            // code directory picks the subcorpus.
            if components.len() < 3 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "bccwj zip {} must live under C-XML/{{FIXED,VARIABLE}}/<SUB>/",
                        path.display()
                    ),
                ));
            }
            let sampling = match components[1] {
                "FIXED" => Sampling::Fixed,
                "VARIABLE" => Sampling::Variable,
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "bccwj zip {} is under unknown tree {other:?}; expected FIXED or VARIABLE",
                            path.display()
                        ),
                    ));
                }
            };
            let subcorpus = Subcorpus::from_code(components[2]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "bccwj zip {} is under unknown subcorpus directory {:?}",
                        path.display(),
                        components[2]
                    ),
                )
            })?;
            // A zip is both a source entry and a discovered document, so
            // both bounds apply; the error names whichever one bit.
            let selected = out.len() as u64 + 1;
            let (field, zip_limit) = if limits.max_source_entries <= limits.max_documents {
                ("max_source_entries", limits.max_source_entries)
            } else {
                ("max_documents", limits.max_documents)
            };
            if selected > zip_limit {
                return Err(limit_exceeded(field, zip_limit, selected, &path));
            }
            out.push(BccwjZip {
                path: path.clone(),
                logical_name: relative.to_string_lossy().into_owned(),
                sampling,
                subcorpus,
            });
        }
    }
    Ok(())
}

/// The lazily-built per-stream metadata table (with its recoverable build
/// notes). One lock per stream; every zip's [`BccwEvents`] shares it, and
/// the first zip triggers the build.
pub type BccwjMetadataLock = Arc<OnceLock<Result<(BccwjMetadata, BccwjBuildNotes), String>>>;

/// The per-zip `CorpusEvent` stream over a caller-owned handle.
///
/// It opens nothing and never reopens: the seam drains the file for its
/// content observation first, records
/// [`zip_file_stat`](crate::corpus::zip::zip_file_stat), rewinds, and hands
/// the same handle over. When the last entry is exhausted the stream re-
/// stats the path and fails the run if the file changed, so the bytes that
/// were hashed are the bytes that were parsed.
///
/// The re-stat is an end-of-stream check: a run that truncates the stream
/// early (a match `--limit`, cancellation, or an earlier error) never
/// reaches it. That is an accepted residual, not a gap to close in the
/// stream: the identity observation already drained the *whole* file
/// before the parse, so the source identity always covers the bytes that
/// were on disk at drain time, and the truncated tail of a concurrent swap
/// can only affect events the run stopped consuming.
pub struct BccwEvents {
    zip: ZipArchive<BufReader<File>>,
    /// The file entries as `(name, index)`, byte-sorted by name.
    entries: Vec<(String, usize)>,
    position: usize,
    pending: VecDeque<CorpusEvent>,
    sampling: Sampling,
    root: PathBuf,
    /// The zip's path — used only for the end-of-stream re-stat.
    path: PathBuf,
    metadata: BccwjMetadataLock,
    zip_name: String,
    integrity: Option<ZipFileStat>,
    /// The run's `max_input_record_bytes`, applied per entry: one C-XML
    /// entry is one input record, exactly as one JSONL line is.
    max_entry_bytes: usize,
}

impl BccwEvents {
    /// Wraps an already-open handle over one inventory zip.
    ///
    /// `max_entry_bytes` is the run's `max_input_record_bytes`; an entry
    /// whose *declared* uncompressed size exceeds it is refused before a
    /// byte is decompressed, and the read itself is bounded so an entry that
    /// under-declares its size cannot outgrow the limit either.
    ///
    /// The inventory `zip` is the one identifier: its path, logical name and
    /// sampling travel together, so no caller can pair one zip's path with
    /// another's name or window.
    ///
    /// # Errors
    ///
    /// An unreadable ZIP container, undecodable entry names, or duplicate
    /// decoded entry names.
    pub fn new(
        file: File,
        entry: &BccwjZip,
        root: &Path,
        metadata: BccwjMetadataLock,
        integrity: ZipFileStat,
        max_entry_bytes: usize,
    ) -> io::Result<Self> {
        let zip_name = entry.logical_name.as_str();
        let zip = ZipArchive::new(BufReader::new(file)).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bccwj zip {zip_name}: {err}"),
            )
        })?;
        let entries = zip_entries(&zip)?;
        Ok(Self {
            zip,
            entries,
            position: 0,
            pending: VecDeque::new(),
            sampling: entry.sampling,
            root: root.to_path_buf(),
            path: entry.path.clone(),
            metadata,
            zip_name: zip_name.to_owned(),
            integrity: Some(integrity),
            max_entry_bytes,
        })
    }
}

impl Iterator for BccwEvents {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }
        if self.position >= self.entries.len() {
            return self.verify_and_finish();
        }
        let (name, index) = self.entries[self.position].clone();
        self.position += 1;
        Some(self.load_document(&name, index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // One document is at least two events (Start/End); the exact count
        // is not knowable before the entries parse.
        (
            self.entries
                .len()
                .saturating_sub(self.position)
                .saturating_mul(2),
            None,
        )
    }
}

impl BccwEvents {
    /// After the last entry: re-stat the zip and fail the run if it is not
    /// the file that was hashed — a replaced or edited distribution member
    /// would otherwise be parsed from one byte sequence and named from
    /// another.
    fn verify_and_finish(&mut self) -> Option<io::Result<CorpusEvent>> {
        let before = self.integrity.take()?;
        let after = match fs::metadata(&self.path) {
            Ok(metadata) => zip_file_stat_from_metadata(&metadata),
            Err(error) => return Some(Err(error)),
        };
        if after != before {
            return Some(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bccwj zip {} changed during the run; refusing to publish",
                    self.zip_name
                ),
            )));
        }
        None
    }

    fn table(&self) -> io::Result<&(BccwjMetadata, BccwjBuildNotes)> {
        self.metadata
            .get_or_init(|| BccwjMetadata::build(&self.root).map_err(|error| error.to_string()))
            .as_ref()
            .map_err(|message| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("bccwj metadata: {message}"),
                )
            })
    }

    fn load_document(&mut self, name: &str, index: usize) -> io::Result<CorpusEvent> {
        let context = |message: String| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bccwj zip {}: entry {}: {message}", self.zip_name, name),
            )
        };
        let basename = basename_from_entry(name);
        let (table, _) = self.table()?;
        let doc_metadata = table
            .lookup(&basename)
            .map_err(|_| context(format!("has no Joined_info row (basename {basename})")))?
            .clone();
        let limit = self.max_entry_bytes;
        let oversized = |observed: &str| {
            context(format!(
                "is {observed} bytes, over the {limit}-byte input-record limit; raise --max-record-bytes to read it"
            ))
        };
        let mut entry = self
            .zip
            .by_index(index)
            .map_err(|err| context(format!("unreadable zip entry: {err}")))?;
        // The declared uncompressed size refuses an oversized entry — and a
        // decompression bomb — before anything is inflated. The bounded read
        // then covers an entry that under-declares its size.
        let declared = entry.size();
        if declared > limit as u64 {
            return Err(oversized(&declared.to_string()));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
        entry
            .by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| context(format!("unreadable zip entry: {err}")))?;
        if bytes.len() > limit {
            return Err(oversized(&format!("more than {limit}")));
        }
        drop(entry);
        let document =
            parse_entry(&bytes, doc_metadata.subcorpus).map_err(|err| context(err.to_string()))?;
        let metadata = metadata_from_json(
            doc_metadata.to_json(self.sampling),
            &format!("bccwj {name}"),
        )
        .map_err(|err| context(err.to_string()))?;
        let mut events: Vec<CorpusEvent> = Vec::with_capacity(2 + document.paragraphs.len() * 2);
        events.push(CorpusEvent::DocumentStart {
            source_name: name.to_owned(),
            metadata,
        });
        events.push(CorpusEvent::DivisionObservation {
            kind: "paragraph".into(),
            observation: crate::corpus::DivisionObservation::Observed,
        });
        for paragraph in &document.paragraphs {
            // D2: the paragraph tags ride the sentences as active_labels,
            // not the paragraph metadata, which stays empty.
            events.push(CorpusEvent::ParagraphStart {
                metadata: Box::new([]),
            });
            for sentence in &paragraph.sentences {
                events.push(CorpusEvent::Sentence {
                    text: sentence.clone(),
                    words: None,
                    enhanced_graph: None,
                    active_labels: coded_labels(&paragraph.tags),
                    metadata: Box::new([]),
                });
            }
            events.push(CorpusEvent::ParagraphEnd);
        }
        events.push(CorpusEvent::DocumentEnd);
        self.pending.extend(events);
        Ok(self
            .pending
            .pop_front()
            .expect("the document events were just queued"))
    }
}

/// The reference's coded-tag set (after the `titleBlock`/`orphanedTitle`
/// rename) — the D2 projection of a paragraph's recorded tag set onto the
/// sentence `active_labels`.
const CODED_TAGS: [&str; 9] = [
    "OCAnswer",
    "OCQuestion",
    "caption",
    "citation",
    "list",
    "quotation",
    "speech",
    "speaker",
    "title",
];

fn coded_labels(tags: &[String]) -> Box<[String]> {
    tags.iter()
        .filter(|tag| CODED_TAGS.contains(&tag.as_str()))
        .cloned()
        .collect()
}

/// The reference's basename rule: the entry filename minus extension, cut
/// at the first `-` (pinned by the differential, spec §3.4).
#[must_use]
pub fn basename_from_entry(entry: &str) -> String {
    let file = entry.rsplit('/').next().unwrap_or(entry);
    let stem = file.split_once('.').map_or(file, |(stem, _)| stem);
    stem.split_once('-')
        .map_or(stem, |(base, _)| base)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::structured::{CorpusEvent, MetadataValue};
    use crate::corpus::zip::zip_file_stat;
    use std::io::Write;

    /// Writes a synthetic distribution (all text self-authored, §2) with the
    /// given zips (logical name → entry name/bytes pairs) and returns the root.
    fn distribution(zips: &[(&str, &[(&str, &str)])], joined: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let doc = dir.path().join("DOC");
        std::fs::create_dir_all(&doc).expect("doc");
        std::fs::write(doc.join("Joined_info.txt"), joined).expect("joined");
        std::fs::write(doc.join("BCCWJ-NDC.txt"), "a\tb\n").expect("ndc");
        std::fs::write(doc.join("CopyRight_Annotation.txt"), "a\tb\n").expect("copyright");
        for &(logical, entries) in zips {
            let path = dir.path().join(logical);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("zip parent");
            }
            let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(
                std::fs::File::create(&path).expect("zip file"),
            ));
            for (entry, content) in entries {
                writer
                    .start_file(entry.to_owned(), zip::write::SimpleFileOptions::default())
                    .expect("start entry");
                writer.write_all(content.as_bytes()).expect("entry");
            }
            writer.finish().expect("finish zip");
        }
        dir
    }

    const JOINED: &str = "a	b	c	d	e	f	g	h	i	j	k	l	m	n	o	p	q	r	s	t
one	bib	題		vol	resp	社	2000	isbn	page					rid	pid	著			LB
";

    #[test]
    fn discovery_validates_the_root_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = BccwjInventory::discover(dir.path(), &TraversalLimits::default())
            .expect_err("no C-XML");
        assert!(error.to_string().contains("C-XML"), "{error}");

        let dir = tempfile::tempdir().expect("tempdir");
        let doc = dir.path().join("DOC");
        std::fs::create_dir_all(dir.path().join("C-XML/FIXED/LB")).expect("tree");
        std::fs::create_dir_all(&doc).expect("doc");
        std::fs::write(doc.join("Joined_info.txt"), JOINED).expect("joined");
        std::fs::write(doc.join("BCCWJ-NDC.txt"), "a\tb\n").expect("ndc");
        std::fs::write(doc.join("CopyRight_Annotation.txt"), "a\tb\n").expect("copyright");
        let error = BccwjInventory::discover(dir.path(), &TraversalLimits::default())
            .expect_err("empty selection");
        assert!(error.to_string().contains("no *.zip"), "{error}");
    }

    #[test]
    fn discovery_lists_zips_byte_sorted_with_sampling_and_subcorpus() {
        let dist = distribution(
            &[
                ("C-XML/VARIABLE/LB/LB.zip", &[("LB/a.xml", "<article/>")]),
                ("C-XML/FIXED/PN/PN.zip", &[("PNb/b.xml", "<article/>")]),
                ("C-XML/FIXED/LB/LB.zip", &[("LBa/a.xml", "<article/>")]),
                ("C-XML/VARIABLE/OY/OY.zip", &[("OY/c.xml", "<article/>")]),
            ],
            JOINED,
        );
        let inventory =
            BccwjInventory::discover(dist.path(), &TraversalLimits::default()).expect("discover");
        assert_eq!(
            inventory
                .zips
                .iter()
                .map(|zip| zip.logical_name.as_str())
                .collect::<Vec<_>>(),
            [
                "C-XML/FIXED/LB/LB.zip",
                "C-XML/FIXED/PN/PN.zip",
                "C-XML/VARIABLE/LB/LB.zip",
                "C-XML/VARIABLE/OY/OY.zip",
            ]
        );
        assert_eq!(inventory.zips[0].sampling, Sampling::Fixed);
        assert_eq!(inventory.zips[0].subcorpus, Subcorpus::LB);
        assert_eq!(inventory.zips[2].sampling, Sampling::Variable);
        assert_eq!(inventory.zips[3].subcorpus, Subcorpus::OY);
        // The context files, byte-sorted, as a distinct identity list.
        assert_eq!(
            inventory
                .context_files
                .iter()
                .map(|path| path
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned())
                .collect::<Vec<_>>(),
            [
                "BCCWJ-NDC.txt",
                "CopyRight_Annotation.txt",
                "Joined_info.txt"
            ]
        );
    }

    #[test]
    fn discovery_rejects_zips_outside_the_tree_layout() {
        let dist = distribution(
            &[("C-XML/FIXED/ZZ/ZZ.zip", &[("x.xml", "<article/>")])],
            JOINED,
        );
        let error = BccwjInventory::discover(dist.path(), &TraversalLimits::default())
            .expect_err("unknown subcorpus");
        assert!(error.to_string().contains("ZZ"), "{error}");

        let dist = distribution(
            &[("C-XML/OTHER/LB/LB.zip", &[("x.xml", "<article/>")])],
            JOINED,
        );
        let error = BccwjInventory::discover(dist.path(), &TraversalLimits::default())
            .expect_err("unknown tree");
        assert!(error.to_string().contains("OTHER"), "{error}");

        let dist = distribution(&[("C-XML/LB.zip", &[] as &[(&str, &str)])], JOINED);
        let error = BccwjInventory::discover(dist.path(), &TraversalLimits::default())
            .expect_err("too shallow");
        assert!(error.to_string().contains("C-XML"), "{error}");
    }

    fn open_events(dist: &Path, logical: &str) -> BccwEvents {
        let inventory =
            BccwjInventory::discover(dist, &TraversalLimits::default()).expect("discover");
        let zip = inventory
            .zips
            .iter()
            .find(|zip| zip.logical_name == logical)
            .expect("zip");
        let file = fs::File::open(&zip.path).expect("open");
        let integrity = zip_file_stat(&file).expect("stat");
        BccwEvents::new(
            file,
            zip,
            dist,
            Arc::new(OnceLock::new()),
            integrity,
            crate::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        )
        .expect("events")
    }

    fn events(dist: &Path, logical: &str) -> Vec<CorpusEvent> {
        open_events(dist, logical)
            .collect::<io::Result<Vec<_>>>()
            .expect("stream")
    }

    #[test]
    fn events_lower_the_document_shape() {
        let xml = "<article><speech><paragraph><sentence><orth>引用文。</orth></sentence></paragraph></speech></article>";
        let dist = distribution(
            &[("C-XML/FIXED/LB/LB.zip", &[("LBa/one.xml", xml)])],
            JOINED,
        );
        let events = events(dist.path(), "C-XML/FIXED/LB/LB.zip");
        // The paragraph's full tag set is {article, paragraph, speech}; the
        // D2 projection keeps only the coded subset — `speech` — dropping
        // the uncoded `article` and `paragraph`.
        assert_eq!(
            events,
            vec![
                CorpusEvent::DocumentStart {
                    source_name: "LBa/one.xml".to_owned(),
                    metadata: row_metadata("fixed"),
                },
                CorpusEvent::DivisionObservation {
                    kind: "paragraph".into(),
                    observation: crate::corpus::DivisionObservation::Observed
                },
                CorpusEvent::ParagraphStart {
                    metadata: Box::new([]),
                },
                CorpusEvent::Sentence {
                    text: "引用文。".to_owned(),
                    words: None,
                    enhanced_graph: None,
                    active_labels: ["speech"].map(String::from).into(),
                    metadata: Box::new([]),
                },
                CorpusEvent::ParagraphEnd,
                CorpusEvent::DocumentEnd,
            ]
        );
    }

    /// The metadata the synthetic `one` row derives: the exact object
    /// `DocMetadata::to_json` emits for it, typed by the single typing rule.
    fn row_metadata(sampling: &str) -> crate::corpus::structured::Metadata {
        let object = serde_json::json!({
            "title": "題",
            "author": "著",
            "publisher": "社",
            "year": 2000,
            "basename": "one",
            "corpus": "BCCWJ",
            "subcorpus": "書籍",
            "category": ["書籍"],
            "genre": ["書籍"],
            "permission": true,
            "sampling": sampling,
        });
        metadata_from_json(object, "test").expect("metadata")
    }

    #[test]
    fn events_report_the_metadata_fields() {
        let xml =
            "<article><paragraph><sentence><orth>文。</orth></sentence></paragraph></article>";
        let dist = distribution(
            &[("C-XML/VARIABLE/OY/OY.zip", &[("OY/one.xml", xml)])],
            JOINED,
        );
        let events = events(dist.path(), "C-XML/VARIABLE/OY/OY.zip");
        let CorpusEvent::DocumentStart { metadata, .. } = &events[0] else {
            panic!("first event is not a DocumentStart");
        };
        let get = |key: &str| {
            metadata
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)
        };
        assert_eq!(
            get("title"),
            Some(&MetadataValue::Scalar(
                crate::corpus::structured::MetadataScalar::String("題".to_owned())
            ))
        );
        assert_eq!(
            get("sampling"),
            Some(&MetadataValue::Scalar(
                crate::corpus::structured::MetadataScalar::String("variable".to_owned())
            ))
        );
        assert_eq!(
            get("subcorpus"),
            Some(&MetadataValue::Scalar(
                crate::corpus::structured::MetadataScalar::String("書籍".to_owned())
            ))
        );
        assert_eq!(
            get("permission"),
            Some(&MetadataValue::Scalar(
                crate::corpus::structured::MetadataScalar::Bool(true)
            ))
        );
    }

    #[test]
    fn a_missing_basename_fails_the_stream_naming_zip_and_entry() {
        let xml =
            "<article><paragraph><sentence><orth>文。</orth></sentence></paragraph></article>";
        // The row's basename is `one`; the entry yields basename `two`.
        let dist = distribution(
            &[("C-XML/FIXED/LB/LB.zip", &[("LBa/two.xml", xml)])],
            JOINED,
        );
        let error: io::Error = open_events(dist.path(), "C-XML/FIXED/LB/LB.zip")
            .next()
            .expect("an event")
            .expect_err("missing basename");
        let message = error.to_string();
        assert!(message.contains("C-XML/FIXED/LB/LB.zip"), "{message}");
        assert!(message.contains("LBa/two.xml"), "{message}");
        assert!(message.contains("two"), "{message}");
    }

    #[test]
    fn malformed_entries_fail_the_stream() {
        let dist = distribution(
            &[(
                "C-XML/FIXED/LB/LB.zip",
                &[("LBa/one.xml", "<article><paragraph>")],
            )],
            JOINED,
        );
        let error: io::Error = open_events(dist.path(), "C-XML/FIXED/LB/LB.zip")
            .next()
            .expect("an event")
            .expect_err("unterminated");
        assert!(error.to_string().contains("<paragraph>"), "{error}");
    }

    #[test]
    fn the_basename_rule_cuts_at_the_first_dash() {
        assert_eq!(basename_from_entry("LBa/LBa0_00002.xml"), "LBa0_00002");
        assert_eq!(basename_from_entry("LB/LBa0_00002.xml"), "LBa0_00002");
        assert_eq!(basename_from_entry("a/b-1.xml"), "b");
        assert_eq!(basename_from_entry("x-1-2.xml"), "x");
    }

    #[test]
    fn a_zip_changed_during_the_run_fails_at_stream_end() {
        let xml =
            "<article><paragraph><sentence><orth>文。</orth></sentence></paragraph></article>";
        let dist = distribution(
            &[("C-XML/FIXED/LB/LB.zip", &[("LBa/one.xml", xml)])],
            JOINED,
        );
        let inventory =
            BccwjInventory::discover(dist.path(), &TraversalLimits::default()).expect("discover");
        let zip = &inventory.zips[0];
        let file = fs::File::open(&zip.path).expect("open");
        let integrity = zip_file_stat(&file).expect("stat");
        let mut events = BccwEvents::new(
            file,
            zip,
            dist.path(),
            Arc::new(OnceLock::new()),
            integrity,
            crate::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        )
        .expect("events");
        // The first event parses fine; the file changes before the stream
        // finishes, and the end-of-stream re-stat must catch it.
        assert!(events.next().expect("start").is_ok());
        let path = zip.path.clone();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    // Grow the file in place: same inode, new length and mtime.
                    let mut handle = fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .expect("append");
                    std::io::Write::write_all(&mut handle, b"\n").expect("grow");
                })
                .join()
                .expect("writer");
        });
        let error = events
            .find_map(std::result::Result::err)
            .expect("the stream must fail");
        let message = error.to_string();
        assert!(message.contains("C-XML/FIXED/LB/LB.zip"), "{message}");
        assert!(message.contains("changed during the run"), "{message}");
    }

    #[test]
    fn an_untouched_zip_streams_to_a_clean_eof() {
        let xml =
            "<article><paragraph><sentence><orth>文。</orth></sentence></paragraph></article>";
        let dist = distribution(
            &[("C-XML/FIXED/LB/LB.zip", &[("LBa/one.xml", xml)])],
            JOINED,
        );
        let events = open_events(dist.path(), "C-XML/FIXED/LB/LB.zip");
        assert_eq!(
            events.count(),
            6,
            "start, observation, para start, sentence, para end, doc end"
        );
    }

    /// A symlink back to an ancestor makes `is_dir()` descend indefinitely,
    /// which would admit one zip file under many logical names. The guarantee
    /// is that discovery is *rejected*; which mechanism rejects it is a
    /// property of the platform's symlink budget, so this asserts rejection
    /// and admits either mechanism.
    ///
    /// Linux permits 40 symlinks per resolution and the bound is 64 levels at
    /// three per cycle, so the depth bound is reached first. macOS stops at
    /// 16 with `ELOOP`, well inside the bound — which is why asserting
    /// `max_depth` alone described one platform rather than the contract, and
    /// why the walk must propagate the resolution error rather than read it
    /// as "not a directory".
    #[test]
    fn a_symlink_cycle_is_rejected_rather_than_silently_truncated() {
        let dist = distribution(
            &[("C-XML/FIXED/LB/LB.zip", &[("LBa/one.xml", "<article/>")])],
            JOINED,
        );
        std::os::unix::fs::symlink(
            dist.path().join("C-XML"),
            dist.path().join("C-XML/FIXED/LB/loop"),
        )
        .expect("symlink");
        let error = BccwjInventory::discover(dist.path(), &TraversalLimits::default())
            .expect_err("a symlink cycle must be refused, never partially admitted");
        let message = error.to_string();
        assert!(
            message.contains("max_depth") || error.raw_os_error() == Some(libc::ELOOP),
            "expected a depth-bound or ELOOP refusal, got: {message}"
        );
    }

    #[test]
    fn the_walk_honors_the_configured_traversal_limits() {
        let dist = distribution(
            &[
                ("C-XML/FIXED/LB/LB.zip", &[("LBa/one.xml", "<article/>")]),
                ("C-XML/FIXED/PN/PN.zip", &[("PNb/one.xml", "<article/>")]),
            ],
            JOINED,
        );
        let shallow = TraversalLimits {
            max_depth: 2,
            ..TraversalLimits::default()
        };
        let error = BccwjInventory::discover(dist.path(), &shallow).expect_err("depth-bounded");
        assert!(error.to_string().contains("max_depth"), "{error}");

        let one_zip = TraversalLimits {
            max_source_entries: 1,
            ..TraversalLimits::default()
        };
        let error = BccwjInventory::discover(dist.path(), &one_zip).expect_err("selection-bounded");
        assert!(error.to_string().contains("max_source_entries"), "{error}");

        // The same distribution is fine under the defaults.
        let inventory =
            BccwjInventory::discover(dist.path(), &TraversalLimits::default()).expect("discover");
        assert_eq!(inventory.zips.len(), 2);
    }

    #[test]
    fn an_oversized_entry_is_refused_before_it_is_inflated() {
        // The entry compresses well, so a byte-count bound applied after
        // decompression is the only thing standing between a hostile
        // archive and an unbounded allocation.
        let body: String = std::iter::repeat_n(
            "<paragraph><sentence><orth>文。</orth></sentence></paragraph>",
            2_000,
        )
        .collect();
        let xml = format!("<article>{body}</article>");
        let dist = distribution(
            &[("C-XML/FIXED/LB/LB.zip", &[("LBa/one.xml", xml.as_str())])],
            JOINED,
        );
        let inventory =
            BccwjInventory::discover(dist.path(), &TraversalLimits::default()).expect("discover");
        let zip = &inventory.zips[0];
        let file = fs::File::open(&zip.path).expect("open");
        let integrity = zip_file_stat(&file).expect("stat");
        let mut events = BccwEvents::new(
            file,
            zip,
            dist.path(),
            Arc::new(OnceLock::new()),
            integrity,
            1_024,
        )
        .expect("events");
        let error = events
            .next()
            .expect("an event")
            .expect_err("over the record limit");
        let message = error.to_string();
        assert!(message.contains("LBa/one.xml"), "{message}");
        assert!(message.contains("--max-record-bytes"), "{message}");

        // The same entry streams under a limit that admits it.
        let file = fs::File::open(&zip.path).expect("open");
        let integrity = zip_file_stat(&file).expect("stat");
        let events = BccwEvents::new(
            file,
            zip,
            dist.path(),
            Arc::new(OnceLock::new()),
            integrity,
            crate::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        )
        .expect("events");
        assert!(events.count() > 5);
    }
}
