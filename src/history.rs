use std::sync::Arc;

use tokio::sync::mpsc;

/// A stored history entry read back from the `query_log` table.
#[derive(Debug, Clone)]
pub(crate) struct HistoryEntry {
    pub(crate) id: i64,
    pub(crate) sql: String,
    pub(crate) database_path: String,
    pub(crate) timestamp: String,
    pub(crate) execution_time_ms: Option<u64>,
    pub(crate) row_count: Option<u64>,
    pub(crate) error_message: Option<String>,
    pub(crate) origin: String,
    #[allow(dead_code)] // will be used when history panel displays params
    pub(crate) params_json: Option<String>,
}

impl HistoryEntry {
    pub(crate) fn is_error(&self) -> bool {
        self.error_message.is_some()
    }
}

/// A saved/bookmarked query.
#[derive(Debug, Clone)]
pub(crate) struct BookmarkEntry {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) sql: String,
    #[allow(dead_code)] // populated from DB for future filtering by database
    pub(crate) database_path: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

/// Data for inserting a new entry into the query log (fire-and-forget).
pub(crate) struct LogEntry {
    pub(crate) sql: String,
    pub(crate) database_path: String,
    pub(crate) execution_time_ms: u64,
    pub(crate) row_count: usize,
    pub(crate) error_message: Option<String>,
    pub(crate) origin: &'static str,
    pub(crate) params_json: Option<String>,
}

/// Messages sent from history tasks back to the main loop.
#[derive(Debug)]
pub(crate) enum HistoryMessage {
    Loaded(Vec<HistoryEntry>),
    LoadFailed(String),
    #[allow(dead_code)] // id carried for logging/debugging; mapped to HistoryReloadRequested
    Deleted(i64),
    BookmarksLoaded(Vec<BookmarkEntry>),
    #[allow(dead_code)] // id carried for logging/debugging; mapped to BookmarkReloadRequested
    BookmarkSaved(i64),
    #[allow(dead_code)] // id carried for logging/debugging; mapped to BookmarkReloadRequested
    BookmarkDeleted(i64),
    #[allow(dead_code)] // id carried for logging/debugging; mapped to BookmarkReloadRequested
    BookmarkUpdated(i64),
    BookmarkSaveFailed(String),
}

/// Persistent query history backed by a local `SQLite` database.
///
/// Follows the same channel-based pattern as [`tursotui_db::DatabaseHandle`]:
/// one `Arc<Database>` shared across spawned tasks, with results flowing
/// back through an unbounded channel.
pub(crate) struct HistoryDb {
    database: Arc<turso::Database>,
    result_tx: mpsc::UnboundedSender<HistoryMessage>,
    result_rx: mpsc::UnboundedReceiver<HistoryMessage>,
}

impl HistoryDb {
    /// Open (or create) the history database at `{config_dir}/tursotui/history.sqlite`.
    pub(crate) async fn open() -> Result<Self, String> {
        let dir = crate::config::app_config_dir()
            .ok_or_else(|| "could not determine config directory".to_string())?;

        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create config dir: {e}"))?;

        let path = dir.join("history.sqlite");
        let path_str = path.to_string_lossy();

        let database = turso::Builder::new_local(&path_str)
            .build()
            .await
            .map_err(|e| format!("failed to open history db: {e}"))?;

        let conn = database
            .connect()
            .map_err(|e| format!("failed to connect to history db: {e}"))?;
        Self::create_schema(&conn).await?;

        let (result_tx, result_rx) = mpsc::unbounded_channel();

        Ok(Self {
            database: Arc::new(database),
            result_tx,
            result_rx,
        })
    }

    /// Create the history and bookmarks schema. Idempotent (`IF NOT EXISTS`).
    async fn create_schema(conn: &turso::Connection) -> Result<(), String> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS query_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sql TEXT NOT NULL,
                database_path TEXT NOT NULL,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                execution_time_ms INTEGER,
                row_count INTEGER,
                status TEXT NOT NULL DEFAULT 'ok',
                error_message TEXT,
                origin TEXT NOT NULL DEFAULT 'user'
            )",
            (),
        )
        .await
        .map_err(|e| format!("failed to create query_log table: {e}"))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_query_log_timestamp ON query_log(timestamp DESC)",
            (),
        )
        .await
        .map_err(|e| format!("failed to create timestamp index: {e}"))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_query_log_database ON query_log(database_path)",
            (),
        )
        .await
        .map_err(|e| format!("failed to create database index: {e}"))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_query_log_sql ON query_log(sql)",
            (),
        )
        .await
        .map_err(|e| format!("failed to create sql index: {e}"))?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS bookmarks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                sql TEXT NOT NULL,
                database_path TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
            (),
        )
        .await
        .map_err(|e| format!("failed to create bookmarks table: {e}"))?;

        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_bookmarks_name_db ON bookmarks(name, database_path)",
            (),
        )
        .await
        .map_err(|e| format!("failed to create bookmarks unique index: {e}"))?;

        // Idempotent migration: add params_json column if not present.
        let mut has_params = conn
            .query(
                "SELECT COUNT(*) FROM pragma_table_info('query_log') WHERE name = 'params_json'",
                (),
            )
            .await
            .map_err(|e| format!("migration check failed: {e}"))?;
        if let Some(row) = has_params
            .next()
            .await
            .map_err(|e| format!("migration check failed: {e}"))?
        {
            let count: i64 = row
                .get(0)
                .map_err(|e| format!("migration check failed: {e}"))?;
            if count == 0 {
                conn.execute("ALTER TABLE query_log ADD COLUMN params_json TEXT", ())
                    .await
                    .map_err(|e| format!("migration failed: {e}"))?;
            }
        }

        Ok(())
    }

    /// Insert a query log entry. Fire-and-forget: errors are silently dropped.
    pub(crate) fn log_query(&self, entry: LogEntry) {
        let db = Arc::clone(&self.database);

        tokio::spawn(async move {
            let Ok(conn) = db.connect() else { return };
            let status = if entry.error_message.is_some() {
                "error"
            } else {
                "ok"
            };
            let _ = conn
                .execute(
                    "INSERT INTO query_log (sql, database_path, execution_time_ms, row_count, status, error_message, origin, params_json)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    turso::params![
                        entry.sql,
                        entry.database_path,
                        entry.execution_time_ms as i64,
                        entry.row_count as i64,
                        status,
                        entry.error_message,
                        entry.origin,
                        entry.params_json
                    ],
                )
                .await;
        });
    }

    /// Request an async load of history entries with optional filters.
    /// Results arrive via [`Self::try_recv`].
    pub(crate) fn request_load(
        &self,
        limit: usize,
        db_filter: Option<&str>,
        origin_filter: Option<&str>,
        search: Option<&str>,
        errors_only: bool,
    ) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();
        let db_filter = db_filter.map(String::from);
        let origin_filter = origin_filter.map(String::from);
        let search = search.map(String::from);

        tokio::spawn(async move {
            let msg =
                match Self::load_entries(&db, limit, db_filter, origin_filter, search, errors_only)
                    .await
                {
                    Ok(entries) => HistoryMessage::Loaded(entries),
                    Err(e) => HistoryMessage::LoadFailed(e),
                };
            let _ = tx.send(msg);
        });
    }

    /// Build and execute the filtered SELECT query.
    async fn load_entries(
        db: &turso::Database,
        limit: usize,
        db_filter: Option<String>,
        origin_filter: Option<String>,
        search: Option<String>,
        errors_only: bool,
    ) -> Result<Vec<HistoryEntry>, String> {
        let conn = db
            .connect()
            .map_err(|e| format!("history connect failed: {e}"))?;

        // Build dynamic WHERE clauses.
        // Because turso's param binding uses positional params and we have a variable
        // number of conditions, we build the SQL string with placeholders and collect
        // params into a Vec<turso::Value>.
        let mut conditions = Vec::new();
        let mut params: Vec<turso::Value> = Vec::new();

        if let Some(ref db_path) = db_filter {
            conditions.push("database_path = ?");
            params.push(turso::Value::Text(db_path.clone()));
        }
        if let Some(ref origin) = origin_filter {
            conditions.push("origin = ?");
            params.push(turso::Value::Text(origin.clone()));
        }
        if let Some(ref term) = search {
            conditions.push("sql LIKE ?");
            params.push(turso::Value::Text(format!("%{term}%")));
        }
        if errors_only {
            conditions.push("error_message IS NOT NULL");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT id, sql, database_path, timestamp, execution_time_ms, row_count, error_message, origin, params_json \
             FROM query_log{where_clause} ORDER BY id DESC LIMIT ?"
        );
        params.push(turso::Value::Integer(limit as i64));

        let mut rows = conn
            .query(&sql, params)
            .await
            .map_err(|e| format!("history query failed: {e}"))?;

        let mut entries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("row iteration: {e}"))?
        {
            entries.push(row_to_entry(&row)?);
        }

        Ok(entries)
    }

    /// Request async deletion of a single history entry by id.
    /// Result arrives via [`Self::try_recv`] as `HistoryMessage::Deleted`.
    pub(crate) fn request_delete(&self, id: i64) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let conn = db
                    .connect()
                    .map_err(|e| format!("history connect failed: {e}"))?;
                conn.execute("DELETE FROM query_log WHERE id = ?", [id])
                    .await
                    .map_err(|e| format!("history delete failed: {e}"))?;
                Ok::<_, String>(())
            }
            .await;

            if result.is_ok() {
                let _ = tx.send(HistoryMessage::Deleted(id));
            }
        });
    }

    /// Delete oldest entries exceeding `max_entries`. Runs synchronously (awaited at startup).
    pub(crate) async fn prune(&self, max_entries: usize) {
        let Ok(conn) = self.database.connect() else {
            return;
        };
        let _ = conn
            .execute(
                "DELETE FROM query_log WHERE id NOT IN (SELECT id FROM query_log ORDER BY id DESC LIMIT ?)",
                [max_entries as i64],
            )
            .await;
    }

    /// Save a bookmark. Sends `BookmarkSaved(id)` on success, `BookmarkSaveFailed(msg)` on error.
    pub(crate) fn save_bookmark(&self, name: String, sql: String, database_path: Option<String>) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let conn = db
                    .connect()
                    .map_err(|e| format!("bookmark connect failed: {e}"))?;
                conn.execute(
                    "INSERT INTO bookmarks (name, sql, database_path) VALUES (?, ?, ?)",
                    turso::params![name, sql, database_path],
                )
                .await
                .map_err(|e| format!("bookmark insert failed: {e}"))?;
                let mut rows = conn
                    .query("SELECT last_insert_rowid()", ())
                    .await
                    .map_err(|e| format!("last_insert_rowid query failed: {e}"))?;
                let row = rows
                    .next()
                    .await
                    .map_err(|e| format!("last_insert_rowid read failed: {e}"))?
                    .ok_or_else(|| "no row returned from last_insert_rowid".to_string())?;
                let id: i64 = row.get(0).map_err(|e| format!("id read failed: {e}"))?;
                Ok::<_, String>(id)
            }
            .await;

            let msg = match result {
                Ok(id) => HistoryMessage::BookmarkSaved(id),
                Err(e) => HistoryMessage::BookmarkSaveFailed(e),
            };
            let _ = tx.send(msg);
        });
    }

    /// Load bookmarks for a given database path (or all if `None`).
    /// Results arrive via [`Self::try_recv`] as `HistoryMessage::BookmarksLoaded`.
    pub(crate) fn load_bookmarks(&self, database_path: Option<&str>) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();
        let database_path = database_path.map(String::from);

        tokio::spawn(async move {
            let result = async {
                let conn = db
                    .connect()
                    .map_err(|e| format!("bookmark connect failed: {e}"))?;

                let (sql, params): (&str, Vec<turso::Value>) =
                    if let Some(ref db_path) = database_path {
                        (
                            "SELECT id, name, sql, database_path, created_at, updated_at \
                         FROM bookmarks \
                         WHERE database_path IS NULL OR database_path = ? \
                         ORDER BY name ASC",
                            vec![turso::Value::Text(db_path.clone())],
                        )
                    } else {
                        (
                            "SELECT id, name, sql, database_path, created_at, updated_at \
                         FROM bookmarks \
                         ORDER BY name ASC",
                            vec![],
                        )
                    };

                let mut rows = conn
                    .query(sql, params)
                    .await
                    .map_err(|e| format!("bookmark query failed: {e}"))?;

                let mut entries = Vec::new();
                while let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| format!("bookmark row iteration: {e}"))?
                {
                    entries.push(row_to_bookmark(&row)?);
                }
                Ok::<_, String>(entries)
            }
            .await;

            let msg = match result {
                Ok(entries) => HistoryMessage::BookmarksLoaded(entries),
                Err(e) => HistoryMessage::LoadFailed(e),
            };
            let _ = tx.send(msg);
        });
    }

    /// Delete a bookmark by id.
    /// Result arrives via [`Self::try_recv`] as `HistoryMessage::BookmarkDeleted`.
    pub(crate) fn delete_bookmark(&self, id: i64) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let conn = db
                    .connect()
                    .map_err(|e| format!("bookmark connect failed: {e}"))?;
                conn.execute("DELETE FROM bookmarks WHERE id = ?", [id])
                    .await
                    .map_err(|e| format!("bookmark delete failed: {e}"))?;
                Ok::<_, String>(())
            }
            .await;

            match result {
                Ok(()) => {
                    let _ = tx.send(HistoryMessage::BookmarkDeleted(id));
                }
                Err(e) => {
                    let _ = tx.send(HistoryMessage::BookmarkSaveFailed(format!(
                        "Delete failed: {e}"
                    )));
                }
            }
        });
    }

    /// Update a bookmark's name (and set `updated_at`).
    /// Result arrives via [`Self::try_recv`] as `HistoryMessage::BookmarkUpdated`.
    pub(crate) fn update_bookmark(&self, id: i64, name: String) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let conn = db
                    .connect()
                    .map_err(|e| format!("bookmark connect failed: {e}"))?;
                conn.execute(
                    "UPDATE bookmarks SET name = ?, updated_at = datetime('now') WHERE id = ?",
                    turso::params![name, id],
                )
                .await
                .map_err(|e| format!("bookmark update failed: {e}"))?;
                Ok::<_, String>(())
            }
            .await;

            match result {
                Ok(()) => {
                    let _ = tx.send(HistoryMessage::BookmarkUpdated(id));
                }
                Err(e) => {
                    let _ = tx.send(HistoryMessage::BookmarkSaveFailed(format!(
                        "Rename failed: {e}"
                    )));
                }
            }
        });
    }

    /// Non-blocking poll for completed history messages.
    pub(crate) fn try_recv(&mut self) -> Option<HistoryMessage> {
        self.result_rx.try_recv().ok()
    }

    /// Wait for a completed result (async, blocking).
    #[cfg(test)]
    async fn recv(&mut self) -> Option<HistoryMessage> {
        self.result_rx.recv().await
    }
}

/// Extract a `BookmarkEntry` from a query result row.
fn row_to_bookmark(row: &turso::Row) -> Result<BookmarkEntry, String> {
    let map_err = |field: &str, e: turso::Error| format!("failed to read bookmark {field}: {e}");

    let id: i64 = row.get(0).map_err(|e| map_err("id", e))?;
    let name: String = row.get(1).map_err(|e| map_err("name", e))?;
    let sql: String = row.get(2).map_err(|e| map_err("sql", e))?;
    let database_path = match row.get_value(3).map_err(|e| map_err("database_path", e))? {
        turso::Value::Text(s) => Some(s),
        _ => None,
    };
    let created_at: String = row.get(4).map_err(|e| map_err("created_at", e))?;
    let updated_at: String = row.get(5).map_err(|e| map_err("updated_at", e))?;

    Ok(BookmarkEntry {
        id,
        name,
        sql,
        database_path,
        created_at,
        updated_at,
    })
}

/// Extract a `HistoryEntry` from a query result row.
fn row_to_entry(row: &turso::Row) -> Result<HistoryEntry, String> {
    let map_err = |field: &str, e: turso::Error| format!("failed to read {field}: {e}");

    let id: i64 = row.get(0).map_err(|e| map_err("id", e))?;
    let sql: String = row.get(1).map_err(|e| map_err("sql", e))?;
    let database_path: String = row.get(2).map_err(|e| map_err("database_path", e))?;
    let timestamp: String = row.get(3).map_err(|e| map_err("timestamp", e))?;

    // Nullable integer columns: read as Value and convert manually
    let execution_time_ms = match row
        .get_value(4)
        .map_err(|e| map_err("execution_time_ms", e))?
    {
        turso::Value::Integer(n) => Some(n as u64),
        _ => None,
    };
    let row_count = match row.get_value(5).map_err(|e| map_err("row_count", e))? {
        turso::Value::Integer(n) => Some(n as u64),
        _ => None,
    };
    let error_message = match row.get_value(6).map_err(|e| map_err("error_message", e))? {
        turso::Value::Text(s) => Some(s),
        _ => None,
    };
    let origin: String = row.get(7).map_err(|e| map_err("origin", e))?;
    let params_json = match row.get_value(8).map_err(|e| map_err("params_json", e))? {
        turso::Value::Text(s) => Some(s),
        _ => None,
    };

    Ok(HistoryEntry {
        id,
        sql,
        database_path,
        timestamp,
        execution_time_ms,
        row_count,
        error_message,
        origin,
        params_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a `HistoryDb` backed by an in-memory database for testing.
    /// Uses the same `create_schema()` as production `open()` — no schema duplication.
    async fn test_history_db() -> HistoryDb {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        HistoryDb::create_schema(&conn).await.unwrap();

        let (result_tx, result_rx) = mpsc::unbounded_channel();
        HistoryDb {
            database: Arc::new(db),
            result_tx,
            result_rx,
        }
    }

    async fn recv_timeout(db: &mut HistoryDb) -> HistoryMessage {
        tokio::time::timeout(std::time::Duration::from_secs(2), db.recv())
            .await
            .expect("recv timed out after 2s")
            .expect("channel closed unexpectedly")
    }

    #[test]
    fn history_entry_is_error_when_has_error_message() {
        let entry = HistoryEntry {
            id: 1,
            sql: "SELECT 1".into(),
            database_path: "test.db".into(),
            timestamp: "2024-01-01".into(),
            execution_time_ms: Some(5),
            row_count: Some(1),
            error_message: Some("fail".into()),
            origin: "editor".into(),
            params_json: None,
        };
        assert!(entry.is_error());
    }

    #[test]
    fn history_entry_is_not_error_when_no_error_message() {
        let entry = HistoryEntry {
            id: 1,
            sql: "SELECT 1".into(),
            database_path: "test.db".into(),
            timestamp: "2024-01-01".into(),
            execution_time_ms: Some(5),
            row_count: Some(1),
            error_message: None,
            origin: "editor".into(),
            params_json: None,
        };
        assert!(!entry.is_error());
    }

    #[tokio::test]
    async fn log_and_load_round_trip() {
        let mut db = test_history_db().await;

        db.log_query(LogEntry {
            sql: "SELECT 1".into(),
            database_path: "test.db".into(),
            execution_time_ms: 5,
            row_count: 1,
            error_message: None,
            origin: "editor",
            params_json: None,
        });

        // log_query is fire-and-forget — no acknowledgement message.
        // Wait for the spawned INSERT task to complete before querying.
        // 100ms is generous; the actual INSERT takes <1ms on in-memory DBs.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        db.request_load(10, None, None, None, false);
        match recv_timeout(&mut db).await {
            HistoryMessage::Loaded(entries) => {
                assert_eq!(entries.len(), 1, "should have 1 history entry");
                assert_eq!(entries[0].sql, "SELECT 1");
                assert_eq!(entries[0].database_path, "test.db");
            }
            other => panic!("expected Loaded, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bookmark_save_load_delete_cycle() {
        let mut db = test_history_db().await;

        // Save
        db.save_bookmark(
            "my query".into(),
            "SELECT * FROM users".into(),
            Some("test.db".to_string()),
        );
        let save_msg = recv_timeout(&mut db).await;
        let bookmark_id = match save_msg {
            HistoryMessage::BookmarkSaved(id) => id,
            other => panic!("expected BookmarkSaved, got: {other:?}"),
        };

        // Load
        db.load_bookmarks(Some("test.db"));
        match recv_timeout(&mut db).await {
            HistoryMessage::BookmarksLoaded(bookmarks) => {
                assert_eq!(bookmarks.len(), 1);
                assert_eq!(bookmarks[0].name, "my query");
                assert_eq!(bookmarks[0].sql, "SELECT * FROM users");
            }
            other => panic!("expected BookmarksLoaded, got: {other:?}"),
        }

        // Delete
        db.delete_bookmark(bookmark_id);
        match recv_timeout(&mut db).await {
            HistoryMessage::BookmarkDeleted(id) => assert_eq!(id, bookmark_id),
            other => panic!("expected BookmarkDeleted, got: {other:?}"),
        }

        // Verify empty
        db.load_bookmarks(Some("test.db"));
        match recv_timeout(&mut db).await {
            HistoryMessage::BookmarksLoaded(bookmarks) => {
                assert!(
                    bookmarks.is_empty(),
                    "bookmarks should be empty after delete"
                );
            }
            other => panic!("expected BookmarksLoaded, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_entries_filters_by_search_term() {
        let mut db = test_history_db().await;

        db.log_query(LogEntry {
            sql: "SELECT * FROM users".into(),
            database_path: "test.db".into(),
            execution_time_ms: 0,
            row_count: 0,
            error_message: None,
            origin: "editor",
            params_json: None,
        });
        db.log_query(LogEntry {
            sql: "INSERT INTO orders VALUES (1)".into(),
            database_path: "test.db".into(),
            execution_time_ms: 0,
            row_count: 0,
            error_message: None,
            origin: "editor",
            params_json: None,
        });

        // log_query is fire-and-forget — no acknowledgement message.
        // Wait for the spawned INSERT task to complete before querying.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        db.request_load(10, None, None, Some("users"), false);
        match recv_timeout(&mut db).await {
            HistoryMessage::Loaded(entries) => {
                assert_eq!(entries.len(), 1, "search should filter to 1 result");
                assert!(entries[0].sql.contains("users"));
            }
            other => panic!("expected Loaded, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bookmark_update_changes_name() {
        let mut db = test_history_db().await;

        db.save_bookmark("original".into(), "SELECT 1".into(), None);
        let bookmark_id = match recv_timeout(&mut db).await {
            HistoryMessage::BookmarkSaved(id) => id,
            other => panic!("expected BookmarkSaved, got: {other:?}"),
        };

        db.update_bookmark(bookmark_id, "renamed".into());
        match recv_timeout(&mut db).await {
            HistoryMessage::BookmarkUpdated(id) => assert_eq!(id, bookmark_id),
            other => panic!("expected BookmarkUpdated, got: {other:?}"),
        }

        // Verify the name changed
        db.load_bookmarks(None);
        match recv_timeout(&mut db).await {
            HistoryMessage::BookmarksLoaded(bookmarks) => {
                assert_eq!(bookmarks.len(), 1);
                assert_eq!(bookmarks[0].name, "renamed");
            }
            other => panic!("expected BookmarksLoaded, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_delete_removes_entry() {
        let mut db = test_history_db().await;

        db.log_query(LogEntry {
            sql: "SELECT 1".into(),
            database_path: "test.db".into(),
            execution_time_ms: 0,
            row_count: 0,
            error_message: None,
            origin: "editor",
            params_json: None,
        });

        // log_query is fire-and-forget — wait for spawned INSERT
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Load to get the ID
        db.request_load(10, None, None, None, false);
        let entry_id = match recv_timeout(&mut db).await {
            HistoryMessage::Loaded(entries) => {
                assert_eq!(entries.len(), 1);
                entries[0].id
            }
            other => panic!("expected Loaded, got: {other:?}"),
        };

        // Delete
        db.request_delete(entry_id);
        match recv_timeout(&mut db).await {
            HistoryMessage::Deleted(id) => assert_eq!(id, entry_id),
            other => panic!("expected Deleted, got: {other:?}"),
        }

        // Verify empty
        db.request_load(10, None, None, None, false);
        match recv_timeout(&mut db).await {
            HistoryMessage::Loaded(entries) => {
                assert!(entries.is_empty(), "entry should be deleted");
            }
            other => panic!("expected Loaded, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn params_json_round_trips_through_log_and_load() {
        let mut db = test_history_db().await;

        let params = r#"["42","hello"]"#;
        db.log_query(LogEntry {
            sql: "SELECT * FROM t WHERE id = ?1 AND name = ?2".into(),
            database_path: "test.db".into(),
            execution_time_ms: 1,
            row_count: 1,
            error_message: None,
            origin: "user",
            params_json: Some(params.to_string()),
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        db.request_load(10, None, None, None, false);
        match recv_timeout(&mut db).await {
            HistoryMessage::Loaded(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(
                    entries[0].params_json.as_deref(),
                    Some(params),
                    "params_json should round-trip through log and load"
                );
            }
            other => panic!("expected Loaded, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn params_json_null_round_trips() {
        let mut db = test_history_db().await;

        db.log_query(LogEntry {
            sql: "SELECT 1".into(),
            database_path: "test.db".into(),
            execution_time_ms: 0,
            row_count: 1,
            error_message: None,
            origin: "user",
            params_json: None,
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        db.request_load(10, None, None, None, false);
        match recv_timeout(&mut db).await {
            HistoryMessage::Loaded(entries) => {
                assert_eq!(entries.len(), 1);
                assert!(
                    entries[0].params_json.is_none(),
                    "params_json should be None when not provided"
                );
            }
            other => panic!("expected Loaded, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn migration_is_idempotent() {
        // Create schema twice on the same connection — should not fail.
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        HistoryDb::create_schema(&conn).await.unwrap();
        // Second call must succeed even though params_json column now exists.
        HistoryDb::create_schema(&conn).await.unwrap();
    }
}
