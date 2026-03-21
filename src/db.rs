use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

/// A single column definition from query results.
#[derive(Debug, Clone)]
pub(crate) struct ColumnDef {
    pub name: String,
    #[allow(dead_code)] // used by RecordDetail (M4 Task 3)
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
    #[allow(dead_code)] // used by ExplainView mark_stale (M4 Task 7)
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
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub file_path: String,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub file_size: Option<u64>,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub page_count: i64,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub page_size: i64,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub encoding: String,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub journal_mode: String,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub schema_version: i64,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub freelist_count: i64,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub data_version: i64,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub turso_version: String,
    #[allow(dead_code)] // read by DbInfoPanel (M4 Task 5)
    pub wal_frames: Option<u64>,
}

/// A single PRAGMA entry for the dashboard.
#[derive(Debug, Clone)]
pub(crate) struct PragmaEntry {
    #[allow(dead_code)] // read by PragmaDashboard (M4 Task 6)
    pub name: String,
    #[allow(dead_code)] // read by PragmaDashboard (M4 Task 6)
    pub value: String,
    #[allow(dead_code)] // read by PragmaDashboard (M4 Task 6)
    pub writable: bool,
    #[allow(dead_code)] // read by PragmaDashboard (M4 Task 6)
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
    #[allow(dead_code)] // constructed by DatabaseHandle::explain (M4 Task 2)
    ExplainCompleted(Vec<Vec<String>>, Vec<String>),
    #[allow(dead_code)] // constructed by DatabaseHandle::explain (M4 Task 2)
    ExplainFailed(String),
    #[allow(dead_code)] // constructed by DatabaseHandle::load_db_info (M4 Task 2)
    DbInfoLoaded(DbInfo),
    #[allow(dead_code)] // constructed by DatabaseHandle::load_db_info (M4 Task 2)
    DbInfoFailed(String),
    #[allow(dead_code)] // constructed by DatabaseHandle::load_pragmas (M4 Task 2)
    PragmasLoaded(Vec<PragmaEntry>),
    #[allow(dead_code)] // constructed by DatabaseHandle::load_pragmas (M4 Task 2)
    PragmasFailed(String),
    #[allow(dead_code)] // constructed by DatabaseHandle::set_pragma (M4 Task 2)
    PragmaSet(String, String),
    #[allow(dead_code)] // constructed by DatabaseHandle::set_pragma (M4 Task 2)
    PragmaFailed(String, String), // (pragma_name, error_message)
    #[allow(dead_code)] // constructed by DatabaseHandle::wal_checkpoint (M4 Task 2)
    WalCheckpointed(String),
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
}
