//! The standalone Sample debugger and the sample form shared with Workbench.
//!
//! The page holds no workbench and does not re-post the corpus search's live
//! pattern. A hit's debug link names a workbench revision, which the page
//! reads once to seed the pattern. A sentence reference opens its stored tokens.
//! For edited or pasted text, Automatic selects the live analyzer by its
//! recorded semantic identity when the request names a corpus.
//!
//! Admission runs in strict order so the cheapest checks reject first: size
//! caps (413) need no dictionary or worker slot; the semaphore (429) needs no
//! dictionary. The blocking task retains the worker slot until analysis ends,
//! including after request cancellation. `store.context.plan()` is resolved
//! *inside* the blocking closure because its first-call dictionary load is
//! synchronous I/O that must not run on the async executor; a load failure
//! surfaces as 503.

use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use askama::Template;
use axum::{
    Form, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::corpus_runtime::{CacheReadiness, CorpusRuntime, ReadyCorpus};
use crate::sample::analyze_sample;
use crate::state::{RequestStore, TokenizerUnavailable};
use crate::templates::{SampleResults, render_error};

// Matches `handlers::search::MAX_PATTERN_BYTES` and the compiler's 1 MiB
// source-byte ceiling, so a real grammar file can also be tested on sample text.
const MAX_PATTERN_BYTES: usize = 1 << 20;
const MAX_SAMPLE_BYTES: usize = 64 * 1024;
const MAX_SAMPLE_LINES: usize = 2000;

// Process-wide bound on concurrent sample analyses (each is a blocking job).
static SAMPLE_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| {
    Semaphore::new(std::thread::available_parallelism().map_or(2, |n| n.get().div_ceil(2)))
});

#[derive(Deserialize)]
pub struct SampleForm {
    pub pattern: String,
    pub sample: String,
    /// Corpus key for comparing the corpus and sample token schemas.
    #[serde(default)]
    pub corpus: Option<String>,
    #[serde(default)]
    analyzer: SampleAnalyzerChoice,
    /// Corpus sentence ordinal whose stored analysis replaces tokenizing
    /// `sample`. The page stops sending it once the text is edited.
    #[serde(default)]
    pub corpus_sentence: Option<u64>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SampleAnalyzerChoice {
    #[default]
    Auto,
    Vibrato,
    Configured,
}

/// A corpus sentence's stored tokens, its token schema and its analyzer's name.
type StoredSentence = (
    lingrep_core::token_match::BoundTokenSentence,
    lingrep_core::token_match::TokenSchema,
    String,
);

async fn ready_corpus(runtime: &CorpusRuntime) -> Option<ReadyCorpus> {
    runtime.start_or_observe().await;
    let mut readiness = runtime.readiness();
    tokio::time::timeout(
        Duration::from_secs(2),
        readiness.wait_for(|state| {
            matches!(
                state,
                CacheReadiness::Ready { .. } | CacheReadiness::Failed { .. }
            )
        }),
    )
    .await
    .ok()?
    .ok()?;
    runtime.ready().await
}

/// `None` when the corpus is unknown or not ready, or the sentence is absent
/// or larger than the sample limit.
async fn stored_sentence(
    store: &RequestStore,
    corpus: Option<&str>,
    sentence: u64,
) -> Option<StoredSentence> {
    let slot = store.corpora.resolve(corpus)?;
    let ready = ready_corpus(&slot.runtime).await?;
    // Stored sentences carry the schema's token attributes; the enhanced
    // graph is stored apart from them and is not read here.
    let schema = lingrep_core::token_match::TokenSchema::new(
        ready
            .artifact
            .token_schema()
            .ok()?
            .attributes()
            .iter()
            .filter(|attribute| {
                attribute.provider() != lingrep_core::token_match::AttributeProvider::EnhancedGraph
            })
            .cloned()
            .collect::<Vec<_>>(),
    )
    .ok()?;
    let analyzer = ready.artifact.analyzer();
    let name = format!("{} {}", analyzer.family, analyzer.profile);
    let reader = ready.reader().ok()?.bind(&ready.identity)?;
    let words = tokio::task::spawn_blocking(move || {
        reader.sentence_attributes_bounded(sentence, MAX_SAMPLE_BYTES)
    })
    .await
    .ok()?
    .ok()??;
    Some((words, schema, name))
}

pub(crate) fn debug_href(
    corpus: Option<&str>,
    search: Option<(&str, u64)>,
    sentence: u64,
) -> String {
    let sentence = sentence.to_string();
    let mut query = vec![("sentence", sentence.as_str())];
    query.extend(corpus.map(|corpus| ("corpus", corpus)));
    let revision;
    if let Some((workbench, id)) = search {
        revision = id.to_string();
        query.extend([("workbench", workbench), ("revision", &revision)]);
    }
    format!(
        "sample?{}",
        serde_urlencoded::to_string(query).unwrap_or_default()
    )
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/sample", post(run_sample))
}

/// The `/sample` page, mounted outside `/api` beside the other pages.
pub fn page_routes() -> Router<RequestStore> {
    Router::new().route("/sample", get(sample_page))
}

#[derive(Deserialize)]
struct SampleQuery {
    corpus: Option<String>,
    sentence: Option<u64>,
    /// With `revision`, the Workbench run whose pattern seeds the page.
    workbench: Option<String>,
    revision: Option<u64>,
}

/// The corpus sentence the page opens with.
struct OpenedSentence {
    corpus: String,
    ordinal: u64,
    text: String,
}

#[derive(Template)]
#[template(path = "sample.html")]
struct SamplePage {
    opened: Option<OpenedSentence>,
    opened_unavailable: bool,
    seed_pattern: String,
    current_page: &'static str,
    frequency_view: bool,
    catalog_keys: Vec<String>,
    corpus: Option<String>,
    analyzer_choice: bool,
    configured_analyzer: Option<String>,
    vibrato_available: bool,
}

/// What the page opens with when nothing else seeds it: one definition with
/// one capture that matches two of the three default sample sentences, so the
/// first view shows a match, a capture and an unmatched sentence.
const STARTER_PATTERN: &str = "\
# A verb of thinking followed by a passive auxiliary: 考えられる, 思われる.
pattern thought := (?<verb> [lemma in {考える, 思う}]) [lemma in {られる, れる}].
";

/// Seeded from the pattern of the Workbench run a hit's debug link names while
/// that run is displayed, otherwise from the `--patterns` file when the
/// operator named one, and otherwise from [`STARTER_PATTERN`]. Each page's
/// pattern box then persists its own draft under its own id.
async fn sample_page(
    State(store): State<RequestStore>,
    axum::extract::Query(query): axum::extract::Query<SampleQuery>,
) -> Html<String> {
    let mut opened = None;
    if let Some(ordinal) = query.sentence
        && let Some((words, ..)) = stored_sentence(&store, query.corpus.as_deref(), ordinal).await
        && let Some(slot) = store.corpora.resolve(query.corpus.as_deref())
    {
        opened = Some(OpenedSentence {
            corpus: slot.name.as_str().to_owned(),
            ordinal,
            text: lingrep_core::token_match::display_text(&words, 0..words.len()),
        });
    }
    let mut seed_pattern = None;
    if let (Some(workbench), Some(revision)) = (query.workbench.as_deref(), query.revision)
        && let Ok(snapshot) = store
            .coordinators
            .displayed_snapshot(
                &crate::revision::WorkbenchId::new(workbench),
                crate::revision::RevisionId(revision),
            )
            .await
    {
        seed_pattern = Some(snapshot.source.to_string());
    }
    let corpus = opened
        .as_ref()
        .map(|sentence| sentence.corpus.clone())
        .or(query.corpus);
    let analyzer_choice = store.sample_analyzer.is_some() || corpus.is_some();
    SamplePage {
        configured_analyzer: store.sample_analyzer.as_ref().map(|analyzer| analyzer.name.clone()),
        vibrato_available: store.context.dict_path().is_some(),
        opened_unavailable: query.sentence.is_some() && opened.is_none(),
        opened,
        corpus,
        analyzer_choice,
        seed_pattern: seed_pattern.unwrap_or_else(|| {
            store
                .sample_seed
                .as_deref()
                .unwrap_or(STARTER_PATTERN)
                .to_owned()
        }),
        current_page: "sample",
        frequency_view: !store.frequency.is_empty(),
        catalog_keys: store.catalogs.keys().map(str::to_owned).collect(),
    }
    .render().map_or_else(|_| {
        Html(
            "<div class=\"alert alert-error\" role=\"status\">Unable to render page. Please retry.</div>"
                .to_string(),
        )
    }, Html)
}

fn fragment(status: StatusCode, key: &str, message: &str) -> Response {
    // Shared `ErrorFragment` markup with Askama escaping; the plain-text
    // fallback is unreachable for this static template but keeps the handler
    // panic-free. Messages are fixed literals — never interpolate user input.
    let html = render_error(key, message).unwrap_or_else(|_| message.to_owned());
    (status, Html(html)).into_response()
}

pub async fn run_sample(
    State(store): State<RequestStore>,
    Form(form): Form<SampleForm>,
) -> Response {
    if form.pattern.len() > MAX_PATTERN_BYTES
        || form.sample.len() > MAX_SAMPLE_BYTES
        || form.sample.lines().count() > MAX_SAMPLE_LINES
    {
        return fragment(
            StatusCode::PAYLOAD_TOO_LARGE,
            "sample.too_large",
            "The sample exceeds the test limits. Shorten it or search a corpus with this pattern.",
        );
    }
    let Ok(permit) = SAMPLE_SLOTS.try_acquire() else {
        return fragment(
            StatusCode::TOO_MANY_REQUESTS,
            "sample.busy",
            "Server busy analyzing samples. Try again in a moment.",
        );
    };
    let pattern = form.pattern;
    let sample = form.sample;
    if let Some(sentence) = form
        .corpus_sentence
        .filter(|_| form.analyzer == SampleAnalyzerChoice::Auto)
    {
        let Some((words, schema, analyzer)) =
            stored_sentence(&store, form.corpus.as_deref(), sentence).await
        else {
            return fragment(
                StatusCode::NOT_FOUND,
                "sample.stored_unavailable",
                "This corpus sentence could not be read. Refresh the page or choose a live analyzer.",
            );
        };
        let report = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::sample::analyze_stored_sentence(&pattern, &schema, &words, analyzer)
        })
        .await;
        return match report {
            Ok(report) => render_report(report),
            Err(_) => fragment(
                StatusCode::INTERNAL_SERVER_ERROR,
                "sample.render_failed",
                "Could not render sample results.",
            ),
        };
    }
    let mut corpus_record = None;
    if let Some(name) = form.corpus.as_deref().filter(|name| !name.is_empty()) {
        if let Some(slot) = store.corpora.resolve(Some(name)) {
            corpus_record = ready_corpus(&slot.runtime)
                .await
                .map(|ready| ready.artifact.analyzer().clone());
        } else if form.analyzer == SampleAnalyzerChoice::Auto {
            return fragment(
                StatusCode::NOT_FOUND,
                "sample.corpus_unavailable",
                "This corpus is unavailable.",
            );
        }
    }
    let bound_live = store.sample_analyzer.as_ref().filter(|analyzer| {
        corpus_record
            .as_ref()
            .is_some_and(|corpus| corpus.semantic_identity == analyzer.semantic_identity)
    });
    let live = match form.analyzer {
        SampleAnalyzerChoice::Auto if form.corpus.as_deref().is_none_or(str::is_empty) => {
            store.sample_analyzer.as_ref()
        }
        SampleAnalyzerChoice::Auto => bound_live,
        SampleAnalyzerChoice::Configured => store.sample_analyzer.as_ref(),
        SampleAnalyzerChoice::Vibrato => None,
    };
    if let Some(analyzer) = live {
        let semantic_identity = analyzer.semantic_identity.clone();
        let result = Arc::clone(analyzer).analyze(pattern, sample, permit).await;
        return match result {
            Ok(mut report) => {
                if let Some(run) = &mut report.run {
                    run.compare_with_corpus(&semantic_identity, corpus_record.as_ref());
                }
                render_report(report)
            }
            Err(crate::sample_analyzer::LiveSampleError::TimedOut) => fragment(
                StatusCode::GATEWAY_TIMEOUT,
                "sample.analysis_timeout",
                "The analyzer took too long. Try a shorter sample.",
            ),
            Err(crate::sample_analyzer::LiveSampleError::Failed) => fragment(
                StatusCode::SERVICE_UNAVAILABLE,
                "sample.analysis_failed",
                "The configured analyzer could not analyze this sample. Try again or choose another analyzer.",
            ),
        };
    }
    if form.analyzer == SampleAnalyzerChoice::Configured {
        return fragment(
            StatusCode::SERVICE_UNAVAILABLE,
            "sample.analysis_unconfigured",
            "No live sample analyzer is configured.",
        );
    }
    if form.analyzer == SampleAnalyzerChoice::Auto
        && corpus_record
            .as_ref()
            .is_some_and(|corpus| corpus.family != "vibrato" || store.context.dict_path().is_none())
    {
        return missing_corpus_analyzer();
    }
    if form.analyzer == SampleAnalyzerChoice::Auto
        && form.corpus.as_deref().is_some_and(|name| !name.is_empty())
        && corpus_record.is_none()
    {
        return fragment(
            StatusCode::SERVICE_UNAVAILABLE,
            "sample.corpus_unavailable",
            "This corpus is unavailable.",
        );
    }
    // The tokenizer's first-call dictionary load is synchronous I/O; resolve it
    // inside the blocking pool so the async executor never blocks. A load
    // failure returns `Err(TokenizerUnavailable)` (503); a genuine join failure
    // is the outer `Err` (500).
    let outcome = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let plan = store.context.plan()?;
        let analyzer = lingrep_corpus::AnalyzerRecord::from_descriptor(plan.descriptor());
        if form.analyzer == SampleAnalyzerChoice::Auto
            && corpus_record
                .as_ref()
                .is_some_and(|corpus| corpus.semantic_identity != analyzer.semantic_identity)
        {
            return Ok(None);
        }
        let dictionary = store
            .context
            .dict_path()
            .and_then(|path| path.file_name())
            .map(|name| format!(" · {}", name.to_string_lossy()))
            .unwrap_or_default();
        let mut report = analyze_sample(
            &pattern,
            &sample,
            &plan,
            format!("{} {}{dictionary}", analyzer.family, analyzer.profile),
        );
        if let Some(run) = &mut report.run {
            run.compare_with_corpus(&analyzer.semantic_identity, corpus_record.as_ref());
        }
        Ok::<_, TokenizerUnavailable>(Some(report))
    })
    .await;
    match outcome {
        Ok(Ok(Some(report))) => render_report(report),
        Ok(Ok(None)) => missing_corpus_analyzer(),
        Ok(Err(TokenizerUnavailable::NoDictionary)) => fragment(
            StatusCode::SERVICE_UNAVAILABLE,
            "sample.unavailable",
            "Sample analysis requires a dictionary. Run `ling dictionary install` or pass \
             --dict-path, then restart the server.",
        ),
        Ok(Err(TokenizerUnavailable::LoadFailed)) => fragment(
            StatusCode::SERVICE_UNAVAILABLE,
            "sample.unavailable",
            "Sample analysis is unavailable because the tokenizer dictionary could not be loaded.",
        ),
        Err(_) => fragment(
            StatusCode::INTERNAL_SERVER_ERROR,
            "sample.failed",
            "Sample analysis failed.",
        ),
    }
}

fn missing_corpus_analyzer() -> Response {
    fragment(
        StatusCode::CONFLICT,
        "sample.analyzer_mismatch",
        "Live analysis for this corpus is unavailable. Choose another analyzer or open a stored sentence.",
    )
}

fn render_report(report: crate::sample::SampleReport) -> Response {
    SampleResults {
        analysis: report.analysis,
        run: report.run,
    }
    .render()
    .map(Html)
    .map_or_else(
        |_| {
            fragment(
                StatusCode::INTERNAL_SERVER_ERROR,
                "sample.render_failed",
                "Could not render sample results.",
            )
        },
        IntoResponse::into_response,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_a_queued_sample_retains_its_worker_slot() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let (started, start) = std::sync::mpsc::channel();
        let blocker = runtime.spawn_blocking(move || {
            started.send(()).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        });
        start
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let before = SAMPLE_SLOTS.available_permits();
        let during = runtime.block_on(async {
            {
                let pending = run_sample(
                    State(RequestStore::default()),
                    Form(SampleForm {
                        pattern: "p := [].".into(),
                        sample: "猫".into(),
                        corpus: None,
                        corpus_sentence: None,
                        analyzer: SampleAnalyzerChoice::Auto,
                    }),
                );
                tokio::pin!(pending);
                assert!(futures::poll!(&mut pending).is_pending());
            }
            SAMPLE_SLOTS.available_permits()
        });
        release.send(()).unwrap();
        runtime.block_on(blocker).unwrap();
        drop(runtime);
        assert_eq!(during, before - 1);
        assert_eq!(SAMPLE_SLOTS.available_permits(), before);
    }
}
