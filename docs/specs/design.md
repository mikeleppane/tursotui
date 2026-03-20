# tursotui — Terminal UI for Turso/SQLite

> Design spec for a ratatui-based terminal user interface for browsing, querying,
> and administering Turso and SQLite databases.
>
> Date: 2026-03-20
> Status: Draft

---

## 1. Overview

### What

`tursotui` is a standalone Rust TUI application for interacting with Turso and SQLite databases. It provides a polished, keyboard-driven interface for schema browsing, SQL querying, result inspection, and database administration — all in the terminal.

### Why

- The current `tursodb` CLI is a line-oriented REPL with no visual affordances for browsing tables, scrolling results, or inspecting schemas.
- Existing terminal database tools (lazysql, gobang, rainfrog) target PostgreSQL/MySQL. None integrate with Turso's engine or features.
- Since Turso uses the SQLite file format, tursotui automatically works as a SQLite TUI too — dramatically increasing the audience.

### Goals

- **Developer tool**: Browse schemas, write queries, inspect results efficiently.
- **Admin tool**: Monitor database health, WAL status, pragma configuration.
- **Lazygit-level polish**: Styled borders, color themes, contextual help, smooth focus transitions.
- **Keyboard-first**: Vim-inspired navigation, minimal mouse dependency.

### Non-Goals (v1)

- Cloud/remote Turso connections (embedded replicas, sync).
- Plugin/extension system.
- Cross-database queries (SQLite `ATTACH` — v2).

---

## 2. Technical Foundation

### Dependencies

| Crate | Purpose |
|-------|---------|
| `turso` (0.6.0-pre.4) | Database connection, queries, pragmas, EXPLAIN |
| `ratatui` (0.29.x or 0.30.x when released) | Terminal UI framework |
| `crossterm` (explicit dep, same version as ratatui) | Terminal event handling backend |
| `tokio` (full features) | Async runtime for `turso` crate |
| `unicode-width` | Correct column width calculation |
| `dirs` | Config/history file paths |
| `serde` + `toml` | Config file serialization (history uses SQLite via `turso`) |
| `clap` (derive) | CLI argument parsing |
| `arboard` (optional, best-effort) | Clipboard access (degrades gracefully in SSH/headless) |

**Note on syntax highlighting:** Use a custom token-based highlighter with a curated SQL keyword list (referenced from `turso_parser::token`). This avoids the heavy `syntect` dependency (~500KB binary impact) and is sufficient for lexical SQL coloring.

### Why `turso` crate (not `turso_core`)

The `turso` Rust binding provides a clean async API (`Builder`, `Connection`, `Statement`, `Rows`) that handles I/O backend selection automatically. It covers everything needed for v1:

- Schema introspection via `SELECT * FROM sqlite_schema`
- Query execution with prepared statements
- EXPLAIN / EXPLAIN QUERY PLAN
- All PRAGMAs (WAL status, cache stats, page info)
- Foreign key metadata via `PRAGMA foreign_key_list(table)`

If deeper internals are needed later (direct pager/WAL access), `turso_core` can be added as an optional dependency.

### Project Location

`tursotui` is a **standalone repository** (not inside the Turso workspace). It depends on `turso` as a crates.io dependency. This gives us independent release cycle and avoids coupling to Turso's CI/review process. If the project gains traction, it can be proposed for upstream inclusion later.

### Async Runtime Strategy

`turso::Connection` is `Send + Sync` — confirmed by `assert_send_sync!(Connection)` at `connection.rs:57` in the turso codebase. Therefore:

- Use `#[tokio::main]` — standard multi-threaded tokio runtime.
- The main thread runs the ratatui render loop (synchronous poll + render).
- Query execution uses `tokio::spawn()` with a **fresh connection** from `database.connect()`, running on the tokio thread pool.
- An `mpsc` channel sends `QueryMessage` from the spawned task back to the render loop.
- The render loop calls `result_rx.try_recv()` each frame (non-blocking) to pick up completed results.

**Initialization:** `Builder::new_local(path).build().await` returns `Database` (async). Then `database.connect()` returns `Connection` (sync — not async). The `Database` object is the connection factory; each call to `.connect()` creates an independent connection to the same file.

**Why not `conn.clone()`:** `Connection::clone()` clones an inner `Arc`, meaning cloned connections **share the same underlying connection state**. Concurrent queries on cloned connections would interleave on a single SQLite connection, which is not safe. Instead, `DatabaseHandle` stores `Arc<Database>` and calls `database.connect()` to create a fresh independent connection for each spawned query task.

**Note on tokio as a dependency:** `tokio` is NOT a default dependency of the `turso` crate — it is only pulled in with the `sync` feature flag. `tursotui` adds `tokio` as its own direct dependency (`tokio = { version = "1", features = ["full"] }`) in its `Cargo.toml`. This is the application-level runtime, not inherited from `turso`.

### Project Structure

```
tursotui/
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point, terminal setup, event loop
│   ├── app.rs               # AppState, Action enum, tab management
│   ├── event.rs             # Event loop (crossterm → Action mapping)
│   ├── db.rs                # Database handle, async query execution
│   ├── theme.rs             # Theme definitions (dark/light)
│   ├── config.rs            # Config file loading/saving
│   ├── components/
│   │   ├── mod.rs           # Component trait definition
│   │   ├── schema.rs        # Schema explorer (tree view)
│   │   ├── editor.rs        # Query editor (syntax highlighted)
│   │   ├── results.rs       # Results table (scrollable, sortable, resizable)
│   │   ├── record.rs        # Record detail (single-row view)
│   │   ├── explain.rs       # EXPLAIN view (bytecode + query plan)
│   │   ├── er_diagram.rs    # ER diagram (box-drawing relationships)
│   │   ├── db_info.rs       # Database info panel
│   │   ├── pragmas.rs       # PRAGMA dashboard (editable)
│   │   ├── history.rs       # Query history popup
│   │   ├── help.rs          # Help overlay
│   │   └── status_bar.rs    # Context-sensitive status bar
│   └── highlight.rs         # SQL syntax highlighting
└── README.md
```

---

## 3. Architecture

### Component Model

Each panel implements a `Component` trait following ratatui 0.30 patterns:

```rust
pub trait Component {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action>;
    fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<Action> { None }
    fn update(&mut self, action: &Action);
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme);
}
```

Components are self-contained: they own their state, handle their own key events when focused, and render themselves into a given `Rect`.

### Application State

The app supports multiple simultaneously open databases. Each database gets its own
workspace — schema cache, editor buffer, results, and admin state are fully independent.
Global state (theme, config, query history) is shared.

```rust
pub struct AppState {
    // Multi-database
    databases: Vec<DatabaseContext>,   // One per open database
    active_db: usize,                 // Index into `databases`

    // Global state
    query_history: Vec<HistoryEntry>, // Shared across databases, tagged by path
    transient_message: Option<(String, Instant)>,  // Status bar message, auto-clears after 3s

    // Config
    theme: Theme,
    config: AppConfig,
}

/// Per-database workspace. Each open database owns its full UI state independently.
pub struct DatabaseContext {
    // Identity
    handle: DatabaseHandle,
    path: String,                       // e.g., "/home/user/myapp.db" or ":memory:"
    label: String,                      // Display name: filename without path ("myapp.db")

    // Navigation (per-database)
    sub_tab: SubTab,                    // Query | Admin
    focus: PanelId,                     // Which panel has keyboard focus
    sidebar_visible: bool,

    // Query workspace
    last_result: Option<QueryResult>,
    last_error: Option<String>,
    execution_time: Option<Duration>,
    bottom_tab: BottomTab,

    // Component state (each database has its own editor, schema tree, etc.)
    schema: SchemaExplorer,
    editor: QueryEditor,
    results: ResultsTable,              // ResultsTable optionally holds a DataEditor when editable
    explain: ExplainView,
    er_diagram: ERDiagram,
    db_info: DbInfoPanel,
    pragmas: PragmaDashboard,
}

pub enum SubTab { Query, Admin }

pub enum BottomTab { Results, Explain, Detail, ERDiagram }

/// PanelId identifies focusable panels. The bottom panel sub-tabs share a single
/// PanelId::Bottom — which sub-tab is active is determined by `bottom_tab`.
/// SwitchBottomTab changes which sub-tab renders; FocusPanel(Bottom) gives it focus.
pub enum PanelId {
    Schema, Editor, Bottom,  // Query sub-tab panels
    DbInfo, Pragmas,         // Admin sub-tab panels
}
```

**Database tab bar:** The top of the screen shows a two-tier tab system:

```
[myapp.db] [analytics.db] [+]         ← database tabs (Ctrl+PgUp/PgDn to switch)
  [Query] [Admin]                      ← sub-tabs within active database (Alt+1/2 to switch)
```

The `[+]` button opens a file picker dialog to add another database. `Ctrl+W` closes the
current database tab (with confirmation if there's an unsaved editor buffer). At least one
database must remain open — closing the last one prompts to open another or quit.

### Action Enum

All state mutations flow through actions:

```rust
pub enum Action {
    // Database management
    SwitchDatabase(usize),              // Switch to database tab by index
    NextDatabase,                       // Ctrl+PgDn
    PrevDatabase,                       // Ctrl+PgUp
    OpenDatabase(PathBuf),              // Open new database (from file picker or CLI)
    CloseDatabase(usize),              // Close database tab by index

    // Navigation (within active database)
    SwitchSubTab(SubTab),
    FocusPanel(PanelId),
    CycleFocus(Direction),              // Esc / Ctrl+Tab
    ToggleSidebar,
    SwitchBottomTab(BottomTab),

    // Query
    ExecuteQuery(String),
    QueryCompleted(QueryResult),       // Also invalidates cached EXPLAIN data — Explain tab resets to "Press Enter to generate"
    QueryFailed(String),
    GenerateExplain,                              // User switched to Explain tab — fire lazy EXPLAIN
    ExplainCompleted(Vec<Vec<String>>, Vec<String>),  // (bytecode rows, plan lines)
    ShowHistory,
    RecallHistory(usize),

    // Schema
    ExpandNode(usize),
    CollapseNode(usize),

    // Results
    SortColumn(usize, SortOrder),
    ResizeColumn(usize, ResizeDirection),  // Grow | Shrink by 1, clamped to [min_width, config.max_column_width]
    SelectRow(usize),
    CopyCell,
    CopyRow,

    // Data Editor
    EditCell(usize, usize),             // (row, col) — enter inline edit mode
    ConfirmCellEdit(String),            // Confirm cell edit with new value
    CancelCellEdit,                     // Revert cell edit
    AddRow,                             // Append new row with defaults
    DeleteRow(usize),                   // Toggle delete mark
    CloneRow(usize),                    // Duplicate row as new insert
    RevertCell(usize, usize),           // Revert single cell to original
    RevertRow(usize),                   // Revert all cells in a row
    RevertAllChanges,                   // Revert ALL pending changes
    PreviewDML,                         // Show generated SQL popup
    SubmitChanges,                      // Submit all changes (with DML preview + confirm)
    SubmitConfirmed,                    // User confirmed submit in DML preview
    FollowFK(usize, usize),            // (row, col) — navigate to referenced row
    FKNavigateBack,                     // Go back in FK navigation history

    // Admin
    RefreshPragmas,
    EditPragma(String, String),

    // Global
    OpenFilePicker,                     // Ctrl+O — open database file picker
    OpenGoToObject,                     // Ctrl+P — quick navigation popup
    GoToObject(ObjectRef),              // Navigate to a specific object
    ToggleTheme,
    ShowHelp,
    Quit,
}
```

### Event Loop

```
┌─────────────────────────────────────────────────┐
│                 Main Loop                        │
│                                                  │
│  1. Poll crossterm events (16ms timeout)         │
│  2. Check result channels for ALL open databases │
│  3. Map event → Action (via focused panel)       │
│  4. Dispatch Action → update active DatabaseCtx  │
│  5. Render all visible components                │
│  6. Repeat                                       │
└─────────────────────────────────────────────────┘
```

The 16ms poll timeout gives ~60fps rendering while staying responsive to input. Query execution runs on a separate tokio task per database, communicating results via per-database `mpsc` channels so the UI never blocks. A query running in a background database tab completes independently and the results are ready when the user switches to that tab.

---

## 4. Layout

### Query Sub-Tab

```
┌─[myapp.db]──[analytics.db]──[+]──────────────────┐
│ ─[Query]──[Admin]──────────────────────────────────│
│ ┌─Schema [Ctrl+B]┐ ┌─Query Editor─────────────────┐│
│ │ 📁 tables      │ │ SELECT u.name, COUNT(o.id)   ││
│ │  ▼ users       │ │ FROM users u                  ││
│ │    ├ id    INT │ │ JOIN orders o ON o.uid=u.id   ││
│ │    ├ name  TXT │ │ GROUP BY u.name;              ││
│ │    └ email TXT │ │                               ││
│ │  ▶ orders      │ ├─[Results]─[Explain]─[Detail]─[ER]┤│
│ │  ▶ products    │ │ name    │ count               ││
│ │ 📁 indexes     │ │ Alice   │ 12                  ││
│ │  ▶ idx_email   │ │ Bob     │ 7                   ││
│ │ 📁 views       │ │ Charlie │ 3                   ││
│ │ 📁 triggers    │ │                               ││
│ └────────────────┘ └──────────────────────────────┘│
│ ◀▶ Tab  ↑↓ Navigate  / Search  F5 Execute  ? Help │
└────────────────────────────────────────────────────┘
```

The top row shows database tabs. The active database is highlighted. `[+]` opens a file picker.
The second row shows sub-tabs (Query / Admin) within the active database.

**Layout structure (ratatui):**

```rust
// Top level: database tabs + sub-tabs + content + status bar
let [db_tabs_area, sub_tabs_area, content_area, status_area] = Layout::vertical([
    Constraint::Length(1),    // Database tab bar
    Constraint::Length(1),    // Sub-tab bar (Query / Admin)
    Constraint::Fill(1),      // Main content
    Constraint::Length(1),    // Status bar
]).areas(frame.area());

// Content: sidebar + main (when sidebar visible)
let [sidebar_area, main_area] = Layout::horizontal([
    Constraint::Percentage(20),  // Schema sidebar
    Constraint::Percentage(80),  // Editor + bottom panel
]).areas(content_area);

// Main: editor + bottom panel
let [editor_area, bottom_area] = Layout::vertical([
    Constraint::Percentage(40),  // Query editor
    Constraint::Percentage(60),  // Results/Explain/Detail/ER
]).areas(main_area);
```

When sidebar is collapsed, editor + bottom panel take 100% width.

### Admin Sub-Tab

```
┌─[myapp.db]──[analytics.db]──[+]──────────────────┐
│ ─[Query]──[Admin]──────────────────────────────────│
│ ┌─Database Info───────┐ ┌─PRAGMA Dashboard────────┐│
│ │ File: ./myapp.db    │ │ journal_mode   WAL      ││
│ │ Size: 2.4 MB        │ │ page_size      4096     ││
│ │ Pages: 612           │ │ cache_size     -2000    ││
│ │ Encoding: UTF-8      │ │ auto_vacuum    NONE     ││
│ │ Schema ver: 14       │ │ wal_autockpt   1000     ││
│ │ Turso ver: 0.6.0     │ │ freelist_count 0        ││
│ │                      │ │ busy_timeout   5000     ││
│ │ WAL Status:          │ │ synchronous    NORMAL   ││
│ │  Frames: 47          │ │                         ││
│ │  Checkpointed: 32    │ │ [Enter: Edit Value]     ││
│ │  Size: 196 KB        │ │                         ││
│ └──────────────────────┘ └─────────────────────────┘│
│ ◀▶ Tab  ↑↓ Navigate  r Refresh  Enter Edit  ? Help│
└────────────────────────────────────────────────────┘
```

**Layout structure:**

```rust
let [left, right] = Layout::horizontal([
    Constraint::Percentage(40),  // Database info
    Constraint::Percentage(60),  // PRAGMA dashboard
]).areas(content_area);
```

---

## 5. Component Specifications

### 5.1 Schema Explorer

**Data source:** `SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name`

**Tree structure:**
```
📁 tables (N)
 ▼ users
   ├ id       INTEGER  PK AUTOINCREMENT
   ├ name     TEXT     NOT NULL
   ├ email    TEXT     UNIQUE
   └ indexes: idx_users_email
 ▶ orders
📁 indexes (N)
 ▶ idx_users_email
📁 views (N)
📁 triggers (N)
```

**Behavior:**
- `j/k` or `↑/↓`: Navigate tree
- `→`: Expand node / drill into table columns
- `←` or `Backspace`: Collapse node
- `Enter`: Toggle expand/collapse on category/table nodes. On a **leaf node** (column), no-op.
- `o`: Populates editor with `SELECT * FROM <table> LIMIT 100;` and moves focus to editor (does NOT auto-execute — user presses `F5` to run). Works on table nodes only.
- `/`: Opens an inline search bar at the bottom of the schema panel. Filters the tree in-place (hides non-matching nodes). Matches on leaf names only (table, index, view, trigger names — not category headers). `Esc` clears the filter and restores the full tree. Case-insensitive substring match.
- Column details fetched via `PRAGMA table_info(table_name)`
- Foreign keys fetched via `PRAGMA foreign_key_list(table_name)`
- Index info via `PRAGMA index_list(table_name)` + `PRAGMA index_info(index_name)`

**State:**
```rust
pub struct SchemaExplorer {
    tree: Vec<TreeNode>,
    visible_indices: Vec<usize>,  // Indices into flattened tree of currently visible nodes
    selected: usize,              // Index into visible_indices (not the full tree)
    scroll_offset: usize,
    filter: Option<String>,       // When active, visible_indices only includes matching nodes
}

pub struct TreeNode {
    kind: NodeKind,        // Category | Table | Column | Index | View | Trigger
    name: String,
    detail: String,        // e.g., "INTEGER PK" for columns
    state: NodeState,      // Collapsed | Loading | Expanded
    depth: usize,
    children: Vec<TreeNode>,
}

pub enum NodeState {
    Collapsed,             // Not yet expanded, no children loaded
    Loading,               // Expand requested, async fetch in progress (shows spinner)
    Expanded,              // Children fully loaded and visible
}
```

### 5.2 Query Editor

**Features:**
- Multi-line text editing
- SQL syntax highlighting (keywords, strings, numbers, comments)
- Cursor movement (arrow keys, Home/End, Ctrl+A/E)
- Basic editing (insert, delete, backspace, newline)
- **Undo/redo**: `Ctrl+Z` / `Ctrl+Y` (or `Ctrl+Shift+Z`). v1 uses a simple snapshot-based undo — each edit records the full buffer state before the change. Cap at 100 undo steps. **Tradeoff:** This copies the entire buffer per edit, which is wasteful for large queries (500+ lines). Acceptable for v1 since most SQL queries are short. A future version can switch to operation-based undo (insert/delete ops with positions) for O(1) per edit.
- `F5` or `Ctrl+Enter`: Execute entire buffer
- `Ctrl+H`: Open query history popup
- Line numbers in left gutter

**Syntax highlighting categories:**

| Category | Examples | Style |
|----------|----------|-------|
| Keyword | `SELECT`, `FROM`, `WHERE`, `JOIN` | Bold, accent color |
| String | `'hello'`, `"name"` | Green |
| Number | `42`, `3.14` | Yellow |
| Comment | `-- comment`, `/* block */` | Dimmed |
| Function | `COUNT()`, `SUM()`, `SUBSTR()` | Cyan |
| Operator | `=`, `<>`, `AND`, `OR`, `NOT` | Default, bold |
| Default | Column names, table names | Default fg |

**Implementation:** Simple token-based highlighter matching SQL keywords. No need for a full parser — just lexical coloring. Can use a curated keyword list from `turso_parser::token` as reference.

**State:**
```rust
pub struct QueryEditor {
    buffer: Vec<String>,           // Lines of text
    cursor: (usize, usize),        // (row, col)
    scroll_offset: usize,
    selection: Option<SelectionRange>,   // start (row, col) to end (row, col)
    undo_stack: Vec<Vec<String>>,  // Buffer snapshots before each edit (capped at 100)
    redo_stack: Vec<Vec<String>>,  // Snapshots for redo after undo
}
```

### 5.3 Results Table

**Features:**
- Scrollable rows and columns
- Column headers with type info
- Sortable by clicking/selecting column header (`s` to cycle sort)
- Column resizing (`<` / `>` to shrink/grow focused column)
- Row selection (highlight current row)
- `Enter` on a row → switches to Record Detail view
- `y`: Copy current cell value to clipboard
- `Y`: Copy entire row (tab-separated)
- NULL values rendered as dimmed italic `NULL`
- Row position indicator: `Row 3 of 147`

**State:**
```rust
pub struct ResultsTable {
    columns: Vec<ColumnDef>,       // name, type, width
    rows: Vec<Vec<Value>>,         // Original typed values (used for sorting)
    display_rows: Vec<Vec<String>>,// String-rendered for display (rebuilt on sort)
    selected_row: usize,
    selected_col: usize,
    scroll: (usize, usize),        // (row_offset, col_offset)
    sort: Option<(usize, SortOrder)>,
    column_widths: Vec<u16>,
    /// Data editor is owned by ResultsTable, not a peer. None = read-only results.
    /// When Some, ResultsTable delegates edit keys to DataEditor and renders its
    /// toolbar/markers. ResultsTable owns the rows (source of truth); DataEditor
    /// tracks changes as a diff layer on top.
    editor: Option<DataEditor>,
}

// Value preserves types for correct sort ordering (numeric 2 < 9 < 10, not string "10" < "2" < "9")


pub struct ColumnDef {
    name: String,
    type_name: String,
    width: u16,                    // Current display width
    min_width: u16,                // Minimum (header length)
}
```

**Column width algorithm:**

1. Initial width = max(header_length, longest_value_in_first_50_rows, 4) — this is a heuristic; later rows may exceed the auto-width, which is why manual resize (`<`/`>`) exists
2. Cap at `config.max_column_width` (default: 40, configurable in `config.toml`)
3. User can resize with `<` / `>` keys (clamped to `[min_width, config.max_column_width]`)
4. Columns that don't fit scroll horizontally

### 5.4 Record Detail

**Single-row view when you want to inspect one record:**

```
┌─Record Detail (Row 3)──────────────────────┐
│ id          │ 42                            │
│ name        │ Alice Wonderland              │
│ email       │ alice@example.com             │
│ bio         │ Lorem ipsum dolor sit amet,   │
│             │ consectetur adipiscing elit... │
│ created_at  │ 2025-01-15 14:30:00           │
│ avatar      │ [BLOB 4.2 KB]                 │
│ deleted     │ NULL                          │
└─────────────────────────────────────────────┘
```

- Key-value layout, one field per row
- Long values wrap
- BLOBs show size, not content
- NULLs styled distinctly
- `j/k` to scroll through fields
- Triggered by `Enter` on a result row (from Results sub-tab)

### 5.5 EXPLAIN View

**Two modes toggled with `Tab` within the panel:**

**Bytecode mode (EXPLAIN):**
```
┌─EXPLAIN──────────────────────────────────────┐
│ addr │ opcode      │ p1 │ p2 │ p3 │ comment  │
│    0 │ Init        │  0 │ 12 │  0 │          │
│    1 │ OpenRead    │  0 │  2 │  0 │ users    │
│    2 │ Rewind      │  0 │ 10 │  0 │          │
│    3 │ Column      │  0 │  1 │  1 │ name     │
│    4 │ ResultRow   │  1 │  1 │  0 │          │
│    5 │ Next        │  0 │  3 │  0 │          │
│    6 │ Halt        │  0 │  0 │  0 │          │
└──────────────────────────────────────────────┘
```

**Query plan mode (EXPLAIN QUERY PLAN):**
```
┌─QUERY PLAN────────────────────────────────────┐
│ SCAN users                                     │
│ └─ USE INDEX idx_users_email (email=?)         │
└────────────────────────────────────────────────┘
```

**Data source:**
- `EXPLAIN <query>` for bytecode
- `EXPLAIN QUERY PLAN <query>` for plan

**Lazy generation:** EXPLAIN output is NOT generated automatically on query execution. It is generated on-demand when the user switches to the Explain sub-tab. This avoids two extra round-trips to the database on every query execution (most users never look at EXPLAIN). The tab shows "Press Enter to generate EXPLAIN" when stale, then fires both queries sequentially on the same connection once activated.

### 5.6 ER Diagram

**Visual entity-relationship diagram using Unicode box-drawing:**

```
┌──────────┐         ┌──────────┐
│ users    │         │ orders   │
├──────────┤    1──* ├──────────┤
│ⓟ id  INT├─────────┤ⓕ uid INT │
│  name TXT│         │ⓟ id  INT │
│  email   │         │  total   │
└──────────┘         └────┬─────┘
                          │ 1──*
                     ┌────┴─────┐
                     │ items    │
                     ├──────────┤
                     │ⓕ oid INT │
                     │ⓟ id  INT │
                     │  qty INT │
                     └──────────┘
```

**Data sources:**
- Tables: `SELECT name FROM sqlite_schema WHERE type='table'`
- Columns: `PRAGMA table_info(table)`
- Foreign keys: `PRAGMA foreign_key_list(table)`

**FK loading strategy:** The ER diagram requires FK data for ALL tables at once, but the schema explorer loads FKs lazily per-table. On first switch to the ER diagram tab, a **batch FK load** runs: iterate all tables and call `PRAGMA foreign_key_list(table)` for each. This is a synchronous burst of small queries (fast — typically <100ms even for 50+ tables). Results are cached in the `ERDiagram` component and reused. The schema explorer's per-table FK cache is also populated as a side effect. A loading spinner is shown while the batch runs.

**Layout algorithm (v1 — grid-based, not full Sugiyama):**

v1 uses a simple grid layout to avoid the implementation complexity of the full Sugiyama layered graph algorithm (topological sort + edge crossing minimization), which can easily consume weeks of dev time. The full algorithm is deferred to v2.

1. Build a directed graph from foreign key relationships
2. Detect cycles (self-referencing tables, mutual FKs) and break them by removing one edge per cycle
3. Assign tables to a grid: tables with FK relationships placed adjacent when possible; orphan tables (no FKs) placed in a final row/column
4. Grid dimensions: `ceil(sqrt(N))` columns, tables sorted alphabetically within each row for deterministic layout
5. Render with box-drawing characters on a virtual canvas
6. FK edges drawn as lines between grid cells (straight or L-shaped, not routed around obstacles)
7. Broken cycle edges rendered as dashed lines (`╌╌╌`) with a `[cycle]` label on the edge

**Scale handling (50+ tables):**

- Only tables that fit within the current viewport are rendered — the canvas uses a virtual coordinate system and only draws cells in `[viewport_x .. viewport_x + width, viewport_y .. viewport_y + height]`
- At default zoom, each table box shows only table name + PK column. Other columns are collapsed behind a `(+N)` indicator
- Pressing `Enter` on a table in the diagram expands it to show all columns in-place
- With `c` you can toggle "compact mode" globally (names only, no columns) for very large schemas
- Edges to/from off-screen tables are drawn as stub arrows at the viewport edge with a label: `→ orders (off screen)`

**State:**
```rust
pub struct ERDiagram {
    tables: Vec<ERTable>,
    relationships: Vec<Relationship>,
    viewport: (i32, i32),          // Pan offset
    zoom: u8,                       // Spacing multiplier
    focused_table: Option<usize>,
    canvas: Vec<Vec<char>>,         // Pre-rendered character grid
}

pub struct Relationship {
    from_table: usize,
    from_column: String,
    to_table: usize,
    to_column: String,
    cardinality: Cardinality,       // OneToOne | OneToMany
}
```

### 5.7 Database Info

**Static panel showing database metadata:**

| Field | Source |
|-------|--------|
| File path | Connection URI |
| File size | `std::fs::metadata` |
| Page count | `PRAGMA page_count` |
| Page size | `PRAGMA page_size` |
| Encoding | `PRAGMA encoding` |
| Journal mode | `PRAGMA journal_mode` |
| Schema version | `PRAGMA schema_version` |
| Turso version | `turso` crate version |
| Freelist pages | `PRAGMA freelist_count` |
| Data version | `PRAGMA data_version` (change-detection counter) |

**WAL status** is displayed only when journal_mode is WAL. Frame count is estimated from WAL file size (`(wal_file_size - 32) / (page_size + 24)`) — the 32-byte subtraction accounts for the WAL file header. We do NOT use `PRAGMA wal_checkpoint()` for display — that is a destructive operation that actually checkpoints. A manual "Checkpoint" action (`c` key) can trigger `PRAGMA wal_checkpoint(PASSIVE)` with explicit user intent.

Refreshes on manual `r` keypress. Does NOT auto-refresh on tab switch to avoid unnecessary I/O.

### 5.8 PRAGMA Dashboard

**Scrollable list of PRAGMAs with current values:**

```
PRAGMA              │ Value
────────────────────┼──────────
journal_mode        │ wal
page_size           │ 4096
cache_size          │ -2000
auto_vacuum         │ 0
wal_autocheckpoint  │ 1000
synchronous         │ 1
busy_timeout        │ 5000
foreign_keys        │ 1
temp_store          │ 0
mmap_size           │ 0
```

- `Enter` on an editable pragma opens inline edit (type new value, Enter to confirm, Esc to cancel)
- Editable pragmas: `cache_size`, `busy_timeout`, `synchronous`, `foreign_keys`, `temp_store`, `mmap_size`, `wal_autocheckpoint`
- Read-only pragmas shown but not editable (dimmed, with a parenthetical note for non-obvious ones: e.g., `page_size` shows "(set at creation time)", `journal_mode` shows "(run in query editor)")
- Changes applied immediately via `PRAGMA name = value`

### 5.9 Data Editor

The data editor enables direct CRUD operations on table data without writing SQL. Edits are **staged locally** with visual indicators, then submitted as a batch — the user always sees the exact DML before it runs.

**When is editing available?**

The data editor activates automatically when the results come from a **single-table query** that the system can map back to a specific table. This includes:

- `SELECT * FROM table_name` (with or without WHERE/ORDER BY/LIMIT)
- Opening a table from the schema explorer (`o` key)
- Any query where all columns resolve to one base table and the table has a `rowid` or explicit primary key

Queries involving JOINs, aggregates, CTEs, subqueries, or views are **read-only** — the results table works normally but the edit toolbar is hidden and edit keys are disabled.

**Detection — two tiers:**

1. **Guaranteed editable:** Queries generated by tursotui itself (schema explorer `o` key, FK navigation `f` key). These are known-safe `SELECT * FROM <table>` queries with a known source table. The source table name is attached to the `QueryResult` at generation time.

2. **User queries — keyword heuristic detection:** For arbitrary user SQL, use a conservative keyword-based heuristic (same approach as the DDL detector in §8). After stripping comments, check that the query starts with `SELECT` and does NOT contain `JOIN`, `UNION`, `INTERSECT`, `EXCEPT`, `GROUP BY`, `WITH`, or multiple `FROM` clauses. If any of these are present, results are read-only. This misses some edge cases (e.g., quoted identifiers containing keywords) but errs on the side of caution — false negatives (marking an editable query as read-only) are safe, false positives (allowing edit on a complex query) are not. **Note:** `turso_parser` is a workspace crate inside the Turso monorepo and is not published to crates.io separately, so it cannot be imported by the standalone tursotui repo. If `turso_parser` is published as a separate crate in the future, the heuristic can be replaced with proper AST analysis.

Then verify the table has a primary key via `PRAGMA table_info(table)`. If there's no PK and no `rowid` (i.e., a `WITHOUT ROWID` table with no PK — rare), editing is disabled with a status bar message: "Table has no primary key — read-only".

**Visual layout (when editing is active):**

```text
┌─Results [editable: users]──────────────────────┐
│ [+Add] [-Del] [Submit (3)] [Revert] [DML]     │
├────────────────────────────────────────────────┤
│   id  │ name          │ email                  │
│    1  │ Alice         │ alice@example.com      │
│    2  │ Bob [edited]  │ bob@new.com [edited]   │
│    3  │ Charlie       │ charlie@example.com    │
│  * 4  │ NEW_ROW       │                        │
│  x 5  │ Dave          │ dave@example.com       │
├────────────────────────────────────────────────┤
│ 3 pending changes: 1 update, 1 insert, 1 del  │
└────────────────────────────────────────────────┘
```

**Row markers (left gutter):**

- **(none)**: Unchanged row (default style)
- **~**: Modified row — has edited cells (yellow)
- **+**: Newly inserted row (green)
- **x**: Marked for deletion (red, strikethrough text)

**Modified cells** are rendered with a colored background (yellow for edits, green for new row cells). The original value is preserved in the change log so the user can revert individual cells.

**Editing workflow:**

1. **Edit a cell:** Navigate to the cell, press `e` or `F2` to enter edit mode. For short values (single-line, <80 chars), an inline text input replaces the cell content. For long or multi-line values (TEXT fields containing newlines, JSON, etc.), a **modal editor popup** opens — a bordered box at 60% terminal size with multi-line editing, similar to the query editor but smaller. `Enter` confirms in inline mode; `Ctrl+Enter` confirms in modal mode. `Esc` reverts in both. The cell is marked as modified (yellow background).

2. **Add a row:** Press `a` or use the `[+Add]` action. A new row is appended at the bottom with default/NULL values for all columns. The row marker shows `+` (green). The user navigates to each cell and edits values.

3. **Delete a row:** Press `d` on the current row. The row is marked for deletion (`x`, red strikethrough) but remains visible. Press `d` again to unmark. Deleted rows are not removed from view until submitted.

4. **Clone a row:** Press `c` to duplicate the current row as a new insert (with a fresh PK — the PK cell is left blank for autoincrement tables, or the user must provide one).

5. **Revert changes:**
   - `u` on a modified cell → reverts that cell to original value
   - `U` on a modified row → reverts all cells in that row
   - `Ctrl+U` → reverts ALL pending changes

6. **Preview DML:** Press `Ctrl+D` to open a popup showing the exact SQL statements that will be executed (INSERT, UPDATE, DELETE). Read-only preview — the user can review before submitting.

7. **Submit changes:** Press `Ctrl+S`. The app:
   - Shows the DML preview popup first (same as `Ctrl+D`)
   - The user confirms with `Enter` or cancels with `Esc`
   - On confirm, executes all statements in a single transaction (`BEGIN; ... COMMIT;`)
   - On success: clears all change markers, refreshes the data
   - On failure: rolls back, shows the error, all changes remain staged

**DML generation:**

```sql
-- For each modified row (keyed by primary key):
UPDATE users SET name = 'Bob2', email = 'bob@new.com' WHERE id = 2;

-- For each inserted row:
INSERT INTO users (name, email) VALUES ('NEW_ROW', NULL);

-- For each deleted row:
DELETE FROM users WHERE id = 5;
```

All statements are wrapped in a transaction. The PK is used in WHERE clauses for UPDATE/DELETE — this is why a PK is required for editability.

**FK navigation:**

When the cursor is on a cell in a foreign key column:

- Press `f` to **follow the FK**: jump to the referenced row in the parent table. This opens the parent table in the results panel (as a new query: `SELECT * FROM parent_table WHERE pk = <value>`) and highlights the referenced row.
- The status bar shows `FK: users.id → orders.user_id` when hovering an FK column.
- Press `Backspace` or `Alt+←` to go **back** to the previous table (navigation history stack, up to 10 entries).

**Text search in data:**

Press `/` while the results table is focused (same as existing search). When the data editor is active, search highlights matching cells across all visible columns. `n` / `N` cycles through matches.

**State:**

```rust
pub struct DataEditor {
    /// Source table name (None = read-only results, not editable)
    source_table: Option<String>,
    /// Primary key column index(es) for this table
    pk_columns: Vec<usize>,
    /// Change log: tracks all pending modifications
    changes: ChangeLog,
    /// Inline cell editor (active when editing a cell)
    cell_editor: Option<CellEditor>,
    /// FK navigation history (back-stack)
    fk_nav_stack: Vec<FKNavEntry>,
}

pub struct ChangeLog {
    edits: Vec<RowEdit>,
}

pub enum RowEdit {
    /// Modified cells in an existing row. Key = PK value(s).
    Update {
        pk: Vec<Value>,
        original: Vec<Value>,        // Full original row
        modified: HashMap<usize, Value>,  // col_index → new value
    },
    /// Newly inserted row (all values provided by user).
    Insert {
        values: Vec<Value>,
    },
    /// Row marked for deletion. Key = PK value(s).
    Delete {
        pk: Vec<Value>,
        original: Vec<Value>,        // For display (strikethrough)
    },
}

pub struct CellEditor {
    row: usize,
    col: usize,
    buffer: String,
    cursor_pos: usize,
}

pub struct FKNavEntry {
    table: String,
    query: String,
    selected_row: usize,
}
```

**Keybindings (when data editor is active, results panel focused):**

- `e` / `F2`: Edit current cell (inline)
- `Enter`: Confirm cell edit / open Record Detail
- `Esc`: Cancel cell edit
- `a`: Add new row
- `d`: Toggle delete mark on current row
- `c`: Clone current row as new insert
- `u`: Revert current cell to original
- `U`: Revert current row (all cells)
- `Ctrl+U`: Revert ALL pending changes
- `Ctrl+D`: Preview DML (show generated SQL)
- `Ctrl+S`: Submit all changes (with DML preview)
- `f`: Follow FK (navigate to referenced row)
- `Alt+←`: Go back in FK navigation history
- `/`: Search in data

These keys are **only active when the results come from an editable query**. For read-only results, `e`, `a`, `d`, `c`, `u`, `U`, `Ctrl+S`, `Ctrl+D`, and `f` are no-ops (or show a status bar message: "Read-only results — edit not available").

---

## 6. Keybindings

### Focus-Aware Key Routing

Single-character keys (`q`, `j`, `k`, `g`, `G`, `o`, `/`, `s`, `y`, `Y`, `r`) are **contextual**: they are consumed by the focused panel, not treated as global shortcuts. This prevents conflicts when the query editor is focused and the user types normally.

**Rule:** Global shortcuts use only modifier+key combinations (`Ctrl+*`, `F*`, numbered keys) except where they only apply when the editor is NOT focused. Each keybinding table below indicates its applicability.

The editor intercepts ALL bare keypresses while focused. `Esc` releases editor focus (moves to next panel). `Tab` inserts indentation (tab_size spaces) in the editor — it does NOT cycle focus when the editor is active. To cycle focus out of the editor, use `Esc` or `Ctrl+Tab`. This matches user expectations from code editors (Helix, VS Code). `Shift+Tab` dedents the current line when the editor is focused.

### Global (always active, regardless of focus)

| Key | Action |
|-----|--------|
| `Ctrl+PgDn` / `Ctrl+PgUp` | Switch to next / previous database tab (fallback: `Alt+]` / `Alt+[`) |
| `Alt+1` / `Alt+2` | Switch to Query / Admin sub-tab (within active database) |
| `Tab` / `Shift+Tab` | Cycle focus between panels (except when editor is focused — see below) |
| `Ctrl+Tab` | Cycle focus between panels (always works, even from editor) |
| `Ctrl+O` | Open file picker to add a new database |
| `Ctrl+W` | Close current database tab (with confirmation) |
| `Ctrl+P` | Go to Object (quick navigation across all databases) |
| `Ctrl+B` | Toggle schema sidebar |
| `Ctrl+T` | Toggle dark/light theme |
| `F1` | Help overlay (`?` also works when editor is NOT focused) |
| `Ctrl+Q` | Quit (safe: no conflict with editor typing) |

**Editor-specific overrides:** When the query editor is focused, `Tab` inserts indentation and `Shift+Tab` dedents. Use `Esc` or `Ctrl+Tab` to leave the editor.

**Note on quitting:** `Ctrl+Q` is the primary and only quit shortcut. `Ctrl+C` is **not** a quit path — it is too easily pressed accidentally while editing SQL. `Ctrl+C` behavior is context-sensitive:

- **During an in-flight query:** detaches the query (see §10)
- **While editor is focused:** no-op (future: copy selection if selection exists)
- **While any other panel is focused:** no-op

**Note on `Ctrl+W`:** Earlier reviews identified `Ctrl+W` as dangerous in some terminals (can close the terminal window). For closing database tabs, `Ctrl+W` is the de facto standard in tabbed apps (browsers, VS Code, etc.). If a terminal intercepts it, the user should remap in their terminal config. This is documented in the help overlay.

### Query Tab — Editor Focused

| Key | Action |
|-----|--------|
| `F5` / `Ctrl+Enter` | Execute query |
| `Ctrl+H` | Query history popup |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |
| Arrow keys | Move cursor |
| `Ctrl+←` / `Ctrl+→` | Word-jump left / right |
| `Home` / `End` / `Ctrl+A` / `Ctrl+E` | Start / end of line |
| Normal typing | Insert characters |
| `Tab` | Insert indentation (tab_size spaces) |
| `Shift+Tab` | Dedent current line |
| `Backspace` / `Delete` | Delete characters |
| `Enter` | New line |
| `Ctrl+L` | Clear editor buffer (and delete auto-save file) |
| `Esc` | Release editor focus (move to next panel) |

### Query Tab — Results Focused

| Key | Action |
|-----|--------|
| `j/k` or `↑/↓` | Navigate rows |
| `h/l` or `←/→` | Navigate columns |
| `s` | Cycle sort on current column |
| `<` / `>` | Shrink / grow column width |
| `Enter` | Open Record Detail for current row |
| `y` | Copy cell to clipboard |
| `Y` | Copy row to clipboard |
| `/` | Search in results |
| `g` / `G` | Jump to first / last row |

### Query Tab — Schema Focused

| Key | Action |
|-----|--------|
| `j/k` or `↑/↓` | Navigate tree |
| `→` | Expand node |
| `Enter` | Toggle expand/collapse |
| `←` or `Backspace` | Collapse node |
| `o` | Populate editor with SELECT * FROM ... LIMIT 100 (focus moves to editor, does not auto-execute) |
| `/` | Filter by name |

### Query Tab — Bottom Panel Sub-tabs

| Key | Action |
|-----|--------|
| `1` | Switch to Results |
| `2` | Switch to Explain |
| `3` | Switch to Record Detail |
| `4` | Switch to ER Diagram |

These bare number keys work because the bottom panel doesn't accept text input — numbers are never typed here. `Ctrl+<number>` is intentionally NOT used as the primary binding due to unreliable terminal support (macOS Terminal.app, tmux, screen all strip or intercept `Ctrl+<number>`).

### Admin Tab

| Key | Action |
|-----|--------|
| `r` | Refresh all values |
| `Enter` | Edit selected pragma value |
| `Esc` | Cancel edit |

---

## 7. Theme System

```rust
pub struct Theme {
    // Base colors
    pub bg: Color,
    pub fg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub accent: Color,
    pub error: Color,
    pub success: Color,
    pub warning: Color,

    // Component-specific
    pub null_style: Style,          // Dimmed italic for NULL values
    pub header_style: Style,        // Bold, underlined for table headers
    pub selected_style: Style,      // Highlighted row/item
    pub status_bar_style: Style,

    // SQL syntax highlighting
    pub sql_keyword: Style,         // Bold, accent color
    pub sql_string: Style,          // Green
    pub sql_number: Style,          // Yellow
    pub sql_comment: Style,         // Dimmed
    pub sql_function: Style,        // Cyan
    pub sql_operator: Style,        // Default, bold

    // ER diagram
    pub er_table_border: Style,
    pub er_pk_style: Style,         // Primary key marker
    pub er_fk_style: Style,         // Foreign key marker
    pub er_relationship: Style,     // Connection lines
}

pub const DARK_THEME: Theme = Theme {
    bg: Color::Rgb(30, 30, 46),         // Catppuccin-inspired dark
    fg: Color::Rgb(205, 214, 244),
    border: Color::Rgb(88, 91, 112),
    border_focused: Color::Rgb(137, 180, 250),
    accent: Color::Rgb(137, 180, 250),
    error: Color::Rgb(243, 139, 168),
    success: Color::Rgb(166, 227, 161),
    warning: Color::Rgb(249, 226, 175),
    // ...
};

pub const LIGHT_THEME: Theme = Theme {
    bg: Color::Rgb(239, 241, 245),
    fg: Color::Rgb(76, 79, 105),
    border: Color::Rgb(172, 176, 190),
    border_focused: Color::Rgb(30, 102, 245),
    accent: Color::Rgb(30, 102, 245),
    // ...
};
```

Theme toggle: `Ctrl+T`, persisted to config file, applied instantly.

---

## 8. Database Handle

Each `DatabaseContext` owns its own `DatabaseHandle`. When the user opens a new database
(via CLI args or `Ctrl+O`), a new `DatabaseHandle` is created and wrapped in a fresh
`DatabaseContext`. All handles are independent — queries in one database do not block another.

```rust
/// The main thread owns the receiver; the sender is cloned into each spawned query task.
/// One per open database. Stores the Database (connection factory), not a single Connection.
pub struct DatabaseHandle {
    database: Arc<turso::Database>,   // Connection factory: .connect() creates independent connections
    result_rx: mpsc::Receiver<QueryMessage>,
    result_tx: mpsc::Sender<QueryMessage>,  // Cloned into tokio::spawn tasks, not moved
}

pub enum QueryMessage {
    Completed(QueryResult),
    Failed(String),
    ExplainCompleted(Vec<Vec<String>>, Vec<String>),  // (bytecode rows, plan lines)
}

pub struct QueryResult {
    pub columns: Vec<ColumnDef>,
    pub rows: Vec<Vec<Value>>,         // turso::Value from Row::get_value() — verify variants at impl time
    pub row_count: usize,
    pub execution_time: Duration,
    // EXPLAIN fields are None until user switches to Explain tab (lazy)
    pub explain: Option<Vec<Vec<String>>>,
    pub query_plan: Option<Vec<String>>,
}
```

**Execution flow:**

1. `DatabaseHandle::execute(sql)` clones `Arc<Database>` and `result_tx`, then calls `tokio::spawn(async move { ... })`
2. Task creates a fresh connection: `let conn = database.connect()?` (sync call — `Database::connect()` is not async)
3. Task runs `conn.prepare(sql).await` + `stmt.query().await` (query methods ARE async)
4. Collects rows by calling `rows.next()` in a loop, stopping after 10,000 rows (the memory limit from §10). The row count reflects total fetched, not total in the result set. If 10,000 rows are hit, the result includes a `truncated: true` flag.
5. Measures wall-clock execution time for the user query only (includes row fetching, not EXPLAIN)
6. Sends `QueryMessage::Completed(result)` (with `explain: None`) or `QueryMessage::Failed(err)` via the cloned sender
7. Main event loop calls `result_rx.try_recv()` each frame to check for results
8. When user switches to Explain tab: `DatabaseHandle::explain(sql)` spawns a second task that creates another fresh connection and runs `EXPLAIN` + `EXPLAIN QUERY PLAN` sequentially, sending `QueryMessage::ExplainCompleted(...)`

**Why `database.connect()` per task (not `conn.clone()`):** `Connection::clone()` clones an inner `Arc`, meaning cloned connections **share the same underlying connection state** — concurrent queries would interleave on a single SQLite connection, which is unsafe. Instead, `DatabaseHandle` stores `Arc<Database>` and calls `.connect()` per spawned task to create truly independent connections to the same database file.

**`:memory:` edge case:** For in-memory databases, each `database.connect()` call may create a separate empty database that doesn't see data from other connections. To handle this, `:memory:` databases should use a single connection (no concurrent query tasks) or use a file-backed temp database (`file::memory:?cache=shared` or a temp file). The implementation should test this and choose the appropriate strategy.

**Task panic safety:** All spawned tasks must wrap their body with `catch_unwind` and send `QueryMessage::Failed("Internal error: task panicked")` if the task panics. Without this, a panic drops the `result_tx` sender and the UI stays at "Executing..." forever.

**In-flight query handling:**
- While a query is executing, the status bar shows "Executing..." and `F5` is disabled
- There is no cancellation API in the `turso` crate; the user must wait for completion
- A future version could add cancellation via `Connection` drop + reconnect

**Schema loading:**

- Full schema loaded asynchronously on startup (UI renders immediately with a loading spinner in the schema panel)
- Refreshed after any DDL execution. Detection: strip leading whitespace and SQL comments (`--` to EOL, `/* ... */`), then check if the first token (case-insensitive) is `CREATE`, `ALTER`, or `DROP`. This is intentionally conservative — false positives (e.g., `CREATE` in a string literal) cause an unnecessary schema reload, which is cheap and harmless. False negatives are less likely since DDL statements must start with these keywords.
- Foreign keys loaded per-table on expansion in tree

---

## 9. Config & Persistence

### Config file: `{config_dir}/tursotui/config.toml`

Where `{config_dir}` is resolved via `dirs::config_dir()` — `~/.config/` on Linux, `~/Library/Application Support/` on macOS, `%APPDATA%` on Windows. All paths below use this convention.

```toml
[theme]
mode = "dark"           # "dark" | "light"

[editor]
tab_size = 4
show_line_numbers = true

[results]
max_column_width = 40
null_display = "NULL"

[history]
max_entries = 5000           # SQLite-backed, handles large histories efficiently
```

### Query history: `{config_dir}/tursotui/history.sqlite`

History is stored in a **SQLite database** (not TOML). This gives us indexed search, efficient pagination for large histories, and avoids parsing a giant TOML file on every startup. The history database is managed via `turso` itself — dogfooding the engine.

```sql
CREATE TABLE query_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sql TEXT NOT NULL,
    database_path TEXT NOT NULL,       -- which database this ran against
    timestamp TEXT NOT NULL,           -- ISO 8601
    execution_time_ms INTEGER,
    row_count INTEGER,
    status TEXT NOT NULL DEFAULT 'ok', -- 'ok' | 'error'
    error_message TEXT,                -- NULL if status = 'ok'
    source TEXT NOT NULL DEFAULT 'user' -- 'user' | 'generated' | 'pragma' | 'ddl'
);

CREATE INDEX idx_query_log_timestamp ON query_log(timestamp DESC);
CREATE INDEX idx_query_log_database ON query_log(database_path);
CREATE INDEX idx_query_log_sql ON query_log(sql);
```

**Every executed query is logged** with a `source` tag: `user` (typed in editor), `generated` (schema explorer `o` key, FK navigation), `pragma` (PRAGMA dashboard edits), `ddl` (CREATE/ALTER/DROP). Internal schema introspection queries (e.g., `PRAGMA table_info` for the schema explorer) are excluded. The history panel defaults to showing `user` + `ddl` queries; the `Tab` filter can toggle to show all sources.

**Deduplication:** No deduplication — every execution is logged as a separate entry, even consecutive identical queries. This preserves execution times and row counts for performance comparison. The history panel UI groups consecutive duplicates visually (shows a `×N` badge) but each entry remains individually accessible.

**Retention:** Configurable via `config.toml` `[history] max_entries = 5000`. Oldest entries are pruned on startup when the limit is exceeded.

### Editor auto-save: `{config_dir}/tursotui/buffers/`

The editor buffer for each open database is auto-saved to disk on every keystroke (debounced to at most once per second via a `last_save: Instant` field — the render loop checks if elapsed > 1s and the buffer is dirty before writing). This prevents data loss on crashes, terminal closures, or accidental quits.

```text
{config_dir}/tursotui/buffers/
├── a1b2c3d4.sql          # Buffer for /home/user/myapp.db (hex hash of full path)
├── e5f6a7b8.sql          # Buffer for /data/analytics.db
└── _memory_.sql           # Buffer for :memory: database
```

Buffer filenames use a hex-encoded hash (e.g., first 8 chars of SHA-256) of the **full absolute database path**. This avoids collisions when two databases share the same filename in different directories (e.g., `/a/myapp.db` and `/b/myapp.db`). A companion `buffers/index.toml` maps hashes back to paths for discoverability.

**Behavior:**

- On startup, if a buffer file exists for the opened database, the editor is pre-populated with the saved content (with a subtle status bar note: "Restored editor buffer")
- Buffers are saved as plain `.sql` files (not TOML), one per database
- The file is the raw editor content, exactly as the user left it
- Buffer files are deleted when the user explicitly clears the editor (`Ctrl+L`)
- This is independent of query history — history logs what was executed, buffers preserve what was being typed

---

## 10. Error Handling

| Scenario | Behavior |
|----------|----------|
| SQL syntax error | Results panel shows error with red border, error text, status bar shows "Error" |
| Connection failure | Modal error popup on startup with path info, offer to retry or quit |
| Database locked | Status bar shows "Database busy, retrying..." (turso handles via busy handler) |
| Large result set | Limit to 10,000 rows in memory, show "10,000+ rows (showing first 10,000)". Sorts operate on the in-memory slice only (client-side sort, not re-queried). |
| Invalid PRAGMA edit | Inline error message next to the pragma field |
| Clipboard unavailable | Status bar shows "Clipboard unavailable" for 3 seconds |
| Terminal too small | Render a centered "Terminal too small (min 80x24)" message instead of the UI |
| Non-existent file | Create new database at that path (same as SQLite behavior) |
| Unreadable file | Modal error on startup: "Cannot open: permission denied" with quit option |
| Query in-flight | `F5` disabled, status bar shows "Executing...", `Ctrl+C` is intercepted |
| `Ctrl+C` during query | Status bar shows "Query detached — still running in background, result will be discarded". The tokio task continues to completion internally (no cancellation API in `turso`), but the result is dropped when it arrives. `Ctrl+C` when idle is a no-op (use `Ctrl+Q` to quit). |

---

## 11. CLI Interface

```text
tursotui [OPTIONS] [DATABASE...]

Arguments:
  [DATABASE...]  One or more paths to SQLite/Turso database files [default: :memory:]

Options:
  -t, --theme        Initial theme: dark | light [default: dark]
  -v, --version      Print version and exit
  -h, --help         Print help
```

**Examples:**

```bash
tursotui myapp.db                      # Open one database
tursotui myapp.db analytics.db         # Open two databases as tabs
tursotui                               # Open :memory: database
```

**Argument parsing:** Use `clap` with derive API.

**Note on `--readonly`:** Read-only mode is intentionally omitted from v1. The `turso::Builder` API does not expose a read-only flag as of 0.6.0-pre.4. If needed as a workaround, filesystem permissions (`chmod 444`) can enforce read-only at the OS level. This feature can be added in a future version if the `turso` API gains read-only support.

**Edge cases:**

- No argument → opens `:memory:` database. The database tab and status bar prominently show `[in-memory]` and a one-time startup notice: "In-memory database — data will not persist after exit"
- Multiple arguments → each opens as a separate database tab, first one is active
- Path with spaces → handled by OS/shell quoting
- Path does not exist → creates new empty database (SQLite default)
- Path exists but is not a database → error: "Not a database file"

**Minimum terminal size:** 80 columns x 24 rows. Below this, the app renders a centered error message instead of the UI and waits for the terminal to be resized.

---

## 12. Missing Component Specs

### 12.1 Status Bar

Persistent single-line bar at the bottom of the screen.

**Content (left to right):**
- Left: Context-sensitive keybinding hints for the focused panel (e.g., `F5 Execute  ↑↓ Navigate  / Search`)
- Center: Query execution status (`Executing...`, `3 rows in 1.2ms`, `Error`)
- Right: Database path + row position when in Results (`Row 3 of 147`)

**Behavior:**
- Updates on every focus change, query completion, and error
- Transient messages (clipboard, errors) display for 3 seconds then revert to default. Mechanism: `AppState` holds `transient_message: Option<(String, Instant)>`; the render loop checks `Instant::elapsed() > 3s` each frame and clears it when expired.
- Themed with `status_bar_style`

### 12.2 Help Overlay

Floating popup rendered on top of all panels when `F1` is pressed (or `?` when the editor is not focused).

**Content:** Full keybinding reference organized by section (Global, Query Tab, Admin Tab). Scrollable if taller than terminal.

**Behavior:**

- `F1` or `Esc` dismisses the overlay
- Rendered as a centered bordered box at 60% terminal width, 80% terminal height
- Semi-transparent background effect: render the popup widget without a `Clear` widget underneath — ratatui's pre-rendered terminal buffer shows through. Underlying content appears dimmed by rendering a full-screen block with `Color::Reset` at reduced opacity (or simply drawing the popup border/content directly over the existing buffer).

### 12.3 Query History Panel

Split-pane view rendered when `Ctrl+H` is pressed. Inspired by DataGrip's "Query History" window — a list of all executed queries with a full syntax-highlighted preview.

```text
┌─Query History [myapp.db]─────────────────────────────────────────────┐
│ ┌─Queries──────────────────────┐ ┌─Preview──────────────────────────┐│
│ │ select * from users;         │ │ CREATE TABLE orders (            ││
│ │ select * from orders whe...  │ │   id INTEGER PRIMARY KEY,        ││
│ │ PRAGMA table_info(users);    │ │   user_id INTEGER NOT NULL,      ││
│ │▌CREATE TABLE orders (...     │ │   total REAL DEFAULT 0.0,        ││
│ │ select count(*) from ord...  │ │   created_at TEXT NOT NULL,      ││
│ │ select * from products;      │ │   FOREIGN KEY (user_id)          ││
│ │ PRAGMA foreign_key_list(...  │ │     REFERENCES users(id)         ││
│ │ select 1/0;            [err] │ │ );                               ││
│ │ WITH cte AS (SELECT ...)...  │ │                                  ││
│ │ INSERT INTO users VALUES...  │ │                                  ││
│ └──────────────────────────────┘ │                                  ││
│ 147 queries  ◆ Filter: all dbs  │                                  ││
│                                  └──────────────────────────────────┘│
│ 2026-03-20 14:30  ·  42 rows  ·  12ms  ·  myapp.db                 │
└──────────────────────────────────────────────────────────────────────┘
```

**Layout:** 40% left (query list), 60% right (SQL preview). Rendered as a floating popup at 80% terminal width, 80% terminal height.

**Left panel — query list:**

- Each entry shows truncated SQL (first line, up to panel width)
- Failed queries show a `[err]` badge in red
- Selected entry is highlighted
- Most recent queries first (reverse chronological)

**Right panel — SQL preview:**

- Full SQL text of the selected query with syntax highlighting
- Scrollable for long queries
- Read-only

**Bottom bar — metadata:**

- Timestamp, row count, execution time, database path for the selected entry

**Filtering:**

- `/`: Full-text search across query SQL (filters the list in real-time)
- `Tab`: Cycle database filter: "all databases" → each open database → "all databases"
- `e`: Filter to errors only (toggle)

**Keybindings:**

- `j/k` or `↑/↓`: Navigate query list
- `Enter`: Recall selected query into the editor (replaces buffer, dismisses popup)
- `Shift+Enter`: Recall AND execute immediately
- `Esc` or `Ctrl+H`: Dismiss
- `/`: Search within history
- `Tab`: Cycle database filter
- `e`: Toggle error-only filter
- `d`: Delete selected entry from history
- `y`: Copy selected query SQL to clipboard

**State:**

```rust
pub struct QueryHistoryPanel {
    entries: Vec<HistoryEntry>,       // Full list from SQLite
    filtered: Vec<usize>,            // Indices into entries after filtering
    selected: usize,                  // Index into filtered
    search: Option<String>,
    db_filter: DbFilter,              // All | Specific(database_path)
    errors_only: bool,
    preview_scroll: usize,            // Scroll offset for right panel
}

pub struct HistoryEntry {
    pub id: i64,
    pub sql: String,
    pub database_path: String,
    pub timestamp: String,            // ISO 8601
    pub execution_time_ms: Option<u64>,
    pub row_count: Option<u64>,
    pub status: QueryStatus,          // Ok | Error(String)
}

pub enum DbFilter { All, Specific(String) }
pub enum QueryStatus { Ok, Error(String) }
```

### 12.4 DML Preview Popup

Floating popup showing the SQL statements that will execute when changes are submitted.

**Trigger:** `Ctrl+D` (preview only) or `Ctrl+S` (preview + submit on confirm).

**Content:** Syntax-highlighted SQL statements grouped by operation type:

```sql
-- 1 UPDATE
UPDATE users SET name = 'Bob2', email = 'bob@new.com' WHERE id = 2;

-- 1 INSERT
INSERT INTO users (name, email) VALUES ('NewUser', NULL);

-- 1 DELETE
DELETE FROM users WHERE id = 5;
```

**Behavior:**

- Scrollable if many changes
- `Enter` submits (when opened via `Ctrl+S`), disabled when opened via `Ctrl+D`
- `Esc` dismisses without submitting
- Summary line at bottom: "3 statements (1 update, 1 insert, 1 delete)"

### 12.5 File Picker Popup

Floating popup rendered when `Ctrl+O` is pressed or the `[+]` database tab is clicked.

**Content:** Simple path input with directory browsing. Not a full file manager — just enough to navigate to a `.db` file.

**Behavior:**

- Text input for typing a path directly (with tab-completion for directory/file names)
- `Enter` opens the typed path as a new database tab
- `Esc` dismisses without opening
- If the path doesn't exist, creates a new database (SQLite default behavior)
- If the path is already open in another tab, switches to that tab instead of opening a duplicate

**Implementation:** A simple text input popup (similar to the history popup), not a full tree-based file browser. For v1, typing a path is sufficient. A richer file picker can be added later.

### 12.6 Go to Object (Quick Navigation)

Floating popup for instant fuzzy navigation to any database object across **all** open databases. Inspired by DataGrip's "Navigate to Object" and VS Code's `Ctrl+P`.

```text
┌─Go to Object───────────────────────────────────┐
│ > usr                                           │
│ ▌ T users          table     [myapp.db]        │
│   T user_roles     table     [myapp.db]        │
│   I idx_users_email index    [myapp.db]        │
│   V user_stats     view      [analytics.db]    │
│   T user_events    table     [analytics.db]    │
│   . user_id        column    users [myapp.db]  │
└────────────────────────────────────────────────┘
```

**Trigger:** `Ctrl+P` (global, works from any panel or sub-tab).

**Data source:** The combined schema cache from all open `DatabaseContext` instances. The schema is already loaded and cached per-database — no extra queries needed to populate the navigator.

**Searchable object types:**

| Type    | Icon | Example                        |
|---------|------|--------------------------------|
| Table   | `T`  | `users`                        |
| Index   | `I`  | `idx_users_email`              |
| View    | `V`  | `user_stats`                   |
| Trigger | `!`  | `trg_audit_insert`             |
| Column  | `.`  | `user_id` (shows parent table) |

Icons are single ASCII characters rendered in the theme's accent color, with the type letter styled distinctly (e.g., `T` for table in bold cyan, `V` for view in green). This avoids unicode rendering inconsistencies across terminals.

**Each result row shows:**

- Type icon (left)
- Object name (bold/accent color, primary match target)
- Object type label (dimmed: "table", "index", etc.)
- Parent context for columns: table name (dimmed)
- Database label in brackets (dimmed: `[myapp.db]`) — essential for multi-database disambiguation

**Search behavior:**

- Fuzzy substring match on object name (case-insensitive)
- Results ranked by: exact prefix match > word-boundary match > substring match > column matches
- Tables and views rank higher than columns and indexes (most-likely navigation targets first)
- Results from the active database rank higher than other databases
- Typing updates results instantly (no debounce needed — schema cache is in-memory)
- Empty search shows recently navigated objects (last 10)

**Navigation on `Enter`:**

1. Switch to the target object's database tab (if not already active)
2. Switch to the Query sub-tab
3. Expand the schema explorer to reveal the object
4. Highlight/select the object in the schema tree
5. For columns: expand the parent table, then select the column

**Keybindings within popup:**

| Key | Action |
|-----|--------|
| Typing | Filter results |
| `j/k` or `↑/↓` | Navigate results |
| `Enter` | Go to selected object |
| `Esc` | Dismiss |
| `Ctrl+P` | Dismiss (toggle) |

**State:**

```rust
pub struct GoToObject {
    query: String,
    results: Vec<ObjectMatch>,
    selected: usize,
    recent: Vec<ObjectRef>,       // Last 10 navigated objects
}

pub struct ObjectMatch {
    name: String,
    kind: ObjectKind,             // Table | Index | View | Trigger | Column
    parent: Option<String>,       // Parent table name (for columns)
    database_idx: usize,          // Index into AppState.databases
    database_label: String,       // Display name of the database
    score: u32,                   // Match ranking score
}

pub enum ObjectKind { Table, Index, View, Trigger, Column }

/// Lightweight reference to a database object — used for navigation history and Action enum.
pub struct ObjectRef {
    pub name: String,
    pub kind: ObjectKind,
    pub parent: Option<String>,       // Parent table (for columns)
    pub database_idx: usize,
}
```

**Performance:** With all schemas cached in memory, filtering is a simple linear scan + score + sort. Even with 10 open databases averaging 100 tables each (1000+ objects), this completes in microseconds — no async needed.

---

## 13. Success Criteria

- [ ] Opens any SQLite or Turso `.db` file from command line: `tursotui myapp.db`
- [ ] Also supports `:memory:` databases
- [ ] Multiple databases open simultaneously as tabs: `tursotui db1.db db2.db`
- [ ] Open additional databases in-app via `Ctrl+O`, close with `Ctrl+W`
- [ ] Each database has independent schema, editor, results, and admin state
- [ ] Schema explorer shows all tables, indexes, views, triggers with column details
- [ ] Go to Object (`Ctrl+P`) fuzzy-searches all objects across all open databases
- [ ] Query editor with SQL syntax highlighting, multi-line editing, undo/redo
- [ ] Query results displayed in scrollable, sortable table with resizable columns
- [ ] Data editor: inline cell editing, add/delete/clone rows for single-table queries
- [ ] Staged changes with color-coded indicators (modified, inserted, deleted)
- [ ] DML preview before submitting changes, transaction-wrapped execution
- [ ] FK navigation: follow foreign keys to referenced rows, back-navigation
- [ ] EXPLAIN and EXPLAIN QUERY PLAN views generated on demand (lazy)
- [ ] ER diagram renders table relationships with box-drawing characters
- [ ] Record detail view for single-row inspection
- [ ] Admin tab shows database info and editable PRAGMAs
- [ ] Dark and light themes with instant toggle
- [ ] Query history persisted in SQLite, searchable split-pane viewer with SQL preview
- [ ] History filterable by database and error status
- [ ] Editor auto-save: buffer restored on restart, no typing lost on crash
- [ ] NULL values visually distinct
- [ ] Copy cell/row to clipboard
- [ ] Contextual help bar + full help overlay
- [ ] Responsive UI during long queries (non-blocking execution)
- [ ] Works on Linux, macOS, and Windows terminals

---

## 14. Testing Strategy

The standalone tursotui repo needs its own test infrastructure. Priority areas:

**Unit tests (highest priority):**

- **DML generation** (§5.9): The most business-logic-heavy and highest-risk code. Every combination of INSERT/UPDATE/DELETE with various column types, NULL handling, PK types, and edge cases (quoted identifiers, empty strings vs NULL). A single incorrect WHERE clause in a DELETE could destroy data.
- **Editability detection heuristic**: Test the keyword-based detector against a corpus of queries — simple SELECTs (should be editable), JOINs, CTEs, UNIONs, aggregates (should NOT be editable), and edge cases.
- **SQL syntax highlighter**: Token-by-token tests for keywords, strings, numbers, comments, functions.
- **Column width algorithm**: Test auto-sizing with various value lengths, unicode widths.

**Integration tests (with `:memory:` database):**

- Full query execution flow: execute → collect rows → display
- Schema loading + refresh after DDL
- PRAGMA read + edit round-trip
- Data editor: edit cell → preview DML → submit → verify data changed
- History logging: execute query → verify entry in history.sqlite
- FK navigation: follow FK → verify correct row selected in parent table

**Snapshot tests (optional, for rendering):**

- Render key components to a `Buffer` and compare against saved snapshots (ratatui's `TestBackend` supports this)
- Useful for catching unintentional layout regressions
