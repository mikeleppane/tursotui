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

### Turso/libsql gotchas

- **PRAGMA `foreign_key_list` is NOT supported** by turso/libsql — returns "Not a valid pragma name". FK info must be parsed from CREATE TABLE SQL in `SchemaEntry::sql` using `parse_foreign_keys()` in `db.rs`.
- **PRAGMA syntax quirk**: turso uses single-quoted values in PRAGMA SET (`PRAGMA name = 'value'`), not double-quoted.
- **Transaction execution**: use `PRAGMA defer_foreign_keys = ON` inside the transaction preamble to avoid FK constraint violations during multi-statement DML.

## Dependencies

- `turso` 0.6.0-pre.5 from crates.io (default-features = false to avoid mimalloc override)
- `ratatui` 0.30 with crossterm backend (re-exported via `ratatui::crossterm`)
- `tokio` for async runtime (turso Builder::build() is async)
- `unicode-width` for display-column width measurement (critical for non-ASCII)
- `serde` + `toml` for config serialization
- `dirs` for platform config/data directory paths
- `arboard` for clipboard (default-features = false for SSH-safe fallback)
