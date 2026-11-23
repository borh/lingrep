//! Filesystem discovery, directory/reader validation, stdin/document opening,
//! and CLI-annotated discovery error translation.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use lingrep_core::corpus::{
    AdmittedCorpus, CorpusDiscovery, CorpusEvent, CorpusFileKind, CorpusReadConfig, DiscoveryError,
    DiscoveryReport, InputFormat, MarkdownDiagnostics, SourceDocument, TraversalLimits,
    ValidatedCorpusRoot, stream_input_events,
};
use lingrep_core::execution::{BoundedRecordError, BoundedRecordReader, ExecutionError};
use lingrep_core::scope::ScopeClause;
use lingrep_corpus::SourceObservation;

use super::batching::TerminalFailure;
use super::hashing::{
    HashingReader, OrderedInventoryObservation, StdinObservation, StdinReader, identity_io_error,
};

#[derive(Debug, thiserror::Error)]
#[error("input failed in {source_name}: {source}")]
pub(super) struct SourceInputError {
    pub(super) source_name: String,
    #[source]
    pub(super) source: io::Error,
}

/// Validates that a discovered root (file vs. directory) is compatible with
/// the selected corpus reader configuration.
///
/// Shared by both direct execution (`FilesystemCorpusEvents`) and prepared
/// artifact builds (`PreparedBuildEvents`) so both surfaces enforce the exact
/// same directory rules and error messages.
pub(crate) fn validate_root_reader_compatibility(
    validated: &ValidatedCorpusRoot,
    config: &CorpusReadConfig,
) -> Result<(), DiscoveryError> {
    if validated.is_directory()
        && matches!(
            config,
            CorpusReadConfig::Soranoha | CorpusReadConfig::SoranohaTei
        )
    {
        return Err(DiscoveryError::IoAt {
            logical_path: PathBuf::from("."),
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
                | CorpusReadConfig::Plain
                | CorpusReadConfig::Conllu
                | CorpusReadConfig::Bccwj
                | CorpusReadConfig::Tei
        )
    {
        return Err(DiscoveryError::IoAt {
            logical_path: PathBuf::from("."),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "a directory corpus requires the Markdown, plain-text, CoNLL-U, TEI, or BCCWJ reader",
            ),
        });
    }
    Ok(())
}

/// Discovers a BCCWJ distribution root, observes its `DOC/` context files into
/// `observation` in canonical order, and returns the paired cursor and stream.
///
/// Shared by both direct execution and prepared builds so both hash the same
/// context bytes in the same phase before the first zip's `begin_source`.
pub(crate) fn open_bccwj_discovery(
    root: &Path,
    limits: &TraversalLimits,
    observation: &SourceObservation,
    record_limit: usize,
) -> Result<
    (
        crate::bccwj_seam::BccwjCursor,
        crate::bccwj_seam::BccwjStream,
    ),
    DiscoveryError,
> {
    let inventory =
        lingrep_core::corpus::bccwj::BccwjInventory::discover(root, limits).map_err(|source| {
            DiscoveryError::IoAt {
                logical_path: root.to_path_buf(),
                source,
            }
        })?;
    // The context tables are part of the source identity: observe them before
    // the first zip's begin_source (the API enforces the phase), so the live
    // run, the naming pass and the build pass all hash the same bytes in the
    // same order.
    crate::bccwj_seam::observe_bccwj_context_files(&inventory, root, observation).map_err(
        |source| DiscoveryError::IoAt {
            logical_path: root.to_path_buf(),
            source,
        },
    )?;
    Ok((
        crate::bccwj_seam::BccwjCursor::new(inventory.zips),
        crate::bccwj_seam::BccwjStream::new(root.to_path_buf(), record_limit),
    ))
}

/// Returns the file kind walked by directory discovery for `config`.
pub(crate) fn directory_file_kind(config: &CorpusReadConfig) -> CorpusFileKind {
    config
        .directory_file_kind()
        .unwrap_or(CorpusFileKind::Markdown)
}

pub(super) fn terminal_failure_from_discovery(error: &DiscoveryError) -> TerminalFailure {
    let (code, logical_path) = match error {
        DiscoveryError::Cycle { logical_path } => (
            "directory_cycle",
            logical_path.to_string_lossy().into_owned(),
        ),
        DiscoveryError::DanglingLink { logical_path } => {
            ("dangling_link", logical_path.to_string_lossy().into_owned())
        }
        DiscoveryError::NonUtf8Name { path } => {
            ("non_utf8_source_name", path.to_string_lossy().into_owned())
        }
        DiscoveryError::NfcNameCollision { first, .. } => (
            "nfc_source_name_collision",
            first.to_string_lossy().into_owned(),
        ),
        DiscoveryError::Limit { logical_path, .. } => (
            "traversal_limit",
            logical_path.to_string_lossy().into_owned(),
        ),
        DiscoveryError::IoAt { logical_path, .. } => {
            ("discovery_io", logical_path.to_string_lossy().into_owned())
        }
    };
    let message = match error {
        DiscoveryError::Limit {
            field, observed, ..
        } => {
            let flag = match *field {
                "max_depth" => "--max-traversal-depth",
                "max_source_entries" => "--max-source-entries",
                "max_directory_entries" => "--max-directory-entries",
                "max_documents" => "--max-documents",
                "max_index_bytes" => "--max-source-index-bytes",
                other => other,
            };
            format!("{error}; rerun with {flag} {observed} or higher to admit this input")
        }
        _ => error.to_string(),
    };
    TerminalFailure::Discovery {
        code,
        logical_path,
        message,
    }
}

pub(crate) fn execution_discovery_error(error: &DiscoveryError) -> ExecutionError {
    terminal_failure_from_discovery(error).execution_error()
}

/// Identity and traversal facts for the filesystem sources read by a run.
pub struct FilesystemObservation {
    pub source: SourceObservation,
    pub inventory: OrderedInventoryObservation,
    pub report: Option<DiscoveryReport>,
}

/// Raw multi-file corpus stream built on the shared bounded discovery cursor.
pub struct FilesystemCorpusEvents {
    discovery: DiscoveryState,
    current: Option<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>>,
    current_source_name: Option<String>,
    config: CorpusReadConfig,
    record_limit: usize,
    observation: FilesystemObservation,
    /// One sink for the whole root, so the labels reported at the end are the
    /// labels the *run* observed rather than the last document's.
    diagnostics: MarkdownDiagnostics,
}

enum DiscoveryState {
    Validated(Box<ValidatedCorpusRoot>),
    Open(Box<CorpusDiscovery>),
    /// The BCCWJ distribution: one document stream per inventory zip.
    ///
    /// The cursor and the per-run stream facts live in the variant that
    /// needs them. Holding them as `Option` fields beside it let the type
    /// say "a BCCWJ walk with no metadata table", which nothing could
    /// construct and a runtime guard had to reject anyway.
    Bccwj {
        cursor: crate::bccwj_seam::BccwjCursor,
        stream: crate::bccwj_seam::BccwjStream,
    },
    Exhausted,
}

impl FilesystemCorpusEvents {
    /// Validates only the selected root. Directory enumeration and recursive
    /// failures remain deferred until the event stream is pulled after `run`.
    pub fn new(
        root: &Path,
        config: CorpusReadConfig,
        limits: TraversalLimits,
        record_limit: usize,
    ) -> Result<(Self, MarkdownDiagnostics), ExecutionError> {
        let validated = CorpusDiscovery::validate_root(root, limits.clone())
            .map_err(|error| execution_discovery_error(&error))?;
        validate_root_reader_compatibility(&validated, &config)
            .map_err(|error| execution_discovery_error(&error))?;
        let observation = FilesystemObservation {
            source: SourceObservation::new(),
            inventory: OrderedInventoryObservation::default(),
            report: None,
        };
        let diagnostics = MarkdownDiagnostics::default();
        let discovery = if matches!(config, CorpusReadConfig::Bccwj) {
            if !validated.is_directory() {
                return Err(ExecutionError::InvalidLimit {
                    field: "input",
                    message: "bccwj input must be a distribution root (a directory containing C-XML/ and DOC/)".into(),
                });
            }
            let (cursor, stream) =
                open_bccwj_discovery(root, &limits, &observation.source, record_limit)
                    .map_err(|error| execution_discovery_error(&error))?;
            DiscoveryState::Bccwj { cursor, stream }
        } else {
            DiscoveryState::Validated(Box::new(validated))
        };
        Ok((
            Self {
                discovery,
                current: None,
                current_source_name: None,
                config,
                record_limit,
                observation,
                diagnostics: diagnostics.clone(),
            },
            diagnostics,
        ))
    }

    pub(crate) fn into_observation(self) -> FilesystemObservation {
        self.observation
    }

    fn open_document(
        &mut self,
        document: &SourceDocument,
    ) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
        open_corpus_document(
            &self.observation.source,
            Some(&mut self.observation.inventory),
            Some(&self.diagnostics),
            &self.config,
            self.record_limit,
            document,
        )
    }
}

impl Iterator for FilesystemCorpusEvents {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.current.as_mut().and_then(Iterator::next) {
                return Some(event.map_err(|error| {
                    contextualize_input_error(
                        error,
                        self.current_source_name.as_deref().unwrap_or_default(),
                    )
                }));
            }
            self.current = None;
            self.current_source_name = None;
            if matches!(self.discovery, DiscoveryState::Validated(_)) {
                let DiscoveryState::Validated(validated) =
                    std::mem::replace(&mut self.discovery, DiscoveryState::Exhausted)
                else {
                    #[expect(
                        clippy::unreachable,
                        reason = "the Exhausted/Validated/Open state was checked above"
                    )]
                    {
                        unreachable!("discovery state was checked")
                    }
                };
                match CorpusDiscovery::from_validated_root_for_kind(
                    *validated,
                    directory_file_kind(&self.config),
                ) {
                    Ok(discovery) => self.discovery = DiscoveryState::Open(Box::new(discovery)),
                    Err(error) => return Some(Err(discovery_io_error(error))),
                }
            }
            if let DiscoveryState::Bccwj { cursor, stream } = &mut self.discovery {
                let Some(document) = cursor.next_document() else {
                    // The inventory is exhausted: publish the report the
                    // build derives over the same inventory — the zip
                    // (source) names, not the entry (document) names.
                    //
                    // Both halves come from the *inventory*, never from the
                    // observation. `validate_ordered_inventory` compares the
                    // observation's digest against this one, so deriving this
                    // one from the observation made that check compare a
                    // value with itself and it could not fail — while for
                    // every other format it proves the sources read are the
                    // sources discovered, in order.
                    self.observation.report = Some(cursor.report());
                    return None;
                };
                let zip = match cursor.zip_for(&document) {
                    Ok(zip) => zip.clone(),
                    Err(error) => return Some(Err(error)),
                };
                let opened = stream.open_observing_labels(
                    &self.observation.source,
                    Some(&mut self.observation.inventory),
                    &zip,
                    &self.diagnostics,
                );
                match opened {
                    Ok(events) => {
                        self.current = Some(events);
                        self.current_source_name = Some(document.logical_name);
                    }
                    Err(error) => {
                        return Some(Err(contextualize_input_error(
                            error,
                            &document.logical_name,
                        )));
                    }
                }
                continue;
            }
            let DiscoveryState::Open(discovery) = &mut self.discovery else {
                return None;
            };
            match discovery.next_document() {
                Ok(Some(document)) => {
                    let logical_name = document.logical_name.clone();
                    match self.open_document(&document) {
                        Ok(events) => {
                            self.current = Some(events);
                            self.current_source_name = Some(logical_name);
                        }
                        Err(error) => {
                            return Some(Err(contextualize_input_error(error, &logical_name)));
                        }
                    }
                }
                Ok(None) => {
                    let DiscoveryState::Open(discovery) =
                        std::mem::replace(&mut self.discovery, DiscoveryState::Exhausted)
                    else {
                        #[expect(clippy::unreachable, reason = "open discovery was just matched")]
                        {
                            unreachable!("open discovery was just matched")
                        }
                    };
                    match discovery.finish() {
                        Ok(report) => {
                            self.observation.report = Some(report);
                            return None;
                        }
                        Err(error) => return Some(Err(discovery_io_error(error))),
                    }
                }
                Err(error) => return Some(Err(discovery_io_error(error))),
            }
        }
    }
}

fn contextualize_input_error(error: io::Error, source_name: &str) -> io::Error {
    if error.get_ref().is_some_and(
        <dyn std::error::Error + std::marker::Send + std::marker::Sync + 'static>::is::<
            BoundedRecordError,
        >,
    ) {
        return error;
    }
    let kind = error.kind();
    io::Error::new(
        kind,
        SourceInputError {
            source_name: source_name.to_owned(),
            source: error,
        },
    )
}

pub type AdmittedFilesystemEvents = AdmittedCorpus<FilesystemCorpusEvents>;

pub fn admitted_filesystem_events(events: FilesystemCorpusEvents) -> AdmittedFilesystemEvents {
    AdmittedCorpus::new(events)
}

/// Applies a scope clause at sentence admission, if one was given.
pub fn scoped<I>(corpus: AdmittedCorpus<I>, clause: Option<&ScopeClause>) -> AdmittedCorpus<I> {
    match clause {
        Some(clause) => corpus.with_scope(clause.clone()),
        None => corpus,
    }
}

pub(crate) fn input_format(config: &CorpusReadConfig) -> InputFormat {
    match config {
        CorpusReadConfig::Plain => InputFormat::Plaintext,
        CorpusReadConfig::Jsonl => InputFormat::Jsonl,
        CorpusReadConfig::Conllu => InputFormat::Conllu,
        CorpusReadConfig::Tei => InputFormat::Tei,
        CorpusReadConfig::Markdown(config) => InputFormat::Markdown(config.clone()),
        // BCCWJ is a directory of zips and a Soranoha archive is a seekable
        // container: neither reader consumes one `BufRead` stream, so both
        // stream through their own arms instead of this mapping. This function
        // is reached only for the file formats above.
        CorpusReadConfig::Bccwj | CorpusReadConfig::Soranoha | CorpusReadConfig::SoranohaTei => {
            #[expect(
                clippy::unreachable,
                reason = "bccwj and soranoha documents stream through their dedicated arms, not the file-format mapping"
            )]
            {
                unreachable!("the bccwj and soranoha configs arrive through their dedicated arms")
            }
        }
    }
}

/// The shared stdin seam: an observation, a record-bounded reader named
/// `"<stdin>"`, and the format-typed event stream.
///
/// Every command that accepts `-` builds exactly these three things, and they
/// have to stay identical or the same bytes would report different identities
/// under different subcommands. Only the *admission* wrapper around the
/// returned events differs — scope clause, projection, source metadata — so
/// that part stays with the caller. The observation is returned rather than
/// held because a command that reports a stream digest keeps it past the run;
/// the reader owns its own clone either way.
pub(crate) fn stdin_events(
    config: &CorpusReadConfig,
    max_input_record_bytes: usize,
) -> (
    StdinObservation,
    Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>,
) {
    let (reader, observation) = StdinReader::new(io::stdin());
    if matches!(config, CorpusReadConfig::Tei) {
        let events = match lingrep_core::corpus::tei::TeiEvents::new(
            reader,
            "<stdin>".into(),
            max_input_record_bytes,
        ) {
            Ok(events) => {
                Box::new(events) as Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>
            }
            Err(error) => Box::new(std::iter::once(Err(error))),
        };
        return (observation, events);
    }
    let (reader, _) =
        BoundedRecordReader::new(BufReader::new(reader), max_input_record_bytes, "<stdin>");
    let events = stream_input_events(input_format(config), reader, "<stdin>".into(), None);
    (observation, events)
}

/// The shared per-document open used by both corpus seams: open the file,
/// record the ordered-inventory observation (direct seam only), wrap the
/// handle in the identity-hash reader and the record bound, and stream the
/// format-typed events with the diagnostics sink (direct seam only).
///
/// The seam **loops** that feed this stay separate: their side channels
/// differ (the discovered documents in prepare, lazy versus
/// eager discovery open, error styles), and extracting the loop would force
/// a callback interface larger than either existing one.
pub(crate) fn open_corpus_document(
    source: &SourceObservation,
    inventory: Option<&mut OrderedInventoryObservation>,
    diagnostics: Option<&MarkdownDiagnostics>,
    config: &CorpusReadConfig,
    record_limit: usize,
    document: &SourceDocument,
) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
    if matches!(
        config,
        CorpusReadConfig::Soranoha | CorpusReadConfig::SoranohaTei
    ) {
        return crate::soranoha_seam::open(
            source,
            inventory,
            document,
            record_limit,
            matches!(config, CorpusReadConfig::SoranohaTei),
        );
    }
    let file = File::open(&document.path)?;
    if let Some(inventory) = inventory {
        inventory
            .observe(&document.logical_name)
            .map_err(identity_io_error)?;
    }
    let reader = HashingReader::new(file, source.clone(), &document.logical_name)
        .map_err(identity_io_error)?;
    if matches!(config, CorpusReadConfig::Tei) {
        return lingrep_core::corpus::tei::TeiEvents::new(
            reader,
            document.logical_name.clone(),
            record_limit,
        )
        .map(|events| {
            Box::new(events) as Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>
        });
    }
    let (reader, _) =
        BoundedRecordReader::new(BufReader::new(reader), record_limit, &document.logical_name);
    Ok(stream_input_events(
        input_format(config),
        reader,
        document.logical_name.clone(),
        diagnostics.cloned(),
    ))
}

fn discovery_io_error(error: DiscoveryError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_limit_errors_include_the_corresponding_cli_option() {
        for (field, flag) in [
            ("max_depth", "--max-traversal-depth"),
            ("max_source_entries", "--max-source-entries"),
            ("max_directory_entries", "--max-directory-entries"),
            ("max_documents", "--max-documents"),
            ("max_index_bytes", "--max-source-index-bytes"),
        ] {
            let error = execution_discovery_error(&DiscoveryError::Limit {
                field,
                limit: 5,
                observed: 6,
                logical_path: "source/nested".into(),
            });
            assert!(error.to_string().contains(&format!("{flag} 6")), "{error}");
            assert!(error.to_string().contains("source/nested"), "{error}");
        }
    }

    #[test]
    fn directory_validation_rejects_soranoha_and_jsonl_directories() {
        let dir = tempfile::tempdir().unwrap();
        let validated =
            CorpusDiscovery::validate_root(dir.path(), TraversalLimits::default()).unwrap();

        let soranoha = validate_root_reader_compatibility(&validated, &CorpusReadConfig::Soranoha)
            .unwrap_err();
        assert!(soranoha.to_string().contains("soranoha input must be"));

        let jsonl =
            validate_root_reader_compatibility(&validated, &CorpusReadConfig::Jsonl).unwrap_err();
        assert!(jsonl.to_string().contains("a directory corpus requires"));

        assert!(validate_root_reader_compatibility(&validated, &CorpusReadConfig::Plain).is_ok());
    }
}
