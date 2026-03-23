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
use ratatui::widgets::{Clear, Paragraph, Tabs};

use app::{AppState, BottomTab, DatabaseContext, PanelId, SubTab};
use components::Component;
use components::history::QueryHistoryPanel;
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

/// Global UI state shared across all database tabs.
struct GlobalUi {
    history: QueryHistoryPanel,
    bookmarks: components::bookmarks::BookmarkPanel,
    /// Persistent clipboard handle — kept alive for the app's lifetime so that
    /// clipboard contents survive on Linux/Wayland (arboard drops contents on Drop).
    clipboard: Option<arboard::Clipboard>,
    /// File picker popup (global since it opens databases, not per-db).
    file_picker: Option<components::file_picker::FilePicker>,
    /// Go to Object popup (global since it searches across all databases).
    goto_object: Option<components::goto_object::GoToObject>,
}

impl GlobalUi {
    fn new() -> Self {
        Self {
            history: QueryHistoryPanel::new(),
            bookmarks: components::bookmarks::BookmarkPanel::new(),
            clipboard: arboard::Clipboard::new().ok(),
            file_picker: None,
            goto_object: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let (cfg, config_err) = config::load_config();

    // Open all databases from CLI args, deduplicating canonical paths.
    let mut databases = Vec::new();
    let mut seen_canonical: Vec<std::path::PathBuf> = Vec::new();
    let mut duplicate_warning: Option<String> = None;

    for path_str in &cli.database {
        // Detect duplicate canonical paths
        if path_str != ":memory:"
            && let Ok(canonical) = std::fs::canonicalize(path_str)
        {
            if seen_canonical.contains(&canonical) {
                duplicate_warning = Some(format!("Duplicate database path ignored: {path_str}"));
                continue;
            }
            seen_canonical.push(canonical);
        }

        let handle = DatabaseHandle::open(path_str)
            .await
            .map_err(|e| format!("failed to open '{path_str}': {e}"))?;
        databases.push(DatabaseContext::new(handle, path_str.clone(), &cfg));
    }

    // Open history database (non-fatal if it fails)
    let (history_db, history_err) = match history::HistoryDb::open().await {
        Ok(db) => {
            db.prune(cfg.history.max_entries).await;
            (Some(db), None)
        }
        Err(e) => (None, Some(format!("History unavailable: {e}"))),
    };

    let mut app = AppState::new(databases, cfg, history_db);

    // Show errors first (they take priority), then warnings
    let startup_msg = config_err.or(history_err);
    if let Some(err_msg) = startup_msg {
        app.transient_message = Some(app::TransientMessage {
            text: err_msg,
            created_at: std::time::Instant::now(),
            is_error: true,
        });
    } else if let Some(warn_msg) = duplicate_warning {
        app.transient_message = Some(app::TransientMessage {
            text: warn_msg,
            created_at: std::time::Instant::now(),
            is_error: false,
        });
    }

    // Trigger schema load on all databases at startup
    for db in &mut app.databases {
        db.handle.load_schema();
    }

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
    let mut global_ui = GlobalUi::new();

    // Restore saved editor buffer for all databases
    for db in &mut app.databases {
        if let Some(saved) = persistence::load_buffer(&db.path)
            && !saved.is_empty()
        {
            db.editor.set_contents(&saved);
            db.editor.mark_saved();
        }
    }
    // Show restore message only if active db had a buffer
    if !app.active_db().editor.contents().is_empty() {
        app.transient_message = Some(app::TransientMessage {
            text: "Restored editor buffer".to_string(),
            created_at: std::time::Instant::now(),
            is_error: false,
        });
    }

    loop {
        // 1. Drain async result channel before key handling
        drain_async_messages(app, &mut global_ui);

        // 2. Poll events (16ms ~ 60fps)
        if let Some(Event::Key(key)) = event::poll_event(Duration::from_millis(16))?
            && key.kind == KeyEventKind::Press
        {
            handle_key_event(key, app, &mut global_ui);
        }

        // 3. Auto-save editor buffer (debounced, 1s) for all databases.
        // Synchronous write — sub-KB buffers are sub-millisecond on local disk.
        for db in &mut app.databases {
            if db.editor.is_dirty() && db.editor.last_save_elapsed() > Duration::from_secs(1) {
                let path = db.path.clone();
                if let Err(e) = persistence::save_buffer(&path, &db.editor.contents()) {
                    app.transient_message = Some(app::TransientMessage {
                        text: format!("Auto-save failed: {e}"),
                        created_at: std::time::Instant::now(),
                        is_error: true,
                    });
                } else {
                    db.editor.mark_saved();
                }
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
            render_ui(frame, app, &mut global_ui);
        })?;
    }

    // Final buffer save on quit for all databases
    for db in &mut app.databases {
        if db.editor.is_dirty() {
            let _ = persistence::save_buffer(&db.path, &db.editor.contents());
        }
    }

    Ok(())
}

/// Drain all pending async messages and dispatch the resulting actions.
fn drain_async_messages(app: &mut AppState, global_ui: &mut GlobalUi) {
    // Phase 1: collect all pending messages from ALL databases with their db_idx
    let mut pending: Vec<(usize, db::QueryMessage)> = Vec::new();
    for (db_idx, db) in app.databases.iter_mut().enumerate() {
        while let Some(msg) = db.handle.try_recv() {
            pending.push((db_idx, msg));
        }
    }

    // Phase 2: process each, routing to the specific database
    for (db_idx, msg) in pending {
        // Handle RowCount directly (needs db_idx routing, no Action needed)
        if let db::QueryMessage::RowCount(ref table, count) = msg {
            app.databases[db_idx]
                .schema_cache
                .row_counts
                .insert(table.clone(), count);
            continue;
        }
        let action = map_query_message(msg);
        app.update_for_db(db_idx, &action);
        dispatch_action_to_db(db_idx, &action, app, global_ui);
    }

    // Drain history messages (collect first to avoid borrow conflicts)
    let history_msgs: Vec<_> = app
        .history_db
        .as_mut()
        .map(|db| std::iter::from_fn(|| db.try_recv()).collect())
        .unwrap_or_default();
    // History messages (HistoryLoaded, etc.) are dispatched to active_db
    // because they only affect the global QueryHistoryPanel — db_idx is
    // not meaningfully used in those handlers.
    for msg in history_msgs {
        let action = map_history_message(msg);
        app.update(&action);
        dispatch_action_to_db(app.active_db, &action, app, global_ui);
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
        db::QueryMessage::RowCount(..) => unreachable!("handled in drain loop"),
    }
}

/// Convert a `HistoryMessage` from the history worker into an `Action`.
fn map_history_message(msg: history::HistoryMessage) -> app::Action {
    match msg {
        history::HistoryMessage::Loaded(entries) => app::Action::HistoryLoaded(entries),
        history::HistoryMessage::LoadFailed(err)
        | history::HistoryMessage::BookmarkSaveFailed(err) => app::Action::SetTransient(err, true),
        history::HistoryMessage::Deleted(_) => app::Action::HistoryReloadRequested,
        history::HistoryMessage::BookmarksLoaded(entries) => app::Action::BookmarksLoaded(entries),
        history::HistoryMessage::BookmarkSaved(_)
        | history::HistoryMessage::BookmarkDeleted(_)
        | history::HistoryMessage::BookmarkUpdated(_) => app::Action::BookmarkReloadRequested,
    }
}

/// Handle a single key press: route to help overlay, focused component, or global handler.
#[allow(clippy::too_many_lines)]
fn handle_key_event(
    key: ratatui::crossterm::event::KeyEvent,
    app: &mut AppState,
    global_ui: &mut GlobalUi,
) {
    let active_idx = app.active_db;

    match app.active_overlay {
        Some(app::Overlay::Help) => {
            handle_help_key(key, app);
            return;
        }
        Some(app::Overlay::History) => {
            if let Some(action) = global_ui.history.handle_key(key) {
                app.update(&action);
                dispatch_action_to_db(active_idx, &action, app, global_ui);
            }
            return;
        }
        Some(app::Overlay::Export) => {
            let db = &mut app.databases[active_idx];
            if let Some(ref mut popup) = db.export_popup
                && let Some(action) = popup.handle_key(key)
            {
                if matches!(&action, app::Action::ExecuteExport) {
                    execute_export(app, global_ui);
                    app.active_overlay = None;
                    app.databases[active_idx].export_popup = None;
                } else {
                    app.update(&action);
                    dispatch_action_to_db(active_idx, &action, app, global_ui);
                }
            }
            return;
        }
        Some(app::Overlay::DmlPreview { submit_enabled }) => {
            let db = &mut app.databases[active_idx];
            match key.code {
                KeyCode::Esc => {
                    app.active_overlay = None;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    db.data_editor.scroll_preview_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    db.data_editor.scroll_preview_up();
                }
                KeyCode::Enter if submit_enabled => {
                    let action = app::Action::SubmitDataEdits;
                    app.update(&action);
                    dispatch_action_to_db(active_idx, &action, app, global_ui);
                }
                _ => {}
            }
            return;
        }
        Some(app::Overlay::FilePicker) => {
            if let Some(ref mut picker) = global_ui.file_picker
                && let Some(action) = picker.handle_key(key)
            {
                match &action {
                    app::Action::OpenDatabase(_) => {
                        // Dispatch OpenDatabase; picker dismissal happens on
                        // success inside dispatch_action_to_db.
                        app.update(&action);
                        dispatch_action_to_db(active_idx, &action, app, global_ui);
                    }
                    app::Action::OpenFilePicker => {
                        // Esc — toggle off via update()
                        app.update(&action);
                        global_ui.file_picker = None;
                    }
                    app::Action::Quit => {
                        app.should_quit = true;
                    }
                    _ => {}
                }
            }
            return;
        }
        Some(app::Overlay::DdlViewer) => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.active_overlay = None;
                    app.ddl_viewer = None;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if let Some(ref mut viewer) = app.ddl_viewer {
                        let max = viewer.sql.lines().count().saturating_sub(1);
                        viewer.scroll = viewer.scroll.saturating_add(1).min(max);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if let Some(ref mut viewer) = app.ddl_viewer {
                        viewer.scroll = viewer.scroll.saturating_sub(1);
                    }
                }
                KeyCode::Char('y') => {
                    if let Some(ref viewer) = app.ddl_viewer {
                        if let Some(ref mut clip) = global_ui.clipboard {
                            let _ = clip.set_text(viewer.sql.clone());
                        }
                        let action =
                            app::Action::SetTransient("DDL copied to clipboard".to_string(), false);
                        app.update(&action);
                    }
                }
                _ => {}
            }
            return;
        }
        Some(app::Overlay::Bookmarks) => {
            if let Some(action) = global_ui.bookmarks.handle_key(key) {
                app.update(&action);
                dispatch_action_to_db(active_idx, &action, app, global_ui);
            }
            return;
        }
        Some(app::Overlay::GoToObject) => {
            if let Some(ref mut goto) = global_ui.goto_object {
                let active_db_path = app.databases[active_idx].path.clone();
                if let Some(action) = goto.handle_key(key, &app.databases, &active_db_path) {
                    match &action {
                        app::Action::GoToObject(obj_ref) => {
                            let obj_ref_clone = obj_ref.clone();
                            app.active_overlay = None;
                            global_ui.goto_object = None;
                            app.update(&action);
                            // After switching database, reveal_and_select on the target db
                            let target_idx = app.active_db;
                            let db = &mut app.databases[target_idx];
                            db.schema
                                .reveal_and_select(&obj_ref_clone.name, obj_ref_clone.kind);
                            // Ensure sidebar is visible so user can see the selection
                            if !db.sidebar_visible {
                                db.sidebar_visible = true;
                            }
                            db.focus = PanelId::Schema;
                        }
                        app::Action::OpenGoToObject => {
                            // Toggle off (Esc or Ctrl+P)
                            app.active_overlay = None;
                            global_ui.goto_object = None;
                        }
                        _ => {
                            app.update(&action);
                            dispatch_action_to_db(active_idx, &action, app, global_ui);
                        }
                    }
                }
            }
            return;
        }
        None => {}
    }

    // Route to focused component first
    let focused = app.active_db().focus;
    let component_action = route_key_to_component(key, focused, app);

    let action = component_action.or_else(|| event::map_global_key(key));
    if let Some(ref action) = action {
        app.update(action);
        dispatch_action_to_db(app.active_db, action, app, global_ui);
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
        let db = &mut app.databases[app.active_db];
        let schema = &db.schema_cache;
        if db.editor.autocomplete_popup.is_some() {
            db.editor.refresh_autocomplete(schema);
        } else if db.editor.autocomplete_enabled() {
            // Auto-trigger: open the popup when enabled and the user types
            // enough characters to meet the min_chars threshold.
            db.editor.auto_trigger_autocomplete(schema);
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
    app: &mut AppState,
) -> Option<app::Action> {
    let db = app.active_db_mut();
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
            _ => match db.bottom_tab {
                BottomTab::Results => {
                    // DataEditor intercepts before ResultsTable when active
                    if db.data_editor.is_active()
                        && let Some(action) = db.data_editor.handle_key(key)
                    {
                        return Some(action);
                    }
                    db.results.handle_key(key)
                }
                BottomTab::Explain => db.explain.handle_key(key),
                BottomTab::Detail => db.record_detail.handle_key(key),
                BottomTab::ERDiagram => db.er_diagram.handle_key(key),
            },
        }
    } else {
        match focused {
            PanelId::Schema => db.schema.handle_key(key),
            PanelId::Editor => db.editor.handle_key(key),
            PanelId::Bottom => unreachable!(), // handled by outer `if focused == PanelId::Bottom`
            PanelId::DbInfo => db.db_info.handle_key(key),
            PanelId::Pragmas => db.pragmas.handle_key(key),
        }
    }
}

/// Dispatch an action to UI components and database handle.
/// Handles both component state updates and I/O triggers.
/// Routes to a specific database by index.
#[allow(clippy::too_many_lines)]
fn dispatch_action_to_db(
    db_idx: usize,
    action: &app::Action,
    app: &mut AppState,
    global_ui: &mut GlobalUi,
) {
    match action {
        app::Action::ExecuteQuery(sql, _source, source_table) => {
            if !sql.trim().is_empty() {
                app.databases[db_idx]
                    .handle
                    .execute(sql.clone(), source_table.clone());
            }
        }
        app::Action::ExecuteFilteredQuery {
            table,
            where_clause,
        } => {
            let db = &mut app.databases[db_idx];
            let sql = if where_clause.is_empty() {
                // Empty where_clause = clear filter, re-run unfiltered
                format!(
                    "SELECT * FROM {} LIMIT 100",
                    crate::components::data_editor::quote_identifier(table)
                )
            } else {
                // SAFETY: where_clause is raw user SQL input — intentionally unescaped.
                // The user is writing SQL directly; escaping would break their intent.
                format!(
                    "SELECT * FROM {} WHERE {} LIMIT 100",
                    crate::components::data_editor::quote_identifier(table),
                    where_clause
                )
            };
            db.last_executed_sql = Some(sql.clone());
            db.last_filter_query = true;
            db.handle.execute(sql, Some(table.clone()));
            db.executing = true;
        }
        app::Action::LoadColumns(table_name) => {
            app.databases[db_idx]
                .handle
                .load_columns(table_name.clone());
        }
        app::Action::PopulateEditor(sql)
        | app::Action::RecallHistory(sql)
        | app::Action::RecallBookmark(sql) => {
            app.databases[db_idx].editor.set_contents(sql);
        }
        app::Action::QueryCompleted(result) => {
            let db = &mut app.databases[db_idx];
            db.last_filter_query = false;
            db.results.set_results(result);
            // Mark explain as stale with the executed SQL
            db.explain.mark_stale(result.sql.clone());
            // Populate record detail with the first row
            if let Some((cols, vals)) = db.results.row_data(0) {
                db.record_detail.set_row(cols, vals);
            } else {
                db.record_detail.clear();
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
                app.databases[db_idx].handle.load_schema();
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
                    database_path: app.databases[db_idx].path.clone(),
                    execution_time_ms: result.execution_time.as_millis() as u64,
                    row_count: result.rows.len(),
                    error_message: None,
                    origin,
                });
            }
            // Editability detection: determine if result targets a single editable table.
            // Clear any pending deferred check — only the latest query matters.
            app.databases[db_idx].pending_edit_table = None;
            let is_fk_activation = app.databases[db_idx].pending_fk_activation;
            app.databases[db_idx].pending_fk_activation = false;
            let source_table = if result.source_table.is_some() {
                result.source_table.clone() // Tier 1: hint from ExecuteQuery
            } else {
                components::data_editor::detect_source_table(&result.sql) // Tier 2: SQL parse
            };
            if let Some(ref table) = source_table {
                let entries = &app.databases[db_idx].schema_cache.entries;
                if components::data_editor::check_view_rejection(table, entries) {
                    app.databases[db_idx].data_editor.deactivate();
                } else if let Some(cols) = app.databases[db_idx].schema_cache.get_columns(table) {
                    let pk_cols = components::data_editor::find_pk_columns(cols);
                    if pk_cols.is_empty() {
                        app.databases[db_idx].data_editor.deactivate();
                        app.transient_message = Some(app::TransientMessage {
                            text: format!("'{table}' has no primary key \u{2014} read-only"),
                            created_at: std::time::Instant::now(),
                            is_error: false,
                        });
                    } else {
                        let cols_cloned = cols.clone();
                        // Use activate_for_fk_nav when this QueryCompleted is from
                        // an FK navigation follow (signaled by pending_fk_activation flag).
                        if is_fk_activation {
                            app.databases[db_idx].data_editor.activate_for_fk_nav(
                                table.clone(),
                                pk_cols,
                                cols_cloned,
                                result.sql.clone(),
                                result.clone(),
                            );
                        } else {
                            app.databases[db_idx].data_editor.activate(
                                table.clone(),
                                pk_cols,
                                cols_cloned,
                                result.sql.clone(),
                                result.clone(),
                            );
                        }
                        // Parse FK info from CREATE TABLE SQL if not yet cached
                        if !app.databases[db_idx]
                            .schema_cache
                            .fk_info
                            .contains_key(table)
                            && let Some(entry) = app.databases[db_idx]
                                .schema_cache
                                .entries
                                .iter()
                                .find(|e| e.name == *table)
                            && let Some(ref sql) = entry.sql
                        {
                            let fks = db::DatabaseHandle::parse_foreign_keys(sql);
                            app.databases[db_idx]
                                .schema_cache
                                .fk_info
                                .insert(table.clone(), fks.clone());
                            app.databases[db_idx].data_editor.update_fk_columns(&fks);
                        } else if let Some(fks) = app.databases[db_idx]
                            .schema_cache
                            .fk_info
                            .get(table)
                            .cloned()
                        {
                            app.databases[db_idx].data_editor.update_fk_columns(&fks);
                        }
                    }
                } else {
                    // Columns not cached — defer activation until ColumnsLoaded arrives.
                    // Store both table name and activating SQL so the deferred path
                    // doesn't rely on last_executed_sql (which may change).
                    app.databases[db_idx].pending_edit_table =
                        Some((table.clone(), result.sql.clone()));
                    app.databases[db_idx].handle.load_columns(table.clone());
                    app.databases[db_idx].data_editor.deactivate();
                }
            } else {
                app.databases[db_idx].data_editor.deactivate();
            }
        }
        app::Action::QueryFailed(err) => {
            // Clear any pending deferred editability check — the query failed
            app.databases[db_idx].pending_edit_table = None;
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
            // Re-focus filter bar so the user can fix their WHERE clause
            // (only if the failing query actually came from the filter bar)
            if app.databases[db_idx].last_filter_query
                && app.databases[db_idx].results.filter_input.is_some()
            {
                app.databases[db_idx].results.filter_bar_active = true;
            }
            app.databases[db_idx].last_filter_query = false;
            // Log failed query to history (use stored SQL from ExecuteQuery)
            if let Some(ref history) = app.history_db
                && let Some(ref sql) = app.databases[db_idx].last_executed_sql
            {
                history.log_query(history::LogEntry {
                    sql: sql.clone(),
                    database_path: app.databases[db_idx].path.clone(),
                    execution_time_ms: 0,
                    row_count: 0,
                    error_message: Some(err.clone()),
                    origin: "user",
                });
            }
        }
        app::Action::SchemaLoaded(entries) => {
            let db = &mut app.databases[db_idx];
            db.schema.set_schema(entries);
            // Populate schema cache and trigger eager column loading for autocomplete
            db.schema_cache.entries.clone_from(entries);
            db.schema_cache.columns.clear();
            db.schema_cache.row_counts.clear();
            db.schema_cache.fully_loaded = false;
            let table_names: Vec<String> = entries
                .iter()
                .filter(|e| e.obj_type == "table" || e.obj_type == "view")
                .map(|e| e.name.clone())
                .collect();
            db.handle.load_all_columns(&table_names);
        }
        app::Action::ColumnsLoaded(table_name, columns) => {
            let db = &mut app.databases[db_idx];
            db.schema.set_columns(table_name, columns.clone());
            // Update schema cache for autocomplete
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
            let was_loaded = db.schema_cache.fully_loaded;
            if db.schema_cache.columns.len() >= expected {
                db.schema_cache.fully_loaded = true;
                // Build ER diagram now that all columns are loaded.
                db.er_diagram
                    .build_from_schema(&db.schema_cache.entries, &db.schema_cache.columns);
                // Fire row count queries once when fully_loaded first becomes true
                if !was_loaded {
                    let table_names: Vec<String> = db
                        .schema_cache
                        .entries
                        .iter()
                        .filter(|e| e.obj_type == "table") // tables only, NOT views
                        .map(|e| e.name.clone())
                        .collect();
                    db.handle.load_row_counts(&table_names);
                }
            }
            // Check if this completes a deferred editability check
            let pending = app.databases[db_idx].pending_edit_table.clone();
            if let Some((ref pending_table, ref activating_sql)) = pending
                && pending_table == table_name
            {
                let db = &mut app.databases[db_idx];
                let pk_cols = components::data_editor::find_pk_columns(columns);
                if pk_cols.is_empty() {
                    db.data_editor.deactivate();
                } else {
                    // Use the cached result from ResultsTable (set when QueryCompleted ran)
                    let cached =
                        db.results
                            .current_result()
                            .cloned()
                            .unwrap_or_else(|| db::QueryResult {
                                columns: vec![],
                                rows: vec![],
                                execution_time: std::time::Duration::ZERO,
                                truncated: false,
                                sql: activating_sql.clone(),
                                rows_affected: 0,
                                query_kind: db::QueryKind::Select,
                                source_table: Some(table_name.clone()),
                            });
                    db.data_editor.activate(
                        table_name.clone(),
                        pk_cols,
                        columns.clone(),
                        activating_sql.clone(),
                        cached,
                    );
                    // Parse FK info from CREATE TABLE SQL if not yet cached
                    if !db.schema_cache.fk_info.contains_key(table_name)
                        && let Some(entry) = db
                            .schema_cache
                            .entries
                            .iter()
                            .find(|e| &e.name == table_name)
                        && let Some(ref sql) = entry.sql
                    {
                        let fks = db::DatabaseHandle::parse_foreign_keys(sql);
                        db.schema_cache
                            .fk_info
                            .insert(table_name.clone(), fks.clone());
                        db.data_editor.update_fk_columns(&fks);
                    }
                }
                app.databases[db_idx].pending_edit_table = None;
            }
        }
        app::Action::SwitchBottomTab(BottomTab::Detail) => {
            // Populate record detail with the currently selected row
            let db = &mut app.databases[db_idx];
            if let Some(selected) = db.results.selected_row() {
                if let Some((cols, vals)) = db.results.row_data(selected) {
                    db.record_detail.set_row(cols, vals);
                } else {
                    db.record_detail.clear();
                }
            } else {
                db.record_detail.clear();
            }
        }
        app::Action::SwitchBottomTab(BottomTab::ERDiagram) => {
            // Note: Tab key inside the ER Diagram emits this action to consume the
            // key event (preventing global CycleFocus) while cycling focused_table
            // internally. The dispatch here is a no-op when already loaded.
            // Lazy build: if schema is loaded but ER diagram hasn't been built yet.
            let db = &mut app.databases[db_idx];
            if !db.er_diagram.loaded && db.schema_cache.fully_loaded {
                db.er_diagram
                    .build_from_schema(&db.schema_cache.entries, &db.schema_cache.columns);
            }
        }
        app::Action::SwitchSubTab(SubTab::Admin) => {
            // Lazy initial load for Admin components
            let db = &mut app.databases[db_idx];
            if db.db_info.try_start_load() {
                let path = db.path.clone();
                db.handle.load_db_info(path);
            }
            if db.pragmas.try_start_load() {
                db.handle.load_pragmas();
            }
        }
        // EXPLAIN result delivery
        app::Action::ExplainCompleted(bytecode, plan) => {
            app.databases[db_idx]
                .explain
                .set_results(bytecode.clone(), plan.clone());
        }
        app::Action::ExplainFailed(err) => {
            app.databases[db_idx].explain.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // DB Info result delivery
        app::Action::DbInfoLoaded(info) => {
            app.databases[db_idx].db_info.set_info(info.clone());
        }
        app::Action::DbInfoFailed(err) => {
            app.databases[db_idx].db_info.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // Pragma result delivery
        app::Action::PragmasLoaded(entries) => {
            app.databases[db_idx].pragmas.set_pragmas(entries.clone());
        }
        app::Action::PragmasFailed(err) => {
            app.databases[db_idx].pragmas.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        app::Action::PragmaSet(name, value) => {
            app.databases[db_idx]
                .pragmas
                .confirm_edit(name, value.clone());
            app.transient_message = Some(app::TransientMessage {
                text: format!("{name} set to {value}"),
                created_at: std::time::Instant::now(),
                is_error: false,
            });
        }
        app::Action::PragmaFailed(name, err) => {
            app.databases[db_idx].pragmas.cancel_edit();
            app.transient_message = Some(app::TransientMessage {
                text: format!("{name}: {err}"),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // WAL checkpoint result delivery
        app::Action::WalCheckpointed(msg) => {
            app.databases[db_idx].db_info.set_checkpointing(false);
            app.transient_message = Some(app::TransientMessage {
                text: msg.clone(),
                created_at: std::time::Instant::now(),
                is_error: false,
            });
            // Refresh db info to update WAL frame count
            let db = &mut app.databases[db_idx];
            if db.db_info.try_start_refresh() {
                let path = db.path.clone();
                db.handle.load_db_info(path);
            }
        }
        app::Action::WalCheckpointFailed(err) => {
            app.databases[db_idx].db_info.set_checkpointing(false);
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // I/O triggers
        app::Action::RefreshDbInfo => {
            let db = &mut app.databases[db_idx];
            if db.db_info.try_start_refresh() {
                let path = db.path.clone();
                db.handle.load_db_info(path);
            }
        }
        app::Action::RefreshPragmas => {
            // Only clear the edit buffer — don't clear pragma_in_flight.
            let db = &mut app.databases[db_idx];
            db.pragmas.clear_editing();
            if db.pragmas.try_start_refresh() {
                db.handle.load_pragmas();
            }
        }
        app::Action::GenerateExplain(sql) => {
            app.databases[db_idx].explain.set_loading();
            app.databases[db_idx].handle.explain(sql.clone());
        }
        app::Action::SetPragma(name, val) => {
            app.databases[db_idx]
                .handle
                .set_pragma(name.clone(), val.clone());
        }
        app::Action::IntegrityCheck => {
            // Guard: info must be loaded, not already running an integrity check
            let info = app.databases[db_idx].db_info.info();
            if info.is_none() {
                app.transient_message = Some(app::TransientMessage {
                    text: "Database info not loaded yet".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
            } else {
                app.databases[db_idx].handle.integrity_check();
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
            app.databases[db_idx].editor.clear();
            let path = app.databases[db_idx].path.clone();
            let _ = persistence::delete_buffer(&path);
        }
        app::Action::ShowHistory => {
            if app.active_overlay == Some(app::Overlay::History) {
                if let Some(ref history_db) = app.history_db {
                    global_ui.history.set_loading();
                    history_db.request_load(
                        500,
                        global_ui.history.db_filter_value(),
                        global_ui.history.origin_filter(),
                        global_ui.history.search_text(),
                        global_ui.history.errors_only(),
                    );
                } else {
                    global_ui.history.set_unavailable();
                }
            }
        }
        app::Action::HistoryLoaded(entries) => {
            global_ui.history.set_entries(entries.clone());
        }
        app::Action::RecallAndExecute(sql) => {
            // Note: duplicates ExecuteQuery state+dispatch logic because we can't
            // recursively call dispatch_action_to_db. Keep in sync.
            let db = &mut app.databases[db_idx];
            db.editor.set_contents(sql);
            if !sql.trim().is_empty() {
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
                    global_ui.history.db_filter_value(),
                    global_ui.history.origin_filter(),
                    global_ui.history.search_text(),
                    global_ui.history.errors_only(),
                );
            }
        }
        app::Action::HistoryReloadRequested => {
            // Reload history after a confirmed delete (separate from DeleteHistoryEntry
            // to avoid re-triggering request_delete in a loop)
            if let Some(ref history_db) = app.history_db {
                history_db.request_load(
                    500,
                    global_ui.history.db_filter_value(),
                    global_ui.history.origin_filter(),
                    global_ui.history.search_text(),
                    global_ui.history.errors_only(),
                );
            }
        }
        app::Action::ShowBookmarks => {
            if app.history_db.is_none() {
                let action =
                    app::Action::SetTransient("Bookmark database unavailable".to_string(), true);
                app.update(&action);
                return;
            }
            // Only load when overlay is actually opening (update() already toggled it)
            if matches!(app.active_overlay, Some(app::Overlay::Bookmarks))
                && let Some(ref hdb) = app.history_db
            {
                let db_path = &app.databases[db_idx].path;
                hdb.load_bookmarks(Some(db_path));
            }
        }
        app::Action::SaveBookmark {
            name,
            sql,
            database_path,
        } => {
            if let Some(ref hdb) = app.history_db {
                hdb.save_bookmark(name.clone(), sql.clone(), database_path.clone());
            }
        }
        app::Action::UpdateBookmark { id, name } => {
            if let Some(ref hdb) = app.history_db {
                hdb.update_bookmark(*id, name.clone());
            }
        }
        app::Action::DeleteBookmark(id) => {
            if let Some(ref hdb) = app.history_db {
                hdb.delete_bookmark(*id);
            }
        }
        app::Action::BookmarksLoaded(entries) => {
            global_ui.bookmarks.set_entries(entries.clone());
        }
        app::Action::BookmarkReloadRequested => {
            if let Some(ref hdb) = app.history_db {
                let db_path = &app.databases[db_idx].path;
                hdb.load_bookmarks(Some(db_path));
            }
        }
        app::Action::RecallAndExecuteBookmark(sql) => {
            let db = &mut app.databases[db_idx];
            db.editor.set_contents(sql);
            if !sql.trim().is_empty() {
                db.executing = true;
                db.last_execution_source = app::ExecutionSource::FullBuffer;
                db.last_executed_sql = Some(sql.clone());
                db.handle.execute(sql.clone(), None);
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
            let db = &mut app.databases[db_idx];
            let info = db.db_info.info();
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
            } else if db.db_info.checkpointing() {
                // Already checkpointing — no-op
            } else {
                db.db_info.set_checkpointing(true);
                db.handle.wal_checkpoint();
            }
        }
        app::Action::TriggerAutocomplete => {
            let db = &mut app.databases[db_idx];
            let schema = &db.schema_cache;
            db.editor.trigger_autocomplete(schema);
        }
        app::Action::ShowExport => {
            if app.active_overlay == Some(app::Overlay::Export) {
                let db = &mut app.databases[db_idx];
                // Create the popup with current result data
                if db.results.export_data().is_some() {
                    let row_count = db.results.row_count();
                    let table_name = db.last_executed_sql.as_deref().map_or_else(
                        || "table_name".to_string(),
                        components::export::infer_table_name,
                    );
                    db.export_popup =
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
                app.databases[db_idx].export_popup = None;
            }
        }
        app::Action::CopyAllResults => {
            let db = &mut app.databases[db_idx];
            if let Some((columns, rows)) = db.results.export_data() {
                let tsv = export::format_tsv(columns, rows);
                let row_count = rows.len();
                match global_ui
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
            let db = &mut app.databases[db_idx];
            if !db.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = db.results.selected_row() else {
                return;
            };
            let col = db.results.selected_col_index();
            let Some((cols, row_vals)) = db.results.row_data(row_idx) else {
                // Row is a pending insert (beyond query-returned rows) — not yet editable
                app.transient_message = Some(app::TransientMessage {
                    text: "Pending insert rows cannot be edited \u{2014} submit first".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                return;
            };
            // Reject BLOB columns
            let col_info = db.data_editor.columns().get(col);
            if col_info.is_some_and(|c| c.col_type.to_lowercase().contains("blob")) {
                app.transient_message = Some(app::TransientMessage {
                    text: "Cannot edit BLOB columns".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                return;
            }
            let pk_cols = db.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            let value = row_vals.get(col).and_then(Option::as_deref);
            let notnull = col_info.is_some_and(|c| c.notnull);
            // Pass the full original row snapshot for ChangeLog::original field
            let _ = cols;
            let original_row: Vec<Option<String>> = row_vals.to_vec();
            db.data_editor
                .start_cell_edit(pk, row_idx, col, value, notnull, original_row);
        }
        app::Action::ConfirmCellEdit(value) => {
            app.databases[db_idx]
                .data_editor
                .confirm_edit(value.clone());
        }
        app::Action::CancelCellEdit => {
            app.databases[db_idx].data_editor.cancel_edit();
        }
        app::Action::AddRow => {
            app.databases[db_idx].data_editor.add_row();
        }
        app::Action::ToggleDeleteRow => {
            let db = &mut app.databases[db_idx];
            if !db.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = db.results.selected_row() else {
                return;
            };
            let Some((_cols, row_vals)) = db.results.row_data(row_idx) else {
                // Pending insert row — remove the insert instead of toggling delete
                let query_row_count = db.results.row_count();
                let insert_idx = row_idx.saturating_sub(query_row_count);
                db.data_editor.remove_pending_insert(insert_idx);
                return;
            };
            let pk_cols = db.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            let original: Vec<Option<String>> = row_vals.to_vec();
            db.data_editor.toggle_delete_row(&pk, &original);
        }
        app::Action::CloneRow(_) => {
            let db = &mut app.databases[db_idx];
            if !db.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = db.results.selected_row() else {
                return;
            };
            let Some((_cols, row_vals)) = db.results.row_data(row_idx) else {
                return;
            };
            let values: Vec<Option<String>> = row_vals.to_vec();
            db.data_editor.clone_row(values);
        }
        app::Action::RevertCell => {
            let db = &mut app.databases[db_idx];
            if !db.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = db.results.selected_row() else {
                return;
            };
            let col = db.results.selected_col_index();
            let Some((_cols, row_vals)) = db.results.row_data(row_idx) else {
                return;
            };
            let pk_cols = db.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            db.data_editor.revert_cell_edit(&pk, col);
        }
        app::Action::RevertRow => {
            let db = &mut app.databases[db_idx];
            if !db.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = db.results.selected_row() else {
                return;
            };
            let Some((_cols, row_vals)) = db.results.row_data(row_idx) else {
                return;
            };
            let pk_cols = db.data_editor.pk_columns();
            let pk: Vec<Option<String>> = pk_cols
                .iter()
                .map(|&i| row_vals.get(i).cloned().flatten())
                .collect();
            db.data_editor.revert_row_edit(&pk);
        }
        app::Action::RevertAll => {
            app.databases[db_idx].data_editor.revert_all_edits();
        }
        app::Action::ShowDmlPreview(_submit_enabled) => {
            let db = &mut app.databases[db_idx];
            // AppState::update() already set the overlay. Here we generate DML and store it.
            if db.data_editor.changes().is_empty() {
                app.active_overlay = None; // cancel overlay set by update()
                app.transient_message = Some(app::TransientMessage {
                    text: "No pending changes".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
            } else {
                let table = db.data_editor.source_table().unwrap_or("").to_string();
                let columns = db.data_editor.columns().to_vec();
                let pk_columns = db.data_editor.pk_columns().to_vec();
                let stmts = components::data_editor::generate_dml(
                    &table,
                    &columns,
                    &pk_columns,
                    db.data_editor.changes(),
                );
                db.data_editor.set_preview_dml(stmts);
            }
        }
        app::Action::SubmitDataEdits => {
            let db = &mut app.databases[db_idx];
            let stmts = db.data_editor.preview_dml().to_vec();
            app.active_overlay = None;
            if !stmts.is_empty() {
                db.handle.execute_transaction(stmts);
            }
        }
        app::Action::DataEditsCommitted => {
            let db = &mut app.databases[db_idx];
            // AppState::update() already cleared the overlay.
            db.data_editor.revert_all_edits();
            // Re-execute activating query to refresh results
            let activating_sql = db.data_editor.activating_query().to_string();
            let source_table = db.data_editor.source_table().map(str::to_string);
            if !activating_sql.is_empty() {
                db.handle.execute(activating_sql, source_table);
            }
            app.transient_message = Some(app::TransientMessage {
                text: "Changes committed successfully".to_string(),
                created_at: std::time::Instant::now(),
                is_error: false,
            });
        }
        app::Action::DataEditsFailed(err) => {
            // AppState::update() already cleared the overlay.
            app.transient_message = Some(app::TransientMessage {
                text: format!("Transaction failed: {err}"),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
            // Changes remain staged — user can inspect and retry
        }
        app::Action::FKLoaded(table, fks) => {
            // AppState::update() already stored the FK info in schema_cache.fk_info.
            // If DataEditor is active and targets this table, update its fk_columns.
            let db = &mut app.databases[db_idx];
            if db.data_editor.is_active() && db.data_editor.source_table() == Some(table.as_str()) {
                db.data_editor.update_fk_columns(fks);
            }
        }
        app::Action::FollowFK => {
            let db = &mut app.databases[db_idx];
            if !db.data_editor.is_active() {
                return;
            }
            let Some(row_idx) = db.results.selected_row() else {
                return;
            };
            let col = db.results.selected_col_index();
            let col_name = db.data_editor.columns().get(col).map(|c| c.name.clone());
            let Some(col_name) = col_name else {
                return;
            };
            // Look up FK info for this column
            let source_table = db
                .data_editor
                .source_table()
                .map(str::to_string)
                .unwrap_or_default();
            let fk = db
                .schema_cache
                .fk_info
                .get(&source_table)
                .and_then(|fks| fks.iter().find(|fk| fk.from_column == col_name))
                .cloned();
            let Some(fk) = fk else {
                app.transient_message = Some(app::TransientMessage {
                    text: "Not an FK column".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                return;
            };
            // Get cell value — reject NULL
            let cell_val = db
                .results
                .row_data(row_idx)
                .and_then(|(_, row)| row.get(col).cloned())
                .flatten();
            let Some(cell_val) = cell_val else {
                app.transient_message = Some(app::TransientMessage {
                    text: "NULL \u{2014} cannot follow FK".to_string(),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                return;
            };
            // Cache cursor state and current result before navigating
            let col_offset = db.results.col_offset();
            let cached = db
                .results
                .current_result()
                .cloned()
                .unwrap_or_else(|| db::QueryResult {
                    columns: vec![],
                    rows: vec![],
                    execution_time: std::time::Duration::ZERO,
                    truncated: false,
                    sql: String::new(),
                    rows_affected: 0,
                    query_kind: db::QueryKind::Select,
                    source_table: None,
                });
            db.data_editor
                .push_fk_state(cached, row_idx, col, col_offset);
            // Generate and dispatch the FK query
            let target_table = fk.to_table.clone();
            let target_col = fk.to_column;
            let quoted_val = components::data_editor::quote_literal(&cell_val);
            let quoted_table = components::data_editor::quote_identifier(&target_table);
            let quoted_col = components::data_editor::quote_identifier(&target_col);
            let sql = format!("SELECT * FROM {quoted_table} WHERE {quoted_col} = {quoted_val}");
            // Signal that the next QueryCompleted is from FK navigation
            // (so activate_for_fk_nav is used instead of activate)
            app.databases[db_idx].pending_fk_activation = true;
            let execute_action = app::Action::ExecuteQuery(
                sql,
                app::ExecutionSource::FullBuffer,
                Some(target_table),
            );
            app.update(&execute_action);
            dispatch_action_to_db(db_idx, &execute_action, app, global_ui);
        }
        app::Action::FKNavigateBack => {
            let db = &mut app.databases[db_idx];
            let Some(entry) = db.data_editor.pop_fk_state() else {
                return;
            };
            // Restore ResultsTable from the cached result
            db.results.set_results(&entry.result);
            db.results
                .restore_cursor(entry.selected_row, entry.selected_col, entry.col_offset);
            // Restore DataEditor state
            db.data_editor.restore_from_fk_entry(entry);
            // Update fk_columns if FK info is cached for the restored table
            let table = db
                .data_editor
                .source_table()
                .map(str::to_string)
                .unwrap_or_default();
            if let Some(fks) = db.schema_cache.fk_info.get(&table).cloned() {
                db.data_editor.update_fk_columns(&fks);
            }
        }
        app::Action::OpenFilePicker => {
            if app.active_overlay == Some(app::Overlay::FilePicker) {
                // Create the file picker with the active database path
                let active_path = app.databases[app.active_db].path.clone();
                global_ui.file_picker =
                    Some(components::file_picker::FilePicker::new(&active_path));
            } else {
                // Toggled off
                global_ui.file_picker = None;
            }
        }
        app::Action::OpenDatabase(path) => {
            let path_str = path.to_string_lossy().to_string();

            // Check if this database is already open (compare canonical paths)
            let canonical_new = std::fs::canonicalize(path).ok();
            let mut existing_idx = None;
            for (i, db) in app.databases.iter().enumerate() {
                if db.path == path_str {
                    existing_idx = Some(i);
                    break;
                }
                if let Some(ref cn) = canonical_new
                    && db.path != ":memory:"
                    && let Ok(ce) = std::fs::canonicalize(&db.path)
                    && ce == *cn
                {
                    existing_idx = Some(i);
                    break;
                }
            }

            if let Some(idx) = existing_idx {
                // Already open — switch to that tab
                let switch_action = app::Action::SwitchDatabase(idx);
                app.update(&switch_action);
                app.transient_message = Some(app::TransientMessage {
                    text: format!("Switched to already-open database: {path_str}"),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                // Dismiss picker on success
                app.active_overlay = None;
                global_ui.file_picker = None;
            } else {
                // Check if path doesn't exist yet (SQLite will create it)
                let is_new = !path.exists();
                // Open the database (blocking async in current tokio context)
                match tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(DatabaseHandle::open(&path_str))
                }) {
                    Ok(handle) => {
                        let new_db = DatabaseContext::new(handle, path_str.clone(), &app.config);
                        app.databases.push(new_db);
                        let new_idx = app.databases.len() - 1;
                        // Switch to the new tab
                        let switch = app::Action::SwitchDatabase(new_idx);
                        app.update(&switch);
                        // Trigger schema load
                        app.databases[new_idx].handle.load_schema();
                        // Restore saved editor buffer if available
                        if let Some(saved) = persistence::load_buffer(&path_str)
                            && !saved.is_empty()
                        {
                            app.databases[new_idx].editor.set_contents(&saved);
                            app.databases[new_idx].editor.mark_saved();
                        }
                        let msg = if is_new {
                            format!("Created new database: {path_str}")
                        } else {
                            format!("Opened: {path_str}")
                        };
                        app.transient_message = Some(app::TransientMessage {
                            text: msg,
                            created_at: std::time::Instant::now(),
                            is_error: false,
                        });
                        // Dismiss picker on success
                        app.active_overlay = None;
                        global_ui.file_picker = None;
                    }
                    Err(e) => {
                        // Keep picker open on failure so user can correct the path
                        app.transient_message = Some(app::TransientMessage {
                            text: format!("Failed to open '{path_str}': {e}"),
                            created_at: std::time::Instant::now(),
                            is_error: true,
                        });
                    }
                }
            }
        }
        app::Action::OpenGoToObject => {
            if app.active_overlay == Some(app::Overlay::GoToObject) {
                // Create the Go to Object popup with all databases' schemas
                let active_path = app.databases[app.active_db].path.clone();
                global_ui.goto_object = Some(components::goto_object::GoToObject::new(
                    &app.databases,
                    &active_path,
                ));
            } else {
                // Toggled off
                global_ui.goto_object = None;
            }
        }
        // GoToObject dispatch is handled in handle_key_event overlay routing
        // (reveal_and_select is called there after the database switch).
        // CloseActiveDatabase: auto-save + removal handled entirely in
        // AppState::update_for_db (before the Vec entry is removed).
        _ => {}
    }
}

/// Build disambiguated tab labels when duplicate filenames exist.
fn build_tab_labels(databases: &[DatabaseContext]) -> Vec<String> {
    let labels: Vec<String> = databases.iter().map(|db| db.label.clone()).collect();
    let mut result = labels.clone();

    for (i, label) in labels.iter().enumerate() {
        // Check if this label appears more than once
        let count = labels.iter().filter(|l| *l == label).count();
        if count > 1 {
            if label == "[in-memory]" {
                // Disambiguate :memory: databases with sequential index
                let nth = labels[..=i].iter().filter(|l| *l == label).count();
                result[i] = format!("[in-memory] #{nth}");
            } else {
                // Disambiguate file-backed databases with parent directory
                let path = std::path::Path::new(&databases[i].path);
                if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
                    result[i] = format!("{} [{}/]", label, parent.to_string_lossy());
                }
            }
        }
    }
    result
}

#[allow(clippy::too_many_lines)]
fn render_ui(frame: &mut Frame, app: &mut AppState, global_ui: &mut GlobalUi) {
    // Copy theme to avoid holding a borrow on app while we mutate databases
    let theme = app.theme;
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

    let multi_db = app.databases.len() > 1;
    let active_idx = app.active_db;
    let active_overlay = app.active_overlay;
    let help_scroll = app.help_scroll;

    // Top level layout depends on whether we have multiple databases
    let (db_tabs_area, sub_tabs_area, content_area, status_area) = if multi_db {
        let [db_tabs, sub_tabs, content, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        (Some(db_tabs), sub_tabs, content, status)
    } else {
        let [sub_tabs, content, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        (None, sub_tabs, content, status)
    };

    // Database tab bar (only when multiple databases open)
    if let Some(db_tabs_area) = db_tabs_area {
        let tab_labels = build_tab_labels(&app.databases);
        let mut tab_items: Vec<String> = tab_labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                if i == active_idx {
                    format!(" \u{25c6} {label} ")
                } else {
                    format!(" {label} ")
                }
            })
            .collect();
        tab_items.push(" [+] ".to_string());

        let db_tabs = Tabs::new(tab_items)
            .select(active_idx)
            .style(Style::default().fg(theme.dim).bg(theme.mantle))
            .highlight_style(
                Style::default()
                    .fg(theme.accent2)
                    .bg(theme.mantle)
                    .add_modifier(Modifier::BOLD),
            )
            .divider("");
        frame.render_widget(db_tabs, db_tabs_area);
    }

    // Sub-tab bar — surface0 background, accent underlined active tab
    let sub_tab_index = match app.databases[active_idx].sub_tab {
        SubTab::Query => 0,
        SubTab::Admin => 1,
    };
    let sub_tabs = Tabs::new(vec![" Query ", " Admin "])
        .select(sub_tab_index)
        .style(Style::default().fg(theme.dim).bg(theme.surface0))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .bg(theme.surface0)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::styled(
            " \u{2502} ",
            Style::default().fg(theme.border),
        ));
    frame.render_widget(sub_tabs, sub_tabs_area);

    // Content area — needs &mut access to database for component rendering
    let sub_tab = app.databases[active_idx].sub_tab;
    let focus = app.databases[active_idx].focus;
    let sidebar_visible = app.databases[active_idx].sidebar_visible;
    let bottom_tab = app.databases[active_idx].bottom_tab;
    match sub_tab {
        SubTab::Query => {
            render_query_tab(
                frame,
                &theme,
                content_area,
                focus,
                sidebar_visible,
                bottom_tab,
                &mut app.databases[active_idx],
            );
        }
        SubTab::Admin => {
            render_admin_tab(
                frame,
                &theme,
                content_area,
                focus,
                &mut app.databases[active_idx],
            );
        }
    }

    // Status bar — needs &AppState for read access
    let de_status = app.databases[active_idx].data_editor.status();
    let selected_row = app.databases[active_idx].results.selected_row();
    let row_count = app.databases[active_idx].results.row_count();
    components::status_bar::render(
        frame,
        status_area,
        app,
        selected_row,
        row_count,
        &theme,
        &de_status,
    );

    // JSON overlay (renders on top of everything except help)
    if app.databases[active_idx].bottom_tab == BottomTab::Detail
        && app.databases[active_idx].record_detail.has_overlay()
    {
        app.databases[active_idx]
            .record_detail
            .render_overlay(frame, area, &theme);
    }

    // History overlay
    if active_overlay == Some(app::Overlay::History) {
        global_ui.history.render(frame, area, &theme);
    }

    // Bookmarks overlay
    if active_overlay == Some(app::Overlay::Bookmarks) {
        let db = &app.databases[active_idx];
        global_ui
            .bookmarks
            .set_editor_content(&db.editor.contents());
        global_ui.bookmarks.set_database_path(&db.path);
        let full = frame.area();
        let popup_w = full.width * 70 / 100;
        let popup_h = full.height * 80 / 100;
        let x = (full.width.saturating_sub(popup_w)) / 2;
        let y = (full.height.saturating_sub(popup_h)) / 2;
        let popup_area = Rect::new(x, y, popup_w, popup_h);
        frame.render_widget(Clear, popup_area);
        global_ui.bookmarks.render(frame, popup_area, &theme);
    }

    // Export overlay
    if active_overlay == Some(app::Overlay::Export)
        && let Some(ref popup) = app.databases[active_idx].export_popup
    {
        popup.render(frame, area, &theme);
    }

    // File picker overlay
    if active_overlay == Some(app::Overlay::FilePicker)
        && let Some(ref picker) = global_ui.file_picker
    {
        picker.render(frame, area, &theme);
    }

    // Go to Object overlay
    if active_overlay == Some(app::Overlay::GoToObject)
        && let Some(ref goto) = global_ui.goto_object
    {
        goto.render(frame, area, &theme);
    }

    // DML preview overlay
    if let Some(app::Overlay::DmlPreview { submit_enabled }) = active_overlay {
        components::dml_preview::render_dml_preview(
            frame,
            area,
            app.databases[active_idx].data_editor.preview_dml(),
            app.databases[active_idx].data_editor.preview_scroll(),
            submit_enabled,
            &theme,
        );
    }

    // DDL viewer overlay
    if active_overlay == Some(app::Overlay::DdlViewer)
        && let Some(ref viewer) = app.ddl_viewer
    {
        let full = frame.area();
        let popup_w = full.width * 70 / 100;
        let popup_h = full.height * 80 / 100;
        let x = (full.width.saturating_sub(popup_w)) / 2;
        let y = (full.height.saturating_sub(popup_h)) / 2;
        let popup_area = Rect::new(x, y, popup_w, popup_h);

        frame.render_widget(Clear, popup_area);
        let title = format!("DDL \u{2014} {}", viewer.object_name);
        let block = components::overlay_block(&title, &theme);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let lines: Vec<Line> = viewer
            .sql
            .lines()
            .map(|line| highlight::highlight_line(line, &theme))
            .collect();
        let total_lines = lines.len();
        let max_scroll = total_lines.saturating_sub(inner.height as usize);
        let effective_scroll = viewer.scroll.min(max_scroll);
        let para = Paragraph::new(lines).scroll((effective_scroll as u16, 0));
        frame.render_widget(para, inner);
    }

    // Modal cell editor overlay (renders above content, below help)
    if let Some(editor) = app.databases[active_idx].data_editor.cell_editor()
        && editor.modal
    {
        let table = app.databases[active_idx]
            .data_editor
            .source_table()
            .unwrap_or("table");
        let col_name = app.databases[active_idx]
            .data_editor
            .columns()
            .get(editor.col)
            .map_or("col", |c| c.name.as_str());
        editor.render_modal(frame, area, table, col_name, &theme);
    }

    // Help overlay (rendered last so it floats on top)
    if active_overlay == Some(app::Overlay::Help) {
        components::help::render(frame, help_scroll, &theme);
    }
}

fn render_query_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    sidebar_visible: bool,
    bottom_tab: BottomTab,
    db: &mut DatabaseContext,
) {
    if sidebar_visible {
        let [sidebar_area, main_area] = Layout::horizontal([
            Constraint::Percentage(db.sidebar_pct),
            Constraint::Percentage(100 - db.sidebar_pct),
        ])
        .areas(area);

        db.schema.set_row_counts(&db.schema_cache.row_counts);
        db.schema
            .render(frame, sidebar_area, focus == PanelId::Schema, theme);

        let [editor_area, bottom_area] = Layout::vertical([
            Constraint::Percentage(db.editor_pct),
            Constraint::Percentage(100 - db.editor_pct),
        ])
        .areas(main_area);

        db.editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        render_autocomplete_popup(frame, &db.editor, editor_area, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, db);
    } else {
        let [editor_area, bottom_area] = Layout::vertical([
            Constraint::Percentage(db.editor_pct),
            Constraint::Percentage(100 - db.editor_pct),
        ])
        .areas(area);

        db.editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        render_autocomplete_popup(frame, &db.editor, editor_area, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, db);
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
    db: &mut DatabaseContext,
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
    let bottom_tabs = Tabs::new(vec![" 1:Results ", " 2:Explain ", " 3:Detail ", " 4:ER "])
        .select(tab_index)
        .style(Style::default().fg(theme.dim).bg(theme.surface0))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .bg(theme.surface0)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::styled("\u{2502}", Style::default().fg(theme.border)));
    frame.render_widget(bottom_tabs, bottom_tabs_area);

    // Inject edit state into ResultsTable before rendering
    if db.data_editor.is_active() {
        db.results
            .set_edit_state(Some(db.data_editor.build_render_state()));
    } else {
        db.results.set_edit_state(None);
    }

    // Render the active bottom component
    let is_focused = focus == PanelId::Bottom;
    match bottom_tab {
        BottomTab::Results => {
            db.results
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::Explain => {
            db.explain
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::Detail => {
            db.record_detail
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::ERDiagram => {
            db.er_diagram
                .render(frame, bottom_content_area, is_focused, theme);
        }
    }
}

fn render_admin_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    db: &mut DatabaseContext,
) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

    db.db_info
        .render(frame, left, focus == PanelId::DbInfo, theme);
    db.pragmas
        .render(frame, right, focus == PanelId::Pragmas, theme);
}

fn execute_export(app: &mut AppState, global_ui: &mut GlobalUi) {
    let active_idx = app.active_db;
    let db = &mut app.databases[active_idx];
    let Some(ref popup) = db.export_popup else {
        return;
    };
    let Some((columns, rows)) = db.results.export_data() else {
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
            match global_ui
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
                                    "Clipboard unavailable \u{2014} saved to {fallback_path} ({e})"
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
