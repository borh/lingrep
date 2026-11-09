use std::ops::Range;
use std::sync::Arc;

use crate::{
    corpus_runtime::{BoundCorpusReader, CorpusRuntime, ReadyCorpus},
    document_reader,
    reading::{self, ReadingPage, ReadingParagraphChoice, ReadingQuery, ReadingSentence},
    revision::{RevisionId, WorkbenchId},
    snapshot::PatternKey,
    state::RequestStore,
};
use askama::Template;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use lingrep_core::corpus::{MetadataScalar, MetadataValue, SentenceId};
use lingrep_corpus::{ReadingSidecar, ReadingSidecarError};
use tokio::sync::OwnedSemaphorePermit;

pub fn routes() -> Router<RequestStore> {
    Router::new()
        .route(
            "/hit-context/{workbench_id}/{revision}/{pattern_key}/{reference_ordinal}/{scope}",
            get(context),
        )
        .route("/read/{corpus}/{sentence}/{start}/{end}/{scope}", get(read))
}

async fn context(
    State(store): State<RequestStore>,
    Path((workbench_id, revision, pattern_key, reference_ordinal, scope)): Path<(
        String,
        u64,
        usize,
        usize,
        String,
    )>,
    Query(query): Query<ReadingQuery>,
) -> Response {
    if !matches!(scope.as_str(), "paragraph" | "document") {
        return invalid_request();
    }
    let workbench = WorkbenchId::new(&workbench_id);
    if let Err(error) = store
        .coordinators
        .displayed_snapshot(&workbench, RevisionId(revision))
        .await
    {
        return super::inspection_response(error.into());
    }
    let permit = store.coordinators.acquire_scan_permit().await;
    let (snapshot, inspector, ready, runtime) =
        match super::bound_inspector(&store, &workbench, RevisionId(revision)).await {
            Ok(bound) => bound,
            Err(error) => return super::inspection_response(error),
        };
    let provenance = super::BoundProvenance::open_or_degrade(&ready).await;
    let reference = PatternKey::new(pattern_key)
        .and_then(|key| snapshot.match_ref(key, reference_ordinal))
        .cloned();
    let Some(reference) = reference else {
        return super::inspection_response(super::EndpointInspectionError::UnknownReference);
    };
    let Some(provenance) = provenance else {
        return super::sanitized(
            StatusCode::SERVICE_UNAVAILABLE,
            "Reading context is unavailable for this corpus.",
        );
    };
    respond(
        &store,
        permit,
        &ready,
        &runtime,
        inspector.reader_clone(),
        Opening {
            provenance,
            sentence: reference.sentence_index,
            span: reference.span,
            query,
            root_url: format!(
                "api/hit-context/{workbench_id}/{revision}/{pattern_key}/{reference_ordinal}"
            ),
            target_id: format!("reading-body-{pattern_key}-{reference_ordinal}"),
            document: scope == "document",
            search_scope: snapshot.search_scope.clone(),
        },
    )
    .await
}

/// The element of the frequency page that `read` fragments are written into.
const READER_TARGET: &str = "document-reader-body";

/// A sentence of a served corpus in its document, with the words `start`
/// to `end` marked: the reader a frequency line opens.
async fn read(
    State(store): State<RequestStore>,
    Path((corpus, sentence, start, end, scope)): Path<(String, u64, usize, usize, String)>,
    Query(query): Query<ReadingQuery>,
) -> Response {
    if !matches!(scope.as_str(), "paragraph" | "document") || start >= end {
        return invalid_request();
    }
    let Some(slot) = store.corpora.resolve(Some(&corpus)) else {
        return super::sanitized(StatusCode::NOT_FOUND, "Unknown corpus.");
    };
    let runtime = Arc::clone(&slot.runtime);
    runtime.start_or_observe().await;
    let Some(ready) = runtime.ready().await else {
        return super::sanitized(StatusCode::SERVICE_UNAVAILABLE, "The corpus is not ready.");
    };
    let permit = store.coordinators.acquire_reading_permit().await;
    let reader = ready
        .reader()
        .ok()
        .and_then(|reader| reader.bind(&ready.identity));
    let (Some(reader), Some(provenance)) = (
        reader,
        super::BoundProvenance::open_or_degrade(&ready).await,
    ) else {
        return super::sanitized(
            StatusCode::SERVICE_UNAVAILABLE,
            "Reading context is unavailable for this corpus.",
        );
    };
    if !ready.sentence_count().is_ok_and(|count| sentence < count) {
        return invalid_request();
    }
    match reader.sentence(sentence).await {
        Ok(Some(tokens)) if end <= tokens.len() => {}
        Ok(_) => return invalid_request(),
        Err(error) => {
            tracing::warn!(%error, "reading context failed");
            runtime.invalidator().invalidate_corrupt().await;
            return super::inspection_response(super::EndpointInspectionError::CacheCorruption);
        }
    }
    respond(
        &store,
        permit,
        &ready,
        &runtime,
        reader,
        Opening {
            provenance,
            sentence,
            span: start..end,
            query,
            root_url: format!("api/read/{corpus}/{sentence}/{start}/{end}"),
            target_id: READER_TARGET.into(),
            document: scope == "document",
            search_scope: crate::search_scope::SearchScope::default(),
        },
    )
    .await
}

struct Opening {
    provenance: super::BoundProvenance,
    sentence: u64,
    span: Range<usize>,
    query: ReadingQuery,
    root_url: String,
    target_id: String,
    document: bool,
    search_scope: crate::search_scope::SearchScope,
}

/// The rendered document when `opening` asks for the document and a sidecar
/// renders `ready`'s corpus, and the sentence reader otherwise.
async fn respond(
    store: &RequestStore,
    permit: OwnedSemaphorePermit,
    ready: &ReadyCorpus,
    runtime: &CorpusRuntime,
    reader: BoundCorpusReader,
    opening: Opening,
) -> Response {
    let sidecar = if opening.document {
        store.reading_sidecars.for_artifact(&ready.artifact)
    } else {
        None
    };
    let blocks = opening.query.blocks();
    let Some(sidecar) = sidecar else {
        if blocks.is_some() {
            return invalid_request();
        }
        return sentences(permit, runtime, reader, opening).await;
    };
    if opening.query.moves_sentences() {
        return invalid_request();
    }
    let result = match blocks {
        None => rendered(permit, &opening, sidecar).await,
        Some(Ok((before, block))) => rendered_pages(permit, &opening, sidecar, before, block).await,
        Some(Err(())) => return invalid_request(),
    };
    match result {
        Ok(html) if html.len() <= reading::MAX_READING_HTML_BYTES => Html(html).into_response(),
        Ok(_) => super::sanitized(
            StatusCode::SERVICE_UNAVAILABLE,
            "Reading context exceeds the display limits.",
        ),
        Err(Failure::Sidecar(ReadingSidecarError::OutOfRange(_))) => invalid_request(),
        Err(Failure::Sidecar(error)) => {
            tracing::warn!(%error, "reading sidecar failed");
            super::sanitized(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The rendered document could not be read.",
            )
        }
        Err(Failure::Corpus(error)) => {
            tracing::warn!(%error, "reading context failed");
            runtime.invalidator().invalidate_corrupt().await;
            super::inspection_response(super::EndpointInspectionError::CacheCorruption)
        }
        Err(Failure::Task) => super::sanitized(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Reading context could not be loaded.",
        ),
    }
}

async fn sentences(
    permit: OwnedSemaphorePermit,
    runtime: &CorpusRuntime,
    reader: BoundCorpusReader,
    opening: Opening,
) -> Response {
    let handle = tokio::runtime::Handle::current();
    let result =
        run_reading_task(permit, move || handle.block_on(load_page(&opening, reader))).await;
    match result {
        Ok(Ok(Some(page))) => match page.render() {
            Ok(html) if html.len() <= reading::MAX_READING_HTML_BYTES => Html(html).into_response(),
            _ => super::sanitized(
                StatusCode::SERVICE_UNAVAILABLE,
                "Reading context exceeds the display limits.",
            ),
        },
        Ok(Ok(None)) => invalid_request(),
        Ok(Err(error)) => {
            tracing::warn!(%error, "reading context failed");
            runtime.invalidator().invalidate_corrupt().await;
            super::inspection_response(super::EndpointInspectionError::CacheCorruption)
        }
        Err(_) => super::sanitized(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Reading context could not be loaded.",
        ),
    }
}

enum Failure {
    Sidecar(ReadingSidecarError),
    Corpus(lingrep_corpus::CorpusError),
    Task,
}

impl From<lingrep_corpus::CorpusError> for Failure {
    fn from(error: lingrep_corpus::CorpusError) -> Self {
        Self::Corpus(error)
    }
}

impl From<ReadingSidecarError> for Failure {
    fn from(error: ReadingSidecarError) -> Self {
        Self::Sidecar(error)
    }
}

impl From<tokio::task::JoinError> for Failure {
    fn from(_: tokio::task::JoinError) -> Self {
        Self::Task
    }
}

/// The page of the selected sentence's document around it, from `sidecar`.
async fn rendered(
    permit: OwnedSemaphorePermit,
    opening: &Opening,
    sidecar: Arc<ReadingSidecar>,
) -> Result<String, Failure> {
    let (mut page, _) = header(opening).await?;
    let analysis = opening
        .provenance
        .view
        .sentence_analysis_bounded(
            SentenceId::new(opening.sentence),
            reading::MAX_READING_SENTENCE_BYTES,
        )
        .await?;
    let chars = analysis
        .as_ref()
        .and_then(|analysis| document_reader::char_range(analysis, opening.span.clone()));
    let sentence = opening.sentence;
    let (permit, read) = blocking(permit, move || {
        document_reader::open(&sidecar, sentence, chars)
    })
    .await?;
    let (blocks, highlight, anchoring) = read?;
    let admitted = admissions(opening, &blocks).await?;
    drop(permit);
    page.rendered = Some(document_reader::RenderedDocument {
        highlight,
        anchoring: if analysis.is_none() {
            document_reader::Anchoring::OverLimit
        } else {
            anchoring
        },
        scoped: !opening.search_scope.is_unrestricted(),
        pages: document_reader::DocumentPages::of(
            format!("{}/document", opening.root_url),
            blocks,
            &admitted,
            true,
            true,
        ),
    });
    page.render().map_err(|_| Failure::Task)
}

/// The blocks of the selected sentence's document before `block`, or from
/// `block` on, from `sidecar`.
async fn rendered_pages(
    permit: OwnedSemaphorePermit,
    opening: &Opening,
    sidecar: Arc<ReadingSidecar>,
    before: bool,
    block: u32,
) -> Result<String, Failure> {
    let document = opening
        .provenance
        .view
        .reading_ranges(SentenceId::new(opening.sentence))
        .await?
        .document_id;
    let (permit, read) = blocking(permit, move || {
        document_reader::turn(&sidecar, document, before, block)
    })
    .await?;
    let blocks = read?;
    let admitted = admissions(opening, &blocks).await?;
    drop(permit);
    document_reader::DocumentPages::of(
        format!("{}/document", opening.root_url),
        blocks,
        &admitted,
        before,
        !before,
    )
    .render()
    .map_err(|_| Failure::Task)
}

/// Whether the search scope admits each sentence with rendered text on
/// `page`.
async fn admissions(
    opening: &Opening,
    page: &lingrep_corpus::ReadingPage,
) -> Result<Vec<bool>, lingrep_corpus::CorpusError> {
    let sentences = document_reader::sentences(page);
    let scope = &opening.search_scope;
    if scope.is_unrestricted() {
        return Ok(vec![true; sentences.len()]);
    }
    opening
        .provenance
        .view
        .section_admissions(
            &sentences,
            scope.included_sections.as_deref(),
            scope.include_unlabelled,
        )
        .await
}

fn file_name(source_name: &str) -> String {
    std::path::Path::new(source_name)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn run_reading_task<T: Send + 'static>(
    permit: OwnedSemaphorePermit,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}

/// Runs `work` on the blocking pool holding `permit`, and returns the
/// permit with its result, so that a dropped request releases the permit
/// only when `work` ends.
async fn blocking<T: Send + 'static>(
    permit: OwnedSemaphorePermit,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<(OwnedSemaphorePermit, T), tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let output = work();
        (permit, output)
    })
    .await
}

fn invalid_request() -> Response {
    super::sanitized(
        StatusCode::BAD_REQUEST,
        "Invalid reading scope or page. Open context again from the retained hit.",
    )
}

/// The reading page's title, source and metadata for the selected
/// sentence, with the sentence's document and paragraph ranges.
async fn header(
    opening: &Opening,
) -> Result<(ReadingPage, lingrep_corpus::SentenceReadingRanges), lingrep_corpus::CorpusError> {
    let provenance = &opening.provenance;
    let ranges = provenance
        .view
        .reading_ranges(SentenceId::new(opening.sentence))
        .await?;
    let source = provenance
        .artifact
        .sentence_provenance(&provenance.view, opening.sentence)
        .await?;
    if source.location.document_id != ranges.document_id {
        return Err(lingrep_corpus::CorpusError::InvalidFormat {
            component: "reading context",
            message: "document identity differs between location and provenance".into(),
        });
    }
    let mut page = ReadingPage {
        show_excluded: opening.query.show_excluded,
        root_url: opening.root_url.clone(),
        target_id: opening.target_id.clone(),
        source: file_name(&source.location.source_name),
        document: opening.document,
        metadata: Vec::new(),
        metadata_omitted: false,
        paragraph_query: String::new(),
        paragraph_choices: Vec::new(),
        paragraph_missing: false,
        sentences: Vec::new(),
        previous: None,
        next: None,
        rendered: None,
    };
    for field in crate::provenance_view::metadata_fields(source.rich.metadata_json.as_deref()) {
        page.push_metadata_bounded(field);
    }
    Ok((page, ranges))
}

async fn load_page(
    opening: &Opening,
    reader: BoundCorpusReader,
) -> Result<Option<ReadingPage>, lingrep_corpus::CorpusError> {
    let provenance = &opening.provenance;
    let (sentence, span, query, document) = (
        opening.sentence,
        opening.span.clone(),
        &opening.query,
        opening.document,
    );
    let search_scope = &opening.search_scope;
    let (mut page, ranges) = header(opening).await?;
    let range = if document {
        if query.paragraph.is_some() {
            return Ok(None);
        }
        ranges.document.clone()
    } else {
        match (query.paragraph, ranges.paragraphs.as_slice()) {
            (Some(id), paragraphs) => {
                let Some(paragraph) = paragraphs.iter().find(|paragraph| paragraph.id == id) else {
                    return Ok(None);
                };
                page.paragraph_query = format!("paragraph={id}&");
                paragraph.sentences.clone()
            }
            (None, [paragraph]) => {
                page.paragraph_query = format!("paragraph={}&", paragraph.id);
                paragraph.sentences.clone()
            }
            (None, []) => {
                page.paragraph_missing = true;
                return Ok(Some(page));
            }
            (None, paragraphs) => {
                page.paragraph_choices = paragraphs
                    .iter()
                    .map(|paragraph| ReadingParagraphChoice {
                        id: paragraph.id,
                        first: paragraph.sentences.start - ranges.document.start + 1,
                        last: paragraph.sentences.end - ranges.document.start,
                    })
                    .collect();
                return Ok(Some(page));
            }
        }
    };
    let Some(ordinals) = reading::page_ordinals(range.clone(), sentence, query) else {
        return Ok(None);
    };
    let admissions = if search_scope.is_unrestricted() {
        vec![true; ordinals.len()]
    } else {
        provenance
            .view
            .section_admissions(
                &ordinals
                    .iter()
                    .copied()
                    .map(SentenceId::new)
                    .collect::<Vec<_>>(),
                search_scope.included_sections.as_deref(),
                search_scope.include_unlabelled,
            )
            .await?
    };
    for (ordinal, admitted) in ordinals.into_iter().zip(admissions) {
        let analysis = if !admitted && !query.show_excluded {
            None
        } else {
            provenance
                .view
                .sentence_analysis_bounded(
                    SentenceId::new(ordinal),
                    reading::MAX_READING_SENTENCE_BYTES,
                )
                .await?
        };
        let selected = ordinal == sentence;
        let mut sentence = ReadingSentence {
            excluded: !admitted,
            ordinal,
            number: ordinal - ranges.document.start + 1,
            selected,
            left: String::new(),
            key: String::new(),
            right: String::new(),
            reconstructed: false,
            omitted: false,
            paragraph_starts: Vec::new(),
            sections: Vec::new(),
        };
        let rich = provenance
            .view
            .rich_sentence_page(&[SentenceId::new(ordinal)])
            .await?
            .pop()
            .flatten()
            .ok_or_else(|| lingrep_corpus::CorpusError::InvalidFormat {
                component: "reading context",
                message: "sentence metadata is missing".into(),
            })?;
        for division in rich
            .metadata
            .structure
            .divisions
            .iter()
            .filter(|division| division.kind == "section")
        {
            for (key, value) in &division.metadata {
                if key == "title"
                    && let MetadataValue::Scalar(MetadataScalar::String(title)) = value
                    && !title.is_empty()
                    && !sentence.sections.contains(title)
                {
                    sentence.sections.push(title.clone());
                }
            }
        }
        for label in crate::provenance_view::section_labels(&rich.active_regions) {
            if !sentence.sections.contains(&label) {
                sentence.sections.push(label);
            }
        }
        if let Some(analysis) = analysis {
            let highlight = selected
                .then(|| reading::source_highlight(&analysis, span.clone()))
                .flatten();
            if (!selected || highlight.is_some()) && !analysis.text.is_empty() {
                if let Some(marked) = highlight {
                    sentence.left = analysis.text.get(..marked.start).unwrap_or_default().into();
                    sentence.key = analysis.text.get(marked.clone()).unwrap_or_default().into();
                    sentence.right = analysis.text.get(marked.end..).unwrap_or_default().into();
                } else {
                    sentence.left = analysis.text;
                }
            } else {
                let words = reader
                    .sentence_words_bounded(ordinal, reading::MAX_READING_SENTENCE_BYTES)
                    .map_err(|error| lingrep_corpus::CorpusError::InvalidFormat {
                        component: "reading context",
                        message: error.to_string(),
                    })?;
                if let Some(words) = words {
                    sentence.reconstructed = true;
                    if selected {
                        if span.end > words.len() || span.start >= span.end {
                            return Err(lingrep_corpus::CorpusError::InvalidFormat {
                                component: "reading context",
                                message: "selected word span is invalid".into(),
                            });
                        }
                        for (index, word) in words.into_iter().enumerate() {
                            if index < span.start {
                                sentence.left.push_str(&word);
                            } else if index < span.end {
                                sentence.key.push_str(&word);
                            } else {
                                sentence.right.push_str(&word);
                            }
                        }
                    } else {
                        sentence.left = words.concat();
                    }
                } else {
                    sentence.omitted = true;
                }
            }
        } else {
            sentence.omitted = true;
        }
        let sentence_ranges = provenance
            .view
            .reading_ranges(SentenceId::new(ordinal))
            .await?;
        if sentence_ranges.document_id != ranges.document_id {
            return Err(lingrep_corpus::CorpusError::InvalidFormat {
                component: "reading context",
                message: "context crossed a document boundary".into(),
            });
        }
        sentence.paragraph_starts = sentence_ranges
            .paragraphs
            .iter()
            .filter(|paragraph| paragraph.sentences.start == ordinal)
            .map(|paragraph| paragraph.id)
            .collect();
        if !page.push_bounded(sentence) {
            break;
        }
    }
    if query.before.is_some() {
        page.sentences.reverse();
    }
    let mut previous_sections = None;
    for sentence in &mut page.sentences {
        if previous_sections.as_ref() == Some(&sentence.sections) {
            sentence.sections.clear();
        } else {
            previous_sections = Some(sentence.sections.clone());
        }
    }
    if let Some(first) = page.sentences.first() {
        page.previous = (first.ordinal > range.start).then_some(first.ordinal);
    }
    if let Some(last) = page.sentences.last() {
        page.next = (last.ordinal + 1 < range.end).then_some(last.ordinal + 1);
    }
    Ok(Some(page))
}

#[cfg(test)]
mod tests {
    #[hegel::test(test_cases = 40, derandomize = true)]
    fn cancelled_reading_requests_retain_the_permit_until_blocking_work_finishes(
        tc: hegel::TestCase,
    ) {
        use std::sync::Arc;
        use tokio::sync::Semaphore;
        let capacity = tc.draw(
            hegel::generators::integers::<usize>()
                .min_value(1)
                .max_value(Semaphore::MAX_PERMITS),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let semaphore = Arc::new(Semaphore::new(capacity));
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let (started, running) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let request = tokio::spawn(super::run_reading_task(permit, move || {
                started.send(()).unwrap();
                wait.recv().unwrap();
            }));
            running.await.unwrap();
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
            assert_eq!(semaphore.available_permits(), capacity - 1);
            release.send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while semaphore.available_permits() != capacity {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
    }
}
