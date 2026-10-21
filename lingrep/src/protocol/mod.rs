//! Versioned, independently-identifiable `ling` JSONL protocols.

use std::io::{self, Write};

use serde::Serialize;
use thiserror::Error;

pub mod attributes;
pub mod capabilities;
pub mod check;
pub mod completions;
pub mod contract;
pub mod diagnostic;
pub mod dictionary;
pub mod discovery;
pub mod doctor;
pub mod invocation;
pub mod manpages;
pub mod query;
pub mod registry;
pub mod sentence_final;
pub mod sources;
pub mod status;
pub mod tokenize;
pub mod variants;

#[cfg(test)]
mod tests;

pub use discovery::ContractId;

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("output I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}

impl OutputError {
    /// A closed stdout pipe is an output failure, never a successful prefix.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == io::ErrorKind::BrokenPipe)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("invalid source metadata projection: {message}")]
    InvalidSourceMetadata { message: String },
    #[error("truncated JSONL record at line {line}: missing LF terminator")]
    TruncatedLine { line: u64 },
    #[error("malformed JSON at line {line}: {message}")]
    MalformedJson { line: u64, message: String },
    #[error("wrong schema at line {line}: expected {expected}, got {actual}")]
    WrongSchema {
        line: u64,
        expected: String,
        actual: String,
    },
    #[error("stream ended without a complete event")]
    MissingComplete,
    #[error("stream contains more than one complete event")]
    DuplicateComplete,
    #[error("stream contains more than one run event")]
    DuplicateRun,
    #[error("stream did not begin with run")]
    MissingRun,
    #[error("a run event appeared after result publication")]
    RunAfterResult,
    #[error("match and summary events cannot occur in one query stream")]
    MixedResultKinds,
    #[error("query result type {result_kind} is incompatible with run mode {mode}")]
    ResultModeMismatch { mode: String, result_kind: String },
    #[error("invalid corpus lifecycle: {message}")]
    InvalidCorpusLifecycle { message: String },
    #[error("definition event appeared after results")]
    DefinitionAfterResult,
    #[error("event appeared after complete")]
    EventAfterComplete,
    #[error("unknown event type {event_type:?} for schema {schema}")]
    UnknownEventType { schema: String, event_type: String },
}

/// Serialize one complete JSON object followed by LF, then flush it. The
/// flush makes one returned success an irreversible published event.
pub fn write_jsonl<W: Write, E: Serialize>(writer: &mut W, event: &E) -> Result<(), OutputError> {
    // Serialize before touching the destination. serde_json otherwise wraps a
    // destination failure in `serde_json::Error`, which loses the typed
    // `BrokenPipe` signal that controls stream completion.
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;
    Ok(())
}

/// Serialize one complete JSON object followed by LF through a caller-owned
/// scratch buffer, **without** flushing.
///
/// Same bytes as [`write_jsonl`], and the same serialize-before-writing order,
/// which is what keeps a closed stdout an [`OutputError::Io`] the caller can
/// recognize as a broken pipe rather than a `serde_json` failure. Two things
/// differ, both deliberate and both only for a high-volume record stream:
///
/// * `scratch` is reused, so serialization does not allocate a fresh line per
///   record. This matters for high-output catalogs.
/// * There is no per-record flush, so a returned success is *not* by itself an
///   irreversible published event. Durability is owed at the logical batch
///   seam instead: [`lingrep_core::execution::MatchSink::finish_batch`] flushes, which
///   is the boundary `RunOutcome::Cancelled` already reports against.
///
/// Serializing straight into the caller's `BufWriter` did not produce a
/// measurable improvement and reclassified a closed pipe as a serialization
/// error.
///
/// A caller that needs a single event to be irreversible on return — a run,
/// definition, diagnostic or complete record — must keep using [`write_jsonl`].
pub fn write_jsonl_buffered<W: Write, E: Serialize>(
    writer: &mut W,
    scratch: &mut Vec<u8>,
    event: &E,
) -> Result<(), OutputError> {
    scratch.clear();
    serde_json::to_writer(&mut *scratch, event)?;
    scratch.push(b'\n');
    writer.write_all(scratch)?;
    Ok(())
}

/// Validate the terminal-state contract for a typed query event stream.
pub fn validate_query_stream<I>(events: I) -> Result<(), ProtocolError>
where
    I: IntoIterator<Item = query::QueryEnvelope>,
{
    let mut saw_run = false;
    let mut complete = false;
    let mut result_kind = None;
    let mut run_mode = None;
    let mut run_corpus = None;
    let mut metadata_keys = Vec::new();
    for envelope in events {
        if envelope.schema != query::SCHEMA {
            return Err(ProtocolError::UnknownEventType {
                schema: envelope.schema,
                event_type: envelope.event.type_name().to_owned(),
            });
        }
        if complete {
            return if envelope.event.is_complete() {
                Err(ProtocolError::DuplicateComplete)
            } else {
                Err(ProtocolError::EventAfterComplete)
            };
        }
        match envelope.event {
            query::QueryEvent::Run(run) if !saw_run => {
                validate_query_run_lifecycle(&run)?;
                saw_run = true;
                metadata_keys = run.published_source_metadata.clone();
                run_mode = Some(run.mode);
                run_corpus = Some(run.corpus);
            }
            query::QueryEvent::Run(_) if result_kind.is_some() => {
                return Err(ProtocolError::RunAfterResult);
            }
            query::QueryEvent::Run(_) => return Err(ProtocolError::DuplicateRun),
            _ if !saw_run => return Err(ProtocolError::MissingRun),
            query::QueryEvent::Match(record) => {
                match &record.source_metadata {
                    None if metadata_keys.is_empty() => {},
                    Some(values) if !metadata_keys.is_empty() && lingrep_core::corpus::SourceMetadataProjection::new(metadata_keys.clone()).is_ok_and(|projection|projection.validate_output(values).is_ok()) => {},
                    _ => return Err(ProtocolError::InvalidSourceMetadata { message: "match metadata does not agree with the run projection or exceeds 64 KiB".into() }),
                }

                validate_result_mode(run_mode.expect("run mode set"), "match")?;
                set_result_kind(&mut result_kind, "match")?;
            }
            query::QueryEvent::Summary(_) => {
                validate_result_mode(run_mode.expect("run mode set"), "summary")?;
                set_result_kind(&mut result_kind, "summary")?;
            }
            query::QueryEvent::Definition(_) if result_kind.is_some() => {
                return Err(ProtocolError::DefinitionAfterResult);
            }
            query::QueryEvent::Definition(_) => {}
            query::QueryEvent::Complete(complete_event) => {
                validate_corpus_lifecycle(
                    run_corpus.as_ref().expect("run corpus set"),
                    &complete_event.corpus,
                    complete_event.coverage,
                    complete_event.termination,
                )?;
                validate_query_complete_lifecycle(
                    run_corpus.as_ref().expect("run corpus set"),
                    &complete_event,
                )?;
                complete = true;
            }
        }
    }
    if !saw_run {
        Err(ProtocolError::MissingRun)
    } else if complete {
        Ok(())
    } else {
        Err(ProtocolError::MissingComplete)
    }
}

pub fn validate_sentence_final_stream<I>(events: I) -> Result<(), ProtocolError>
where
    I: IntoIterator<Item = sentence_final::SentenceFinalEnvelope>,
{
    let mut saw_run = false;
    let mut complete = false;
    for envelope in events {
        if envelope.schema != sentence_final::SCHEMA {
            return Err(ProtocolError::WrongSchema {
                line: 0,
                expected: sentence_final::SCHEMA.into(),
                actual: envelope.schema,
            });
        }
        if complete {
            return match envelope.event {
                sentence_final::SentenceFinalEvent::Complete(_) => {
                    Err(ProtocolError::DuplicateComplete)
                }
                _ => Err(ProtocolError::EventAfterComplete),
            };
        }
        match envelope.event {
            sentence_final::SentenceFinalEvent::Run(_) if saw_run => {
                return Err(ProtocolError::DuplicateRun);
            }
            sentence_final::SentenceFinalEvent::Run(_) => saw_run = true,
            sentence_final::SentenceFinalEvent::Row(_) if !saw_run => {
                return Err(ProtocolError::MissingRun);
            }
            sentence_final::SentenceFinalEvent::Complete(_) if !saw_run => {
                return Err(ProtocolError::MissingRun);
            }
            sentence_final::SentenceFinalEvent::Row(_) => {}
            sentence_final::SentenceFinalEvent::Complete(_) => complete = true,
        }
    }
    if !saw_run {
        Err(ProtocolError::MissingRun)
    } else if complete {
        Ok(())
    } else {
        Err(ProtocolError::MissingComplete)
    }
}

fn validate_query_run_lifecycle(run: &query::QueryRun) -> Result<(), ProtocolError> {
    let projection =
        lingrep_core::corpus::SourceMetadataProjection::new(run.published_source_metadata.clone())
            .map_err(|error| ProtocolError::InvalidSourceMetadata {
                message: error.to_string(),
            })?;
    if projection.keys() != run.published_source_metadata
        || (run.mode == query::QueryMode::Summarize && !projection.keys().is_empty())
    {
        return Err(ProtocolError::InvalidSourceMetadata {
            message: "run metadata fields must be sorted, unique, and restricted to matches".into(),
        });
    }

    let inventory_is_valid = match &run.corpus {
        query::RunCorpus::Prepared { .. } => run.inventory.is_some(),
        query::RunCorpus::Direct { .. } | query::RunCorpus::Stdin { .. } => run.inventory.is_none(),
    };
    if inventory_is_valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidCorpusLifecycle {
            message: "run inventory availability does not match the corpus kind".into(),
        })
    }
}

fn validate_result_mode(mode: query::QueryMode, result_kind: &str) -> Result<(), ProtocolError> {
    let valid = matches!(
        (mode, result_kind),
        (query::QueryMode::Matches, "match") | (query::QueryMode::Summarize, "summary")
    );
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::ResultModeMismatch {
            mode: match mode {
                query::QueryMode::Matches => "matches",
                query::QueryMode::Summarize => "summarize",
            }
            .into(),
            result_kind: result_kind.into(),
        })
    }
}

pub(crate) fn validate_corpus_lifecycle(
    run: &query::RunCorpus,
    complete: &query::CompleteCorpus,
    coverage: query::Coverage,
    termination: query::Termination,
) -> Result<(), ProtocolError> {
    use query::{CompleteCorpus, Coverage, DirectCompletion, RunCorpus, Termination};

    if !matches!(
        (coverage, termination),
        (Coverage::Full, Termination::Eof) | (Coverage::Prefix, Termination::ResultLimit)
    ) {
        return Err(ProtocolError::InvalidCorpusLifecycle {
            message: "coverage and termination must be full/eof or prefix/result_limit".into(),
        });
    }

    let valid = match (run, complete) {
        (
            RunCorpus::Prepared {
                source_identity: run_source,
                analysis_identity: run_analysis,
            },
            CompleteCorpus::Prepared {
                source_identity: complete_source,
                analysis_identity: complete_analysis,
            },
        ) => run_source == complete_source && run_analysis == complete_analysis,
        (RunCorpus::Direct { .. }, CompleteCorpus::Direct { completion }) => match completion {
            DirectCompletion::Known { .. } => {
                coverage == Coverage::Full && termination == Termination::Eof
            }
            DirectCompletion::PrefixUnavailable => {
                coverage == Coverage::Prefix && termination == Termination::ResultLimit
            }
        },
        (
            RunCorpus::Stdin {
                stream_id: run_stream,
            },
            CompleteCorpus::Stdin {
                stream_id: complete_stream,
                observed_content_digest,
            },
        ) => {
            run_stream == complete_stream
                && match (coverage, termination) {
                    (Coverage::Full, Termination::Eof) => observed_content_digest.is_some(),
                    (Coverage::Prefix, Termination::ResultLimit) => {
                        observed_content_digest.is_none()
                    }
                    _ => false,
                }
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidCorpusLifecycle {
            message: "run and complete corpus descriptors disagree".into(),
        })
    }
}

fn validate_query_complete_lifecycle(
    run: &query::RunCorpus,
    complete: &query::QueryComplete,
) -> Result<(), ProtocolError> {
    use query::{Coverage, RunCorpus};

    let prefix_is_valid = match complete.coverage {
        Coverage::Full => complete.committed_prefix.is_none(),
        Coverage::Prefix => complete.committed_prefix.is_some(),
    };
    if !prefix_is_valid {
        return Err(ProtocolError::InvalidCorpusLifecycle {
            message: "full completion omits committed_prefix and prefix completion includes it"
                .into(),
        });
    }
    let inventory_is_valid = match (run, complete.coverage) {
        (RunCorpus::Prepared { .. }, _) => complete.inventory.is_some(),
        (RunCorpus::Direct { .. }, Coverage::Full) => complete.inventory.is_some(),
        (RunCorpus::Direct { .. }, Coverage::Prefix) | (RunCorpus::Stdin { .. }, _) => {
            complete.inventory.is_none()
        }
    };
    if inventory_is_valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidCorpusLifecycle {
            message: "inventory availability does not match the corpus lifecycle".into(),
        })
    }
}

fn set_result_kind(
    slot: &mut Option<&'static str>,
    next: &'static str,
) -> Result<(), ProtocolError> {
    match slot {
        Some(current) if *current != next => Err(ProtocolError::MixedResultKinds),
        Some(_) => Ok(()),
        None => {
            *slot = Some(next);
            Ok(())
        }
    }
}

/// Parse and validate a complete query JSONL stream without allowing a caller
/// to accidentally bypass schema or event-order checks.
pub fn validate_query_jsonl(bytes: &[u8]) -> Result<(), ProtocolError> {
    validate_jsonl(
        bytes,
        query::SCHEMA,
        &["run", "definition", "match", "summary", "complete"],
        |line, value| {
            serde_json::from_value::<query::QueryEnvelope>(value)
                .map_err(|error| classify_json_error(line, error))
        },
    )
    .and_then(validate_query_stream)
}

pub fn validate_tokenize_jsonl(bytes: &[u8]) -> Result<(), ProtocolError> {
    let events = validate_jsonl(
        bytes,
        tokenize::SCHEMA,
        &["run", "sentence", "complete"],
        |line, value| {
            serde_json::from_value::<tokenize::TokenizeEnvelope>(value)
                .map_err(|error| classify_json_error(line, error))
        },
    )?;
    validate_tokenize_stream(events)
}

fn validate_tokenize_stream(events: Vec<tokenize::TokenizeEnvelope>) -> Result<(), ProtocolError> {
    let mut run_corpus = None;
    let mut complete = false;
    for envelope in events {
        if complete {
            return if matches!(envelope.event, tokenize::TokenizeEvent::Complete(_)) {
                Err(ProtocolError::DuplicateComplete)
            } else {
                Err(ProtocolError::EventAfterComplete)
            };
        }
        match envelope.event {
            tokenize::TokenizeEvent::Run(run) if run_corpus.is_none() => {
                run_corpus = Some(run.corpus);
            }
            tokenize::TokenizeEvent::Run(_) => return Err(ProtocolError::DuplicateRun),
            tokenize::TokenizeEvent::Sentence(_) if run_corpus.is_none() => {
                return Err(ProtocolError::MissingRun);
            }
            tokenize::TokenizeEvent::Sentence(sentence) => {
                let valid_stream_id = match run_corpus.as_ref().expect("run corpus set") {
                    query::RunCorpus::Stdin { stream_id } => {
                        sentence.stream_id.as_deref() == Some(stream_id)
                    }
                    query::RunCorpus::Prepared { .. } | query::RunCorpus::Direct { .. } => {
                        sentence.stream_id.is_none()
                    }
                };
                if !valid_stream_id {
                    return Err(ProtocolError::InvalidCorpusLifecycle {
                        message: "sentence stream ID does not match its tokenize run".into(),
                    });
                }
            }
            tokenize::TokenizeEvent::Complete(_) if run_corpus.is_none() => {
                return Err(ProtocolError::MissingRun);
            }
            tokenize::TokenizeEvent::Complete(complete_event) => {
                let run = run_corpus.as_ref().expect("run corpus set");
                validate_corpus_lifecycle(
                    run,
                    &complete_event.corpus,
                    complete_event.coverage,
                    complete_event.termination,
                )?;
                if complete_event.coverage != query::Coverage::Full
                    || complete_event.termination != query::Termination::Eof
                {
                    return Err(ProtocolError::InvalidCorpusLifecycle {
                        message: "tokenize completion must be full/eof".into(),
                    });
                }
                let inventory_is_valid = match run {
                    query::RunCorpus::Prepared { .. } | query::RunCorpus::Direct { .. } => {
                        complete_event.inventory.is_some()
                    }
                    query::RunCorpus::Stdin { .. } => complete_event.inventory.is_none(),
                };
                if !inventory_is_valid {
                    return Err(ProtocolError::InvalidCorpusLifecycle {
                        message:
                            "tokenize inventory availability does not match the corpus lifecycle"
                                .into(),
                    });
                }
                complete = true;
            }
        }
    }
    if run_corpus.is_none() {
        Err(ProtocolError::MissingRun)
    } else if complete {
        Ok(())
    } else {
        Err(ProtocolError::MissingComplete)
    }
}

pub fn validate_sources_jsonl(bytes: &[u8]) -> Result<(), ProtocolError> {
    let events = validate_jsonl(
        bytes,
        sources::SCHEMA,
        &["run", "source", "complete"],
        |line, value| {
            serde_json::from_value::<sources::SourcesEnvelope>(value)
                .map_err(|error| classify_json_error(line, error))
        },
    )?;
    validate_run_records(events.into_iter().map(|event| match event.event {
        sources::SourcesEvent::Run(_) => Record::Run,
        sources::SourcesEvent::Source(_) => Record::Entry,
        sources::SourcesEvent::Complete(_) => Record::Complete,
    }))
}

pub fn validate_check_jsonl(bytes: &[u8]) -> Result<(), ProtocolError> {
    let events = validate_jsonl(bytes, check::SCHEMA, &["check"], |line, value| {
        serde_json::from_value::<check::CheckEnvelope>(value)
            .map_err(|error| classify_json_error(line, error))
    })?;
    if events.len() == 1 {
        Ok(())
    } else if events.is_empty() {
        Err(ProtocolError::MissingRun)
    } else {
        Err(ProtocolError::DuplicateComplete)
    }
}

/// Diagnostics are an independent JSONL stream: zero or more exact-schema
/// diagnostic envelopes, each framed by LF.
pub fn validate_diagnostic_jsonl(bytes: &[u8]) -> Result<(), ProtocolError> {
    validate_jsonl(bytes, diagnostic::SCHEMA, &["diagnostic"], |line, value| {
        serde_json::from_value::<diagnostic::DiagnosticEnvelope>(value)
            .map_err(|error| classify_json_error(line, error))
    })?;
    Ok(())
}

enum Record {
    Run,
    Entry,
    Complete,
}

fn validate_run_records(records: impl IntoIterator<Item = Record>) -> Result<(), ProtocolError> {
    let mut run = false;
    let mut complete = false;
    for record in records {
        if complete {
            return match record {
                Record::Complete => Err(ProtocolError::DuplicateComplete),
                _ => Err(ProtocolError::EventAfterComplete),
            };
        }
        match record {
            Record::Run if !run => run = true,
            Record::Run => return Err(ProtocolError::DuplicateRun),
            Record::Entry if !run => return Err(ProtocolError::MissingRun),
            Record::Entry => {}
            Record::Complete if !run => return Err(ProtocolError::MissingRun),
            Record::Complete => complete = true,
        }
    }
    if !run {
        Err(ProtocolError::MissingRun)
    } else if !complete {
        Err(ProtocolError::MissingComplete)
    } else {
        Ok(())
    }
}

fn validate_jsonl<T>(
    bytes: &[u8],
    schema: &str,
    event_types: &[&str],
    parse: impl Fn(u64, serde_json::Value) -> Result<T, ProtocolError>,
) -> Result<Vec<T>, ProtocolError> {
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(ProtocolError::TruncatedLine {
            line: bytes.iter().filter(|byte| **byte == b'\n').count() as u64 + 1,
        });
    }
    let mut events = Vec::new();
    for (offset, raw) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let line = (offset + 1) as u64;
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        if raw.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_slice(raw).map_err(|error| ProtocolError::MalformedJson {
                line,
                message: error.to_string(),
            })?;
        let actual = value
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if actual != schema {
            return Err(ProtocolError::WrongSchema {
                line,
                expected: schema.into(),
                actual: actual.into(),
            });
        }
        let event_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ProtocolError::MalformedJson {
                line,
                message: "missing string envelope type".into(),
            })?;
        if !event_types.contains(&event_type) {
            return Err(ProtocolError::UnknownEventType {
                schema: schema.into(),
                event_type: event_type.into(),
            });
        }
        events.push(parse(line, value)?);
    }
    Ok(events)
}

fn classify_json_error(line: u64, error: serde_json::Error) -> ProtocolError {
    ProtocolError::MalformedJson {
        line,
        message: error.to_string(),
    }
}
