// Temporary compatibility shim -- re-exports from tursotui-db.
// Will be removed in Phase 4 when all callers import directly.
pub(crate) use tursotui_db::{
    ColumnDef, ColumnInfo, CustomTypeInfo, DatabaseHandle, DbInfo, ForeignKeyInfo, PragmaEntry,
    QueryKind, QueryMessage, QueryResult, SchemaEntry,
};

pub(crate) use tursotui_sql::parser::{detect_statements, parse_foreign_keys};
pub(crate) use tursotui_sql::validation::sanitize_pragma_value;
