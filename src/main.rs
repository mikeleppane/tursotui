mod app;
mod autocomplete;
mod components;
mod config;
mod db;
mod event;
mod export;
mod highlight;
mod history;
mod persistence;
mod theme;

use std::time::Duration;

use clap::Parser;
use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Tabs};

use app::{AppState, BottomTab, DatabaseContext, PanelId, SubTab};
use components::Component;
use components::data_editor::DataEditor;
use components::db_info::DbInfoPanel;
use components::editor::QueryEditor;
use components::explain::ExplainView;
use components::history::QueryHistoryPanel;
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
    history: QueryHistoryPanel,
    export_popup: Option<components::export::ExportPopup>,
    data_editor: DataEditor,
    /// Persistent clipboard handle — kept alive for the app's lifetime so that
    /// clipboard contents survive on Linux/Wayland (arboard drops contents on Drop).
    clipboard: Option<arboard::Clipboard>,
}

impl UiPanels {
    fn new(config: &crate::config::AppConfig) -> Self {
        let mut editor = QueryEditor::with_tab_size(config.editor.tab_size);
        editor.set_autocomplete_config(
            config.editor.autocomplete,
            config.editor.autocomplete_min_chars,
        );
        Self {
            schema: SchemaExplorer::new(),
            editor,
            results: ResultsTable::with_config(
                config.results.max_column_width,
                config.results.null_display.clone(),
            ),
            explain: ExplainView::new(),
            record_detail: RecordDetail::new(),
            db_info: DbInfoPanel::new(),
            pragmas: PragmaDashboard::new(),
            history: QueryHistoryPanel::new(),
            export_popup: None,
            data_editor: DataEditor::new(),
            clipboard: arboard::Clipboard::new().ok(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let (cfg, config_err) = config::load_config();

    // Open the first database (multi-db support in Milestone 7)
    let path = cli.database.first().map_or(":memory:", String::as_str);
    let handle = DatabaseHandle::open(path)
        .await
        .map_err(|e| format!("failed to open '{path}': {e}"))?;
    let db_context = DatabaseContext::new(handle, path.to_string());

    // Open history database (non-fatal if it fails)
    let (history_db, history_err) = match history::HistoryDb::open().await {
        Ok(db) => {
            db.prune(cfg.history.max_entries).await;
            (Some(db), None)
        }
        Err(e) => (None, Some(format!("History unavailable: {e}"))),
    };

    let mut app = AppState::new(db_context, cfg, history_db);

    // Show config or history error as transient message
    let startup_err = config_err.or(history_err);
    if let Some(err_msg) = startup_err {
        app.transient_message = Some(app::TransientMessage {
            text: err_msg,
            created_at: std::time::Instant::now(),
            is_error: true,
        });
    }

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
    let mut panels = UiPanels::new(&app.config);

    // Restore saved editor buffer
    if let Some(saved) = persistence::load_buffer(&app.active_db().path)
        && !saved.is_empty()
    {
        panels.editor.set_contents(&saved);
        panels.editor.mark_saved();
        app.transient_message = Some(app::TransientMessage {
            text: "Restored editor buffer".to_string(),
            created_at: std::time::Instant::now(),
            is_error: false,
        });
    }

    loop {
        // 1. Drain async result channel before key handling
        drain_async_messages(app, &mut panels);

        // 2. Poll events (16ms ~ 60fps)
        if let Some(Event::Key(key)) = event::poll_event(Duration::from_millis(16))?
            && key.kind == KeyEventKind::Press
        {
            handle_key_event(key, app, &mut panels);
        }

        // 3. Auto-save editor buffer (debounced, 1s).
        // Synchronous write — sub-KB buffers are sub-millisecond on local disk.
        // If slow-filesystem jank is reported, migrate to spawn_blocking.
        if panels.editor.is_dirty() && panels.editor.last_save_elapsed() > Duration::from_secs(1) {
            let path = app.active_db().path.clone();
            if let Err(e) = persistence::save_buffer(&path, &panels.editor.contents()) {
                app.transient_message = Some(app::TransientMessage {
                    text: format!("Auto-save failed: {e}"),
                    created_at: std::time::Instant::now(),
                    is_error: true,
                });
            } else {
                panels.editor.mark_saved();
            }
        }

        // 4. Clear expired transient message
        if let Some(ref tm) = app.transient_message
            && tm.created_at.elapsed() >= components::status_bar::TRANSIENT_TTL
        {
            app.transient_message = None;
        }

        // 5. Render
        if app.should_quit {
            break;
        }

        terminal.draw(|frame| {
            render_ui(frame, app, &mut panels);
        })?;
    }

    // Final buffer save on quit
    if panels.editor.is_dirty() {
        let _ = persistence::save_buffer(&app.active_db().path, &panels.editor.contents());
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

    // Drain history messages (collect first to avoid borrow conflicts)
    let history_msgs: Vec<_> = app
        .history_db
        .as_mut()
        .map(|db| std::iter::from_fn(|| db.try_recv()).collect())
        .unwrap_or_default();
    for msg in history_msgs {
        let action = map_history_message(msg);
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
        db::QueryMessage::TransactionFailed(err) => app::Action::DataEditsFailed(err),
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
        db::QueryMessage::IntegrityCheckCompleted(msg) => app::Action::IntegrityCheckCompleted(msg),
        db::QueryMessage::IntegrityCheckFailed(msg) => app::Action::IntegrityCheckFailed(msg),
        db::QueryMessage::TransactionCommitted => app::Action::DataEditsCommitted,
        db::QueryMessage::ForeignKeysLoaded(table, fks) => app::Action::FKLoaded(table, fks),
    }
}

/// Convert a `HistoryMessage` from the history worker into an `Action`.
fn map_history_message(msg: history::HistoryMessage) -> app::Action {
    match msg {
        history::HistoryMessage::Loaded(entries) => app::Action::HistoryLoaded(entries),
        history::HistoryMessage::LoadFailed(err) => app::Action::SetTransient(err, true),
        history::HistoryMessage::Deleted(_) => app::Action::HistoryReloadRequested,
    }
}

/// Handle a single key press: route to help overlay, focused component, or global handler.
fn handle_key_event(
    key: ratatui::crossterm::event::KeyEvent,
    app: &mut AppState,
    panels: &mut UiPanels,
) {
    match app.active_overlay {
        Some(app::Overlay::Help) => {
            handle_help_key(key, app);
            return;
        }
        Some(app::Overlay::History) => {
            if let Some(action) = panels.history.handle_key(key) {
                app.update(&action);
                dispatch_action_to_components(&action, app, panels);
            }
            return;
        }
        Some(app::Overlay::Export) => {
            if let Some(ref mut popup) = panels.export_popup
                && let Some(action) = popup.handle_key(key)
            {
                if matches!(&action, app::Action::ExecuteExport) {
                    execute_export(app, panels);
                    app.active_overlay = None;
                    panels.export_popup = None;
                } else {
                    app.update(&action);
                    dispatch_action_to_components(&action, app, panels);
                }
            }
            return;
        }
        Some(app::Overlay::DmlPreview { .. }) => {
            if key.code == KeyCode::Esc {
                app.active_overlay = None;
            }
            return;
        }
        None => {}
    }

    // Route to focused component first
    let focused = app.active_db().focus;
    let component_action = route_key_to_component(key, focused, app, panels);

    let action = component_action.or_else(|| event::map_global_key(key));
    if let Some(ref action) = action {
        app.update(action);
        dispatch_action_to_components(action, app, panels);
    }

    // Refresh or auto-trigger autocomplete after buffer-modifying keys
    // (typing, backspace, delete). Navigation keys (Up/Down/Esc/Tab) are handled
    // by the popup interceptor and should NOT trigger a refresh.
    let buffer_changed = matches!(
        (key.modifiers, key.code),
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(_))
            | (KeyModifiers::NONE, KeyCode::Backspace | KeyCode::Delete)
    );
    if buffer_changed && app.active_db().focus == PanelId::Editor {
        let schema = &app.active_db().schema_cache;
        if panels.editor.autocomplete_popup.is_some() {
            panels.editor.refresh_autocomplete(schema);
        } else if panels.editor.autocomplete_enabled() {
            // Auto-trigger: open the popup when enabled and the user types
            // enough characters to meet the min_chars threshold.
            panels.editor.auto_trigger_autocomplete(schema);
        }
    }
}

/// Handle key events when the help overlay is visible.
fn handle_help_key(key: ratatui::crossterm::event::KeyEvent, app: &mut AppState) {
    match key.code {
        KeyCode::F(1) | KeyCode::Esc | KeyCode::Char('?') => {
            app.active_overlay = None;
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
                BottomTab::Results => {
                    // DataEditor intercepts before ResultsTable when active
                    if panels.data_editor.is_active()
                        && let Some(action) = panels.data_editor.handle_key(key)
                    {
                        return Some(action);
                    }
                    panels.results.handle_key(key)
                }
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
        app::Action::ExecuteQuery(sql, _source, source_table) => {
            if !sql.trim().is_empty() {
                app.active_db_mut()
                    .handle
                    .execute(sql.clone(), source_table.clone());
            }
        }
        app::Action::LoadColumns(table_name) => {
            app.active_db_mut().handle.load_columns(table_name.clone());
        }
        app::Action::PopulateEditor(sql) | app::Action::RecallHistory(sql) => {
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
            // Refresh schema if the query contained DDL
            let has_ddl = matches!(result.query_kind, db::QueryKind::Ddl)
                || (matches!(result.query_kind, db::QueryKind::Batch { .. }) && {
                    let sql_lower = result.sql.to_lowercase();
                    sql_lower.contains("create ")
                        || sql_lower.contains("alter ")
                        || sql_lower.contains("drop ")
                });
            if has_ddl {
                app.active_db_mut().handle.load_schema();
            }
            // Log to query history
            let origin = match result.query_kind {
                db::QueryKind::Ddl => "ddl",
                db::QueryKind::Pragma => "pragma",
                _ => "user",
            };
            if let Some(ref history) = app.history_db {
                history.log_query(history::LogEntry {
                    sql: result.sql.clone(),
                    database_path: app.active_db().path.clone(),
                    execution_time_ms: result.execution_time.as_millis() as u64,
                    row_count: result.rows.len(),
                    error_message: None,
                    origin,
                });
            }
            // Editability detection: determine if result targets a single editable table.
            // Clear any pending deferred check — only the latest query matters.
            app.pending_edit_table = None;
            let source_table = if result.source_table.is_some() {
                result.source_table.clone() // Tier 1: hint from ExecuteQuery
            } else {
                components::data_editor::detect_source_table(&result.sql) // Tier 2: SQL parse
            };
            if let Some(ref table) = source_table {
                let entries = &app.active_db().schema_cache.entries;
                if components::data_editor::check_view_rejection(table, entries) {
                    panels.data_editor.deactivate();
                } else if let Some(cols) = app.active_db().schema_cache.get_columns(table) {
                    let pk_cols = components::data_editor::find_pk_columns(cols);
                    if pk_cols.is_empty() {
                        panels.data_editor.deactivate();
                        app.transient_message = Some(app::TransientMessage {
                            text: format!("'{table}' has no primary key — read-only"),
                            created_at: std::time::Instant::now(),
                            is_error: false,
                        });
                    } else {
                        let cols_cloned = cols.clone();
                        panels.data_editor.activate(
                            table.clone(),
                            pk_cols,
                            cols_cloned,
                            result.sql.clone(),
                        );
                    }
                } else {
                    // Columns not cached — defer activation until ColumnsLoaded arrives.
                    // Store both table name and activating SQL so the deferred path
                    // doesn't rely on last_executed_sql (which may change).
                    app.pending_edit_table = Some((table.clone(), result.sql.clone()));
                    app.active_db_mut().handle.load_columns(table.clone());
                    panels.data_editor.deactivate();
                }
            } else {
                panels.data_editor.deactivate();
            }
        }
        app::Action::QueryFailed(err) => {
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
            // Log failed query to history (use stored SQL from ExecuteQuery)
            if let Some(ref history) = app.history_db
                && let Some(ref sql) = app.active_db().last_executed_sql
            {
                history.log_query(history::LogEntry {
                    sql: sql.clone(),
                    database_path: app.active_db().path.clone(),
                    execution_time_ms: 0,
                    row_count: 0,
                    error_message: Some(err.clone()),
                    origin: "user",
                });
            }
        }
        app::Action::SchemaLoaded(entries) => {
            panels.schema.set_schema(entries);
            // Populate schema cache and trigger eager column loading for autocomplete
            let db = app.active_db_mut();
            db.schema_cache.entries.clone_from(entries);
            db.schema_cache.columns.clear();
            db.schema_cache.fully_loaded = false;
            let table_names: Vec<String> = entries
                .iter()
                .filter(|e| e.obj_type == "table" || e.obj_type == "view")
                .map(|e| e.name.clone())
                .collect();
            db.handle.load_all_columns(&table_names);
        }
        app::Action::ColumnsLoaded(table_name, columns) => {
            panels.schema.set_columns(table_name, columns.clone());
            // Update schema cache for autocomplete
            let db = app.active_db_mut();
            db.schema_cache
                .columns
                .insert(table_name.clone(), columns.clone());
            // Check if all tables/views have been loaded
            let expected = db
                .schema_cache
                .entries
                .iter()
                .filter(|e| e.obj_type == "table" || e.obj_type == "view")
                .count();
            if db.schema_cache.columns.len() >= expected {
                db.schema_cache.fully_loaded = true;
            }
            // Check if this completes a deferred editability check
            let pending = app.pending_edit_table.clone();
            if let Some((ref pending_table, ref activating_sql)) = pending
                && pending_table == table_name
            {
                let pk_cols = components::data_editor::find_pk_columns(columns);
                if pk_cols.is_empty() {
                    panels.data_editor.deactivate();
                } else {
                    panels.data_editor.activate(
                        table_name.clone(),
                        pk_cols,
                        columns.clone(),
                        activating_sql.clone(),
                    );
                }
                app.pending_edit_table = None;
            }
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
        app::Action::IntegrityCheck => {
            // Guard: info must be loaded, not already running an integrity check
            let info = panels.db_info.info();
            if info.is_none() {
                app.transient_message = Some(app::TransientMessage {
                    text: "Database info not loaded yet".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
            } else {
                app.active_db().handle.integrity_check();
            }
        }
        app::Action::IntegrityCheckCompleted(msg) => {
            app.transient_message = Some(app::TransientMessage {
                text: msg.clone(),
                created_at: std::time::Instant::now(),
                is_error: false,
            });
        }
        app::Action::IntegrityCheckFailed(msg) => {
            app.transient_message = Some(app::TransientMessage {
                text: format!("Integrity check failed: {msg}"),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        app::Action::ClearEditor => {
            panels.editor.clear();
            let path = app.active_db().path.clone();
            let _ = persistence::delete_buffer(&path);
        }
        app::Action::ShowHistory => {
            if app.active_overlay == Some(app::Overlay::History) {
                if let Some(ref history_db) = app.history_db {
                    panels.history.set_loading();
                    history_db.request_load(
                        500,
                        panels.history.db_filter_value(),
                        panels.history.origin_filter(),
                        panels.history.search_text(),
                        panels.history.errors_only(),
                    );
                } else {
                    panels.history.set_unavailable();
                }
            }
        }
        app::Action::HistoryLoaded(entries) => {
            panels.history.set_entries(entries.clone());
        }
        app::Action::RecallAndExecute(sql) => {
            // Note: duplicates ExecuteQuery state+dispatch logic because we can't
            // recursively call dispatch_action_to_components. Keep in sync.
            panels.editor.set_contents(sql);
            if !sql.trim().is_empty() {
                let db = app.active_db_mut();
                db.executing = true;
                db.last_execution_source = app::ExecutionSource::FullBuffer;
                db.last_executed_sql = Some(sql.clone());
                db.handle.execute(sql.clone(), None);
            }
        }
        app::Action::DeleteHistoryEntry(id) => {
            if let Some(ref history_db) = app.history_db {
                history_db.request_delete(*id);
                // Reload triggered by HistoryReloadRequested when Deleted confirmation arrives
                history_db.request_load(
                    500,
                    panels.history.db_filter_value(),
                    panels.history.origin_filter(),
                    panels.history.search_text(),
                    panels.history.errors_only(),
                );
            }
        }
        app::Action::HistoryReloadRequested => {
            // Reload history after a confirmed delete (separate from DeleteHistoryEntry
            // to avoid re-triggering request_delete in a loop)
            if let Some(ref history_db) = app.history_db {
                history_db.request_load(
                    500,
                    panels.history.db_filter_value(),
                    panels.history.origin_filter(),
                    panels.history.search_text(),
                    panels.history.errors_only(),
                );
            }
        }
        app::Action::ToggleTheme => {
            if let Err(e) = crate::config::save_config(&app.config) {
                app.transient_message = Some(app::TransientMessage {
                    text: format!("Config save failed: {e}"),
                    created_at: std::time::Instant::now(),
                    is_error: true,
                });
            }
        }
        app::Action::WalCheckpoint => {
            // Guard: info must be loaded, journal mode must be WAL, not already checkpointing
            let info = panels.db_info.info();
            let is_wal = info.is_some_and(|i| {
                i.journal_mode.eq_ignore_ascii_case("wal")
                    || i.journal_mode.eq_ignore_ascii_case("mvcc")
            });
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
        app::Action::TriggerAutocomplete => {
            let schema = &app.active_db().schema_cache;
            panels.editor.trigger_autocomplete(schema);
        }
        app::Action::ShowExport => {
            if app.active_overlay == Some(app::Overlay::Export) {
                // Create the popup with current result data
                if panels.results.export_data().is_some() {
                    let row_count = panels.results.row_count();
                    let table_name = app.active_db().last_executed_sql.as_deref().map_or_else(
                        || "table_name".to_string(),
                        components::export::infer_table_name,
                    );
                    panels.export_popup =
                        Some(components::export::ExportPopup::new(row_count, table_name));
                } else {
                    // No results to export
                    app.active_overlay = None;
                    app.transient_message = Some(app::TransientMessage {
                        text: "No results to export".to_string(),
                        created_at: std::time::Instant::now(),
                        is_error: false,
                    });
                }
            } else {
                panels.export_popup = None;
            }
        }
        app::Action::CopyAllResults => {
            if let Some((columns, rows)) = panels.results.export_data() {
                let tsv = export::format_tsv(columns, rows);
                let row_count = rows.len();
                match panels
                    .clipboard
                    .as_mut()
                    .ok_or(arboard::Error::ContentNotAvailable)
                    .and_then(|cb| cb.set_text(&tsv))
                {
                    Ok(()) => {
                        app.transient_message = Some(app::TransientMessage {
                            text: format!("{row_count} rows copied as TSV"),
                            created_at: std::time::Instant::now(),
                            is_error: false,
                        });
                    }
                    Err(e) => {
                        app.transient_message = Some(app::TransientMessage {
                            text: format!("Clipboard unavailable: {e}"),
                            created_at: std::time::Instant::now(),
                            is_error: true,
                        });
                    }
                }
            } else {
                app.transient_message = Some(app::TransientMessage {
                    text: "No results to copy".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
            }
        }
        // Data editor cell edit actions
        app::Action::StartCellEdit => {
            if !panels.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = panels.results.selected_row() else {
                return;
            };
            let col = panels.results.selected_col_index();
            let Some((cols, row_vals)) = panels.results.row_data(row_idx) else {
                // Row is a pending insert (beyond query-returned rows) — not yet editable
                app.transient_message = Some(app::TransientMessage {
                    text: "Pending insert rows cannot be edited — submit first".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                return;
            };
            // Reject BLOB columns
            let col_info = panels.data_editor.columns().get(col);
            if col_info.is_some_and(|c| c.col_type.to_lowercase().contains("blob")) {
                app.transient_message = Some(app::TransientMessage {
                    text: "Cannot edit BLOB columns".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                return;
            }
            let pk_cols = panels.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            let value = row_vals.get(col).and_then(Option::as_deref);
            let notnull = col_info.is_some_and(|c| c.notnull);
            // Pass the full original row snapshot for ChangeLog::original field
            let _ = cols;
            let original_row: Vec<Option<String>> = row_vals.to_vec();
            panels
                .data_editor
                .start_cell_edit(pk, row_idx, col, value, notnull, original_row);
        }
        app::Action::ConfirmCellEdit(value) => {
            panels.data_editor.confirm_edit(value.clone());
        }
        app::Action::CancelCellEdit => {
            panels.data_editor.cancel_edit();
        }
        app::Action::AddRow => {
            panels.data_editor.add_row();
        }
        app::Action::ToggleDeleteRow => {
            if !panels.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = panels.results.selected_row() else {
                return;
            };
            let Some((_cols, row_vals)) = panels.results.row_data(row_idx) else {
                // Pending insert row — remove the insert instead of toggling delete
                let query_row_count = panels.results.row_count();
                let insert_idx = row_idx.saturating_sub(query_row_count);
                panels.data_editor.remove_pending_insert(insert_idx);
                return;
            };
            let pk_cols = panels.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            let original: Vec<Option<String>> = row_vals.to_vec();
            panels.data_editor.toggle_delete_row(&pk, &original);
        }
        app::Action::CloneRow(_) => {
            if !panels.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = panels.results.selected_row() else {
                return;
            };
            let Some((_cols, row_vals)) = panels.results.row_data(row_idx) else {
                return;
            };
            let values: Vec<Option<String>> = row_vals.to_vec();
            panels.data_editor.clone_row(values);
        }
        app::Action::RevertCell => {
            if !panels.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = panels.results.selected_row() else {
                return;
            };
            let col = panels.results.selected_col_index();
            let Some((_cols, row_vals)) = panels.results.row_data(row_idx) else {
                return;
            };
            let pk_cols = panels.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            panels.data_editor.revert_cell_edit(&pk, col);
        }
        app::Action::RevertRow => {
            if !panels.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = panels.results.selected_row() else {
                return;
            };
            let Some((_cols, row_vals)) = panels.results.row_data(row_idx) else {
                return;
            };
            let pk_cols = panels.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            panels.data_editor.revert_row_edit(&pk);
        }
        app::Action::RevertAll => {
            panels.data_editor.revert_all_edits();
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

    // JSON overlay (renders on top of everything except help)
    if db.bottom_tab == BottomTab::Detail && panels.record_detail.has_overlay() {
        panels.record_detail.render_overlay(frame, area, theme);
    }

    // History overlay
    if app.active_overlay == Some(app::Overlay::History) {
        panels.history.render(frame, area, theme);
    }

    // Export overlay
    if app.active_overlay == Some(app::Overlay::Export)
        && let Some(ref popup) = panels.export_popup
    {
        popup.render(frame, area, theme);
    }

    // Modal cell editor overlay (renders above content, below help)
    if let Some(editor) = panels.data_editor.cell_editor()
        && editor.modal
    {
        let table = panels.data_editor.source_table().unwrap_or("table");
        let col_name = panels
            .data_editor
            .columns()
            .get(editor.col)
            .map_or("col", |c| c.name.as_str());
        editor.render_modal(frame, area, table, col_name, theme);
    }

    // Help overlay (rendered last so it floats on top)
    if app.active_overlay == Some(app::Overlay::Help) {
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
        render_autocomplete_popup(frame, &panels.editor, editor_area, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, panels);
    } else {
        let [editor_area, bottom_area] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        panels
            .editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        render_autocomplete_popup(frame, &panels.editor, editor_area, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, panels);
    }
}

/// Render the autocomplete popup over the editor if active.
fn render_autocomplete_popup(
    frame: &mut Frame,
    editor: &components::editor::QueryEditor,
    editor_area: Rect,
    theme: &Theme,
) {
    let Some(ref popup) = editor.autocomplete_popup else {
        return;
    };
    let (cursor_row, cursor_col) = editor.cursor_position();
    let line_count = editor.buffer_lines().len();
    let gutter_digits = line_count.to_string().len();
    let gutter_width = (gutter_digits + 1) as u16;
    // editor_area inner (subtract border)
    let inner_x = editor_area.x + 1 + gutter_width;
    let inner_y = editor_area.y + 1;
    // Use char index (not display width) to match the editor's own cursor
    // placement at editor.rs:920. Both are wrong for wide chars (CJK/emoji)
    // but must stay consistent so the popup aligns with the terminal cursor.
    let cursor_x = inner_x + cursor_col as u16;
    let cursor_y = inner_y + cursor_row.saturating_sub(editor.scroll_offset()) as u16;
    popup.render(frame, cursor_x, cursor_y, theme);
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

    // Inject edit state into ResultsTable before rendering
    if panels.data_editor.is_active() {
        panels
            .results
            .set_edit_state(Some(panels.data_editor.build_render_state()));
    } else {
        panels.results.set_edit_state(None);
    }

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

fn execute_export(app: &mut AppState, panels: &mut UiPanels) {
    let Some(ref popup) = panels.export_popup else {
        return;
    };
    let Some((columns, rows)) = panels.results.export_data() else {
        return;
    };

    let formatted = match popup.format {
        components::export::ExportFormat::Csv => export::format_csv(columns, rows),
        components::export::ExportFormat::Json => export::format_json(columns, rows),
        components::export::ExportFormat::SqlInsert => {
            export::format_sql_insert(columns, rows, &popup.table_name)
        }
    };

    let row_count = rows.len();

    match popup.target {
        components::export::ExportTarget::Clipboard => {
            match panels
                .clipboard
                .as_mut()
                .ok_or(arboard::Error::ContentNotAvailable)
                .and_then(|cb| cb.set_text(&formatted))
            {
                Ok(()) => {
                    app.transient_message = Some(app::TransientMessage {
                        text: format!("{row_count} rows copied as {}", popup.format.label()),
                        created_at: std::time::Instant::now(),
                        is_error: false,
                    });
                }
                Err(e) => {
                    // Fallback: try to write to file
                    let fallback_path = format!("./export.{}", popup.format.extension());
                    match std::fs::write(&fallback_path, &formatted) {
                        Ok(()) => {
                            app.transient_message = Some(app::TransientMessage {
                                text: format!(
                                    "Clipboard unavailable -- saved to {fallback_path} ({e})"
                                ),
                                created_at: std::time::Instant::now(),
                                is_error: false,
                            });
                        }
                        Err(write_err) => {
                            app.transient_message = Some(app::TransientMessage {
                                text: format!("Export failed: {write_err}"),
                                created_at: std::time::Instant::now(),
                                is_error: true,
                            });
                        }
                    }
                }
            }
        }
        components::export::ExportTarget::File => {
            match std::fs::write(&popup.file_path, &formatted) {
                Ok(()) => {
                    app.transient_message = Some(app::TransientMessage {
                        text: format!("{row_count} rows exported to {}", popup.file_path),
                        created_at: std::time::Instant::now(),
                        is_error: false,
                    });
                }
                Err(e) => {
                    app.transient_message = Some(app::TransientMessage {
                        text: format!("Export failed: {e}"),
                        created_at: std::time::Instant::now(),
                        is_error: true,
                    });
                }
            }
        }
    }
}
