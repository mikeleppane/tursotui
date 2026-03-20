# Milestone 1: Skeleton + Core Loop

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A running TUI application that opens a SQLite/Turso database, renders the full layout with placeholder panels, supports focus cycling between panels, theme display, and clean quit.

**Architecture:** Single-database skeleton (multi-db in Milestone 7). Entry point sets up crossterm + ratatui, starts a tokio runtime, opens the database, then enters the main event loop: poll crossterm events → map to Actions → update state → render. Each panel area is a bordered placeholder with its name. Focus cycling highlights the focused panel's border.

**Tech Stack:** Rust 2024 edition, ratatui 0.30, crossterm (via ratatui), tokio 1.x, turso (path dep), clap 4.x

**Spec reference:** `docs/specs/design.md` — Sections 2 (Foundation), 3 (Architecture), 4 (Layout), 6 (Keybindings/Focus), 7 (Theme), 11 (CLI)

---

## File Structure

After Milestone 1, the project will have these files:

```
src/
├── main.rs          # Entry point: CLI parsing, terminal setup/teardown, tokio runtime
├── app.rs           # AppState, DatabaseContext, Action enum, PanelId, SubTab, BottomTab
├── event.rs         # Event loop: crossterm poll → Action mapping, global key handling
├── db.rs            # DatabaseHandle: Arc<Database>, mpsc channels, connection factory
├── theme.rs         # Theme struct, DARK_THEME constant
├── components/
│   ├── mod.rs       # Component trait definition
│   └── placeholder.rs  # Placeholder component (renders bordered box with label)
```

Additional files at root:
- `Cargo.toml` — dependencies
- `CLAUDE.md` — agent instructions for this project

---

## Task 1: Project Setup (Cargo.toml + CLAUDE.md)

**Files:**
- Modify: `Cargo.toml`
- Create: `CLAUDE.md`

- [ ] **Step 1: Update Cargo.toml with all Milestone 1 dependencies**

```toml
[package]
name = "tursotui"
version = "0.1.0"
edition = "2024"

[dependencies]
# Database engine — path dep during development, switch to git/crates.io when publishing
# Using default-features = false to avoid mimalloc global allocator override
turso = { path = "../turso/bindings/rust", default-features = false }

# TUI framework (0.30 includes crossterm re-export)
ratatui = "0.30"

# Async runtime — needed for turso's Builder::build().await
tokio = { version = "1", features = ["full"] }

# CLI argument parsing
clap = { version = "4", features = ["derive"] }
```

- [ ] **Step 2: Create CLAUDE.md with project-specific instructions**

```markdown
# tursotui — Terminal UI for Turso/SQLite

## Quick Reference

    cargo build                  # build (never release mode during dev)
    cargo run -- test.db         # run with a database file
    cargo run                    # run with :memory: database
    cargo test                   # run tests
    cargo fmt                    # format
    cargo clippy -- -D warnings  # lint

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
```

- [ ] **Step 3: Verify the project compiles**

Run: `cargo build`
Expected: Successful compilation with empty main

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock CLAUDE.md
git commit -m "setup: configure dependencies and project instructions"
```

---

## Task 2: Theme System

**Files:**
- Create: `src/theme.rs`

- [ ] **Step 1: Create the Theme struct and DARK_THEME constant**

```rust
// src/theme.rs
use ratatui::style::{Color, Modifier, Style};

/// Visual theme for the entire application.
/// Every styled element references a field here — no hardcoded colors elsewhere.
pub struct Theme {
    // Base
    pub bg: Color,
    pub fg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub accent: Color,
    pub error: Color,
    pub success: Color,
    pub warning: Color,

    // Components
    pub null_style: Style,
    pub header_style: Style,
    pub selected_style: Style,
    pub status_bar_style: Style,

    // SQL highlighting (used in later milestones)
    pub sql_keyword: Style,
    pub sql_string: Style,
    pub sql_number: Style,
    pub sql_comment: Style,
    pub sql_function: Style,
    pub sql_operator: Style,

    // ER diagram (used in later milestones)
    pub er_table_border: Style,
    pub er_pk_style: Style,
    pub er_fk_style: Style,
    pub er_relationship: Style,
}

/// Catppuccin Mocha-inspired dark theme.
pub const DARK_THEME: Theme = Theme {
    bg: Color::Rgb(30, 30, 46),
    fg: Color::Rgb(205, 214, 244),
    border: Color::Rgb(88, 91, 112),
    border_focused: Color::Rgb(137, 180, 250),
    accent: Color::Rgb(137, 180, 250),
    error: Color::Rgb(243, 139, 168),
    success: Color::Rgb(166, 227, 161),
    warning: Color::Rgb(249, 226, 175),

    null_style: Style::new().fg(Color::Rgb(88, 91, 112)).add_modifier(Modifier::ITALIC),
    header_style: Style::new().fg(Color::Rgb(205, 214, 244)).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    selected_style: Style::new().fg(Color::Rgb(30, 30, 46)).bg(Color::Rgb(137, 180, 250)),
    status_bar_style: Style::new().fg(Color::Rgb(205, 214, 244)).bg(Color::Rgb(49, 50, 68)),

    sql_keyword: Style::new().fg(Color::Rgb(137, 180, 250)).add_modifier(Modifier::BOLD),
    sql_string: Style::new().fg(Color::Rgb(166, 227, 161)),
    sql_number: Style::new().fg(Color::Rgb(249, 226, 175)),
    sql_comment: Style::new().fg(Color::Rgb(88, 91, 112)),
    sql_function: Style::new().fg(Color::Rgb(148, 226, 213)),
    sql_operator: Style::new().add_modifier(Modifier::BOLD),

    er_table_border: Style::new().fg(Color::Rgb(137, 180, 250)),
    er_pk_style: Style::new().fg(Color::Rgb(249, 226, 175)).add_modifier(Modifier::BOLD),
    er_fk_style: Style::new().fg(Color::Rgb(148, 226, 213)),
    er_relationship: Style::new().fg(Color::Rgb(88, 91, 112)),
};
```

- [ ] **Step 2: Verify it compiles**

Add `mod theme;` to `main.rs` temporarily and run `cargo build`.

- [ ] **Step 3: Commit**

```bash
git add src/theme.rs
git commit -m "feat: add Theme struct and Catppuccin dark theme"
```

---

## Task 3: Component Trait

**Files:**
- Create: `src/components/mod.rs`
- Create: `src/components/placeholder.rs`

- [ ] **Step 1: Define the Component trait**

```rust
// src/components/mod.rs
pub mod placeholder;

use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::prelude::*;

use crate::app::Action;
use crate::theme::Theme;

/// Every panel in the UI implements this trait.
pub trait Component {
    /// Handle a key event when this component has focus.
    /// Returns Some(Action) if the key produced a state change, None if ignored.
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action>;

    /// Handle a mouse event. Default: ignore.
    fn handle_mouse(&mut self, _mouse: MouseEvent) -> Option<Action> {
        None
    }

    /// React to an action dispatched by the app.
    fn update(&mut self, action: &Action);

    /// Render into the given area. `focused` indicates whether this panel has keyboard focus.
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme);
}
```

- [ ] **Step 2: Create the Placeholder component**

A simple bordered box showing its label. Used for all panels in Milestone 1.

```rust
// src/components/placeholder.rs
use ratatui::crossterm::event::KeyEvent;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::Action;
use crate::theme::Theme;

use super::Component;

/// Temporary placeholder panel — renders a bordered box with a label.
/// Replaced by real components in later milestones.
pub struct Placeholder {
    label: String,
}

impl Placeholder {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl Component for Placeholder {
    fn handle_key(&mut self, _key: KeyEvent) -> Option<Action> {
        None // Placeholders don't handle keys
    }

    fn update(&mut self, _action: &Action) {}

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let border_style = if focused {
            Style::default().fg(theme.border_focused)
        } else {
            Style::default().fg(theme.border)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(self.label.as_str())
            .title_style(if focused {
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            });

        let content = Paragraph::new(format!("[{}]", self.label))
            .style(Style::default().fg(theme.fg))
            .alignment(Alignment::Center)
            .block(block);

        frame.render_widget(content, area);
    }
}
```

- [ ] **Step 3: Verify it compiles**

Add `mod components;` to `main.rs` (requires `mod app;` stub — create a minimal `app.rs` with `pub enum Action { Quit }` to satisfy the import). Run `cargo build`.

- [ ] **Step 4: Commit**

```bash
git add src/components/
git commit -m "feat: add Component trait and Placeholder component"
```

---

## Task 4: Application State and Action Enum

**Files:**
- Create: `src/app.rs`

- [ ] **Step 1: Define core types (PanelId, SubTab, BottomTab, Action)**

```rust
// src/app.rs
use std::time::Instant;

use crate::db::DatabaseHandle;
use crate::theme::{Theme, DARK_THEME};

// --- Enums ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubTab {
    Query,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    Results,
    Explain,
    Detail,
    ERDiagram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelId {
    Schema,
    Editor,
    Bottom,
    DbInfo,
    Pragmas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// All state mutations flow through actions.
/// Milestone 1 only uses a subset — the rest are defined but unused.
#[derive(Debug)]
pub enum Action {
    // Navigation
    SwitchSubTab(SubTab),
    FocusPanel(PanelId),
    CycleFocus(Direction),
    ToggleSidebar,
    SwitchBottomTab(BottomTab),

    // Global
    ToggleTheme,
    ShowHelp,
    Quit,
}
```

- [ ] **Step 2: Define DatabaseContext and AppState**

Append to `src/app.rs`:

```rust
/// Per-database workspace.
pub struct DatabaseContext {
    pub handle: DatabaseHandle,
    pub path: String,
    pub label: String,

    // Navigation
    pub sub_tab: SubTab,
    pub focus: PanelId,
    pub sidebar_visible: bool,
    pub bottom_tab: BottomTab,
}

impl DatabaseContext {
    pub fn new(handle: DatabaseHandle, path: String) -> Self {
        let label = if path == ":memory:" {
            "[in-memory]".to_string()
        } else {
            std::path::Path::new(&path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone())
        };

        Self {
            handle,
            path,
            label,
            sub_tab: SubTab::Query,
            focus: PanelId::Editor,
            sidebar_visible: true,
            bottom_tab: BottomTab::Results,
        }
    }

    /// Returns the ordered list of focusable panels for the current sub-tab.
    pub fn focusable_panels(&self) -> Vec<PanelId> {
        match self.sub_tab {
            SubTab::Query => {
                let mut panels = vec![];
                if self.sidebar_visible {
                    panels.push(PanelId::Schema);
                }
                panels.push(PanelId::Editor);
                panels.push(PanelId::Bottom);
                panels
            }
            SubTab::Admin => vec![PanelId::DbInfo, PanelId::Pragmas],
        }
    }

    /// Cycle focus to the next/previous panel.
    pub fn cycle_focus(&mut self, direction: Direction) {
        let panels = self.focusable_panels();
        if panels.is_empty() {
            return;
        }
        let current = panels.iter().position(|p| *p == self.focus).unwrap_or(0);
        let next = match direction {
            Direction::Forward => (current + 1) % panels.len(),
            Direction::Backward => (current + panels.len() - 1) % panels.len(),
        };
        self.focus = panels[next];
    }
}

/// Global application state.
pub struct AppState {
    pub databases: Vec<DatabaseContext>,
    pub active_db: usize,
    pub theme: Theme,
    pub transient_message: Option<(String, Instant)>,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(db_context: DatabaseContext) -> Self {
        Self {
            databases: vec![db_context],
            active_db: 0,
            theme: DARK_THEME,
            transient_message: None,
            should_quit: false,
        }
    }

    /// Get the active database context.
    pub fn active_db(&self) -> &DatabaseContext {
        &self.databases[self.active_db]
    }

    /// Get the active database context mutably.
    pub fn active_db_mut(&mut self) -> &mut DatabaseContext {
        &mut self.databases[self.active_db]
    }

    /// Process an action and update state.
    pub fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::CycleFocus(dir) => self.active_db_mut().cycle_focus(dir),
            Action::FocusPanel(panel) => self.active_db_mut().focus = panel,
            Action::SwitchSubTab(tab) => {
                let db = self.active_db_mut();
                db.sub_tab = tab;
                // Reset focus to first panel of new sub-tab
                let panels = db.focusable_panels();
                if let Some(&first) = panels.first() {
                    db.focus = first;
                }
            }
            Action::ToggleSidebar => {
                let db = self.active_db_mut();
                db.sidebar_visible = !db.sidebar_visible;
                // If sidebar was hidden and focus was on Schema, move focus
                if !db.sidebar_visible && db.focus == PanelId::Schema {
                    db.focus = PanelId::Editor;
                }
            }
            Action::SwitchBottomTab(tab) => {
                self.active_db_mut().bottom_tab = tab;
            }
            Action::ToggleTheme => {
                // Only dark theme in M1; light theme added later
            }
            Action::ShowHelp => {
                // Help overlay added in later milestone
            }
        }
    }
}
```

- [ ] **Step 3: Verify it compiles**

This requires a `DatabaseHandle` stub. Create a minimal `src/db.rs`:

```rust
// src/db.rs — stub for Task 4 compilation
pub struct DatabaseHandle;
```

Update `main.rs`:
```rust
mod app;
mod components;
mod db;
mod theme;

fn main() {}
```

Run: `cargo build`

- [ ] **Step 4: Commit**

```bash
git add src/app.rs src/db.rs src/main.rs
git commit -m "feat: add AppState, DatabaseContext, Action enum, and core types"
```

---

## Task 5: Database Handle

**Files:**
- Modify: `src/db.rs`

- [ ] **Step 1: Implement DatabaseHandle**

Replace the stub with the real implementation:

```rust
// src/db.rs
use std::sync::Arc;
use tokio::sync::mpsc;

/// Messages sent from query tasks back to the main loop.
#[derive(Debug)]
pub enum QueryMessage {
    // Defined here for later milestones. M1 doesn't use them.
}

/// Wraps an Arc<Database> and provides a channel for receiving query results.
/// One per open database.
pub struct DatabaseHandle {
    pub database: Arc<turso::Database>,
    pub path: String,
    pub result_rx: mpsc::UnboundedReceiver<QueryMessage>,
    pub result_tx: mpsc::UnboundedSender<QueryMessage>,
}

impl DatabaseHandle {
    /// Open a database at the given path.
    /// Uses turso::Builder::new_local().build().await (async) for Database creation,
    /// then stores it in an Arc for sharing with spawned tasks.
    pub async fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let database = turso::Builder::new_local(path).build().await?;
        let (result_tx, result_rx) = mpsc::unbounded_channel();

        Ok(Self {
            database: Arc::new(database),
            path: path.to_string(),
            result_rx,
            result_tx,
        })
    }

    /// Create a fresh, independent connection for a query task.
    pub fn connect(&self) -> Result<turso::Connection, Box<dyn std::error::Error>> {
        Ok(self.database.connect()?)
    }

    /// Check for completed query results (non-blocking).
    pub fn try_recv(&mut self) -> Option<QueryMessage> {
        self.result_rx.try_recv().ok()
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: Success (may have unused warnings — that's fine)

- [ ] **Step 3: Commit**

```bash
git add src/db.rs
git commit -m "feat: add DatabaseHandle with Arc<Database> and mpsc channels"
```

---

## Task 6: Event Handling

**Files:**
- Create: `src/event.rs`

- [ ] **Step 1: Implement global key mapping**

```rust
// src/event.rs
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, KeyEventKind};
use std::time::Duration;

use crate::app::{Action, Direction, SubTab};

/// Poll for a crossterm event with the given timeout.
/// Returns None if no event occurred within the timeout.
pub fn poll_event(timeout: Duration) -> std::io::Result<Option<Event>> {
    if event::poll(timeout)? {
        Ok(Some(event::read()?))
    } else {
        Ok(None)
    }
}

/// Map a key event to an Action. Handles global keys that work regardless of focus.
/// Returns None if the key should be forwarded to the focused component.
pub fn map_global_key(key: KeyEvent) -> Option<Action> {
    // Only handle key press events, not release/repeat
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match (key.modifiers, key.code) {
        // Quit
        (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(Action::Quit),

        // Focus cycling
        (KeyModifiers::CONTROL, KeyCode::Tab) => Some(Action::CycleFocus(Direction::Forward)),
        (KeyModifiers::NONE, KeyCode::Tab) => Some(Action::CycleFocus(Direction::Forward)),
        (KeyModifiers::SHIFT, KeyCode::BackTab) => Some(Action::CycleFocus(Direction::Backward)),
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::CycleFocus(Direction::Forward)),

        // Sidebar toggle
        (KeyModifiers::CONTROL, KeyCode::Char('b')) => Some(Action::ToggleSidebar),

        // Sub-tab switching (Alt+1, Alt+2)
        (KeyModifiers::ALT, KeyCode::Char('1')) => Some(Action::SwitchSubTab(SubTab::Query)),
        (KeyModifiers::ALT, KeyCode::Char('2')) => Some(Action::SwitchSubTab(SubTab::Admin)),

        // Theme toggle
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => Some(Action::ToggleTheme),

        // Help
        (KeyModifiers::NONE, KeyCode::F(1)) => Some(Action::ShowHelp),

        _ => None, // Not a global key — forward to focused component
    }
}
```

- [ ] **Step 2: Verify it compiles**

Add `mod event;` to `main.rs`. Run: `cargo build`

- [ ] **Step 3: Commit**

```bash
git add src/event.rs src/main.rs
git commit -m "feat: add event polling and global key mapping"
```

---

## Task 7: Layout Rendering

**Files:**
- Modify: `src/main.rs`

This is the big integration task. It wires everything together: CLI parsing, terminal setup, database open, event loop, layout rendering.

- [ ] **Step 1: Implement the full main.rs**

```rust
// src/main.rs
mod app;
mod components;
mod db;
mod event;
mod theme;

use std::time::Duration;

use clap::Parser;
use ratatui::crossterm::event::Event;
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

use app::{Action, AppState, DatabaseContext, PanelId, SubTab};
use components::placeholder::Placeholder;
use components::Component;
use db::DatabaseHandle;
use theme::Theme;

/// Terminal UI for Turso and SQLite databases.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to SQLite/Turso database file(s). Defaults to :memory:
    #[arg(default_value = ":memory:")]
    database: Vec<String>,

    /// Initial theme: dark | light
    #[arg(short, long, default_value = "dark")]
    theme: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Open the first database (multi-db support in Milestone 7)
    let path = cli.database.first().map(|s| s.as_str()).unwrap_or(":memory:");
    let handle = DatabaseHandle::open(path).await?;
    let db_context = DatabaseContext::new(handle, path.to_string());
    let mut app = AppState::new(db_context);

    // Initialize terminal
    let mut terminal = ratatui::init();

    // Main event loop
    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
    ratatui::restore();

    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    // Placeholder components (replaced by real ones in later milestones)
    let mut schema = Placeholder::new("Schema Explorer");
    let mut editor = Placeholder::new("Query Editor");
    let mut bottom = Placeholder::new("Results");
    let mut db_info = Placeholder::new("Database Info");
    let mut pragmas = Placeholder::new("PRAGMA Dashboard");
    let mut status_msg = String::new();

    loop {
        if app.should_quit {
            break;
        }

        // 1. Render
        let db = app.active_db();
        let theme = &app.theme;
        let sub_tab = db.sub_tab;
        let focus = db.focus;
        let sidebar_visible = db.sidebar_visible;
        let label = db.label.clone();
        let bottom_tab = db.bottom_tab;

        terminal.draw(|frame| {
            render_layout(
                frame,
                theme,
                &label,
                sub_tab,
                focus,
                sidebar_visible,
                bottom_tab,
                &mut schema,
                &mut editor,
                &mut bottom,
                &mut db_info,
                &mut pragmas,
                &status_msg,
            );
        })?;

        // 2. Poll events (16ms ≈ 60fps)
        if let Some(event) = event::poll_event(Duration::from_millis(16))? {
            if let Event::Key(key) = event {
                // Try global keys first
                if let Some(action) = crate::event::map_global_key(key) {
                    app.update(action);
                }
                // In later milestones: forward to focused component if not consumed
            }
        }

        // 3. Update status message
        status_msg = format!(
            "Focus: {:?}  |  Tab/Esc: cycle  |  Ctrl+B: sidebar  |  Alt+1/2: Query/Admin  |  Ctrl+Q: quit",
            app.active_db().focus,
        );
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_layout(
    frame: &mut Frame,
    theme: &Theme,
    db_label: &str,
    sub_tab: SubTab,
    focus: PanelId,
    sidebar_visible: bool,
    bottom_tab: app::BottomTab,
    schema: &mut Placeholder,
    editor: &mut Placeholder,
    bottom: &mut Placeholder,
    db_info: &mut Placeholder,
    pragmas: &mut Placeholder,
    status_msg: &str,
) {
    let area = frame.area();

    // Check minimum terminal size
    if area.width < 80 || area.height < 24 {
        let msg = Paragraph::new("Terminal too small (min 80x24)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.error));
        frame.render_widget(msg, area);
        return;
    }

    // Top level: db tabs + sub-tabs + content + status bar
    let [db_tabs_area, sub_tabs_area, content_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // --- Database tab bar ---
    let db_tabs = Tabs::new(vec![format!(" {} ", db_label)])
        .select(0)
        .style(Style::default().fg(theme.fg).bg(theme.bg))
        .highlight_style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD));
    frame.render_widget(db_tabs, db_tabs_area);

    // --- Sub-tab bar ---
    let sub_tab_index = match sub_tab {
        SubTab::Query => 0,
        SubTab::Admin => 1,
    };
    let sub_tabs = Tabs::new(vec![" Query ", " Admin "])
        .select(sub_tab_index)
        .style(Style::default().fg(theme.fg))
        .highlight_style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    frame.render_widget(sub_tabs, sub_tabs_area);

    // --- Content area ---
    match sub_tab {
        SubTab::Query => {
            render_query_tab(
                frame, theme, content_area, focus, sidebar_visible, bottom_tab,
                schema, editor, bottom,
            );
        }
        SubTab::Admin => {
            render_admin_tab(frame, theme, content_area, focus, db_info, pragmas);
        }
    }

    // --- Status bar ---
    let status = Paragraph::new(status_msg)
        .style(theme.status_bar_style);
    frame.render_widget(status, status_area);
}

fn render_query_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    sidebar_visible: bool,
    _bottom_tab: app::BottomTab,
    schema: &mut Placeholder,
    editor: &mut Placeholder,
    bottom: &mut Placeholder,
) {
    if sidebar_visible {
        let [sidebar_area, main_area] = Layout::horizontal([
            Constraint::Percentage(20),
            Constraint::Percentage(80),
        ])
        .areas(area);

        schema.render(frame, sidebar_area, focus == PanelId::Schema, theme);

        let [editor_area, bottom_area] = Layout::vertical([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ])
        .areas(main_area);

        editor.render(frame, editor_area, focus == PanelId::Editor, theme);
        bottom.render(frame, bottom_area, focus == PanelId::Bottom, theme);
    } else {
        let [editor_area, bottom_area] = Layout::vertical([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ])
        .areas(area);

        editor.render(frame, editor_area, focus == PanelId::Editor, theme);
        bottom.render(frame, bottom_area, focus == PanelId::Bottom, theme);
    }
}

fn render_admin_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    db_info: &mut Placeholder,
    pragmas: &mut Placeholder,
) {
    let [left, right] = Layout::horizontal([
        Constraint::Percentage(40),
        Constraint::Percentage(60),
    ])
    .areas(area);

    db_info.render(frame, left, focus == PanelId::DbInfo, theme);
    pragmas.render(frame, right, focus == PanelId::Pragmas, theme);
}
```

- [ ] **Step 2: Verify it compiles and runs**

Run: `cargo build`
Then: `cargo run` (opens `:memory:` database)
Expected: TUI renders with placeholder panels, focused panel has blue border, Ctrl+Q quits cleanly.

- [ ] **Step 3: Test key interactions manually**

Verify each of these works:

- `Tab` / `Esc`: cycles focus between Schema → Editor → Results (focus border moves)
- `Ctrl+B`: toggles sidebar (Schema panel appears/disappears)
- `Alt+1` / `Alt+2`: switches between Query and Admin sub-tabs
- `Ctrl+Q`: quits cleanly (terminal restored)
- Resize terminal below 80x24: shows "Terminal too small" message

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire up event loop, layout rendering, and focus cycling"
```

---

## Task 8: Test with a Real Database File

**Files:** (no new files — manual verification)

- [ ] **Step 1: Create a test database**

```bash
sqlite3 /tmp/test.db "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT); INSERT INTO users VALUES (1, 'Alice', 'alice@example.com'); INSERT INTO users VALUES (2, 'Bob', 'bob@example.com');"
```

- [ ] **Step 2: Run tursotui with the test database**

```bash
cargo run -- /tmp/test.db
```

Expected: TUI renders, database tab shows `test.db` instead of `[in-memory]`. All key interactions work the same.

- [ ] **Step 3: Test with a non-existent path**

```bash
cargo run -- /tmp/new_empty.db
```

Expected: Creates a new empty database (SQLite default behavior). TUI renders normally.

- [ ] **Step 4: Test --help and --version**

```bash
cargo run -- --help
cargo run -- --version
```

Expected: Help text shows usage with `[DATABASE...]` argument and `--theme` option. Version shows `tursotui 0.1.0`.

---

## Milestone 1 Completion Criteria

After completing all tasks, verify:

- [ ] `cargo build` succeeds with no errors
- [ ] `cargo clippy -- -D warnings` passes (fix any warnings)
- [ ] `cargo fmt` produces no changes
- [ ] `cargo run` opens with `:memory:` database, shows `[in-memory]` label
- [ ] `cargo run -- some.db` opens with the file, shows filename in tab bar
- [ ] Tab/Esc cycles focus (visible border change)
- [ ] Ctrl+B toggles sidebar
- [ ] Alt+1/Alt+2 switches Query/Admin tabs
- [ ] Ctrl+Q quits cleanly
- [ ] Terminal is fully restored after quit (no garbled output)
- [ ] Small terminal shows "Terminal too small" message

---

## What's Next

**Milestone 2: Schema Explorer + Query Editor** — Implements the tree view for database schema and the SQL editor with syntax highlighting and undo/redo. These are the first real `Component` implementations replacing the placeholders.
