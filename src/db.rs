use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

/// Version of the turso crate — update manually when bumping the dependency in Cargo.toml.
const TURSO_VERSION: &str = "0.6.0-pre.5";

/// Writable pragmas that can be set via `set_pragma`. Used as a whitelist for validation.
/// Note: `mmap_size` and `wal_autocheckpoint` are standard `SQLite` pragmas but not supported by
/// turso/libsql — they return "Not a valid pragma name".
const WRITABLE_PRAGMAS: &[&str] = &[
    "cache_size",
    "busy_timeout",
    "synchronous",
    "foreign_keys",
    "temp_store",
];

/// A single column definition from query results.
#[derive(Debug, Clone)]
pub(crate) struct ColumnDef {
    pub name: String,
    pub type_name: String,
}

/// Result of a completed query.
#[derive(Debug, Clone)]
pub(crate) struct QueryResult {
    pub columns: Vec<ColumnDef>,
    pub rows: Vec<Vec<turso::Value>>,
    pub execution_time: Duration,
    /// True if the result was capped at 10,000 rows.
    pub truncated: bool,
    /// The SQL statement that produced this result.
    pub sql: String,
}

/// A raw schema entry from `sqlite_schema`.
#[derive(Debug, Clone)]
pub(crate) struct SchemaEntry {
    pub obj_type: String,
    pub name: String,
    pub tbl_name: String,
    #[allow(dead_code)] // shown in SQL preview panel (later milestone)
    pub sql: Option<String>,
}

/// Column info from PRAGMA `table_info`.
#[derive(Debug, Clone)]
pub(crate) struct ColumnInfo {
    pub name: String,
    pub col_type: String,
    #[allow(dead_code)] // displayed as column flag (later milestone)
    pub notnull: bool,
    #[allow(dead_code)] // displayed in column detail (later milestone)
    pub default_value: Option<String>,
    pub pk: bool,
}

/// Database metadata from PRAGMAs and file system.
#[derive(Debug, Clone)]
pub(crate) struct DbInfo {
    pub file_path: String,
    pub file_size: Option<u64>,
    pub page_count: i64,
    pub page_size: i64,
    pub encoding: String,
    pub journal_mode: String,
    pub schema_version: i64,
    pub freelist_count: i64,
    // data_version not supported by turso/libsql
    pub turso_version: &'static str,
    pub wal_frames: Option<u64>,
}

/// A single PRAGMA entry for the dashboard.
#[derive(Debug, Clone)]
pub(crate) struct PragmaEntry {
    pub name: String,
    pub value: String,
    pub writable: bool,
    pub note: Option<String>,
}

/// Messages sent from query tasks back to the main loop.
#[derive(Debug)]
pub(crate) enum QueryMessage {
    Completed(QueryResult),
    Failed(String),
    SchemaLoaded(Vec<SchemaEntry>),
    SchemaFailed(String),
    ColumnsLoaded(String, Vec<ColumnInfo>),
    ExplainCompleted(Vec<Vec<String>>, Vec<String>),
    ExplainFailed(String),
    DbInfoLoaded(DbInfo),
    DbInfoFailed(String),
    PragmasLoaded(Vec<PragmaEntry>),
    PragmasFailed(String),
    PragmaSet(String, String),
    PragmaFailed(String, String), // (pragma_name, error_message)
    WalCheckpointed(String),
    WalCheckpointFailed(String),
}

/// Wraps an `Arc<Database>` and provides a channel for receiving query results.
/// One per open database.
pub(crate) struct DatabaseHandle {
    database: Arc<turso::Database>,
    result_rx: mpsc::UnboundedReceiver<QueryMessage>,
    result_tx: mpsc::UnboundedSender<QueryMessage>,
}

impl DatabaseHandle {
    /// Open a database at the given path.
    pub async fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let database = turso::Builder::new_local(path).build().await?;
        let (result_tx, result_rx) = mpsc::unbounded_channel();

        Ok(Self {
            database: Arc::new(database),
            result_rx,
            result_tx,
        })
    }

    /// Get a clone of the database `Arc` for spawning query tasks.
    #[allow(dead_code)]
    pub fn database(&self) -> Arc<turso::Database> {
        Arc::clone(&self.database)
    }

    /// Get a clone of the sender for spawning query tasks.
    #[allow(dead_code)]
    pub fn sender(&self) -> mpsc::UnboundedSender<QueryMessage> {
        self.result_tx.clone()
    }

    /// Create a fresh, independent connection for a query task.
    #[allow(dead_code)]
    pub fn connect(&self) -> Result<turso::Connection, Box<dyn std::error::Error>> {
        Ok(self.database.connect()?)
    }

    /// Check for completed query results (non-blocking).
    ///
    /// `Disconnected` cannot occur here because `self` holds `result_tx` — the channel
    /// stays open as long as the handle exists. Spawned tasks clone the sender via
    /// `sender()`, so even if all tasks complete, the original sender keeps the channel alive.
    pub fn try_recv(&mut self) -> Option<QueryMessage> {
        self.result_rx.try_recv().ok()
    }

    /// Execute a SQL query in the background. Results arrive via `try_recv()`.
    pub fn execute(&self, sql: String) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        // Outer task catches panics from the inner task (spec §8 requirement).
        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                let start = Instant::now();
                let result = Self::run_query(&db, &sql).await;
                let elapsed = start.elapsed();

                match result {
                    Ok((columns, rows, truncated)) => QueryMessage::Completed(QueryResult {
                        columns,
                        rows,
                        execution_time: elapsed,
                        truncated,
                        sql,
                    }),
                    Err(e) => QueryMessage::Failed(e.to_string()),
                }
            });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => QueryMessage::Failed("Internal error: query task panicked".to_string()),
            };
            let _ = tx_panic.send(msg);
        });
    }

    const MAX_ROWS: usize = 10_000;

    async fn run_query(
        db: &turso::Database,
        sql: &str,
    ) -> Result<
        (Vec<ColumnDef>, Vec<Vec<turso::Value>>, bool),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let conn = db.connect()?;
        let mut rows = conn.query(sql, ()).await?;

        let columns: Vec<ColumnDef> = rows
            .columns()
            .into_iter()
            .map(|c| ColumnDef {
                name: c.name().to_string(),
                type_name: c.decl_type().unwrap_or("").to_string(),
            })
            .collect();

        let col_count = rows.column_count();
        let mut result_rows = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows.next().await? {
            let mut values = Vec::with_capacity(col_count);
            for idx in 0..col_count {
                values.push(row.get_value(idx)?);
            }
            result_rows.push(values);
            if result_rows.len() >= Self::MAX_ROWS {
                truncated = true;
                break;
            }
        }

        Ok((columns, result_rows, truncated))
    }

    /// Load the database schema in the background.
    pub fn load_schema(&self) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                let result = Self::run_schema_load(&db).await;
                match result {
                    Ok(entries) => QueryMessage::SchemaLoaded(entries),
                    Err(e) => QueryMessage::SchemaFailed(e.to_string()),
                }
            });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => {
                    QueryMessage::SchemaFailed("Internal error: schema task panicked".to_string())
                }
            };
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_schema_load(
        db: &turso::Database,
    ) -> Result<Vec<SchemaEntry>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;
        let mut rows = conn
            .query(
                "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name",
                (),
            )
            .await?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            let obj_type: String = row.get_value(0)?.as_text().cloned().unwrap_or_default();
            let name: String = row.get_value(1)?.as_text().cloned().unwrap_or_default();
            let tbl_name: String = row.get_value(2)?.as_text().cloned().unwrap_or_default();
            let sql = row.get_value(3)?.as_text().cloned();

            if name.starts_with("sqlite_") {
                continue;
            }
            entries.push(SchemaEntry {
                obj_type,
                name,
                tbl_name,
                sql,
            });
        }
        Ok(entries)
    }

    /// Load column info for a specific table.
    pub fn load_columns(&self, table_name: String) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                let result = Self::run_column_load(&db, &table_name).await;
                match result {
                    Ok(columns) => QueryMessage::ColumnsLoaded(table_name, columns),
                    Err(e) => QueryMessage::Failed(format!(
                        "Failed to load columns for {table_name}: {e}"
                    )),
                }
            });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => {
                    QueryMessage::Failed("Internal error: column load task panicked".to_string())
                }
            };
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_column_load(
        db: &turso::Database,
        table_name: &str,
    ) -> Result<Vec<ColumnInfo>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;
        // PRAGMA doesn't support parameterized queries, so sanitize by escaping
        // single quotes (doubling them) to prevent SQL injection.
        let safe_name = table_name.replace('\'', "''");
        let mut rows = conn
            .query(&format!("PRAGMA table_info('{safe_name}')"), ())
            .await?;

        let mut columns = Vec::new();
        while let Some(row) = rows.next().await? {
            let name = row.get_value(1)?.as_text().cloned().unwrap_or_default();
            let col_type = row.get_value(2)?.as_text().cloned().unwrap_or_default();
            let notnull = row.get_value(3)?.as_integer().copied().unwrap_or(0) != 0;
            let default_value = row.get_value(4)?.as_text().cloned();
            let pk = row.get_value(5)?.as_integer().copied().unwrap_or(0) != 0;
            columns.push(ColumnInfo {
                name,
                col_type,
                notnull,
                default_value,
                pk,
            });
        }
        Ok(columns)
    }

    /// Run EXPLAIN and EXPLAIN QUERY PLAN for a SQL statement in the background.
    pub fn explain(&self, sql: String) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move { Self::run_explain(&db, &sql).await });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => {
                    QueryMessage::ExplainFailed("Internal error: explain task panicked".to_string())
                }
            };
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_explain(db: &turso::Database, sql: &str) -> QueryMessage {
        match Self::run_explain_inner(db, sql).await {
            Ok((bytecode_rows, plan_lines)) => {
                QueryMessage::ExplainCompleted(bytecode_rows, plan_lines)
            }
            Err(e) => QueryMessage::ExplainFailed(e.to_string()),
        }
    }

    async fn run_explain_inner(
        db: &turso::Database,
        sql: &str,
    ) -> Result<(Vec<Vec<String>>, Vec<String>), Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;

        // Run EXPLAIN <sql> — collects bytecode rows
        let mut rows = conn.query(&format!("EXPLAIN {sql}"), ()).await?;
        let col_count = rows.column_count();
        let mut bytecode_rows = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut cells = Vec::with_capacity(col_count);
            for idx in 0..col_count {
                cells.push(Self::value_to_string(&row.get_value(idx)?));
            }
            bytecode_rows.push(cells);
        }

        // Run EXPLAIN QUERY PLAN <sql> — collects plan lines
        let mut rows = conn.query(&format!("EXPLAIN QUERY PLAN {sql}"), ()).await?;
        let mut plan_lines = Vec::new();
        while let Some(row) = rows.next().await? {
            // EXPLAIN QUERY PLAN returns columns: id, parent, notused, detail
            // We want the detail column (index 3)
            let detail = row.get_value(3)?.as_text().cloned().unwrap_or_default();
            plan_lines.push(detail);
        }

        Ok((bytecode_rows, plan_lines))
    }

    /// Load database metadata (PRAGMAs + file system info) in the background.
    pub fn load_db_info(&self, path: String) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move { Self::run_db_info_load(&db, &path).await });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => {
                    QueryMessage::DbInfoFailed("Internal error: db info task panicked".to_string())
                }
            };
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_db_info_load(db: &turso::Database, path: &str) -> QueryMessage {
        match Self::run_db_info_load_inner(db, path).await {
            Ok(info) => QueryMessage::DbInfoLoaded(info),
            Err(e) => QueryMessage::DbInfoFailed(e.to_string()),
        }
    }

    async fn run_db_info_load_inner(
        db: &turso::Database,
        path: &str,
    ) -> Result<DbInfo, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;

        let page_count = Self::pragma_i64(&conn, "page_count").await?;
        let page_size = Self::pragma_i64(&conn, "page_size").await?;
        let encoding = Self::pragma_string(&conn, "encoding").await?;
        let journal_mode = Self::pragma_string(&conn, "journal_mode").await?;
        let schema_version = Self::pragma_i64(&conn, "schema_version").await?;
        let freelist_count = Self::pragma_i64(&conn, "freelist_count").await?;
        // data_version is not supported by turso/libsql

        let (file_size, wal_frames) = if path == ":memory:" {
            (None, None)
        } else {
            let meta = tokio::fs::metadata(path).await?;
            let file_size = Some(meta.len());

            let wal_path = format!("{path}-wal");
            let wal_frames = if page_size <= 0 {
                None
            } else {
                match tokio::fs::metadata(&wal_path).await {
                    Ok(wal_meta) => wal_meta
                        .len()
                        .checked_sub(32)
                        .map(|n| n / (page_size as u64 + 24)),
                    Err(_) => None,
                }
            };

            (file_size, wal_frames)
        };

        Ok(DbInfo {
            file_path: path.to_string(),
            file_size,
            page_count,
            page_size,
            encoding,
            journal_mode,
            schema_version,
            freelist_count,
            turso_version: TURSO_VERSION,
            wal_frames,
        })
    }

    /// Load all monitored PRAGMA values in the background.
    pub fn load_pragmas(&self) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move { Self::run_pragmas_load(&db).await });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => {
                    QueryMessage::PragmasFailed("Internal error: pragmas task panicked".to_string())
                }
            };
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_pragmas_load(db: &turso::Database) -> QueryMessage {
        match Self::run_pragmas_load_inner(db).await {
            Ok(entries) => QueryMessage::PragmasLoaded(entries),
            Err(e) => QueryMessage::PragmasFailed(e.to_string()),
        }
    }

    async fn run_pragmas_load_inner(
        db: &turso::Database,
    ) -> Result<Vec<PragmaEntry>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;

        let writable_pragmas = WRITABLE_PRAGMAS;

        // Read-only pragmas with notes
        // Note: auto_vacuum returns 0-column rows in turso/libsql, so it's excluded
        let readonly_pragmas: &[(&str, &str)] = &[
            ("journal_mode", "(run in query editor)"),
            ("page_size", "(set at creation time)"),
        ];

        let mut entries = Vec::new();

        for &name in writable_pragmas {
            let value = Self::pragma_string(&conn, name).await?;
            entries.push(PragmaEntry {
                name: name.to_string(),
                value,
                writable: true,
                note: None,
            });
        }

        for &(name, note) in readonly_pragmas {
            let value = Self::pragma_string(&conn, name).await?;
            entries.push(PragmaEntry {
                name: name.to_string(),
                value,
                writable: false,
                note: Some(note.to_string()),
            });
        }

        Ok(entries)
    }

    /// Set a PRAGMA value and read back the confirmed value.
    pub fn set_pragma(&self, name: String, value: String) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();
        let name_for_panic = name.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle =
                tokio::spawn(async move { Self::run_set_pragma(&db, &name, &value).await });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => QueryMessage::PragmaFailed(
                    name_for_panic,
                    "Internal error: set_pragma task panicked".to_string(),
                ),
            };
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_set_pragma(db: &turso::Database, name: &str, value: &str) -> QueryMessage {
        match Self::run_set_pragma_inner(db, name, value).await {
            Ok(confirmed) => QueryMessage::PragmaSet(name.to_string(), confirmed),
            Err(e) => QueryMessage::PragmaFailed(name.to_string(), e.to_string()),
        }
    }

    async fn run_set_pragma_inner(
        db: &turso::Database,
        name: &str,
        value: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // Defense-in-depth: validate name against the writable whitelist.
        // Primary validation is in PragmaDashboard, but this prevents misuse
        // if set_pragma is called from a different path in the future.
        if !WRITABLE_PRAGMAS.contains(&name) {
            return Err(format!("{name} is not a writable pragma").into());
        }

        let conn = db.connect()?;

        // Set the pragma value
        conn.execute(&format!("PRAGMA {name} = {value}"), ())
            .await?;

        // Read back to confirm
        let confirmed = Self::pragma_string(&conn, name).await?;
        Ok(confirmed)
    }

    /// Run a passive WAL checkpoint in the background.
    pub fn wal_checkpoint(&self) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move { Self::run_wal_checkpoint(&db).await });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => QueryMessage::WalCheckpointFailed(
                    "Internal error: wal_checkpoint task panicked".to_string(),
                ),
            };
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_wal_checkpoint(db: &turso::Database) -> QueryMessage {
        match Self::run_wal_checkpoint_inner(db).await {
            Ok(result_msg) => QueryMessage::WalCheckpointed(result_msg),
            Err(e) => QueryMessage::WalCheckpointFailed(e.to_string()),
        }
    }

    async fn run_wal_checkpoint_inner(
        db: &turso::Database,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;
        let mut rows = conn.query("PRAGMA wal_checkpoint(PASSIVE)", ()).await?;

        if let Some(row) = rows.next().await? {
            let busy = row.get_value(0)?.as_integer().copied().unwrap_or(0);
            let log = row.get_value(1)?.as_integer().copied().unwrap_or(0);
            let checkpointed = row.get_value(2)?.as_integer().copied().unwrap_or(0);
            Ok(format!(
                "Checkpoint complete: {checkpointed}/{log} pages checkpointed (busy={busy})"
            ))
        } else {
            Ok("Checkpoint complete".to_string())
        }
    }

    // ── Shared PRAGMA helpers ──────────────────────────────────────────

    /// Read a single PRAGMA value as an i64.
    /// Returns 0 if the pragma returns no rows or 0 columns (unsupported by turso/libsql).
    async fn pragma_i64(
        conn: &turso::Connection,
        name: &str,
    ) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        let mut rows = conn.query(&format!("PRAGMA {name}"), ()).await?;
        if rows.column_count() == 0 {
            return Ok(0);
        }
        if let Some(row) = rows.next().await? {
            Ok(row.get_value(0)?.as_integer().copied().unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    /// Read a single PRAGMA value as a String.
    /// Returns empty string if the pragma returns no rows or 0 columns (unsupported by turso/libsql).
    async fn pragma_string(
        conn: &turso::Connection,
        name: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut rows = conn.query(&format!("PRAGMA {name}"), ()).await?;
        if rows.column_count() == 0 {
            return Ok(String::new());
        }
        if let Some(row) = rows.next().await? {
            // PRAGMA values can be integer or text depending on the pragma.
            // Try text first, fall back to integer-to-string.
            let val = row.get_value(0)?;
            Ok(Self::value_to_string(&val))
        } else {
            Ok(String::new())
        }
    }

    /// Convert a `turso::Value` to a display string.
    /// Note: Null maps to empty string (not "NULL") — this is correct for PRAGMA values
    /// and EXPLAIN output where null means "no value". The results table uses its own
    /// rendering which displays SQL NULL distinctly.
    fn value_to_string(val: &turso::Value) -> String {
        match val {
            turso::Value::Null => String::new(),
            turso::Value::Integer(n) => n.to_string(),
            turso::Value::Real(f) => f.to_string(),
            turso::Value::Text(s) => s.clone(),
            turso::Value::Blob(b) => format!("[BLOB {} B]", b.len()),
        }
    }
}
