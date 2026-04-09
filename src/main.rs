mod app;
mod autocomplete;
mod components;
mod config;
mod dispatch;
mod event;
mod export;
mod highlight;
mod history;
mod input;
mod layout;
mod mouse;
mod persistence;
mod theme;

use std::collections::HashSet;
use std::time::Duration;

use clap::Parser;
use ratatui::crossterm::event::{Event, KeyEventKind};
use tokio::sync::mpsc;

use app::{AppState, DatabaseContext, TableId};
use components::history::QueryHistoryPanel;
use tursotui_db::{DatabaseHandle, QueryMessage};

/// Result of an async database open operation.
pub(crate) enum DbOpenMessage {
    DatabaseOpened(DatabaseHandle, String, bool), // handle, path, is_new_file
    DatabaseOpenFailed(String, String),           // path, error
}

/// Terminal UI for Turso and `SQLite` databases.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to SQLite/Turso database file(s). Defaults to :memory:
    #[arg(default_value = ":memory:")]
    database: Vec<String>,
}

/// Global UI state shared across all database tabs.
pub(crate) struct GlobalFeatures {
    pub(crate) history: QueryHistoryPanel,
    pub(crate) bookmarks: components::bookmarks::BookmarkPanel,
    /// Persistent clipboard handle — kept alive for the app's lifetime so that
    /// clipboard contents survive on Linux/Wayland (arboard drops contents on Drop).
    pub(crate) clipboard: Option<arboard::Clipboard>,
    /// File picker popup (global since it opens databases, not per-db).
    pub(crate) file_picker: Option<components::file_picker::FilePicker>,
    /// Go to Object popup (global since it searches across all databases).
    pub(crate) goto_object: Option<components::goto_object::GoToObject>,
    /// Channel for receiving async database-open results.
    pub(crate) db_open_rx: mpsc::UnboundedReceiver<DbOpenMessage>,
    pub(crate) db_open_tx: mpsc::UnboundedSender<DbOpenMessage>,
    /// Paths currently being opened asynchronously (prevents double-open).
    pub(crate) opening_paths: HashSet<String>,
}

impl GlobalFeatures {
    fn new() -> Self {
        let (db_open_tx, db_open_rx) = mpsc::unbounded_channel();
        Self {
            history: QueryHistoryPanel::new(),
            bookmarks: components::bookmarks::BookmarkPanel::new(),
            clipboard: arboard::Clipboard::new().ok(),
            file_picker: None,
            goto_object: None,
            db_open_rx,
            db_open_tx,
            opening_paths: HashSet::new(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let config::ConfigLoadResult {
        config: cfg,
        error: config_err,
        was_created,
    } = config::load_config();

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

    // First-run hint — lowest priority, only shown when no other startup message exists.
    // was_created == true implies no prior session, so no saved buffer to restore either.
    if app.transient_message.is_none() && was_created {
        app.transient_message = Some(app::TransientMessage {
            text: "Press F1 for help".to_string(),
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
        use ratatui::crossterm::event::DisableMouseCapture;
        use ratatui::crossterm::execute;
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        prev_hook(info);
    }));

    let mut terminal = ratatui::init();
    if app.config.mouse.mouse_mode {
        use ratatui::crossterm::event::EnableMouseCapture;
        use ratatui::crossterm::execute;
        let _ = execute!(std::io::stdout(), EnableMouseCapture);
    }
    let result = run_loop(&mut terminal, &mut app);
    {
        use ratatui::crossterm::event::DisableMouseCapture;
        use ratatui::crossterm::execute;
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();

    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut global_ui = GlobalFeatures::new();
    global_ui
        .history
        .set_slow_threshold(app.config.performance.slow_query_ms);

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
        match event::poll_event(Duration::from_millis(16))? {
            Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                input::handle_key_event(key, app, &mut global_ui);
            }
            Some(Event::Mouse(mouse_event)) if app.config.mouse.mouse_mode => {
                mouse::handle_mouse_event(mouse_event, app, &mut global_ui);
            }
            _ => {}
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
            layout::render_ui(frame, app, &mut global_ui);
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
fn drain_async_messages(app: &mut AppState, global_ui: &mut GlobalFeatures) {
    // Step 1: collect all pending messages from ALL databases with their db_idx
    let mut pending: Vec<(usize, QueryMessage)> = Vec::new();
    for (db_idx, db) in app.databases.iter_mut().enumerate() {
        while let Some(msg) = db.handle.try_recv() {
            pending.push((db_idx, msg));
        }
    }

    // Step 2: process each, routing to the specific database
    for (db_idx, msg) in pending {
        // Handle RowCount directly (needs db_idx routing, no Action needed)
        if let QueryMessage::RowCount(ref table, count) = msg {
            app.databases[db_idx]
                .schema_cache
                .row_counts
                .insert(TableId::new(table.as_str()), count);
            continue;
        }
        // Handle CustomTypesLoaded directly (needs db_idx routing)
        if let QueryMessage::CustomTypesLoaded(ref types) = msg {
            let db = &mut app.databases[db_idx];
            db.schema_cache.custom_types.clone_from(types);
            db.schema.set_custom_types(types);
            continue;
        }
        let action = dispatch::map_query_message(msg);
        app.update_for_db(db_idx, &action);
        app.databases[db_idx].broadcast_update(&action);
        dispatch::dispatch_action_to_db(db_idx, &action, app, global_ui);
    }

    // Drain database open messages (async database opens)
    while let Ok(msg) = global_ui.db_open_rx.try_recv() {
        match msg {
            DbOpenMessage::DatabaseOpened(handle, path_str, is_new) => {
                global_ui.opening_paths.remove(&path_str);
                let new_db = DatabaseContext::new(handle, path_str.clone(), &app.config);
                app.databases.push(new_db);
                let new_idx = app.databases.len() - 1;
                let switch = app::Action::Nav(app::NavAction::SwitchDatabase(new_idx));
                app.update(&switch);
                app.databases[new_idx].handle.load_schema();
                // Restore saved editor buffer if available
                if let Some(saved) = persistence::load_buffer(&path_str)
                    && !saved.is_empty()
                {
                    app.databases[new_idx].editor.set_contents(&saved);
                    app.databases[new_idx].editor.mark_saved();
                }
                let msg_text = if is_new {
                    format!("Created new database: {path_str}")
                } else {
                    format!("Opened: {path_str}")
                };
                app.transient_message = Some(app::TransientMessage {
                    text: msg_text,
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
                // Dismiss picker on success
                app.global_overlay = None;
                global_ui.file_picker = None;
            }
            DbOpenMessage::DatabaseOpenFailed(path_str, err) => {
                global_ui.opening_paths.remove(&path_str);
                // Keep picker open on failure so user can correct the path
                app.transient_message = Some(app::TransientMessage {
                    text: format!("Failed to open '{path_str}': {err}"),
                    created_at: std::time::Instant::now(),
                    is_error: true,
                });
            }
        }
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
        let action = dispatch::map_history_message(msg);
        app.update(&action);
        app.databases[app.active_db].broadcast_update(&action);
        dispatch::dispatch_action_to_db(app.active_db, &action, app, global_ui);
    }
}
