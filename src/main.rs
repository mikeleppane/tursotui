mod app;
mod components;
mod db;
mod event;
mod highlight;
mod theme;

use std::time::Duration;

use clap::Parser;
use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Tabs};

use app::{AppState, BottomTab, DatabaseContext, PanelId, SubTab};
use components::Component;
use components::db_info::DbInfoPanel;
use components::editor::QueryEditor;
use components::explain::ExplainView;
use components::pragmas::PragmaDashboard;
use components::record::RecordDetail;
use components::results::ResultsTable;
use components::schema::SchemaExplorer;
use db::DatabaseHandle;
use theme::Theme;

/// Terminal UI for Turso and `SQLite` databases.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to SQLite/Turso database file(s). Defaults to :memory:
    #[arg(default_value = ":memory:")]
    database: Vec<String>,
}

/// UI panels for the application.
/// Grouped to reduce parameter counts in render functions.
/// Will move into `DatabaseContext` when multi-database support lands (Milestone 7).
struct UiPanels {
    schema: SchemaExplorer,
    editor: QueryEditor,
    results: ResultsTable,
    explain: ExplainView,
    record_detail: RecordDetail,
    db_info: DbInfoPanel,
    pragmas: PragmaDashboard,
}

impl UiPanels {
    fn new() -> Self {
        Self {
            schema: SchemaExplorer::new(),
            editor: QueryEditor::new(),
            results: ResultsTable::new(),
            explain: ExplainView::new(),
            record_detail: RecordDetail::new(),
            db_info: DbInfoPanel::new(),
            pragmas: PragmaDashboard::new(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Open the first database (multi-db support in Milestone 7)
    let path = cli.database.first().map_or(":memory:", String::as_str);
    let handle = DatabaseHandle::open(path)
        .await
        .map_err(|e| format!("failed to open '{path}': {e}"))?;
    let db_context = DatabaseContext::new(handle, path.to_string());
    let mut app = AppState::new(db_context);

    // Trigger schema load on startup
    app.active_db_mut().handle.load_schema();

    // Install panic hook to restore terminal before printing the panic message
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        prev_hook(info);
    }));

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app);
    ratatui::restore();

    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut panels = UiPanels::new();

    loop {
        // 1. Drain async result channel before key handling
        drain_async_messages(app, &mut panels);

        // 2. Poll events (16ms ~ 60fps)
        if let Some(Event::Key(key)) = event::poll_event(Duration::from_millis(16))?
            && key.kind == KeyEventKind::Press
        {
            handle_key_event(key, app, &mut panels);
        }

        // 3. Clear expired transient message
        if let Some(ref tm) = app.transient_message
            && tm.created_at.elapsed() >= components::status_bar::TRANSIENT_TTL
        {
            app.transient_message = None;
        }

        // 4. Render
        if app.should_quit {
            break;
        }

        terminal.draw(|frame| {
            render_ui(frame, app, &mut panels);
        })?;
    }

    Ok(())
}

/// Drain all pending async messages and dispatch the resulting actions.
fn drain_async_messages(app: &mut AppState, panels: &mut UiPanels) {
    while let Some(msg) = app.active_db_mut().handle.try_recv() {
        let action = map_query_message(msg);
        app.update(&action);
        dispatch_action_to_components(&action, app, panels);
    }
}

/// Convert a `QueryMessage` from the database worker into an `Action`.
fn map_query_message(msg: db::QueryMessage) -> app::Action {
    match msg {
        db::QueryMessage::Completed(result) => app::Action::QueryCompleted(result),
        db::QueryMessage::Failed(err) => app::Action::QueryFailed(err),
        db::QueryMessage::SchemaFailed(err) => app::Action::SetTransient(err, true),
        db::QueryMessage::SchemaLoaded(entries) => app::Action::SchemaLoaded(entries),
        db::QueryMessage::ColumnsLoaded(table, cols) => app::Action::ColumnsLoaded(table, cols),
        db::QueryMessage::ExplainCompleted(bytecode, plan) => {
            app::Action::ExplainCompleted(bytecode, plan)
        }
        db::QueryMessage::ExplainFailed(err) => app::Action::ExplainFailed(err),
        db::QueryMessage::DbInfoLoaded(info) => app::Action::DbInfoLoaded(info),
        db::QueryMessage::DbInfoFailed(err) => app::Action::DbInfoFailed(err),
        db::QueryMessage::PragmasLoaded(entries) => app::Action::PragmasLoaded(entries),
        db::QueryMessage::PragmasFailed(err) => app::Action::PragmasFailed(err),
        db::QueryMessage::PragmaSet(name, val) => app::Action::PragmaSet(name, val),
        db::QueryMessage::PragmaFailed(name, err) => app::Action::PragmaFailed(name, err),
        db::QueryMessage::WalCheckpointed(msg) => app::Action::WalCheckpointed(msg),
        db::QueryMessage::WalCheckpointFailed(err) => app::Action::WalCheckpointFailed(err),
    }
}

/// Handle a single key press: route to help overlay, focused component, or global handler.
fn handle_key_event(
    key: ratatui::crossterm::event::KeyEvent,
    app: &mut AppState,
    panels: &mut UiPanels,
) {
    if app.help_visible {
        handle_help_key(key, app);
        return;
    }

    // Route to focused component first
    let focused = app.active_db().focus;
    let component_action = route_key_to_component(key, focused, app, panels);

    let action = component_action.or_else(|| event::map_global_key(key));
    if let Some(ref action) = action {
        app.update(action);
        dispatch_action_to_components(action, app, panels);
    }
}

/// Handle key events when the help overlay is visible.
fn handle_help_key(key: ratatui::crossterm::event::KeyEvent, app: &mut AppState) {
    match key.code {
        KeyCode::F(1) | KeyCode::Esc | KeyCode::Char('?') => {
            app.help_visible = false;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.help_scroll = app.help_scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.help_scroll = app.help_scroll.saturating_sub(1);
        }
        KeyCode::Char('g') => {
            app.help_scroll = 0;
        }
        KeyCode::Char('G') => {
            app.help_scroll = usize::MAX; // clamped in render
        }
        KeyCode::Char('q') if key.modifiers == KeyModifiers::CONTROL => {
            app.should_quit = true;
        }
        _ => {}
    }
}

/// Route a key event to the appropriate focused component.
/// When Bottom is focused, number keys switch sub-tabs; other keys go to the active bottom component.
fn route_key_to_component(
    key: ratatui::crossterm::event::KeyEvent,
    focused: PanelId,
    app: &AppState,
    panels: &mut UiPanels,
) -> Option<app::Action> {
    if focused == PanelId::Bottom {
        match key.code {
            KeyCode::Char('1') if key.modifiers == KeyModifiers::NONE => {
                Some(app::Action::SwitchBottomTab(BottomTab::Results))
            }
            KeyCode::Char('2') if key.modifiers == KeyModifiers::NONE => {
                Some(app::Action::SwitchBottomTab(BottomTab::Explain))
            }
            KeyCode::Char('3') if key.modifiers == KeyModifiers::NONE => {
                Some(app::Action::SwitchBottomTab(BottomTab::Detail))
            }
            KeyCode::Char('4') if key.modifiers == KeyModifiers::NONE => {
                Some(app::Action::SwitchBottomTab(BottomTab::ERDiagram))
            }
            _ => match app.active_db().bottom_tab {
                BottomTab::Results => panels.results.handle_key(key),
                BottomTab::Explain => panels.explain.handle_key(key),
                BottomTab::Detail => panels.record_detail.handle_key(key),
                BottomTab::ERDiagram => None,
            },
        }
    } else {
        match focused {
            PanelId::Schema => panels.schema.handle_key(key),
            PanelId::Editor => panels.editor.handle_key(key),
            PanelId::Bottom => unreachable!(), // handled by outer `if focused == PanelId::Bottom`
            PanelId::DbInfo => panels.db_info.handle_key(key),
            PanelId::Pragmas => panels.pragmas.handle_key(key),
        }
    }
}

/// Dispatch an action to UI components and database handle.
/// Handles both component state updates and I/O triggers (the handle lives in `AppState`).
#[allow(clippy::too_many_lines)]
fn dispatch_action_to_components(action: &app::Action, app: &mut AppState, panels: &mut UiPanels) {
    match action {
        app::Action::ExecuteQuery(sql) => {
            if !sql.trim().is_empty() {
                app.active_db_mut().handle.execute(sql.clone());
            }
        }
        app::Action::LoadColumns(table_name) => {
            app.active_db_mut().handle.load_columns(table_name.clone());
        }
        app::Action::PopulateEditor(sql) => {
            panels.editor.set_contents(sql);
        }
        app::Action::QueryCompleted(result) => {
            panels.results.set_results(result);
            // Mark explain as stale with the executed SQL
            panels.explain.mark_stale(result.sql.clone());
            // Populate record detail with the first row
            if let Some((cols, vals)) = panels.results.row_data(0) {
                panels.record_detail.set_row(cols, vals);
            } else {
                panels.record_detail.clear();
            }
        }
        app::Action::QueryFailed(err) => {
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        app::Action::SchemaLoaded(entries) => {
            panels.schema.set_schema(entries);
        }
        app::Action::ColumnsLoaded(table_name, columns) => {
            panels.schema.set_columns(table_name, columns.clone());
        }
        app::Action::SwitchBottomTab(BottomTab::Detail) => {
            // Populate record detail with the currently selected row
            if let Some(selected) = panels.results.selected_row() {
                if let Some((cols, vals)) = panels.results.row_data(selected) {
                    panels.record_detail.set_row(cols, vals);
                } else {
                    panels.record_detail.clear();
                }
            } else {
                panels.record_detail.clear();
            }
        }
        app::Action::SwitchSubTab(SubTab::Admin) => {
            // Lazy initial load for Admin components
            if panels.db_info.try_start_load() {
                let path = app.active_db().path.clone();
                app.active_db_mut().handle.load_db_info(path);
            }
            if panels.pragmas.try_start_load() {
                app.active_db_mut().handle.load_pragmas();
            }
        }
        // EXPLAIN result delivery
        app::Action::ExplainCompleted(bytecode, plan) => {
            panels.explain.set_results(bytecode.clone(), plan.clone());
        }
        app::Action::ExplainFailed(err) => {
            panels.explain.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // DB Info result delivery
        app::Action::DbInfoLoaded(info) => {
            panels.db_info.set_info(info.clone());
        }
        app::Action::DbInfoFailed(err) => {
            panels.db_info.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // Pragma result delivery
        app::Action::PragmasLoaded(entries) => {
            panels.pragmas.set_pragmas(entries.clone());
        }
        app::Action::PragmasFailed(err) => {
            panels.pragmas.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        app::Action::PragmaSet(name, value) => {
            panels.pragmas.confirm_edit(name, value.clone());
            app.transient_message = Some(app::TransientMessage {
                text: format!("{name} set to {value}"),
                created_at: std::time::Instant::now(),
                is_error: false,
            });
        }
        app::Action::PragmaFailed(name, err) => {
            panels.pragmas.cancel_edit();
            app.transient_message = Some(app::TransientMessage {
                text: format!("{name}: {err}"),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // WAL checkpoint result delivery
        app::Action::WalCheckpointed(msg) => {
            panels.db_info.set_checkpointing(false);
            app.transient_message = Some(app::TransientMessage {
                text: msg.clone(),
                created_at: std::time::Instant::now(),
                is_error: false,
            });
            // Refresh db info to update WAL frame count
            if panels.db_info.try_start_refresh() {
                let path = app.active_db().path.clone();
                app.active_db_mut().handle.load_db_info(path);
            }
        }
        app::Action::WalCheckpointFailed(err) => {
            panels.db_info.set_checkpointing(false);
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // I/O triggers
        app::Action::RefreshDbInfo => {
            if panels.db_info.try_start_refresh() {
                let path = app.active_db().path.clone();
                app.active_db_mut().handle.load_db_info(path);
            }
        }
        app::Action::RefreshPragmas => {
            // Only clear the edit buffer — don't clear pragma_in_flight.
            // If a set_pragma is in flight, its response will still arrive and
            // be handled by confirm_edit/cancel_edit in the PragmaSet/PragmaFailed arm.
            panels.pragmas.clear_editing();
            if panels.pragmas.try_start_refresh() {
                app.active_db_mut().handle.load_pragmas();
            }
        }
        app::Action::GenerateExplain(sql) => {
            panels.explain.set_loading();
            app.active_db_mut().handle.explain(sql.clone());
        }
        app::Action::SetPragma(name, val) => {
            // pragma_in_flight + in_flight_index are pre-set by PragmaDashboard::handle_key_editing
            // before emitting this action. No need to set them here.
            app.active_db_mut()
                .handle
                .set_pragma(name.clone(), val.clone());
        }
        app::Action::WalCheckpoint => {
            // Guard: info must be loaded, journal mode must be WAL, not already checkpointing
            let info = panels.db_info.info();
            let is_wal = info.is_some_and(|i| i.journal_mode.eq_ignore_ascii_case("wal"));
            if info.is_none() {
                app.transient_message = Some(app::TransientMessage {
                    text: "Database info not loaded yet".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
            } else if !is_wal {
                app.transient_message = Some(app::TransientMessage {
                    text: "Checkpoint requires WAL mode".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
            } else if panels.db_info.checkpointing() {
                // Already checkpointing — no-op
            } else {
                panels.db_info.set_checkpointing(true);
                app.active_db_mut().handle.wal_checkpoint();
            }
        }
        _ => {}
    }
}

fn render_ui(frame: &mut Frame, app: &AppState, panels: &mut UiPanels) {
    let theme = &app.theme;
    let db = app.active_db();
    let area = frame.area();

    // Minimum terminal size check
    if area.width < 80 || area.height < 24 {
        let msg = Paragraph::new("Terminal too small (min 80x24)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.error));
        let [_, center, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        frame.render_widget(msg, center);
        return;
    }

    // Top level: db tabs + sub-tabs + content + status bar
    let [db_tabs_area, sub_tabs_area, content_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // Database tab bar
    let db_tabs = Tabs::new(vec![format!(" {} ", db.label)])
        .select(0)
        .style(Style::default().fg(theme.fg).bg(theme.bg))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(db_tabs, db_tabs_area);

    // Sub-tab bar
    let sub_tab_index = match db.sub_tab {
        SubTab::Query => 0,
        SubTab::Admin => 1,
    };
    let sub_tabs = Tabs::new(vec![" Query ", " Admin "])
        .select(sub_tab_index)
        .style(Style::default().fg(theme.fg))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
    frame.render_widget(sub_tabs, sub_tabs_area);

    // Content area
    match db.sub_tab {
        SubTab::Query => {
            render_query_tab(
                frame,
                theme,
                content_area,
                db.focus,
                db.sidebar_visible,
                db.bottom_tab,
                panels,
            );
        }
        SubTab::Admin => {
            render_admin_tab(frame, theme, content_area, db.focus, panels);
        }
    }

    // Status bar
    components::status_bar::render(
        frame,
        status_area,
        app,
        panels.results.selected_row(),
        panels.results.row_count(),
        theme,
    );

    // Help overlay (rendered last so it floats on top)
    if app.help_visible {
        components::help::render(frame, app.help_scroll, theme);
    }
}

fn render_query_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    sidebar_visible: bool,
    bottom_tab: BottomTab,
    panels: &mut UiPanels,
) {
    if sidebar_visible {
        let [sidebar_area, main_area] =
            Layout::horizontal([Constraint::Percentage(20), Constraint::Percentage(80)])
                .areas(area);

        panels
            .schema
            .render(frame, sidebar_area, focus == PanelId::Schema, theme);

        let [editor_area, bottom_area] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(main_area);

        panels
            .editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, panels);
    } else {
        let [editor_area, bottom_area] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        panels
            .editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, panels);
    }
}

fn render_bottom_panel(
    frame: &mut Frame,
    theme: &Theme,
    bottom_area: Rect,
    focus: PanelId,
    bottom_tab: BottomTab,
    panels: &mut UiPanels,
) {
    let [bottom_tabs_area, bottom_content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(bottom_area);

    // Render bottom sub-tab bar
    let tab_index = match bottom_tab {
        BottomTab::Results => 0,
        BottomTab::Explain => 1,
        BottomTab::Detail => 2,
        BottomTab::ERDiagram => 3,
    };
    let bottom_tabs = Tabs::new(vec!["1:Results", "2:Explain", "3:Detail", "4:ER"])
        .select(tab_index)
        .style(Style::default().fg(theme.fg))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(bottom_tabs, bottom_tabs_area);

    // Render the active bottom component
    let is_focused = focus == PanelId::Bottom;
    match bottom_tab {
        BottomTab::Results => {
            panels
                .results
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::Explain => {
            panels
                .explain
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::Detail => {
            panels
                .record_detail
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::ERDiagram => {
            // Placeholder for ER Diagram — coming in a future milestone
            let block = ratatui::widgets::Block::bordered()
                .border_style(if is_focused {
                    Style::default().fg(theme.border_focused)
                } else {
                    Style::default().fg(theme.border)
                })
                .title("ER Diagram")
                .title_style(if is_focused {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg)
                });
            let inner = block.inner(bottom_content_area);
            frame.render_widget(block, bottom_content_area);
            if inner.height > 0 && inner.width > 0 {
                let msg = "ER Diagram \u{2014} coming soon";
                let msg_width = unicode_width::UnicodeWidthStr::width(msg) as u16;
                let x = inner.x + inner.width.saturating_sub(msg_width) / 2;
                let y = inner.y + inner.height / 2;
                let msg_area = Rect::new(x, y, msg_width.min(inner.width), 1);
                frame.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(theme.border)),
                    msg_area,
                );
            }
        }
    }
}

fn render_admin_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    panels: &mut UiPanels,
) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

    panels
        .db_info
        .render(frame, left, focus == PanelId::DbInfo, theme);
    panels
        .pragmas
        .render(frame, right, focus == PanelId::Pragmas, theme);
}
