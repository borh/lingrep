//! The immutable filesystem corpus source shared by command-line and web front
//! ends.
//!
//! [`CorpusSource`] discovers a corpus on disk (a single file or a recursively
//! traversed Markdown/CoNLL-U directory) and streams it once or many times as plain sentences
//! ([`CorpusStream`]) or structured admitted events
//! ([`CorpusStructuredStream`]). Each stream opens one file at a time and wraps
//! per-file failures with the offending path, so a malformed document in a
//! directory is attributable.
//!
//! The source carries configuration only — never observed state — so it is
//! `Clone` and a caller may stream it repeatedly. Per-stream diagnostics
//! (observed Markdown labels) live in the stream, not the source, and for a
//! Markdown source one shared [`MarkdownDiagnostics`] sink accumulates labels
//! across every selected file rather than resetting per file.
//!
//! This module deliberately knows nothing about the on-disk token cache or the
//! provenance database; those concerns stay with the web runtime that owns the
//! cache lifecycle.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use super::discovery::{CorpusDiscovery, CorpusFileKind, TraversalLimits};
use super::markdown_dialect::{MarkdownDiagnostics, MarkdownReaderConfig};
use super::structured::{AdmittedCorpus, AdmittedCorpusEvent, CorpusEvent};
use super::text_source::{InputFormat, stream_input_events};
use clap::ValueEnum;

/// Fixed reader-configuration tag for the plain-text stream. See
/// [`CorpusReadConfig::reader_tag`].
const PLAIN_CONFIG_TAG: u64 = 0xfb3a_e7c1_d4f0_06a3u64;
/// Fixed reader-configuration tag for the JSONL stream. See
/// [`CorpusReadConfig::reader_tag`].
const JSONL_CONFIG_TAG: u64 = 0x2c8b_1f5d_9e7a_04c5u64;
/// Fixed reader tag for CoNLL-U with explicit metadata ownership and unavailable
/// paragraph structure. Comment consumption remains unconditional.
const CONLLU_CONFIG_TAG: u64 = 0x94d2_6c1e_58af_43b9u64;
/// Fixed reader-configuration tag for the BCCWJ stream. Configuration only
/// — the reader carries no tunable policy, so the tag is a constant. It is
/// one of the only BCCWJ-derived bytes in the V1 artifact identity (§3.1).
const BCCWJ_CONFIG_TAG: u64 = 0x7d1c_9e4a_b3f2_8c6eu64;
/// Fixed reader-configuration tag for a Soranoha bulk archive. Configuration
/// only — the catalog policy is the format's, with no tunable knob.
const SORANOHA_CONFIG_TAG: u64 = 0x4e7a_b51c_0d93_6f28u64;

/// Resolve the corpus format tag for a CLI input path, purely from the
/// extension and an explicit override — never opening or stat-ing the path.
///
/// This is the shared inference rule for every front end's `--input` /
/// `--input-format` pair. It returns only a [`CorpusFormat`] tag (never a
/// `Result`): a caller converts the tag into a policy-bearing
/// [`CorpusReadConfig`] via [`CorpusReadConfig::new`] before
/// [`CorpusSource::discover`] opens anything.
///
/// # Rules
/// - `override_format` wins if `Some` — an explicit `--input-format` always
///   takes precedence. Directory admission is validated later by
///   [`CorpusSource::discover`].
/// - `None` path (stdin) → [`CorpusFormat::Plain`].
/// - `.md` / `.markdown` extension (case-insensitive) →
///   [`CorpusFormat::Markdown`].
/// - `.jsonl` extension (case-insensitive) → [`CorpusFormat::Jsonl`].
/// - A path with **no** file extension → [`CorpusFormat::Markdown`]: by spec a
///   directory means sorted top-level regular `.md` files only, and the
///   resolver is pure, so it infers "no extension" as "directory-ish" rather
///   than calling `is_dir()`. Any *other* extension falls through to
///   [`CorpusFormat::Plain`].
///
/// A `.zip` is **not** inferred as [`CorpusFormat::Soranoha`]: a ZIP is a
/// container other formats also use, and a catalog-bearing archive is the
/// caller's claim about what is inside, not something an extension proves.
/// An archive is declared with `--input-format soranoha`.
///
/// # Non-fallibility
/// The resolver inspects only the extension and the override — it never opens
/// the path, never stats it, and never returns an error. A mismatched
/// directory + override is reported later by [`CorpusSource::discover`], not
/// here.
#[must_use]
pub fn resolve_input_format(
    path: Option<&Path>,
    override_format: Option<CorpusFormat>,
) -> CorpusFormat {
    if let Some(format) = override_format {
        return format;
    }
    let Some(path) = path else {
        return CorpusFormat::Plain;
    };
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("jsonl") => CorpusFormat::Jsonl,
        Some(extension) if extension.eq_ignore_ascii_case("conllu") => CorpusFormat::Conllu,
        Some(extension)
            if extension.eq_ignore_ascii_case("md")
                || extension.eq_ignore_ascii_case("markdown") =>
        {
            CorpusFormat::Markdown
        }
        Some(_) => CorpusFormat::Plain,
        None => CorpusFormat::Markdown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CorpusFormat {
    Plain,
    Jsonl,
    Markdown,
    Conllu,
    Bccwj,
    Soranoha,
    SoranohaTei,
    Tei,
}

impl CorpusFormat {
    /// Whether this format can emit labels that answer a section scope.
    ///
    /// Keep this decision with the format catalog so every query surface gains
    /// the same answer when another built-in file format is added.
    #[must_use]
    pub const fn supports_section_labels(self) -> bool {
        match self {
            Self::Plain
            | Self::Jsonl
            | Self::Conllu
            | Self::Soranoha
            | Self::SoranohaTei
            | Self::Tei => false,
            Self::Markdown | Self::Bccwj => true,
        }
    }

    /// The metadata keys this format carries at sentence grain.
    ///
    /// Static per format, not observed per file: a `--where` predicate over a
    /// declared key is answered per sentence even when a particular file never
    /// carries the comment, so the direct and prepared paths report the same
    /// grain for the same clause. A prepared artifact records this same set at
    /// build time; JSONL additionally declares keys per document.
    #[must_use]
    pub const fn sentence_metadata_keys(self) -> &'static [&'static str] {
        match self {
            Self::Conllu => &super::conllu::SENTENCE_METADATA_KEYS,
            Self::Plain
            | Self::Jsonl
            | Self::Markdown
            | Self::Bccwj
            | Self::Soranoha
            | Self::SoranohaTei
            | Self::Tei => &[],
        }
    }
}

/// The resolved reader configuration for a source: which format to read, and —
/// for Markdown — how to read it. Configuration only; a reusable value can
/// stream many sources without carrying observed state between them.
///
/// Two members are not [`InputFormat`]s. `Bccwj` is a distribution root rather
/// than a file, and `Soranoha` is one archive whose reader needs a seekable
/// handle rather than a `BufRead`. File-only consumers refuse both. A future
/// split of this enum should follow the handle each reader needs: only BCCWJ
/// is "not a file", so a file/distribution split would not separate the two.
#[derive(Debug, Clone)]
pub enum CorpusReadConfig {
    Plain,
    Jsonl,
    Markdown(MarkdownReaderConfig),
    Conllu,
    /// Configuration only — the distribution root is a `CorpusSource`
    /// concern, never part of the read config.
    Bccwj,
    /// A Soranoha bulk archive. Not an [`InputFormat`]: its reader seeks
    /// inside one file to reach its members, where every [`InputFormat`]
    /// reader consumes one `BufRead` stream.
    Soranoha,
    SoranohaTei,
    Tei,
}

impl CorpusReadConfig {
    /// Build the config for a declared format. The Markdown reader config is
    /// consulted only for the Markdown variant; for Plain/Jsonl it is ignored.
    #[must_use]
    pub fn new(format: CorpusFormat, markdown: MarkdownReaderConfig) -> Self {
        match format {
            CorpusFormat::Plain => Self::Plain,
            CorpusFormat::Jsonl => Self::Jsonl,
            CorpusFormat::Markdown => Self::Markdown(markdown),
            CorpusFormat::Conllu => Self::Conllu,
            CorpusFormat::Bccwj => Self::Bccwj,
            CorpusFormat::Soranoha => Self::Soranoha,
            CorpusFormat::SoranohaTei => Self::SoranohaTei,
            CorpusFormat::Tei => Self::Tei,
        }
    }

    /// The format this resolved config will read.
    #[must_use]
    pub fn format(&self) -> CorpusFormat {
        match self {
            Self::Plain => CorpusFormat::Plain,
            Self::Jsonl => CorpusFormat::Jsonl,
            Self::Markdown(_) => CorpusFormat::Markdown,
            Self::Conllu => CorpusFormat::Conllu,
            Self::Bccwj => CorpusFormat::Bccwj,
            Self::Soranoha => CorpusFormat::Soranoha,
            Self::SoranohaTei => CorpusFormat::SoranohaTei,
            Self::Tei => CorpusFormat::Tei,
        }
    }

    /// A stable, configuration-dependent tag.
    ///
    /// Plain and JSONL resolve to fixed tags (they carry no tunable policy);
    /// Markdown resolves to its [`MarkdownReaderConfig::reader_tag`], so a
    /// changed section policy or recovery mode yields a different tag. The
    /// source-backed index fingerprint includes this tag in its header so that
    /// a different resolved reader configuration produces a different
    /// fingerprint even when the selected files' raw bytes are unchanged.
    #[must_use]
    pub fn reader_tag(&self) -> u64 {
        match self {
            Self::Plain => PLAIN_CONFIG_TAG,
            Self::Jsonl => JSONL_CONFIG_TAG,
            Self::Markdown(config) => config.reader_tag(),
            Self::Conllu => CONLLU_CONFIG_TAG,
            Self::Bccwj => BCCWJ_CONFIG_TAG,
            Self::Soranoha => SORANOHA_CONFIG_TAG,
            Self::SoranohaTei => 0x81e4_73f9_d94a_b067,
            Self::Tei => 0x785d_1c80_2e96_4bb3,
        }
    }

    fn input_format(&self) -> InputFormat {
        match self {
            Self::Plain => InputFormat::Plaintext,
            Self::Jsonl => InputFormat::Jsonl,
            Self::Markdown(config) => InputFormat::Markdown(config.clone()),
            Self::Conllu => InputFormat::Conllu,
            Self::Tei => InputFormat::Tei,
            // [`Self::discover`] rejects the Bccwj and Soranoha configs at
            // construction, so no `CorpusSource` can ever hold one; neither is
            // an `InputFormat` (BCCWJ is a directory of zips, spec §3.1, and a
            // Soranoha archive's reader needs a seekable handle).
            Self::Bccwj | Self::Soranoha | Self::SoranohaTei => {
                #[expect(
                    clippy::unreachable,
                    reason = "discover rejects the Bccwj and Soranoha configs at construction, so no CorpusSource holds one"
                )]
                {
                    unreachable!("CorpusSource never streams a Bccwj or Soranoha config")
                }
            }
        }
    }

    /// File selection used when this config reads a directory corpus.
    #[must_use]
    pub const fn directory_file_kind(&self) -> Option<CorpusFileKind> {
        match self {
            Self::Markdown(_) => Some(CorpusFileKind::Markdown),
            Self::Conllu => Some(CorpusFileKind::Conllu),
            Self::Tei => Some(CorpusFileKind::Xml),
            Self::Plain | Self::Jsonl | Self::Bccwj | Self::Soranoha | Self::SoranohaTei => None,
        }
    }
}

/// An immutable corpus location and reader configuration.
///
/// Construct with [`CorpusSource::discover`], then stream repeatedly with
/// [`CorpusSource::stream`] / [`CorpusSource::structured_stream`]. The source
/// owns no per-stream state, so independent streams do not interfere.
#[derive(Debug, Clone)]
pub struct CorpusSource {
    location: SourceLocation,
    config: CorpusReadConfig,
}

#[derive(Debug, Clone)]
enum SourceLocation {
    File(PathBuf),
    Directory(PathBuf),
}

impl CorpusSource {
    /// Discover a corpus at `path`: a directory of recursively discovered
    /// Markdown/CoNLL-U files, or a single file read with the declared config.
    ///
    /// Directory discovery accepts Markdown and CoNLL-U configs and rejects
    /// every other resolved format before opening any file. Directory entries
    /// are filtered to the selected format's extensions (case-insensitive) in
    /// bounded per-directory depth-first order,
    /// following directory symlinks and excluding sidecars; an empty selection
    /// is an error, not a silent empty stream. A
    /// single file is admitted as-is, with no extension inference: the declared
    /// config is the format.
    pub fn discover(path: impl AsRef<Path>, config: CorpusReadConfig) -> io::Result<Self> {
        if matches!(
            config,
            CorpusReadConfig::Bccwj | CorpusReadConfig::Soranoha | CorpusReadConfig::SoranohaTei
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "neither a bccwj distribution nor a soranoha archive is a CorpusSource; ling reads both through its own corpus path, and lingrep-web serves them only as prepared artifacts",
            ));
        }
        let root = path.as_ref().to_path_buf();
        let metadata = fs::metadata(&root)?;
        let location = if metadata.is_dir() {
            let Some(file_kind) = config.directory_file_kind() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a directory corpus requires a Markdown, CoNLL-U, or TEI read config",
                ));
            };
            let mut discovery =
                CorpusDiscovery::new_for_kind(&root, TraversalLimits::default(), file_kind)
                    .map_err(super::discovery::DiscoveryError::into_io)?;
            if discovery
                .next_document()
                .map_err(super::discovery::DiscoveryError::into_io)?
                .is_none()
            {
                discovery
                    .finish()
                    .map_err(super::discovery::DiscoveryError::into_io)?;
            }
            SourceLocation::Directory(root)
        } else if metadata.is_file() {
            SourceLocation::File(root)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "corpus path is neither a regular file nor a directory",
            ));
        };
        Ok(Self { location, config })
    }

    /// The resolved reader configuration.
    #[must_use]
    pub fn config(&self) -> &CorpusReadConfig {
        &self.config
    }

    /// Stream the source as plain sentence texts, opening one file at a time and
    /// wrapping per-file failures with the offending path.
    ///
    /// A fresh stream each call; consuming one does not affect another.
    #[must_use]
    pub fn stream(&self) -> CorpusStream {
        CorpusStream {
            structured: self.structured_stream(),
        }
    }

    /// Stream the source as structured admitted corpus events, opening one file
    /// at a time and wrapping per-file failures with the offending path.
    ///
    /// For a Markdown source, one shared [`MarkdownDiagnostics`] sink is created
    /// for the whole source and injected into every selected file's reader, so
    /// observed labels accumulate across the directory rather than resetting per
    /// file. The sink is readable after the stream completes via
    /// [`CorpusStructuredStream::diagnostics`]. For a non-Markdown source the
    /// sink is `None`.
    #[must_use]
    pub fn structured_stream(&self) -> CorpusStructuredStream {
        let diagnostics = match &self.config {
            CorpusReadConfig::Markdown(_) => Some(MarkdownDiagnostics::default()),
            _ => None,
        };
        CorpusStructuredStream {
            admitted: AdmittedCorpus::new(CorpusRawIter {
                files: match &self.location {
                    SourceLocation::File(path) => {
                        CorpusFileIter::Files(vec![path.clone()].into_iter())
                    }
                    SourceLocation::Directory(root) => CorpusFileIter::Discovery(Box::new(Some(
                        CorpusDiscovery::new_for_kind(
                            root,
                            TraversalLimits::default(),
                            self.config
                                .directory_file_kind()
                                .expect("a directory source has a file selection"),
                        )
                        .map_err(super::discovery::DiscoveryError::into_io),
                    ))),
                },
                current: None,
                current_path: None,
                input_format: self.config.input_format(),
                diagnostics: diagnostics.clone(),
            }),
            diagnostics,
        }
    }
}

/// A plain-text sentence stream over a [`CorpusSource`].
pub struct CorpusStream {
    structured: CorpusStructuredStream,
}

impl CorpusStream {
    /// The shared Markdown diagnostics sink, if this is a Markdown source.
    /// `None` for non-Markdown sources.
    #[must_use]
    pub fn diagnostics(&self) -> Option<&MarkdownDiagnostics> {
        self.structured.diagnostics()
    }
}

impl Iterator for CorpusStream {
    type Item = io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.structured.next()? {
                Ok(AdmittedCorpusEvent::Sentence(sentence)) => return Some(Ok(sentence.text)),
                Ok(_) => {}
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// A structured admitted-event stream over a [`CorpusSource`].
pub struct CorpusStructuredStream {
    admitted: AdmittedCorpus<CorpusRawIter>,
    diagnostics: Option<MarkdownDiagnostics>,
}

impl CorpusStructuredStream {
    /// The shared Markdown diagnostics sink, if this is a Markdown source.
    /// `None` for non-Markdown sources. Read after consuming the iterator to
    /// observe the labels every selected file wrote.
    #[must_use]
    pub fn diagnostics(&self) -> Option<&MarkdownDiagnostics> {
        self.diagnostics.as_ref()
    }
}

impl Iterator for CorpusStructuredStream {
    type Item = io::Result<AdmittedCorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        self.admitted.next()
    }
}

struct CorpusRawIter {
    files: CorpusFileIter,
    current: Option<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>>,
    current_path: Option<PathBuf>,
    input_format: InputFormat,
    diagnostics: Option<MarkdownDiagnostics>,
}

enum CorpusFileIter {
    Files(std::vec::IntoIter<PathBuf>),
    Discovery(Box<Option<Result<CorpusDiscovery, io::Error>>>),
}

impl CorpusFileIter {
    fn next_path(&mut self) -> io::Result<Option<PathBuf>> {
        match self {
            Self::Files(files) => Ok(files.next()),
            Self::Discovery(discovery) => {
                if discovery.is_none() {
                    return Ok(None);
                }
                let next = {
                    let state = discovery.as_mut();
                    let discovery = state
                        .as_mut()
                        .expect("discovery presence was checked")
                        .as_mut()
                        .map_err(|error| io::Error::new(error.kind(), error.to_string()))?;
                    discovery
                        .next_document()
                        .map_err(super::discovery::DiscoveryError::into_io)?
                };
                if let Some(document) = next {
                    Ok(Some(document.path))
                } else {
                    let discovery = discovery.take().expect("discovery presence was checked")?;
                    discovery
                        .finish()
                        .map_err(super::discovery::DiscoveryError::into_io)?;
                    Ok(None)
                }
            }
        }
    }
}

impl Iterator for CorpusRawIter {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.current.as_mut().and_then(Iterator::next) {
                return Some(item.map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "{}: {error}",
                            self.current_path
                                .as_deref()
                                .unwrap_or_else(|| Path::new("<corpus>"))
                                .display()
                        ),
                    )
                }));
            }
            let path = match self.files.next_path() {
                Ok(Some(path)) => path,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            };
            let reader = match File::open(&path) {
                Ok(file_handle) => BufReader::new(file_handle),
                Err(error) => {
                    return Some(Err(io::Error::new(
                        error.kind(),
                        format!("{}: {error}", path.display()),
                    )));
                }
            };
            self.current_path = Some(path.clone());
            // The shared diagnostics sink is cloned per file so the stream
            // retains a readable handle while each file's reader writes into the
            // same underlying `Arc<Mutex<…>>`. For a Markdown source every file
            // shares one sink; for other formats `None` keeps the reader's
            // diagnostics path uniform without allocating a sink the caller
            // cannot read.
            let diagnostics = self.diagnostics.clone();
            self.current = Some(stream_input_events(
                self.input_format.clone(),
                reader,
                path.display().to_string(),
                diagnostics,
            ));
        }
    }
}
