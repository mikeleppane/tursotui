# tursotui — Terminal UI for Turso/SQLite

## Quick Reference

    cargo build                  # build (never release mode during dev)
    cargo run -- test.db         # run with a database file
    cargo run                    # run with :memory: database
    cargo test                   # run tests
    cargo fmt                    # format
    cargo clippy --all-features --all-targets -- -D warnings  # lint

## Architecture

See `docs/specs/design.md` for the full design spec.
Milestone plans live in `docs/plans/`.

### Key patterns

- **Component trait** (`src/components/mod.rs`): each panel implements `handle_key`, `update`, `render`
- **Action enum** (`src/app.rs`): all state mutations flow through actions (unidirectional data flow)
- **Two-phase dispatch**: `AppState::update()` handles state changes, then `dispatch_action_to_components()` in main.rs routes to components and I/O
- **DatabaseHandle** (`src/db.rs`): stores `Arc<Database>`, creates fresh connections per query task via `tokio::spawn`
- **Event loop** (`src/main.rs`): drain async channel → poll crossterm (16ms) → route to focused component → fallback to global keys → render
- **UiPanels** (`src/main.rs`): groups component instances
- **Status bar** (`src/components/status_bar.rs`): a render function, NOT a Component — no key handling, reads from AppState
- **Panel block helpers** (`src/components/mod.rs`): `panel_block()` and `overlay_block()` produce consistently-styled `Block` widgets with rounded borders and padded titles — all components use these instead of building blocks directly
- **Data editor** (`src/components/data_editor.rs`): manages ChangeLog (one-entry-per-PK invariant), DML generation, FK navigation stack, and EditRenderState for visual overlay injection into ResultsTable
- **Theme system** (`src/theme.rs`): Catppuccin Mocha (dark) and Latte (light) with semantic color roles — all colors referenced via Theme fields, never hardcoded

### Conventions

- `pub(crate)` visibility everywhere (no `pub` exports)
- `#[forbid(unsafe_code)]` — no unsafe allowed
- Pedantic clippy with selective allows — see `[lints.clippy]` in Cargo.toml
- All string width calculations must use `unicode-width`, never `.len()` (byte count)
- All panel borders use `BorderType::Rounded` via the `panel_block`/`overlay_block` helpers — never create `Block::bordered()` directly
- SQL identifiers must be escaped with `quote_identifier()` (double-quotes), literals with `quote_literal()` (single-quotes) — both in `data_editor.rs`
- Status bar is intentionally minimal — show panel name + context info + "F1 Help", no keybinding cheat sheets (those belong in the help overlay)

### Turso/libsql compatibility

The authoritative reference is [COMPAT.md](https://github.com/tursodatabase/turso/blob/main/COMPAT.md).
Turso aims for full SQLite compatibility but has gaps that directly affect this project.

#### PRAGMA limitations

- **`foreign_key_list` NOT supported** — returns "Not a valid pragma name". FK info must be parsed from CREATE TABLE SQL in `SchemaEntry::sql` using `parse_foreign_keys()` in `db.rs`.
- **`defer_foreign_keys` NOT supported** — returns "Not a valid pragma name". Intentionally omitted from turso's pragma whitelist (`parser/src/ast.rs PragmaName` enum). Turso does support `DEFERRABLE INITIALLY DEFERRED` on column constraints and `BEGIN DEFERRED` transactions, but not the connection-level pragma. Our `execute_transaction` in `ops.rs` no longer calls this pragma — FK checks run immediately per-statement, so DML generation must order statements to respect FK dependencies.
- **Syntax quirk**: turso uses single-quoted values in PRAGMA SET (`PRAGMA name = 'value'`), not double-quoted.
- **65+ PRAGMAs work**, including: `foreign_keys`, `journal_mode`, `cache_size`, `page_size`, `table_info`, `index_info`, `integrity_check`, `quick_check`, `user_version`, `busy_timeout`.
- **`synchronous`** only supports `OFF` and `FULL` (not `NORMAL`).
- **Journal mode** restricted to WAL and MVCC only — rollback modes (`DELETE`, `TRUNCATE`, `PERSIST`, `MEMORY`) are NOT supported.
- **Notable unsupported PRAGMAs**: `auto_vacuum`, `mmap_size`, `locking_mode`, `optimize`, `secure_delete`, `recursive_triggers`, `collation_list`, `compile_options`, `threads`.
- **Turso-specific PRAGMAs**: `capture_data_changes_conn` (CDC), `cipher`/`hexkey` (encryption, experimental), `list_types` (custom type inspection).

#### SQL feature gaps

- **Window functions partial** — only aggregate functions (`count`, `sum`, `avg`, `min`, `max`, `total`, `group_concat`) work as window functions. Dedicated window functions (`ROW_NUMBER()`, `RANK()`, `DENSE_RANK()`, `NTILE()`, `LAG()`, `LEAD()`, `FIRST_VALUE()`, `LAST_VALUE()`, `NTH_VALUE()`, `CUME_DIST()`, `PERCENT_RANK()`) are NOT supported. No custom frame specifications (`ROWS BETWEEN`, explicit `RANGE BETWEEN`, `GROUPS BETWEEN`). No `EXCLUDE` clause. `FILTER (WHERE...)` not supported on window functions.
- **CTEs partial** — `WITH` works but no `RECURSIVE`, no `MATERIALIZED`/`NOT MATERIALIZED` hints, only `SELECT` in CTE body.
- **JOINs** — `INNER JOIN`, `LEFT OUTER JOIN`, `FULL OUTER JOIN`, `NATURAL JOIN`, `JOIN...USING` work. No `RIGHT JOIN`, no `CROSS JOIN`.
- **Not supported** — `SAVEPOINT`/`RELEASE`, `GENERATED` columns, `INDEXED BY`, `REINDEX`, `VACUUM`, `MATCH` operator, `CREATE TEMPORARY TABLE`, `CREATE TABLE ... AS SELECT`, `WITHOUT ROWID` tables, custom collations (only `BINARY`/`NOCASE`/`RTRIM`), `!<`/`!>` operators, loading `.so`/`.dll` SQLite extensions.
- **Binary `%` operator** not supported in expressions.
- **Subqueries** — scalar subqueries only; tuple comparisons with subqueries don't work.
- **Views and Triggers** — experimental features requiring explicit enablement via `--experimental-views` / `--experimental-triggers` flags or SDK builder methods. API may change.
- **FTS syntax differs from SQLite FTS5** — Turso uses Tantivy-based FTS with `CREATE INDEX ... USING fts`, `fts_match()`, `fts_score()`, `fts_highlight()` instead of FTS5's virtual tables, `MATCH`, `bm25()`, `highlight()`.

#### What DOES work well

- All core DML/DDL: `CREATE TABLE`, `ALTER TABLE`, `INSERT` (with `UPSERT`/`RETURNING`), `UPDATE`, `DELETE`, `SELECT`
- `GROUP BY`, `HAVING`, `ORDER BY`, `LIMIT`, `LIKE`, `GLOB`, `BETWEEN`, `IN`, `EXISTS`, `CASE WHEN`
- Transactions: `BEGIN`/`COMMIT`/`ROLLBACK` (also `BEGIN CONCURRENT` for MVCC)
- 80+ scalar functions: `printf`, `coalesce`, `ifnull`, `substr`, `replace`, `round`, `abs`, `hex`, `typeof`, `json_*`, all trig/math functions
- Aggregate functions: `avg`, `count`, `group_concat`, `sum`, `total`, `min`, `max`
- Full date/time functions: `date`, `time`, `datetime`, `strftime`, `unixepoch`, `julianday`
- 30+ JSON functions including `json_extract`, `json_each`, operators (`->`, `->>`)
- Built-in extensions: UUID (`uuid4()`, `uuid7()`), regexp, vector search, FTS (Tantivy-powered), CSV virtual tables, `generate_series`, percentile aggregates
- Turso-specific: `stddev()` aggregate, `timediff()`, CDC helper functions (`table_columns_json_array()`, `bin_record_json_object()`)
- Custom types for STRICT tables: built-in `boolean`, `varchar(N)`, `date`, `time`, `timestamp`, `numeric(P,S)`, `uuid`, `inet`, `bytea`, `json`, `jsonb` (experimental, requires enablement)
- Native vector types: `FLOAT64`, `FLOAT32`, `FLOAT16`, `FLOATB16`, `FLOAT8`, `FLOAT1BIT` with DiskANN indexing via `libsql_vector_idx()`

#### Rust SDK notes

- Crate: `libsql` (our `turso` crate wraps it). `Builder::new_local(path).build().await?` for local files.
- Other connection modes (future): `Builder::new_remote(url, token)` for cloud, `Builder::new_remote_replica(path, url, token)` for embedded replicas with `sync_interval()` and `read_your_writes(true)`.
- Crate features: `remote` (HTTP-only, no C compiler), `core` (local SQLite, needs C), `replication` (embedded replicas, needs C), `encryption` (at-rest, needs cmake).
- `db.connect()?` gives a `Connection`. Each connection is independent — safe to create per-task.
- Positional params: `libsql::params![val]` with `?1` placeholders. Named: `libsql::named_params!{":key": val}`.
- `execute_batch()` runs multiple statements in an implicit transaction — all-or-nothing.
- `EXCLUSIVE` behaves identically to `IMMEDIATE` in WAL mode. Only one active transaction per connection.
- `de::from_row` can deserialize into serde structs (not used in this project, but available).
- No concurrent multi-process access to the same database file.

## Dependencies

- `turso` 0.6.0-pre.5 from crates.io (default-features = false to avoid mimalloc override)
- `ratatui` 0.30 with crossterm backend (re-exported via `ratatui::crossterm`)
- `tokio` for async runtime (turso Builder::build() is async)
- `unicode-width` for display-column width measurement (critical for non-ASCII)
- `serde` + `toml` for config serialization
- `dirs` for platform config/data directory paths
- `arboard` for clipboard (default-features = false for SSH-safe fallback)

## Known issues discovered via testing

### `execute_transaction` was failing on turso due to `defer_foreign_keys` (FIXED)

`ops.rs::execute_transaction()` was calling `PRAGMA defer_foreign_keys = ON` before every transaction. Turso returns "Not a valid pragma name", so every data edit transaction failed. Fixed by removing the pragma call. FK checks now run immediately per-statement — DML generation in `data_editor.rs` must order statements to respect FK dependencies (parent inserts before child inserts, child deletes before parent deletes).

### History search does not escape SQL LIKE wildcards

`history.rs::load_entries()` uses `LIKE '%{term}%'` for search filtering. The `%` and `_` characters in search terms are not escaped, so searching for `id_col` also matches `idXcol` (the `_` acts as a single-character wildcard). Low severity but affects search precision.

### `HistoryDb` schema extracted into `create_schema()`

Schema creation is in `HistoryDb::create_schema()`, shared by both `open()` and the test helper `test_history_db()`. No duplication.
