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
- **`defer_foreign_keys` NOT supported** at the PRAGMA level. Use `PRAGMA defer_foreign_keys = ON` inside transaction SQL strings (our `execute_transaction` does this).
- **Syntax quirk**: turso uses single-quoted values in PRAGMA SET (`PRAGMA name = 'value'`), not double-quoted.
- **65+ PRAGMAs work**, including: `foreign_keys`, `journal_mode`, `cache_size`, `page_size`, `table_info`, `index_info`, `integrity_check`, `quick_check`, `user_version`, `busy_timeout`.
- **Notable unsupported PRAGMAs**: `auto_vacuum`, `mmap_size`, `locking_mode`, `optimize`, `secure_delete`, `recursive_triggers`, `collation_list`, `compile_options`.

#### SQL feature gaps

- **Window functions partial** — default frame definitions work but ranking functions (`RANK()`, `ROW_NUMBER()`, `DENSE_RANK()`) fail with "no such function". `FILTER (WHERE...)` on aggregates is silently ignored.
- **CTEs partial** — `WITH` works but no `RECURSIVE`, no `MATERIALIZED` hint, only `SELECT` in CTE body.
- **JOINs** — no `RIGHT JOIN`, no `CROSS JOIN`. `LEFT JOIN` and `NATURAL JOIN` work.
- **Not supported** — `SAVEPOINT`/`RELEASE`, `GENERATED` columns, `INDEXED BY`, `REINDEX`, `VACUUM`, `MATCH` operator.
- **Binary `%` operator** not supported in expressions.
- **Subqueries** — scalar subqueries only; tuple comparisons with subqueries don't work.

#### What DOES work well

- All core DML/DDL: `CREATE TABLE`, `ALTER TABLE`, `INSERT` (with `UPSERT`/`RETURNING`), `UPDATE`, `DELETE`, `SELECT`
- `GROUP BY`, `HAVING`, `ORDER BY`, `LIMIT`, `LIKE`, `GLOB`, `BETWEEN`, `IN`, `EXISTS`, `CASE WHEN`
- Transactions: `BEGIN`/`COMMIT`/`ROLLBACK` (also `BEGIN CONCURRENT` for MVCC)
- 80+ scalar functions: `printf`, `coalesce`, `ifnull`, `substr`, `replace`, `round`, `abs`, `hex`, `typeof`, `json_*`, all trig/math functions
- Aggregate functions: `avg`, `count`, `group_concat`, `sum`, `total`, `min`, `max`
- Full date/time functions: `date`, `time`, `datetime`, `strftime`, `unixepoch`, `julianday`
- 30+ JSON functions including `json_extract`, `json_each`, operators (`->`, `->>`)
- Built-in extensions: UUID (`uuid4()`, `uuid7()`), regexp, vector search, FTS (Tantivy-powered), CSV virtual tables, `generate_series`, percentile aggregates

#### Rust SDK notes

- Crate: `libsql` (our `turso` crate wraps it). `Builder::new_local(path).build().await?` for local files.
- `db.connect()?` gives a `Connection`. Each connection is independent — safe to create per-task.
- Positional params: `libsql::params![val]` with `?1` placeholders. Named: `libsql::named_params!{":key": val}`.
- `execute_batch()` runs multiple statements in an implicit transaction — all-or-nothing.
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
