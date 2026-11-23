//! Prepared-corpus build and execution adapters.
//!
//! The build iterator owns one bounded discovery cursor and opens one admitted
//! source at a time. The pending documents expose the discovery records
//! whose `DocumentStart` is next, so `CorpusBuilder` can bind physical
//! discovery facts to the same single admitted event stream it consumes.

use std::io;
use std::path::Path;

use lingrep_core::corpus::{
    CorpusDiscovery, CorpusEvent, CorpusFormat, CorpusReadConfig, DiscoveryError, DiscoveryReport,
    SourceDocument, ValidatedCorpusRoot,
};
use lingrep_corpus::SourceObservation;

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
    current_document: Option<String>,
    config: CorpusReadConfig,
    record_limit: usize,
    observation: SourceObservation,
    documents: Vec<SourceDocument>,
    report: Option<DiscoveryReport>,
}

impl PreparedBuildEvents {
    pub(crate) fn new(
        validated: ValidatedCorpusRoot,
        config: CorpusReadConfig,
        record_limit: usize,
        observation: SourceObservation,
        limits: &lingrep_core::corpus::TraversalLimits,
    ) -> Result<Self, DiscoveryError> {
        crate::direct_source::validate_root_reader_compatibility(&validated, &config)?;
        let discovery = if matches!(config, CorpusReadConfig::Bccwj) {
            let (cursor, stream) = crate::direct_source::open_bccwj_discovery(
                validated.root(),
                limits,
                &observation,
                record_limit,
            )?;
            PreparedDiscovery::Bccwj { cursor, stream }
        } else {
            let file_kind = crate::direct_source::directory_file_kind(&config);
            PreparedDiscovery::Generic(Box::new(CorpusDiscovery::from_validated_root_for_kind(
                validated, file_kind,
            )?))
        };
        Ok(Self {
            discovery: Some(discovery),
            current: None,
            current_document: None,
            config,
            record_limit,
            observation,
            documents: Vec::new(),
            report: None,
        })
    }

    pub(crate) fn take_documents(&mut self) -> Vec<SourceDocument> {
        std::mem::take(&mut self.documents)
    }

    pub(crate) fn into_discovery_report(self) -> Option<DiscoveryReport> {
        self.report
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
                        self.current_document = Some(document.logical_name.clone());
                        self.documents.push(document);
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
                        Ok(report) => {
                            self.report = Some(report);
                            return None;
                        }
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
