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

Key patterns:
- Component trait: each panel implements `handle_key`, `update`, `render`
- Action enum: all state mutations flow through actions (unidirectional data flow)
- DatabaseHandle: stores Arc<Database>, creates fresh connections per query task
- Event loop: crossterm poll (16ms) → Action → update → render

## Dependencies

- `turso` crate is a PATH dependency pointing to `../turso/bindings/rust`
- `ratatui` 0.30 with crossterm backend (re-exported via `ratatui::crossterm`)
- `tokio` for async runtime (turso Builder::build() is async)
