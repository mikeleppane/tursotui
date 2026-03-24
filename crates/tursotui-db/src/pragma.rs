//! Pragma operations: loading, setting, and helper functions.

use std::sync::Arc;

use tursotui_sql::validation::sanitize_pragma_value;

use crate::handle::DatabaseHandle;
use crate::types::{DbInfo, PragmaEntry, QueryMessage};

/// Writable pragmas that can be set via `set_pragma`. Used as a whitelist for validation.
/// Note: `mmap_size` and `wal_autocheckpoint` are standard `SQLite` pragmas but not supported by
/// turso/libsql -- they return "Not a valid pragma name".
pub const WRITABLE_PRAGMAS: &[&str] = &[
    "cache_size",
    "busy_timeout",
    "synchronous",
    "foreign_keys",
    "temp_store",
    "query_only",
    "max_page_count",
];

impl DatabaseHandle {
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
            turso_version: crate::TURSO_VERSION,
            wal_frames,
        })
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

        // Defense-in-depth: sanitize value to prevent SQL injection.
        // Uses the shared sanitize_pragma_value() which is also called by
        // PragmaDashboard for fast UI feedback.
        let safe_value = sanitize_pragma_value(name, value)?;

        let conn = db.connect()?;

        // Set the pragma value -- safe_value is guaranteed to be a plain integer
        conn.execute(&format!("PRAGMA {name} = {safe_value}"), ())
            .await?;

        // Read back to confirm
        let confirmed = Self::pragma_string(&conn, name).await?;
        Ok(confirmed)
    }

    // ── Shared PRAGMA helpers ──────────────────────────────────────────

    /// Read a single PRAGMA value as an i64.
    /// Returns 0 if the pragma returns no rows or 0 columns (unsupported by turso/libsql).
    pub(crate) async fn pragma_i64(
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
    pub(crate) async fn pragma_string(
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
}
