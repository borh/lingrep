//! Prepared-corpus build and execution adapters.
//!
//! The build iterator owns one bounded discovery cursor and opens one admitted
//! source at a time. The paired queue exposes exactly the discovery record
//! whose `DocumentStart` is next, so `CorpusBuilder` can bind physical
//! discovery facts to the same single admitted event stream it consumes.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lingrep_core::corpus::{
    CorpusDiscovery, CorpusEvent, CorpusFormat, CorpusReadConfig, DiscoveryError, DiscoveryReport,
    SourceDocument, ValidatedCorpusRoot,
};
use lingrep_corpus::SourceObservation;

#[derive(Clone, Default)]
pub(crate) struct DiscoveredDocuments {
    pending: Arc<Mutex<VecDeque<SourceDocument>>>,
}

impl DiscoveredDocuments {
    pub(crate) fn drain(&self) -> Result<Vec<SourceDocument>, io::Error> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| io::Error::other("prepared source queue lock poisoned"))?;
        Ok(pending.drain(..).collect())
    }
}

/// The prepared build's document cursor: the generic Markdown/plain walk,
/// or the BCCWJ inventory (one document stream per zip; the entries are the
/// admitted documents, the zip is the discovered source).
enum PreparedDiscovery {
    Generic(Box<CorpusDiscovery>),
    Bccwj {
        cursor: crate::bccwj_seam::BccwjCursor,
        stream: crate::bccwj_seam::BccwjStream,
    },
}

impl PreparedDiscovery {
    fn next_document(&mut self) -> Result<Option<SourceDocument>, DiscoveryError> {
        match self {
            Self::Generic(discovery) => discovery.next_document(),
            Self::Bccwj { cursor, .. } => Ok(cursor.next_document()),
        }
    }

    fn finish(self) -> Result<DiscoveryReport, DiscoveryError> {
        match self {
            Self::Generic(discovery) => discovery.finish(),
            // The digest the builder's discovered-source registration
            // derives (`discovery_digest`) — the report check in
            // `finish_guarded` compares against exactly this value.
            Self::Bccwj { cursor, .. } => Ok(cursor.report()),
        }
    }
}

pub(crate) struct PreparedBuildEvents {
    discovery: Option<PreparedDiscovery>,
    current: Option<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>>,
    /// Logical name of the document `current` is reading, retained solely to
    /// name it in a reader error.
    ///
    /// A reader reports `line 321: …` because that is all it knows — it was
    /// handed one stream, not a tree. On a single-file run the execution layer
    /// supplies the rest (`input failed before sentence N in 00393.md: …`), but
    /// the build path has no such layer, so the same fault over a directory
    /// arrived as a bare line number with nothing to open. Over a
    /// thousand-document corpus that is a diagnostic that locates nothing.
    current_document: Option<String>,
    config: CorpusReadConfig,
    record_limit: usize,
    observation: SourceObservation,
    documents: DiscoveredDocuments,
    report: Arc<Mutex<Option<DiscoveryReport>>>,
}

impl PreparedBuildEvents {
    pub(crate) fn new(
        validated: ValidatedCorpusRoot,
        config: CorpusReadConfig,
        record_limit: usize,
        observation: SourceObservation,
        limits: &lingrep_core::corpus::TraversalLimits,
    ) -> Result<
        (
            Self,
            DiscoveredDocuments,
            Arc<Mutex<Option<DiscoveryReport>>>,
        ),
        DiscoveryError,
    > {
        if validated.is_directory()
            && matches!(
                config,
                CorpusReadConfig::Soranoha | CorpusReadConfig::SoranohaTei
            )
        {
            return Err(DiscoveryError::IoAt {
                logical_path: ".".into(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "soranoha input must be a bulk archive file (the .zip downloaded from soranoha.org)",
                ),
            });
        }
        if validated.is_directory()
            && !matches!(
                config,
                CorpusReadConfig::Markdown(_)
                    | CorpusReadConfig::Conllu
                    | CorpusReadConfig::Bccwj
                    | CorpusReadConfig::Tei
            )
        {
            return Err(DiscoveryError::IoAt {
                logical_path: ".".into(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a directory corpus requires the Markdown, CoNLL-U, TEI, or BCCWJ reader",
                ),
            });
        }
        let discovery = if matches!(config, CorpusReadConfig::Bccwj) {
            let root = validated.root().to_path_buf();
            let inventory = lingrep_core::corpus::bccwj::BccwjInventory::discover(&root, limits)
                .map_err(|source| DiscoveryError::IoAt {
                    logical_path: root.clone(),
                    source,
                })?;
            // The context tables are part of the source identity: observed
            // before the first zip's begin_source, in the same order the
            // naming pass and direct execution use.
            crate::bccwj_seam::observe_bccwj_context_files(&inventory, &root, &observation)
                .map_err(|source| DiscoveryError::IoAt {
                    logical_path: root.clone(),
                    source,
                })?;
            PreparedDiscovery::Bccwj {
                cursor: crate::bccwj_seam::BccwjCursor::new(inventory.zips),
                stream: crate::bccwj_seam::BccwjStream::new(root, record_limit),
            }
        } else {
            let file_kind = config
                .directory_file_kind()
                .unwrap_or(lingrep_core::corpus::CorpusFileKind::Markdown);
            PreparedDiscovery::Generic(Box::new(CorpusDiscovery::from_validated_root_for_kind(
                validated, file_kind,
            )?))
        };
        let documents = DiscoveredDocuments::default();
        let report = Arc::new(Mutex::new(None));
        Ok((
            Self {
                discovery: Some(discovery),
                current: None,
                current_document: None,
                config,
                record_limit,
                observation,
                documents: documents.clone(),
                report: Arc::clone(&report),
            },
            documents,
            report,
        ))
    }

    fn open_document(
        &self,
        document: &SourceDocument,
    ) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
        if let Some(PreparedDiscovery::Bccwj { cursor, stream }) = &self.discovery {
            // The zip is recovered from the document's own ordinal rather
            // than stashed by the producer for the consumer, so there is no
            // order-dependent channel between `next_document` and here.
            return stream.open(&self.observation, None, cursor.zip_for(document)?);
        }
        crate::direct_source::open_corpus_document(
            &self.observation,
            None,
            None,
            &self.config,
            self.record_limit,
            document,
        )
    }

    /// Prefixes a reader error with the document it came from.
    ///
    /// The `ErrorKind` is preserved: callers classify on it, and a reader's
    /// `InvalidData` must not become `Other` on the way to being readable.
    fn name_source(&self, error: io::Error) -> io::Error {
        let Some(name) = self.current_document.as_deref() else {
            return error;
        };
        io::Error::new(error.kind(), format!("{name}: {error}"))
    }
}

impl Iterator for PreparedBuildEvents {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.current.as_mut().and_then(Iterator::next) {
                // Only this branch is named. Discovery's own errors below carry
                // their own logical path and downcast to `DiscoveryError`,
                // which re-wrapping would erase.
                return Some(event.map_err(|error| self.name_source(error)));
            }
            self.current = None;
            self.current_document = None;
            let discovery = self.discovery.as_mut()?;
            match discovery.next_document() {
                Ok(Some(document)) => match self.open_document(&document) {
                    Ok(events) => {
                        let mut pending = match self.documents.pending.lock() {
                            Ok(pending) => pending,
                            Err(_) => {
                                return Some(Err(io::Error::other(
                                    "prepared source queue lock poisoned",
                                )));
                            }
                        };
                        self.current_document = Some(document.logical_name.clone());
                        pending.push_back(document);
                        drop(pending);
                        self.current = Some(events);
                    }
                    Err(error) => return Some(Err(error)),
                },
                Ok(None) => {
                    let discovery = self
                        .discovery
                        .take()
                        .expect("open discovery was just borrowed");
                    match discovery.finish() {
                        Ok(report) => match self.report.lock() {
                            Ok(mut stored) => {
                                *stored = Some(report);
                                return None;
                            }
                            Err(_) => {
                                return Some(Err(io::Error::other(
                                    "prepared discovery report lock poisoned",
                                )));
                            }
                        },
                        Err(error) => return Some(Err(io::Error::other(error))),
                    }
                }
                Err(error) => return Some(Err(io::Error::other(error))),
            }
        }
    }
}

pub(crate) fn corpus_format_for_validated_root(
    root: &Path,
    is_directory: bool,
    override_format: Option<CorpusFormat>,
) -> CorpusFormat {
    if is_directory {
        // A directory defaults to Markdown; an explicit override selects a
        // supported CoNLL-U tree or the BCCWJ distribution root.
        override_format.unwrap_or(CorpusFormat::Markdown)
    } else {
        lingrep_core::corpus::resolve_input_format(Some(root), override_format)
    }
}
