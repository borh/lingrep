use crate::{
    corpus_runtime::BoundCorpusReader,
    reading::{self, ReadingPage, ReadingParagraphChoice, ReadingQuery, ReadingSentence},
    revision::{RevisionId, WorkbenchId},
    snapshot::{MatchRef, PatternKey},
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

pub fn routes() -> Router<RequestStore> {
    Router::new().route(
        "/hit-context/{workbench_id}/{revision}/{pattern_key}/{reference_ordinal}/{scope}",
        get(context),
    )
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
    let reader = inspector.reader_clone();
    let root_url =
        format!("api/hit-context/{workbench_id}/{revision}/{pattern_key}/{reference_ordinal}");
    let target_id = format!("reading-body-{pattern_key}-{reference_ordinal}");
    let handle = tokio::runtime::Handle::current();
    let search_scope = snapshot.search_scope.clone();
    let result = run_reading_task(permit, move || {
        handle.block_on(load_page(
            &provenance,
            reader,
            reference,
            query,
            root_url,
            target_id,
            scope == "document",
            search_scope,
        ))
    })
    .await;
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

async fn run_reading_task<T: Send + 'static>(
    permit: tokio::sync::OwnedSemaphorePermit,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}

fn invalid_request() -> Response {
    super::sanitized(
        StatusCode::BAD_REQUEST,
        "Invalid reading scope or page. Open context again from the retained hit.",
    )
}

async fn load_page(
    provenance: &super::BoundProvenance,
    reader: BoundCorpusReader,
    reference: MatchRef,
    query: ReadingQuery,
    root_url: String,
    target_id: String,
    document: bool,
    search_scope: crate::search_scope::SearchScope,
) -> Result<Option<ReadingPage>, lingrep_corpus::CorpusError> {
    let ranges = provenance
        .view
        .reading_ranges(SentenceId::new(reference.sentence_index))
        .await?;
    let source = provenance
        .artifact
        .sentence_provenance(&provenance.view, reference.sentence_index)
        .await?;
    if source.location.document_id != ranges.document_id {
        return Err(lingrep_corpus::CorpusError::InvalidFormat {
            component: "reading context",
            message: "document identity differs between location and provenance".into(),
        });
    }
    let mut page = ReadingPage {
        show_excluded: query.show_excluded,
        root_url,
        target_id,
        source: std::path::Path::new(source.location.source_name.as_ref())
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        document,
        metadata: Vec::new(),
        metadata_omitted: false,
        paragraph_query: String::new(),
        paragraph_choices: Vec::new(),
        paragraph_missing: false,
        sentences: Vec::new(),
        previous: None,
        next: None,
    };
    for field in crate::provenance_view::metadata_fields(source.rich.metadata_json.as_deref()) {
        page.push_metadata_bounded(field);
    }
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
    let Some(ordinals) = reading::page_ordinals(range.clone(), reference.sentence_index, &query)
    else {
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
        let selected = ordinal == reference.sentence_index;
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
                .then(|| reading::source_highlight(&analysis, reference.span.clone()))
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
                        if reference.span.end > words.len()
                            || reference.span.start >= reference.span.end
                        {
                            return Err(lingrep_corpus::CorpusError::InvalidFormat {
                                component: "reading context",
                                message: "selected word span is invalid".into(),
                            });
                        }
                        for (index, word) in words.into_iter().enumerate() {
                            if index < reference.span.start {
                                sentence.left.push_str(&word);
                            } else if index < reference.span.end {
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
