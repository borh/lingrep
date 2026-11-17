use std::path::Path;
use std::sync::Arc;

use lingrep_corpus::{CorpusArtifact, CorpusError, MAX_SECTION_LABELS, ProvenanceView};

pub use lingrep_corpus::SectionCoverage;

use crate::corpus_runtime::{CacheReadiness, CorpusIdentity};
use crate::corpus_set::CorpusSlot;
use crate::state::RequestStore;
use crate::templates::format_count;

/// Stand-in label for a corpus that cannot be resolved to a slot, and so has no
/// `display_name` to show. Same generic word the export About sheet falls back
/// to; never a path.
pub(crate) const UNNAMED_CORPUS: &str = "corpus";

/// Compact count for section labels. All arithmetic is integer; rounding happens
/// before the unit is finalized, so a value that rounds up across a unit boundary
/// promotes cleanly (`999_950` → `"1M"`, never `"1000k"`). `u128` intermediates
/// avoid overflow at large `n`.
pub fn format_abbrev(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let n = u128::from(n);
    let (mut unit, mut suffix) = if n < 1_000_000 {
        (1_000u128, 'k')
    } else {
        (1_000_000u128, 'M')
    };
    let mut tenths = (n * 10 + unit / 2) / unit;
    if suffix == 'k' && tenths >= 10_000 {
        unit = 1_000_000;
        suffix = 'M';
        tenths = (n * 10 + unit / 2) / unit;
    }
    let int = tenths / 10;
    let frac = tenths % 10;
    if frac == 0 {
        format!("{int}{suffix}")
    } else {
        format!("{int}.{frac}{suffix}")
    }
}

/// First 12 hex chars of BLAKE3 of the corpus identity — an opaque operator
/// handle, never the path-bearing identity itself.
pub fn identity_digest(identity: &CorpusIdentity) -> String {
    let full = blake3::hash(identity.as_str().as_bytes())
        .to_hex()
        .to_string();
    // BLAKE3 hex is always 64 ASCII chars, so `[..12]` ends on a char boundary.
    #[expect(
        clippy::string_slice,
        reason = "BLAKE3 hex is 64 ASCII chars, so `[..12]` is ASCII-aligned"
    )]
    full[..12].to_string()
}

/// One section ready for the template.
#[derive(Debug, Clone)]
pub struct SectionRow {
    pub label: String,
    pub count_label: String,
}

/// Precomputed, template-ready corpus context. Field population expresses the six
/// rendering states (see spec): the template branches only on `Option`/emptiness,
/// never on a custom enum.
#[derive(Debug, Clone)]
pub struct CorpusContextView {
    pub name: String,
    /// Protocol key of the corpus this panel describes, stamped by
    /// [`slot_context_view`]. Empty when no slot could be resolved, which is the
    /// one case with no corpus to name.
    ///
    /// This is what the poll and the search form carry back, so the *selected*
    /// corpus — never the server's default — drives the next request.
    pub corpus_key: String,
    pub identity_digest: Option<String>,
    pub polling: bool,
    pub note: Option<String>,
    /// i18n interpolation args for `note` (JSON, empty when the note has no
    /// parameters), rendered as `data-i18n-args` by the template.
    pub note_args: Option<String>,
    /// i18n key for `note`; absent means the note is free-text.
    pub note_key: Option<&'static str>,
    pub sentence_count_label: Option<String>,
    pub document_count_label: Option<String>,
    /// Raw counts for i18n interpolation of the counts line; always Some
    /// exactly when the corresponding label is.
    pub sentence_count: Option<u64>,
    pub document_count: Option<u64>,
    /// Every section the corpus records, in coverage order. Not truncated: the
    /// row scrolls horizontally, so an overflow disclosure hid labels behind a
    /// click that the reader could already have reached by scrolling.
    pub sections: Vec<SectionRow>,
    /// The token attribute keys the corpus carries, which the pattern editor
    /// completes; empty until the corpus is ready.
    pub token_keys: Vec<String>,
}

impl CorpusContextView {
    /// State 1 / 6: ready with provenance and successful aggregates. Zero sections
    /// leaves both section vecs empty, which the template renders as state 6.
    pub fn ready(
        name: String,
        digest: String,
        sentence_count: u64,
        document_count: u64,
        sections: Vec<SectionCoverage>,
    ) -> Self {
        let rows: Vec<SectionRow> = sections
            .into_iter()
            .map(|s| SectionRow {
                label: s.label,
                count_label: format_abbrev(s.coverage),
            })
            .collect();
        Self {
            name,
            corpus_key: String::new(),
            identity_digest: Some(digest),
            polling: false,
            note: None,
            note_args: None,
            note_key: None,
            sentence_count_label: Some(format_count(sentence_count)),
            document_count_label: Some(format_count(document_count)),
            sentence_count: Some(sentence_count),
            document_count: Some(document_count),
            sections: rows,
            token_keys: Vec::new(),
        }
    }

    /// State 2: ready with provenance, but aggregate composition failed.
    pub fn details_unavailable(name: String, digest: String, sentence_count: u64) -> Self {
        Self {
            name,
            corpus_key: String::new(),
            identity_digest: Some(digest),
            polling: false,
            note: Some("Corpus details unavailable.".into()),
            note_args: None,
            note_key: Some("corpus.details_unavailable"),
            sentence_count_label: Some(format_count(sentence_count)),
            document_count_label: None,
            sentence_count: Some(sentence_count),
            document_count: None,
            sections: Vec::new(),
            token_keys: Vec::new(),
        }
    }

    /// State 3: ready with sentence count only.
    pub fn sentence_only(name: String, digest: String, sentence_count: u64) -> Self {
        Self {
            name,
            corpus_key: String::new(),
            identity_digest: Some(digest),
            polling: false,
            note: None,
            note_args: None,
            note_key: None,
            sentence_count_label: Some(format_count(sentence_count)),
            document_count_label: None,
            sentence_count: Some(sentence_count),
            document_count: None,
            sections: Vec::new(),
            token_keys: Vec::new(),
        }
    }

    /// Shows preparation progress, including a record count when the runtime supplies one.
    pub fn preparing(name: String, lines_processed: Option<u64>) -> Self {
        let (note, note_args) = match lines_processed {
            Some(processed) => (
                format!(
                    "Corpus is preparing… {} records read",
                    format_count(processed)
                ),
                Some(format!("{{\"n\": {processed}}}")),
            ),
            None => ("Corpus is preparing…".into(), None),
        };
        Self {
            name,
            corpus_key: String::new(),
            identity_digest: None,
            polling: true,
            note: Some(note),
            note_args,
            note_key: Some("corpus.preparing"),
            sentence_count_label: None,
            document_count_label: None,
            sentence_count: None,
            document_count: None,
            sections: Vec::new(),
            token_keys: Vec::new(),
        }
    }

    /// State 5: corpus failed. Terminal — no polling.
    pub fn failed(name: String) -> Self {
        Self {
            name,
            corpus_key: String::new(),
            identity_digest: None,
            polling: false,
            note: Some("Corpus unavailable.".into()),
            note_args: None,
            note_key: Some("corpus.unavailable"),
            sentence_count_label: None,
            document_count_label: None,
            sentence_count: None,
            document_count: None,
            sections: Vec::new(),
            token_keys: Vec::new(),
        }
    }
}

pub(crate) type Memo<K, V> = Arc<tokio::sync::Mutex<Option<(K, V)>>>;

/// Caches successful computations by keys compared with `same`; a different key
/// replaces the entry, and concurrent callers share one computation.
/// Once started, a computation finishes even if its caller is dropped;
/// dropping the caller before it starts skips the computation.
pub(crate) async fn detached_memo<K, V, F, Fut>(
    cache: &Memo<K, V>,
    key: K,
    same: fn(&K, &K) -> bool,
    compute: F,
) -> Result<V, CorpusError>
where
    K: Send + 'static,
    V: Clone + Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<V, CorpusError>> + Send,
{
    if let Some((cached, value)) = cache.lock().await.as_ref()
        && same(cached, &key)
    {
        return Ok(value.clone());
    }
    let cache = Arc::clone(cache);
    let waiting = Arc::new(());
    let caller = Arc::downgrade(&waiting);
    let computation = tokio::spawn(async move {
        let mut guard = cache.lock().await;
        if let Some((cached, value)) = guard.as_ref()
            && same(cached, &key)
        {
            return Ok(value.clone());
        }
        if caller.strong_count() == 0 {
            return Err(CorpusError::Io(std::io::Error::other(
                "the caller left before the computation started",
            )));
        }
        let value = compute().await?;
        *guard = Some((key, value.clone()));
        Ok(value)
    });
    let joined = computation.await;
    drop(waiting);
    match joined {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => Err(CorpusError::Io(std::io::Error::other(
            "the server stopped before the computation finished",
        ))),
    }
}

/// Caches successful results by corpus identity, including after caller cancellation.
pub async fn identity_memo<F, Fut>(
    cache: &Memo<CorpusIdentity, Arc<CorpusContextView>>,
    identity: CorpusIdentity,
    compute: F,
) -> Result<Arc<CorpusContextView>, CorpusError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<Arc<CorpusContextView>, CorpusError>> + Send,
{
    detached_memo(cache, identity, |cached, wanted| cached == wanted, compute).await
}

/// Human-facing corpus name: the file stem of the configured corpus path. Never
/// the full path or the internal identity (see trust boundary).
pub(crate) fn corpus_display_name(corpus_path: &str) -> String {
    Path::new(corpus_path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "corpus".to_string())
}

/// Build the ready (state 1/6) view from shared artifact provenance.
pub async fn build_ready_view(
    name: String,
    digest: String,
    artifact: &CorpusArtifact,
    provenance: &ProvenanceView,
) -> Result<CorpusContextView, CorpusError> {
    let sections = provenance.section_coverage(MAX_SECTION_LABELS).await?;
    Ok(CorpusContextView::ready(
        name,
        digest,
        provenance.sentence_count(),
        artifact.document_count(),
        sections,
    ))
}

/// Resolve the corpus-context view for the current request, applying the
/// single-flight memo for the ready-with-provenance path and mapping every
/// runtime/query outcome to one of the six rendering states.
///
/// This is the **request's selected** corpus, not a workbench's: the panel
/// describes what the page is currently pointed at. `selected` is the polled
/// `k`, which the partial itself emitted from [`CorpusContextView::corpus_key`],
/// so a poll keeps observing the corpus the page selected rather than drifting
/// back to the default. `None` means the default, so a request predating the
/// parameter still works.
/// The landing page already selects for itself — see [`slot_context_view`].
pub async fn corpus_context_view(
    store: &RequestStore,
    selected: Option<&str>,
) -> CorpusContextView {
    // The lookup must stay FALLIBLE. `resolve(None)` cannot miss for a validated
    // configuration, but `k` arrives from the client's URL, and an undeclared or
    // malformed key resolves to `None`. Panicking here would then be a panicking
    // request handler — a trivial denial of service — so an unresolvable
    // selection renders the terminal "corpus unavailable" state instead.
    let Some(slot) = store.corpora.resolve(selected) else {
        return CorpusContextView::failed(UNNAMED_CORPUS.to_owned());
    };
    // The step without which a never-selected slot stays Idle with readiness
    // Checking forever: start_or_observe only transitions from Idle, and nothing
    // else in this path calls it. Idempotent once building or ready.
    //
    // It belongs here rather than in `slot_context_view`, which stays a pure
    // description of an already-resolved slot: the landing page starts its own
    // slot before describing it. Without this line the poll renders the
    // `preparing` state with `polling: true` — promising progress that nothing
    // will ever make — and the browser polls "Corpus is preparing…" forever.
    slot.runtime.start_or_observe().await;
    slot_context_view(slot).await
}

/// The context panel for one **already-resolved** corpus.
///
/// Split out because the landing page resolves its own slot from the target's
/// `k` and must describe *that* corpus; resolving a second time here could
/// describe a different one than the page is pointed at.
pub async fn slot_context_view(slot: &CorpusSlot) -> CorpusContextView {
    // The key is a property of the slot, not of any readiness state, so it is
    // stamped once onto whichever of the six views the runtime produced.
    let token_keys = match slot.runtime.ready().await {
        Some(ready) => ready.artifact.token_schema().map_or_else(
            |_| Vec::new(),
            |schema| {
                schema
                    .keys()
                    .iter()
                    .map(|key| key.as_str().to_owned())
                    .collect()
            },
        ),
        None => Vec::new(),
    };
    CorpusContextView {
        corpus_key: slot.name.as_str().to_owned(),
        token_keys,
        ..slot_readiness_view(slot).await
    }
}

/// Which of the six rendering states the slot's runtime is currently in.
async fn slot_readiness_view(slot: &CorpusSlot) -> CorpusContextView {
    let name = slot.display_name.clone();

    let runtime = &slot.runtime;
    let Some(ready) = runtime.ready().await else {
        // `CacheReadiness::Failed` is a struct variant (`Failed { category }`), so
        // match with `{ .. }`; `Ready`/`Checking`/`Idle` all fall through to the
        // preparing state (a `Ready` readiness with `ready()` == None is a benign
        // transient the poll will resolve).
        return match *runtime.readiness().borrow() {
            CacheReadiness::Failed { .. } => CorpusContextView::failed(name), // state 5
            CacheReadiness::Building { lines_processed } => {
                CorpusContextView::preparing(name, Some(lines_processed)) // state 4
            }
            _ => CorpusContextView::preparing(name, None), // state 4
        };
    };

    let digest = identity_digest(&ready.identity);

    // The runtime's cached view, not a fresh `artifact.provenance_view()`.
    // The uncached call revalidates every sentence row, and this runs on the
    // index page and on every corpus-context poll.
    let provenance = match ready.provenance().await {
        Ok(provenance) => provenance,
        Err(error) => {
            tracing::warn!(%error, "corpus provenance unavailable; showing sentence count only");
            return CorpusContextView::details_unavailable(
                name,
                digest,
                ready.sentence_count().unwrap_or_default(),
            );
        }
    };
    let sentence_count = provenance.sentence_count();

    let built = identity_memo(slot.context_slot(), ready.identity.clone(), {
        let artifact = Arc::clone(&ready.artifact);
        let name = name.clone();
        let digest = digest.clone();
        move || async move {
            build_ready_view(name, digest, &artifact, &provenance)
                .await
                .map(Arc::new)
        }
    })
    .await;

    match built {
        Ok(view) => (*view).clone(), // state 1/6
        Err(error) => {
            tracing::warn!(%error, "corpus context aggregates failed; showing sentence count only");
            CorpusContextView::details_unavailable(name, digest, sentence_count) // state 2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus_runtime::CorpusIdentity;

    #[test]
    fn format_abbrev_pins_every_boundary() {
        assert_eq!(format_abbrev(96), "96");
        assert_eq!(format_abbrev(1_500), "1.5k");
        assert_eq!(format_abbrev(2_000), "2k");
        assert_eq!(format_abbrev(96_000), "96k");
        assert_eq!(format_abbrev(412_000), "412k");
        assert_eq!(format_abbrev(999_949), "999.9k");
        assert_eq!(format_abbrev(999_950), "1M"); // unit promotion
        assert_eq!(format_abbrev(1_240_000), "1.2M");
        assert_eq!(format_abbrev(12_000_000), "12M");
        let _ = format_abbrev(u64::MAX); // must not panic/overflow
    }

    #[test]
    fn identity_digest_is_12_hex_and_hides_the_identity() {
        let id = CorpusIdentity::new("/secret/path.jsonl:10:99|/dict:1:1");
        let d = identity_digest(&id);
        assert_eq!(d.len(), 12);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!d.contains("secret"));
    }

    #[test]
    fn ready_keeps_every_section_and_formats() {
        let sections = (0..10)
            .map(|i| SectionCoverage {
                label: format!("s{i}"),
                coverage: (10 - i) as u64 * 1000,
            })
            .collect();
        let v = CorpusContextView::ready(
            "corp".into(),
            "abc123abc123".into(),
            1_240_000,
            8_300,
            sections,
        );
        assert_eq!(v.sentence_count_label.as_deref(), Some("1,240,000"));
        assert_eq!(v.document_count_label.as_deref(), Some("8,300"));
        assert_eq!(v.sections.len(), 10);
        assert_eq!(v.sections[0].count_label, "10k");
        assert!(v.note.is_none());
        assert!(!v.polling);
        assert!(v.identity_digest.is_some());
    }

    #[test]
    fn ready_with_no_sections_lists_none() {
        let v = CorpusContextView::ready("corp".into(), "abc123abc123".into(), 5, 1, vec![]);
        assert!(v.sections.is_empty());
        assert_eq!(v.document_count_label.as_deref(), Some("1"));
    }

    #[test]
    fn degraded_states_populate_the_right_fields() {
        let d = CorpusContextView::details_unavailable("c".into(), "abc123abc123".into(), 42);
        assert_eq!(d.sentence_count_label.as_deref(), Some("42"));
        assert!(d.document_count_label.is_none());
        assert!(d.sections.is_empty());
        assert_eq!(d.note.as_deref(), Some("Corpus details unavailable."));

        let s = CorpusContextView::sentence_only("c".into(), "abc123abc123".into(), 42);
        assert_eq!(s.sentence_count_label.as_deref(), Some("42"));
        assert!(s.note.is_none());

        let p = CorpusContextView::preparing("c".into(), None);
        assert!(p.sentence_count_label.is_none());
        assert!(p.identity_digest.is_none());
        assert!(p.polling);
        assert_eq!(p.note.as_deref(), Some("Corpus is preparing…"));

        let p = CorpusContextView::preparing("c".into(), Some(12_345));
        assert!(p.polling);
        assert_eq!(
            p.note.as_deref(),
            Some("Corpus is preparing… 12,345 records read")
        );

        let f = CorpusContextView::failed("c".into());
        assert!(!f.polling);
        assert_eq!(f.note.as_deref(), Some("Corpus unavailable."));
    }

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    fn view_fixture() -> CorpusContextView {
        CorpusContextView::sentence_only("c".into(), "abc123abc123".into(), 1)
    }

    #[tokio::test]
    async fn memo_runs_compute_once_per_identity_under_concurrency() {
        let cache = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let id = CorpusIdentity::new("x");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let calls = Arc::clone(&calls);
                let id = id.clone();
                tokio::spawn(async move {
                    identity_memo(&cache, id, move || {
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::task::yield_now().await;
                            Ok(Arc::new(view_fixture()))
                        }
                    })
                    .await
                    .map(|_| ())
                })
            })
            .collect();
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "single-flight: compute runs once"
        );
    }

    #[tokio::test]
    async fn a_computation_whose_caller_is_dropped_finishes_and_is_cached() {
        let cache = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let caller = tokio::spawn({
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            async move {
                identity_memo(&cache, CorpusIdentity::new("x"), move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let _ = started.send(());
                    let _ = released.await;
                    Ok(Arc::new(view_fixture()))
                })
                .await
                .map(|_| ())
            }
        });
        running.await.expect("the computation started");
        caller.abort();
        assert!(caller.await.expect_err("aborted caller").is_cancelled());
        let _ = release.send(());

        let again = Arc::clone(&calls);
        identity_memo(&cache, CorpusIdentity::new("x"), move || async move {
            again.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(view_fixture()))
        })
        .await
        .expect("the cached view");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn computations_queued_by_callers_that_left_do_not_run_after_a_failure() {
        let cache = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, released) = tokio::sync::watch::channel(false);
        let held = cache.lock().await;
        let mut callers: Vec<_> = (0..5)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let calls = Arc::clone(&calls);
                let mut released = released.clone();
                tokio::spawn(async move {
                    identity_memo(&cache, CorpusIdentity::new("x"), move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let _ = released.wait_for(|released| *released).await;
                        Err(CorpusError::SqliteSidecar)
                    })
                    .await
                    .map(|_| ())
                })
            })
            .collect();
        tokio::task::yield_now().await;
        drop(held);
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let first = callers.remove(0);
        for caller in callers {
            caller.abort();
            assert!(caller.await.expect_err("aborted caller").is_cancelled());
        }

        release.send(true).expect("a waiting computation");
        assert!(first.await.expect("the first caller").is_err());
        drop(cache.lock().await);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn memo_rebuilds_on_identity_change_but_reuses_same_identity() {
        let cache = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let compute = || {
            let calls = Arc::clone(&calls);
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(view_fixture()))
            }
        };
        identity_memo(&cache, CorpusIdentity::new("a"), compute())
            .await
            .unwrap();
        identity_memo(&cache, CorpusIdentity::new("a"), compute())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1); // cache hit
        identity_memo(&cache, CorpusIdentity::new("b"), compute())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2); // rebuilt on change
    }

    #[tokio::test]
    async fn memo_does_not_cache_failures() {
        let cache = Arc::new(Mutex::new(None));
        let id = CorpusIdentity::new("x");
        let err = identity_memo(&cache, id.clone(), || async {
            Err(CorpusError::InvalidFormat {
                component: "provenance.sqlite",
                message: "injected".into(),
            })
        })
        .await;
        assert!(err.is_err());
        assert!(cache.lock().await.is_none());
        let ok = identity_memo(&cache, id, || async { Ok(Arc::new(view_fixture())) }).await;
        assert!(ok.is_ok());
        assert!(cache.lock().await.is_some());
    }
}
