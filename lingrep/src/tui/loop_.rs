use crate::tui::command::Command;
use crate::tui::message::{ExplorerMessage, UiMsg};
use crate::tui::model::{Model, PlaygroundMatch};
use crate::tui::types::{AppMode, LoadStatus, Tab, TaskKind};
use crate::tui::update::update;
use crate::tui::workers::TaskHandles;
use crossbeam_channel::{Receiver, Sender};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use lingrep_cli_common::sanitize_terminal;
use lingrep_core::execution::CancellationFlag;
use lingrep_core::fsm::CompiledCatalog;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stderr>>,
    mut model: Model,
    execution: Arc<crate::interactive_execution::InteractiveExecution>,
    process_cancellation: &CancellationFlag,
) -> Result<super::ExplorerOutcome, std::io::Error> {
    let (tx, rx): (Sender<ExplorerMessage>, Receiver<ExplorerMessage>) =
        crossbeam_channel::bounded(64);

    let mut handles = TaskHandles::default();
    let tick_rate = Duration::from_millis(50);
    let mut last_tick = Instant::now();

    // Auto-start pattern processing if pattern_draft is non-empty
    let initial_cmd = if model.pattern_draft.trim().is_empty() {
        None
    } else {
        let request_id = model.next_request_id();
        model.tasks.pattern.status = LoadStatus::Loading;
        model.tasks.pattern.request_id = Some(request_id);
        model.cache.frequencies.put(request_id, HashMap::new());
        Some(Command::StartPattern {
            request_id,
            pattern_text: model.pattern_draft.clone(),
        })
    };

    if let Some(cmd) = initial_cmd {
        run_command(cmd, &tx, &mut handles, &execution);
    }

    loop {
        if process_cancellation.is_cancelled() {
            cancel_all(&mut handles);
            return Ok(super::ExplorerOutcome::Cancelled);
        }
        while let Ok(msg) = rx.try_recv() {
            let (new_model, cmd) = update(msg, model);
            model = new_model;

            if let Some(c) = cmd {
                run_command(c, &tx, &mut handles, &execution);
            }

            if matches!(model.mode, AppMode::Exiting) {
                cancel_all(&mut handles);
                return Ok(super::ExplorerOutcome::Completed);
            }
        }

        if event::poll(Duration::from_millis(0))? {
            let ev = event::read()?;
            if let Some(ui_msg) = translate_event(ev, &model) {
                let (new_model, cmd) = update(ExplorerMessage::Ui(ui_msg), model);
                model = new_model;

                if matches!(model.mode, AppMode::Exiting) {
                    cancel_all(&mut handles);
                    return Ok(super::ExplorerOutcome::Completed);
                }

                if let Some(c) = cmd {
                    run_command(c, &tx, &mut handles, &execution);
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if let Some(ref mut watcher) = model.pattern_watcher
                && let Some(change) = watcher.check()
            {
                let (new_model, cmd) = update(ExplorerMessage::PatternFileChanged(change), model);
                model = new_model;
                if let Some(c) = cmd {
                    run_command(c, &tx, &mut handles, &execution);
                }
                last_tick = Instant::now();
                terminal.draw(|f| view(f, &model))?;
                std::thread::sleep(Duration::from_millis(16));
                continue;
            }

            let (new_model, cmd) = update(ExplorerMessage::Tick, model);
            model = new_model;
            if let Some(c) = cmd {
                run_command(c, &tx, &mut handles, &execution);
            }
            last_tick = Instant::now();
        }

        terminal.draw(|f| view(f, &model))?;

        std::thread::sleep(Duration::from_millis(16));
    }
}

fn translate_event(ev: Event, model: &Model) -> Option<UiMsg> {
    match ev {
        Event::Key(key) if matches!(model.mode, AppMode::Error { .. }) => {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc | KeyCode::Enter) => Some(UiMsg::DismissError),
                (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(UiMsg::Exit),
                _ => None,
            }
        }
        Event::Key(key) if matches!(model.mode, AppMode::EditingPattern) => {
            match (key.modifiers, key.code) {
                (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                    Some(UiMsg::Cancel(TaskKind::Pattern))
                }
                (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(UiMsg::Exit),
                (KeyModifiers::ALT | KeyModifiers::NONE, KeyCode::Enter) => {
                    Some(UiMsg::SubmitPattern)
                }
                (KeyModifiers::NONE, KeyCode::Esc | KeyCode::Tab) => Some(UiMsg::ConfirmSelection),
                _ => Some(UiMsg::KeyPress(key)),
            }
        }
        Event::Key(key)
            if matches!(
                model.mode,
                AppMode::Viewing {
                    tab: Tab::Playground
                }
            ) =>
        {
            match (key.modifiers, key.code) {
                (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(UiMsg::Exit),
                (KeyModifiers::NONE, KeyCode::Tab) => Some(UiMsg::PlaygroundToggleFocus),
                (KeyModifiers::SHIFT, KeyCode::BackTab) => Some(UiMsg::PlaygroundToggleFocus),
                (KeyModifiers::NONE, KeyCode::Enter) => Some(UiMsg::SubmitPlayground),
                (KeyModifiers::NONE, KeyCode::Esc) => Some(UiMsg::TabChange(Tab::Frequencies)),
                _ => Some(UiMsg::KeyPress(key)),
            }
        }
        Event::Key(key) => match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('q')) => Some(UiMsg::Exit),
            (KeyModifiers::NONE, KeyCode::Tab) => {
                if let AppMode::Viewing { tab } = &model.mode {
                    Some(UiMsg::TabChange(tab.next()))
                } else {
                    None
                }
            }
            (KeyModifiers::SHIFT, KeyCode::BackTab) => {
                if let AppMode::Viewing { tab } = &model.mode {
                    Some(UiMsg::TabChange(tab.prev()))
                } else {
                    None
                }
            }
            (KeyModifiers::ALT, KeyCode::Enter) => Some(UiMsg::SubmitPattern),
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if matches!(model.mode, AppMode::EditingPattern) {
                    Some(UiMsg::SubmitPattern)
                } else {
                    Some(UiMsg::ConfirmSelection)
                }
            }
            (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                let kind = match model.mode {
                    AppMode::Viewing { tab: Tab::Surface } => TaskKind::Surface,
                    AppMode::Viewing { tab: Tab::Context } => TaskKind::Kwic,
                    _ => TaskKind::Pattern,
                };
                Some(UiMsg::Cancel(kind))
            }
            (KeyModifiers::NONE, KeyCode::Up) => Some(UiMsg::ScrollUp),
            (KeyModifiers::NONE, KeyCode::Down) => Some(UiMsg::ScrollDown),
            (KeyModifiers::NONE, KeyCode::PageUp) => Some(UiMsg::PageUp),
            (KeyModifiers::NONE, KeyCode::PageDown) => Some(UiMsg::PageDown),
            (KeyModifiers::NONE, KeyCode::Char('j')) => Some(UiMsg::SelectNextPattern),
            (KeyModifiers::NONE, KeyCode::Char('k')) => Some(UiMsg::SelectPrevPattern),
            (KeyModifiers::NONE, KeyCode::Char('s')) => Some(UiMsg::ToggleSort),
            (KeyModifiers::NONE, KeyCode::Char('d')) => Some(UiMsg::ToggleDetailPanel),
            (KeyModifiers::NONE, KeyCode::Char('e')) => {
                if matches!(model.mode, AppMode::Viewing { .. }) {
                    Some(UiMsg::StartEditPattern)
                } else {
                    None
                }
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                if matches!(model.mode, AppMode::EditingPattern) {
                    Some(UiMsg::ConfirmSelection) // Exit edit mode
                } else {
                    None
                }
            }
            _ => Some(UiMsg::KeyPress(key)),
        },
        Event::Mouse(mouse) => Some(UiMsg::Mouse(mouse)),
        Event::Resize(_, _) => None,
        _ => None,
    }
}

fn run_command(
    cmd: Command,
    tx: &Sender<ExplorerMessage>,
    handles: &mut TaskHandles,
    execution: &Arc<crate::interactive_execution::InteractiveExecution>,
) {
    match cmd {
        Command::StartPattern {
            request_id,
            pattern_text,
        } => {
            if let Some(h) = handles.pattern.take() {
                h.cancel.cancel();
            }

            let handle = crate::tui::workers::PatternWorker::spawn(
                request_id,
                pattern_text,
                execution.clone(),
                tx.clone(),
            );

            handles.pattern = Some(handle);
        }

        Command::Cancel { kind, request_id } => {
            let handle = match kind {
                TaskKind::Pattern => &mut handles.pattern,
                TaskKind::Surface => &mut handles.surface,
                TaskKind::Kwic => &mut handles.kwic,
            };

            if let Some(h) = handle.take()
                && h.request_id == request_id
            {
                h.cancel.cancel();
            }
        }

        Command::StartSurface {
            request_id,
            pattern_id,
            pattern_name,
            pattern_text,
        } => {
            if let Some(h) = handles.surface.take() {
                h.cancel.cancel();
            }

            let matcher = match compile_pattern(&pattern_text) {
                Ok(matcher) => Arc::new(matcher),
                Err(error) => {
                    let _ = tx.send(ExplorerMessage::SurfaceFailed {
                        request_id,
                        pattern_id,
                        code: Some(crate::tui::types::ErrorCode::PatternError),
                        error,
                    });
                    return;
                }
            };

            let handle = crate::tui::workers::SurfaceWorker::spawn(
                request_id,
                pattern_id,
                pattern_name,
                matcher,
                execution.clone(),
                tx.clone(),
            );

            handles.surface = Some(handle);
        }

        Command::StartKwic {
            request_id,
            pattern_id,
            pattern_name,
            pattern_text,
        } => {
            if let Some(h) = handles.kwic.take() {
                h.cancel.cancel();
            }

            let matcher = match compile_pattern(&pattern_text) {
                Ok(matcher) => Arc::new(matcher),
                Err(error) => {
                    let _ = tx.send(ExplorerMessage::KwicFailed {
                        request_id,
                        pattern_id,
                        code: Some(crate::tui::types::ErrorCode::PatternError),
                        error,
                    });
                    return;
                }
            };

            let handle = crate::tui::workers::KwicWorker::spawn(
                request_id,
                pattern_id,
                pattern_name,
                matcher,
                execution.clone(),
                tx.clone(),
            );

            handles.kwic = Some(handle);
        }
        Command::StartPlayground {
            request_id,
            pattern_text,
            sample_text,
        } => {
            let message = match execute_playground(execution, &pattern_text, &sample_text) {
                Ok(matches) => ExplorerMessage::PlaygroundComplete {
                    request_id,
                    matches,
                },
                Err(error) => ExplorerMessage::PlaygroundFailed { request_id, error },
            };
            let _ = tx.send(message);
        }
    }
}

fn execute_playground(
    execution: &crate::interactive_execution::InteractiveExecution,
    pattern_text: &str,
    sample_text: &str,
) -> Result<Vec<PlaygroundMatch>, String> {
    let matcher = compile_pattern(pattern_text)?;
    let tokens = execution
        .tokenize_sample(sample_text)
        .map_err(|error| error.to_string())?;
    // The playground previews what the pattern being edited would publish, so
    // it shows published records — otherwise a catalog whose policy references
    // a fragment previews synthesized `__policy__` names the researcher never
    // wrote.
    //
    // A failure reaches the researcher as `PlaygroundFailed`, the same channel
    // as a compile error, rather than as an empty preview: this function's
    // whole contract is that an empty result means "matched nothing".
    Ok(matcher
        .find_published_matches(&tokens)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|matched| PlaygroundMatch {
            start_token: matched.start,
            end_token: matched.end,
            matched_text: tokens
                .get(matched.start..matched.end)
                .unwrap_or_default()
                .iter()
                .filter_map(|token| token.orth())
                .collect(),
            pattern_name: Some(matched.pattern.to_string()),
        })
        .collect())
}

fn cancel_all(handles: &mut TaskHandles) {
    if let Some(h) = handles.pattern.take() {
        h.cancel.cancel();
    }
    if let Some(h) = handles.surface.take() {
        h.cancel.cancel();
    }
    if let Some(h) = handles.kwic.take() {
        h.cancel.cancel();
    }
}

fn compile_pattern(pattern_text: &str) -> Result<CompiledCatalog, String> {
    crate::tui::compile_catalog_source("tui.lx", pattern_text)
}

// ============================================================================
// View functions using ratatui widgets
// ============================================================================

fn view(f: &mut Frame, model: &Model) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(area);

    // Header
    let header_text = get_header_text(model);
    let header = Paragraph::new(header_text).style(Style::default().add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    // Main content
    match &model.mode {
        AppMode::Viewing { tab } => match tab {
            Tab::Frequencies => view_frequencies_widget(f, model, chunks[1]),
            Tab::Surface => view_surface_widget(f, model, chunks[1]),
            Tab::Context => view_context_widget(f, model, chunks[1]),
            Tab::Parsed => view_parsed_widget(f, model, chunks[1]),
            Tab::Playground => view_playground_widget(f, model, chunks[1]),
        },
        AppMode::EditingPattern => {
            let para = Paragraph::new(format!("Edit: {}", sanitize_terminal(&model.pattern_draft)))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Pattern Editor"),
                );
            f.render_widget(para, chunks[1]);
        }
        AppMode::Error { message } => {
            let para = Paragraph::new(sanitize_terminal(message))
                .style(Style::default().fg(Color::Red))
                .block(Block::default().borders(Borders::ALL).title("Error"));
            f.render_widget(para, chunks[1]);
        }
        AppMode::Exiting => {}
    }

    // Footer with key hints
    let hints = get_key_hints(model);
    let footer = Paragraph::new(hints).style(Style::default().fg(Color::DarkGray));
    f.render_widget(footer, chunks[2]);
}

fn get_header_text(model: &Model) -> String {
    let status = match &model.tasks.pattern.status {
        LoadStatus::Idle => "Idle".to_string(),
        LoadStatus::Loading => {
            let p = &model.tasks.pattern.progress;
            if p.total > 0 {
                format!(
                    "Loading {:.0}%",
                    (p.current as f64 / p.total as f64) * 100.0
                )
            } else {
                "Loading...".to_string()
            }
        }
        LoadStatus::Complete => {
            let base = format!("{} patterns loaded", model.pattern_names.len());
            if let Some(ref reload) = model.reload_status {
                format!("{} | {}", base, sanitize_terminal(reload))
            } else {
                base
            }
        }
        LoadStatus::Failed => "Failed".to_string(),
        LoadStatus::Cancelling => "Cancelling".to_string(),
    };

    let selected = model
        .selected_pattern
        .and_then(|pid| model.pattern_ids.get(&pid))
        .map_or_else(|| "-".into(), |name| sanitize_terminal(name));

    let tab_name = match &model.mode {
        AppMode::Viewing { tab } => format!("{tab:?}"),
        _ => "Frequencies".to_string(),
    };

    format!(" {tab_name} | {status} | Selected: {selected} ")
}

fn get_key_hints(model: &Model) -> String {
    match &model.mode {
        AppMode::Viewing { tab } => match tab {
            Tab::Frequencies => {
                " j/k: select | Enter: view | s: sort | e: edit | Tab: next | q: quit ".to_string()
            }
            Tab::Surface => {
                " Tab/Shift+Tab: switch | Up/Down: scroll | d: detail | q: quit ".to_string()
            }
            Tab::Context => {
                " Tab/Shift+Tab: switch | Up/Down: scroll | d: detail | q: quit ".to_string()
            }
            Tab::Parsed => " Tab/Shift+Tab: switch | e: edit | q: quit ".to_string(),
            Tab::Playground => {
                " Type: edit focused field | Tab: switch | Enter: run | Esc: back | Ctrl-q: quit "
                    .to_string()
            }
        },
        AppMode::EditingPattern => " Enter: submit | Esc: cancel | Tab: view ".to_string(),
        AppMode::Error { .. } => " Enter/Esc: dismiss | Ctrl-q: quit ".to_string(),
        AppMode::Exiting => String::new(),
    }
}
fn view_frequencies_widget(f: &mut Frame, model: &Model, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let vm = crate::tui::model::derive_viewmodel(model, chunks[0].height);

    let scroll = model.scroll.frequencies.offset;
    let viewport = (chunks[0].height as usize).saturating_sub(3);
    let start = scroll.min(vm.patterns.len().saturating_sub(1));
    let end = (start + viewport).min(vm.patterns.len());

    let rows: Vec<Row> = vm.patterns[start..end]
        .iter()
        .map(|(pid, name, count)| {
            let selected = Some(*pid) == model.selected_pattern;
            let style = if selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(sanitize_terminal(name)),
                Cell::from(count.to_string()),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Percentage(70), Constraint::Percentage(30)],
    )
    .header(
        Row::new(vec!["Pattern", "Count"])
            .style(Style::default().add_modifier(Modifier::BOLD))
            .bottom_margin(1),
    )
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Patterns ({}) - Total: {} | Sort: {}",
        vm.patterns.len(),
        vm.total_matches,
        model.sort_mode.label()
    )))
    .highlight_symbol("▶ ");

    f.render_widget(table, chunks[0]);

    let details_text = if let Some(pid) = model.selected_pattern {
        if let Some(name) = model.pattern_ids.get(&pid) {
            let count = vm
                .patterns
                .iter()
                .find(|(p, _, _)| *p == pid)
                .map_or(0, |(_, _, c)| *c);
            format!(
                "Pattern: {}\n\nMatch count: {}\n\nPress Enter to view details\n( surface forms & KWIC )",
                sanitize_terminal(name),
                count
            )
        } else {
            "No pattern selected".to_string()
        }
    } else {
        "No pattern selected\n\nUse j/k to select".to_string()
    };

    let details =
        Paragraph::new(details_text).block(Block::default().borders(Borders::ALL).title("Details"));
    f.render_widget(details, chunks[1]);
}

fn view_surface_widget(f: &mut Frame, model: &Model, area: Rect) {
    let vm = crate::tui::model::derive_viewmodel(model, area.height);

    if let Some(pid) = model.selected_pattern
        && let Some(pattern_name) = model.pattern_ids.get(&pid)
    {
        let status = match &model.tasks.surface.status {
            LoadStatus::Idle => "Waiting...",
            LoadStatus::Loading => "Loading surface forms...",
            LoadStatus::Complete => &format!("{} surface forms", vm.surface_total) as &str,
            LoadStatus::Failed => "Failed to load",
            LoadStatus::Cancelling => "Cancelling...",
        };

        if vm.surface_entries.is_empty()
            && matches!(model.tasks.surface.status, LoadStatus::Complete)
        {
            let para = Paragraph::new("No surface forms found").block(
                Block::default().borders(Borders::ALL).title(format!(
                    "Surface: {} | {}",
                    sanitize_terminal(pattern_name),
                    status
                )),
            );
            f.render_widget(para, area);
            return;
        }

        let scroll = model.scroll.surface.offset;
        let viewport = (area.height as usize).saturating_sub(3);
        let start = scroll
            .saturating_sub(vm.surface_window_start)
            .min(vm.surface_entries.len().saturating_sub(1));
        let end = (start + viewport).min(vm.surface_entries.len());

        let rows: Vec<Row> = vm.surface_entries[start..end]
            .iter()
            .map(|entry| {
                Row::new(vec![
                    Cell::from(sanitize_terminal(&entry.form)),
                    Cell::from(entry.count.to_string()),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [Constraint::Percentage(70), Constraint::Percentage(30)],
        )
        .header(
            Row::new(vec!["Surface Form", "Count"])
                .style(Style::default().add_modifier(Modifier::BOLD))
                .bottom_margin(1),
        )
        .block(Block::default().borders(Borders::ALL).title(format!(
            "Surface: {} | {}",
            sanitize_terminal(pattern_name),
            status
        )));

        f.render_widget(table, area);
        return;
    }

    let para = Paragraph::new("No pattern selected\n\nGo to Frequencies tab and select a pattern")
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Surface Forms"),
        );
    f.render_widget(para, area);
}

fn view_context_widget(f: &mut Frame, model: &Model, area: Rect) {
    use ratatui::text::Span;

    let vm = crate::tui::model::derive_viewmodel(model, area.height);

    if let Some(pid) = model.selected_pattern
        && let Some(pattern_name) = model.pattern_ids.get(&pid)
    {
        let status = match &model.tasks.kwic.status {
            LoadStatus::Idle => "Waiting...",
            LoadStatus::Loading => {
                &format!("Loading... ({} lines)", model.tasks.kwic.received_lines) as &str
            }
            LoadStatus::Complete => &format!("{} lines", vm.kwic_total) as &str,
            LoadStatus::Failed => "Failed to load",
            LoadStatus::Cancelling => "Cancelling...",
        };

        if vm.kwic_lines.is_empty() && matches!(model.tasks.kwic.status, LoadStatus::Complete) {
            let para = Paragraph::new("No KWIC lines found").block(
                Block::default().borders(Borders::ALL).title(format!(
                    "KWIC: {} | {}",
                    sanitize_terminal(pattern_name),
                    status
                )),
            );
            f.render_widget(para, area);
            return;
        }

        let (kwic_area, detail_area) = if model.show_detail_panel {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(area);
            (chunks[0], Some(chunks[1]))
        } else {
            (area, None)
        };

        let scroll = model.scroll.context.offset;
        let viewport = (kwic_area.height as usize).saturating_sub(3);
        let start = scroll
            .saturating_sub(vm.kwic_window_start)
            .min(vm.kwic_lines.len().saturating_sub(1));
        let end = (start + viewport).min(vm.kwic_lines.len());

        let rows: Vec<Row> = vm.kwic_lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, kwic)| {
                let is_selected = Some(start + i) == vm.selected_kwic_idx;
                let base_style = if is_selected {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };

                Row::new(vec![
                    Cell::new(Span::styled(
                        format!(
                            "{:>width$}",
                            sanitize_terminal(&kwic.pre),
                            width = vm.kwic_before_width as usize
                        ),
                        base_style.fg(Color::DarkGray),
                    )),
                    Cell::new(Span::styled(
                        sanitize_terminal(&kwic.key),
                        base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )),
                    Cell::new(Span::styled(
                        sanitize_terminal(&kwic.post),
                        base_style.fg(Color::DarkGray),
                    )),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(vm.kwic_before_width),
                Constraint::Length(vm.kwic_key_width),
                Constraint::Length(vm.kwic_after_width),
            ],
        )
        .header(
            Row::new(vec![
                Cell::from(Span::styled(
                    format!(
                        "{:>width$}",
                        "Before",
                        width = vm.kwic_before_width as usize
                    ),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(
                    "Match",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(
                    "After",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
            ])
            .bottom_margin(1),
        )
        .block(Block::default().borders(Borders::ALL).title(format!(
            "KWIC: {} | {}",
            sanitize_terminal(pattern_name),
            status
        )));

        f.render_widget(table, kwic_area);

        if let Some(detail_area) = detail_area {
            if let Some(selected_kwic) = vm
                .selected_kwic_idx
                .and_then(|index| vm.kwic_lines.get(index))
            {
                let detail_text = format!(
                    "Match Details\n\n\
                        Full text:\n{} {} {}\n\n\
                        Key: {}",
                    sanitize_terminal(&selected_kwic.pre),
                    sanitize_terminal(&selected_kwic.key),
                    sanitize_terminal(&selected_kwic.post),
                    sanitize_terminal(&selected_kwic.key),
                );

                let detail = Paragraph::new(detail_text).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Morphology (d: toggle)"),
                );
                f.render_widget(detail, detail_area);
            } else {
                let detail = Paragraph::new("No line selected").block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Morphology (d: toggle)"),
                );
                f.render_widget(detail, detail_area);
            }
        }

        return;
    }

    let para = Paragraph::new("No pattern selected\n\nGo to Frequencies tab and select a pattern")
        .block(Block::default().borders(Borders::ALL).title("KWIC"));
    f.render_widget(para, area);
}

fn view_parsed_widget(f: &mut Frame, model: &Model, area: Rect) {
    let text = if model.pattern_draft.is_empty() {
        "No pattern loaded".into()
    } else {
        sanitize_terminal(&model.pattern_draft)
    };

    let para = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Pattern Definition"),
    );
    f.render_widget(para, area);
}

fn view_playground_widget(f: &mut Frame, model: &Model, area: Rect) {
    let (pattern_focus, sample_focus) = match model.playground.focus {
        crate::tui::types::PlaygroundFocus::Pattern => (">", " "),
        crate::tui::types::PlaygroundFocus::Sample => (" ", ">"),
    };
    let result = if let Some(error) = &model.playground_error {
        format!("Error: {}", sanitize_terminal(error))
    } else if model.playground_matches.is_empty() {
        "No matches yet".to_string()
    } else {
        model
            .playground_matches
            .iter()
            .map(|matched| {
                format!(
                    "{} [{}..{}] {}",
                    sanitize_terminal(matched.pattern_name.as_deref().unwrap_or("<anonymous>")),
                    matched.start_token,
                    matched.end_token,
                    sanitize_terminal(&matched.matched_text)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let para = Paragraph::new(format!(
        "Pattern {pattern_focus} {}\n\nSample  {sample_focus} {}\n\n\
         Results:\n{}\n\n\
         Enter executes samples in direct mode. Prepared artifacts do not retain a tokenizer and \
         report that mode-specific limitation.",
        sanitize_terminal(&model.playground.pattern),
        sanitize_terminal(&model.playground.sample),
        result,
    ))
    .block(Block::default().borders(Borders::ALL).title("Playground"));
    f.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::cache::{KwicLine, PagedResult, SurfaceEntry};
    use crate::tui::command::Command;
    use crate::tui::types::{PatternId, RequestId};
    use crate::tui::workers::TaskHandles;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;

    #[test]
    fn input_modes_route_printable_keys_to_the_editor() {
        let mut model = Model {
            mode: AppMode::EditingPattern,
            ..Default::default()
        };
        assert!(matches!(
            translate_event(
                Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
                &model,
            ),
            Some(UiMsg::KeyPress(_))
        ));

        model.mode = AppMode::Viewing {
            tab: Tab::Playground,
        };
        assert!(matches!(
            translate_event(
                Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
                &model,
            ),
            Some(UiMsg::KeyPress(_))
        ));
    }

    #[test]
    fn errors_can_be_dismissed_or_exit() {
        let model = Model {
            mode: AppMode::Error {
                message: "failed".into(),
            },
            ..Default::default()
        };

        assert!(matches!(
            translate_event(
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                &model,
            ),
            Some(UiMsg::DismissError)
        ));
        assert!(matches!(
            translate_event(
                Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
                &model,
            ),
            Some(UiMsg::Exit)
        ));
        assert_eq!(get_key_hints(&model), " Enter/Esc: dismiss | Ctrl-q: quit ");
    }

    #[test]
    fn control_c_cancels_the_visible_task() {
        for (tab, expected) in [
            (Tab::Frequencies, TaskKind::Pattern),
            (Tab::Surface, TaskKind::Surface),
            (Tab::Context, TaskKind::Kwic),
        ] {
            let model = Model {
                mode: AppMode::Viewing { tab },
                ..Default::default()
            };
            let message = translate_event(
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                &model,
            );
            assert!(matches!(message, Some(UiMsg::Cancel(kind)) if kind == expected));
        }
    }

    #[test]
    fn playground_render_shows_sanitized_editable_fields() {
        let hostile = "p\u{001b}[2J";
        let mut model = Model {
            mode: AppMode::Viewing {
                tab: Tab::Playground,
            },
            ..Default::default()
        };
        model.playground.pattern = hostile.into();
        model.playground.sample = "猫".into();

        let screen = rendered_screen(&model);

        assert!(screen.contains("Pattern > p\\u{001b}[2J"), "{screen}");
        assert!(screen.contains("Sample    猫"), "{screen}");
        assert!(
            screen.contains("Prepared artifacts do not retain"),
            "{screen}"
        );
        assert!(!screen.contains('\u{001b}'));
    }

    #[test]
    fn direct_playground_executes_with_the_retained_tokenizer() {
        let (_directory, execution) = crate::interactive_execution::direct_test_execution("猫。\n");

        let matches = execute_playground(&execution, "p := [text=猫].", "猫。").unwrap();

        assert_eq!(
            matches,
            [PlaygroundMatch {
                start_token: 0,
                end_token: 1,
                matched_text: "猫".into(),
                pattern_name: Some("p".into()),
            }]
        );
    }

    #[test]
    fn cross_page_kwic_highlight_and_detail_follow_the_absolute_selection() {
        let mut model = Model::default();
        let request = RequestId(71);
        let pattern = model.get_or_create_pattern_id("paged");
        model.selected_pattern = Some(pattern);
        model.mode = AppMode::Viewing { tab: Tab::Context };
        model.show_detail_panel = true;
        model.tasks.kwic.status = LoadStatus::Complete;
        model.selected_kwic_line = 256;
        model.scroll.context.offset = 256;
        model.cache.kwic.put(
            (request, pattern),
            PagedResult::from_rows(
                &(0..300)
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
        model.ensure_visible_window();

        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view(frame, &model)).unwrap();
        let buffer = terminal.backend().buffer();
        let screen = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        assert!(screen.contains("key-256"), "{screen}");
        assert!(screen.contains("Key: key-256"), "{screen}");
        assert!(!screen.contains("Key: key-255"), "{screen}");
        assert!(
            buffer.content.iter().any(|cell| cell.bg == Color::DarkGray),
            "selected absolute row must remain visibly highlighted"
        );
    }

    #[test]
    fn replaced_short_kwic_result_is_immediately_visible_selected_and_detailed() {
        let mut model = Model::default();
        let request = RequestId(72);
        let pattern = model.get_or_create_pattern_id("replaced");
        model.selected_pattern = Some(pattern);
        model.mode = AppMode::Viewing { tab: Tab::Context };
        model.show_detail_panel = true;
        model.scroll.context.offset = 299;
        model.selected_kwic_line = 299;
        model.tasks.kwic.status = LoadStatus::Loading;
        model.tasks.kwic.request_id = Some(request);
        model.tasks.kwic.pattern_id = Some(pattern);
        let result = PagedResult::from_rows(&[KwicLine {
            pre: "new-pre".into(),
            key: "new-key".into(),
            post: "new-post".into(),
            match_start: 0,
            match_end: 1,
        }]);
        let (model, _) = crate::tui::update::update(
            ExplorerMessage::KwicFinished {
                request_id: request,
                pattern_id: pattern,
                store: result.store.clone(),
            },
            model,
        );

        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view(frame, &model)).unwrap();
        let buffer = terminal.backend().buffer();
        let screen = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(screen.contains("new-key"), "{screen}");
        assert!(screen.contains("Key: new-key"), "{screen}");
        assert!(
            buffer.content.iter().any(|cell| cell.bg == Color::DarkGray),
            "replacement row must be highlighted without another input event"
        );
    }

    fn rendered_screen(model: &Model) -> String {
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view(frame, model)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn old_tui_renders_terminal_controls_as_visible_escapes() {
        let hostile = "source\u{001b}[2J\u{0085}\u{202e}";
        let mut model = Model::default();
        let request_id = RequestId(7);
        let pattern_id = PatternId(3);
        model.pattern_names.insert(hostile.into(), pattern_id);
        model.pattern_ids.insert(pattern_id, hostile.into());
        model.selected_pattern = Some(pattern_id);
        model.reload_status = Some(hostile.into());
        model
            .cache
            .frequencies
            .put(request_id, [(pattern_id, 1)].into());
        model.cache.surface.put(
            (request_id, pattern_id),
            PagedResult::from_rows(&[SurfaceEntry {
                form: hostile.into(),
                count: 1,
            }]),
        );
        model.cache.kwic.put(
            (request_id, pattern_id),
            PagedResult::from_rows(&[KwicLine {
                pre: hostile.into(),
                key: hostile.into(),
                post: hostile.into(),
                match_start: 0,
                match_end: 1,
            }]),
        );
        model.tasks.pattern.status = LoadStatus::Complete;
        model.tasks.surface.status = LoadStatus::Complete;
        model.tasks.kwic.status = LoadStatus::Complete;

        let mut snapshots = String::new();
        for tab in [Tab::Frequencies, Tab::Surface, Tab::Context, Tab::Parsed] {
            model.mode = AppMode::Viewing { tab };
            model.pattern_draft = hostile.into();
            snapshots.push_str(&rendered_screen(&model));
        }
        model.mode = AppMode::Error {
            message: hostile.into(),
        };
        snapshots.push_str(&rendered_screen(&model));

        assert!(snapshots.contains("source\\u{001b}[2J\\u{0085}\\u{202e}"));
        assert!(!snapshots.contains('\u{001b}'));
        assert!(!snapshots.contains('\u{0085}'));
        assert!(!snapshots.contains('\u{202e}'));
    }

    #[test]
    fn invalid_detail_drafts_emit_one_corresponding_failure_terminal() {
        let (_directory, execution) = crate::interactive_execution::direct_test_execution("猫。\n");
        for (command, is_expected) in [
            (
                Command::StartSurface {
                    request_id: RequestId(41),
                    pattern_id: PatternId(7),
                    pattern_name: "p".into(),
                    pattern_text: "not valid pattern".into(),
                },
                true,
            ),
            (
                Command::StartKwic {
                    request_id: RequestId(42),
                    pattern_id: PatternId(8),
                    pattern_name: "p".into(),
                    pattern_text: "not valid pattern".into(),
                },
                false,
            ),
        ] {
            let (tx, rx) = crossbeam_channel::unbounded();
            let mut handles = TaskHandles::default();
            run_command(command, &tx, &mut handles, &execution);
            let messages = rx.try_iter().collect::<Vec<_>>();
            assert_eq!(messages.len(), 1, "{messages:?}");
            if is_expected {
                assert!(matches!(
                    messages[0],
                    ExplorerMessage::SurfaceFailed {
                        request_id: RequestId(41),
                        pattern_id: PatternId(7),
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    messages[0],
                    ExplorerMessage::KwicFailed {
                        request_id: RequestId(42),
                        pattern_id: PatternId(8),
                        ..
                    }
                ));
            }
            assert!(handles.surface.is_none());
            assert!(handles.kwic.is_none());
        }
    }
}
