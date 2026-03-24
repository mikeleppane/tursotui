use crate::GlobalMessage;
use crate::GlobalUi;
use crate::app::{self, AppState, BottomTab, SubTab};
use crate::components;
use crate::export;
use crate::history;
use crate::persistence;
use tursotui_db::{DatabaseHandle, QueryKind, QueryMessage, QueryResult};
use tursotui_sql::parser::parse_foreign_keys;

/// Convert a `QueryMessage` from the database worker into an `Action`.
pub(crate) fn map_query_message(msg: QueryMessage) -> app::Action {
    match msg {
        QueryMessage::Completed(result) => app::Action::QueryCompleted(result),
        QueryMessage::Failed(err) => app::Action::QueryFailed(err),
        QueryMessage::SchemaFailed(err) => app::Action::SetTransient(err, true),
        QueryMessage::TransactionFailed(err) => app::Action::DataEditsFailed(err),
        QueryMessage::SchemaLoaded(entries) => app::Action::SchemaLoaded(entries),
        QueryMessage::ColumnsLoaded(table, cols) => app::Action::ColumnsLoaded(table, cols),
        QueryMessage::ExplainCompleted(bytecode, plan) => {
            app::Action::ExplainCompleted(bytecode, plan)
        }
        QueryMessage::ExplainFailed(err) => app::Action::ExplainFailed(err),
        QueryMessage::DbInfoLoaded(info) => app::Action::DbInfoLoaded(info),
        QueryMessage::DbInfoFailed(err) => app::Action::DbInfoFailed(err),
        QueryMessage::PragmasLoaded(entries) => app::Action::PragmasLoaded(entries),
        QueryMessage::PragmasFailed(err) => app::Action::PragmasFailed(err),
        QueryMessage::PragmaSet(name, val) => app::Action::PragmaSet(name, val),
        QueryMessage::PragmaFailed(name, err) => app::Action::PragmaFailed(name, err),
        QueryMessage::WalCheckpointed(msg) => app::Action::WalCheckpointed(msg),
        QueryMessage::WalCheckpointFailed(err) => app::Action::WalCheckpointFailed(err),
        QueryMessage::IntegrityCheckCompleted(msg) => app::Action::IntegrityCheckCompleted(msg),
        QueryMessage::IntegrityCheckFailed(msg) => app::Action::IntegrityCheckFailed(msg),
        QueryMessage::TransactionCommitted => app::Action::DataEditsCommitted,
        QueryMessage::ForeignKeysLoaded(table, fks) => app::Action::FKLoaded(table, fks),
        QueryMessage::RowCount(..) | QueryMessage::CustomTypesLoaded(..) => {
            unreachable!("handled in drain loop")
        }
    }
}

/// Convert a `HistoryMessage` from the history worker into an `Action`.
pub(crate) fn map_history_message(msg: history::HistoryMessage) -> app::Action {
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

/// Dispatch an action to UI components and database handle.
/// Handles both component state updates and I/O triggers.
/// Routes to a specific database by index.
#[allow(clippy::too_many_lines)]
pub(crate) fn dispatch_action_to_db(
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
                    tursotui_sql::quoting::quote_identifier(table)
                )
            } else {
                // SAFETY: where_clause is raw user SQL input — intentionally unescaped.
                // The user is writing SQL directly; escaping would break their intent.
                format!(
                    "SELECT * FROM {} WHERE {} LIMIT 100",
                    tursotui_sql::quoting::quote_identifier(table),
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
        // PopulateEditor, RecallHistory, RecallBookmark handled by editor.update() via broadcast
        app::Action::QueryCompleted(result) => {
            let db = &mut app.databases[db_idx];
            db.last_filter_query = false;
            // Enrich source_table if not provided (Tier 2: parse from SQL)
            let mut enriched = result.clone();
            if enriched.source_table.is_none() {
                enriched.source_table = tursotui_sql::parser::detect_source_table(&enriched.sql);
            }
            db.results.set_results(&enriched);
            // Mark explain as stale with the executed SQL
            db.explain.mark_stale(result.sql.clone());
            // Populate record detail with the first row
            if let Some((cols, vals)) = db.results.row_data(0) {
                db.record_detail.set_row(cols, vals);
            } else {
                db.record_detail.clear();
            }
            // Refresh schema if the query contained DDL
            let has_ddl = matches!(result.query_kind, QueryKind::Ddl)
                || (matches!(result.query_kind, QueryKind::Batch { .. }) && {
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
                QueryKind::Ddl => "ddl",
                QueryKind::Pragma => "pragma",
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
                tursotui_sql::parser::detect_source_table(&result.sql) // Tier 2: SQL parse
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
                            let fks = parse_foreign_keys(sql);
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
            db.handle.load_custom_types();
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
                            .unwrap_or_else(|| QueryResult {
                                columns: vec![],
                                rows: vec![],
                                execution_time: std::time::Duration::ZERO,
                                truncated: false,
                                sql: activating_sql.clone(),
                                rows_affected: 0,
                                query_kind: QueryKind::Select,
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
                        let fks = parse_foreign_keys(sql);
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
        // ExplainCompleted handled by explain.update() via broadcast
        app::Action::ExplainFailed(err) => {
            app.databases[db_idx].explain.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // DbInfoLoaded handled by db_info.update() via broadcast
        app::Action::DbInfoFailed(err) => {
            app.databases[db_idx].db_info.set_loading_failed();
            app.transient_message = Some(app::TransientMessage {
                text: err.clone(),
                created_at: std::time::Instant::now(),
                is_error: true,
            });
        }
        // PragmasLoaded handled by pragmas.update() via broadcast
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
        // ConfirmCellEdit, CancelCellEdit, AddRow handled by data_editor.update() via broadcast
        // Data editor cell edit actions (requiring cross-component coordination)
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
        app::Action::CloneRow => {
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
                .unwrap_or_else(|| QueryResult {
                    columns: vec![],
                    rows: vec![],
                    execution_time: std::time::Duration::ZERO,
                    truncated: false,
                    sql: String::new(),
                    rows_affected: 0,
                    query_kind: QueryKind::Select,
                    source_table: None,
                });
            db.data_editor
                .push_fk_state(cached, row_idx, col, col_offset);
            // Generate and dispatch the FK query
            let target_table = fk.to_table.clone();
            let target_col = fk.to_column;
            let quoted_val = tursotui_sql::quoting::quote_literal(&cell_val);
            let quoted_table = tursotui_sql::quoting::quote_identifier(&target_table);
            let quoted_col = tursotui_sql::quoting::quote_identifier(&target_col);
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
            } else if global_ui.opening_paths.contains(&path_str) {
                // Already opening this path — ignore duplicate request
                app.transient_message = Some(app::TransientMessage {
                    text: format!("Already opening: {path_str}"),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
            } else {
                // Check if path doesn't exist yet (SQLite will create it)
                let is_new = !path.exists();
                global_ui.opening_paths.insert(path_str.clone());
                // Open the database asynchronously — result arrives via global_rx
                let tx = global_ui.global_tx.clone();
                let path_clone = path_str.clone();
                tokio::spawn(async move {
                    match DatabaseHandle::open(&path_clone).await {
                        Ok(handle) => {
                            let _ =
                                tx.send(GlobalMessage::DatabaseOpened(handle, path_clone, is_new));
                        }
                        Err(e) => {
                            let _ = tx
                                .send(GlobalMessage::DatabaseOpenFailed(path_clone, e.to_string()));
                        }
                    }
                });
                app.transient_message = Some(app::TransientMessage {
                    text: format!("Opening {path_str}..."),
                    created_at: std::time::Instant::now(),
                    is_error: false,
                });
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

/// Execute the export action using the current export popup configuration.
pub(crate) fn execute_export(app: &mut AppState, global_ui: &mut GlobalUi) {
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
