use std::sync::Arc;

use tokio::sync::mpsc;

/// Messages sent from query tasks back to the main loop.
/// Intentionally empty — no variants can be constructed until query execution is added.
#[derive(Debug)]
pub(crate) enum QueryMessage {}

/// Wraps an `Arc<Database>` and provides a channel for receiving query results.
/// One per open database.
pub(crate) struct DatabaseHandle {
    #[allow(dead_code)]
    database: Arc<turso::Database>,
    #[allow(dead_code)]
    result_rx: mpsc::UnboundedReceiver<QueryMessage>,
    #[allow(dead_code)]
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
    /// `Disconnected` cannot occur here because `self` holds `result_tx` — the channel
    /// stays open as long as the handle exists. Spawned tasks clone the sender via
    /// `sender()`, so even if all tasks complete, the original sender keeps the channel alive.
    #[allow(dead_code)]
    pub fn try_recv(&mut self) -> Option<QueryMessage> {
        self.result_rx.try_recv().ok()
    }
}
