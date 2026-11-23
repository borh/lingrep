use crate::tui::command::Command;
use crate::tui::message::{ExplorerMessage, UiMsg};
use crate::tui::model::Model;
use crate::tui::types::ErrorCode;
use crate::tui::types::{AppMode, LoadStatus, PatternId, Tab, TaskKind};
use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};

pub fn update(msg: ExplorerMessage, mut model: Model) -> (Model, Option<Command>) {
    match msg {
        ExplorerMessage::Ui(ui_msg) => handle_ui(ui_msg, model),

        ExplorerMessage::PatternFileChanged(change) => (
            model,
            Some(Command::ReloadPatternFile {
                path: change.path,
                reload_count: change.reload_count,
            }),
        ),

        ExplorerMessage::PatternFileLoaded {
            path,
            reload_count,
            result,
        } => {
            let source = match result {
                Ok(source) => source,
                Err(error) => {
                    model.mode = AppMode::Error {
                        message: format!("failed to reload {}: {error}", path.display()),
                    };
                    return (model, None);
                }
            };
            model.pattern_draft = source;
            let pattern_count = model.pattern_names.len();
            model.reload_status = Some(format!(
                "Reloaded patterns ({pattern_count}) - #{reload_count}"
            ));
            start_pattern_scan(model)
        }

        ExplorerMessage::PatternProgress {
            request_id,
            current,
            total,
        } => {
            if model.tasks.pattern.request_id != Some(request_id) {
                return (model, None);
            }
            model.tasks.pattern.progress.current = current;
            model.tasks.pattern.progress.total = total;
            (model, None)
        }

        ExplorerMessage::PatternDelta {
            request_id,
            delta_counts,
        } => {
            if model.tasks.pattern.request_id != Some(request_id) {
                return (model, None);
            }

            model.tasks.pattern.displayed_request_id = Some(request_id);
            if model.cache.frequencies.get(&request_id).is_none() {
                model
                    .cache
                    .frequencies
                    .put(request_id, std::collections::HashMap::new());
            }

            let pids: Vec<(crate::tui::types::PatternId, u64)> = delta_counts
                .into_iter()
                .map(|(name, count)| (model.get_or_create_pattern_id(&name), count))
                .collect();

            if let Some(freqs) = model.cache.frequencies.get_mut(&request_id) {
                for (pid, delta) in pids {
                    *freqs.entry(pid).or_insert(0) += delta;
                }
            }

            (model, None)
        }

        ExplorerMessage::PatternFinished { request_id } => {
            if model.tasks.pattern.request_id != Some(request_id) {
                return (model, None);
            }

            model.tasks.pattern.status = LoadStatus::Complete;
            model.tasks.pattern.displayed_request_id = Some(request_id);
            model.tasks.pattern.request_id = None;
            model.mode = AppMode::Viewing {
                tab: Tab::Frequencies,
            };
            (model, None)
        }

        ExplorerMessage::PatternFailed {
            request_id,
            code,
            error,
        } => {
            if model.tasks.pattern.request_id != Some(request_id) {
                return (model, None);
            }

            model.tasks.pattern.request_id = None;
            if code == Some(ErrorCode::Cancelled) {
                model.tasks.pattern.status = LoadStatus::Idle;
                return (model, None);
            }
            model.tasks.pattern.status = LoadStatus::Failed;
            model.mode = AppMode::Error { message: error };
            (model, None)
        }

        ExplorerMessage::SurfaceFinished {
            request_id,
            pattern_id,
            result,
        } => {
            if model.tasks.surface.request_id != Some(request_id) {
                return (model, None);
            }
            model.tasks.surface.received_lines = result.len();
            model.cache.surface.put((request_id, pattern_id), result);
            model.scroll.surface.offset = 0;
            model.tasks.surface.page_request_id = None;
            model.tasks.surface.status = LoadStatus::Complete;
            model.tasks.surface.displayed_request_id = Some(request_id);
            model.tasks.surface.request_id = None;
            (model, None)
        }

        ExplorerMessage::SurfaceWindowLoaded {
            request_id,
            pattern_id,
            page_request_id,
            window_start,
            result,
        } => {
            if model.tasks.surface.active_request_id() != Some(request_id)
                || model.tasks.surface.page_request_id != Some(page_request_id)
            {
                return (model, None);
            }
            model.tasks.surface.page_request_id = None;
            match result {
                Ok(window) => {
                    if let Some(entry) = model.cache.surface.get_mut(&(request_id, pattern_id)) {
                        entry.install_window(window_start, window);
                    }
                }
                Err(error) => {
                    model.mode = AppMode::Error {
                        message: format!("failed to read result page: {error}"),
                    };
                }
            }
            (model, None)
        }

        ExplorerMessage::SurfaceFailed {
            request_id,
            pattern_id,
            code,
            error,
        } => {
            if model.tasks.surface.request_id != Some(request_id) {
                return (model, None);
            }
            model.tasks.surface.request_id = None;
            model.tasks.surface.page_request_id = None;
            if code == Some(ErrorCode::Cancelled) {
                model.tasks.surface.status = LoadStatus::Idle;
                model.tasks.surface.displayed_request_id = None;
                model.tasks.surface.pattern_id = None;
                return (model, None);
            }
            model.tasks.surface.status = LoadStatus::Failed;
            model.tasks.surface.pattern_id = Some(pattern_id);
            model.mode = AppMode::Error { message: error };
            (model, None)
        }

        ExplorerMessage::KwicFinished {
            request_id,
            pattern_id,
            result,
        } => {
            if model.tasks.kwic.request_id != Some(request_id) {
                return (model, None);
            }
            model.tasks.kwic.received_lines = result.len();
            model.cache.kwic.put((request_id, pattern_id), result);
            model.scroll.context.offset = 0;
            model.selected_kwic_line = 0;
            model.tasks.kwic.page_request_id = None;
            model.tasks.kwic.status = LoadStatus::Complete;
            model.tasks.kwic.displayed_request_id = Some(request_id);
            model.tasks.kwic.request_id = None;
            (model, None)
        }

        ExplorerMessage::KwicWindowLoaded {
            request_id,
            pattern_id,
            page_request_id,
            window_start,
            result,
        } => {
            if model.tasks.kwic.active_request_id() != Some(request_id)
                || model.tasks.kwic.page_request_id != Some(page_request_id)
            {
                return (model, None);
            }
            model.tasks.kwic.page_request_id = None;
            match result {
                Ok(window) => {
                    if let Some(entry) = model.cache.kwic.get_mut(&(request_id, pattern_id)) {
                        entry.install_window(window_start, window);
                    }
                }
                Err(error) => {
                    model.mode = AppMode::Error {
                        message: format!("failed to read result page: {error}"),
                    };
                }
            }
            (model, None)
        }

        ExplorerMessage::KwicFailed {
            request_id,
            pattern_id,
            code,
            error,
        } => {
            if model.tasks.kwic.request_id != Some(request_id) {
                return (model, None);
            }
            model.tasks.kwic.request_id = None;
            model.tasks.kwic.page_request_id = None;
            if code == Some(ErrorCode::Cancelled) {
                model.tasks.kwic.status = LoadStatus::Idle;
                model.tasks.kwic.displayed_request_id = None;
                model.tasks.kwic.pattern_id = None;
                return (model, None);
            }
            model.tasks.kwic.status = LoadStatus::Failed;
            model.tasks.kwic.pattern_id = Some(pattern_id);
            model.mode = AppMode::Error { message: error };
            (model, None)
        }

        ExplorerMessage::PlaygroundComplete {
            request_id,
            matches,
        } => {
            if model.playground_request_id == Some(request_id) {
                model.playground_request_id = None;
                model.playground_matches = matches;
                model.playground_error = None;
            }
            (model, None)
        }

        ExplorerMessage::PlaygroundFailed { request_id, error } => {
            if model.playground_request_id == Some(request_id) {
                model.playground_request_id = None;
                model.playground_matches.clear();
                model.playground_error = Some(error);
            }
            (model, None)
        }

        ExplorerMessage::Tick => (model, None),
    }
}

fn get_scroll_mut(model: &mut Model) -> Option<&mut crate::tui::model::Scroll> {
    match model.mode {
        AppMode::Viewing {
            tab: Tab::Frequencies,
        } => Some(&mut model.scroll.frequencies),
        AppMode::Viewing { tab: Tab::Surface } => Some(&mut model.scroll.surface),
        AppMode::Viewing { tab: Tab::Context } => Some(&mut model.scroll.context),
        AppMode::Viewing { tab: Tab::Parsed } => Some(&mut model.scroll.parsed),
        _ => None,
    }
}

fn handle_ui(msg: UiMsg, mut model: Model) -> (Model, Option<Command>) {
    match msg {
        UiMsg::TabChange(tab) => {
            model.mode = AppMode::Viewing { tab };
            let (model, cmd) = match tab {
                Tab::Surface => start_surface_if_selected(model),
                Tab::Context => start_kwic_if_selected(model),
                _ => (model, None),
            };
            (model, cmd)
        }

        UiMsg::SubmitPattern => start_pattern_scan(model),

        UiMsg::Cancel(TaskKind::Pattern) => {
            let Some(active_id) = model.tasks.pattern.request_id else {
                return (model, None);
            };
            model.tasks.pattern.status = LoadStatus::Cancelling;
            let cmd = Command::Cancel {
                kind: TaskKind::Pattern,
                request_id: active_id,
            };
            (model, Some(cmd))
        }

        UiMsg::Cancel(TaskKind::Surface) => {
            let Some(active_id) = model.tasks.surface.request_id else {
                return (model, None);
            };
            model.tasks.surface.status = LoadStatus::Cancelling;
            let cmd = Command::Cancel {
                kind: TaskKind::Surface,
                request_id: active_id,
            };
            (model, Some(cmd))
        }

        UiMsg::Cancel(TaskKind::Kwic) => {
            let Some(active_id) = model.tasks.kwic.request_id else {
                return (model, None);
            };
            model.tasks.kwic.status = LoadStatus::Cancelling;
            let cmd = Command::Cancel {
                kind: TaskKind::Kwic,
                request_id: active_id,
            };
            (model, Some(cmd))
        }

        UiMsg::DismissError => {
            model.mode = AppMode::Viewing {
                tab: Tab::Frequencies,
            };
            (model, None)
        }

        UiMsg::Exit => {
            model.mode = AppMode::Exiting;
            (model, None)
        }

        UiMsg::SelectNextPattern => {
            if let AppMode::Viewing {
                tab: Tab::Frequencies,
            } = model.mode
            {
                let pattern_list: Vec<PatternId> = model
                    .ordered_patterns()
                    .into_iter()
                    .map(|(pid, _, _)| pid)
                    .collect();

                if !pattern_list.is_empty() {
                    let current_idx: usize = model
                        .selected_pattern
                        .and_then(|pid| pattern_list.iter().position(|p| *p == pid))
                        .map_or(0, |i: usize| (i + 1).min(pattern_list.len() - 1));
                    model.selected_pattern = Some(pattern_list[current_idx]);
                }
            }
            (model, None)
        }

        UiMsg::SelectPrevPattern => {
            if let AppMode::Viewing {
                tab: Tab::Frequencies,
            } = model.mode
            {
                let pattern_list: Vec<PatternId> = model
                    .ordered_patterns()
                    .into_iter()
                    .map(|(pid, _, _)| pid)
                    .collect();

                if !pattern_list.is_empty() {
                    let current_idx: usize = model
                        .selected_pattern
                        .and_then(|pid| pattern_list.iter().position(|p| *p == pid))
                        .map_or(0, |i: usize| i.saturating_sub(1));
                    model.selected_pattern = Some(pattern_list[current_idx]);
                }
            }
            (model, None)
        }

        UiMsg::ConfirmSelection => {
            if matches!(model.mode, AppMode::EditingPattern) {
                model.mode = AppMode::Viewing {
                    tab: Tab::Frequencies,
                };
                return (model, None);
            }
            if let AppMode::Viewing {
                tab: Tab::Frequencies,
            } = model.mode
                && model.selected_pattern.is_some()
            {
                model.mode = AppMode::Viewing { tab: Tab::Surface };
                return start_surface_if_selected(model);
            }
            (model, None)
        }

        UiMsg::ScrollUp => {
            if let Some(scroll) = get_scroll_mut(&mut model) {
                scroll.offset = scroll.offset.saturating_sub(1);
            }
            if matches!(model.mode, AppMode::Viewing { tab: Tab::Context }) {
                model.selected_kwic_line = model.scroll.context.offset;
            }
            let cmd = model.clamp_scroll_and_plan_window_load();
            (model, cmd)
        }

        UiMsg::ScrollDown => {
            if let Some(scroll) = get_scroll_mut(&mut model) {
                scroll.offset = scroll.offset.saturating_add(1);
            }
            if matches!(model.mode, AppMode::Viewing { tab: Tab::Context }) {
                model.selected_kwic_line = model.scroll.context.offset;
            }
            let cmd = model.clamp_scroll_and_plan_window_load();
            (model, cmd)
        }

        UiMsg::PageUp => {
            if let Some(scroll) = get_scroll_mut(&mut model) {
                scroll.offset = scroll.offset.saturating_sub(20);
            }
            if matches!(model.mode, AppMode::Viewing { tab: Tab::Context }) {
                model.selected_kwic_line = model.scroll.context.offset;
            }
            let cmd = model.clamp_scroll_and_plan_window_load();
            (model, cmd)
        }

        UiMsg::PageDown => {
            if let Some(scroll) = get_scroll_mut(&mut model) {
                scroll.offset = scroll.offset.saturating_add(20);
            }
            if matches!(model.mode, AppMode::Viewing { tab: Tab::Context }) {
                model.selected_kwic_line = model.scroll.context.offset;
            }
            let cmd = model.clamp_scroll_and_plan_window_load();
            (model, cmd)
        }

        UiMsg::ToggleSort => {
            model.sort_mode = model.sort_mode.next();
            (model, None)
        }

        UiMsg::ToggleDetailPanel => {
            model.show_detail_panel = !model.show_detail_panel;
            (model, None)
        }

        UiMsg::StartEditPattern => {
            model.mode = AppMode::EditingPattern;
            (model, None)
        }

        UiMsg::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(scroll) = get_scroll_mut(&mut model) {
                        scroll.offset = scroll.offset.saturating_sub(1);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(scroll) = get_scroll_mut(&mut model) {
                        scroll.offset = scroll.offset.saturating_add(1);
                    }
                }
                _ => {}
            }
            if matches!(model.mode, AppMode::Viewing { tab: Tab::Context }) {
                model.selected_kwic_line = model.scroll.context.offset;
            }
            let cmd = model.clamp_scroll_and_plan_window_load();
            (model, cmd)
        }

        UiMsg::KeyPress(key) => {
            let playground_input = matches!(
                model.mode,
                AppMode::Viewing {
                    tab: Tab::Playground
                }
            );
            let input = match model.mode {
                AppMode::EditingPattern => Some(&mut model.pattern_draft),
                AppMode::Viewing {
                    tab: Tab::Playground,
                } => Some(match model.playground.focus {
                    crate::tui::types::PlaygroundFocus::Pattern => &mut model.playground.pattern,
                    crate::tui::types::PlaygroundFocus::Sample => &mut model.playground.sample,
                }),
                _ => None,
            };
            if let Some(input) = input {
                let mut changed = false;
                match key.code {
                    KeyCode::Char(character)
                        if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) =>
                    {
                        input.push(character);
                        changed = true;
                    }
                    KeyCode::Backspace => {
                        changed = input.pop().is_some();
                    }
                    _ => {}
                }
                if playground_input && changed {
                    model.playground_matches.clear();
                    model.playground_error = None;
                }
            }
            (model, None)
        }

        UiMsg::PlaygroundToggleFocus => {
            model.playground.focus = model.playground.focus.toggle();
            (model, None)
        }

        UiMsg::SubmitPlayground => {
            if model.playground.pattern.trim().is_empty()
                || model.playground.sample.trim().is_empty()
            {
                return (model, None);
            }
            let request_id = model.next_request_id();
            model.playground_request_id = Some(request_id);
            model.playground_matches.clear();
            model.playground_error = None;
            let pattern_text = model.playground.pattern.clone();
            let sample_text = model.playground.sample.clone();
            (
                model,
                Some(Command::StartPlayground {
                    request_id,
                    pattern_text,
                    sample_text,
                }),
            )
        }
    }
}

fn start_pattern_scan(mut model: Model) -> (Model, Option<Command>) {
    if model.pattern_draft.trim().is_empty() {
        return (model, None);
    }

    let pattern_text = model.pattern_draft.clone();
    let request_id = model.next_request_id();

    model.tasks.pattern.status = LoadStatus::Loading;
    model.tasks.pattern.request_id = Some(request_id);
    model.tasks.pattern.displayed_request_id = Some(request_id);
    model.tasks.pattern.progress = Default::default();
    model.tasks.surface.displayed_request_id = None;
    model.tasks.surface.page_request_id = None;
    model.tasks.kwic.displayed_request_id = None;
    model.tasks.kwic.page_request_id = None;

    model
        .cache
        .frequencies
        .put(request_id, std::collections::HashMap::new());

    (
        model,
        Some(Command::StartPattern {
            request_id,
            pattern_text,
        }),
    )
}

fn start_surface_if_selected(mut model: Model) -> (Model, Option<Command>) {
    if let Some(pid) = model.selected_pattern
        && let Some(pattern_name) = model.pattern_ids.get(&pid).cloned()
    {
        let pattern_text = model.pattern_draft.clone();
        let request_id = model.next_request_id();

        model.tasks.surface.status = LoadStatus::Loading;
        model.tasks.surface.request_id = Some(request_id);
        model.tasks.surface.displayed_request_id = Some(request_id);
        model.tasks.surface.pattern_id = Some(pid);
        model.tasks.surface.received_lines = 0;
        model.tasks.surface.page_request_id = None;
        model.scroll.surface.offset = 0;

        return (
            model,
            Some(Command::StartSurface {
                request_id,
                pattern_id: pid,
                pattern_name,
                pattern_text,
            }),
        );
    }
    (model, None)
}

fn start_kwic_if_selected(mut model: Model) -> (Model, Option<Command>) {
    if let Some(pid) = model.selected_pattern
        && let Some(pattern_name) = model.pattern_ids.get(&pid).cloned()
    {
        let pattern_text = model.pattern_draft.clone();
        let request_id = model.next_request_id();

        model.tasks.kwic.status = LoadStatus::Loading;
        model.tasks.kwic.request_id = Some(request_id);
        model.tasks.kwic.displayed_request_id = Some(request_id);
        model.tasks.kwic.pattern_id = Some(pid);
        model.tasks.kwic.received_lines = 0;
        model.tasks.kwic.page_request_id = None;
        model.scroll.context.offset = 0;
        model.selected_kwic_line = 0;

        return (
            model,
            Some(Command::StartKwic {
                request_id,
                pattern_id: pid,
                pattern_name,
                pattern_text,
            }),
        );
    }
    (model, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cache::{KwicLine, PagedResult, SurfaceEntry};
    use crate::tui::types::{PatternId, PlaygroundFocus, RequestId};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn make_model() -> Model {
        Model::new()
    }

    fn run_window_command(model: Model, cmd: Option<Command>) -> Model {
        match cmd {
            Some(Command::LoadKwicWindow {
                request_id,
                pattern_id,
                page_request_id,
                store,
                start,
            }) => {
                let result = store
                    .read_window(start as u64, crate::tui::paged_store::DEFAULT_PAGE_ROWS)
                    .map_err(|error| error.to_string());
                let (model, next) = update(
                    ExplorerMessage::KwicWindowLoaded {
                        request_id,
                        pattern_id,
                        page_request_id,
                        window_start: start,
                        result,
                    },
                    model,
                );
                assert!(next.is_none());
                model
            }
            Some(Command::LoadSurfaceWindow {
                request_id,
                pattern_id,
                page_request_id,
                store,
                start,
            }) => {
                let result = store
                    .read_window(start as u64, crate::tui::paged_store::DEFAULT_PAGE_ROWS)
                    .map_err(|error| error.to_string());
                let (model, next) = update(
                    ExplorerMessage::SurfaceWindowLoaded {
                        request_id,
                        pattern_id,
                        page_request_id,
                        window_start: start,
                        result,
                    },
                    model,
                );
                assert!(next.is_none());
                model
            }
            None => model,
            other => panic!("unexpected command: {other:?}"),
        }
    }

    fn paged_kwic_model(rows: usize, selected: usize) -> Model {
        let mut model = Model::new();
        let pattern = model.get_or_create_pattern_id("paged");
        model.selected_pattern = Some(pattern);
        model.mode = AppMode::Viewing { tab: Tab::Context };
        model.selected_kwic_line = selected;
        model.scroll.context.offset = selected;
        model.tasks.kwic.displayed_request_id = Some(RequestId(90));
        model.cache.kwic.put(
            (RequestId(90), pattern),
            PagedResult::from_rows(
                &(0..rows)
                    .map(|index| KwicLine {
                        pre: format!("pre-{index}"),
                        key: format!("key-{index}"),
                        post: format!("post-{index}"),
                        match_start: index,
                        match_end: index + 1,
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        let cmd = model.clamp_scroll_and_plan_window_load();
        run_window_command(model, cmd)
    }

    #[test]
    fn kwic_selection_crosses_windows_and_clamps_to_exact_ends() {
        let model = paged_kwic_model(300, 255);
        let (model, cmd) = update(ExplorerMessage::Ui(UiMsg::ScrollDown), model);
        assert_eq!(model.selected_kwic_line, 256);
        assert_eq!(model.scroll.context.offset, 256);
        assert!(matches!(
            cmd,
            Some(Command::LoadKwicWindow { start: 256, .. })
        ));
        let model = run_window_command(model, cmd);
        let view = crate::tui::model::derive_viewmodel(&model, 30);
        assert_eq!(view.kwic_window_start, 256);
        assert_eq!(view.selected_kwic_idx, Some(0));
        assert_eq!(view.kwic_lines[0].key, "key-256");

        let (model, cmd) = update(ExplorerMessage::Ui(UiMsg::ScrollUp), model);
        assert_eq!(model.selected_kwic_line, 255);
        assert_eq!(model.scroll.context.offset, 255);
        assert!(matches!(
            cmd,
            Some(Command::LoadKwicWindow { start: 255, .. })
        ));
        let model = run_window_command(model, cmd);
        let view = crate::tui::model::derive_viewmodel(&model, 30);
        assert_eq!(view.selected_kwic_idx, Some(0));
        assert_eq!(view.kwic_lines[0].key, "key-255");

        let model = paged_kwic_model(300, 299);
        let (model, cmd) = update(ExplorerMessage::Ui(UiMsg::ScrollDown), model);
        assert!(
            cmd.is_none(),
            "clamped scroll at already-loaded end emits no redundant I/O command"
        );
        assert_eq!(model.selected_kwic_line, 299);
        assert_eq!(model.scroll.context.offset, 299);
        let view = crate::tui::model::derive_viewmodel(&model, 30);
        assert_eq!(view.kwic_lines.len(), 1);
        assert_eq!(view.selected_kwic_idx, Some(0));
        assert_eq!(view.kwic_lines[0].key, "key-299");
    }

    #[test]
    fn stale_window_load_reply_is_ignored_after_newer_scroll() {
        let model = paged_kwic_model(300, 10);
        let (model, cmd1) = update(ExplorerMessage::Ui(UiMsg::ScrollDown), model);
        let Some(Command::LoadKwicWindow {
            request_id,
            pattern_id,
            page_request_id: stale_page_id,
            ..
        }) = cmd1
        else {
            panic!("expected first window command");
        };
        let (model, cmd2) = update(ExplorerMessage::Ui(UiMsg::ScrollDown), model);
        assert!(matches!(
            cmd2,
            Some(Command::LoadKwicWindow { start: 12, .. })
        ));

        // Stale reply from first scroll must not overwrite window or clear pending page id.
        let (model, _) = update(
            ExplorerMessage::KwicWindowLoaded {
                request_id,
                pattern_id,
                page_request_id: stale_page_id,
                window_start: 11,
                result: Ok(vec![KwicLine {
                    pre: "stale".into(),
                    key: "stale".into(),
                    post: "stale".into(),
                    match_start: 0,
                    match_end: 1,
                }]),
            },
            model,
        );
        assert!(model.tasks.kwic.page_request_id.is_some());
        let model = run_window_command(model, cmd2);
        assert!(model.tasks.kwic.page_request_id.is_none());
        let view = crate::tui::model::derive_viewmodel(&model, 30);
        assert_eq!(view.kwic_window_start, 12);
        assert_eq!(view.kwic_lines[0].key, "key-12");
    }

    #[test]
    fn completed_short_details_reset_navigation_from_replaced_long_results() {
        let mut surface = Model::new();
        let pattern = surface.get_or_create_pattern_id("p");
        surface.selected_pattern = Some(pattern);
        surface.mode = AppMode::Viewing { tab: Tab::Surface };
        surface.scroll.surface.offset = 299;
        surface.tasks.surface.request_id = Some(RequestId(101));
        surface.tasks.surface.pattern_id = Some(pattern);
        surface.tasks.surface.status = LoadStatus::Loading;
        let short_surface = PagedResult::from_rows(&[SurfaceEntry {
            form: "short-surface".into(),
            count: 1,
        }]);
        let (surface, _) = update(
            ExplorerMessage::SurfaceFinished {
                request_id: RequestId(101),
                pattern_id: pattern,
                result: short_surface,
            },
            surface,
        );
        assert_eq!(surface.scroll.surface.offset, 0);
        let view = crate::tui::model::derive_viewmodel(&surface, 30);
        assert_eq!(view.surface_entries[0].form, "short-surface");

        let mut kwic = Model::new();
        let pattern = kwic.get_or_create_pattern_id("p");
        kwic.selected_pattern = Some(pattern);
        kwic.mode = AppMode::Viewing { tab: Tab::Context };
        kwic.scroll.context.offset = 299;
        kwic.selected_kwic_line = 299;
        kwic.tasks.kwic.request_id = Some(RequestId(102));
        kwic.tasks.kwic.pattern_id = Some(pattern);
        kwic.tasks.kwic.status = LoadStatus::Loading;
        let short_kwic = PagedResult::from_rows(&[KwicLine {
            pre: "short-pre".into(),
            key: "short-key".into(),
            post: "short-post".into(),
            match_start: 0,
            match_end: 1,
        }]);
        let (kwic, _) = update(
            ExplorerMessage::KwicFinished {
                request_id: RequestId(102),
                pattern_id: pattern,
                result: short_kwic,
            },
            kwic,
        );
        assert_eq!(kwic.scroll.context.offset, 0);
        assert_eq!(kwic.selected_kwic_line, 0);
        let view = crate::tui::model::derive_viewmodel(&kwic, 30);
        assert_eq!(view.selected_kwic_idx, Some(0));
        assert_eq!(view.kwic_lines[0].key, "short-key");
    }

    #[test]
    fn test_tab_change_updates_mode() {
        let model = make_model();
        let (new_model, cmd) = update(ExplorerMessage::Ui(UiMsg::TabChange(Tab::Surface)), model);
        assert!(matches!(
            new_model.mode,
            AppMode::Viewing { tab: Tab::Surface }
        ));
        assert!(cmd.is_none());
    }

    #[test]
    fn test_exit_sets_exiting_mode() {
        let model = make_model();
        let (new_model, _) = update(ExplorerMessage::Ui(UiMsg::Exit), model);
        assert!(matches!(new_model.mode, AppMode::Exiting));
    }

    #[test]
    fn test_stale_pattern_result_is_ignored() {
        let mut model = make_model();
        model.tasks.pattern.request_id = Some(RequestId(2));
        let (new_model, cmd) = update(
            ExplorerMessage::PatternFinished {
                request_id: RequestId(1),
            },
            model,
        );
        assert_eq!(new_model.tasks.pattern.request_id, Some(RequestId(2)));
        assert!(cmd.is_none());
    }

    #[test]
    fn test_cancel_retains_request_id_until_acknowledgement() {
        let mut model = make_model();
        model.tasks.pattern.request_id = Some(RequestId(1));
        model.tasks.pattern.status = LoadStatus::Loading;
        let (new_model, cmd) = update(ExplorerMessage::Ui(UiMsg::Cancel(TaskKind::Pattern)), model);
        assert_eq!(new_model.tasks.pattern.status, LoadStatus::Cancelling);
        assert_eq!(new_model.tasks.pattern.request_id, Some(RequestId(1)));
        assert!(matches!(cmd, Some(Command::Cancel { .. })));
    }

    #[test]
    fn cancelling_tasks_without_active_requests_is_a_no_op() {
        for kind in [TaskKind::Pattern, TaskKind::Surface, TaskKind::Kwic] {
            let mut model = make_model();
            match kind {
                TaskKind::Pattern => model.tasks.pattern.status = LoadStatus::Complete,
                TaskKind::Surface => model.tasks.surface.status = LoadStatus::Complete,
                TaskKind::Kwic => model.tasks.kwic.status = LoadStatus::Complete,
            }
            let expected_pattern = model.tasks.pattern.status;
            let expected_surface = model.tasks.surface.status;
            let expected_kwic = model.tasks.kwic.status;
            let (model, command) = update(ExplorerMessage::Ui(UiMsg::Cancel(kind)), model);

            assert!(command.is_none(), "{kind:?}");
            assert_eq!(model.tasks.pattern.status, expected_pattern, "{kind:?}");
            assert_eq!(model.tasks.surface.status, expected_surface, "{kind:?}");
            assert_eq!(model.tasks.kwic.status, expected_kwic, "{kind:?}");
        }
    }

    #[test]
    fn cancelled_pattern_acknowledgement_returns_to_idle() {
        let mut model = make_model();
        model.tasks.pattern.request_id = Some(RequestId(7));
        model.tasks.pattern.status = LoadStatus::Loading;

        let (model, command) = update(ExplorerMessage::Ui(UiMsg::Cancel(TaskKind::Pattern)), model);
        assert_eq!(model.tasks.pattern.status, LoadStatus::Cancelling);
        assert!(matches!(command, Some(Command::Cancel { .. })));

        let (model, command) = update(
            ExplorerMessage::PatternFailed {
                request_id: RequestId(7),
                code: Some(ErrorCode::Cancelled),
                error: "Processing cancelled".into(),
            },
            model,
        );
        assert!(command.is_none());
        assert_eq!(model.tasks.pattern.status, LoadStatus::Idle);
        assert!(model.tasks.pattern.request_id.is_none());
        assert!(!matches!(model.mode, AppMode::Error { .. }));
    }

    #[test]
    fn cancelled_surface_acknowledgement_returns_to_idle() {
        let mut model = make_model();
        model.tasks.surface.request_id = Some(RequestId(7));
        model.tasks.surface.pattern_id = Some(PatternId(3));
        model.tasks.surface.status = LoadStatus::Loading;

        let (model, command) = update(ExplorerMessage::Ui(UiMsg::Cancel(TaskKind::Surface)), model);
        assert_eq!(model.tasks.surface.status, LoadStatus::Cancelling);
        assert_eq!(model.tasks.surface.request_id, Some(RequestId(7)));
        assert!(matches!(command, Some(Command::Cancel { .. })));

        let (model, command) = update(
            ExplorerMessage::SurfaceFailed {
                request_id: RequestId(7),
                pattern_id: PatternId(99),
                code: Some(ErrorCode::Cancelled),
                error: "Processing cancelled".into(),
            },
            model,
        );
        assert!(command.is_none());
        assert_eq!(model.tasks.surface.status, LoadStatus::Idle);
        assert!(model.tasks.surface.request_id.is_none());
        assert!(model.tasks.surface.pattern_id.is_none());
        assert!(!matches!(model.mode, AppMode::Error { .. }));
    }

    #[test]
    fn cancelled_kwic_acknowledgement_returns_to_idle() {
        let mut model = make_model();
        model.tasks.kwic.request_id = Some(RequestId(7));
        model.tasks.kwic.pattern_id = Some(PatternId(3));
        model.tasks.kwic.status = LoadStatus::Loading;

        let (model, _) = update(ExplorerMessage::Ui(UiMsg::Cancel(TaskKind::Kwic)), model);
        let (model, command) = update(
            ExplorerMessage::KwicFailed {
                request_id: RequestId(7),
                pattern_id: PatternId(99),
                code: Some(ErrorCode::Cancelled),
                error: "Processing cancelled".into(),
            },
            model,
        );
        assert!(command.is_none());
        assert_eq!(model.tasks.kwic.status, LoadStatus::Idle);
        assert!(model.tasks.kwic.request_id.is_none());
        assert!(model.tasks.kwic.pattern_id.is_none());
        assert!(!matches!(model.mode, AppMode::Error { .. }));
    }

    #[test]
    fn current_surface_failure_is_terminal_even_if_pattern_id_differs() {
        let mut model = make_model();
        model.tasks.surface.request_id = Some(RequestId(7));
        model.tasks.surface.pattern_id = Some(PatternId(3));
        model.tasks.surface.status = LoadStatus::Loading;

        let (model, command) = update(
            ExplorerMessage::SurfaceFailed {
                request_id: RequestId(7),
                pattern_id: PatternId(99),
                code: Some(ErrorCode::CacheError),
                error: "cache checksum mismatch".into(),
            },
            model,
        );
        assert!(command.is_none());
        assert_eq!(model.tasks.surface.status, LoadStatus::Failed);
        assert!(model.tasks.surface.request_id.is_none());
        assert_eq!(model.tasks.surface.pattern_id, Some(PatternId(99)));
        assert!(matches!(
            model.mode,
            AppMode::Error { ref message } if message.contains("checksum")
        ));
    }

    #[test]
    fn stale_surface_failure_does_not_mutate_the_active_request() {
        let mut model = make_model();
        model.tasks.surface.request_id = Some(RequestId(8));
        model.tasks.surface.pattern_id = Some(PatternId(3));
        model.tasks.surface.status = LoadStatus::Loading;

        let (model, command) = update(
            ExplorerMessage::SurfaceFailed {
                request_id: RequestId(7),
                pattern_id: PatternId(99),
                code: Some(ErrorCode::CacheError),
                error: "stale failure".into(),
            },
            model,
        );
        assert!(command.is_none());
        assert_eq!(model.tasks.surface.status, LoadStatus::Loading);
        assert_eq!(model.tasks.surface.request_id, Some(RequestId(8)));
        assert_eq!(model.tasks.surface.pattern_id, Some(PatternId(3)));
        assert!(!matches!(model.mode, AppMode::Error { .. }));
    }

    #[test]
    fn current_kwic_failure_is_terminal_and_visible() {
        let mut model = make_model();
        model.tasks.kwic.request_id = Some(RequestId(7));
        model.tasks.kwic.pattern_id = Some(PatternId(3));
        model.tasks.kwic.status = LoadStatus::Loading;

        let (model, command) = update(
            ExplorerMessage::KwicFailed {
                request_id: RequestId(7),
                pattern_id: PatternId(3),
                code: Some(ErrorCode::CacheError),
                error: "cache checksum mismatch".into(),
            },
            model,
        );
        assert!(command.is_none());
        assert_eq!(model.tasks.kwic.status, LoadStatus::Failed);
        assert!(model.tasks.kwic.request_id.is_none());
        assert!(matches!(model.mode, AppMode::Error { .. }));
    }

    #[test]
    fn test_pattern_delta_updates_frequencies() {
        let mut model = make_model();
        let req_id = RequestId(1);
        model.tasks.pattern.request_id = Some(req_id);
        model.tasks.pattern.status = LoadStatus::Loading;

        let delta = vec![("pattern_a".to_string(), 5), ("pattern_b".to_string(), 10)];
        let (mut new_model, _) = update(
            ExplorerMessage::PatternDelta {
                request_id: req_id,
                delta_counts: delta,
            },
            model,
        );

        let freqs = new_model.cache.frequencies.get(&req_id).unwrap();
        let pid_a = *new_model.pattern_names.get("pattern_a").unwrap();
        let pid_b = *new_model.pattern_names.get("pattern_b").unwrap();
        assert_eq!(*freqs.get(&pid_a).unwrap(), 5);
        assert_eq!(*freqs.get(&pid_b).unwrap(), 10);
    }

    #[test]
    fn test_pattern_finished_sets_complete() {
        let mut model = make_model();
        let req_id = RequestId(1);
        model.tasks.pattern.request_id = Some(req_id);
        model.tasks.pattern.status = LoadStatus::Loading;

        let (new_model, _) = update(
            ExplorerMessage::PatternFinished { request_id: req_id },
            model,
        );

        assert_eq!(new_model.tasks.pattern.status, LoadStatus::Complete);
        assert!(new_model.tasks.pattern.request_id.is_none());
    }

    #[test]
    fn test_pattern_failed_sets_error_mode() {
        let mut model = make_model();
        let req_id = RequestId(1);
        model.tasks.pattern.request_id = Some(req_id);
        model.tasks.pattern.status = LoadStatus::Loading;

        let (new_model, _) = update(
            ExplorerMessage::PatternFailed {
                request_id: req_id,
                code: None,
                error: "test error".to_string(),
            },
            model,
        );

        assert_eq!(new_model.tasks.pattern.status, LoadStatus::Failed);
        assert!(matches!(new_model.mode, AppMode::Error { .. }));
        if let AppMode::Error { message } = &new_model.mode {
            assert_eq!(message, "test error");
        }
    }

    #[test]
    fn test_dismiss_error_returns_to_viewing() {
        let mut model = make_model();
        model.mode = AppMode::Error {
            message: "error".to_string(),
        };

        let (new_model, _) = update(ExplorerMessage::Ui(UiMsg::DismissError), model);

        assert!(matches!(
            new_model.mode,
            AppMode::Viewing {
                tab: Tab::Frequencies
            }
        ));
    }

    #[test]
    fn test_submit_empty_pattern_does_nothing() {
        let model = make_model();
        let (new_model, cmd) = update(ExplorerMessage::Ui(UiMsg::SubmitPattern), model);

        assert!(cmd.is_none());
        assert_eq!(new_model.tasks.pattern.status, LoadStatus::Idle);
    }

    #[test]
    fn test_submit_pattern_starts_loading() {
        let mut model = make_model();
        model.pattern_draft = "test pattern".to_string();

        let (new_model, cmd) = update(ExplorerMessage::Ui(UiMsg::SubmitPattern), model);

        assert_eq!(new_model.tasks.pattern.status, LoadStatus::Loading);
        assert!(new_model.tasks.pattern.request_id.is_some());
        assert!(matches!(cmd, Some(Command::StartPattern { .. })));
    }

    #[test]
    fn test_tick_does_nothing() {
        let model = make_model();
        let expected_seq = model.request_seq;
        let (new_model, cmd) = update(ExplorerMessage::Tick, model);

        assert!(cmd.is_none());
        assert_eq!(expected_seq, new_model.request_seq);
    }

    #[test]
    fn test_pattern_progress_updates() {
        let mut model = make_model();
        let req_id = RequestId(1);
        model.tasks.pattern.request_id = Some(req_id);
        model.tasks.pattern.status = LoadStatus::Loading;

        let (model, _) = update(
            ExplorerMessage::PatternProgress {
                request_id: req_id,
                current: 5000,
                total: 10000,
            },
            model,
        );

        assert_eq!(model.tasks.pattern.progress.current, 5000);
        assert_eq!(model.tasks.pattern.progress.total, 10000);
    }

    #[test]
    fn test_pattern_progress_rejected_if_stale() {
        let mut model = make_model();
        model.tasks.pattern.request_id = Some(RequestId(2));
        model.tasks.pattern.progress.current = 100;
        model.tasks.pattern.progress.total = 1000;

        let (model, _) = update(
            ExplorerMessage::PatternProgress {
                request_id: RequestId(1),
                current: 5000,
                total: 10000,
            },
            model,
        );

        assert_eq!(model.tasks.pattern.progress.current, 100);
        assert_eq!(model.tasks.pattern.progress.total, 1000);
    }

    #[test]
    fn test_full_pattern_flow() {
        let mut model = make_model();
        model.pattern_draft = "[upos=VERB].".to_string();

        let (model, cmd) = update(ExplorerMessage::Ui(UiMsg::SubmitPattern), model);
        assert_eq!(model.tasks.pattern.status, LoadStatus::Loading);
        let request_id = model.tasks.pattern.request_id.unwrap();
        assert!(matches!(cmd, Some(Command::StartPattern { .. })));

        let delta = vec![
            ("pattern_1".to_string(), 100),
            ("pattern_2".to_string(), 50),
        ];
        let (model, _) = update(
            ExplorerMessage::PatternDelta {
                request_id,
                delta_counts: delta,
            },
            model,
        );

        assert_eq!(model.pattern_names.len(), 2);
        assert!(model.pattern_names.contains_key("pattern_1"));
        assert!(model.pattern_names.contains_key("pattern_2"));

        let (model, _) = update(ExplorerMessage::PatternFinished { request_id }, model);
        assert_eq!(model.tasks.pattern.status, LoadStatus::Complete);

        assert_eq!(model.pattern_names.len(), 2);
    }

    #[test]
    fn test_get_scroll_mut_returns_correct_scroll() {
        let mut model = make_model();
        model.mode = AppMode::Viewing { tab: Tab::Context };

        let scroll = get_scroll_mut(&mut model);
        assert!(scroll.is_some());
        scroll.unwrap().offset = 5;

        assert_eq!(model.scroll.context.offset, 5);
    }

    #[test]
    fn test_get_scroll_mut_returns_none_for_non_viewing_mode() {
        let mut model = make_model();
        model.mode = AppMode::EditingPattern;

        let scroll = get_scroll_mut(&mut model);
        assert!(scroll.is_none());
    }

    #[test]
    fn test_playground_toggle_focus() {
        let model = Model::default();
        assert_eq!(model.playground.focus, PlaygroundFocus::Pattern);

        let (new_model, _) = update(ExplorerMessage::Ui(UiMsg::PlaygroundToggleFocus), model);
        assert_eq!(new_model.playground.focus, PlaygroundFocus::Sample);
    }

    #[test]
    fn editing_keypresses_mutate_the_pattern_and_escape_returns_to_viewing() {
        let mut model = make_model();
        model.mode = AppMode::EditingPattern;
        model.pattern_draft = "p".into();

        let (model, _) = update(
            ExplorerMessage::Ui(UiMsg::KeyPress(KeyEvent::new(
                KeyCode::Char('猫'),
                KeyModifiers::NONE,
            ))),
            model,
        );
        assert_eq!(model.pattern_draft, "p猫");
        let (model, _) = update(
            ExplorerMessage::Ui(UiMsg::KeyPress(KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::NONE,
            ))),
            model,
        );
        assert_eq!(model.pattern_draft, "p");
        let (model, command) = update(ExplorerMessage::Ui(UiMsg::ConfirmSelection), model);
        assert!(matches!(
            model.mode,
            AppMode::Viewing {
                tab: Tab::Frequencies
            }
        ));
        assert!(command.is_none());
    }

    #[test]
    fn playground_keypresses_edit_the_focused_field() {
        let mut model = make_model();
        model.mode = AppMode::Viewing {
            tab: Tab::Playground,
        };

        let (model, _) = update(
            ExplorerMessage::Ui(UiMsg::KeyPress(KeyEvent::new(
                KeyCode::Char('p'),
                KeyModifiers::NONE,
            ))),
            model,
        );
        assert_eq!(model.playground.pattern, "p");
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::PlaygroundToggleFocus), model);
        let (model, _) = update(
            ExplorerMessage::Ui(UiMsg::KeyPress(KeyEvent::new(
                KeyCode::Char('猫'),
                KeyModifiers::NONE,
            ))),
            model,
        );
        assert_eq!(model.playground.pattern, "p");
        assert_eq!(model.playground.sample, "猫");
    }

    #[test]
    fn playground_submit_starts_direct_sample_execution() {
        let mut model = make_model();
        model.mode = AppMode::Viewing {
            tab: Tab::Playground,
        };
        model.playground.pattern = "p := [text=猫].".into();
        model.playground.sample = "猫。".into();

        let (model, command) = update(ExplorerMessage::Ui(UiMsg::SubmitPlayground), model);

        assert_eq!(model.playground_request_id, Some(RequestId(0)));
        assert!(matches!(
            command,
            Some(Command::StartPlayground {
                request_id: RequestId(0),
                pattern_text,
                sample_text,
            }) if pattern_text == "p := [text=猫]." && sample_text == "猫。"
        ));
    }

    #[test]
    fn mouse_wheel_preserves_scroll_navigation() {
        let model = make_model();
        let event = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::Mouse(event)), model);
        assert_eq!(model.scroll.frequencies.offset, 1);

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            ..event
        };
        let (model, command) = update(ExplorerMessage::Ui(UiMsg::Mouse(click)), model);
        assert_eq!(model.scroll.frequencies.offset, 1);
        assert!(command.is_none());
    }

    #[test]
    fn pattern_file_change_requests_async_reload_without_disk_io_and_reloads_on_reply() {
        let path = std::path::PathBuf::from("/nonexistent/pure-elm/patterns.lx");
        let mut model = make_model();
        model.pattern_draft = "old := [text=犬].".into();

        // Step 1: PatternFileChanged emits a ReloadPatternFile command purely without reading disk.
        let (model, command) = update(
            ExplorerMessage::PatternFileChanged(crate::tui::watcher::PatternFileChanged {
                path: path.clone(),
                reload_count: 1,
            }),
            model,
        );
        assert_eq!(model.pattern_draft, "old := [text=犬].");
        assert!(matches!(
            command,
            Some(Command::ReloadPatternFile {
                path: ref cmd_path,
                reload_count: 1,
            }) if cmd_path == &path
        ));

        // Step 2: PatternFileLoaded(Ok) updates draft and emits StartPattern.
        let (model, command) = update(
            ExplorerMessage::PatternFileLoaded {
                path: path.clone(),
                reload_count: 1,
                result: Ok("new := [text=猫].".into()),
            },
            model,
        );
        assert_eq!(model.pattern_draft, "new := [text=猫].");
        assert!(matches!(
            command,
            Some(Command::StartPattern { pattern_text, .. })
                if pattern_text == "new := [text=猫]."
        ));

        // Step 3: PatternFileLoaded(Err) enters Error mode purely.
        let (model, command) = update(
            ExplorerMessage::PatternFileLoaded {
                path,
                reload_count: 2,
                result: Err("permission denied".into()),
            },
            model,
        );
        assert!(command.is_none());
        assert!(matches!(
            model.mode,
            AppMode::Error { ref message } if message.contains("permission denied")
        ));
    }

    #[test]
    fn stale_request_cache_entries_are_excluded_from_viewmodel_and_navigation() {
        let mut model = make_model();
        model.pattern_draft = "alpha := [text=猫].\nold_only := [text=犬].".into();

        // First pattern catalog run
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::SubmitPattern), model);
        let req0 = model.tasks.pattern.request_id.unwrap();
        let (model, _) = update(
            ExplorerMessage::PatternDelta {
                request_id: req0,
                delta_counts: vec![("alpha".into(), 10), ("old_only".into(), 4)],
            },
            model,
        );
        let (mut model, _) = update(ExplorerMessage::PatternFinished { request_id: req0 }, model);
        let alpha_pid = *model.pattern_names.get("alpha").unwrap();
        model.selected_pattern = Some(alpha_pid);

        // Populate Surface and KWIC for the first catalog run
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::TabChange(Tab::Surface)), model);
        let surface_req0 = model.tasks.surface.request_id.unwrap();
        let old_surface = PagedResult::from_rows(&[SurfaceEntry {
            form: "old-surface".into(),
            count: 10,
        }]);
        let (model, _) = update(
            ExplorerMessage::SurfaceFinished {
                request_id: surface_req0,
                pattern_id: alpha_pid,
                result: old_surface,
            },
            model,
        );

        let (model, _) = update(ExplorerMessage::Ui(UiMsg::TabChange(Tab::Context)), model);
        let kwic_req0 = model.tasks.kwic.request_id.unwrap();
        let old_kwic = PagedResult::from_rows(&[KwicLine {
            pre: "old-pre".into(),
            key: "old-key".into(),
            post: "old-post".into(),
            match_start: 0,
            match_end: 1,
        }]);
        let (mut model, _) = update(
            ExplorerMessage::KwicFinished {
                request_id: kwic_req0,
                pattern_id: alpha_pid,
                result: old_kwic,
            },
            model,
        );

        // Edit and submit a second pattern catalog
        model.mode = AppMode::Viewing {
            tab: Tab::Frequencies,
        };
        model.pattern_draft = "alpha := [text=猫].\nbeta := [text=鳥].".into();
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::SubmitPattern), model);
        let req1 = model.tasks.pattern.request_id.unwrap();
        let (model, _) = update(
            ExplorerMessage::PatternDelta {
                request_id: req1,
                delta_counts: vec![("alpha".into(), 3), ("beta".into(), 2)],
            },
            model,
        );
        let (model, _) = update(ExplorerMessage::PatternFinished { request_id: req1 }, model);

        let vm = crate::tui::model::derive_viewmodel(&model, 80);
        let displayed: Vec<(&str, u64)> = vm
            .patterns
            .iter()
            .map(|(_, name, count)| (*name, *count))
            .collect();
        assert_eq!(displayed, vec![("alpha", 3), ("beta", 2)]);
        assert_eq!(vm.total_matches, 5);

        // Switching to Surface starts a new request; while loading, stale surface must not leak
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::TabChange(Tab::Surface)), model);
        let surface_req1 = model.tasks.surface.request_id.unwrap();
        let loading_vm = crate::tui::model::derive_viewmodel(&model, 80);
        assert!(
            loading_vm.surface_entries.is_empty(),
            "stale surface leaked while loading"
        );
        assert_eq!(loading_vm.surface_total, 0);

        let new_surface = PagedResult::from_rows(&[SurfaceEntry {
            form: "new-surface".into(),
            count: 3,
        }]);
        let (model, _) = update(
            ExplorerMessage::SurfaceFinished {
                request_id: surface_req1,
                pattern_id: alpha_pid,
                result: new_surface,
            },
            model,
        );
        let vm = crate::tui::model::derive_viewmodel(&model, 80);
        assert_eq!(vm.surface_entries.len(), 1);
        assert_eq!(vm.surface_entries[0].form, "new-surface");

        // Switching to Context starts a new request; while loading, stale KWIC must not leak
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::TabChange(Tab::Context)), model);
        let kwic_req1 = model.tasks.kwic.request_id.unwrap();
        let loading_vm = crate::tui::model::derive_viewmodel(&model, 80);
        assert!(
            loading_vm.kwic_lines.is_empty(),
            "stale KWIC leaked while loading"
        );
        assert_eq!(loading_vm.kwic_total, 0);

        let new_kwic = PagedResult::from_rows(&[KwicLine {
            pre: "new-pre".into(),
            key: "new-key".into(),
            post: "new-post".into(),
            match_start: 0,
            match_end: 1,
        }]);
        let (model, _) = update(
            ExplorerMessage::KwicFinished {
                request_id: kwic_req1,
                pattern_id: alpha_pid,
                result: new_kwic,
            },
            model,
        );
        let vm = crate::tui::model::derive_viewmodel(&model, 80);
        assert_eq!(vm.kwic_lines.len(), 1);
        assert_eq!(vm.kwic_lines[0].key, "new-key");
    }

    #[test]
    fn pattern_selection_follows_every_active_sort_mode() {
        let mut model = make_model();
        model.pattern_draft = "alpha := [text=a].\nmiddle := [text=m].\nzebra := [text=z].".into();
        let (model, _) = update(ExplorerMessage::Ui(UiMsg::SubmitPattern), model);
        let req = model.tasks.pattern.request_id.unwrap();
        let (model, _) = update(
            ExplorerMessage::PatternDelta {
                request_id: req,
                delta_counts: vec![
                    ("middle".into(), 20),
                    ("alpha".into(), 10),
                    ("zebra".into(), 30),
                ],
            },
            model,
        );
        let (mut model, _) = update(ExplorerMessage::PatternFinished { request_id: req }, model);

        for _ in 0..4 {
            let mode = model.sort_mode;
            let expected: Vec<PatternId> = crate::tui::model::derive_viewmodel(&model, 80)
                .patterns
                .into_iter()
                .map(|(pid, _, _)| pid)
                .collect();
            assert_eq!(expected.len(), 3, "{mode:?}");

            model.selected_pattern = None;
            let mut visited_forward = Vec::new();
            for _ in 0..3 {
                let (next_model, _) = update(ExplorerMessage::Ui(UiMsg::SelectNextPattern), model);
                model = next_model;
                visited_forward.push(model.selected_pattern.unwrap());
            }
            assert_eq!(visited_forward, expected, "forward order for {mode:?}");

            // Clamped at the end
            let (next_model, _) = update(ExplorerMessage::Ui(UiMsg::SelectNextPattern), model);
            model = next_model;
            assert_eq!(model.selected_pattern, Some(expected[2]), "{mode:?}");

            // Walk backward to the start
            let mut visited_backward = Vec::new();
            for _ in 0..2 {
                let (prev_model, _) = update(ExplorerMessage::Ui(UiMsg::SelectPrevPattern), model);
                model = prev_model;
                visited_backward.push(model.selected_pattern.unwrap());
            }
            assert_eq!(
                visited_backward,
                vec![expected[1], expected[0]],
                "backward order for {mode:?}"
            );

            // Clamped at the start
            let (prev_model, _) = update(ExplorerMessage::Ui(UiMsg::SelectPrevPattern), model);
            model = prev_model;
            assert_eq!(model.selected_pattern, Some(expected[0]), "{mode:?}");

            let (toggled, _) = update(ExplorerMessage::Ui(UiMsg::ToggleSort), model);
            model = toggled;
        }
    }
}
