//! Schema loading: tables, views, columns, custom types, and row counts.

use std::sync::Arc;

use crate::handle::DatabaseHandle;
use crate::types::{ColumnInfo, CustomTypeInfo, IndexDetail, QueryMessage, SchemaEntry};

/// Base storage types returned by `PRAGMA list_types` that we filter out.
const BASE_TYPES: &[&str] = tursotui_sql::keywords::BASE_TYPES;

/// Turso built-in custom types -- shipped with the engine, not user-defined.
const BUILTIN_CUSTOM_TYPES: &[&str] = &[
    "bigint",
    "boolean",
    "bytea",
    "date",
    "inet",
    "json",
    "jsonb",
    "numeric",
    "smallint",
    "time",
    "timestamp",
    "uuid",
    "varchar",
];

impl DatabaseHandle {
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

    pub(crate) async fn run_schema_load(
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

            if name.starts_with("sqlite_") || name.starts_with("__turso_") {
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

    /// Load custom types via `PRAGMA list_types`, filtering out base types.
    pub fn load_custom_types(&self) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                match Self::run_custom_types_load(&db).await {
                    Ok(types) => QueryMessage::CustomTypesLoaded(types),
                    // Silently produce empty list on failure -- custom types are optional.
                    Err(_) => QueryMessage::CustomTypesLoaded(Vec::new()),
                }
            });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => QueryMessage::CustomTypesLoaded(Vec::new()),
            };
            let _ = tx_panic.send(msg);
        });
    }

    pub(crate) async fn run_custom_types_load(
        db: &turso::Database,
    ) -> Result<Vec<CustomTypeInfo>, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;
        let mut rows = conn.query("PRAGMA list_types", ()).await?;

        let mut types = Vec::new();
        while let Some(row) = rows.next().await? {
            let type_name: String = row.get_value(0)?.as_text().cloned().unwrap_or_default();
            let parent: String = row.get_value(1)?.as_text().cloned().unwrap_or_default();

            // Skip the 5 base types -- only keep custom/built-in extended types.
            // Case-insensitive: PRAGMA output format isn't guaranteed uppercase.
            if BASE_TYPES
                .iter()
                .any(|b| b.eq_ignore_ascii_case(&type_name))
            {
                continue;
            }

            let name = Self::extract_type_name(&type_name);
            if name.is_empty() {
                continue;
            }

            let builtin = BUILTIN_CUSTOM_TYPES
                .iter()
                .any(|b| b.eq_ignore_ascii_case(&name));

            types.push(CustomTypeInfo {
                name,
                parent,
                builtin,
            });
        }
        Ok(types)
    }

    /// Extract the type name from a `PRAGMA list_types` type column.
    /// Strips parenthesized parameters: `"varchar(value text, maxlen integer)"` -> `"varchar"`.
    pub(crate) fn extract_type_name(raw: &str) -> String {
        raw.find('(').map_or(raw, |i| &raw[..i]).to_owned()
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
    pub fn load_row_counts(&self, tables: &[String]) {
        for table in tables {
            let db = Arc::clone(&self.database);
            let tx = self.result_tx.clone();
            let table_name = table.clone();
            let table_lower = table.to_lowercase();
            tokio::spawn(async move {
                let Ok(conn) = db.connect() else { return };
                let sql = format!(
                    "SELECT COUNT(*) FROM {}",
                    tursotui_sql::quoting::quote_identifier(&table_name)
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

    pub(crate) async fn run_column_load(
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

    /// Load index metadata for a specific table.
    pub fn load_indexes(&self, table_name: String) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                let result = load_index_details(&db, &table_name).await;
                match result {
                    Ok(indexes) => QueryMessage::IndexDetailsLoaded(table_name, indexes),
                    Err(e) => QueryMessage::Failed(format!(
                        "Failed to load indexes for {table_name}: {e}"
                    )),
                }
            });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => {
                    QueryMessage::Failed("Internal error: index load task panicked".to_string())
                }
            };
            let _ = tx_panic.send(msg);
        });
    }
}

/// Load index details for a single table via PRAGMA `index_list` + `index_info`.
pub(crate) async fn load_index_details(
    db: &turso::Database,
    table_name: &str,
) -> Result<Vec<IndexDetail>, Box<dyn std::error::Error + Send + Sync>> {
    let conn = db.connect()?;
    let mut indexes = Vec::new();
    // Escape single quotes in identifiers for PRAGMA syntax (Turso uses single-quoted values).
    let safe_table = table_name.replace('\'', "''");
    let mut list_rows = conn
        .query(
            &format!("PRAGMA index_list('{safe_table}')"),
            (),
        )
        .await?;
    while let Some(row) = list_rows.next().await? {
        let idx_name: String = row.get(1)?;
        let unique: i64 = row.get(2)?;
        let safe_idx = idx_name.replace('\'', "''");
        let mut info_rows = conn
            .query(
                &format!("PRAGMA index_info('{safe_idx}')"),
                (),
            )
            .await?;
        let mut columns = Vec::new();
        while let Some(info_row) = info_rows.next().await? {
            let col_name: String = info_row.get(2)?;
            columns.push(col_name);
        }
        indexes.push(IndexDetail {
            name: idx_name,
            table_name: table_name.to_string(),
            unique: unique != 0,
            columns,
        });
    }
    Ok(indexes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_custom_types_returns_base_types_only() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let types = DatabaseHandle::run_custom_types_load(&db).await.unwrap();
        // Without experimental custom types, only base types are returned
        // and those are filtered out -- result should be empty.
        assert!(
            types.is_empty(),
            "expected no custom types without experimental flag, got: {types:?}"
        );
    }

    #[test]
    fn base_types_are_filtered() {
        // Verify the BASE_TYPES constant contains the expected entries
        assert!(BASE_TYPES.contains(&"INTEGER"));
        assert!(BASE_TYPES.contains(&"REAL"));
        assert!(BASE_TYPES.contains(&"TEXT"));
        assert!(BASE_TYPES.contains(&"BLOB"));
        assert!(BASE_TYPES.contains(&"ANY"));
        assert_eq!(BASE_TYPES.len(), 5);
    }

    #[test]
    fn extract_type_name_strips_parens() {
        assert_eq!(
            DatabaseHandle::extract_type_name("varchar(value text, maxlen integer)"),
            "varchar"
        );
        assert_eq!(
            DatabaseHandle::extract_type_name(
                "numeric(value any, precision integer, scale integer)"
            ),
            "numeric"
        );
        assert_eq!(
            DatabaseHandle::extract_type_name("boolean(value any)"),
            "boolean"
        );
    }

    #[test]
    fn extract_type_name_no_parens() {
        assert_eq!(DatabaseHandle::extract_type_name("email"), "email");
        assert_eq!(DatabaseHandle::extract_type_name("INTEGER"), "INTEGER");
    }

    // ── Integration tests via DatabaseHandle ─────────────────────────

    async fn recv_timeout(handle: &mut DatabaseHandle) -> QueryMessage {
        tokio::time::timeout(std::time::Duration::from_secs(2), handle.recv())
            .await
            .expect("recv timed out after 2s")
            .expect("channel closed unexpectedly")
    }

    #[tokio::test]
    async fn load_schema_returns_user_tables() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        let conn = handle.connect().unwrap();
        conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", ())
            .await
            .unwrap();

        handle.load_schema();
        match recv_timeout(&mut handle).await {
            QueryMessage::SchemaLoaded(entries) => {
                let user_table = entries.iter().find(|e| e.name == "users");
                assert!(user_table.is_some(), "users table should appear in schema");
                assert_eq!(user_table.unwrap().obj_type, "table");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_schema_excludes_internal_tables() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        handle.load_schema();
        match recv_timeout(&mut handle).await {
            QueryMessage::SchemaLoaded(entries) => {
                let internal = entries.iter().find(|e| e.name.starts_with("sqlite_"));
                assert!(internal.is_none(), "sqlite_ tables should be filtered out");
                let turso = entries.iter().find(|e| e.name.starts_with("__turso_"));
                assert!(turso.is_none(), "__turso_ tables should be filtered out");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_columns_returns_column_info() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        let conn = handle.connect().unwrap();
        conn.execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, price REAL DEFAULT 0.0)",
            (),
        )
        .await
        .unwrap();

        handle.load_columns("items".into());
        match recv_timeout(&mut handle).await {
            QueryMessage::ColumnsLoaded(table, cols) => {
                assert_eq!(table, "items");
                assert_eq!(cols.len(), 3, "items should have 3 columns");
                let id_col = cols.iter().find(|c| c.name == "id").unwrap();
                assert!(id_col.pk, "id should be primary key");
                let name_col = cols.iter().find(|c| c.name == "name").unwrap();
                assert!(name_col.notnull, "name should be NOT NULL");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_columns_handles_sql_keyword_table_name() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        let conn = handle.connect().unwrap();
        conn.execute(
            "CREATE TABLE \"select\" (id INTEGER PRIMARY KEY, \"from\" TEXT)",
            (),
        )
        .await
        .unwrap();

        handle.load_columns("select".into());
        match recv_timeout(&mut handle).await {
            QueryMessage::ColumnsLoaded(table, cols) => {
                assert_eq!(table, "select", "table name should preserve SQL keyword");
                assert_eq!(cols.len(), 2);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_row_counts_returns_counts_per_table() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        let conn = handle.connect().unwrap();
        conn.execute("CREATE TABLE a (id INTEGER PRIMARY KEY)", ())
            .await
            .unwrap();
        conn.execute("CREATE TABLE b (id INTEGER PRIMARY KEY)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO a VALUES (1)", ()).await.unwrap();
        conn.execute("INSERT INTO a VALUES (2)", ()).await.unwrap();
        conn.execute("INSERT INTO b VALUES (1)", ()).await.unwrap();

        handle.load_row_counts(&["a".into(), "b".into()]);

        // Two independent tasks spawned — collect both messages
        let mut counts = std::collections::HashMap::new();
        for _ in 0..2 {
            let msg = recv_timeout(&mut handle).await;
            if let QueryMessage::RowCount(table, count) = msg {
                counts.insert(table, count);
            }
        }
        assert_eq!(counts.get("a"), Some(&2), "table a should have 2 rows");
        assert_eq!(counts.get("b"), Some(&1), "table b should have 1 row");
    }

    #[tokio::test]
    async fn load_indexes_returns_index_details() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, email TEXT)",
            (),
        )
        .await
        .unwrap();
        conn.execute("CREATE INDEX idx_t_name ON t(name)", ())
            .await
            .unwrap();
        conn.execute("CREATE UNIQUE INDEX idx_t_email ON t(email)", ())
            .await
            .unwrap();
        let indexes = load_index_details(&db, "t").await.unwrap();
        assert_eq!(indexes.len(), 2);
        let name_idx = indexes.iter().find(|i| i.name == "idx_t_name").unwrap();
        assert!(!name_idx.unique);
        assert_eq!(name_idx.columns, vec!["name"]);
        let email_idx = indexes.iter().find(|i| i.name == "idx_t_email").unwrap();
        assert!(email_idx.unique);
        assert_eq!(email_idx.columns, vec!["email"]);
    }
}
