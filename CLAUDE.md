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
- **UiPanels** (`src/main.rs`): groups component instances; will move into `DatabaseContext` when multi-db lands (Milestone 7)
- **Status bar** (`src/components/status_bar.rs`): a render function, NOT a Component — no key handling, reads from AppState

### Conventions

- `pub(crate)` visibility everywhere (no `pub` exports)
- `#[forbid(unsafe_code)]` — no unsafe allowed
- Pedantic clippy with selective allows — see `[lints.clippy]` in Cargo.toml
- All string width calculations must use `unicode-width`, never `.len()` (byte count)

## Dependencies

- `turso` 0.6.0-pre.5 from crates.io (default-features = false to avoid mimalloc override)
- `ratatui` 0.30 with crossterm backend (re-exported via `ratatui::crossterm`)
- `tokio` for async runtime (turso Builder::build() is async)
- `unicode-width` for display-column width measurement (critical for non-ASCII)
