use askama::Template;
use lingrep_core::token_match::TokenAttributeIndex;

use crate::inspection::{CaptureInspection, CompositionItem, TokenInspection};
use crate::revision_status::RevisionStatus;
use crate::sample::SampleAnalysis;

/// An HTML fragment that has already been rendered by Askama, and therefore
/// already escaped at its own boundaries. The only constructor takes such a
/// rendered string, so a value of this type *is* the "safe to emit without
/// re-escaping" invariant — enforced by the type system rather than a `|safe`
/// comment. `Display` writes the inner bytes verbatim; templates still spell
/// `|safe` to opt out of double-escaping.
#[derive(Debug, Clone)]
pub struct TrustedFragmentHtml(String);

impl TrustedFragmentHtml {
    /// Wrap an already-rendered, already-escaped fragment.
    pub fn from_rendered(html: String) -> Self {
        Self(html)
    }
}

impl std::fmt::Display for TrustedFragmentHtml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexPage {
    pub workbench_id: String,
    pub current_page: &'static str,
    pub seed_pattern: String,
    pub auto_run: bool,
    /// The seeded query is queued rather than abandoned: the corpus is still
    /// preparing, so the page runs it once the corpus-context poll reports a
    /// ready corpus. Never set together with `auto_run`, which runs immediately.
    pub run_when_ready: bool,
    pub link_banner: Option<LinkBanner>,
    pub link_notice: Option<LinkNotice>,
    pub corpus_context: crate::corpus_context::CorpusContextView,
    /// Declaration-ordered corpora offered by the selector. See [`CorpusOption`].
    pub corpora: Vec<CorpusOption>,
    /// Discovery-ordered catalog keys offered in the library's Catalogs tab.
    pub catalog_keys: Vec<String>,
    /// Ascending retention values the run control offers. See
    /// [`retention_offers`](crate::state::retention_offers).
    pub retained_options: Vec<usize>,
    /// The offer selected on load: the server's configured ceiling, so a page
    /// nobody touches runs exactly what it ran before the control existed.
    pub retained_default: usize,
}

impl IndexPage {
    /// Whether this offer is the one selected on load. A method rather than a
    /// template comparison: the loop yields a reference and Askama has no
    /// dereference operator.
    pub fn is_default_retention(&self, option: &usize) -> bool {
        *option == self.retained_default
    }
}

/// One corpus the selector offers.
///
/// Carries the two things a browser may see — the protocol key the wire
/// already carries and the researcher-facing display name — and never the
/// corpus path, which is the trust boundary `identity_digest` exists to keep.
pub struct CorpusOption {
    /// The `[A-Za-z0-9_-]` protocol key, safe unescaped in an attribute and a URL.
    pub key: String,
    /// Any UTF-8; escaped by the template like every other text value.
    pub display_name: String,
    /// Whether this is the corpus the page is currently pointed at. No option
    /// is active when no slot resolved, which is the unknown-corpus landing.
    pub active: bool,
}

impl CorpusOption {
    /// The declared corpora as selector options, in declaration order.
    ///
    /// `active_key` is the resolved corpus key the page is pointed at — the
    /// same value the panel and the search form carry — so the selector cannot
    /// disagree with them about which corpus this page is about.
    #[must_use]
    pub fn list(corpora: &crate::corpus_set::CorpusSet, active_key: &str) -> Vec<Self> {
        corpora
            .iter()
            .map(|(name, slot)| Self {
                key: name.as_str().to_owned(),
                display_name: slot.display_name.clone(),
                active: name.as_str() == active_key,
            })
            .collect()
    }
}

pub struct LinkBanner {
    pub link_digest: String,
    pub live_digest: String,
}

/// A landing notice explaining why a shared link was not run.
///
/// The served English text and its i18n key travel together so they cannot
/// drift apart: the inline text is what an English-mode pin reads, and `key` is
/// what `localize` swaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkNotice {
    pub text: &'static str,
    pub key: &'static str,
}

#[derive(Template)]
#[template(path = "partials/corpus_context.html")]
pub struct CorpusContextPartial {
    pub corpus_context: crate::corpus_context::CorpusContextView,
    /// The same options the landing page renders: the partial replaces the
    /// whole section on every poll, so omitting them would swap the selector
    /// away the first time the corpus reported on itself.
    pub corpora: Vec<CorpusOption>,
}

#[derive(Template)]
#[template(path = "fragments/stream_container.html")]
pub struct StreamContainer<'a> {
    pub request_id: &'a str,
    pub status: &'a RevisionStatus,
}

#[derive(Template)]
#[template(path = "fragments/revision_status.html")]
pub struct RevisionStatusFragment<'a> {
    pub status: &'a RevisionStatus,
}

#[derive(Template)]
#[template(path = "fragments/result_event.html")]
pub struct ResultEventFragment<'a> {
    pub explorer: &'a str,
    pub scope_control: &'a str,
    pub announcement: &'a str,
}

pub struct PatternResultData {
    pub pattern_id: usize,
    pub pattern_name: String,
    pub count: u64,
    pub count_label: String,
    pub auto_load: bool,
    pub authored_index: usize,
    pub metadata_groups: Box<[crate::pattern_catalog::MetadataGroup]>,
}

#[derive(Template)]
#[template(path = "fragments/results_explorer.html")]
pub struct ResultsExplorer<'a> {
    pub workbench_id: &'a str,
    pub revision: u64,
    pub results: &'a [PatternResultData],
    pub has_results: bool,
    pub grouping_fields: &'a [crate::pattern_catalog::GroupingField],
    pub default_grouping_field: &'a str,
    pub section_analysis: Option<&'a TrustedFragmentHtml>,
}

pub struct SectionOption {
    pub label: String,
    pub count_label: String,
    pub selected: bool,
}

#[derive(Template)]
#[template(path = "fragments/section_scope_control.html")]
pub struct SectionScopeControl<'a> {
    pub workbench_id: &'a str,
    pub revision: u64,
    pub options: &'a [SectionOption],
    pub selected_count: usize,
    pub total_count: usize,
    pub summary: String,
    pub summary_empty: bool,
    pub distance_value: &'a str,
    pub oob: bool,
}

#[derive(Template)]
#[template(path = "fragments/section_analysis.html")]
pub struct SectionAnalysis<'a> {
    pub workbench_id: &'a str,
    pub revision: u64,
    pub selected_labels: &'a [String],
    pub has_selection: bool,
    pub has_comparison: bool,
    pub distance_jensen_shannon: bool,
    pub distance_cosine: bool,
    pub comparison: Option<&'a crate::section_comparison::SectionComparison>,
    pub dendrogram: Option<&'a crate::section_dendrogram::SectionDendrogram>,
    pub section_dendrogram: Option<&'a crate::section_dendrogram::SectionDendrogram>,
}

pub struct LivePatternResultData<'a> {
    pub pattern_name: &'a str,
    pub count: u64,
    pub count_label: String,
}

#[derive(Template)]
#[template(path = "fragments/live_results.html")]
pub struct LiveResults<'a> {
    pub results: &'a [LivePatternResultData<'a>],
}

/// What a pattern *is*, beside what it matched.
///
/// The detail pane reported counts, distributions and hits and said nothing
/// about the definition behind them, which for a fragment-heavy catalog means
/// the one question a reader has — what does this pattern actually match? —
/// was answerable only by scrolling the catalog and resolving `@names` by
/// hand.
pub struct PatternDefinitionView {
    /// The definition as the catalog spells it.
    pub declaration: String,
    /// The same definition with every reference resolved, absent when the
    /// definition stands on none.
    pub expanded: Option<String>,
    /// The names resolved, in first-appearance order.
    pub references: Vec<String>,
    /// Pre-rendered SVG railroad / state-flow diagram of the expanded
    /// definition's expression.
    pub diagram: Option<TrustedFragmentHtml>,
}

#[derive(Template)]
#[template(path = "fragments/pattern_detail.html")]
pub struct PatternDetailFragment<'a> {
    pub pattern_name: &'a str,
    /// Absent when the run's catalog no longer parses or holds no definition
    /// of this name. The band is then omitted rather than reported: results
    /// are still on screen and still correct, and a broken source is not
    /// something a reader of *these* results can act on.
    pub definition: Option<PatternDefinitionView>,
    /// Raw exact count for i18n interpolation args.
    pub exact_total: u64,
    pub exact_total_label: String,
    pub retained_count: usize,
    pub projection: &'a crate::pattern_detail::PatternDetailProjection,
    pub workbench_id: &'a str,
    pub revision: u64,
    pub pattern_key: usize,
}

impl PatternDetailFragment<'_> {
    /// Whether the rows below are a sample rather than every match.
    ///
    /// The distinction is not cosmetic: the pane retains a reservoir sample,
    /// so "1,000 of 12,000" now means a thousand drawn from across the corpus
    /// and not its first thousand. A reader interprets an example list
    /// differently depending on which it is.
    pub fn sampled(&self) -> bool {
        self.exact_total > self.retained_count as u64
    }
}

#[derive(Template)]
#[template(path = "fragments/hit_detail.html")]
pub struct HitDetailFragment<'a> {
    pub reference_ordinal: usize,
    pub workbench_id: &'a str,
    pub revision: u64,
    pub pattern_key: usize,
    pub id_prefix: String,
    pub tokens: Vec<KwicTokenData>,
    pub capture_lanes: Vec<KwicCaptureLaneData>,
    pub captures: Vec<KwicCaptureData>,
    pub graph_bindings: Vec<GraphBindingData>,
    pub provenance: Option<crate::provenance_view::HitProvenance>,
}

pub fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[derive(Clone)]
pub struct KwicTokenData {
    pub index: usize,
    pub is_match: bool,
    pub orth: String,
    pub lemma: String,
    pub pos: String,
    pub cform: String,
}

#[derive(Clone)]
pub struct KwicCaptureLaneData {
    pub captures: Vec<KwicCaptureData>,
}

#[derive(Clone)]
pub struct KwicCaptureData {
    pub ordinal: usize,
    pub name: String,
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub grid_start: usize,
    pub grid_span: usize,
    pub detail_html: TrustedFragmentHtml,
}

#[derive(Clone)]
pub struct GraphBindingData {
    pub name: String,
    pub is_node: bool,
    pub identity: String,
    pub relation: Option<String>,
    pub attributes: Vec<GraphBindingAttributeData>,
}

#[derive(Clone)]
pub struct GraphBindingAttributeData {
    pub name: String,
    pub value: String,
}

#[derive(Template)]
#[template(path = "fragments/capture_inspection.html")]
pub struct CaptureInspectionFragment<'a> {
    pub capture: &'a CaptureInspection,
}

#[derive(Template)]
#[template(path = "fragments/token_inspection.html")]
pub struct TokenInspectionFragment {
    pub inspection: TokenInspection,
    pub total_fields: usize,
    pub surface_copy: String,
    pub machine_copy: String,
}

impl TokenInspectionFragment {
    pub fn new(inspection: TokenInspection) -> Self {
        let surface_copy = inspection.summary.orth.clone().unwrap_or_default().into();
        let machine_copy = inspection
            .groups
            .iter()
            .flat_map(|group| group.fields.iter())
            .filter_map(|field| {
                field
                    .value
                    .as_ref()
                    .map(|value| format!("{}={value}", field.meta.key))
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            inspection,
            total_fields: TokenAttributeIndex::ALL.len(),
            surface_copy,
            machine_copy,
        }
    }
}

#[derive(Template)]
#[template(path = "sample_results.html")]
pub struct SampleResults {
    pub analysis: crate::sample::SampleAnalysis,
}

#[derive(Template)]
#[template(path = "fragments/error.html")]
pub struct ErrorFragment<'a> {
    pub message: &'a str,
}

pub fn render_error(message: &str) -> Result<String, askama::Error> {
    ErrorFragment { message }.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section_fragment(
        comparison: Option<&crate::section_comparison::SectionComparison>,
        dendrogram: Option<&crate::section_dendrogram::SectionDendrogram>,
        section_dendrogram: Option<&crate::section_dendrogram::SectionDendrogram>,
    ) -> TrustedFragmentHtml {
        let html = SectionAnalysis {
            workbench_id: "wb",
            revision: 1,
            selected_labels: &[],
            has_selection: true,
            has_comparison: comparison.is_some(),
            distance_jensen_shannon: true,
            distance_cosine: false,
            comparison,
            dendrogram,
            section_dendrogram,
        }
        .render()
        .expect("renders section analysis");
        TrustedFragmentHtml::from_rendered(html)
    }

    /// Renders against a tally that agrees with the retained sample, which is
    /// what these template tests always assumed the distribution was.
    fn project(
        hits: Vec<crate::pattern_detail::RetainedHitText>,
    ) -> crate::pattern_detail::PatternDetailProjection {
        let sections = crate::pattern_detail::sample_sections(&hits);
        crate::pattern_detail::PatternDetailProjection::new(
            hits,
            &sections,
            crate::snapshot::PatternKey::new(0).expect("pattern zero exists"),
        )
    }

    #[test]
    fn pattern_detail_separates_exact_total_sample_distribution_and_rows() {
        let hits = vec![
            crate::pattern_detail::RetainedHitText {
                reference_ordinal: 0,
                left: "前".into(),
                key: "甲".into(),
                right: "後".into(),
                section_labels: Box::new([]),
            },
            crate::pattern_detail::RetainedHitText {
                reference_ordinal: 1,
                left: "左".into(),
                key: "乙".into(),
                right: "右".into(),
                section_labels: Box::new([]),
            },
        ];
        let projection = project(hits);
        let html = PatternDetailFragment {
            definition: None,
            pattern_name: "broad",
            exact_total: 72_730,
            exact_total_label: "72,730".into(),
            retained_count: 2,
            projection: &projection,
            workbench_id: "wb",
            revision: 7,
            pattern_key: 3,
        }
        .render()
        .unwrap();
        // 2 of 72,730 is a sample, and the pane says which kind: the rows are
        // drawn from across the corpus, not taken from its opening.
        assert!(html.contains("72,730 corpus matches · 2 shown, sampled at random"));
        assert!(html.contains("data-i18n=\"detail.counts_sampled\""));
        assert!(!html.contains("retained rows"));
        assert_eq!(html.matches("<tr class=\"kwic-row\"").count(), 2);
        assert!(html.contains("role=\"columnheader\""));
        assert!(html.contains("role=\"rowheader\""));
        assert!(html.contains("role=\"gridcell\""));
        assert!(html.contains("data-variation=\"0\""));
        assert!(!html.contains("<section id=\"hit-inspector\""));
        assert!(html.contains("data-expand-sentence"));
        assert!(!html.contains("<meter"));
        assert!(html.contains("data-detail-view=\"definition\""));
        assert!(html.contains("data-ranked-list=\"surface\""));
        assert!(html.contains("surface-bar-fill"));
        assert!(!html.contains("surface-dock"));
        assert!(!html.contains("kwic-token-ribbon"));
        assert!(!html.contains("capture-lane"));
    }

    #[test]
    fn pattern_detail_uses_declarative_ranked_order_and_textual_tails() {
        let hits = (0..25)
            .map(|index| crate::pattern_detail::RetainedHitText {
                reference_ordinal: index,
                left: "前".into(),
                key: format!("surface-{index:02}"),
                right: "後".into(),
                section_labels: vec![format!("section-{index:02}")].into_boxed_slice(),
            })
            .collect();
        let projection = project(hits);
        let html = PatternDetailFragment {
            definition: None,
            pattern_name: "broad",
            exact_total: 25,
            exact_total_label: "25".into(),
            retained_count: 25,
            projection: &projection,
            workbench_id: "wb",
            revision: 7,
            pattern_key: 3,
        }
        .render()
        .unwrap();

        assert!(html.contains("data-ranked-order"));
        assert!(html.contains("value=\"count-desc\""));
        assert!(html.contains("value=\"name-asc\""));
        // The surface tail describes the corpus and counts sentences rather
        // than hits, because the cross-tabulation it reads counts a sentence
        // once however many times the surface occurs in it.
        assert!(html.contains("Other: 6 sentences across 6 variations"));
        // The section list has no tail at all: it is bounded by the corpus's
        // own label inventory, so every row is drawn.
        assert!(!html.contains("more sections"));
        assert!(!html.contains("data-ranked-tail=\"section\""));
        assert!(!html.contains("data-ranked-toggle=\"section\""));
        assert!(!html.contains(
            "data-ranked-tail=\"surface\"><span>Other</span><span class=\"surface-bar\"",
        ));
    }

    #[test]
    fn test_index_page_renders_full_layout_via_inheritance() {
        let page = IndexPage {
            corpora: vec![],
            workbench_id: "test-tab".into(),
            current_page: "corpus",
            seed_pattern: String::new(),
            auto_run: false,
            run_when_ready: false,
            link_banner: None,
            link_notice: None,
            corpus_context: crate::corpus_context::CorpusContextView::preparing(
                "test-corpus".into(),
                Some(42),
            ),
            catalog_keys: vec![],
            retained_options: crate::state::retention_offers(1_000),
            retained_default: 1_000,
        };
        let html = page.render().unwrap();
        // An empty seed leaves the editor blank (placeholder shows), so nothing
        // sits between the textarea tags.
        assert!(html.contains("required></textarea>"));
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<title>lingrep-web</title>"));
        assert!(html.contains("src=\"assets/results-explorer.js\""));
        assert!(html.contains("src=\"assets/app.js\""));
        assert!(!html.contains("item.setAttribute(\"aria-pressed\""));
        // The copy-to-clipboard and auto-expand behavior lives in app.js so
        // the CSP can forbid inline scripts.
        assert!(!html.contains("status.textContent = message"));
    }

    /// The query band: what a running edit needs is inside one pinnable
    /// element, with no repeated chrome above it.
    #[test]
    fn the_index_page_puts_the_edit_loop_in_one_band_and_drops_the_repeated_chrome() {
        let page = IndexPage {
            corpora: vec![],
            workbench_id: "test-tab".into(),
            current_page: "corpus",
            seed_pattern: String::new(),
            auto_run: false,
            run_when_ready: false,
            link_banner: None,
            link_notice: None,
            corpus_context: crate::corpus_context::CorpusContextView::preparing(
                "test-corpus".into(),
                Some(42),
            ),
            catalog_keys: vec![],
            retained_options: crate::state::retention_offers(1_000),
            retained_default: 1_000,
        };
        let html = page.render().unwrap();
        let band = html
            .split_once("data-query-band")
            .expect("the band exists")
            .1;
        let band = band
            .split_once("<div id=\"live-results\"")
            .expect("the band closes")
            .0;
        for inside in [
            "corpus-context",
            "data-query-workspace",
            "id=\"search-status\"",
            "id=\"results-stale\"",
        ] {
            assert!(band.contains(inside), "{inside} belongs inside the band");
        }
        // The results are outside it, which is the whole point: the band is
        // pinned and they scroll underneath rather than being pushed down.
        assert!(!band.contains("id=\"results\""));
        // An h1 repeating the nav brand, an h2 repeating the active nav item,
        // and a blurb restating both cost ~230px above every result.
        assert!(!html.contains("<h1>lingrep-web</h1>"));
        assert!(!html.contains("search-intro"));
        assert!(!html.contains("hero.blurb"));
        // Four lines, not ten: the band is sized for the edit loop.
        assert!(html.contains("name=\"pattern\" rows=\"4\""));
        // One element per id. Two elements shared each of these, so htmx wrote
        // to the first and the second was dead markup that a reader watching
        // the wrong one saw never update.
        assert_eq!(html.matches("id=\"search-status\"").count(), 1);
        assert_eq!(html.matches("id=\"results-stale\"").count(), 1);
        // Syntax help is an overlay outside the band, not a disclosure inside
        // it: opened in place it grew the band and displaced the editor.
        assert!(html.contains("<dialog class=\"editor-help-dialog\""));
        assert!(!html.contains("<details class=\"editor-help\""));
        // The controls row keeps only the opener; the reference text itself
        // sits after the query library, outside the workspace.
        let controls = band
            .split_once("permalink-controls")
            .expect("the controls row")
            .1
            .split_once("</div>")
            .expect("the controls row closes")
            .0;
        assert!(controls.contains("data-editor-help-open"));
        assert!(!controls.contains("class=\"help\""));
        assert!(
            html.find("data-query-library") < html.find("editor-help-dialog"),
            "the help overlay must not sit inside the query workspace"
        );
        // Retention is a run parameter and posts with the query. The ceiling
        // is selected, so an untouched page runs what it always ran.
        let form = band.split_once("data-corpus-search").expect("the form").1;
        assert!(form.contains("name=\"retained\""));
        assert!(form.contains("<option value=\"20\">20</option>"));
        assert!(form.contains("<option value=\"1000\" selected>1000</option>"));
        assert!(form.contains("name=\"derive_compounds\" value=\"true\""));
        assert!(form.contains("name=\"compound_category\""));
        assert!(form.contains("value=\"compound\""));
        assert!(form.contains("value=\"source\""));
    }

    #[test]
    fn test_index_page_renders() {
        let page = IndexPage {
            corpora: vec![],
            workbench_id: "test-tab".into(),
            current_page: "corpus",
            // `&` and `<` exercise the textarea's HTML escaping (lingrep patterns uses `&`).
            seed_pattern: "seed := [lemma=だ & orth=そう].\n<b>".into(),
            auto_run: false,
            run_when_ready: false,
            link_banner: None,
            link_notice: None,
            corpus_context: crate::corpus_context::CorpusContextView::preparing(
                "test-corpus".into(),
                Some(42),
            ),
            catalog_keys: vec![],
            retained_options: crate::state::retention_offers(1_000),
            retained_default: 1_000,
        };
        let html = page.render().unwrap();
        assert!(html.contains("hx-post"));
        assert!(html.contains("api/search"));
        assert!(html.contains("textarea id=\"corpus-pattern\""));
        assert!(html.contains("rows=\"4\""));
        assert!(html.contains("data-pattern-editor"));
        assert!(html.contains("data-pattern-submit=\"corpus-pattern\""));
        assert!(html.contains("class=\"query-workspace\""));
        // Editor fold chrome: workspace marker, collapsed bar, fold button.
        assert!(html.contains("data-query-workspace"));
        assert!(html.contains("data-editor-collapsed-bar"));
        assert!(html.contains("data-editor-fold"));
        assert!(html.contains("data-editor-toggle"));
        assert!(html.contains("data-query-library"));
        assert!(html.contains("role=\"tablist\""));
        assert!(html.contains("data-query-tab=\"recent\""));
        assert!(html.contains("data-query-tab=\"saved\""));
        assert!(html.contains("data-query-list"));
        assert!(html.contains("data-query-status"));
        assert!(html.contains("data-save-current"));
        assert!(html.contains("data-clear-recent"));
        assert!(html.contains("type=\"module\" src=\"assets/query-library.js\""));
        let form_end = html.find("</form>").unwrap();
        let library = html.find("data-query-library").unwrap();
        assert!(
            form_end < library,
            "the query library must not be nested in the search form"
        );
        // The seed grammar renders as the editor's content, HTML-escaped.
        assert!(html.contains("seed := [lemma=だ &amp; orth=そう]."));
        assert!(html.contains("&lt;b&gt;"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn test_stream_container() {
        let status = crate::revision_status::project_revision_status(None);
        let container = StreamContainer {
            request_id: "abc123",
            status: &status,
        };
        let html = container.render().unwrap();
        assert!(html.contains("sse-connect=\"api/stream/abc123\""));
        assert!(html.contains("sse-swap=\"progress,result,error\""));
        assert!(html.contains("id=\"live-results\" hx-swap-oob=\"innerHTML\""));
        assert!(html.contains("hx-swap=\"innerHTML\""));
        assert!(!html.contains("result:beforeend"));
    }

    #[test]
    fn revision_status_escapes_failure_and_limits_live_announcements() {
        use crate::revision_status::{RevisionStatus, RevisionStatusKind};

        let progress = RevisionStatus {
            omission_notice: None,
            omission_args: None,
            message: "Scanning…".into(),
            message_key: None,
            message_args: None,
            busy: true,
            stale: false,
            terminal: false,
            kind: RevisionStatusKind::Neutral,
        };
        let progress_html = RevisionStatusFragment { status: &progress }
            .render()
            .unwrap();
        assert!(progress_html.contains("aria-live=\"off\""));
        assert!(!progress_html.contains("role=\"status\""));

        let failure = RevisionStatus {
            omission_notice: None,
            omission_args: None,
            message: "Unexpected <script>alert(1)</script>".into(),
            message_key: None,
            message_args: None,
            busy: false,
            stale: true,
            terminal: true,
            kind: RevisionStatusKind::Error,
        };
        let failure_html = RevisionStatusFragment { status: &failure }
            .render()
            .unwrap();
        assert!(failure_html.contains("role=\"status\""));
        assert!(failure_html.contains("&lt;script&gt;"));
        assert!(!failure_html.contains("<script>"));
        assert!(failure_html.contains("Latest query did not complete"));
    }

    #[test]
    fn revision_status_completed_with_omissions_renders_a_notice_line() {
        use crate::revision_status::{RevisionStatus, RevisionStatusKind};

        let completed = RevisionStatus {
            omission_notice: Some(
                "Skipped 2 oversized sentences (first: sentence 338066, ~35.2 MiB tokenized > ~32.0 MiB limit)."
                    .into(),
            ),
            omission_args: None,
            message: "Results are current.".into(),
 message_key: None,
 message_args: None,
            busy: false,
            stale: false,
            terminal: true,
            kind: RevisionStatusKind::Success,
        };
        let html = RevisionStatusFragment { status: &completed }
            .render()
            .unwrap();
        assert!(html.contains(r#"class="omission-notice""#), "{html}");
        assert!(html.contains("Skipped 2 oversized sentences"), "{html}");

        let clean = RevisionStatus {
            omission_notice: None,
            omission_args: None,
            message: "Results are current.".into(),
            message_key: None,
            message_args: None,
            busy: false,
            stale: false,
            terminal: true,
            kind: RevisionStatusKind::Success,
        };
        let clean_html = RevisionStatusFragment { status: &clean }.render().unwrap();
        assert!(!clean_html.contains("omission-notice"), "{clean_html}");
    }

    /// The band exists to make rows comparable, so what it must render is one
    /// scale: `intensity` is already relative to the whole table, and the
    /// template must not reintroduce a per-row denominator.
    #[test]
    fn the_comparison_band_shades_every_row_on_one_scale() {
        use crate::section_comparison::{
            ComparisonCell, ComparisonColumn, ComparisonRow, SectionComparison,
        };

        let cell = |count: u64, intensity: u64, rate: u64| ComparisonCell {
            count,
            intensity,
            rate_tenths: rate,
            rate_label: format!("{}.{}%", rate / 10, rate % 10),
        };
        let comparison = SectionComparison {
            columns: vec![
                ComparisonColumn {
                    label: "methods".into(),
                    universe: 40,
                },
                ComparisonColumn {
                    label: "results".into(),
                    universe: 20,
                },
            ],
            rows: vec![
                ComparisonRow {
                    pattern_key: 0,
                    pattern_name: "a".into(),
                    cells: vec![cell(8, 100, 200), cell(2, 25, 100)],
                    displayed_total: 10,
                },
                ComparisonRow {
                    pattern_key: 1,
                    pattern_name: "b".into(),
                    cells: vec![cell(1, 13, 25), cell(4, 50, 200)],
                    displayed_total: 5,
                },
            ],
            hidden_sections: 3,
            max_count: 8,
        };
        let section_analysis = section_fragment(Some(&comparison), None, None);
        let html = ResultsExplorer {
            workbench_id: "wb",
            revision: 1,
            results: &[],
            has_results: false,
            grouping_fields: &[],
            default_grouping_field: "",
            section_analysis: Some(&section_analysis),
        }
        .render()
        .unwrap();

        assert!(html.contains("class=\"section-comparison\""), "{html}");
        let section_panel = html
            .find("data-detail-view=\"sections\" data-section-comparison")
            .expect("section comparison panel");
        assert!(
            !html.contains("class=\"section-selection-panel\""),
            "scope selection lives in the corpus context"
        );
        assert!(
            !html.contains("class=\"corpus-analysis-scope\""),
            "the corpus scope control is not duplicated in the results tab"
        );
        assert!(section_panel < html.len(), "{html}");
        assert!(
            html.contains("data-detail-view=\"sections\" data-section-comparison"),
            "{html}"
        );
        assert!(html.contains("data-detail-tab=\"sections\""), "{html}");
        // `b`'s largest cell is 4, but it is shaded 50% because the table's
        // maximum is 8. A per-row scale would print 100% here.
        assert!(html.contains("opacity:50%"), "{html}");
        assert!(html.contains("opacity:100%"), "{html}");
        // The section size is on the header, because equal counts under
        // unequal sections are not equal densities.
        assert!(
            html.contains(">methods</span><span class=\"comparison-universe\">40<"),
            "{html}"
        );
        assert!(html.contains("title=\"4 · 20.0%\""), "{html}");
        assert!(html.contains("3 smaller sections are not shown"), "{html}");
    }

    /// The tree is drawn server-side, so the geometry is the contract: every
    /// leaf gets a label, every join gets a polyline, and the join's tooltip
    /// carries the distance a reader reads the axis against.
    #[test]
    fn the_dendrogram_band_draws_one_leaf_and_one_join_per_pattern() {
        use crate::section_comparison::{
            ComparisonCell, ComparisonColumn, ComparisonRow, SectionComparison,
        };
        use crate::section_dendrogram::SectionDendrogram;

        let row = |key: usize, name: &str, counts: [u64; 2]| ComparisonRow {
            pattern_key: key,
            pattern_name: name.into(),
            cells: counts
                .iter()
                .map(|count| ComparisonCell {
                    count: *count,
                    intensity: 0,
                    rate_tenths: 0,
                    rate_label: String::new(),
                })
                .collect(),
            displayed_total: counts.iter().sum(),
        };
        let comparison = SectionComparison {
            columns: vec![
                ComparisonColumn {
                    label: "methods".into(),
                    universe: 100,
                },
                ComparisonColumn {
                    label: "results".into(),
                    universe: 100,
                },
            ],
            rows: vec![
                row(0, "left", [10, 0]),
                row(1, "right", [0, 10]),
                row(2, "even", [5, 5]),
            ],
            hidden_sections: 0,
            max_count: 10,
        };
        let dendrogram = SectionDendrogram::new(&comparison).expect("three patterns cluster");
        let section_analysis = section_fragment(Some(&comparison), Some(&dendrogram), None);
        let html = ResultsExplorer {
            workbench_id: "wb",
            revision: 1,
            results: &[],
            has_results: false,
            grouping_fields: &[],
            default_grouping_field: "",
            section_analysis: Some(&section_analysis),
        }
        .render()
        .unwrap();

        assert!(html.contains("class=\"section-dendrogram\""), "{html}");
        assert!(html.contains("data-detail-view=\"clusters\""), "{html}");
        assert!(html.contains("data-section-dendrogram"), "{html}");
        assert!(html.contains("data-detail-tab=\"clusters\""), "{html}");
        assert_eq!(
            html.matches("class=\"dendrogram-label\"").count(),
            3,
            "one label per pattern: {html}"
        );
        assert_eq!(
            html.matches("class=\"dendrogram-link\"").count(),
            2,
            "three leaves join in two steps: {html}"
        );
        // `even` sits between the two disjoint patterns, so it joins one of
        // them first at 0.56, and the root then averages that pattern's 0.56
        // against the disjoint pair's 1.00 — average linkage reading the whole
        // group rather than its nearest or its worst member.
        assert!(html.contains(">2 · 0.56<"), "{html}");
        assert!(html.contains(">3 · 0.78<"), "{html}");
        // The deepest join is the axis's right end, so the two agree. Pinned
        // by the label rather than by a coordinate, because the gutter's width
        // follows the pattern names.
        assert_eq!(
            html.matches("class=\"dendrogram-tick-label\"").count(),
            5,
            "five ticks across the scale: {html}"
        );
        assert!(html.contains(">0.00</text>"), "{html}");
        assert!(html.contains(">0.78</text>"), "{html}");
        // The full name stays reachable even when the drawn label is cut.
        assert!(html.contains(">left<title>left</title>"), "{html}");
        assert!(
            html.contains("data-i18n=\"results.cluster_js_distance\""),
            "{html}"
        );
        assert!(
            !html.contains("are omitted"),
            "all profiles in this comparison have observations: {html}"
        );
        assert!(
            !html.contains("data-section-dendrogram-transposed"),
            "the transposed band is absent when it was not built: {html}"
        );
    }

    /// The transpose is its own band with its own caption, drawn by the same
    /// macro, so the two trees cannot diverge in geometry while saying
    /// different things about what they cluster.
    #[test]
    fn the_transposed_band_clusters_sections_and_captions_itself_as_such() {
        use crate::section_comparison::{
            ComparisonCell, ComparisonColumn, ComparisonRow, SectionComparison,
        };
        use crate::section_dendrogram::SectionDendrogram;

        let row = |key: usize, name: &str, counts: [u64; 3]| ComparisonRow {
            pattern_key: key,
            pattern_name: name.into(),
            cells: counts
                .iter()
                .map(|count| ComparisonCell {
                    count: *count,
                    intensity: 0,
                    rate_tenths: 0,
                    rate_label: String::new(),
                })
                .collect(),
            displayed_total: counts.iter().sum(),
        };
        let column = |label: &str| ComparisonColumn {
            label: label.into(),
            universe: 100,
        };
        let comparison = SectionComparison {
            columns: vec![column("methods"), column("results"), column("discussion")],
            rows: vec![row(0, "p", [80, 40, 10]), row(1, "q", [20, 10, 80])],
            hidden_sections: 0,
            max_count: 80,
        };
        let sections =
            SectionDendrogram::by_pattern_profile(&comparison).expect("three sections cluster");
        let section_analysis = section_fragment(Some(&comparison), None, Some(&sections));
        let html = ResultsExplorer {
            workbench_id: "wb",
            revision: 1,
            results: &[],
            has_results: false,
            grouping_fields: &[],
            default_grouping_field: "",
            section_analysis: Some(&section_analysis),
        }
        .render()
        .unwrap();

        assert!(
            html.contains("data-section-dendrogram-transposed"),
            "{html}"
        );
        assert!(
            html.contains("Sections clustered by pattern profile"),
            "{html}"
        );
        assert_eq!(
            html.matches("class=\"dendrogram-label\"").count(),
            3,
            "one label per section: {html}"
        );
        assert!(html.contains(">methods<title>methods</title>"), "{html}");
        // The two bands are told apart by their titles, so an assistive reader
        // is not given the same description twice.
        assert!(html.contains("id=\"dendrogram-sections-title\""), "{html}");
        assert!(
            !html.contains("id=\"dendrogram-patterns-title\""),
            "only the band that was built is drawn: {html}"
        );
    }

    #[test]
    fn a_run_with_nothing_to_compare_renders_no_comparison_band() {
        let html = ResultsExplorer {
            workbench_id: "wb",
            revision: 1,
            results: &[],
            has_results: false,
            grouping_fields: &[],
            default_grouping_field: "",
            section_analysis: None,
        }
        .render()
        .unwrap();
        assert!(!html.contains("section-comparison"), "{html}");
        assert!(!html.contains("section-dendrogram"), "{html}");
    }

    #[test]
    fn results_explorer_loads_first_matching_pattern() {
        let results = [PatternResultData {
            pattern_id: 2,
            pattern_name: "verb".into(),
            count: 72_730,
            count_label: "72,730".into(),
            auto_load: true,
            authored_index: 0,
            metadata_groups: Box::default(),
        }];
        let html = ResultsExplorer {
            workbench_id: "wb",
            revision: 7,
            results: &results,
            has_results: true,
            grouping_fields: &[],
            default_grouping_field: "",
            section_analysis: None,
        }
        .render()
        .unwrap();
        assert!(html.contains("class=\"results-explorer\""));
        assert!(html.contains("numeric-heading") && html.contains(">Corpus matches</th>"));
        assert!(html.contains("<td class=\"pattern-count\">72,730</td>"));
        assert!(
            html.contains("data-pattern-rail-filter"),
            "rail filter input renders"
        );
        assert!(!html.contains("exact corpus matches"));
        assert!(html.contains("id=\"detail-body\""));
        assert!(html.contains("api/pattern-detail/wb/7/2"));
        assert!(html.contains("hx-trigger=\"load, click\""));
        assert!(html.contains("hx-sync=\"#detail-body:replace\""));
        // The inspector dock is the explorer's only live region (hit details
        // announce politely); the catalog and detail panel do not.
        assert!(html.contains("id=\"inspector-dock\""));
        assert!(html.contains("id=\"inspector-body\""));
        assert!(!html.contains("kwic-inspector-row"));
        assert_eq!(
            html.matches("aria-live=\"polite\"").count(),
            1,
            "only the inspector dock body is a live region"
        );
        assert!(
            html.contains("class=\"pattern-catalog\" open"),
            "catalog must render open"
        );
        assert!(
            html.contains("class=\"pattern-catalog-heading\""),
            "desktop heading must render"
        );
        assert_eq!(
            html.matches("data-selected-pattern").count(),
            2,
            "heading and summary both carry the label"
        );
    }

    #[test]
    fn empty_explorer_is_current_and_idle() {
        let html = ResultsExplorer {
            workbench_id: "wb",
            revision: 8,
            results: &[],
            has_results: false,
            grouping_fields: &[],
            default_grouping_field: "",
            section_analysis: None,
        }
        .render()
        .unwrap();
        assert!(html.contains("No matches"));
        assert!(!html.contains("hx-trigger=\"load, click\""));
    }

    #[test]
    fn results_explorer_renders_export_download_link() {
        let explorer = ResultsExplorer {
            workbench_id: "wb1",
            revision: 5,
            results: &[],
            has_results: true,
            grouping_fields: &[],
            default_grouping_field: "",
            section_analysis: None,
        };
        let html = explorer.render().expect("renders");
        assert!(html.contains("href=\"api/export/wb1/5\""));
        assert!(html.contains("download"));
        assert!(html.contains("Download all results"));
    }

    #[test]
    fn corpus_context_ready_renders_counts_sections_and_digest_tooltip() {
        use crate::corpus_context::CorpusContextView;
        use crate::corpus_context::SectionCoverage;
        let sections = (0..10)
            .map(|i| SectionCoverage {
                label: format!("s{i}"),
                coverage: (10 - i) as u64 * 1000,
            })
            .collect();
        let view = CorpusContextView::ready(
            "mycorp".into(),
            "abcdef012345".into(),
            1_240_000,
            8_300,
            sections,
        );
        let html = CorpusContextPartial {
            corpora: vec![],
            corpus_context: view,
        }
        .render()
        .unwrap();
        assert!(html.contains(r#"<span data-i18n="corpus.label">Corpus:</span> <span title="abcdef012345">mycorp</span>"#));
        assert!(html.contains("data-corpus-digest=\"abcdef012345\""));
        assert!(html.contains("1,240,000 sentences · 8,300 documents"));
        assert!(html.contains("class=\"corpus-context-sections\""));
        assert!(html.contains("class=\"corpus-analysis-scope\""));
        assert!(html.contains("<span class=\"scope-count\">10</span>"));
        assert_eq!(html.matches(r#"class="corpus-section""#).count(), 10);
        assert!(html.contains("s9 <small>1k</small></span>"));
        assert!(!html.contains("corpus-section-row"));
        assert!(!html.contains("corpus-section-more"));
        assert!(!html.contains("corpus-section-overflow"));
        assert!(!html.contains("hx-get")); // terminal state: no polling
    }

    #[test]
    fn corpus_context_without_sections_keeps_the_scope_mount() {
        use crate::corpus_context::CorpusContextView;
        let view = CorpusContextView::ready("c".into(), "abcdef012345".into(), 5, 1, vec![]);
        let html = CorpusContextPartial {
            corpora: vec![],
            corpus_context: view,
        }
        .render()
        .unwrap();
        assert!(html.contains("5 sentences · 1 documents"));
        assert!(html.contains("corpus-context-sections"));
        assert!(html.contains(
            "id=\"corpus-analysis-scope\" class=\"corpus-context-sections\" hx-preserve"
        ));
        assert!(!html.contains("corpus-sections-label"));
        assert!(!html.contains("hx-get"));
    }

    #[test]
    fn corpus_context_preparing_polls_and_hides_counts() {
        use crate::corpus_context::CorpusContextView;
        let html = CorpusContextPartial {
            corpora: vec![],
            corpus_context: CorpusContextView {
                corpus_key: "sci".into(),
                ..CorpusContextView::preparing("c".into(), Some(42))
            },
        }
        .render()
        .unwrap();
        // The poll carries the selected key, so it keeps observing the corpus
        // the page is pointed at rather than drifting back to the default.
        assert!(html.contains("hx-get=\"api/corpus-context?k=sci\""));
        assert!(html.contains("hx-trigger=\"load delay:2s, every 3s\""));
        assert!(html.contains("Corpus is preparing"));
        assert!(!html.contains("sentences"));
        assert!(!html.contains("title=")); // no identity digest yet
        assert!(!html.contains("data-corpus-digest"));
    }

    #[test]
    fn corpus_context_failed_and_unavailable_are_terminal_notes() {
        use crate::corpus_context::CorpusContextView;
        let failed = CorpusContextPartial {
            corpora: vec![],
            corpus_context: CorpusContextView::failed("c".into()),
        }
        .render()
        .unwrap();
        assert!(failed.contains("Corpus unavailable."));
        assert!(!failed.contains("hx-get"));

        let unavailable = CorpusContextPartial {
            corpora: vec![],
            corpus_context: CorpusContextView::details_unavailable(
                "c".into(),
                "abcdef012345".into(),
                42,
            ),
        }
        .render()
        .unwrap();
        assert!(unavailable.contains("42 sentences"));
        assert!(unavailable.contains("Corpus details unavailable."));
        assert!(!unavailable.contains("documents"));
        assert!(!unavailable.contains("hx-get"));
    }

    #[test]
    fn corpus_context_sentence_only_has_no_documents_or_note() {
        use crate::corpus_context::CorpusContextView;
        let html = CorpusContextPartial {
            corpora: vec![],
            corpus_context: CorpusContextView::sentence_only("c".into(), "abcdef012345".into(), 7),
        }
        .render()
        .unwrap();
        assert!(html.contains("7 sentences"));
        assert!(!html.contains("documents"));
        assert!(!html.contains("corpus-context-note"));
    }
}
