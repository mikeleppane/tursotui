//! Struct and enum definitions for database query results, schema, and metadata.

use std::time::Duration;

use tursotui_sql::parser::ForeignKeyInfo;
use tursotui_sql::query_kind::QueryKind;

/// Bound parameter values for parameterized query execution.
#[derive(Debug, Clone)]
pub enum QueryParams {
    /// Positional parameters bound to `?1`, `?2`, … placeholders.
    Positional(Vec<turso::Value>),
    /// Named parameters bound to `:name`, `@name`, or `$name` placeholders.
    Named(Vec<(String, turso::Value)>),
}

/// A single column definition from query results.
#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: String,
}

/// Result of a completed query.
#[derive(Debug, Clone)]
pub struct QueryResult {
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

/// A raw schema entry from `sqlite_schema`.
#[derive(Debug, Clone)]
pub struct SchemaEntry {
    pub obj_type: String,
    pub name: String,
    pub tbl_name: String,
    pub sql: Option<String>,
}

/// Column info from PRAGMA `table_info`.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub col_type: String,
    pub notnull: bool,
    pub default_value: Option<String>,
    pub pk: bool,
}

/// A custom type from `PRAGMA list_types` (non-base types only).
#[derive(Debug, Clone)]
pub struct CustomTypeInfo {
    pub name: String,
    pub parent: String,
    /// True for Turso's built-in types (uuid, boolean, etc.), false for user-defined.
    pub builtin: bool,
}

/// Index metadata from PRAGMA `index_list` + `index_info`.
#[derive(Debug, Clone)]
pub struct IndexDetail {
    pub name: String,
    pub table_name: String,
    pub unique: bool,
    /// Columns in index order (first = leftmost key).
    pub columns: Vec<String>,
}

/// Database metadata from PRAGMAs and file system.
#[derive(Debug, Clone)]
pub struct DbInfo {
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
pub struct PragmaEntry {
    pub name: String,
    pub value: String,
    pub writable: bool,
    pub note: Option<String>,
}

/// Per-column profiling statistics.
#[derive(Debug, Clone)]
pub struct ColumnProfile {
    pub name: String,
    pub col_type: String,
    pub total_count: u64,
    pub null_count: u64,
    pub distinct_count: u64,
    pub min: Option<String>,
    pub max: Option<String>,
    pub avg: Option<f64>,
    pub sum: Option<f64>,
    pub stddev: Option<f64>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub avg_length: Option<f64>,
    pub top_values: Vec<(String, u64)>,
}

/// Complete profile result for a table.
#[derive(Debug, Clone)]
pub struct ProfileData {
    pub table_name: String,
    pub total_rows: u64,
    pub sampled: bool,
    pub columns: Vec<ColumnProfile>,
}

/// Messages sent from query tasks back to the main loop.
#[derive(Debug)]
pub enum QueryMessage {
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
    TransactionCommitted,
    TransactionFailed(String),
    ForeignKeysLoaded(String, Vec<ForeignKeyInfo>),
    CustomTypesLoaded(Vec<CustomTypeInfo>),
    RowCount(String, u64), // (table_name_lowercase, count)
    IndexDetailsLoaded(String, Vec<IndexDetail>), // (table_name, indexes)
    ProfileCompleted(ProfileData),
    ProfileFailed(String),
    StddevProbeResult(bool),
}

/// Convert a turso Value to a display-ready `Option<String>`.
/// Returns None for NULL (callers use None for "no value" semantics).
pub fn value_to_display(val: &turso::Value) -> Option<String> {
    match val {
        turso::Value::Null => None,
        turso::Value::Integer(n) => Some(n.to_string()),
        turso::Value::Real(f) => Some(f.to_string()),
        turso::Value::Text(s) => Some(s.clone()),
        turso::Value::Blob(b) => Some(format!("[BLOB {} B]", b.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turso::Value;

    #[test]
    fn value_to_display_null_returns_none() {
        assert_eq!(value_to_display(&Value::Null), None, "NULL should be None");
    }

    #[test]
    fn value_to_display_integer() {
        assert_eq!(
            value_to_display(&Value::Integer(42)),
            Some("42".to_string()),
        );
    }

    #[test]
    fn value_to_display_negative_integer() {
        assert_eq!(
            value_to_display(&Value::Integer(-1)),
            Some("-1".to_string()),
        );
    }

    #[test]
    fn value_to_display_integer_boundaries() {
        assert_eq!(
            value_to_display(&Value::Integer(i64::MAX)),
            Some(i64::MAX.to_string()),
        );
        assert_eq!(
            value_to_display(&Value::Integer(i64::MIN)),
            Some(i64::MIN.to_string()),
        );
    }

    #[test]
    fn value_to_display_real() {
        assert_eq!(
            value_to_display(&Value::Real(3.14)),
            Some("3.14".to_string()),
        );
    }

    #[test]
    fn value_to_display_text() {
        assert_eq!(
            value_to_display(&Value::Text("hello".into())),
            Some("hello".to_string()),
        );
    }

    #[test]
    fn value_to_display_empty_text_is_some_not_none() {
        assert_eq!(
            value_to_display(&Value::Text(String::new())),
            Some(String::new()),
            "empty text should be Some(\"\"), not None"
        );
    }

    #[test]
    fn value_to_display_blob_shows_byte_count() {
        let result = value_to_display(&Value::Blob(vec![0, 1, 2]));
        assert_eq!(result, Some("[BLOB 3 B]".to_string()));
    }

    #[test]
    fn value_to_display_empty_blob() {
        let result = value_to_display(&Value::Blob(vec![]));
        assert_eq!(result, Some("[BLOB 0 B]".to_string()));
    }
}
