#![allow(
    dead_code,
    reason = "SchemaExplorer is wired into main.rs in a later task"
)]

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::app::{Action, Direction};
use crate::db::{ColumnInfo, SchemaEntry};
use crate::theme::Theme;

use super::Component;

/// A node in the schema tree — either a table/view header or a column under one.
#[derive(Debug, Clone)]
enum TreeNode {
    Table {
        name: String,
        obj_type: String, // "table" or "view"
        expanded: bool,
        columns: Vec<ColumnInfo>,
        columns_loaded: bool,
    },
    Column {
        table_index: usize, // index into tables vec
        col: ColumnInfo,
    },
}

/// A tree-view sidebar showing database tables and their columns.
///
/// Tables are expandable/collapsible. Selecting a table loads its columns
/// via `Action::LoadColumns`. Press `o` to populate the editor with
/// `SELECT * FROM "table_name" LIMIT 100;`.
pub(crate) struct SchemaExplorer {
    /// Only `TreeNode::Table` variants are stored here.
    tables: Vec<TreeNode>,
    /// Flattened view: tables interleaved with their expanded columns.
    visible: Vec<TreeNode>,
    selected: usize,
    scroll_offset: usize,
}

impl SchemaExplorer {
    pub(crate) fn new() -> Self {
        Self {
            tables: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            scroll_offset: 0,
        }
    }

    /// Replace all schema entries. Called when `SchemaLoaded` arrives.
    pub(crate) fn set_schema(&mut self, entries: &[SchemaEntry]) {
        self.tables = entries
            .iter()
            .filter(|e| e.obj_type == "table" || e.obj_type == "view")
            .map(|e| TreeNode::Table {
                name: e.name.clone(),
                obj_type: e.obj_type.clone(),
                expanded: false,
                columns: Vec::new(),
                columns_loaded: false,
            })
            .collect();
        self.selected = 0;
        self.scroll_offset = 0;
        self.rebuild_visible();
    }

    /// Attach columns to a table. Called when `ColumnsLoaded` arrives.
    pub(crate) fn set_columns(&mut self, table_name: &str, columns: Vec<ColumnInfo>) {
        for node in &mut self.tables {
            if let TreeNode::Table {
                name,
                columns: table_cols,
                columns_loaded,
                ..
            } = node
                && name == table_name
            {
                *table_cols = columns;
                *columns_loaded = true;
                break;
            }
        }
        self.rebuild_visible();
    }

    /// Find the index into `self.tables` for the currently selected visible node.
    /// Returns `None` if selection is invalid.
    fn selected_table_index(&self) -> Option<usize> {
        match self.visible.get(self.selected) {
            Some(TreeNode::Table { name, .. }) => self
                .tables
                .iter()
                .position(|t| matches!(t, TreeNode::Table { name: n, .. } if *n == *name)),
            Some(TreeNode::Column { table_index, .. }) => Some(*table_index),
            None => None,
        }
    }

    /// Collapse a table by its index in `self.tables` and move selection to it.
    fn collapse_to_parent(&mut self, table_idx: usize) {
        if let Some(TreeNode::Table { expanded, .. }) = self.tables.get_mut(table_idx) {
            *expanded = false;
        }
        let parent_name = match self.tables.get(table_idx) {
            Some(TreeNode::Table { name, .. }) => name.clone(),
            _ => return,
        };
        self.rebuild_visible();
        if let Some(pos) = self
            .visible
            .iter()
            .position(|node| matches!(node, TreeNode::Table { name: n, .. } if *n == parent_name))
        {
            self.selected = pos;
        }
    }

    /// Toggle expand/collapse of the currently selected table.
    /// Returns `Some(Action::LoadColumns(...))` if columns have not been loaded yet.
    fn toggle_expand(&mut self) -> Option<Action> {
        let table_idx = match self.visible.get(self.selected) {
            Some(TreeNode::Table { .. }) => self.selected_table_index()?,
            Some(TreeNode::Column { table_index, .. }) => {
                // Selected is a column — collapse its parent.
                let idx = *table_index;
                self.collapse_to_parent(idx);
                return None;
            }
            None => return None,
        };

        let (currently_expanded, columns_loaded, table_name) = match &self.tables[table_idx] {
            TreeNode::Table {
                expanded,
                columns_loaded,
                name,
                ..
            } => (*expanded, *columns_loaded, name.clone()),
            TreeNode::Column { .. } => return None,
        };

        if currently_expanded {
            // Collapse
            if let TreeNode::Table { expanded, .. } = &mut self.tables[table_idx] {
                *expanded = false;
            }
            self.rebuild_visible();
            None
        } else {
            // Expand
            if let TreeNode::Table { expanded, .. } = &mut self.tables[table_idx] {
                *expanded = true;
            }
            self.rebuild_visible();

            if columns_loaded {
                None
            } else {
                Some(Action::LoadColumns(table_name))
            }
        }
    }

    /// Rebuild the `visible` list from `tables` and their expanded columns.
    fn rebuild_visible(&mut self) {
        self.visible.clear();
        for (table_idx, node) in self.tables.iter().enumerate() {
            match node {
                TreeNode::Table {
                    expanded, columns, ..
                } => {
                    self.visible.push(node.clone());
                    if *expanded {
                        for col in columns {
                            self.visible.push(TreeNode::Column {
                                table_index: table_idx,
                                col: col.clone(),
                            });
                        }
                    }
                }
                TreeNode::Column { .. } => {
                    // `self.tables` should only contain Table nodes; skip any stale entries.
                }
            }
        }
        // Clamp selection to valid range.
        if self.visible.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.visible.len() {
            self.selected = self.visible.len() - 1;
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        if !self.visible.is_empty() && self.selected + 1 < self.visible.len() {
            self.selected += 1;
        }
    }

    /// Adjust `scroll_offset` to keep `selected` item in the visible window.
    fn adjust_scroll(&mut self, visible_height: usize) {
        if visible_height == 0 {
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible_height {
            self.scroll_offset = self.selected - visible_height + 1;
        }
    }
}

impl Component for SchemaExplorer {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            // Navigation
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                self.move_down();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.move_up();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                if !self.visible.is_empty() {
                    self.selected = self.visible.len() - 1;
                }
                None
            }

            // Enter → toggle expand/collapse on table nodes; no-op on columns.
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if matches!(
                    self.visible.get(self.selected),
                    Some(TreeNode::Column { .. })
                ) {
                    return None;
                }
                self.toggle_expand()
            }

            // Space / l / → → toggle expand/collapse (on columns, collapses parent).
            (KeyModifiers::NONE, KeyCode::Char(' ' | 'l') | KeyCode::Right) => self.toggle_expand(),

            // o → populate editor with SELECT * FROM table.
            (KeyModifiers::NONE, KeyCode::Char('o')) => {
                let table_idx = self.selected_table_index()?;
                if let TreeNode::Table { name, .. } = &self.tables[table_idx] {
                    let quoted = name.replace('"', "\"\"");
                    let sql = format!("SELECT * FROM \"{quoted}\" LIMIT 100;");
                    Some(Action::PopulateEditor(sql))
                } else {
                    None
                }
            }

            // Collapse / move to parent
            (KeyModifiers::NONE, KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace) => {
                match self.visible.get(self.selected) {
                    Some(TreeNode::Table { expanded: true, .. }) => self.toggle_expand(),
                    Some(TreeNode::Column { table_index, .. }) => {
                        let idx = *table_index;
                        self.collapse_to_parent(idx);
                        None
                    }
                    _ => None,
                }
            }

            // Focus cycling
            (KeyModifiers::NONE, KeyCode::Tab | KeyCode::Esc) => {
                Some(Action::CycleFocus(Direction::Forward))
            }
            (_, KeyCode::BackTab) => Some(Action::CycleFocus(Direction::Backward)),

            _ => None,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let border_style = if focused {
            Style::default().fg(theme.border_focused)
        } else {
            Style::default().fg(theme.border)
        };
        let title_style = if focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };

        let block = Block::bordered()
            .border_style(border_style)
            .title("Schema")
            .title_style(title_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let visible_height = inner.height as usize;
        self.adjust_scroll(visible_height);

        let show_scrollbar = self.visible.len() > visible_height;
        let content_width = if show_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        let dim_style = Style::default().add_modifier(Modifier::DIM);
        let pk_indicator = "* ";
        let no_pk = "  ";

        for (display_idx, item_idx) in
            (self.scroll_offset..self.scroll_offset + visible_height).enumerate()
        {
            let Some(node) = self.visible.get(item_idx) else {
                break;
            };

            let y = inner.y + display_idx as u16;
            if y >= inner.y + inner.height {
                break;
            }

            let is_selected = item_idx == self.selected;
            let row_area = Rect {
                x: inner.x,
                y,
                width: content_width,
                height: 1,
            };

            match node {
                TreeNode::Table {
                    name,
                    expanded,
                    obj_type,
                    ..
                } => {
                    let arrow = if *expanded { "▼ " } else { "▶ " };
                    let type_hint = if obj_type == "view" { " [view]" } else { "" };
                    let text = format!("{arrow}{name}{type_hint}");

                    let style = if is_selected {
                        theme.selected_style
                    } else {
                        Style::default().fg(theme.fg)
                    };

                    // Truncate to fit content width
                    let display = truncate_str(&text, content_width as usize);
                    let line = Paragraph::new(display).style(style);
                    frame.render_widget(line, row_area);
                }
                TreeNode::Column { col, .. } => {
                    let pk_mark = if col.pk { pk_indicator } else { no_pk };

                    let base_style = if is_selected {
                        theme.selected_style
                    } else {
                        Style::default().fg(theme.fg)
                    };

                    // Render as a Line with mixed styles: name normal, type dimmed
                    let name_part = format!("  {pk_mark}{}", col.name);
                    let type_part = format!(" : {}", col.col_type);

                    // Build spans: if selected, all highlighted; otherwise name+type dimmed type
                    let cw = content_width as usize;
                    let widget = if is_selected {
                        let total = format!("{name_part}{type_part}");
                        let display = truncate_str(&total, cw);
                        Paragraph::new(display).style(base_style)
                    } else {
                        // Name part in normal style, type part dimmed
                        let name_display = truncate_str(&name_part, cw);
                        let name_chars = name_display.chars().count();
                        let type_display = if name_chars < cw {
                            truncate_str(&type_part, cw - name_chars)
                        } else {
                            String::new()
                        };
                        let spans = vec![
                            Span::styled(name_display, Style::default().fg(theme.fg)),
                            Span::styled(type_display, dim_style.fg(theme.border)),
                        ];
                        Paragraph::new(Line::from(spans))
                    };

                    frame.render_widget(widget, row_area);
                }
            }
        }

        if show_scrollbar {
            let scrollbar_area = Rect {
                x: inner.x + content_width,
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            let mut scrollbar_state =
                ScrollbarState::new(self.visible.len()).position(self.scroll_offset);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }
    }
}

/// Truncate a string to at most `max_chars` characters (not bytes).
fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        // Leave room for "…"
        let take = max_chars.saturating_sub(1);
        let truncated: String = s.chars().take(take).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ColumnInfo, SchemaEntry};

    fn make_schema_entry(obj_type: &str, name: &str) -> SchemaEntry {
        SchemaEntry {
            obj_type: obj_type.to_string(),
            name: name.to_string(),
            tbl_name: name.to_string(),
            sql: None,
        }
    }

    fn make_column(name: &str, col_type: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            col_type: col_type.to_string(),
            notnull: false,
            default_value: None,
            pk,
        }
    }

    fn table_name(node: &TreeNode) -> Option<&str> {
        match node {
            TreeNode::Table { name, .. } => Some(name.as_str()),
            TreeNode::Column { .. } => None,
        }
    }

    fn is_expanded(node: &TreeNode) -> bool {
        matches!(node, TreeNode::Table { expanded: true, .. })
    }

    // --- test_set_schema_creates_tables ---

    #[test]
    fn test_set_schema_creates_tables() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "posts"),
            make_schema_entry("index", "idx_users_email"), // should be filtered out
            make_schema_entry("view", "active_users"),
        ];
        explorer.set_schema(&entries);

        // Only tables and views should appear.
        assert_eq!(explorer.tables.len(), 3);
        assert_eq!(table_name(&explorer.tables[0]), Some("users"));
        assert_eq!(table_name(&explorer.tables[1]), Some("posts"));
        assert_eq!(table_name(&explorer.tables[2]), Some("active_users"));

        // All start collapsed, selection at 0.
        assert!(!is_expanded(&explorer.tables[0]));
        assert!(!is_expanded(&explorer.tables[1]));
        assert_eq!(explorer.selected, 0);

        // Visible = just the table headers (no columns yet).
        assert_eq!(explorer.visible.len(), 3);
    }

    // --- test_toggle_expand_triggers_load_columns ---

    #[test]
    fn test_toggle_expand_triggers_load_columns() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // First expand: columns not loaded → returns LoadColumns action.
        let action = explorer.toggle_expand();
        assert!(
            matches!(action, Some(Action::LoadColumns(ref name)) if name == "users"),
            "Expected LoadColumns(\"users\"), got {action:?}"
        );
        assert!(is_expanded(&explorer.tables[0]));

        // Collapse.
        let action = explorer.toggle_expand();
        assert!(action.is_none());
        assert!(!is_expanded(&explorer.tables[0]));

        // Mark columns as loaded (simulate ColumnsLoaded arriving).
        explorer.set_columns("users", vec![make_column("id", "INTEGER", true)]);

        // Expand again: columns already loaded → no action.
        let action = explorer.toggle_expand();
        assert!(action.is_none());
        assert!(is_expanded(&explorer.tables[0]));
    }

    // --- test_set_columns_attaches_to_table ---

    #[test]
    fn test_set_columns_attaches_to_table() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("table", "posts"),
        ]);

        // Expand users so columns show in visible.
        explorer.toggle_expand(); // users → LoadColumns (ignore action)
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("email", "TEXT", false),
            ],
        );

        // Visible should be: users (expanded), id column, email column, posts (collapsed)
        assert_eq!(explorer.visible.len(), 4);
        assert!(matches!(&explorer.visible[0], TreeNode::Table { name, .. } if name == "users"));
        assert!(matches!(&explorer.visible[1], TreeNode::Column { col, .. } if col.name == "id"));
        assert!(
            matches!(&explorer.visible[2], TreeNode::Column { col, .. } if col.name == "email")
        );
        assert!(matches!(&explorer.visible[3], TreeNode::Table { name, .. } if name == "posts"));

        // set_columns for a nonexistent table is a no-op.
        explorer.set_columns("nonexistent", vec![make_column("x", "TEXT", false)]);
        assert_eq!(explorer.visible.len(), 4); // unchanged
    }

    // --- test_navigation_up_down ---

    #[test]
    fn test_navigation_up_down() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "a"),
            make_schema_entry("table", "b"),
            make_schema_entry("table", "c"),
        ]);

        assert_eq!(explorer.selected, 0);

        explorer.move_down();
        assert_eq!(explorer.selected, 1);

        explorer.move_down();
        assert_eq!(explorer.selected, 2);

        // Already at last — move_down is a no-op.
        explorer.move_down();
        assert_eq!(explorer.selected, 2);

        explorer.move_up();
        assert_eq!(explorer.selected, 1);

        explorer.move_up();
        assert_eq!(explorer.selected, 0);

        // Already at first — move_up is a no-op.
        explorer.move_up();
        assert_eq!(explorer.selected, 0);
    }

    // --- test_collapse_hides_columns ---

    #[test]
    fn test_collapse_hides_columns() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Expand and load columns.
        explorer.toggle_expand();
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("name", "TEXT", false),
            ],
        );

        // Expanded: users + 2 columns = 3 visible.
        assert_eq!(explorer.visible.len(), 3);

        // Collapse by toggling again (selection is still on users table node).
        explorer.toggle_expand();

        // Collapsed: only the table header.
        assert_eq!(explorer.visible.len(), 1);
        assert!(
            matches!(&explorer.visible[0], TreeNode::Table { name, expanded: false, .. } if name == "users")
        );
    }

    // --- test_navigate_into_columns ---

    #[test]
    fn test_navigate_into_columns() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        explorer.toggle_expand();
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("name", "TEXT", false),
            ],
        );

        // visible: [users, id, name]
        assert_eq!(explorer.selected, 0); // on users
        explorer.move_down();
        assert_eq!(explorer.selected, 1); // on id
        explorer.move_down();
        assert_eq!(explorer.selected, 2); // on name
        explorer.move_down(); // no-op (last item)
        assert_eq!(explorer.selected, 2);
    }

    // --- test_set_schema_resets_state ---

    #[test]
    fn test_set_schema_resets_state() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "a")]);
        explorer.selected = 0;

        // Re-load schema — everything resets.
        explorer.set_schema(&[
            make_schema_entry("table", "x"),
            make_schema_entry("table", "y"),
        ]);
        assert_eq!(explorer.selected, 0);
        assert_eq!(explorer.tables.len(), 2);
        assert_eq!(explorer.visible.len(), 2);
    }

    // --- test_empty_schema ---

    #[test]
    fn test_empty_schema() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[]);
        assert_eq!(explorer.tables.len(), 0);
        assert_eq!(explorer.visible.len(), 0);
        assert_eq!(explorer.selected, 0);

        // Navigation on empty is a no-op.
        explorer.move_down();
        explorer.move_up();
        assert_eq!(explorer.selected, 0);
    }

    // --- test_truncate_str ---

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_exact() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        let result = truncate_str("hello world", 7);
        // "hello w" → take 6 chars + "…" = 7 display chars
        assert_eq!(result.chars().count(), 7);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn test_truncate_str_zero() {
        assert_eq!(truncate_str("hello", 0), "");
    }

    // --- toggle_expand on column node ---

    #[test]
    fn test_toggle_expand_on_column_collapses_parent() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Expand and load columns.
        explorer.toggle_expand();
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("name", "TEXT", false),
            ],
        );
        // visible: [users, id, name]
        assert_eq!(explorer.visible.len(), 3);

        // Move to column "id" and toggle_expand → collapses parent.
        explorer.move_down();
        assert_eq!(explorer.selected, 1);
        let action = explorer.toggle_expand();
        assert!(action.is_none());
        // Parent collapsed, selection moved to parent.
        assert_eq!(explorer.visible.len(), 1);
        assert_eq!(explorer.selected, 0);
    }

    // --- Enter on column is no-op ---

    #[test]
    fn test_enter_on_column_is_noop() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.toggle_expand();
        explorer.set_columns("users", vec![make_column("id", "INTEGER", true)]);

        // Move to column
        explorer.move_down();
        assert_eq!(explorer.selected, 1);

        // Enter on column: no-op (doesn't collapse parent)
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(action.is_none());
        // Still expanded, selection unchanged
        assert_eq!(explorer.visible.len(), 2);
        assert_eq!(explorer.selected, 1);
    }

    // --- o key populates editor ---

    #[test]
    fn test_o_key_populates_editor() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(
            matches!(action, Some(Action::PopulateEditor(ref sql)) if sql.contains("\"users\"")),
            "Expected PopulateEditor with quoted table name, got {action:?}"
        );
    }

    #[test]
    fn test_o_key_escapes_quotes_in_table_name() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "my\"table")]);

        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(
            matches!(action, Some(Action::PopulateEditor(ref sql)) if sql.contains("\"my\"\"table\"")),
            "Expected escaped double quotes, got {action:?}"
        );
    }
}
