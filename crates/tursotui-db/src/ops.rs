//! Maintenance operations: EXPLAIN, WAL checkpoint, integrity check, transactions.

use std::sync::Arc;
use std::time::Instant;

use tursotui_sql::parser::detect_statements;
use tursotui_sql::query_kind::QueryKind;

use crate::handle::DatabaseHandle;
use crate::types::{ColumnDef, QueryMessage, QueryResult};

impl DatabaseHandle {
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
        // Use only the first statement to prevent injecting extra statements
        // after the EXPLAIN prefix (e.g., "SELECT 1; DROP TABLE x").
        let statements = detect_statements(sql);
        let first_stmt = statements.first().ok_or("empty SQL")?;

        let conn = db.connect()?;

        // Run EXPLAIN <sql> -- collects bytecode rows
        let mut rows = conn.query(&format!("EXPLAIN {first_stmt}"), ()).await?;
        let col_count = rows.column_count();
        let mut bytecode_rows = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut cells = Vec::with_capacity(col_count);
            for idx in 0..col_count {
                cells.push(Self::value_to_string(&row.get_value(idx)?));
            }
            bytecode_rows.push(cells);
        }

        // Run EXPLAIN QUERY PLAN <first_stmt> -- collects plan lines
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {first_stmt}"), ())
            .await?;
        let mut plan_lines = Vec::new();
        while let Some(row) = rows.next().await? {
            // EXPLAIN QUERY PLAN returns columns: id, parent, notused, detail
            // We want the detail column (index 3)
            let detail = row.get_value(3)?.as_text().cloned().unwrap_or_default();
            plan_lines.push(detail);
        }

        Ok((bytecode_rows, plan_lines))
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
    pub fn execute_transaction(&self, statements: Vec<String>) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
