#![allow(
    dead_code,
    reason = "QueryMessage variants and some methods not used until later milestones"
)]

use std::sync::Arc;

use tokio::sync::mpsc;

/// Messages sent from query tasks back to the main loop.
/// Intentionally empty — no variants can be constructed until query execution is added.
#[derive(Debug)]
pub(crate) enum QueryMessage {}

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
    /// Returns `Some` if a message is available, `None` if the channel is empty.
    /// Logs a debug warning if the channel is disconnected (sender dropped unexpectedly).
    pub fn try_recv(&mut self) -> Option<QueryMessage> {
        match self.result_rx.try_recv() {
            Ok(msg) => Some(msg),
            Err(mpsc::error::TryRecvError::Empty) => None,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                debug_assert!(false, "query result channel disconnected unexpectedly");
                None
            }
        }
    }
}
