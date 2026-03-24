//! `DatabaseHandle` struct and basic connection management.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::types::QueryMessage;

/// Wraps an `Arc<Database>` and provides a channel for receiving query results.
/// One per open database.
pub struct DatabaseHandle {
    pub(crate) database: Arc<turso::Database>,
    pub(crate) result_rx: mpsc::UnboundedReceiver<QueryMessage>,
    pub(crate) result_tx: mpsc::UnboundedSender<QueryMessage>,
}

impl DatabaseHandle {
    /// Open a database at the given path.
    pub async fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let database = turso::Builder::new_local(path)
            .experimental_custom_types(true)
            .build()
            .await?;
        let (result_tx, result_rx) = mpsc::unbounded_channel();

        Ok(Self {
            database: Arc::new(database),
            result_rx,
            result_tx,
        })
    }

    /// Get a clone of the database `Arc` for spawning query tasks.
    pub fn database(&self) -> Arc<turso::Database> {
        Arc::clone(&self.database)
    }

    /// Get a clone of the sender for spawning query tasks.
    pub fn sender(&self) -> mpsc::UnboundedSender<QueryMessage> {
        self.result_tx.clone()
    }

    /// Create a fresh, independent connection for a query task.
    pub fn connect(&self) -> Result<turso::Connection, Box<dyn std::error::Error>> {
        Ok(self.database.connect()?)
    }

    /// Check for completed query results (non-blocking).
    ///
    /// `Disconnected` cannot occur here because `self` holds `result_tx` -- the channel
    /// stays open as long as the handle exists. Spawned tasks clone the sender via
    /// `sender()`, so even if all tasks complete, the original sender keeps the channel alive.
    pub fn try_recv(&mut self) -> Option<QueryMessage> {
        self.result_rx.try_recv().ok()
    }

    /// Wait for a completed query result (async, blocking).
    ///
    /// Returns `None` only if all senders are dropped, which cannot happen while `self`
    /// is alive (it holds `result_tx`). In practice this always returns `Some`.
    pub async fn recv(&mut self) -> Option<QueryMessage> {
        self.result_rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_memory_database_succeeds() {
        let handle = DatabaseHandle::open(":memory:").await;
        assert!(handle.is_ok(), "opening in-memory DB should succeed");
    }

    #[tokio::test]
    async fn connect_returns_connections_sharing_same_database() {
        let handle = DatabaseHandle::open(":memory:").await.unwrap();
        let conn1 = handle.connect().unwrap();
        let conn2 = handle.connect().unwrap();

        // Verify both connections share the same underlying database (Arc<Database>)
        conn1
            .execute("CREATE TABLE t (x INTEGER)", ())
            .await
            .unwrap();
        let mut rows = conn2
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='t'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "conn2 should see table created by conn1 — both share the same database"
        );
    }

    #[tokio::test]
    async fn try_recv_returns_none_when_empty() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        assert!(
            handle.try_recv().is_none(),
            "fresh handle should have no pending messages"
        );
    }

    #[tokio::test]
    async fn sender_can_send_and_recv() {
        let mut handle = DatabaseHandle::open(":memory:").await.unwrap();
        let tx = handle.sender();
        tx.send(QueryMessage::Failed("test".into())).unwrap();
        let msg = handle.try_recv();
        assert!(
            matches!(msg, Some(QueryMessage::Failed(ref s)) if s == "test"),
            "should receive the sent message"
        );
    }
}
