mod app;
mod components;
mod db;
mod event;
mod highlight;
mod theme;

use std::time::Duration;

use clap::Parser;
use ratatui::crossterm::event::{Event, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Tabs};

use app::{AppState, DatabaseContext, PanelId, SubTab};
use components::Component;
use components::editor::QueryEditor;
use components::placeholder::Placeholder;
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
    db_info: Placeholder, // Still placeholder — Admin tab (later milestone)
    pragmas: Placeholder, // Still placeholder — Admin tab (later milestone)
}

impl UiPanels {
    fn new() -> Self {
        Self {
            schema: SchemaExplorer::new(),
            editor: QueryEditor::new(),
            results: ResultsTable::new(),
            db_info: Placeholder::new("Database Info"),
            pragmas: Placeholder::new("PRAGMA Dashboard"),
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
        // 1. Drain async result channel before key handling (multiple messages may arrive per frame)
        while let Some(msg) = app.active_db_mut().handle.try_recv() {
            let action = match msg {
                db::QueryMessage::Completed(result) => app::Action::QueryCompleted(result),
                db::QueryMessage::Failed(err) | db::QueryMessage::SchemaFailed(err) => {
                    app::Action::QueryFailed(err)
                }
                db::QueryMessage::SchemaLoaded(entries) => app::Action::SchemaLoaded(entries),
                db::QueryMessage::ColumnsLoaded(table, cols) => {
                    app::Action::ColumnsLoaded(table, cols)
                }
            };
            app.update(&action);
            dispatch_action_to_components(&action, app, &mut panels);
        }

        // 2. Poll events (16ms ~ 60fps)
        if let Some(Event::Key(key)) = event::poll_event(Duration::from_millis(16))?
            && key.kind == KeyEventKind::Press
        {
            // Route to focused component first
            let focused = app.active_db().focus;
            let component_action = match focused {
                PanelId::Schema => panels.schema.handle_key(key),
                PanelId::Editor => panels.editor.handle_key(key),
                PanelId::Bottom => panels.results.handle_key(key),
                PanelId::DbInfo => panels.db_info.handle_key(key),
                PanelId::Pragmas => panels.pragmas.handle_key(key),
            };

            let action = component_action.or_else(|| event::map_global_key(key));
            if let Some(ref action) = action {
                app.update(action);
                dispatch_action_to_components(action, app, &mut panels);
            }
        }

        // 3. Render
        if app.should_quit {
            break;
        }

        terminal.draw(|frame| {
            render_ui(frame, app, &mut panels);
        })?;
    }

    Ok(())
}

/// Dispatch an action to UI components and database handle.
/// Handles both component state updates and I/O triggers (the handle lives in `AppState`).
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
        }
        app::Action::QueryFailed(err) => {
            app.transient_message = Some((err.clone(), std::time::Instant::now()));
        }
        app::Action::SchemaLoaded(entries) => {
            panels.schema.set_schema(entries);
        }
        app::Action::ColumnsLoaded(table_name, columns) => {
            panels.schema.set_columns(table_name, columns.clone());
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
                panels,
            );
        }
        SubTab::Admin => {
            render_admin_tab(frame, theme, content_area, db.focus, panels);
        }
    }

    // Status bar — show transient error message if present (5s TTL), otherwise default hints
    let (status_text, status_style) = if let Some((ref msg, at)) = app.transient_message
        && at.elapsed() < Duration::from_secs(5)
    {
        (
            format!(" Error: {msg}"),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        let focus = db.focus;
        (
            format!(
                " Focus: {focus}  |  Tab/Esc: cycle  |  Ctrl+B: sidebar  |  Alt+1/2: Query/Admin  |  Ctrl+Q: quit",
            ),
            theme.status_bar_style,
        )
    };
    let status = Paragraph::new(status_text).style(status_style);
    frame.render_widget(status, status_area);
}

fn render_query_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    sidebar_visible: bool,
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
        panels
            .results
            .render(frame, bottom_area, focus == PanelId::Bottom, theme);
    } else {
        let [editor_area, bottom_area] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        panels
            .editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        panels
            .results
            .render(frame, bottom_area, focus == PanelId::Bottom, theme);
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
