//! Schema loading: tables, views, columns, custom types, and row counts.

use std::sync::Arc;

use crate::handle::DatabaseHandle;
use crate::types::{ColumnInfo, CustomTypeInfo, QueryMessage, SchemaEntry};

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
}
