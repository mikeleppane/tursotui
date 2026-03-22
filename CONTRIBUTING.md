# Contributing to tursotui

Thanks for your interest in contributing! This guide covers the conventions and workflows used in this project.

## Getting Started

1. Fork and clone the repository
2. Install [Rust](https://rustup.rs/) (edition 2024, Rust 1.85+)
3. Build and run tests:

```sh
cargo build
cargo test
```

## Development Workflow

### Build, Format, Lint, Test

Every change must pass all four before submission:

```sh
cargo build                  # build (never release mode during dev)
cargo fmt                    # format code
cargo clippy --all-features --all-targets -- -D warnings  # lint
cargo test                   # run tests
```

### Running

```sh
cargo run -- mydb.sqlite     # run with a database file
cargo run                    # run with :memory: database
```

## Code Conventions

### Rust Standards

- **`pub(crate)` visibility everywhere** — no `pub` exports.
- **`#[forbid(unsafe_code)]`** — no unsafe code allowed, no exceptions.
- **Pedantic clippy** with selective allows — see `[lints.clippy]` in `Cargo.toml`.
- **Unicode-aware string widths** — always use `unicode-width` for display-column calculations, never `.len()`.
- Follow the patterns established in the codebase: `Component` trait, `Action` enum, two-phase dispatch.

### Git Commit Conventions

This project uses [Conventional Commits](https://www.conventionalcommits.org/) with required scopes.

Format: `type(scope): description`

Examples:
```
feat(editor): add multi-cursor support
fix(results): correct column width calculation for CJK characters
refactor(db): extract connection pooling into dedicated module
test(autocomplete): add integration tests for alias resolution
docs(readme): add installation instructions
```

Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`, `style`, `ci`, `build`

### Architecture

See `docs/specs/design.md` for the full design specification.

Key patterns:
- **Unidirectional data flow** — components emit `Action`s, `AppState` processes them, results route back.
- **Component trait** — each panel implements `handle_key`, `update`, `render`.
- **Two-phase dispatch** — `AppState::update()` handles state, then `dispatch_action_to_components()` routes to components and I/O.
- **Async queries** — `tokio::spawn` with fresh connections, results via `mpsc` channel.

## AI-Assisted Development

This project includes [Claude Code](https://docs.anthropic.com/en/docs/claude-code) skills in `.claude/skills/` that encode project conventions for AI-assisted development:

- **`rust-standards`** — Rust coding standards covering ownership patterns, type safety, clean code, and performance-conscious design.
- **`git-conventions`** — commit message conventions with enforced scopes and conventional commit format.
- **`improve-architecture`** — guidelines for finding architectural improvement opportunities, focusing on testability and module design.

If you use Claude Code (or another AI coding assistant that supports skills), these will automatically guide contributions to match project standards. The project also has a `CLAUDE.md` file with architecture context and quick-reference commands.

## Submitting Changes

1. Create a feature branch from `main`
2. Make your changes, following the conventions above
3. Ensure all four checks pass: build, fmt, clippy, test
4. Submit a pull request with a clear description of the change and motivation

## Project Structure

```
src/
  main.rs              # Entry point, event loop, terminal setup
  app.rs               # AppState, Action enum, state management
  db.rs                # DatabaseHandle, query execution, schema loading
  config.rs            # TOML configuration
  theme.rs             # Color themes
  highlight.rs         # SQL syntax highlighting
  autocomplete.rs      # Autocomplete engine
  export.rs            # Export formatting (CSV/JSON/SQL)
  persistence.rs       # Editor buffer persistence
  history.rs           # Query history (SQLite-backed)
  event.rs             # Event loop helpers
  components/
    mod.rs             # Component trait
    schema.rs          # Schema explorer tree
    editor.rs          # Query editor
    results.rs         # Results table
    record.rs          # Record detail view
    explain.rs         # EXPLAIN view
    autocomplete.rs    # Autocomplete popup UI
    export.rs          # Export popup UI
    history.rs         # Query history popup
    db_info.rs         # Database info panel
    pragmas.rs         # PRAGMA dashboard
    help.rs            # Help overlay
    status_bar.rs      # Status bar renderer
```

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
