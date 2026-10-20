use crate::tui::cache::{KwicLine, ResultCache, SurfaceEntry};
use crate::tui::types::{
    AppMode, LoadStatus, PatternId, PlaygroundFocus, Progress, RequestId, Tab,
};
use crate::tui::watcher::PatternWatcher;
use lingrep_cli_common::sanitize_terminal;
use std::collections::HashMap;
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

pub fn derive_viewmodel(model: &Model, viewport_height: u16) -> ViewModel<'_> {
    let mut patterns: Vec<(PatternId, &str, u64)> = Vec::new();
    let mut total_matches: u64 = 0;

    for (_req_id, freqs) in &model.cache.frequencies {
        for (pid, count) in freqs {
            if let Some(name) = model.pattern_ids.get(pid) {
                patterns.push((*pid, name.as_str(), *count));
                total_matches += count;
            }
        }
    }

    match model.sort_mode {
        SortMode::CountDesc => patterns.sort_by_key(|b| std::cmp::Reverse(b.2)),
        SortMode::CountAsc => patterns.sort_by_key(|a| a.2),
        SortMode::NameAsc => patterns.sort_by(|a, b| a.1.cmp(b.1)),
        SortMode::NameDesc => patterns.sort_by(|a, b| b.1.cmp(a.1)),
    }

    // Derive surface entries for selected pattern
    let mut surface_entries: Vec<&SurfaceEntry> = Vec::new();
    let mut surface_total = 0;
    let mut surface_window_start = 0;
    if let Some(pid) = model.selected_pattern {
        for ((_req_id, cache_pid), entries) in &model.cache.surface {
            if *cache_pid == pid {
                surface_entries = entries.window.iter().collect();
                surface_total = entries.len();
                surface_window_start = entries.window_start;
                break;
            }
        }
    }
    surface_entries.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.form.as_bytes().cmp(right.form.as_bytes()))
    });
    // Derive KWIC lines for selected pattern
    let mut kwic_lines: Vec<&KwicLine> = Vec::new();
    let mut kwic_total = 0;
    let mut kwic_window_start = 0;
    if let Some(pid) = model.selected_pattern {
        for ((_req_id, cache_pid), lines) in &model.cache.kwic {
            if *cache_pid == pid {
                kwic_lines = lines.window.iter().collect();
                kwic_total = lines.len();
                kwic_window_start = lines.window_start;
                break;
            }
        }
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

    let remaining = viewport_height.saturating_sub(kwic_key_width + 4);
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
    pub selected_pattern: Option<PatternId>,
    pub scroll: ScrollState,
    pub request_seq: u64,
    pub pattern_names: HashMap<String, PatternId>,
    pub pattern_ids: HashMap<PatternId, String>,
    pub sort_mode: SortMode,
    pub selected_kwic_line: usize,
    pub show_detail_panel: bool,
    pub playground: PlaygroundInput,
    pub playground_request_id: Option<RequestId>,
    pub playground_matches: Vec<PlaygroundMatch>,
    pub playground_error: Option<String>,
    pub pattern_watcher: Option<PatternWatcher>,
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
    pub progress: Progress,
}

impl Default for PatternLoadState {
    fn default() -> Self {
        Self {
            status: LoadStatus::Idle,
            request_id: None,
            progress: Progress::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SubTaskLoadState {
    pub status: LoadStatus,
    pub request_id: Option<RequestId>,
    pub pattern_id: Option<PatternId>,
    pub received_lines: usize,
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

    pub fn with_pattern_file(mut self, path: std::path::PathBuf) -> Self {
        if path.exists() && path != std::path::PathBuf::new() {
            self.pattern_watcher = Some(PatternWatcher::new(path));
        }
        self
    }

    pub fn next_request_id(&mut self) -> RequestId {
        let id = RequestId(self.request_seq);
        self.request_seq += 1;
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

    pub fn ensure_visible_window(&mut self) {
        let Some(pattern_id) = self.selected_pattern else {
            return;
        };
        let result = match self.mode {
            AppMode::Viewing { tab: Tab::Surface } => self
                .cache
                .surface
                .iter_mut()
                .find(|((_, cached_pattern), _)| *cached_pattern == pattern_id)
                .map(|(_, result)| {
                    self.scroll.surface.offset = self
                        .scroll
                        .surface
                        .offset
                        .min(result.len().saturating_sub(1));
                    result.ensure_window(self.scroll.surface.offset)
                }),
            AppMode::Viewing { tab: Tab::Context } => self
                .cache
                .kwic
                .iter_mut()
                .find(|((_, cached_pattern), _)| *cached_pattern == pattern_id)
                .map(|(_, result)| {
                    let last = result.len().saturating_sub(1);
                    self.scroll.context.offset = self.scroll.context.offset.min(last);
                    self.selected_kwic_line = self.selected_kwic_line.min(last);
                    result.ensure_window(self.scroll.context.offset)
                }),
            _ => None,
        };
        if let Some(Err(error)) = result {
            self.mode = AppMode::Error {
                message: format!("failed to read result page: {error}"),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cache::PagedResult;

    #[test]
    fn test_playground_input_default() {
        let input = PlaygroundInput::default();
        assert!(input.pattern.is_empty());
        assert!(input.sample.is_empty());
        assert_eq!(input.focus, PlaygroundFocus::Pattern);
    }

    #[test]
    fn test_model_default() {
        let model = Model::default();
        assert!(matches!(model.mode, AppMode::Viewing { .. }));
        assert!(model.pattern_draft.is_empty());
        assert!(model.selected_pattern.is_none());
    }

    #[test]
    fn test_model_is_cloneable() {
        let model = Model::default();
        let cloned = model.clone();
        assert_eq!(model.request_seq, cloned.request_seq);
    }

    #[test]
    fn test_task_states_default() {
        let tasks = TaskStates::default();
        assert_eq!(tasks.pattern.status, LoadStatus::Idle);
        assert!(tasks.pattern.request_id.is_none());
    }

    #[test]
    fn test_sub_task_load_state_default() {
        let state = SubTaskLoadState::default();
        assert_eq!(state.status, LoadStatus::Idle);
        assert!(state.request_id.is_none());
        assert!(state.pattern_id.is_none());
        assert_eq!(state.received_lines, 0);
    }

    #[test]
    fn test_model_new() {
        let model = Model::new();
        assert!(matches!(
            model.mode,
            AppMode::Viewing {
                tab: Tab::Frequencies
            }
        ));
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
    fn test_pattern_load_state_default() {
        let state = PatternLoadState::default();
        assert_eq!(state.status, LoadStatus::Idle);
        assert!(state.request_id.is_none());
        assert_eq!(state.progress.current, 0);
        assert_eq!(state.progress.total, 0);
    }

    #[test]
    fn test_viewmodel_sorts_by_count_desc() {
        use crate::tui::types::RequestId;
        use std::collections::HashMap as StdHashMap;

        let mut model = Model::default();
        let _ = model.get_or_create_pattern_id("zebra");
        let _ = model.get_or_create_pattern_id("alpha");

        let req_id = RequestId(0);
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
    fn test_model_has_playground() {
        let model = Model::default();
        assert!(model.playground.pattern.is_empty());
        assert!(model.playground.sample.is_empty());
    }

    #[test]
    fn test_viewmodel_bounds_kwic_selection() {
        use crate::tui::cache::{KwicLine, PagedResult};
        let mut model = Model::default();
        let pid = model.get_or_create_pattern_id("test");
        model.selected_pattern = Some(pid);
        model.selected_kwic_line = 100; // Out of bounds

        let req_id = RequestId(0);
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
        model.ensure_visible_window();

        let vm = derive_viewmodel(&model, 20);
        assert_eq!(model.selected_kwic_line, 0);
        assert_eq!(vm.selected_kwic_idx, Some(0));
    }

    #[test]
    fn high_cardinality_results_seek_pages_without_materializing_the_corpus() {
        use crate::tui::cache::{KwicLine, SurfaceEntry};
        use crate::tui::paged_store::DEFAULT_PAGE_ROWS;

        let mut model = Model::default();
        let pid = model.get_or_create_pattern_id("paged");
        model.selected_pattern = Some(pid);
        let key = (RequestId(88), pid);
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
        model.ensure_visible_window();
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
        model.ensure_visible_window();
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
