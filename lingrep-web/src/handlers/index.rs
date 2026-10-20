use askama::Template;
use axum::{
    Router,
    extract::{RawQuery, State},
    response::Html,
    routing::get,
};

use crate::{
    corpus_context::{CorpusContextView, UNNAMED_CORPUS, slot_context_view},
    corpus_set::CorpusName,
    permalink::{self, MAX_REQUEST_TARGET_LEN, WIRE_VERSION},
    revision::WorkbenchId,
    state::RequestStore,
    templates::{CorpusOption, IndexPage, LinkBanner, LinkNotice},
};

const NOTICE_UNREADABLE: LinkNotice = LinkNotice {
    text: "This shared link could not be read, so the default pattern is shown.",
    key: "notice.unreadable",
};
const NOTICE_CORPUS_LOADING: LinkNotice = LinkNotice {
    text: "The corpus is still loading, so the pattern was seeded but not run automatically.",
    key: "notice.corpus_loading",
};
/// A link whose `k` names a corpus this server does not declare. The pattern is
/// intact — only the corpus is missing — so this says what is wrong rather than
/// reporting the link as unreadable and discarding a query the server decoded
/// perfectly well. It names no remedy, because a single-corpus server renders no
/// selector to point at.
const NOTICE_UNKNOWN_CORPUS: LinkNotice = LinkNotice {
    text: "This link is for a corpus this server does not have, so the pattern was seeded but not run.",
    key: "notice.unknown_corpus",
};
/// A failed corpus is terminal, so "still loading" would contradict the context
/// panel's own "Corpus unavailable." The panel states the failure; this states
/// its consequence for the link.
const NOTICE_CORPUS_UNAVAILABLE: LinkNotice = LinkNotice {
    text: "The corpus is unavailable, so the pattern was seeded but not run.",
    key: "notice.corpus_unavailable",
};

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/", get(index_page))
}

/// A parsed landing target.
///
/// `corpus_key` **selects** and `corpus` **asserts**: `k` says which corpus the
/// link is for, while `c` says which content produced the results the sharer
/// saw. They are independent because a lazily built corpus has no identity yet —
/// an absent `c` means "no assertion", not "no corpus".
pub struct WireParams<'a> {
    pub version: &'a str,
    pub query: &'a str,
    /// The mandatory corpus selector.
    pub corpus_key: &'a str,
    /// Optional content identity assertion.
    pub corpus: Option<&'a str>,
}

fn parse_wire(raw: &str) -> Option<WireParams<'_>> {
    if raw.len().checked_add(2)? > MAX_REQUEST_TARGET_LEN {
        return None;
    }
    let mut version = None;
    let mut query = None;
    let mut corpus_key = None;
    let mut corpus = None;
    for pair in raw.split('&') {
        let (key, value) = pair.split_once('=')?;
        let slot = match key {
            "v" => &mut version,
            "q" => &mut query,
            "k" => &mut corpus_key,
            "c" => &mut corpus,
            _ => return None,
        };
        if value.is_empty() || slot.replace(value).is_some() {
            return None;
        }
    }
    let params = WireParams {
        version: version?,
        query: query?,
        corpus_key: corpus_key?,
        corpus,
    };
    if params.version != WIRE_VERSION {
        return None;
    }
    if !params
        .query
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return None;
    }
    // The wire validator, not the operator one: `encode` always emits `k`, so
    // `k=default` is an ordinary target that this parser must accept.
    if CorpusName::parse(params.corpus_key).is_err() {
        return None;
    }
    if let Some(digest) = params.corpus
        && (digest.len() != 12
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return None;
    }
    Some(params)
}

/// Test-only re-export of the wire parser, so the endpoint test can prove that
/// what `encode` emits is what the landing page accepts. The two live in
/// different modules, and a mismatch between them is exactly the class of defect
/// this asserts against.
#[cfg(any(test, feature = "testing"))]
pub fn parse_wire_for_test(raw: &str) -> Option<WireParams<'_>> {
    parse_wire(raw)
}

/// What the request target asked for, once parsed.
enum Target<'a> {
    /// A bare `/`: no shared link at all.
    Bare,
    /// A target that did not parse. Distinguished from [`Target::Bare`] because
    /// only this one earns the "could not be read" notice.
    Unreadable,
    Link(WireParams<'a>),
}

fn parse_target(raw: Option<&str>) -> Target<'_> {
    match raw.filter(|raw| !raw.is_empty()) {
        None => Target::Bare,
        Some(raw) => parse_wire(raw).map_or(Target::Unreadable, Target::Link),
    }
}

struct Landing {
    seed_pattern: String,
    auto_run: bool,
    /// Queue the seeded query instead of abandoning it: the corpus is preparing,
    /// so the page runs it once the context poll reports readiness. Set only for
    /// a link that wanted to run — a bare landing has nothing to queue — and
    /// never together with `auto_run`.
    run_when_ready: bool,
    link_banner: Option<LinkBanner>,
    link_notice: Option<LinkNotice>,
}

/// What the selected corpus can currently say about itself.
///
/// Read from the very panel the page is about to render, rather than from a
/// second look at the runtime: two independent reads could disagree, and the
/// page would then contradict itself — a panel saying "Corpus unavailable."
/// above a notice saying "still loading" is exactly the defect this exists to
/// prevent.
enum Readiness<'a> {
    /// Has an identity, so a link's `c` can be compared against it.
    Ready(&'a str),
    /// Cold, checking or building: no identity yet, but not terminal.
    Preparing,
    /// Terminally failed.
    Failed,
}

impl<'a> Readiness<'a> {
    fn of(context: &'a CorpusContextView) -> Self {
        match (context.identity_digest.as_deref(), context.polling) {
            (Some(digest), _) => Self::Ready(digest),
            // Only the preparing states self-refresh; `failed` is terminal and
            // is the one not-ready view that does not poll.
            (None, true) => Self::Preparing,
            (None, false) => Self::Failed,
        }
    }
}

/// Decide what the landing page seeds and whether it runs.
///
/// A link with no `c` asserts nothing, so a ready corpus simply runs it.
fn interpret(target: &Target<'_>, default_pattern: &str, readiness: &Readiness<'_>) -> Landing {
    let fallback = |link_notice| Landing {
        seed_pattern: default_pattern.to_string(),
        auto_run: false,
        run_when_ready: false,
        link_banner: None,
        link_notice,
    };

    let params = match target {
        Target::Bare => return fallback(None),
        Target::Unreadable => return fallback(Some(NOTICE_UNREADABLE)),
        Target::Link(params) => params,
    };
    let Ok(seed_pattern) = permalink::decode(params.query) else {
        return fallback(Some(NOTICE_UNREADABLE));
    };
    let run = Landing {
        seed_pattern,
        auto_run: true,
        run_when_ready: false,
        link_banner: None,
        link_notice: None,
    };
    match (readiness, params.corpus) {
        // Ready, and the link asserts nothing about which content it saw.
        (Readiness::Ready(_), None) => run,
        // Ready, and the link's assertion names this content.
        (Readiness::Ready(live), Some(link)) if link == *live => run,
        (Readiness::Ready(live), Some(link)) => Landing {
            auto_run: false,
            link_banner: Some(LinkBanner {
                link_digest: link.to_string(),
                live_digest: (*live).to_string(),
            }),
            ..run
        },
        // No identity to compare, so nothing runs *yet*. The two not-ready
        // states are distinguished because the panel above renders them
        // differently, and a notice must not contradict it — and because only
        // preparing is transient: a link over a building corpus is queued, while
        // a failed corpus is terminal and has nothing to wait for.
        (Readiness::Preparing, _) => Landing {
            auto_run: false,
            run_when_ready: true,
            link_notice: Some(NOTICE_CORPUS_LOADING),
            ..run
        },
        (Readiness::Failed, _) => Landing {
            auto_run: false,
            link_notice: Some(NOTICE_CORPUS_UNAVAILABLE),
            ..run
        },
    }
}

pub async fn index_page(
    State(store): State<RequestStore>,
    RawQuery(raw): RawQuery,
) -> Html<String> {
    let target = parse_target(raw.as_deref());
    let selected_key = match &target {
        Target::Link(params) => Some(params.corpus_key),
        Target::Bare | Target::Unreadable => None,
    };
    // The lookup must stay FALLIBLE: `k` arrives from the client's URL, so an
    // undeclared key is an ordinary miss and panicking here would be a trivial
    // denial of service.
    let Some(slot) = store.corpora.resolve(selected_key) else {
        return unknown_corpus_page(&store, &target);
    };
    // The step without which a never-selected slot stays Idle with readiness
    // Checking forever: start_or_observe only transitions from Idle, and nothing
    // else in a request path calls it. Idempotent once building or ready.
    slot.runtime.start_or_observe().await;
    let corpus_context = slot_context_view(slot).await;
    let landing = interpret(
        &target,
        &store.default_pattern,
        &Readiness::of(&corpus_context),
    );
    render_index(&store, landing, corpus_context)
}

/// The landing page for a target naming a corpus this server does not declare.
///
/// The query is kept: only the corpus is missing, so the decoded pattern is
/// seeded and the selector — rendered whenever this server declares more than
/// one corpus — is what retargets it. A target whose query does not decode is a
/// different defect and keeps the unreadable-link notice.
///
/// The panel carries the generic placeholder rather than any resolved slot, so a
/// guessed key reveals nothing about the server's configuration — least of all a
/// filesystem path.
fn unknown_corpus_page(store: &RequestStore, target: &Target<'_>) -> Html<String> {
    let decoded = match target {
        Target::Link(params) => permalink::decode(params.query).ok(),
        // Unreachable for a validated configuration — a target naming no corpus
        // resolves to the default slot — but the default pattern is the honest
        // answer if it ever is reached.
        Target::Bare | Target::Unreadable => None,
    };
    let (seed_pattern, link_notice) = decoded.map_or_else(
        || (store.default_pattern.to_string(), NOTICE_UNREADABLE),
        |pattern| (pattern, NOTICE_UNKNOWN_CORPUS),
    );
    let landing = Landing {
        seed_pattern,
        auto_run: false,
        run_when_ready: false,
        link_banner: None,
        link_notice: Some(link_notice),
    };
    render_index(
        store,
        landing,
        CorpusContextView::failed(UNNAMED_CORPUS.to_owned()),
    )
}

fn render_index(
    store: &RequestStore,
    landing: Landing,
    corpus_context: CorpusContextView,
) -> Html<String> {
    IndexPage {
        workbench_id: WorkbenchId::random().as_str().to_owned(),
        current_page: "corpus",
        seed_pattern: landing.seed_pattern,
        auto_run: landing.auto_run,
        run_when_ready: landing.run_when_ready,
        link_banner: landing.link_banner,
        link_notice: landing.link_notice,
        // Marked against the panel's own key rather than a second resolution,
        // so the selector always shows the corpus the panel describes.
        corpora: CorpusOption::list(&store.corpora, &corpus_context.corpus_key),
        corpus_context,
        catalog_keys: store.catalogs.keys().map(str::to_owned).collect(),
        retained_options: crate::state::retention_offers(store.config.detail_limits.max_kwic),
        retained_default: store.config.detail_limits.max_kwic,
    }
    .render().map_or_else(|_| {
        Html(
            "<div class=\"alert alert-error\" role=\"status\">Unable to render page. Please retry.</div>"
                .to_string(),
        )
    }, Html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_target_boundary_is_enforced() {
        let framing = "v=1&q=&k=default&c=abcdef012345";
        let query_len = MAX_REQUEST_TARGET_LEN - 2 - framing.len();
        let raw_at_limit = format!("v=1&q={}&k=default&c=abcdef012345", "A".repeat(query_len));
        assert_eq!(raw_at_limit.len() + 2, MAX_REQUEST_TARGET_LEN);
        assert!(parse_wire(&raw_at_limit).is_some());
        let raw_over_limit = format!("{raw_at_limit}A");
        assert!(parse_wire(&raw_over_limit).is_none());
    }

    #[test]
    fn strict_parser_accepts_only_the_emitted_shape() {
        let target =
            permalink::encode("p := [upos=VERB].", "default", Some("abcdef012345")).unwrap();
        let raw = target.strip_prefix("/?").unwrap();
        let params = parse_wire(raw).unwrap();
        assert_eq!(params.version, WIRE_VERSION);
        assert_eq!(params.corpus, Some("abcdef012345"));
        assert!(parse_wire(&format!("{raw}&q={}", params.query)).is_none());
        assert!(parse_wire(&format!("{raw}&extra=x")).is_none());
    }

    #[test]
    fn retired_targets_are_refused() {
        assert!(parse_wire("v=1&q=AAAA&c=abcdef012345").is_none());
        assert!(parse_wire("v=2&q=AAAA&k=default&c=abcdef012345").is_none());
    }

    #[test]
    fn v1_accepts_a_selector_with_no_digest() {
        let target = permalink::encode("p := [].", "sci", None).unwrap();
        let params = parse_wire(target.strip_prefix("/?").unwrap()).unwrap();
        assert_eq!(params.corpus_key, "sci");
        assert_eq!(params.corpus, None);
    }

    #[test]
    fn a_selector_must_match_the_name_charset() {
        let bad = "v=1&q=AAAA&k=has%20space";
        assert!(parse_wire(bad).is_none());
    }

    /// A server declaring two corpora, the second keyed `sci`. Both paths are
    /// fictional: nothing here touches the filesystem, and the point of the
    /// fixture is that the path is *available to leak* and must not be.
    async fn two_corpus_store(default_path: &str) -> crate::state::RequestStore {
        use std::path::PathBuf;

        use crate::corpus_context::corpus_display_name;
        use crate::state::{Config, DetailLimits, RequestStore};

        let science_path = "/private/corpora/science-b.jsonl";
        RequestStore::new(Config {
            dict_path: String::new(),
            corpora: vec![
                (
                    CorpusName::default_key(),
                    PathBuf::from(default_path),
                    corpus_display_name(default_path),
                ),
                (
                    CorpusName::parse_declared("sci").unwrap(),
                    PathBuf::from(science_path),
                    corpus_display_name(science_path),
                ),
            ],
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 1_000 },
        })
    }

    #[tokio::test]
    async fn the_selector_lists_every_corpus_and_never_leaks_a_path() {
        let store = two_corpus_store("/private/corpora/secret-a.jsonl").await;
        let html = super::index_page(axum::extract::State(store), axum::extract::RawQuery(None))
            .await
            .0;
        assert!(html.contains("data-corpus-select"));
        assert!(html.contains("value=\"sci\""));
        // A bare landing is pointed at the default corpus, so that option — and
        // only that one — is preselected.
        assert!(html.contains("value=\"default\" selected"));
        assert!(!html.contains("value=\"sci\" selected"));
        // The display name is what a researcher reads; the path it was derived
        // from must not reach the browser in any form.
        assert!(html.contains(">secret-a</option>"));
        assert!(html.contains(">science-b</option>"));
        // A resolved slot needs no placeholder: two `selected` options would
        // make the failure-restore in app.js pick the wrong one.
        assert!(!html.contains(r#"<option value="" disabled selected"#));
        assert!(!html.contains("/private/corpora"));
        assert!(!html.contains("secret-a.jsonl"));
    }

    /// The switch's live region must outlive the corpus-context poll, which
    /// replaces the whole `.corpus-context` section every few seconds while a
    /// corpus prepares. A region inside that section would be erased seconds
    /// after reporting; the pin is that the node is not in the swapped subtree.
    #[tokio::test]
    async fn the_switch_status_region_sits_outside_the_polled_section() {
        let store = two_corpus_store("/private/corpora/secret-a.jsonl").await;
        let html = super::index_page(axum::extract::State(store), axum::extract::RawQuery(None))
            .await
            .0;
        let section_end = html
            .find("</section>")
            .expect("the corpus-context section is rendered");
        let status = html
            .find("data-corpus-select-status")
            .expect("the switch reports somewhere");
        assert!(
            status > section_end,
            "the live region must not sit inside the swapped section"
        );
    }

    /// An undeclared `k` keeps the pattern the server already decoded. The
    /// selector is what makes keeping it
    /// actionable — the query is seeded, unrun, and retargetable.
    #[tokio::test]
    async fn an_undeclared_corpus_keeps_the_decoded_pattern_and_offers_the_selector() {
        let store = two_corpus_store("/private/corpora/secret-a.jsonl").await;
        let target = permalink::encode("kept := [upos=VERB].", "gone", None).unwrap();
        let raw = target.strip_prefix("/?").unwrap().to_owned();
        let html = super::index_page(
            axum::extract::State(store),
            axum::extract::RawQuery(Some(raw)),
        )
        .await
        .0;

        assert!(html.contains("kept := [upos=VERB]."));
        assert!(html.contains("data-i18n=\"notice.unknown_corpus\""));
        assert!(!html.contains("data-i18n=\"notice.unreadable\""));
        // Nothing runs: there is no corpus to run against until one is chosen.
        assert!(!html.contains("data-autorun"));
        assert!(!html.contains("data-run-when-ready"));
        // The selector is the retargeting affordance. No declared corpus claims
        // to be the one this page is pointed at, because none is — the
        // placeholder holds that position so that choosing the first corpus
        // still changes the control's value, and so still navigates.
        assert!(html.contains("data-corpus-select"));
        assert!(html.contains(r#"<option value="" disabled selected"#));
        assert!(!html.contains("value=\"default\" selected"));
        assert!(!html.contains("value=\"sci\" selected"));
    }

    /// A target that does not decode is a different defect from a target naming
    /// a corpus this server lacks, and keeps the unreadable-link notice.
    #[tokio::test]
    async fn an_undeclared_corpus_with_an_undecodable_query_stays_unreadable() {
        let store = two_corpus_store("/private/corpora/secret-a.jsonl").await;
        let html = super::index_page(
            axum::extract::State(store),
            axum::extract::RawQuery(Some("v=1&q=notbase64url&k=gone".to_owned())),
        )
        .await
        .0;
        assert!(html.contains("data-i18n=\"notice.unreadable\""));
        assert!(!html.contains("data-i18n=\"notice.unknown_corpus\""));
    }

    #[tokio::test]
    async fn the_index_page_lists_every_discovered_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.lx"), "alpha := [].\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/beta.lx"), "beta := [].\n").unwrap();
        let index = std::sync::Arc::new(crate::catalog_index::CatalogIndex::discover(dir.path()));

        let store = crate::state::RequestStore::default().with_catalogs(index);
        let html = super::index_page(axum::extract::State(store), axum::extract::RawQuery(None))
            .await
            .0;

        assert!(html.contains("data-catalog-key=\"alpha.lx\""));
        assert!(html.contains("data-catalog-key=\"sub/beta.lx\""));
        assert!(html.contains("data-query-tab=\"catalogs\""));
    }
}
