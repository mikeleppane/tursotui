//! Database operations for tursotui: handle management, query execution,
//! schema loading, pragma operations, and maintenance tasks.

pub mod handle;
pub mod ops;
pub mod pragma;
pub mod profile;
pub mod query;
pub mod schema;
pub mod types;

// Re-export primary types
pub use handle::DatabaseHandle;
pub use types::{
    ColumnDef, ColumnInfo, ColumnProfile, CustomTypeInfo, DbInfo, IndexDetail, PragmaEntry,
    ProfileData, QueryMessage, QueryParams, QueryResult, SchemaEntry,
};

// Re-export from tursotui-sql
pub use tursotui_sql::parser::ForeignKeyInfo;
pub use tursotui_sql::query_kind::QueryKind;

// Re-export turso::Value so binary crate can drop direct turso dep
pub use turso::Value;

/// Version of the turso crate.
pub const TURSO_VERSION: &str = "0.6.0-pre.5";
