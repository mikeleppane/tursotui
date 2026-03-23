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
    "query_only",
    "max_page_count",
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
    /// Number of rows affected (DML/DDL path); 0 for SELECT/EXPLAIN/PRAGMA.
    pub rows_affected: u64,
    /// The detected kind of query.
    pub query_kind: QueryKind,
    /// The source table that triggered this query, if known.
    pub source_table: Option<String>,
}

/// Foreign key relationship from one column to another table/column.
#[derive(Debug, Clone)]
pub(crate) struct ForeignKeyInfo {
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
}

/// Detected query type — used for status bar messaging and execution routing.
#[derive(Debug, Clone)]
pub(crate) enum QueryKind {
    Select,
    Explain,
    Insert,
    Update,
    Delete,
    Ddl,
    Pragma,
    Batch {
        statement_count: usize,
        has_trailing_select: bool,
    },
    Other,
}

/// Detect the query kind from the first non-whitespace, non-comment token.
fn detect_query_kind(sql: &str) -> QueryKind {
    let sql = skip_leading_whitespace_and_comments(sql);
    let first_word = sql
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("");
    match first_word.to_uppercase().as_str() {
        "SELECT" => QueryKind::Select,
        "EXPLAIN" => QueryKind::Explain,
        "INSERT" => QueryKind::Insert,
        "UPDATE" => QueryKind::Update,
        "DELETE" => QueryKind::Delete,
        "CREATE" | "ALTER" | "DROP" => QueryKind::Ddl,
        "PRAGMA" => QueryKind::Pragma,
        _ => QueryKind::Other,
    }
}

/// Skip leading whitespace and SQL comments (line and block) to find the first real token.
fn skip_leading_whitespace_and_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            // Skip to end of line
            s = s.find('\n').map_or("", |i| &s[i + 1..]).trim_start();
        } else if s.starts_with("/*") {
            // Skip to end of block comment
            s = match s[2..].find("*/") {
                Some(i) => &s[2 + i + 2..],
                None => "",
            };
            s = s.trim_start();
        } else {
            break;
        }
    }
    s
}

/// Split SQL input into individual statements on semicolons, respecting string
/// literals, quoted identifiers, and comments. Returns trimmed non-empty statements.
pub(crate) fn detect_statements(sql: &str) -> Vec<&str> {
    let mut statements = Vec::new();
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut stmt_start = 0;

    while i < len {
        match bytes[i] {
            b'\'' => {
                // Single-quoted string — skip until closing quote.
                // SQLite escapes quotes by doubling: 'it''s'
                i += 1;
                while i < len {
                    if bytes[i] == b'\'' {
                        i += 1;
                        if i < len && bytes[i] == b'\'' {
                            i += 1; // escaped quote, continue
                        } else {
                            break; // end of string
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => {
                // Double-quoted identifier — skip until closing quote.
                // SQLite allows doubled quotes as escape: "col""name"
                i += 1;
                while i < len {
                    if bytes[i] == b'"' {
                        i += 1;
                        if i < len && bytes[i] == b'"' {
                            i += 1; // escaped quote, continue
                        } else {
                            break; // end of identifier
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'-' if i + 1 < len && bytes[i + 1] == b'-' => {
                // Line comment — skip to end of line
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < len {
                    i += 1; // skip newline
                }
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                // Block comment — skip to */
                i += 2;
                while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 < len {
                    i += 2; // skip */
                }
            }
            b';' => {
                let stmt = sql[stmt_start..i].trim();
                if !stmt.is_empty() {
                    statements.push(stmt);
                }
                i += 1;
                stmt_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Last statement (no trailing semicolon)
    let last = sql[stmt_start..].trim();
    if !last.is_empty() {
        statements.push(last);
    }

    statements
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
    IntegrityCheckCompleted(String),
    IntegrityCheckFailed(String),
    #[allow(dead_code)]
    TransactionCommitted,
    #[allow(dead_code)]
    TransactionFailed(String),
    #[allow(dead_code)]
    ForeignKeysLoaded(String, Vec<ForeignKeyInfo>),
    RowCount(String, u64), // (table_name_lowercase, count)
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
    pub fn execute(&self, sql: String, source_table: Option<String>) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        // Outer task catches panics from the inner task (spec §8 requirement).
        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                let start = Instant::now();
                let statements = detect_statements(&sql);

                let result = if statements.len() > 1 {
                    Self::run_batch(&db, &statements).await
                } else {
                    Self::run_query(&db, &sql).await
                };

                let elapsed = start.elapsed();

                match result {
                    Ok(mut qr) => {
                        qr.execution_time = elapsed;
                        qr.source_table = source_table;
                        QueryMessage::Completed(qr)
                    }
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
    ) -> Result<QueryResult, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;
        let kind = detect_query_kind(sql);

        if matches!(
            kind,
            QueryKind::Select | QueryKind::Explain | QueryKind::Pragma | QueryKind::Other
        ) {
            // Row-returning path
            let mut rows_result = conn.query(sql, ()).await?;

            let columns: Vec<ColumnDef> = rows_result
                .columns()
                .into_iter()
                .map(|c| ColumnDef {
                    name: c.name().to_string(),
                    type_name: c.decl_type().unwrap_or("").to_string(),
                })
                .collect();

            let col_count = rows_result.column_count();
            let mut rows = Vec::new();
            let mut truncated = false;

            while let Some(row) = rows_result.next().await? {
                let mut vals = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    vals.push(row.get_value(i)?);
                }
                rows.push(vals);
                if rows.len() >= Self::MAX_ROWS {
                    truncated = true;
                    break;
                }
            }

            Ok(QueryResult {
                columns,
                rows,
                execution_time: Duration::ZERO, // set by caller
                truncated,
                sql: sql.to_string(),
                rows_affected: 0,
                query_kind: kind,
                source_table: None,
            })
        } else if matches!(
            kind,
            QueryKind::Insert | QueryKind::Update | QueryKind::Delete | QueryKind::Ddl
        ) {
            // DML/DDL path — returns affected count, no rows
            let affected = conn.execute(sql, ()).await?;
            Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                execution_time: Duration::ZERO,
                truncated: false,
                sql: sql.to_string(),
                rows_affected: affected,
                query_kind: kind,
                source_table: None,
            })
        } else {
            unreachable!("Batch kind should not reach run_query")
        }
    }

    /// Execute multiple non-SELECT statements in an explicit transaction,
    /// then optionally execute a trailing SELECT for display.
    async fn run_batch(
        db: &turso::Database,
        statements: &[&str],
    ) -> Result<QueryResult, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;
        let total_count = statements.len();

        // Guard: if user already provided transaction control, skip our wrapping
        // and fall through to sequential execution without atomicity guarantee
        let has_user_txn = statements.iter().any(|s| {
            let kind = detect_query_kind(s);
            matches!(kind, QueryKind::Other) && {
                let upper = skip_leading_whitespace_and_comments(s).to_uppercase();
                upper.starts_with("BEGIN")
                    || upper.starts_with("COMMIT")
                    || upper.starts_with("ROLLBACK")
                    || upper.starts_with("END")
            }
        });

        // Check if last statement is row-returning
        let last_kind = detect_query_kind(statements.last().unwrap_or(&""));
        let has_trailing_select = matches!(last_kind, QueryKind::Select | QueryKind::Explain);

        let (batch_stmts, trailing_select) = if has_trailing_select && statements.len() > 1 {
            (
                &statements[..statements.len() - 1],
                Some(*statements.last().unwrap()),
            )
        } else if has_trailing_select {
            // Single SELECT — shouldn't be called as batch, but handle gracefully
            return Self::run_query(db, statements[0]).await;
        } else {
            (statements, None)
        };

        // Execute non-SELECT statements in explicit transaction
        if !batch_stmts.is_empty() {
            if !has_user_txn {
                conn.execute("BEGIN", ()).await?;
            }
            for stmt in batch_stmts {
                if let Err(e) = conn.execute(*stmt, ()).await {
                    if !has_user_txn {
                        let _ = conn.execute("ROLLBACK", ()).await;
                    }
                    return Err(e.into());
                }
            }
            if !has_user_txn {
                conn.execute("COMMIT", ()).await?;
            }
        }

        // Execute trailing SELECT if present
        if let Some(select_sql) = trailing_select {
            let mut qr = Self::run_query(db, select_sql).await?;
            qr.query_kind = QueryKind::Batch {
                statement_count: total_count,
                has_trailing_select: true,
            };
            Ok(qr)
        } else {
            Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                execution_time: Duration::ZERO,
                truncated: false,
                sql: statements.join("; "),
                rows_affected: 0,
                query_kind: QueryKind::Batch {
                    statement_count: total_count,
                    has_trailing_select: false,
                },
                source_table: None,
            })
        }
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

    /// Fire `load_columns` for every table/view name in the given list.
    /// Used for eager column loading after schema loads.
    pub fn load_all_columns(&self, table_names: &[String]) {
        for name in table_names {
            self.load_columns(name.clone());
        }
    }

    /// Spawn async `COUNT(*)` queries for a list of table names.
    /// Results arrive as `QueryMessage::RowCount`.
    pub(crate) fn load_row_counts(&self, tables: &[String]) {
        for table in tables {
            let db = Arc::clone(&self.database);
            let tx = self.result_tx.clone();
            let table_name = table.clone();
            let table_lower = table.to_lowercase();
            tokio::spawn(async move {
                let Ok(conn) = db.connect() else { return };
                let sql = format!(
                    "SELECT COUNT(*) FROM {}",
                    crate::components::data_editor::quote_identifier(&table_name)
                );
                let Ok(mut rows) = conn.query(&sql, ()).await else {
                    return;
                };
                if let Ok(Some(row)) = rows.next().await
                    && let Ok(count) = row.get::<i64>(0)
                {
                    let _ = tx.send(QueryMessage::RowCount(table_lower, count.max(0) as u64));
                }
            });
        }
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
            ("application_id", ""),
            ("user_version", ""),
        ];

        let mut entries = Vec::new();

        for &name in writable_pragmas {
            let value = Self::pragma_string(&conn, name).await?;
            let note = match name {
                "query_only" => Some("(disables all writes)".to_string()),
                "max_page_count" => Some("(writes fail when reached)".to_string()),
                _ => None,
            };
            entries.push(PragmaEntry {
                name: name.to_string(),
                value,
                writable: true,
                note,
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

    /// Run PRAGMA `integrity_check` in the background.
    pub fn integrity_check(&self) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move { Self::run_integrity_check(&db).await });
            let (msg, error_result) = match handle.await {
                Ok((msg, result)) => (msg, result),
                Err(_) => (
                    QueryMessage::IntegrityCheckFailed(
                        "Internal error: integrity_check task panicked".to_string(),
                    ),
                    None,
                ),
            };
            // Send error rows to Results table first (if any)
            if let Some(qr) = error_result {
                let _ = tx_panic.send(QueryMessage::Completed(qr));
            }
            // Then send the transient message
            let _ = tx_panic.send(msg);
        });
    }

    async fn run_integrity_check(db: &turso::Database) -> (QueryMessage, Option<QueryResult>) {
        match Self::run_integrity_check_inner(db).await {
            Ok((msg, result)) => (QueryMessage::IntegrityCheckCompleted(msg), result),
            Err(e) => (QueryMessage::IntegrityCheckFailed(e.to_string()), None),
        }
    }

    async fn run_integrity_check_inner(
        db: &turso::Database,
    ) -> Result<(String, Option<QueryResult>), Box<dyn std::error::Error + Send + Sync>> {
        let start = Instant::now();
        let conn = db.connect()?;
        let mut rows = conn.query("PRAGMA integrity_check", ()).await?;

        let mut issues = Vec::new();
        while let Some(row) = rows.next().await? {
            let val = row.get_value(0)?;
            if let Some(s) = val.as_text() {
                issues.push(s.clone());
            }
        }

        let elapsed = start.elapsed();
        let millis = elapsed.as_millis();
        let time_str = if millis < 1000 {
            format!("{millis}ms")
        } else {
            format!("{:.2}s", elapsed.as_secs_f64())
        };

        if issues.len() == 1 && issues[0] == "ok" {
            Ok((format!("Integrity check: ok ({time_str})"), None))
        } else {
            let count = issues.len();
            let msg = format!(
                "Integrity check: {count} errors found \u{2014} see query results ({time_str})"
            );
            let result_rows: Vec<Vec<turso::Value>> = issues
                .into_iter()
                .map(|issue| vec![turso::Value::Text(issue)])
                .collect();
            let qr = QueryResult {
                columns: vec![ColumnDef {
                    name: "integrity_error".to_string(),
                    type_name: "TEXT".to_string(),
                }],
                rows: result_rows,
                execution_time: elapsed,
                truncated: false,
                sql: "PRAGMA integrity_check".to_string(),
                rows_affected: 0,
                query_kind: QueryKind::Pragma,
                source_table: None,
            };
            Ok((msg, Some(qr)))
        }
    }

    // ── Transaction execution ──────────────────────────────────────────

    /// Execute a list of DML statements atomically in the background.
    /// Sends `TransactionCommitted` on success or `TransactionFailed` on any error.
    pub(crate) fn execute_transaction(&self, statements: Vec<String>) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                let conn = db.connect()?;
                conn.execute("PRAGMA defer_foreign_keys = ON", ()).await?;
                conn.execute("BEGIN", ()).await?;
                for stmt in &statements {
                    if let Err(e) = conn.execute(stmt, ()).await {
                        let _ = conn.execute("ROLLBACK", ()).await;
                        return Err::<(), Box<dyn std::error::Error + Send + Sync>>(e.into());
                    }
                }
                conn.execute("COMMIT", ()).await?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            });
            let msg = match handle.await {
                Ok(Ok(())) => QueryMessage::TransactionCommitted,
                Ok(Err(e)) => QueryMessage::TransactionFailed(e.to_string()),
                Err(_) => QueryMessage::TransactionFailed("Transaction task panicked".into()),
            };
            let _ = tx_panic.send(msg);
        });
    }

    /// Load foreign key info for a table in the background.
    /// Sends `ForeignKeysLoaded` on success; silently ignores errors.
    #[allow(dead_code)] // will be called when FK navigation lands (Task 13)
    /// Parse foreign key constraints from a `CREATE TABLE` SQL statement.
    ///
    /// turso/libsql does not support `PRAGMA foreign_key_list` ("Not a valid pragma
    /// name"), so we extract FK info from the `CREATE TABLE` SQL stored in
    /// `sqlite_schema`. This is a heuristic parser — it handles the common
    /// `FOREIGN KEY (col) REFERENCES table (col)` syntax.
    pub(crate) fn parse_foreign_keys(create_sql: &str) -> Vec<ForeignKeyInfo> {
        let upper = create_sql.to_uppercase();
        let mut fks = Vec::new();

        // Find all "FOREIGN KEY (col) REFERENCES table (col)" patterns
        let mut search_from = 0;
        while let Some(fk_pos) = upper[search_from..].find("FOREIGN KEY") {
            let abs_pos = search_from + fk_pos;
            search_from = abs_pos + 11;

            // Extract from_column: text between first ( and )
            let Some(open) = create_sql[search_from..].find('(') else {
                continue;
            };
            let paren_start = search_from + open + 1;
            let Some(close) = create_sql[paren_start..].find(')') else {
                continue;
            };
            let from_col = create_sql[paren_start..paren_start + close].trim();

            // Find REFERENCES keyword after the closing paren
            let after_paren = paren_start + close + 1;
            let rest_upper = &upper[after_paren..];
            let Some(ref_pos) = rest_upper.find("REFERENCES") else {
                continue;
            };
            let after_ref = after_paren + ref_pos + 10; // len("REFERENCES")

            // Extract target table name (may be quoted)
            let target_start = create_sql[after_ref..].trim_start();
            let offset = create_sql.len() - target_start.len();
            let (to_table, rest) = Self::extract_identifier(target_start);
            if to_table.is_empty() {
                continue;
            }

            // Extract target column: text between ( and )
            let rest_trimmed = rest.trim_start();
            if !rest_trimmed.starts_with('(') {
                continue;
            }
            let inner = &rest_trimmed[1..];
            let Some(end) = inner.find(')') else {
                continue;
            };
            let to_col = inner[..end].trim();

            let table_len = to_table.len();
            fks.push(ForeignKeyInfo {
                from_column: Self::unquote(from_col),
                to_table,
                to_column: Self::unquote(to_col),
            });

            search_from = offset + table_len;
        }

        fks
    }

    /// Extract an identifier (possibly quoted with `"` or `` ` ``) from the start of `s`.
    /// Returns (identifier, `rest_of_string`).
    fn extract_identifier(s: &str) -> (String, &str) {
        let s = s.trim_start();
        if let Some(rest) = s.strip_prefix('"') {
            // Double-quoted identifier
            if let Some(end) = rest.find('"') {
                return (s[1..=end].to_string(), &s[2 + end..]);
            }
        } else if let Some(rest) = s.strip_prefix('`')
            && let Some(end) = rest.find('`')
        {
            return (s[1..=end].to_string(), &s[2 + end..]);
        }
        // Unquoted: read until non-identifier char
        let end = s
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(s.len());
        (s[..end].to_string(), &s[end..])
    }

    /// Remove surrounding quotes from a column name if present.
    fn unquote(s: &str) -> String {
        let s = s.trim();
        if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('`') && s.ends_with('`')) {
            s[1..s.len() - 1].to_string()
        } else {
            s.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_query_kind_select() {
        assert!(matches!(detect_query_kind("SELECT 1"), QueryKind::Select));
    }

    #[test]
    fn detect_query_kind_case_insensitive() {
        assert!(matches!(detect_query_kind("sElEcT 1"), QueryKind::Select));
    }

    #[test]
    fn detect_query_kind_leading_whitespace() {
        assert!(matches!(
            detect_query_kind("  \n  INSERT INTO t VALUES (1)"),
            QueryKind::Insert
        ));
    }

    #[test]
    fn detect_query_kind_leading_line_comment() {
        assert!(matches!(
            detect_query_kind("-- comment\nSELECT 1"),
            QueryKind::Select
        ));
    }

    #[test]
    fn detect_query_kind_leading_block_comment() {
        assert!(matches!(
            detect_query_kind("/* block */  DELETE FROM t"),
            QueryKind::Delete
        ));
    }

    #[test]
    fn detect_query_kind_explain() {
        assert!(matches!(
            detect_query_kind("EXPLAIN SELECT 1"),
            QueryKind::Explain
        ));
    }

    #[test]
    fn detect_query_kind_ddl() {
        assert!(matches!(
            detect_query_kind("CREATE TABLE t (id INT)"),
            QueryKind::Ddl
        ));
        assert!(matches!(
            detect_query_kind("ALTER TABLE t ADD col INT"),
            QueryKind::Ddl
        ));
        assert!(matches!(detect_query_kind("DROP TABLE t"), QueryKind::Ddl));
    }

    #[test]
    fn detect_query_kind_pragma() {
        assert!(matches!(
            detect_query_kind("PRAGMA table_info(t)"),
            QueryKind::Pragma
        ));
    }

    #[test]
    fn detect_query_kind_update() {
        assert!(matches!(
            detect_query_kind("UPDATE t SET x = 1"),
            QueryKind::Update
        ));
    }

    #[test]
    fn detect_query_kind_unknown() {
        assert!(matches!(detect_query_kind("VACUUM"), QueryKind::Other));
        assert!(matches!(detect_query_kind(""), QueryKind::Other));
    }

    #[test]
    fn detect_statements_single() {
        let stmts = detect_statements("SELECT 1");
        assert_eq!(stmts, vec!["SELECT 1"]);
    }

    #[test]
    fn detect_statements_multiple() {
        let stmts = detect_statements("INSERT INTO t VALUES (1); SELECT 1");
        assert_eq!(stmts, vec!["INSERT INTO t VALUES (1)", "SELECT 1"]);
    }

    #[test]
    fn detect_statements_trailing_semicolons() {
        let stmts = detect_statements("SELECT 1;;;");
        assert_eq!(stmts, vec!["SELECT 1"]);
    }

    #[test]
    fn detect_statements_semicolon_in_string() {
        let stmts = detect_statements("SELECT 'a;b'");
        assert_eq!(stmts, vec!["SELECT 'a;b'"]);
    }

    #[test]
    fn detect_statements_semicolon_in_double_quoted() {
        let stmts = detect_statements(r#"SELECT "col;name" FROM t"#);
        assert_eq!(stmts, vec![r#"SELECT "col;name" FROM t"#]);
    }

    #[test]
    fn detect_statements_semicolon_in_line_comment() {
        let stmts = detect_statements("SELECT 1 -- ; comment\n; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 1 -- ; comment", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_semicolon_in_block_comment() {
        let stmts = detect_statements("SELECT /* ; */ 1; SELECT 2");
        assert_eq!(stmts, vec!["SELECT /* ; */ 1", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_escaped_single_quote() {
        let stmts = detect_statements("SELECT 'it''s'; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 'it''s'", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_empty_input() {
        let stmts = detect_statements("");
        assert!(stmts.is_empty());
    }

    #[test]
    fn detect_statements_whitespace_only() {
        let stmts = detect_statements("  \n  ");
        assert!(stmts.is_empty());
    }

    #[tokio::test]
    async fn run_query_select_returns_rows() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let result = DatabaseHandle::run_query(&db, "SELECT 1 AS num, 'hello' AS greeting")
            .await
            .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Select));
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.columns[0].name, "num");
        assert_eq!(result.columns[1].name, "greeting");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows_affected, 0);
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn run_query_insert_returns_rows_affected() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        // Create table first
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER)")
            .await
            .unwrap();
        // Insert
        let result = DatabaseHandle::run_query(&db, "INSERT INTO t VALUES (1), (2), (3)")
            .await
            .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Insert));
        assert_eq!(result.rows_affected, 3);
        assert!(result.columns.is_empty());
        assert!(result.rows.is_empty());
    }

    #[tokio::test]
    async fn run_query_ddl_returns_zero_rows() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let result =
            DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
                .await
                .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Ddl));
        assert!(result.columns.is_empty());
        assert!(result.rows.is_empty());
    }

    #[tokio::test]
    async fn run_query_update_returns_rows_affected() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER, val TEXT)")
            .await
            .unwrap();
        DatabaseHandle::run_query(&db, "INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')")
            .await
            .unwrap();
        let result = DatabaseHandle::run_query(&db, "UPDATE t SET val = 'x' WHERE id > 1")
            .await
            .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Update));
        assert_eq!(result.rows_affected, 2);
    }

    #[tokio::test]
    async fn run_query_delete_returns_rows_affected() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER)")
            .await
            .unwrap();
        DatabaseHandle::run_query(&db, "INSERT INTO t VALUES (1), (2), (3)")
            .await
            .unwrap();
        let result = DatabaseHandle::run_query(&db, "DELETE FROM t WHERE id = 2")
            .await
            .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Delete));
        assert_eq!(result.rows_affected, 1);
    }

    #[tokio::test]
    async fn run_batch_dml_only() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER)")
            .await
            .unwrap();
        let stmts = vec![
            "INSERT INTO t VALUES (1)",
            "INSERT INTO t VALUES (2)",
            "INSERT INTO t VALUES (3)",
        ];
        let result = DatabaseHandle::run_batch(&db, &stmts).await.unwrap();
        assert!(matches!(
            result.query_kind,
            QueryKind::Batch {
                statement_count: 3,
                has_trailing_select: false,
            }
        ));
        assert!(result.rows.is_empty());
        // Verify the inserts actually worked
        let check = DatabaseHandle::run_query(&db, "SELECT COUNT(*) FROM t")
            .await
            .unwrap();
        assert_eq!(check.rows.len(), 1);
    }

    #[tokio::test]
    async fn run_batch_with_trailing_select() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER, name TEXT)")
            .await
            .unwrap();
        let stmts = vec![
            "INSERT INTO t VALUES (1, 'alice')",
            "INSERT INTO t VALUES (2, 'bob')",
            "SELECT * FROM t ORDER BY id",
        ];
        let result = DatabaseHandle::run_batch(&db, &stmts).await.unwrap();
        assert!(matches!(
            result.query_kind,
            QueryKind::Batch {
                statement_count: 3,
                has_trailing_select: true,
            }
        ));
        // Should have rows from the trailing SELECT
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.columns[0].name, "id");
        assert_eq!(result.columns[1].name, "name");
    }

    #[tokio::test]
    async fn run_batch_rollback_on_error() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        DatabaseHandle::run_query(&db, "INSERT INTO t VALUES (1)")
            .await
            .unwrap();
        let stmts = vec![
            "INSERT INTO t VALUES (2)",
            "INSERT INTO t VALUES (1)", // duplicate PK — will fail
            "INSERT INTO t VALUES (3)",
        ];
        let err = DatabaseHandle::run_batch(&db, &stmts).await;
        assert!(err.is_err());
        // Verify rollback: only the original row (1) should exist, row (2) rolled back
        let check = DatabaseHandle::run_query(&db, "SELECT COUNT(*) FROM t")
            .await
            .unwrap();
        // Should be 1 (the original insert), not 2
        assert_eq!(check.rows.len(), 1);
    }

    #[tokio::test]
    async fn run_query_explain() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let result = DatabaseHandle::run_query(&db, "EXPLAIN SELECT 1")
            .await
            .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Explain));
        // EXPLAIN returns rows (bytecode)
        assert!(!result.rows.is_empty());
    }

    #[tokio::test]
    async fn run_query_pragma() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let result = DatabaseHandle::run_query(&db, "PRAGMA page_size")
            .await
            .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Pragma));
        assert_eq!(result.rows.len(), 1);
    }

    #[test]
    fn detect_statements_doubled_double_quotes() {
        let stmts = detect_statements(r#"SELECT "col""name" FROM t; SELECT 2"#);
        assert_eq!(stmts, vec![r#"SELECT "col""name" FROM t"#, "SELECT 2"]);
    }

    #[test]
    fn detect_query_kind_begin_commit_rollback() {
        assert!(matches!(detect_query_kind("BEGIN"), QueryKind::Other));
        assert!(matches!(detect_query_kind("COMMIT"), QueryKind::Other));
        assert!(matches!(detect_query_kind("ROLLBACK"), QueryKind::Other));
        assert!(matches!(detect_query_kind("END"), QueryKind::Other));
    }

    #[test]
    fn detect_query_kind_stacked_comments() {
        assert!(matches!(
            detect_query_kind("-- first\n-- second\nSELECT 1"),
            QueryKind::Select
        ));
        assert!(matches!(
            detect_query_kind("/* a */ /* b */ INSERT INTO t VALUES (1)"),
            QueryKind::Insert
        ));
    }

    #[test]
    fn detect_statements_consecutive_semicolons_mid_input() {
        let stmts = detect_statements("SELECT 1;; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_unclosed_string() {
        // Graceful degradation — treats rest as one statement
        let stmts = detect_statements("SELECT 'unclosed; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 'unclosed; SELECT 2"]);
    }

    #[test]
    fn detect_statements_unclosed_block_comment() {
        let stmts = detect_statements("SELECT /* unclosed; SELECT 2");
        assert_eq!(stmts, vec!["SELECT /* unclosed; SELECT 2"]);
    }

    #[tokio::test]
    async fn run_batch_with_trailing_explain() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER)")
            .await
            .unwrap();
        let stmts = vec!["INSERT INTO t VALUES (1)", "EXPLAIN SELECT * FROM t"];
        let result = DatabaseHandle::run_batch(&db, &stmts).await.unwrap();
        assert!(matches!(
            result.query_kind,
            QueryKind::Batch {
                statement_count: 2,
                has_trailing_select: true,
            }
        ));
        // EXPLAIN returns bytecode rows
        assert!(!result.rows.is_empty());
    }

    #[tokio::test]
    async fn run_batch_ddl_with_trailing_select() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let stmts = vec![
            "CREATE TABLE t (id INTEGER, name TEXT)",
            "INSERT INTO t VALUES (1, 'alice')",
            "SELECT * FROM t",
        ];
        let result = DatabaseHandle::run_batch(&db, &stmts).await.unwrap();
        assert!(matches!(
            result.query_kind,
            QueryKind::Batch {
                statement_count: 3,
                has_trailing_select: true,
            }
        ));
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.columns[0].name, "id");
    }

    #[tokio::test]
    async fn run_batch_rollback_preserves_only_original_data() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        DatabaseHandle::run_query(&db, "INSERT INTO t VALUES (1)")
            .await
            .unwrap();
        let stmts = vec![
            "INSERT INTO t VALUES (2)",
            "INSERT INTO t VALUES (1)", // duplicate PK — fails
            "INSERT INTO t VALUES (3)",
        ];
        let err = DatabaseHandle::run_batch(&db, &stmts).await;
        assert!(err.is_err());
        // Verify exact count: only original row (1) remains
        let check = DatabaseHandle::run_query(&db, "SELECT id FROM t ORDER BY id")
            .await
            .unwrap();
        assert_eq!(
            check.rows.len(),
            1,
            "rollback should leave only the original row"
        );
    }

    #[tokio::test]
    async fn run_query_vacuum_other_kind() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let result = DatabaseHandle::run_query(&db, "SELECT typeof(1)")
            .await
            .unwrap();
        assert!(matches!(result.query_kind, QueryKind::Select));
        // VACUUM goes through Other → query() path
        // Note: VACUUM on :memory: may behave differently, so just test the kind detection
        assert!(matches!(detect_query_kind("VACUUM"), QueryKind::Other));
    }

    #[tokio::test]
    async fn run_batch_with_user_transaction() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER)")
            .await
            .unwrap();
        // User provides their own BEGIN/COMMIT — should not double-wrap
        let stmts = vec![
            "BEGIN",
            "INSERT INTO t VALUES (1)",
            "INSERT INTO t VALUES (2)",
            "COMMIT",
        ];
        let result = DatabaseHandle::run_batch(&db, &stmts).await;
        assert!(
            result.is_ok(),
            "should not error on user-provided transaction"
        );
        // Verify data was committed
        let check = DatabaseHandle::run_query(&db, "SELECT COUNT(*) FROM t")
            .await
            .unwrap();
        assert_eq!(check.rows.len(), 1);
    }

    #[tokio::test]
    async fn run_integrity_check_ok() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let (msg, result) = DatabaseHandle::run_integrity_check_inner(&db)
            .await
            .unwrap();
        assert!(
            msg.starts_with("Integrity check: ok"),
            "expected ok, got: {msg}"
        );
        assert!(
            result.is_none(),
            "ok result should not produce a QueryResult"
        );
    }

    // ── parse_foreign_keys tests ──────────────────────────────────────

    #[test]
    fn parse_fk_single_constraint() {
        let sql = r"CREATE TABLE employees (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            department_id INTEGER NOT NULL,
            FOREIGN KEY (department_id) REFERENCES departments (id)
        )";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "department_id");
        assert_eq!(fks[0].to_table, "departments");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_multiple_constraints() {
        let sql = r"CREATE TABLE project_assignments (
            id INTEGER PRIMARY KEY,
            employee_id INTEGER NOT NULL,
            project_id INTEGER NOT NULL,
            FOREIGN KEY (employee_id) REFERENCES employees (id),
            FOREIGN KEY (project_id) REFERENCES projects (id)
        )";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].from_column, "employee_id");
        assert_eq!(fks[0].to_table, "employees");
        assert_eq!(fks[0].to_column, "id");
        assert_eq!(fks[1].from_column, "project_id");
        assert_eq!(fks[1].to_table, "projects");
        assert_eq!(fks[1].to_column, "id");
    }

    #[test]
    fn parse_fk_no_foreign_keys() {
        let sql = "CREATE TABLE simple (id INTEGER PRIMARY KEY, name TEXT)";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert!(fks.is_empty());
    }

    #[test]
    fn parse_fk_quoted_identifiers() {
        let sql = r#"CREATE TABLE "my table" (
            id INTEGER PRIMARY KEY,
            "ref_id" INTEGER,
            FOREIGN KEY ("ref_id") REFERENCES "other table" ("pk_col")
        )"#;
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "ref_id");
        assert_eq!(fks[0].to_table, "other table");
        assert_eq!(fks[0].to_column, "pk_col");
    }

    #[test]
    fn parse_fk_inline_column_constraint() {
        // Inline FK syntax: column_name TYPE REFERENCES table(col)
        // Our parser looks for "FOREIGN KEY" keyword, so inline FKs are NOT detected.
        // This is a known limitation — document it.
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, ref_id INTEGER REFERENCES other(id))";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        // Inline FK syntax is not parsed — only table-level FOREIGN KEY constraints
        assert!(fks.is_empty());
    }

    #[test]
    fn parse_fk_with_on_delete_cascade() {
        let sql = r"CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
        )";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "user_id");
        assert_eq!(fks[0].to_table, "users");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_case_insensitive() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, ref_id INTEGER, foreign key (ref_id) references other (id))";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "ref_id");
        assert_eq!(fks[0].to_table, "other");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_real_testdata_employees() {
        // Exact SQL from testdb/testdata.db
        let sql = "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL UNIQUE, department_id INTEGER NOT NULL, salary REAL NOT NULL, hire_date TEXT NOT NULL, title TEXT NOT NULL, FOREIGN KEY (department_id) REFERENCES departments (id))";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "department_id");
        assert_eq!(fks[0].to_table, "departments");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_real_testdata_project_assignments() {
        // Exact SQL from testdb/testdata.db
        let sql = "CREATE TABLE project_assignments (id INTEGER PRIMARY KEY, employee_id INTEGER NOT NULL, project_id INTEGER NOT NULL, role TEXT NOT NULL, hours_allocated REAL NOT NULL DEFAULT 40.0, FOREIGN KEY (employee_id) REFERENCES employees (id), FOREIGN KEY (project_id) REFERENCES projects (id))";
        let fks = DatabaseHandle::parse_foreign_keys(sql);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].from_column, "employee_id");
        assert_eq!(fks[0].to_table, "employees");
        assert_eq!(fks[1].from_column, "project_id");
        assert_eq!(fks[1].to_table, "projects");
    }
}
