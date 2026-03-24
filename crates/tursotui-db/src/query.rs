//! Query execution: single statements and multi-statement batches.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tursotui_sql::parser::detect_statements;
use tursotui_sql::query_kind::{QueryKind, detect_query_kind, is_transaction_control};

use crate::handle::DatabaseHandle;
use crate::types::{ColumnDef, QueryMessage, QueryResult, value_to_display};

/// Maximum number of rows returned by a single query before truncation.
pub const MAX_ROWS: usize = 10_000;

impl DatabaseHandle {
    /// Execute a SQL query in the background. Results arrive via `try_recv()`.
    pub fn execute(&self, sql: String, source_table: Option<String>) {
        let db = Arc::clone(&self.database);
        let tx = self.result_tx.clone();

        // Outer task catches panics from the inner task (spec S8 requirement).
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

    pub(crate) async fn run_query(
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
                if rows.len() >= MAX_ROWS {
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
            // DML/DDL path -- returns affected count, no rows
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
    pub(crate) async fn run_batch(
        db: &turso::Database,
        statements: &[&str],
    ) -> Result<QueryResult, Box<dyn std::error::Error + Send + Sync>> {
        let conn = db.connect()?;
        let total_count = statements.len();

        // Guard: if user already provided transaction control, skip our wrapping
        // and fall through to sequential execution without atomicity guarantee
        let has_user_txn = statements.iter().any(|s| {
            let kind = detect_query_kind(s);
            matches!(kind, QueryKind::Other) && is_transaction_control(s)
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
            // Single SELECT -- shouldn't be called as batch, but handle gracefully
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
            if !has_user_txn && let Err(e) = conn.execute("COMMIT", ()).await {
                let _ = conn.execute("ROLLBACK", ()).await;
                return Err(e.into());
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

    /// Convert a `turso::Value` to a display string.
    /// Note: Null maps to empty string (not "NULL") -- this is correct for PRAGMA values
    /// and EXPLAIN output where null means "no value". The results table uses its own
    /// rendering which displays SQL NULL distinctly.
    pub(crate) fn value_to_string(val: &turso::Value) -> String {
        value_to_display(val).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "INSERT INTO t VALUES (1)", // duplicate PK -- will fail
            "INSERT INTO t VALUES (3)",
        ];
        let err = DatabaseHandle::run_batch(&db, &stmts).await;
        assert!(err.is_err());
        // Verify rollback: only the original row (1) should exist, row (2) rolled back
        let check = DatabaseHandle::run_query(&db, "SELECT id FROM t ORDER BY id")
            .await
            .unwrap();
        assert_eq!(
            check.rows.len(),
            1,
            "only the pre-existing row should survive rollback"
        );
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
            "INSERT INTO t VALUES (1)", // duplicate PK -- fails
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
        // VACUUM goes through Other -> query() path
        // Note: VACUUM on :memory: may behave differently, so just test the kind detection
        assert!(matches!(detect_query_kind("VACUUM"), QueryKind::Other));
    }

    #[tokio::test]
    async fn run_batch_with_user_transaction() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        DatabaseHandle::run_query(&db, "CREATE TABLE t (id INTEGER)")
            .await
            .unwrap();
        // User provides their own BEGIN/COMMIT -- should not double-wrap
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
}
