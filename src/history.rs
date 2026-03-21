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
    #[allow(dead_code)] // used when origin filtering lands (M8)
    pub(crate) origin: String,
}

impl HistoryEntry {
    pub(crate) fn is_error(&self) -> bool {
        self.error_message.is_some()
    }
}

/// Data for inserting a new entry into the query log (fire-and-forget).
pub(crate) struct LogEntry {
    pub(crate) sql: String,
    pub(crate) database_path: String,
    pub(crate) execution_time_ms: u64,
    pub(crate) row_count: usize,
    pub(crate) error_message: Option<String>,
    pub(crate) origin: &'static str,
}

/// Messages sent from history tasks back to the main loop.
#[derive(Debug)]
pub(crate) enum HistoryMessage {
    Loaded(Vec<HistoryEntry>),
    LoadFailed(String),
    #[allow(dead_code)] // id carried for logging/debugging; mapped to HistoryReloadRequested
    Deleted(i64),
}

/// Persistent query history backed by a local `SQLite` database.
///
/// Follows the same channel-based pattern as [`crate::db::DatabaseHandle`]:
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

        // Create schema
        let conn = database
            .connect()
            .map_err(|e| format!("failed to connect to history db: {e}"))?;

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

        let (result_tx, result_rx) = mpsc::unbounded_channel();

        Ok(Self {
            database: Arc::new(database),
            result_tx,
            result_rx,
        })
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
                    "INSERT INTO query_log (sql, database_path, execution_time_ms, row_count, status, error_message, origin)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                    turso::params![
                        entry.sql,
                        entry.database_path,
                        entry.execution_time_ms as i64,
                        entry.row_count as i64,
                        status,
                        entry.error_message,
                        entry.origin
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
            "SELECT id, sql, database_path, timestamp, execution_time_ms, row_count, error_message, origin \
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

    /// Non-blocking poll for completed history messages.
    pub(crate) fn try_recv(&mut self) -> Option<HistoryMessage> {
        self.result_rx.try_recv().ok()
    }
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

    Ok(HistoryEntry {
        id,
        sql,
        database_path,
        timestamp,
        execution_time_ms,
        row_count,
        error_message,
        origin,
    })
}
