<p align="center">
  <img src="images/logo.png" alt="tursotui logo" width="600">
</p>

<p align="center">
  A keyboard-driven terminal UI for browsing, querying, and administering Turso and SQLite databases.
  <br>
  Built with Rust, <a href="https://ratatui.rs">ratatui</a>, and vim-inspired navigation.
</p>

<p align="center">
  <a href="https://github.com/mikeleppane/tursotui/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-1.85%2B-orange.svg" alt="Rust: 1.85+"></a>
  <a href="https://github.com/mikeleppane/tursotui"><img src="https://img.shields.io/badge/turso--first-SQLite--compatible-teal" alt="Turso-first"></a>
</p>

## Table of Contents

- [Demo](#demo)
- [Screenshots](#screenshots)
- [Features](#features)
- [Turso Compatibility](#turso-compatibility)
- [Installation](#installation)
- [Usage](#usage)
- [Keybindings](#keybindings)
- [Configuration](#configuration)
- [Architecture](#architecture)
- [Tech Stack](#tech-stack)
- [License](#license)
- [Acknowledgments](#acknowledgments)
- [Contact](#contact)

## Demo

<p align="center">
  <a href="https://youtu.be/bo3zhI8sRZY">
    <img src="https://img.youtube.com/vi/bo3zhI8sRZY/maxresdefault.jpg" alt="tursotui demo video" width="700">
  </a>
  <br>
  <em>Click the image to watch the demo on YouTube</em>
</p>

## Screenshots

<p align="center">
  <img src="images/screenshot1.png" alt="Schema browser, SQL editor, and results table" width="700">
  <br>
  <em>Schema browser with row counts, SQL editor with syntax highlighting, and sortable results table</em>
</p>

<p align="center">
  <img src="images/screenshot2.png" alt="DML Preview popup showing generated SQL statements" width="700">
  <br>
  <em>DML Preview — review generated INSERT, UPDATE, and DELETE statements before committing</em>
</p>

<p align="center">
  <img src="images/screenshot3.png" alt="Bookmarks overlay with saved queries and SQL preview" width="700">
  <br>
  <em>Saved Bookmarks — name, search, and recall frequently-used queries</em>
</p>

<p align="center">
  <img src="images/screenshot4.png" alt="ER Diagram with table relationships and FK edges" width="700">
  <br>
  <em>ER Diagram — visual entity-relationship view with FK edges, PK/FK markers, and cycle detection</em>
</p>

## Features

**Multi-Database Tabs** — open multiple databases simultaneously with a tab bar. Switch between them with `Ctrl+PgDn`/`Ctrl+PgUp`, open new databases with `Ctrl+O` file picker, close with `Ctrl+W`. Each database has independent schema, editor, and results state.

**Schema Browser** — color-coded tree view of tables, views, indexes, triggers, and columns with inline search filtering, async row counts, and DDL viewing. Each entity type has a distinct color for quick visual scanning.

**SQL Editor** — syntax-highlighted editor with undo/redo, text selection, auto-save, active line highlighting, and statement-at-cursor execution. Supports parameterized queries with bound parameters.

**Parameterized Queries** — when the editor detects SQL parameters (`?1`, `:name`, `$name`), a parameter bar appears below the editor. Tab between fields, type values, use `Ctrl+N` for NULL. Parameters are bound safely via the database driver — no string interpolation. Values persist across restarts and are logged in query history.

**Schema-Aware Autocomplete** — context-sensitive completions for table names, columns, SQL keywords, and qualified references with alias resolution.

**Results Table** — sortable, resizable columns with alternating row colors, cell/row clipboard copy, JSON pretty-printing, and configurable NULL display. Indexed columns are marked with a `·` indicator in the header for quick identification of indexed vs. non-indexed columns.

**Index Awareness** — columns that are the leading key of a database index are marked with `·` in the results header. When filtering on a non-indexed column (via the `w` WHERE filter) on a table with >1,000 rows, a brief hint warns that the query may be slow.

**Inline Data Editor** — edit table data directly in the results view. Add, modify, and delete rows with full change tracking. Preview generated DML (INSERT/UPDATE/DELETE) before committing. Transactional submission with automatic rollback on failure.

**Foreign Key Navigation** — follow FK references from any cell to the referenced row. Breadcrumb trail with back-navigation to retrace your path through related tables.

**Record Detail** — vertical key-value view for inspecting a single row across all columns, with JSON syntax coloring for structured values.

**ER Diagram** — visual entity-relationship diagram built from foreign key definitions. Grid layout with box-drawing borders, PK/FK markers, relationship edges, cycle detection with dashed lines, and adjustable spacing.

**Go to Object** — fuzzy search across all open databases (`Ctrl+P`). Instantly navigate to any table, view, index, trigger, or column with ranked results.

**Enhanced EXPLAIN View** — bytecode table and query plan tree, toggled with a single key. Query plan lines are color-coded by scan type (red for full table scans, yellow for temp B-trees, green for index seeks). Warnings section highlights performance issues and suggests CREATE INDEX statements based on WHERE/ORDER BY columns. Press Enter on a suggestion to send it to the editor, or `y` to copy to clipboard.

**Data Profiling** — press `5` to open the Profile tab, then `Enter` to generate. Two-column layout: left shows columns with colored completeness indicators (`●` green = no nulls, `◐` yellow = <50% nulls, `○` red = ≥50% nulls, `∅` dim = all null), right shows per-column statistics including null count, distinct count, uniqueness ratio, min/max, and for numeric columns: avg, sum, stddev (Turso). Text columns show length statistics. Top-5 value frequency with bar chart visualization (hidden for high-cardinality columns with >50 distinct values). Automatic sampling for tables over 10,000 rows (configurable via `[profile] sample_threshold` in config). Profile auto-invalidates on DML operations — stale indicator (`*`) appears on the tab. Press `r` to refresh, `Ctrl+Up/Down` to scroll stats.

**Schema Diffing** — press `F7` to compare schemas between two open databases. Visual diff overlay shows added (+), removed (-), modified (~), and identical (=) objects with color-coded status icons. Expand modified tables to see column-level diffs including type changes. Copy DDL or auto-generated migration SQL (ALTER TABLE for added columns, 12-step rebuild guidance for type changes) to clipboard. Toggle visibility of identical objects with `i`.

**Slow Query Tracking** — execution time in the status bar is color-coded against a configurable threshold (`[performance] slow_query_ms` in config, default 500ms): yellow for slow, red for very slow. Query history overlay adds `s` to filter slow queries only and `S` to sort by execution time descending. Slow entries are marked with a `⏱` icon.

**Export** — save results as CSV, JSON, or SQL INSERT statements to file or clipboard. Quick TSV copy with a shortcut.

**Saved Bookmarks** — name and save frequently-used queries (`F3`). Database-scoped with search, rename, delete, and one-key recall or execute. Backed by SQLite for persistence across sessions.

**Quick Table Filter** — press `w` on the results panel to type a WHERE clause and instantly filter table data without touching the editor. Two-phase dismiss (defocus then clear).

**DDL Viewer** — press `Shift+D` on any schema object to view its full CREATE statement with syntax highlighting, scrolling, and clipboard copy.

**Query History** — SQLite-backed per-database history with search, recall, re-execute, auto-prune, slow-query filtering, and execution time sorting.

**Admin Tab** — database info (file stats, WAL status, journal mode), PRAGMA dashboard with inline editing, WAL checkpoint, and integrity checks.

**Mouse Support** — click-to-focus panels, click-to-select rows and columns, click-to-expand schema tree nodes, mousewheel scroll on all panels, drag panel borders to resize, click tab bars to switch. Toggle with `F8`. Hold `Shift+Click` for native terminal text selection when mouse mode is enabled.

**Theming** — Catppuccin Mocha (dark) and Catppuccin Latte (light) themes with rounded borders, toggled at runtime.

## Turso Compatibility

tursotui is **Turso-first** — all features are tested and designed to work within Turso's compatibility surface. It also works with standard SQLite databases.

**Turso-aware highlights:**

- Schema introspection uses Turso-compatible PRAGMAs (`table_info`, `index_list`, etc.)
- Foreign key metadata parsed from `CREATE TABLE` SQL (works around Turso's missing `PRAGMA foreign_key_list`)
- Syntax highlighting and autocomplete include Turso-specific functions: UUID (`uuid4()`, `uuid7()`), vector search (`vector_distance_cos()`, `vector_top_k()`), FTS (`fts_match()`, `fts_score()`), time functions, regexp, and more
- DB Info panel recognizes MVCC journal mode with Turso-specific labeling
- Data editor generates FK-aware DML ordering (parent inserts before child, child deletes before parent)

**Known Turso limitations** (not tursotui bugs — these are upstream gaps):

- No `RIGHT JOIN`, `CROSS JOIN`, or ranking window functions (`ROW_NUMBER`, `RANK`, etc.)
- No `SAVEPOINT`/`RELEASE`, `VACUUM`, `REINDEX`, `GENERATED` columns, or recursive CTEs
- Views and Triggers are experimental features requiring enablement flags
- FTS uses different syntax than SQLite FTS5 (`fts_match()` instead of `MATCH`)

See [COMPAT.md](https://github.com/tursodatabase/turso/blob/main/COMPAT.md) for the full Turso compatibility matrix.

## Installation

### From source

Requires [Rust](https://rustup.rs/) (edition 2024, Rust 1.85+).

```sh
git clone https://github.com/mikeleppane/tursotui.git
cd tursotui
cargo build --release
```

The binary is at `target/release/tursotui`.

## Usage

```sh
# Open a Turso/SQLite database file
tursotui mydb.db

# Open an in-memory database
tursotui

# Open multiple databases in tabs
tursotui db1.db db2.db
```

## Keybindings

### Global

| Key | Action |
|-----|--------|
| `Ctrl+Q` | Quit |
| `Ctrl+Tab` | Cycle focus between panels |
| `Ctrl+B` | Toggle schema sidebar |
| `Alt+1` / `Alt+2` | Switch Query / Admin tab |
| `Ctrl+T` | Toggle dark/light theme |
| `F1` / `?` | Help overlay |
| `Ctrl+O` | Open database file |
| `Ctrl+P` | Go to Object (fuzzy search) |
| `Ctrl+PgDn` / `Ctrl+PgUp` | Next / previous database tab |
| `Ctrl+W` | Close current database tab |
| `Ctrl+Left` / `Ctrl+Right` | Resize sidebar (narrower / wider) |
| `Ctrl+Up` / `Ctrl+Down` | Resize editor (shorter / taller) |
| `F3` | Bookmarks overlay |
| `Ctrl+Shift+E` | Export results |
| `Ctrl+Shift+C` | Quick copy results (TSV) |
| `F8` | Toggle mouse mode |

### Query Editor

| Key | Action |
|-----|--------|
| `F5` / `Ctrl+Enter` | Execute query |
| `Ctrl+Shift+Enter` | Execute selection or statement at cursor |
| `Ctrl+Space` | Trigger autocomplete |
| `Tab` | Accept completion |
| `Ctrl+Z` / `Ctrl+Y` | Undo / Redo |
| `Ctrl+L` | Clear buffer |
| `Ctrl+H` | Query history |
| `Shift+Arrow` | Extend selection |
| `Ctrl+Shift+A` | Select all |

### Schema Explorer

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate up/down |
| `Enter` / `Space` / `l` | Expand / collapse |
| `h` | Collapse / go to parent |
| `o` | Query table (`SELECT *`) |
| `Shift+D` | View DDL (CREATE statement) |
| `/` | Filter by name |

### Results Table

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate rows |
| `h` / `l` | Navigate columns |
| `g` / `G` | First / last row |
| `s` | Cycle sort on column |
| `<` / `>` | Shrink / grow column |
| `w` | WHERE filter bar |
| `y` | Copy cell |
| `Y` | Copy row |

### Data Editor (when results are editable)

| Key | Action |
|-----|--------|
| `e` / `F2` | Edit current cell |
| `Enter` | Confirm cell edit |
| `Esc` | Cancel cell edit |
| `Ctrl+N` | Set cell to NULL |
| `Ctrl+Enter` / `F10` | Confirm modal edit |
| `a` | Add new row |
| `d` | Toggle delete mark |
| `c` | Clone row |
| `u` / `U` | Revert cell / row |
| `Ctrl+U` | Revert all changes |
| `Ctrl+D` | Preview DML |
| `Ctrl+S` | Submit changes |
| `f` | Follow FK reference |
| `Alt+Left` | FK back-navigation |

### Bottom Panels

| Key | Action |
|-----|--------|
| `1` / `2` / `3` / `4` / `5` | Results / Explain / Detail / ER Diagram / Profile |
| `Tab` (Explain) | Toggle Bytecode / Query Plan |
| `Enter` (Explain) | Generate EXPLAIN |
| `Tab` (ER Diagram) | Cycle focus between tables |
| `Enter` (ER Diagram) | Expand / collapse table columns |
| `h/j/k/l` (ER Diagram) | Pan viewport |
| `+` / `-` (ER Diagram) | Adjust spacing |
| `c` (ER Diagram) | Toggle compact mode |
| `o` (ER Diagram) | Query focused table |

### Admin Tab

| Key | Action |
|-----|--------|
| `r` | Refresh |
| `c` | WAL checkpoint |
| `i` | Integrity check |
| `Enter` (Pragmas) | Edit selected pragma |

Press `Esc` in any panel to release focus.

## Configuration

Config file location: `~/.config/tursotui/config.toml`

```toml
[editor]
tab_size = 4
autocomplete = true
autocomplete_min_chars = 1

[results]
max_column_width = 40
null_display = "NULL"

[history]
max_entries = 5000

[theme]
mode = "dark"    # "dark" or "light"

[mouse]
mouse_mode = true  # enable mouse capture (click, scroll, drag)
```

## Architecture

tursotui uses a **unidirectional data flow** architecture inspired by Elm/Redux. Components emit `Action`s, `AppState` processes state changes, and results route back through a two-phase dispatch.

### System Overview

```mermaid
graph TB
    subgraph Terminal
        CT[Crossterm Events<br><i>key, mouse, resize</i>]
    end

    subgraph EventLoop["Event Loop (main.rs)"]
        DRAIN[Drain async channel]
        POLL[Poll crossterm — 16ms]
        ROUTE[Route to focused component]
        GLOBAL[Global key fallback]
        RENDER[Render frame]
    end

    subgraph State["Application State (app.rs)"]
        AS[AppState]
        AC[Action enum<br><i>50+ variants</i>]
        SC[SchemaCache]
        DC[DatabaseContext<br><i>per-tab state</i>]
    end

    subgraph Components["Components (Component trait)"]
        direction LR
        SE[Schema<br>Explorer]
        QE[Query<br>Editor]
        RT[Results<br>Table]
        DE[Data<br>Editor]
        EV[Explain<br>View]
        RD[Record<br>Detail]
        ER[ER<br>Diagram]
        AI[Admin<br>Info]
        PD[Pragma<br>Dashboard]
    end

    subgraph Overlays
        direction LR
        HLP[Help]
        HST[History]
        BK[Bookmarks]
        EXP[Export]
        DML[DML Preview]
        FP[File Picker]
        GTO[Go to Object]
        DDL[DDL Viewer]
    end

    subgraph Async["Async I/O (tokio)"]
        DH[DatabaseHandle<br><i>Arc&lt;Database&gt;</i>]
        SPAWN["tokio::spawn<br><i>fresh connection per query</i>"]
        MPSC[mpsc channel]
    end

    subgraph Persistence
        CFG[Config<br><i>TOML</i>]
        HDB[History DB<br><i>SQLite</i>]
        BUF[Editor Buffer<br><i>auto-save</i>]
    end

    CT --> POLL
    DRAIN --> ROUTE
    POLL --> ROUTE
    ROUTE --> Components
    ROUTE --> Overlays
    Components -->|emit Action| AC
    Overlays -->|emit Action| AC
    AC -->|"phase 1: state"| AS
    AC -->|"phase 2: dispatch"| Components
    AC -->|"phase 2: I/O"| DH
    DH --> SPAWN
    SPAWN --> MPSC
    MPSC --> DRAIN
    AS --- SC
    AS --- DC
    RENDER --> Components
    RENDER --> Overlays
    CFG -.-> AS
    HDB -.-> HST
    BUF -.-> QE

    ROUTE -.->|unhandled| GLOBAL
```

### Event Loop & Data Flow

```mermaid
sequenceDiagram
    participant T as Terminal
    participant EL as Event Loop
    participant C as Component
    participant AS as AppState
    participant DB as DatabaseHandle
    participant TK as tokio task

    Note over EL: Each frame (~60fps)

    EL->>EL: Drain async results (try_recv)
    EL->>T: Poll crossterm (16ms timeout)
    T-->>EL: KeyEvent

    EL->>C: handle_key(event)
    C-->>EL: Action

    rect rgb(40, 40, 60)
        Note over EL,AS: Two-phase dispatch
        EL->>AS: update(action) — state mutations
        EL->>C: dispatch — route to components & I/O
    end

    alt Database action (e.g. ExecuteQuery)
        EL->>DB: execute_query(sql)
        DB->>TK: tokio::spawn with fresh connection
        TK-->>EL: QueryResult via mpsc
        EL->>AS: update(QueryCompleted)
        EL->>C: dispatch(QueryCompleted)
    end

    EL->>C: render(frame, area)
```

### UI Layout

```text
┌──────────────────────────────────────────────────────────────────┐
│  Tab Bar: [db1.sqlite] [db2.sqlite] [:memory:]                  │
├────────────────┬─────────────────────────────────────────────────┤
│                │                                                 │
│  Schema        │  Query Editor                                   │
│  Explorer      │  ┌─────────────────────────────────────────┐   │
│                │  │ SELECT * FROM users                      │   │
│  ▸ Tables (5)  │  │ WHERE active = 1;                        │   │
│    users       │  └─────────────────────────────────────────┘   │
│    orders      ├─────────────────────────────────────────────────┤
│    products    │                                                 │
│  ▸ Views (2)   │  Results Table / Explain / Detail / ER Diagram  │
│  ▸ Indexes (3) │  ┌────────┬──────────┬─────────┬────────────┐  │
│                │  │ id     │ name     │ email   │ active     │  │
│                │  ├────────┼──────────┼─────────┼────────────┤  │
│                │  │ 1      │ Alice    │ a@b.com │ 1          │  │
│                │  │ 2      │ Bob      │ b@c.com │ 1          │  │
│                │  └────────┴──────────┴─────────┴────────────┘  │
├────────────────┴─────────────────────────────────────────────────┤
│  Status: users │ 2 rows │ 4 cols                        F1 Help │
└──────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

- **Unidirectional data flow** — components emit `Action`s, `AppState` processes state changes, results route back to components via two-phase dispatch.
- **Async queries** — `tokio::spawn` with fresh connections per query, results delivered via `mpsc` channel. No shared connection state between tasks.
- **Component trait** — each panel implements `handle_key`, `update`, `render` with consistent `panel_block` / `overlay_block` helpers for styled borders.
- **Catppuccin theme system** — full Mocha (dark) and Latte (light) palettes with semantic color roles for schema types, editor highlighting, and data editing states.
- **Transactional data editing** — change log with one-entry-per-PK invariant, DML generation, and FK-aware statement ordering for safe transactional submission.
- **No unsafe code** — `#[forbid(unsafe_code)]` enforced project-wide.

## Tech Stack

| Crate | Purpose |
| ------- | --------- |
| [turso](https://crates.io/crates/turso) | Database engine (libSQL/SQLite) |
| [ratatui](https://crates.io/crates/ratatui) | Terminal UI framework |
| [tokio](https://crates.io/crates/tokio) | Async runtime |
| [clap](https://crates.io/crates/clap) | CLI argument parsing |
| [arboard](https://crates.io/crates/arboard) | Clipboard access |
| [unicode-width](https://crates.io/crates/unicode-width) | Display-column width measurement |
| [serde](https://crates.io/crates/serde) / [toml](https://crates.io/crates/toml) | Configuration serialization |
| [serde_json](https://crates.io/crates/serde_json) | JSON detection and pretty-printing |
| [dirs](https://crates.io/crates/dirs) | Platform config/data directories |

## License

MIT

## Acknowledgments

- Built with [Rust](https://www.rust-lang.org/)
- TUI powered by [ratatui](https://ratatui.rs)
- Cross-platform terminal handling by [crossterm](https://github.com/crossterm-rs/crossterm)
- Theme palette by [Catppuccin](https://github.com/catppuccin/catppuccin)

## Contact

**Author:** Mikko Leppänen
**Email:** <mleppan23@gmail.com>
**GitHub:** [@mikeleppane](https://github.com/mikeleppane)
