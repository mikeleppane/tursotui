use std::collections::HashMap;
use std::collections::HashSet;

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
        QueryMessage::IndexDetailsLoaded(table, indexes) => {
            app::Action::IndexDetailsLoaded(table, indexes)
        }
        QueryMessage::RowCount(..) | QueryMessage::CustomTypesLoaded(..) => {
            // These are handled directly in the drain loop (main.rs) and should
            // never reach here. Panic in debug builds to catch the invariant
            // violation early; degrade gracefully in release.
            debug_assert!(
                false,
                "RowCount/CustomTypesLoaded must be handled in drain loop"
            );
            app::Action::SetTransient("Internal: unexpected message route".to_string(), false)
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

/// Extract the leading column name from a WHERE clause for index hint purposes.
///
/// This is a best-effort heuristic: take the first token before any comparison
/// operator and strip identifier quoting. Handles common patterns like
/// `status = 'active'`, `id > 5`, `name LIKE '%foo%'`.
/// Returns `None` for function expressions like `LOWER(name) = 'foo'`.
fn extract_filter_column(where_clause: &str) -> Option<String> {
    let trimmed = where_clause.trim();
    let first_token = trimmed
        .split(|c: char| c.is_whitespace() || c == '=' || c == '<' || c == '>' || c == '!')
        .next()?;
    // Skip function expressions — if the token contains '(' it's not a bare column
    if first_token.contains('(') {
        return None;
    }
    let col = first_token
        .trim_matches('"')
        .trim_matches('`')
        .trim_matches('[')
        .trim_matches(']');
    if col.is_empty() {
        None
    } else {
        Some(col.to_string())
    }
}

/// Case-insensitive lookup in the index details cache.
fn get_table_indexes<'a>(
    index_details: &'a HashMap<String, Vec<tursotui_db::IndexDetail>>,
    table: &str,
) -> Option<&'a Vec<tursotui_db::IndexDetail>> {
    index_details
        .get(table)
        .or_else(|| index_details.get(&table.to_lowercase()))
}

/// Set execution state on a `DatabaseContext` and fire the query.
///
/// Shared by `RecallAndExecute`, `RecallAndExecuteBookmark`, and any path
/// that needs to start a query execution outside of the `ExecuteQuery` action.
fn begin_execution(db: &mut app::DatabaseContext, sql: &str, source: app::ExecutionSource) {
    if !sql.trim().is_empty() {
        db.executing = true;
        db.last_execution_source = source;
        db.last_executed_sql = Some(sql.to_owned());
        db.handle.execute(sql.to_owned(), None, None);
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
        app::Action::ExecuteQuery {
            sql,
            source: _,
            source_table,
            params,
        } => {
            if !sql.trim().is_empty() {
                app.databases[db_idx].handle.execute(
                    sql.clone(),
                    source_table.clone(),
                    params.clone(),
                );
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
            db.handle.execute(sql, Some(table.clone()), None);
            db.executing = true;
            // Show a hint if the filter column is not indexed and the table is large.
            if !where_clause.is_empty()
                && let Some(col_name) = extract_filter_column(where_clause)
            {
                let is_indexed = get_table_indexes(&db.schema_cache.index_details, table)
                    .is_some_and(|indexes| {
                        indexes.iter().any(|idx| {
                            idx.columns
                                .first()
                                .is_some_and(|c| c.eq_ignore_ascii_case(&col_name))
                        })
                    });
                let row_count = db
                    .schema_cache
                    .row_counts
                    .get(&table.to_lowercase())
                    .copied()
                    .unwrap_or(0);
                if !is_indexed && row_count > 1000 {
                    app.transient_message = Some(app::TransientMessage {
                        text: format!(
                            "Column \"{col_name}\" is not indexed \u{2014} query may be slow on large tables"
                        ),
                        created_at: std::time::Instant::now(),
                        is_error: false,
                    });
                }
            }
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
            // Populate index indicators: mark leading-key columns for the source table.
            let leading_cols: HashSet<String> = enriched
                .source_table
                .as_deref()
                .and_then(|t| get_table_indexes(&db.schema_cache.index_details, t))
                .map(|indexes| {
                    indexes
                        .iter()
                        .filter_map(|idx| idx.columns.first().cloned())
                        .collect()
                })
                .unwrap_or_default();
            db.results.set_indexed_columns(leading_cols);
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
                    params_json: app.databases[db_idx].last_executed_params_json.clone(),
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
            app.databases[db_idx].pending_fk_activation = false;
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
                    params_json: app.databases[db_idx].last_executed_params_json.clone(),
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
            db.schema_cache.index_details.clear();
            db.schema_cache.fully_loaded = false;
            let table_names: Vec<String> = entries
                .iter()
                .filter(|e| e.obj_type == "table" || e.obj_type == "view")
                .map(|e| e.name.clone())
                .collect();
            db.handle.load_all_columns(&table_names);
            db.handle.load_custom_types();
            // Trigger index loading for each table (indexes only exist on tables, not views)
            for entry in entries.iter().filter(|e| e.obj_type == "table") {
                db.handle.load_indexes(entry.name.clone());
            }
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
        app::Action::RecallAndExecute(sql) | app::Action::RecallAndExecuteBookmark(sql) => {
            let db = &mut app.databases[db_idx];
            db.editor.set_contents(sql);
            begin_execution(db, sql, app::ExecutionSource::FullBuffer);
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
                db.handle.execute(activating_sql, source_table, None);
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
            let execute_action = app::Action::ExecuteQuery {
                sql,
                source: app::ExecutionSource::FullBuffer,
                source_table: Some(target_table),
                params: None,
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Action;
    use std::time::Duration;
    use tursotui_db::{
        ColumnDef, ColumnInfo, DbInfo, ForeignKeyInfo, IndexDetail, PragmaEntry, SchemaEntry,
    };

    fn dummy_query_result() -> QueryResult {
        QueryResult {
            columns: vec![ColumnDef {
                name: "id".to_string(),
                type_name: "INTEGER".to_string(),
            }],
            rows: vec![],
            execution_time: Duration::from_millis(1),
            truncated: false,
            sql: "SELECT 1".to_string(),
            rows_affected: 0,
            query_kind: QueryKind::Select,
            source_table: None,
        }
    }

    // ── map_query_message tests ──────────────────────────────────────

    #[test]
    fn map_query_message_completed_returns_query_completed() {
        let qr = dummy_query_result();
        let action = map_query_message(QueryMessage::Completed(qr));
        assert!(
            matches!(action, Action::QueryCompleted(_)),
            "Completed should map to QueryCompleted"
        );
    }

    #[test]
    fn map_query_message_failed_preserves_error_message() {
        let action = map_query_message(QueryMessage::Failed("timeout".into()));
        match action {
            Action::QueryFailed(msg) => assert_eq!(msg, "timeout"),
            other => panic!("expected QueryFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_schema_loaded_preserves_entries() {
        let entries = vec![SchemaEntry {
            obj_type: "table".into(),
            name: "users".into(),
            tbl_name: "users".into(),
            sql: Some("CREATE TABLE users (id INTEGER)".into()),
        }];
        let action = map_query_message(QueryMessage::SchemaLoaded(entries));
        match action {
            Action::SchemaLoaded(e) => assert_eq!(e.len(), 1),
            other => panic!("expected SchemaLoaded, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_schema_failed_becomes_set_transient() {
        let action = map_query_message(QueryMessage::SchemaFailed("no schema".into()));
        match action {
            Action::SetTransient(msg, is_error) => {
                assert_eq!(msg, "no schema");
                assert!(is_error, "schema failure should be marked as error");
            }
            other => panic!("expected SetTransient, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_transaction_failed_becomes_data_edits_failed() {
        let action = map_query_message(QueryMessage::TransactionFailed("constraint".into()));
        match action {
            Action::DataEditsFailed(msg) => assert_eq!(msg, "constraint"),
            other => panic!("expected DataEditsFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_columns_loaded_preserves_table_and_columns() {
        let cols = vec![ColumnInfo {
            name: "id".into(),
            col_type: "INTEGER".into(),
            notnull: false,
            default_value: None,
            pk: true,
        }];
        let action = map_query_message(QueryMessage::ColumnsLoaded("users".into(), cols));
        match action {
            Action::ColumnsLoaded(table, c) => {
                assert_eq!(table, "users");
                assert_eq!(c.len(), 1);
            }
            other => panic!("expected ColumnsLoaded, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_explain_completed_preserves_data() {
        let bytecode = vec![vec!["Init".to_string()]];
        let plan = vec!["SCAN users".to_string()];
        let action = map_query_message(QueryMessage::ExplainCompleted(
            bytecode.clone(),
            plan.clone(),
        ));
        match action {
            Action::ExplainCompleted(bc, pl) => {
                assert_eq!(bc, bytecode);
                assert_eq!(pl, plan);
            }
            other => panic!("expected ExplainCompleted, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_explain_failed() {
        let action = map_query_message(QueryMessage::ExplainFailed("syntax error".into()));
        match action {
            Action::ExplainFailed(msg) => assert_eq!(msg, "syntax error"),
            other => panic!("expected ExplainFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_db_info_loaded() {
        let info = DbInfo {
            file_path: ":memory:".into(),
            file_size: None,
            page_count: 1,
            page_size: 4096,
            encoding: "UTF-8".into(),
            journal_mode: "wal".into(),
            schema_version: 1,
            freelist_count: 0,
            turso_version: "0.6.0",
            wal_frames: None,
        };
        let action = map_query_message(QueryMessage::DbInfoLoaded(info));
        assert!(matches!(action, Action::DbInfoLoaded(_)));
    }

    #[test]
    fn map_query_message_db_info_failed() {
        let action = map_query_message(QueryMessage::DbInfoFailed("no access".into()));
        match action {
            Action::DbInfoFailed(msg) => assert_eq!(msg, "no access"),
            other => panic!("expected DbInfoFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_pragmas_loaded() {
        let entries = vec![PragmaEntry {
            name: "cache_size".into(),
            value: "-2000".into(),
            writable: true,
            note: None,
        }];
        let action = map_query_message(QueryMessage::PragmasLoaded(entries));
        match action {
            Action::PragmasLoaded(e) => assert_eq!(e.len(), 1),
            other => panic!("expected PragmasLoaded, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_pragmas_failed() {
        let action = map_query_message(QueryMessage::PragmasFailed("error".into()));
        match action {
            Action::PragmasFailed(msg) => assert_eq!(msg, "error"),
            other => panic!("expected PragmasFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_pragma_set_preserves_name_and_value() {
        let action = map_query_message(QueryMessage::PragmaSet("cache_size".into(), "4000".into()));
        match action {
            Action::PragmaSet(name, val) => {
                assert_eq!(name, "cache_size");
                assert_eq!(val, "4000");
            }
            other => panic!("expected PragmaSet, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_pragma_failed_preserves_name_and_error() {
        let action = map_query_message(QueryMessage::PragmaFailed(
            "cache_size".into(),
            "invalid value".into(),
        ));
        match action {
            Action::PragmaFailed(name, err) => {
                assert_eq!(name, "cache_size");
                assert_eq!(err, "invalid value");
            }
            other => panic!("expected PragmaFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_wal_checkpointed() {
        let action = map_query_message(QueryMessage::WalCheckpointed("ok".into()));
        match action {
            Action::WalCheckpointed(msg) => assert_eq!(msg, "ok"),
            other => panic!("expected WalCheckpointed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_wal_checkpoint_failed() {
        let action = map_query_message(QueryMessage::WalCheckpointFailed("busy".into()));
        match action {
            Action::WalCheckpointFailed(msg) => assert_eq!(msg, "busy"),
            other => panic!("expected WalCheckpointFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_integrity_check_completed() {
        let action = map_query_message(QueryMessage::IntegrityCheckCompleted("ok".into()));
        match action {
            Action::IntegrityCheckCompleted(msg) => assert_eq!(msg, "ok"),
            other => panic!("expected IntegrityCheckCompleted, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_integrity_check_failed() {
        let action = map_query_message(QueryMessage::IntegrityCheckFailed("corrupt".into()));
        match action {
            Action::IntegrityCheckFailed(msg) => assert_eq!(msg, "corrupt"),
            other => panic!("expected IntegrityCheckFailed, got: {other:?}"),
        }
    }

    #[test]
    fn map_query_message_transaction_committed_becomes_data_edits_committed() {
        let action = map_query_message(QueryMessage::TransactionCommitted);
        assert!(
            matches!(action, Action::DataEditsCommitted),
            "TransactionCommitted should map to DataEditsCommitted"
        );
    }

    #[test]
    fn map_query_message_fk_loaded_preserves_table_and_fks() {
        let fks = vec![ForeignKeyInfo {
            from_column: "user_id".into(),
            to_table: "users".into(),
            to_column: "id".into(),
        }];
        let action = map_query_message(QueryMessage::ForeignKeysLoaded("orders".into(), fks));
        match action {
            Action::FKLoaded(table, f) => {
                assert_eq!(table, "orders");
                assert_eq!(f.len(), 1);
            }
            other => panic!("expected FKLoaded, got: {other:?}"),
        }
    }

    // RowCount and CustomTypesLoaded share a match arm that debug_assert!(false)
    // in debug mode and degrades to SetTransient in release mode.

    #[test]
    #[cfg(not(debug_assertions))]
    fn map_query_message_row_count_falls_through_in_release() {
        let action = map_query_message(QueryMessage::RowCount("t".into(), 42));
        assert!(
            matches!(action, Action::SetTransient(_, false)),
            "RowCount should degrade to non-error SetTransient in release"
        );
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn map_query_message_custom_types_loaded_falls_through_in_release() {
        let action = map_query_message(QueryMessage::CustomTypesLoaded(vec![]));
        assert!(
            matches!(action, Action::SetTransient(_, false)),
            "CustomTypesLoaded should degrade to non-error SetTransient in release"
        );
    }

    #[test]
    #[should_panic(expected = "RowCount/CustomTypesLoaded must be handled in drain loop")]
    #[cfg(debug_assertions)]
    fn map_query_message_row_count_panics_in_debug() {
        let _ = map_query_message(QueryMessage::RowCount("t".into(), 42));
    }

    #[test]
    #[should_panic(expected = "RowCount/CustomTypesLoaded must be handled in drain loop")]
    #[cfg(debug_assertions)]
    fn map_query_message_custom_types_loaded_panics_in_debug() {
        let _ = map_query_message(QueryMessage::CustomTypesLoaded(vec![]));
    }

    #[test]
    fn map_query_message_index_details_loaded_returns_action() {
        let detail = IndexDetail {
            name: "idx".into(),
            table_name: "t".into(),
            unique: false,
            columns: vec![],
        };
        let action = map_query_message(QueryMessage::IndexDetailsLoaded("t".into(), vec![detail]));
        assert!(
            matches!(action, Action::IndexDetailsLoaded(ref table, ref indexes)
                if table == "t" && indexes.len() == 1),
            "IndexDetailsLoaded should map to Action::IndexDetailsLoaded"
        );
    }

    // ── map_history_message tests ────────────────────────────────────

    #[test]
    fn map_history_message_loaded_returns_history_loaded() {
        let action = map_history_message(history::HistoryMessage::Loaded(vec![]));
        assert!(matches!(action, Action::HistoryLoaded(_)));
    }

    #[test]
    fn map_history_message_load_failed_returns_set_transient() {
        let action = map_history_message(history::HistoryMessage::LoadFailed("db error".into()));
        match action {
            Action::SetTransient(msg, is_error) => {
                assert_eq!(msg, "db error");
                assert!(is_error);
            }
            other => panic!("expected SetTransient, got: {other:?}"),
        }
    }

    #[test]
    fn map_history_message_deleted_returns_reload_requested() {
        let action = map_history_message(history::HistoryMessage::Deleted(42));
        assert!(matches!(action, Action::HistoryReloadRequested));
    }

    #[test]
    fn map_history_message_bookmarks_loaded() {
        let action = map_history_message(history::HistoryMessage::BookmarksLoaded(vec![]));
        assert!(matches!(action, Action::BookmarksLoaded(_)));
    }

    #[test]
    fn map_history_message_bookmark_saved_returns_reload() {
        let action = map_history_message(history::HistoryMessage::BookmarkSaved(1));
        assert!(matches!(action, Action::BookmarkReloadRequested));
    }

    #[test]
    fn map_history_message_bookmark_deleted_returns_reload() {
        let action = map_history_message(history::HistoryMessage::BookmarkDeleted(1));
        assert!(matches!(action, Action::BookmarkReloadRequested));
    }

    #[test]
    fn map_history_message_bookmark_updated_returns_reload() {
        let action = map_history_message(history::HistoryMessage::BookmarkUpdated(1));
        assert!(matches!(action, Action::BookmarkReloadRequested));
    }

    #[test]
    fn map_history_message_bookmark_save_failed_returns_transient() {
        let action =
            map_history_message(history::HistoryMessage::BookmarkSaveFailed("oops".into()));
        match action {
            Action::SetTransient(msg, is_error) => {
                assert_eq!(msg, "oops");
                assert!(is_error);
            }
            other => panic!("expected SetTransient, got: {other:?}"),
        }
    }

    // ── extract_filter_column tests ──────────────────────────────────

    #[test]
    fn extract_filter_column_simple_equality() {
        assert_eq!(
            extract_filter_column("status = 'active'"),
            Some("status".to_string())
        );
    }

    #[test]
    fn extract_filter_column_greater_than() {
        assert_eq!(extract_filter_column("id > 5"), Some("id".to_string()));
    }

    #[test]
    fn extract_filter_column_like() {
        assert_eq!(
            extract_filter_column("name LIKE '%foo%'"),
            Some("name".to_string())
        );
    }

    #[test]
    fn extract_filter_column_strips_double_quotes() {
        assert_eq!(
            extract_filter_column("\"my_col\" = 1"),
            Some("my_col".to_string())
        );
    }

    #[test]
    fn extract_filter_column_strips_backticks() {
        assert_eq!(extract_filter_column("`col` = 1"), Some("col".to_string()));
    }

    #[test]
    fn extract_filter_column_empty_returns_none() {
        assert_eq!(extract_filter_column(""), None);
    }

    #[test]
    fn extract_filter_column_only_whitespace_returns_none() {
        assert_eq!(extract_filter_column("   "), None);
    }

    #[test]
    fn extract_filter_column_not_equal_operator() {
        assert_eq!(
            extract_filter_column("type != 'foo'"),
            Some("type".to_string())
        );
    }

    #[test]
    fn extract_filter_column_function_expression_returns_none() {
        assert_eq!(extract_filter_column("LOWER(name) = 'foo'"), None);
    }

    #[test]
    fn extract_filter_column_compound_where() {
        // Best-effort: returns the first column only
        assert_eq!(
            extract_filter_column("a > 5 AND b = 3"),
            Some("a".to_string())
        );
    }
}
