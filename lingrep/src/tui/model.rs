use crate::tui::cache::{KwicLine, ResultCache, SurfaceEntry};
use crate::tui::command::Command;
use crate::tui::types::{
    AppMode, LoadStatus, PatternId, PlaygroundFocus, Progress, RequestId, Tab,
};
use lingrep_cli_common::sanitize_terminal;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

pub struct ViewModel<'a> {
    pub patterns: Vec<(PatternId, &'a str, u64)>,
    pub total_matches: u64,
    pub surface_entries: Vec<&'a SurfaceEntry>,
    pub surface_total: usize,
    pub surface_window_start: usize,
    pub kwic_lines: Vec<&'a KwicLine>,
    pub kwic_total: usize,
    pub kwic_window_start: usize,
    pub kwic_key_width: u16,
    pub kwic_before_width: u16,
    pub kwic_after_width: u16,
    pub selected_kwic_idx: Option<usize>,
}

pub fn derive_viewmodel(model: &Model, viewport_width: u16) -> ViewModel<'_> {
    let patterns = model.ordered_patterns();
    let total_matches: u64 = patterns.iter().map(|(_, _, count)| *count).sum();

    // Derive surface entries for selected pattern from the active request only
    let mut surface_entries: Vec<&SurfaceEntry> = Vec::new();
    let mut surface_total = 0;
    let mut surface_window_start = 0;
    if let Some(pid) = model.selected_pattern
        && let Some(req_id) = model.tasks.surface.active_request_id()
        && let Some(entries) = model.cache.surface.peek(&(req_id, pid))
    {
        surface_entries = entries.window.iter().collect();
        surface_total = entries.len();
        surface_window_start = entries.window_start;
    }
    surface_entries.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.form.as_bytes().cmp(right.form.as_bytes()))
    });
    // Derive KWIC lines for selected pattern from the active request only
    let mut kwic_lines: Vec<&KwicLine> = Vec::new();
    let mut kwic_total = 0;
    let mut kwic_window_start = 0;
    if let Some(pid) = model.selected_pattern
        && let Some(req_id) = model.tasks.kwic.active_request_id()
        && let Some(lines) = model.cache.kwic.peek(&(req_id, pid))
    {
        kwic_lines = lines.window.iter().collect();
        kwic_total = lines.len();
        kwic_window_start = lines.window_start;
    }

    // Bound selected_kwic_idx to actual line count
    let selected_kwic_idx = model
        .selected_kwic_line
        .checked_sub(kwic_window_start)
        .filter(|index| *index < kwic_lines.len());

    // Compute column widths for KWIC display
    let kwic_key_width = kwic_lines
        .iter()
        .map(|line| UnicodeWidthStr::width(sanitize_terminal(&line.key).as_ref()))
        .max()
        .unwrap_or(5)
        .max(5) as u16
        + 2;

    let remaining = viewport_width.saturating_sub(kwic_key_width + 4);
    let kwic_before_width = remaining / 2;
    let kwic_after_width = remaining.saturating_sub(kwic_before_width);

    ViewModel {
        patterns,
        total_matches,
        surface_entries,
        surface_total,
        surface_window_start,
        kwic_lines,
        kwic_total,
        kwic_window_start,
        kwic_key_width,
        kwic_before_width,
        kwic_after_width,
        selected_kwic_idx,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    #[default]
    CountDesc,
    CountAsc,
    NameAsc,
    NameDesc,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            SortMode::CountDesc => SortMode::CountAsc,
            SortMode::CountAsc => SortMode::NameAsc,
            SortMode::NameAsc => SortMode::NameDesc,
            SortMode::NameDesc => SortMode::CountDesc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortMode::CountDesc => "Count ▼",
            SortMode::CountAsc => "Count ▲",
            SortMode::NameAsc => "Name ▲",
            SortMode::NameDesc => "Name ▼",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Scroll {
    pub offset: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub mode: AppMode,
    pub tasks: TaskStates,
    pub cache: ResultCache,
    pub pattern_draft: String,
    /// Text of the most recent pattern scan. Surface and KWIC scans compile
    /// this, not the draft, so they describe the patterns in the frequency table.
    pub scanned_pattern: String,
    pub selected_pattern: Option<PatternId>,
    pub scroll: ScrollState,
    pub request_seq: u64,
    pub(crate) page_request_seq: u64,
    pub pattern_names: HashMap<String, PatternId>,
    pub pattern_ids: HashMap<PatternId, String>,
    pub sort_mode: SortMode,
    pub selected_kwic_line: usize,
    pub show_detail_panel: bool,
    pub playground: PlaygroundInput,
    pub playground_request_id: Option<RequestId>,
    pub playground_matches: Vec<PlaygroundMatch>,
    pub playground_error: Option<String>,
    pub pattern_file: Option<PathBuf>,
    pub reload_status: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScrollState {
    pub frequencies: Scroll,
    pub surface: Scroll,
    pub context: Scroll,
    pub parsed: Scroll,
}

#[derive(Debug, Clone, Default)]
pub struct PlaygroundInput {
    pub pattern: String,
    pub sample: String,
    pub focus: PlaygroundFocus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaygroundMatch {
    pub start_token: usize,
    pub end_token: usize,
    pub matched_text: String,
    pub pattern_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskStates {
    pub pattern: PatternLoadState,
    pub surface: SubTaskLoadState,
    pub kwic: SubTaskLoadState,
}

#[derive(Debug, Clone)]
pub struct PatternLoadState {
    pub status: LoadStatus,
    pub request_id: Option<RequestId>,
    pub displayed_request_id: Option<RequestId>,
    pub progress: Progress,
}

impl PatternLoadState {
    pub fn active_request_id(&self) -> Option<RequestId> {
        self.request_id.or(self.displayed_request_id)
    }
}

impl Default for PatternLoadState {
    fn default() -> Self {
        Self {
            status: LoadStatus::Idle,
            request_id: None,
            displayed_request_id: None,
            progress: Progress::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SubTaskLoadState {
    pub status: LoadStatus,
    pub request_id: Option<RequestId>,
    pub displayed_request_id: Option<RequestId>,
    pub pattern_id: Option<PatternId>,
    pub received_lines: usize,
    pub(crate) page_request_id: Option<u64>,
}

impl SubTaskLoadState {
    pub fn active_request_id(&self) -> Option<RequestId> {
        self.request_id.or(self.displayed_request_id)
    }
}

impl Model {
    pub fn new() -> Self {
        Self {
            mode: AppMode::Viewing {
                tab: Tab::Frequencies,
            },
            ..Default::default()
        }
    }

    pub fn with_pattern_text(mut self, text: String) -> Self {
        self.pattern_draft = text;
        self
    }

    pub fn with_pattern_file(mut self, path: PathBuf) -> Self {
        self.pattern_file = (!path.as_os_str().is_empty()).then_some(path);
        self
    }

    pub fn next_request_id(&mut self) -> RequestId {
        let id = RequestId(self.request_seq);
        self.request_seq += 1;
        id
    }

    pub(crate) fn next_page_request_id(&mut self) -> u64 {
        let id = self.page_request_seq;
        self.page_request_seq += 1;
        id
    }

    pub fn get_or_create_pattern_id(&mut self, name: &str) -> PatternId {
        if let Some(&id) = self.pattern_names.get(name) {
            return id;
        }

        let id = PatternId(self.pattern_names.len() as u32);
        self.pattern_names.insert(name.to_string(), id);
        self.pattern_ids.insert(id, name.to_string());
        id
    }

    pub fn ordered_patterns(&self) -> Vec<(PatternId, &str, u64)> {
        let Some(req_id) = self.tasks.pattern.active_request_id() else {
            return Vec::new();
        };
        let Some(freqs) = self.cache.frequencies.peek(&req_id) else {
            return Vec::new();
        };
        let mut patterns: Vec<(PatternId, &str, u64)> = freqs
            .iter()
            .filter_map(|(pid, count)| {
                self.pattern_ids
                    .get(pid)
                    .map(|name| (*pid, name.as_str(), *count))
            })
            .collect();
        match self.sort_mode {
            SortMode::CountDesc => {
                patterns.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(b.1)));
            }
            SortMode::CountAsc => {
                patterns.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.cmp(b.1)));
            }
            SortMode::NameAsc => patterns.sort_by(|a, b| a.1.cmp(b.1)),
            SortMode::NameDesc => patterns.sort_by(|a, b| b.1.cmp(a.1)),
        }
        patterns
    }

    /// Clamps the active tab's scroll position against the cached result's
    /// known row count and returns an asynchronous window-load command whenever
    /// the resident page window does not start at the clamped offset.
    ///
    /// Pure: performs no disk or filesystem I/O.
    pub(crate) fn clamp_scroll_and_plan_window_load(&mut self) -> Option<Command> {
        let pattern_id = self.selected_pattern?;
        match self.mode {
            AppMode::Viewing { tab: Tab::Surface } => {
                let request_id = self.tasks.surface.active_request_id()?;
                let (last, load_plan) = {
                    let result = self.cache.surface.peek(&(request_id, pattern_id))?;
                    let last = result.len().saturating_sub(1);
                    let clamped = self.scroll.surface.offset.min(last);
                    let plan = result
                        .window_start_to_load(clamped)
                        .map(|start| (Arc::clone(&result.store), start));
                    (last, plan)
                };
                self.scroll.surface.offset = self.scroll.surface.offset.min(last);
                let Some((store, start)) = load_plan else {
                    self.tasks.surface.page_request_id = None;
                    return None;
                };
                let page_request_id = self.next_page_request_id();
                self.tasks.surface.page_request_id = Some(page_request_id);
                Some(Command::LoadSurfaceWindow {
                    request_id,
                    pattern_id,
                    page_request_id,
                    store,
                    start,
                })
            }
            AppMode::Viewing { tab: Tab::Context } => {
                let request_id = self.tasks.kwic.active_request_id()?;
                let (last, load_plan) = {
                    let result = self.cache.kwic.peek(&(request_id, pattern_id))?;
                    let last = result.len().saturating_sub(1);
                    let clamped = self.scroll.context.offset.min(last);
                    let plan = result
                        .window_start_to_load(clamped)
                        .map(|start| (Arc::clone(&result.store), start));
                    (last, plan)
                };
                self.scroll.context.offset = self.scroll.context.offset.min(last);
                self.selected_kwic_line = self.selected_kwic_line.min(last);
                let Some((store, start)) = load_plan else {
                    self.tasks.kwic.page_request_id = None;
                    return None;
                };
                let page_request_id = self.next_page_request_id();
                self.tasks.kwic.page_request_id = Some(page_request_id);
                Some(Command::LoadKwicWindow {
                    request_id,
                    pattern_id,
                    page_request_id,
                    store,
                    start,
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cache::PagedResult;

    #[test]
    fn new_model_starts_in_frequencies_with_idle_tasks_and_an_empty_editor() {
        for model in [Model::new(), Model::default()] {
            assert!(matches!(
                model.mode,
                AppMode::Viewing {
                    tab: Tab::Frequencies
                }
            ));
            assert!(model.pattern_draft.is_empty());
            assert!(model.selected_pattern.is_none());
            assert!(model.playground.pattern.is_empty());
            assert!(model.playground.sample.is_empty());
            assert_eq!(model.playground.focus, PlaygroundFocus::Pattern);
            assert_eq!(model.tasks.pattern.status, LoadStatus::Idle);
            assert!(model.tasks.pattern.request_id.is_none());
            assert_eq!(model.tasks.pattern.progress.current, 0);
            assert_eq!(model.tasks.pattern.progress.total, 0);
            for task in [&model.tasks.surface, &model.tasks.kwic] {
                assert_eq!(task.status, LoadStatus::Idle);
                assert!(task.request_id.is_none());
                assert!(task.pattern_id.is_none());
                assert_eq!(task.received_lines, 0);
            }
        }
    }

    #[test]
    fn test_next_request_id() {
        let mut model = Model::default();
        let id1 = model.next_request_id();
        let id2 = model.next_request_id();
        assert_eq!(id1, RequestId(0));
        assert_eq!(id2, RequestId(1));
        assert_eq!(model.request_seq, 2);
    }

    #[test]
    fn test_get_or_create_pattern_id() {
        let mut model = Model::default();

        let id1 = model.get_or_create_pattern_id("pattern_a");
        assert_eq!(id1, PatternId(0));

        let id2 = model.get_or_create_pattern_id("pattern_b");
        assert_eq!(id2, PatternId(1));

        let id1_again = model.get_or_create_pattern_id("pattern_a");
        assert_eq!(id1_again, PatternId(0));

        assert_eq!(model.pattern_names.len(), 2);
        assert_eq!(model.pattern_ids.len(), 2);
    }

    #[test]
    fn test_viewmodel_sorts_by_count_desc() {
        use crate::tui::types::RequestId;
        use std::collections::HashMap as StdHashMap;

        let mut model = Model::default();
        let _ = model.get_or_create_pattern_id("zebra");
        let _ = model.get_or_create_pattern_id("alpha");

        let req_id = RequestId(0);
        model.tasks.pattern.displayed_request_id = Some(req_id);
        model.cache.frequencies.put(req_id, StdHashMap::new());
        if let Some(freqs) = model.cache.frequencies.get_mut(&req_id) {
            let pid_zebra = *model.pattern_names.get("zebra").unwrap();
            let pid_alpha = *model.pattern_names.get("alpha").unwrap();
            freqs.insert(pid_zebra, 100);
            freqs.insert(pid_alpha, 200);
        }

        let vm = derive_viewmodel(&model, 20);
        assert_eq!(vm.patterns.len(), 2);
        assert_eq!(vm.patterns[0].2, 200); // alpha first (higher count)
        assert_eq!(vm.patterns[1].2, 100); // zebra second
    }

    #[test]
    fn test_viewmodel_derives_surface_entries() {
        use crate::tui::cache::SurfaceEntry;
        let mut model = Model::default();
        let pid = model.get_or_create_pattern_id("test_pattern");
        model.selected_pattern = Some(pid);

        let req_id = RequestId(0);
        model.tasks.surface.displayed_request_id = Some(req_id);
        let key = (req_id, pid);
        model.cache.surface.put(
            key,
            PagedResult::from_rows(&[
                SurfaceEntry {
                    form: "b".to_string(),
                    count: 5,
                },
                SurfaceEntry {
                    form: "a".to_string(),
                    count: 10,
                },
            ]),
        );

        let vm = derive_viewmodel(&model, 20);
        assert_eq!(vm.surface_entries.len(), 2);
        assert_eq!(vm.surface_entries[0].count, 10); // sorted by count desc
    }

    #[test]
    fn old_viewmodel_sorts_surfaces_and_preserves_kwic_arrival_order() {
        use crate::tui::cache::{KwicLine, PagedResult, SurfaceEntry};

        let mut model = Model::default();
        let pid = model.get_or_create_pattern_id("ordered");
        model.selected_pattern = Some(pid);
        let key = (RequestId(11), pid);
        model.tasks.surface.displayed_request_id = Some(RequestId(11));
        model.tasks.kwic.displayed_request_id = Some(RequestId(11));
        model.cache.surface.put(
            key,
            PagedResult::from_rows(&[
                SurfaceEntry {
                    form: "low".into(),
                    count: 2,
                },
                SurfaceEntry {
                    form: "high".into(),
                    count: 7,
                },
                SurfaceEntry {
                    form: "middle".into(),
                    count: 5,
                },
            ]),
        );
        model.cache.kwic.put(
            key,
            PagedResult::from_rows(&[
                KwicLine {
                    pre: "first-left".into(),
                    key: "first".into(),
                    post: "first-right".into(),
                    match_start: 0,
                    match_end: 1,
                },
                KwicLine {
                    pre: "second-left".into(),
                    key: "second".into(),
                    post: "second-right".into(),
                    match_start: 0,
                    match_end: 1,
                },
            ]),
        );

        let view = derive_viewmodel(&model, 40);
        assert_eq!(
            view.surface_entries
                .iter()
                .map(|entry| (entry.form.as_str(), entry.count))
                .collect::<Vec<_>>(),
            vec![("high", 7), ("middle", 5), ("low", 2)]
        );
        assert_eq!(
            view.kwic_lines
                .iter()
                .map(|line| line.key.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn test_viewmodel_bounds_kwic_selection() {
        use crate::tui::cache::{KwicLine, PagedResult};
        let mut model = Model::default();
        let pid = model.get_or_create_pattern_id("test");
        model.selected_pattern = Some(pid);
        model.selected_kwic_line = 100; // Out of bounds

        let req_id = RequestId(0);
        model.tasks.kwic.displayed_request_id = Some(req_id);
        let key = (req_id, pid);
        model.cache.kwic.put(
            key,
            PagedResult::from_rows(&[KwicLine {
                pre: "a".into(),
                key: "match".into(),
                post: "b".into(),
                match_start: 0,
                match_end: 0,
            }]),
        );
        model.mode = AppMode::Viewing { tab: Tab::Context };
        assert!(model.clamp_scroll_and_plan_window_load().is_none());

        let vm = derive_viewmodel(&model, 20);
        assert_eq!(model.selected_kwic_line, 0);
        assert_eq!(vm.selected_kwic_idx, Some(0));
    }

    #[test]
    fn with_pattern_file_stores_path_purely_without_touching_disk() {
        let missing = PathBuf::from("/nonexistent/lingrep-pure-test/patterns.lx");
        let model = Model::new().with_pattern_file(missing.clone());
        assert_eq!(model.pattern_file, Some(missing));
        assert_eq!(
            Model::new().with_pattern_file(PathBuf::new()).pattern_file,
            None
        );
    }

    #[test]
    fn high_cardinality_results_plan_page_loads_without_materializing_the_corpus() {
        use crate::tui::cache::{KwicLine, SurfaceEntry};
        use crate::tui::paged_store::DEFAULT_PAGE_ROWS;

        let mut model = Model::default();
        let pid = model.get_or_create_pattern_id("paged");
        model.selected_pattern = Some(pid);
        let key = (RequestId(88), pid);
        model.tasks.surface.displayed_request_id = Some(RequestId(88));
        model.tasks.kwic.displayed_request_id = Some(RequestId(88));
        let rows = DEFAULT_PAGE_ROWS * 3 + 9;
        model.cache.surface.put(
            key,
            PagedResult::from_rows(
                &(0..rows)
                    .map(|index| SurfaceEntry {
                        form: format!("{index:05}"),
                        count: (rows - index) as u64,
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        model.cache.kwic.put(
            key,
            PagedResult::from_rows(
                &(0..rows)
                    .map(|index| KwicLine {
                        pre: String::new(),
                        key: index.to_string(),
                        post: String::new(),
                        match_start: index,
                        match_end: index + 1,
                    })
                    .collect::<Vec<_>>(),
            ),
        );

        model.mode = AppMode::Viewing { tab: Tab::Surface };
        model.scroll.surface.offset = DEFAULT_PAGE_ROWS - 1;
        let Some(Command::LoadSurfaceWindow { store, start, .. }) =
            model.clamp_scroll_and_plan_window_load()
        else {
            panic!("expected async surface window load command");
        };
        assert_eq!(start, DEFAULT_PAGE_ROWS - 1);
        let loaded = store.read_window(start as u64, DEFAULT_PAGE_ROWS).unwrap();
        model
            .cache
            .surface
            .get_mut(&key)
            .unwrap()
            .install_window(start, loaded);
        let surface = derive_viewmodel(&model, 30);
        assert_eq!(surface.surface_total, rows);
        assert_eq!(surface.surface_window_start, DEFAULT_PAGE_ROWS - 1);
        assert_eq!(surface.surface_entries.len(), DEFAULT_PAGE_ROWS);
        assert_eq!(
            surface.surface_entries[0].form,
            format!("{:05}", DEFAULT_PAGE_ROWS - 1)
        );
        assert_eq!(
            surface.surface_entries[1].form,
            format!("{DEFAULT_PAGE_ROWS:05}")
        );

        model.mode = AppMode::Viewing { tab: Tab::Context };
        model.scroll.context.offset = DEFAULT_PAGE_ROWS * 2 + 3;
        model.selected_kwic_line = model.scroll.context.offset;
        let Some(Command::LoadKwicWindow { store, start, .. }) =
            model.clamp_scroll_and_plan_window_load()
        else {
            panic!("expected async kwic window load command");
        };
        assert_eq!(start, DEFAULT_PAGE_ROWS * 2 + 3);
        let loaded = store.read_window(start as u64, DEFAULT_PAGE_ROWS).unwrap();
        model
            .cache
            .kwic
            .get_mut(&key)
            .unwrap()
            .install_window(start, loaded);
        let kwic = derive_viewmodel(&model, 30);
        assert_eq!(kwic.kwic_total, rows);
        assert_eq!(kwic.kwic_window_start, DEFAULT_PAGE_ROWS * 2 + 3);
        assert_eq!(kwic.kwic_lines.len(), DEFAULT_PAGE_ROWS);
        assert_eq!(
            kwic.kwic_lines[0].key,
            (DEFAULT_PAGE_ROWS * 2 + 3).to_string()
        );
        assert_eq!(kwic.selected_kwic_idx, Some(0));
    }
}
