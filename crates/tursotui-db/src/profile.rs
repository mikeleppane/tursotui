//! Data profiling: per-column statistics, top values, and sampling for large tables.

use std::sync::Arc;

use crate::handle::DatabaseHandle;
use crate::types::{ColumnInfo, ColumnProfile, ProfileData, QueryMessage};

/// Numeric type prefixes for detecting numeric columns (case-insensitive).
const NUMERIC_TYPES: &[&str] = &[
    "integer", "int", "real", "float", "double", "decimal", "numeric", "bigint", "smallint",
    "tinyint",
];

/// Text type prefixes for detecting text/blob columns (case-insensitive).
const TEXT_TYPES: &[&str] = &[
    "text", "varchar", "char", "string", "blob", "clob", "nvarchar", "nchar",
];

/// Check if a column type is numeric.
fn is_numeric_type(col_type: &str) -> bool {
    let lower = col_type.to_lowercase();
    NUMERIC_TYPES.iter().any(|t| lower.starts_with(t))
}

/// Check if a column type is text-like.
fn is_text_type(col_type: &str) -> bool {
    let lower = col_type.to_lowercase();
    // Empty type defaults to text-like behavior (SQLite flexible typing).
    lower.is_empty() || TEXT_TYPES.iter().any(|t| lower.starts_with(t))
}

/// Build the aggregate profile query for a single column.
///
/// Returns a SELECT statement whose column order matches the positional indices
/// used by `parse_profile_row`. The `sample_clause` is an optional WHERE clause
/// for sampling (e.g., `WHERE rowid % 10 = 0`).
pub fn build_profile_query(
    table_name: &str,
    col: &ColumnInfo,
    sample_clause: &str,
    supports_stddev: bool,
) -> String {
    let quoted_table = tursotui_sql::quoting::quote_identifier(table_name);
    let quoted_col = tursotui_sql::quoting::quote_identifier(&col.name);

    // Positions: 0=total_count, 1=null_count, 2=distinct_count, 3=min_val, 4=max_val
    let mut selects = vec![
        "COUNT(*) AS total_count".to_string(),
        format!("COUNT(*) - COUNT({quoted_col}) AS null_count"),
        format!("COUNT(DISTINCT {quoted_col}) AS distinct_count"),
        format!("MIN({quoted_col}) AS min_val"),
        format!("MAX({quoted_col}) AS max_val"),
    ];

    // Numeric columns: positions 5=avg, 6=sum, optionally 7=stddev
    if is_numeric_type(&col.col_type) {
        selects.push(format!("AVG({quoted_col}) AS avg_val"));
        selects.push(format!("SUM({quoted_col}) AS sum_val"));
        if supports_stddev {
            selects.push(format!("stddev({quoted_col}) AS stddev_val"));
        }
    }

    // Text columns: positions vary based on whether numeric was added
    if is_text_type(&col.col_type) {
        selects.push(format!("MIN(LENGTH({quoted_col})) AS min_length"));
        selects.push(format!("MAX(LENGTH({quoted_col})) AS max_length"));
        selects.push(format!("AVG(LENGTH({quoted_col})) AS avg_length"));
    }

    let select_clause = selects.join(", ");
    if sample_clause.is_empty() {
        format!("SELECT {select_clause} FROM {quoted_table}")
    } else {
        format!("SELECT {select_clause} FROM {quoted_table} {sample_clause}")
    }
}

/// Build a top-N values query for a column.
///
/// The `sample_clause` (e.g., `WHERE rowid % 10 = 0`) is combined with the
/// NOT NULL filter when present, so sampling applies to top-values too.
pub fn build_top_values_query(
    table_name: &str,
    col_name: &str,
    limit: usize,
    sample_clause: &str,
) -> String {
    let quoted_table = tursotui_sql::quoting::quote_identifier(table_name);
    let quoted_col = tursotui_sql::quoting::quote_identifier(col_name);
    let where_clause = if sample_clause.is_empty() {
        format!("WHERE {quoted_col} IS NOT NULL")
    } else {
        // sample_clause starts with "WHERE rowid % N = 0" — append AND
        format!("{sample_clause} AND {quoted_col} IS NOT NULL")
    };
    format!(
        "SELECT CAST({quoted_col} AS TEXT) AS val, COUNT(*) AS cnt \
         FROM {quoted_table} {where_clause} \
         GROUP BY {quoted_col} ORDER BY cnt DESC LIMIT {limit}"
    )
}

impl DatabaseHandle {
    /// Profile a table in the background. Results arrive as `QueryMessage::ProfileCompleted`
    /// or `ProfileFailed` via `try_recv()`.
    pub fn profile_table(
        &self,
        table_name: String,
        columns: Vec<ColumnInfo>,
        total_rows: u64,
        sample_threshold: u64,
        supports_stddev: bool,
    ) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let tx_panic = tx.clone();
            let handle = tokio::spawn(async move {
                match run_profile(
                    &db,
                    &table_name,
                    &columns,
                    total_rows,
                    sample_threshold,
                    supports_stddev,
                )
                .await
                {
                    Ok(data) => QueryMessage::ProfileCompleted(data),
                    Err(e) => QueryMessage::ProfileFailed(e.to_string()),
                }
            });
            let msg = match handle.await {
                Ok(msg) => msg,
                Err(_) => {
                    QueryMessage::ProfileFailed("Internal error: profile task panicked".to_string())
                }
            };
            let _ = tx_panic.send(msg);
        });
    }

    /// Probe whether `stddev()` is available (Turso-specific aggregate).
    /// Sends `QueryMessage::StddevProbeResult(true/false)` via the channel.
    pub fn probe_stddev(&self) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let conn = db.connect()?;
                conn.query("SELECT stddev(1)", ()).await?;
                Ok::<bool, Box<dyn std::error::Error + Send + Sync>>(true)
            }
            .await
            .unwrap_or(false);
            let _ = tx.send(QueryMessage::StddevProbeResult(result));
        });
    }
}

/// Run the full profiling pipeline for a table.
async fn run_profile(
    db: &turso::Database,
    table_name: &str,
    columns: &[ColumnInfo],
    total_rows: u64,
    sample_threshold: u64,
    supports_stddev: bool,
) -> Result<ProfileData, Box<dyn std::error::Error + Send + Sync>> {
    let conn = db.connect()?;

    let sampled = total_rows > sample_threshold && sample_threshold > 0;
    let sample_clause = if sampled {
        let modulus = (total_rows / sample_threshold).max(1);
        format!("WHERE rowid % {modulus} = 0")
    } else {
        String::new()
    };

    let mut col_profiles = Vec::with_capacity(columns.len());

    for col in columns {
        let sql = build_profile_query(table_name, col, &sample_clause, supports_stddev);
        let mut rows = conn.query(&sql, ()).await?;
        let is_numeric = is_numeric_type(&col.col_type);
        let is_text = is_text_type(&col.col_type);

        let mut profile = ColumnProfile {
            name: col.name.clone(),
            col_type: col.col_type.clone(),
            total_count: 0,
            null_count: 0,
            distinct_count: 0,
            min: None,
            max: None,
            avg: None,
            sum: None,
            stddev: None,
            min_length: None,
            max_length: None,
            avg_length: None,
            top_values: Vec::new(),
        };

        if let Some(row) = rows.next().await? {
            // Core stats (always at positions 0-4)
            profile.total_count = row_get_u64(&row, 0);
            profile.null_count = row_get_u64(&row, 1);
            profile.distinct_count = row_get_u64(&row, 2);
            profile.min = row_get_display(&row, 3);
            profile.max = row_get_display(&row, 4);

            let mut next_idx = 5;

            if is_numeric {
                profile.avg = row_get_f64(&row, next_idx);
                next_idx += 1;
                profile.sum = row_get_f64(&row, next_idx);
                next_idx += 1;
                if supports_stddev {
                    profile.stddev = row_get_f64(&row, next_idx);
                    next_idx += 1;
                }
            }

            if is_text {
                profile.min_length = row_get_opt_u64(&row, next_idx);
                profile.max_length = row_get_opt_u64(&row, next_idx + 1);
                profile.avg_length = row_get_f64(&row, next_idx + 2);
            }
        }

        // Top values query (also sampled for large tables)
        let top_sql = build_top_values_query(table_name, &col.name, 5, &sample_clause);
        let mut top_rows = conn.query(&top_sql, ()).await?;
        while let Some(top_row) = top_rows.next().await? {
            let val = top_row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_text().cloned())
                .unwrap_or_default();
            let cnt = top_row.get::<i64>(1).unwrap_or(0);
            profile.top_values.push((val, cnt.max(0) as u64));
        }

        col_profiles.push(profile);
    }

    Ok(ProfileData {
        table_name: table_name.to_string(),
        total_rows,
        sampled,
        columns: col_profiles,
    })
}

/// Extract a u64 from a positional column, defaulting to 0.
fn row_get_u64(row: &turso::Row, idx: usize) -> u64 {
    // Try integer first, then fall back to real (some aggregates return real).
    row.get_value(idx).ok().map_or(0, |v| match v {
        turso::Value::Integer(n) => n.max(0) as u64,
        turso::Value::Real(f) => f.max(0.0) as u64,
        _ => 0,
    })
}

/// Extract an optional u64 from a positional column.
fn row_get_opt_u64(row: &turso::Row, idx: usize) -> Option<u64> {
    row.get_value(idx).ok().and_then(|v| match v {
        turso::Value::Integer(n) => Some(n.max(0) as u64),
        turso::Value::Real(f) => Some(f.max(0.0) as u64),
        _ => None,
    })
}

/// Extract an optional f64 from a positional column.
#[allow(clippy::cast_precision_loss)]
fn row_get_f64(row: &turso::Row, idx: usize) -> Option<f64> {
    row.get_value(idx).ok().and_then(|v| match v {
        turso::Value::Real(f) => Some(f),
        turso::Value::Integer(n) => Some(n as f64),
        _ => None,
    })
}

/// Extract an optional display string from a positional column.
fn row_get_display(row: &turso::Row, idx: usize) -> Option<String> {
    row.get_value(idx).ok().and_then(|v| match v {
        turso::Value::Text(s) => Some(s),
        turso::Value::Integer(n) => Some(n.to_string()),
        turso::Value::Real(f) => Some(f.to_string()),
        turso::Value::Null => None,
        turso::Value::Blob(b) => Some(format!("[BLOB {} B]", b.len())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_profile_query_numeric_with_stddev() {
        let col = ColumnInfo {
            name: "age".to_string(),
            col_type: "INTEGER".to_string(),
            notnull: false,
            default_value: None,
            pk: false,
        };
        let sql = build_profile_query("users", &col, "", true);
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("AVG(\"age\")"));
        assert!(sql.contains("SUM(\"age\")"));
        assert!(sql.contains("stddev(\"age\")"));
        assert!(sql.contains("FROM \"users\""));
        assert!(!sql.contains("LENGTH"));
    }

    #[test]
    fn build_profile_query_numeric_without_stddev() {
        let col = ColumnInfo {
            name: "price".to_string(),
            col_type: "REAL".to_string(),
            notnull: false,
            default_value: None,
            pk: false,
        };
        let sql = build_profile_query("products", &col, "", false);
        assert!(sql.contains("AVG(\"price\")"));
        assert!(sql.contains("SUM(\"price\")"));
        assert!(!sql.contains("stddev"));
    }

    #[test]
    fn build_profile_query_text_column() {
        let col = ColumnInfo {
            name: "name".to_string(),
            col_type: "TEXT".to_string(),
            notnull: false,
            default_value: None,
            pk: false,
        };
        let sql = build_profile_query("users", &col, "", false);
        assert!(sql.contains("MIN(LENGTH(\"name\"))"));
        assert!(sql.contains("MAX(LENGTH(\"name\"))"));
        assert!(sql.contains("AVG(LENGTH(\"name\"))"));
        assert!(!sql.contains("SUM"));
        assert!(!sql.contains("stddev"));
    }

    #[test]
    fn build_profile_query_with_sample_clause() {
        let col = ColumnInfo {
            name: "id".to_string(),
            col_type: "INTEGER".to_string(),
            notnull: true,
            default_value: None,
            pk: true,
        };
        let sql = build_profile_query("big_table", &col, "WHERE rowid % 10 = 0", false);
        assert!(sql.contains("WHERE rowid % 10 = 0"));
    }

    #[test]
    fn build_profile_query_escapes_identifiers() {
        let col = ColumnInfo {
            name: "select".to_string(),
            col_type: "TEXT".to_string(),
            notnull: false,
            default_value: None,
            pk: false,
        };
        let sql = build_profile_query("from", &col, "", false);
        assert!(sql.contains("\"select\""));
        assert!(sql.contains("\"from\""));
    }

    #[test]
    fn build_top_values_query_basic() {
        let sql = build_top_values_query("users", "status", 5, "");
        assert!(sql.contains("\"status\""));
        assert!(sql.contains("\"users\""));
        assert!(sql.contains("ORDER BY cnt DESC"));
        assert!(sql.contains("LIMIT 5"));
        assert!(sql.contains("IS NOT NULL"));
    }

    #[test]
    fn build_top_values_query_with_sampling() {
        let sql = build_top_values_query("big", "col", 5, "WHERE rowid % 10 = 0");
        assert!(sql.contains("WHERE rowid % 10 = 0 AND \"col\" IS NOT NULL"));
    }

    #[test]
    fn is_numeric_type_matches() {
        assert!(is_numeric_type("INTEGER"));
        assert!(is_numeric_type("integer"));
        assert!(is_numeric_type("REAL"));
        assert!(is_numeric_type("FLOAT"));
        assert!(is_numeric_type("DOUBLE"));
        assert!(is_numeric_type("DECIMAL(10,2)"));
        assert!(is_numeric_type("NUMERIC"));
        assert!(is_numeric_type("BIGINT"));
        assert!(is_numeric_type("INT"));
        assert!(!is_numeric_type("TEXT"));
        assert!(!is_numeric_type("VARCHAR(255)"));
        assert!(!is_numeric_type("BLOB"));
    }

    #[test]
    fn is_text_type_matches() {
        assert!(is_text_type("TEXT"));
        assert!(is_text_type("VARCHAR(255)"));
        assert!(is_text_type("CHAR(10)"));
        assert!(is_text_type("STRING"));
        assert!(is_text_type("BLOB"));
        assert!(is_text_type("")); // empty type defaults to text-like
        assert!(!is_text_type("INTEGER"));
        assert!(!is_text_type("REAL"));
    }

    // ── Integration tests ────────────────────────────────

    async fn recv_timeout(handle: &mut DatabaseHandle) -> QueryMessage {
        tokio::time::timeout(std::time::Duration::from_secs(5), handle.recv())
            .await
            .expect("recv timed out after 5s")
            .expect("channel closed unexpectedly")
    }

    #[tokio::test]
    async fn profile_table_basic() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        let conn = handle.connect().unwrap();
        conn.execute(
            "CREATE TABLE test_profile (id INTEGER PRIMARY KEY, name TEXT, score REAL)",
            (),
        )
        .await
        .unwrap();
        conn.execute("INSERT INTO test_profile VALUES (1, 'alice', 95.5)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO test_profile VALUES (2, 'bob', 87.0)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO test_profile VALUES (3, NULL, 92.3)", ())
            .await
            .unwrap();

        let columns = vec![
            ColumnInfo {
                name: "id".to_string(),
                col_type: "INTEGER".to_string(),
                notnull: true,
                default_value: None,
                pk: true,
            },
            ColumnInfo {
                name: "name".to_string(),
                col_type: "TEXT".to_string(),
                notnull: false,
                default_value: None,
                pk: false,
            },
            ColumnInfo {
                name: "score".to_string(),
                col_type: "REAL".to_string(),
                notnull: false,
                default_value: None,
                pk: false,
            },
        ];

        handle.profile_table("test_profile".to_string(), columns, 3, 10_000, false);

        match recv_timeout(&mut handle).await {
            QueryMessage::ProfileCompleted(data) => {
                assert_eq!(data.table_name, "test_profile");
                assert_eq!(data.total_rows, 3);
                assert!(!data.sampled);
                assert_eq!(data.columns.len(), 3);

                // id column
                let id_col = &data.columns[0];
                assert_eq!(id_col.total_count, 3);
                assert_eq!(id_col.null_count, 0);
                assert_eq!(id_col.distinct_count, 3);
                assert!(id_col.avg.is_some()); // numeric

                // name column
                let name_col = &data.columns[1];
                assert_eq!(name_col.null_count, 1);
                assert_eq!(name_col.distinct_count, 2);
                assert!(name_col.min_length.is_some()); // text

                // score column
                let score_col = &data.columns[2];
                assert_eq!(score_col.null_count, 0);
                assert!(score_col.sum.is_some());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_stddev_returns_result() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        handle.probe_stddev();

        match recv_timeout(&mut handle).await {
            QueryMessage::StddevProbeResult(supported) => {
                // In standard SQLite, stddev is not built-in; in turso it is.
                // Either result is valid for this test.
                let _ = supported;
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
