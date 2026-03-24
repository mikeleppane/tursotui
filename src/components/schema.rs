use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::app::{Action, Direction, ObjectKind};
use crate::db::{ColumnInfo, CustomTypeInfo, SchemaEntry};
use crate::theme::Theme;

use super::Component;

/// Approximate panel height used for scroll adjustment in `reveal_and_select`.
/// Actual height is only available at render time; this is a conservative estimate.
const ESTIMATED_VISIBLE_ROWS: usize = 20;

/// The kind of top-level grouping category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CategoryKind {
    Tables,
    Views,
    Indexes,
    Triggers,
    CustomTypes,
}

/// A node in the schema tree: category header, table/view, index, trigger, or column.
#[derive(Debug, Clone)]
enum TreeNode {
    Category {
        label: String, // e.g. "Tables (3)"
        kind: CategoryKind,
        expanded: bool,
    },
    Table {
        name: String,
        obj_type: String, // "table" or "view"
        expanded: bool,
        columns: Vec<ColumnInfo>,
        columns_loaded: bool,
    },
    Index {
        name: String,
        table_name: String,
    },
    Trigger {
        name: String,
        table_name: String,
    },
    Column {
        table_name: String,
        col: ColumnInfo,
    },
    CustomType {
        name: String,
        parent_type: String,
    },
}

/// A tree-view sidebar showing database tables, views, indexes, and triggers
/// organized under category headers.
///
/// Categories are expandable/collapsible. Tables within categories are also
/// expandable to show columns. Selecting a table loads its columns
/// via `Action::LoadColumns`. Press `o` to populate the editor with
/// `SELECT * FROM "table_name" LIMIT 100;`.
pub(crate) struct SchemaExplorer {
    /// Each entry is (category header, children under that category).
    categories: Vec<(TreeNode, Vec<TreeNode>)>,
    /// Flattened view: category headers, children, and expanded columns.
    visible: Vec<TreeNode>,
    selected: usize,
    scroll_offset: usize,
    /// Active search filter. `None` means no filter; `Some("")` means filter bar
    /// is open but no text has been typed yet (show everything).
    filter: Option<String>,
    /// `true` while the user is typing into the filter input bar.
    filter_active: bool,
    /// Cache of DDL SQL keyed by object name, populated on schema load.
    ddl_cache: std::collections::HashMap<String, String>,
    /// Lowercase names of custom types for column annotation.
    custom_type_names: std::collections::HashSet<String>,
    /// Approximate row counts keyed by lowercase table name.
    row_counts: std::collections::HashMap<String, u64>,
}

impl SchemaExplorer {
    pub(crate) fn new() -> Self {
        Self {
            categories: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            filter: None,
            filter_active: false,
            custom_type_names: std::collections::HashSet::new(),
            ddl_cache: std::collections::HashMap::new(),
            row_counts: std::collections::HashMap::new(),
        }
    }

    /// Replace all schema entries. Called when `SchemaLoaded` arrives.
    pub(crate) fn set_schema(&mut self, entries: &[SchemaEntry]) {
        let mut tables = Vec::new();
        let mut views = Vec::new();
        let mut indexes = Vec::new();
        let mut triggers = Vec::new();

        for e in entries {
            match e.obj_type.as_str() {
                "table" => tables.push(TreeNode::Table {
                    name: e.name.clone(),
                    obj_type: e.obj_type.clone(),
                    expanded: false,
                    columns: Vec::new(),
                    columns_loaded: false,
                }),
                "view" => views.push(TreeNode::Table {
                    name: e.name.clone(),
                    obj_type: e.obj_type.clone(),
                    expanded: false,
                    columns: Vec::new(),
                    columns_loaded: false,
                }),
                "index" => indexes.push(TreeNode::Index {
                    name: e.name.clone(),
                    table_name: e.tbl_name.clone(),
                }),
                "trigger" => triggers.push(TreeNode::Trigger {
                    name: e.name.clone(),
                    table_name: e.tbl_name.clone(),
                }),
                _ => {}
            }
        }

        self.categories.clear();

        if !tables.is_empty() {
            let header = TreeNode::Category {
                label: format!("Tables ({})", tables.len()),
                kind: CategoryKind::Tables,
                expanded: true,
            };
            self.categories.push((header, tables));
        }

        if !views.is_empty() {
            let header = TreeNode::Category {
                label: format!("Views ({})", views.len()),
                kind: CategoryKind::Views,
                expanded: true,
            };
            self.categories.push((header, views));
        }

        if !indexes.is_empty() {
            let header = TreeNode::Category {
                label: format!("Indexes ({})", indexes.len()),
                kind: CategoryKind::Indexes,
                expanded: false,
            };
            self.categories.push((header, indexes));
        }

        if !triggers.is_empty() {
            let header = TreeNode::Category {
                label: format!("Triggers ({})", triggers.len()),
                kind: CategoryKind::Triggers,
                expanded: false,
            };
            self.categories.push((header, triggers));
        }

        self.selected = 0;
        self.scroll_offset = 0;
        self.filter = None;
        self.filter_active = false;

        self.ddl_cache.clear();
        for e in entries {
            if let Some(ref sql) = e.sql {
                self.ddl_cache.insert(e.name.clone(), sql.clone());
            }
        }

        self.rebuild_visible();
    }

    /// Replace custom types. Called when `CustomTypesLoaded` arrives.
    pub(crate) fn set_custom_types(&mut self, types: &[CustomTypeInfo]) {
        // Update the name set for column annotation.
        self.custom_type_names = types.iter().map(|t| t.name.to_lowercase()).collect();

        // Remove any existing CustomTypes category.
        self.categories.retain(|(h, _)| {
            !matches!(
                h,
                TreeNode::Category {
                    kind: CategoryKind::CustomTypes,
                    ..
                }
            )
        });

        // Only show user-defined types in the tree category (not Turso built-ins).
        let user_defined: Vec<TreeNode> = types
            .iter()
            .filter(|t| !t.builtin)
            .map(|t| TreeNode::CustomType {
                name: t.name.clone(),
                parent_type: t.parent.clone(),
            })
            .collect();

        if user_defined.is_empty() {
            self.rebuild_visible();
            return;
        }

        let header = TreeNode::Category {
            label: format!("Custom Types ({})", user_defined.len()),
            kind: CategoryKind::CustomTypes,
            expanded: false,
        };
        let children = user_defined;
        self.categories.push((header, children));
        self.rebuild_visible();
    }

    /// Attach columns to a table. Called when `ColumnsLoaded` arrives.
    pub(crate) fn set_columns(&mut self, table_name: &str, columns: Vec<ColumnInfo>) {
        for (_header, children) in &mut self.categories {
            for child in children.iter_mut() {
                if let TreeNode::Table {
                    name,
                    columns: table_cols,
                    columns_loaded,
                    ..
                } = child
                    && name == table_name
                {
                    *table_cols = columns;
                    *columns_loaded = true;
                    self.rebuild_visible();
                    return;
                }
            }
        }
    }

    /// Update row counts for display next to table names.
    pub(crate) fn set_row_counts(&mut self, counts: &std::collections::HashMap<String, u64>) {
        self.row_counts.clone_from(counts);
    }

    /// Look up cached DDL SQL for a schema object by name.
    fn get_ddl_sql(&self, name: &str) -> Option<String> {
        self.ddl_cache.get(name).cloned()
    }

    /// Find a mutable reference to a Table node by name across all categories.
    fn find_table_mut(&mut self, target_name: &str) -> Option<&mut TreeNode> {
        for (_header, children) in &mut self.categories {
            for child in children.iter_mut() {
                if matches!(child, TreeNode::Table { name, .. } if name == target_name) {
                    return Some(child);
                }
            }
        }
        None
    }

    /// Collapse a table by name and move selection to it.
    fn collapse_table_to_parent(&mut self, target_name: &str) {
        if let Some(TreeNode::Table { expanded, .. }) = self.find_table_mut(target_name) {
            *expanded = false;
        }
        self.rebuild_visible();
        if let Some(pos) = self
            .visible
            .iter()
            .position(|node| matches!(node, TreeNode::Table { name, .. } if name == target_name))
        {
            self.selected = pos;
        }
    }

    /// Determine which category kind the currently selected item belongs to.
    fn selected_parent_category_kind(&self) -> Option<CategoryKind> {
        let selected_node = self.visible.get(self.selected)?;
        match selected_node {
            TreeNode::Category { kind, .. } => Some(*kind),
            TreeNode::Table { obj_type, .. } => {
                if obj_type == "view" {
                    Some(CategoryKind::Views)
                } else {
                    Some(CategoryKind::Tables)
                }
            }
            TreeNode::Index { .. } => Some(CategoryKind::Indexes),
            TreeNode::Trigger { .. } => Some(CategoryKind::Triggers),
            TreeNode::CustomType { .. } => Some(CategoryKind::CustomTypes),
            TreeNode::Column { table_name, .. } => {
                // Find which category this table's column belongs to
                for (header, children) in &self.categories {
                    if let TreeNode::Category { kind, .. } = header {
                        for child in children {
                            if matches!(child, TreeNode::Table { name, .. } if name == table_name) {
                                return Some(*kind);
                            }
                        }
                    }
                }
                None
            }
        }
    }

    /// Toggle expand/collapse of the currently selected node.
    /// Returns `Some(Action::LoadColumns(...))` if columns have not been loaded yet.
    fn toggle_expand(&mut self) -> Option<Action> {
        let selected_node = self.visible.get(self.selected)?.clone();
        match selected_node {
            TreeNode::Category { kind, expanded, .. } => {
                // Toggle category expand/collapse
                for (header, _children) in &mut self.categories {
                    if let TreeNode::Category {
                        kind: k,
                        expanded: e,
                        ..
                    } = header
                        && *k == kind
                    {
                        *e = !expanded;
                        break;
                    }
                }
                self.rebuild_visible();
                None
            }
            TreeNode::Table {
                name,
                expanded,
                columns_loaded,
                ..
            } => {
                if expanded {
                    // Collapse
                    if let Some(TreeNode::Table {
                        expanded: e_ref, ..
                    }) = self.find_table_mut(&name)
                    {
                        *e_ref = false;
                    }
                    self.rebuild_visible();
                    None
                } else {
                    // Expand
                    if let Some(TreeNode::Table {
                        expanded: e_ref, ..
                    }) = self.find_table_mut(&name)
                    {
                        *e_ref = true;
                    }
                    self.rebuild_visible();
                    if columns_loaded {
                        None
                    } else {
                        Some(Action::LoadColumns(name))
                    }
                }
            }
            TreeNode::Column { table_name, .. } => {
                // Collapse the parent table
                self.collapse_table_to_parent(&table_name);
                None
            }
            TreeNode::Index { .. } | TreeNode::Trigger { .. } | TreeNode::CustomType { .. } => {
                // Leaf nodes: no-op (use h/Left to navigate to parent)
                None
            }
        }
    }

    /// Rebuild the `visible` list from `categories` and their expanded children.
    ///
    /// When a non-empty filter is active, only leaf nodes whose name contains
    /// the query (case-insensitive) are shown.  Category headers are included
    /// only when at least one child matches.  Columns are shown when their
    /// parent table matches and is expanded.
    fn rebuild_visible(&mut self) {
        self.visible.clear();

        let query = self
            .filter
            .as_deref()
            .filter(|q| !q.is_empty())
            .map(str::to_lowercase);

        for (header, children) in &self.categories {
            if let TreeNode::Category { expanded, .. } = header {
                if *expanded {
                    // Collect matching children first so we can decide
                    // whether to show the category header.
                    let mut matched_children: Vec<TreeNode> = Vec::new();
                    for child in children {
                        let child_matches = match (&query, child) {
                            (
                                Some(q),
                                TreeNode::Table { name, .. }
                                | TreeNode::Index { name, .. }
                                | TreeNode::Trigger { name, .. }
                                | TreeNode::CustomType { name, .. },
                            ) => name.to_lowercase().contains(q.as_str()),
                            (None, _) => true,
                            _ => false,
                        };

                        if child_matches {
                            matched_children.push(child.clone());
                            // If child is an expanded table, push its columns
                            if let TreeNode::Table {
                                name,
                                expanded: table_expanded,
                                columns,
                                ..
                            } = child
                                && *table_expanded
                            {
                                for col in columns {
                                    matched_children.push(TreeNode::Column {
                                        table_name: name.clone(),
                                        col: col.clone(),
                                    });
                                }
                            }
                        }
                    }

                    if query.is_none() || !matched_children.is_empty() {
                        self.visible.push(header.clone());
                        self.visible.extend(matched_children);
                    }
                } else {
                    // Collapsed category: show header only if no filter or
                    // any child would match.
                    let show = match &query {
                        None => true,
                        Some(q) => children.iter().any(|child| {
                            let (TreeNode::Table { name, .. }
                            | TreeNode::Index { name, .. }
                            | TreeNode::Trigger { name, .. }
                            | TreeNode::CustomType { name, .. }) = child
                            else {
                                return false;
                            };
                            name.to_lowercase().contains(q.as_str())
                        }),
                    };
                    if show {
                        self.visible.push(header.clone());
                    }
                }
            }
        }

        // Clamp selection to valid range.
        if self.visible.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.visible.len() {
            self.selected = self.visible.len() - 1;
        }
        // Clamp scroll_offset to valid range.
        if !self.visible.is_empty() && self.scroll_offset >= self.visible.len() {
            self.scroll_offset = self.visible.len().saturating_sub(1);
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

    /// Handle keystrokes while the filter input bar is active.
    fn handle_filter_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.filter = None;
                self.filter_active = false;
                self.rebuild_visible();
                None
            }
            KeyCode::Enter => {
                self.filter_active = false;
                None
            }
            KeyCode::Down => {
                self.filter_active = false;
                self.move_down();
                None
            }
            KeyCode::Up => {
                self.filter_active = false;
                self.move_up();
                None
            }
            KeyCode::Backspace => {
                if let Some(ref mut s) = self.filter {
                    s.pop();
                }
                self.rebuild_visible();
                None
            }
            KeyCode::Char(c) => {
                if let Some(ref mut s) = self.filter {
                    s.push(c);
                } else {
                    self.filter = Some(String::from(c));
                }
                self.rebuild_visible();
                None
            }
            _ => None,
        }
    }

    /// Navigate the schema tree to reveal and select a specific object.
    ///
    /// Used by Go to Object (`Ctrl+P`) to navigate to search results.
    /// Expands ancestor nodes, rebuilds the visible vec, and selects the target.
    ///
    /// For columns with unloaded parent: falls back to selecting the parent table.
    /// Returns `true` if the target (or parent fallback) was found and selected.
    pub(crate) fn reveal_and_select(&mut self, name: &str, kind: ObjectKind) -> bool {
        // Clear any active filter
        self.filter = None;
        self.filter_active = false;

        match kind {
            ObjectKind::Table | ObjectKind::View => {
                let cat_kind = if kind == ObjectKind::Table {
                    CategoryKind::Tables
                } else {
                    CategoryKind::Views
                };
                self.expand_category_and_select(
                    cat_kind,
                    name,
                    |node, n| matches!(node, TreeNode::Table { name: tn, .. } if tn == n),
                )
            }
            ObjectKind::Index => self.expand_category_and_select(
                CategoryKind::Indexes,
                name,
                |node, n| matches!(node, TreeNode::Index { name: tn, .. } if tn == n),
            ),
            ObjectKind::Trigger => self.expand_category_and_select(
                CategoryKind::Triggers,
                name,
                |node, n| matches!(node, TreeNode::Trigger { name: tn, .. } if tn == n),
            ),
            ObjectKind::CustomType => self.expand_category_and_select(
                CategoryKind::CustomTypes,
                name,
                |node, n| matches!(node, TreeNode::CustomType { name: tn, .. } if tn == n),
            ),
            ObjectKind::Column => self.reveal_and_select_column(name),
        }
    }

    /// Shared helper: expand a category, rebuild visible, find a node, and scroll to it.
    fn expand_category_and_select(
        &mut self,
        cat_kind: CategoryKind,
        name: &str,
        matches_node: impl Fn(&TreeNode, &str) -> bool,
    ) -> bool {
        // Expand the category and verify the target exists in children
        let mut found_in_children = false;
        for (header, children) in &mut self.categories {
            if let TreeNode::Category {
                kind: k, expanded, ..
            } = header
                && *k == cat_kind
            {
                *expanded = true;
                found_in_children = children.iter().any(|c| matches_node(c, name));
                break;
            }
        }
        if !found_in_children {
            return false;
        }

        self.rebuild_visible();

        // Find and select the target in visible
        if let Some(pos) = self
            .visible
            .iter()
            .position(|node| matches_node(node, name))
        {
            self.select_and_scroll(pos);
            return true;
        }
        false
    }

    /// Handle the Column case of `reveal_and_select`: find the parent table,
    /// expand both the category and table, then select the column.
    fn reveal_and_select_column(&mut self, name: &str) -> bool {
        // Find which table contains this column
        let mut found_table: Option<String> = None;
        let mut columns_loaded = false;

        for (_header, children) in &self.categories {
            for child in children {
                if let TreeNode::Table {
                    name: tbl_name,
                    columns,
                    columns_loaded: loaded,
                    ..
                } = child
                    && columns.iter().any(|c| c.name == name)
                {
                    found_table = Some(tbl_name.clone());
                    columns_loaded = *loaded;
                    break;
                }
            }
            if found_table.is_some() {
                break;
            }
        }

        let Some(table_name) = found_table else {
            return false;
        };

        if !columns_loaded {
            // Fall back to selecting the parent table
            return self.reveal_and_select(&table_name, ObjectKind::Table);
        }

        // Expand the parent category and table
        'outer: for (header, children) in &mut self.categories {
            if let TreeNode::Category { expanded, .. } = header {
                for child in children.iter_mut() {
                    if let TreeNode::Table {
                        name: n,
                        expanded: tbl_exp,
                        ..
                    } = child
                        && *n == table_name
                    {
                        *expanded = true; // category
                        *tbl_exp = true; // table
                        break 'outer;
                    }
                }
            }
        }

        self.rebuild_visible();

        // Find the specific column in visible
        if let Some(pos) = self.visible.iter().position(|node| {
            matches!(node, TreeNode::Column { table_name: tn, col, .. }
                if tn == &table_name && col.name == name)
        }) {
            self.select_and_scroll(pos);
            return true;
        }

        // Column not visible (shouldn't happen if columns_loaded was true)
        false
    }

    /// Set the selected index and adjust scroll to keep it visible.
    fn select_and_scroll(&mut self, pos: usize) {
        self.selected = pos;
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + ESTIMATED_VISIBLE_ROWS {
            self.scroll_offset = self.selected.saturating_sub(ESTIMATED_VISIBLE_ROWS) + 1;
        }
    }
}

impl Component for SchemaExplorer {
    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if self.filter_active {
            return self.handle_filter_key(key);
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

            // Enter: toggle on Category and Table nodes; no-op on Column/Index/Trigger.
            (KeyModifiers::NONE, KeyCode::Enter) => {
                match self.visible.get(self.selected) {
                    Some(TreeNode::Category { .. } | TreeNode::Table { .. }) => {
                        self.toggle_expand()
                    }
                    _ => None, // no-op for Column, Index, Trigger
                }
            }

            // Space / l / Right: toggle expand/collapse (on columns, collapses parent).
            (KeyModifiers::NONE, KeyCode::Char(' ' | 'l') | KeyCode::Right) => self.toggle_expand(),

            // o: populate editor with SELECT * FROM table (only for Table/View nodes).
            (KeyModifiers::NONE, KeyCode::Char('o')) => {
                let node = self.visible.get(self.selected)?;
                match node {
                    TreeNode::Table { name, .. } => {
                        let quoted = name.replace('"', "\"\"");
                        let sql = format!("SELECT * FROM \"{quoted}\" LIMIT 100;");
                        Some(Action::PopulateEditor(sql))
                    }
                    _ => None, // no-op for Category, Index, Trigger, Column
                }
            }

            // D: view DDL for the selected schema object.
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('D')) => {
                if let Some(
                    TreeNode::Table { name, .. }
                    | TreeNode::Index { name, .. }
                    | TreeNode::Trigger { name, .. },
                ) = self.visible.get(self.selected)
                {
                    if let Some(sql) = self.get_ddl_sql(name) {
                        return Some(Action::ShowDdl {
                            name: name.clone(),
                            sql,
                        });
                    }
                    return Some(Action::SetTransient(
                        "No DDL available for this object".to_string(),
                        true,
                    ));
                }
                None
            }

            // Re-activate filter for editing when filter bar is visible
            (KeyModifiers::NONE, KeyCode::Backspace) if self.filter.is_some() => {
                self.filter_active = true;
                self.handle_filter_key(key)
            }

            // Collapse / move to parent
            (KeyModifiers::NONE, KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace) => {
                match self.visible.get(self.selected).cloned() {
                    Some(TreeNode::Category { expanded: true, .. }) => self.toggle_expand(),
                    Some(TreeNode::Table {
                        expanded: true,
                        name,
                        ..
                    }) => {
                        self.collapse_table_to_parent(&name);
                        // Re-expand is wrong here; we want to just collapse.
                        // `collapse_table_to_parent` already collapsed it.
                        None
                    }
                    Some(TreeNode::Table {
                        expanded: false, ..
                    }) => {
                        // Collapsed table: move to parent category
                        if let Some(kind) = self.selected_parent_category_kind()
                            && let Some(pos) = self.visible.iter().position(
                                |n| matches!(n, TreeNode::Category { kind: k, .. } if *k == kind),
                            )
                        {
                            self.selected = pos;
                        }
                        None
                    }
                    Some(TreeNode::Column { table_name, .. }) => {
                        self.collapse_table_to_parent(&table_name);
                        None
                    }
                    Some(
                        TreeNode::Index { .. }
                        | TreeNode::Trigger { .. }
                        | TreeNode::CustomType { .. },
                    ) => {
                        // Move to parent category
                        if let Some(kind) = self.selected_parent_category_kind()
                            && let Some(pos) = self.visible.iter().position(
                                |n| matches!(n, TreeNode::Category { kind: k, .. } if *k == kind),
                            )
                        {
                            self.selected = pos;
                        }
                        None
                    }
                    Some(TreeNode::Category {
                        expanded: false, ..
                    })
                    | None => None,
                }
            }

            // Search filter
            (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.filter = Some(String::new());
                self.filter_active = true;
                None
            }

            // Clear accepted filter on Esc (first press clears filter, second press releases focus)
            (KeyModifiers::NONE, KeyCode::Esc) if self.filter.is_some() => {
                self.filter = None;
                self.filter_active = false;
                self.rebuild_visible();
                None
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
        let block = super::panel_block("Schema", focused, theme);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let filter_bar_showing = self.filter.is_some();
        let visible_height = if filter_bar_showing {
            (inner.height as usize).saturating_sub(1)
        } else {
            inner.height as usize
        };
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
                TreeNode::Category {
                    label,
                    expanded,
                    kind,
                } => {
                    let arrow = if *expanded { "\u{25bc} " } else { "\u{25b6} " };
                    let icon_color = match kind {
                        CategoryKind::Tables => theme.schema_table,
                        CategoryKind::Views => theme.schema_view,
                        CategoryKind::Indexes => theme.schema_index,
                        CategoryKind::Triggers => theme.schema_trigger,
                        CategoryKind::CustomTypes => theme.schema_custom_type,
                    };

                    let cw = content_width as usize;
                    let text = format!("{arrow}{label}");
                    let display = truncate_str(&text, cw);
                    let widget = if is_selected {
                        Paragraph::new(display).style(theme.selected_style)
                    } else {
                        let spans = vec![Span::styled(
                            display,
                            Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
                        )];
                        Paragraph::new(Line::from(spans))
                    };
                    frame.render_widget(widget, row_area);
                }
                TreeNode::Table {
                    name,
                    expanded,
                    obj_type,
                    ..
                } => {
                    let arrow = if *expanded { "\u{25bc} " } else { "\u{25b6} " };
                    let is_view = obj_type == "view";
                    let icon_color = if is_view {
                        theme.schema_view
                    } else {
                        theme.schema_table
                    };
                    let type_hint = if is_view { " [view]" } else { "" };
                    // Row count suffix for tables only (not views)
                    let count_suffix = if is_view {
                        String::new()
                    } else {
                        self.row_counts
                            .get(&name.to_lowercase())
                            .map(|c| format!(" ({})", format_count(*c)))
                            .unwrap_or_default()
                    };

                    let cw = content_width as usize;
                    let name_text = format!("  {arrow}{name}");
                    if is_selected {
                        // Build text, only including count if it fits fully
                        let base = format!("{name_text}{type_hint}");
                        let base_w = unicode_width::UnicodeWidthStr::width(base.as_str());
                        let count_w = unicode_width::UnicodeWidthStr::width(count_suffix.as_str());
                        let text = if base_w + count_w <= cw {
                            format!("{base}{count_suffix}")
                        } else {
                            base
                        };
                        let display = truncate_str(&text, cw);
                        frame.render_widget(
                            Paragraph::new(display).style(theme.selected_style),
                            row_area,
                        );
                    } else {
                        let name_display = truncate_str(&name_text, cw);
                        let name_w = unicode_width::UnicodeWidthStr::width(name_display.as_str());
                        // Build suffix spans: type hint first, then count
                        let hint_display = if name_w < cw && !type_hint.is_empty() {
                            truncate_str(type_hint, cw - name_w)
                        } else {
                            String::new()
                        };
                        let hint_w = unicode_width::UnicodeWidthStr::width(hint_display.as_str());
                        let count_display = if name_w + hint_w < cw && !count_suffix.is_empty() {
                            truncate_str(&count_suffix, cw - name_w - hint_w)
                        } else {
                            String::new()
                        };
                        let spans = vec![
                            Span::styled(name_display, Style::default().fg(icon_color)),
                            Span::styled(hint_display, dim_style),
                            Span::styled(count_display, Style::default().fg(theme.border)),
                        ];
                        frame.render_widget(Paragraph::new(Line::from(spans)), row_area);
                    }
                }
                TreeNode::Index { name, table_name } | TreeNode::Trigger { name, table_name } => {
                    let cw = content_width as usize;
                    let is_trigger = matches!(node, TreeNode::Trigger { .. });
                    let icon_color = if is_trigger {
                        theme.schema_trigger
                    } else {
                        theme.schema_index
                    };
                    let name_part = format!("  {name}");
                    let table_part = format!(" ({table_name})");

                    let widget = if is_selected {
                        let total = format!("{name_part}{table_part}");
                        let display = truncate_str(&total, cw);
                        Paragraph::new(display).style(theme.selected_style)
                    } else {
                        let name_display = truncate_str(&name_part, cw);
                        let name_width =
                            unicode_width::UnicodeWidthStr::width(name_display.as_str());
                        let table_display = if name_width < cw {
                            truncate_str(&table_part, cw - name_width)
                        } else {
                            String::new()
                        };
                        let spans = vec![
                            Span::styled(name_display, Style::default().fg(icon_color)),
                            Span::styled(table_display, dim_style),
                        ];
                        Paragraph::new(Line::from(spans))
                    };

                    frame.render_widget(widget, row_area);
                }
                TreeNode::CustomType { name, parent_type } => {
                    let cw = content_width as usize;
                    let name_part = format!("  {name}");
                    let parent_part = format!(" : {parent_type}");

                    let widget = if is_selected {
                        let total = format!("{name_part}{parent_part}");
                        let display = truncate_str(&total, cw);
                        Paragraph::new(display).style(theme.selected_style)
                    } else {
                        let name_display = truncate_str(&name_part, cw);
                        let name_width =
                            unicode_width::UnicodeWidthStr::width(name_display.as_str());
                        let parent_display = if name_width < cw {
                            truncate_str(&parent_part, cw - name_width)
                        } else {
                            String::new()
                        };
                        let spans = vec![
                            Span::styled(
                                name_display,
                                Style::default().fg(theme.schema_custom_type),
                            ),
                            Span::styled(parent_display, dim_style),
                        ];
                        Paragraph::new(Line::from(spans))
                    };

                    frame.render_widget(widget, row_area);
                }
                TreeNode::Column { col, .. } => {
                    let pk_mark = if col.pk { pk_indicator } else { no_pk };
                    let col_color = if col.pk {
                        theme.schema_pk
                    } else {
                        theme.schema_column
                    };

                    let name_part = format!("    {pk_mark}{}", col.name);
                    let type_part = format!(" : {}", col.col_type);

                    // Check if column type is a known custom type (strip parens for matching).
                    let base_type = col
                        .col_type
                        .find('(')
                        .map_or(col.col_type.as_str(), |i| &col.col_type[..i])
                        .to_lowercase();
                    let type_color = if self.custom_type_names.contains(&base_type) {
                        theme.schema_custom_type
                    } else {
                        theme.schema_type
                    };

                    let cw = content_width as usize;
                    let widget = if is_selected {
                        let total = format!("{name_part}{type_part}");
                        let display = truncate_str(&total, cw);
                        Paragraph::new(display).style(theme.selected_style)
                    } else {
                        let name_display = truncate_str(&name_part, cw);
                        let name_width =
                            unicode_width::UnicodeWidthStr::width(name_display.as_str());
                        let type_display = if name_width < cw {
                            truncate_str(&type_part, cw - name_width)
                        } else {
                            String::new()
                        };
                        let spans = vec![
                            Span::styled(name_display, Style::default().fg(col_color)),
                            Span::styled(type_display, Style::default().fg(type_color)),
                        ];
                        Paragraph::new(Line::from(spans))
                    };

                    frame.render_widget(widget, row_area);
                }
            }
        }

        if show_scrollbar {
            let scrollbar_height = if filter_bar_showing {
                inner.height.saturating_sub(1)
            } else {
                inner.height
            };
            let scrollbar_area = Rect {
                x: inner.x + content_width,
                y: inner.y,
                width: 1,
                height: scrollbar_height,
            };
            let mut scrollbar_state =
                ScrollbarState::new(self.visible.len()).position(self.scroll_offset);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }

        // Render filter bar at the bottom of the inner area.
        if let Some(ref query) = self.filter {
            let filter_y = inner.y + inner.height.saturating_sub(1);
            let filter_area = Rect {
                x: inner.x,
                y: filter_y,
                width: inner.width,
                height: 1,
            };
            let prompt = Span::styled("/ ", Style::default().fg(theme.accent));
            let text = Span::styled(query.as_str(), Style::default().fg(theme.fg));
            let cursor = if self.filter_active {
                Span::styled("\u{2588}", Style::default().fg(theme.accent))
            } else {
                Span::raw("")
            };
            let line = Line::from(vec![prompt, text, cursor]);
            frame.render_widget(Paragraph::new(line), filter_area);
        }
    }
}

/// Truncate a string to at most `max_width` display columns (not bytes or chars).
fn truncate_str(s: &str, max_width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if max_width == 0 {
        return String::new();
    }
    if s.width() <= max_width {
        return s.to_string();
    }
    // Leave room for "\u{2026}" (1 display column)
    let target = max_width.saturating_sub(1);
    let mut current_width = 0;
    let mut truncated = String::new();
    for ch in s.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > target {
            break;
        }
        current_width += ch_width;
        truncated.push(ch);
    }
    truncated.push('\u{2026}');
    truncated
}

/// Format an integer with thousands separators (e.g. `1247` -> `"1,247"`).
fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
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

    fn make_schema_entry_with_tbl(obj_type: &str, name: &str, tbl_name: &str) -> SchemaEntry {
        SchemaEntry {
            obj_type: obj_type.to_string(),
            name: name.to_string(),
            tbl_name: tbl_name.to_string(),
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

    /// Count how many categories were built.
    fn category_count(explorer: &SchemaExplorer) -> usize {
        explorer.categories.len()
    }

    /// Get the category kind and child count for category at index.
    fn category_info(explorer: &SchemaExplorer, idx: usize) -> Option<(CategoryKind, usize)> {
        explorer.categories.get(idx).map(|(header, children)| {
            let kind = match header {
                TreeNode::Category { kind, .. } => *kind,
                _ => panic!("expected Category node"),
            };
            (kind, children.len())
        })
    }

    /// Check if a category is expanded.
    fn is_category_expanded(explorer: &SchemaExplorer, idx: usize) -> bool {
        matches!(
            &explorer.categories[idx].0,
            TreeNode::Category { expanded: true, .. }
        )
    }

    /// Count visible Table nodes with a given name.
    fn count_visible_tables(explorer: &SchemaExplorer, name: &str) -> usize {
        explorer
            .visible
            .iter()
            .filter(|n| matches!(n, TreeNode::Table { name: n, .. } if n == name))
            .count()
    }

    /// Count visible Category nodes.
    fn count_visible_categories(explorer: &SchemaExplorer) -> usize {
        explorer
            .visible
            .iter()
            .filter(|n| matches!(n, TreeNode::Category { .. }))
            .count()
    }

    // --- test_set_schema_creates_categories ---

    #[test]
    fn test_set_schema_creates_categories() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "posts"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
            make_schema_entry("view", "active_users"),
        ];
        explorer.set_schema(&entries);

        // 3 categories: Tables, Views, Indexes (no triggers)
        assert_eq!(category_count(&explorer), 3);
        assert_eq!(category_info(&explorer, 0), Some((CategoryKind::Tables, 2)));
        assert_eq!(category_info(&explorer, 1), Some((CategoryKind::Views, 1)));
        assert_eq!(
            category_info(&explorer, 2),
            Some((CategoryKind::Indexes, 1))
        );

        // Tables and Views expanded, Indexes collapsed
        assert!(is_category_expanded(&explorer, 0));
        assert!(is_category_expanded(&explorer, 1));
        assert!(!is_category_expanded(&explorer, 2));

        // Selection at 0
        assert_eq!(explorer.selected, 0);

        // Visible: Tables header + 2 tables + Views header + 1 view + Indexes header = 6
        assert_eq!(explorer.visible.len(), 6);
    }

    // --- test_toggle_expand_triggers_load_columns ---

    #[test]
    fn test_toggle_expand_triggers_load_columns() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // visible: [Tables (1) header, users]
        // Select users (index 1)
        explorer.move_down();
        assert_eq!(explorer.selected, 1);

        // First expand: columns not loaded -> returns LoadColumns action.
        let action = explorer.toggle_expand();
        assert!(
            matches!(action, Some(Action::LoadColumns(ref name)) if name == "users"),
            "Expected LoadColumns(\"users\"), got {action:?}"
        );

        // Collapse.
        let action = explorer.toggle_expand();
        assert!(action.is_none());

        // Mark columns as loaded (simulate ColumnsLoaded arriving).
        explorer.set_columns("users", vec![make_column("id", "INTEGER", true)]);

        // Expand again: columns already loaded -> no action.
        let action = explorer.toggle_expand();
        assert!(action.is_none());
    }

    // --- test_set_columns_attaches_to_table ---

    #[test]
    fn test_set_columns_attaches_to_table() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("table", "posts"),
        ]);

        // Select "users" (index 1, after category header)
        explorer.move_down();
        explorer.toggle_expand(); // users -> LoadColumns (ignore action)
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("email", "TEXT", false),
            ],
        );

        // Visible should be: Tables header, users (expanded), id col, email col, posts (collapsed)
        assert_eq!(explorer.visible.len(), 5);
        assert!(matches!(
            &explorer.visible[0],
            TreeNode::Category {
                kind: CategoryKind::Tables,
                ..
            }
        ));
        assert!(matches!(&explorer.visible[1], TreeNode::Table { name, .. } if name == "users"));
        assert!(matches!(&explorer.visible[2], TreeNode::Column { col, .. } if col.name == "id"));
        assert!(
            matches!(&explorer.visible[3], TreeNode::Column { col, .. } if col.name == "email")
        );
        assert!(matches!(&explorer.visible[4], TreeNode::Table { name, .. } if name == "posts"));

        // set_columns for a nonexistent table is a no-op.
        explorer.set_columns("nonexistent", vec![make_column("x", "TEXT", false)]);
        assert_eq!(explorer.visible.len(), 5); // unchanged
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

        // visible: [Tables header, a, b, c]
        assert_eq!(explorer.selected, 0);

        explorer.move_down();
        assert_eq!(explorer.selected, 1);

        explorer.move_down();
        assert_eq!(explorer.selected, 2);

        explorer.move_down();
        assert_eq!(explorer.selected, 3);

        // Already at last - move_down is a no-op.
        explorer.move_down();
        assert_eq!(explorer.selected, 3);

        explorer.move_up();
        assert_eq!(explorer.selected, 2);

        explorer.move_up();
        assert_eq!(explorer.selected, 1);

        explorer.move_up();
        assert_eq!(explorer.selected, 0);

        // Already at first - move_up is a no-op.
        explorer.move_up();
        assert_eq!(explorer.selected, 0);
    }

    // --- test_collapse_hides_columns ---

    #[test]
    fn test_collapse_hides_columns() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Select users
        explorer.move_down();

        // Expand and load columns.
        explorer.toggle_expand();
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("name", "TEXT", false),
            ],
        );

        // Expanded: header + users + 2 columns = 4 visible.
        assert_eq!(explorer.visible.len(), 4);

        // Select users again (should be at position 1 after set_columns)
        explorer.selected = 1;

        // Collapse by toggling again (selection is on users table node).
        explorer.toggle_expand();

        // Collapsed: header + the table header only = 2.
        assert_eq!(explorer.visible.len(), 2);
        assert!(matches!(
            &explorer.visible[0],
            TreeNode::Category {
                kind: CategoryKind::Tables,
                ..
            }
        ));
        assert!(matches!(
            &explorer.visible[1],
            TreeNode::Table { name, expanded: false, .. } if name == "users"
        ));
    }

    // --- test_navigate_into_columns ---

    #[test]
    fn test_navigate_into_columns() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Select users
        explorer.move_down();

        explorer.toggle_expand();
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("name", "TEXT", false),
            ],
        );

        // visible: [Tables header, users, id, name]
        assert_eq!(explorer.selected, 1); // on users
        explorer.move_down();
        assert_eq!(explorer.selected, 2); // on id
        explorer.move_down();
        assert_eq!(explorer.selected, 3); // on name
        explorer.move_down(); // no-op (last item)
        assert_eq!(explorer.selected, 3);
    }

    // --- test_set_schema_resets_state ---

    #[test]
    fn test_set_schema_resets_state() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "a")]);
        explorer.selected = 0;

        // Re-load schema - everything resets.
        explorer.set_schema(&[
            make_schema_entry("table", "x"),
            make_schema_entry("table", "y"),
        ]);
        assert_eq!(explorer.selected, 0);
        assert_eq!(category_count(&explorer), 1); // just Tables
        // visible: Tables header + 2 tables = 3
        assert_eq!(explorer.visible.len(), 3);
    }

    // --- test_empty_schema ---

    #[test]
    fn test_empty_schema() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[]);
        assert_eq!(category_count(&explorer), 0);
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
        // "hello " (6 display cols) + "\u{2026}" (1 display col) = 7
        assert_eq!(unicode_width::UnicodeWidthStr::width(result.as_str()), 7);
        assert!(result.ends_with('\u{2026}'));
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

        // Select users
        explorer.move_down();

        // Expand and load columns.
        explorer.toggle_expand();
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("name", "TEXT", false),
            ],
        );
        // visible: [Tables header, users, id, name]
        assert_eq!(explorer.visible.len(), 4);

        // Move to column "id" and toggle_expand -> collapses parent.
        explorer.move_down(); // from users (1) to id (2)
        assert_eq!(explorer.selected, 2);
        let action = explorer.toggle_expand();
        assert!(action.is_none());
        // Parent collapsed, selection moved to parent table.
        assert_eq!(explorer.visible.len(), 2); // header + collapsed users
        assert_eq!(explorer.selected, 1); // on "users"
    }

    // --- Enter on column is no-op ---

    #[test]
    fn test_enter_on_column_is_noop() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Select users
        explorer.move_down();

        explorer.toggle_expand();
        explorer.set_columns("users", vec![make_column("id", "INTEGER", true)]);

        // Move to column
        explorer.move_down();
        assert_eq!(explorer.selected, 2);

        // Enter on column: no-op (doesn't collapse parent)
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(action.is_none());
        // Still expanded, selection unchanged
        assert_eq!(explorer.visible.len(), 3); // header + users + id
        assert_eq!(explorer.selected, 2);
    }

    // --- o key populates editor ---

    #[test]
    fn test_o_key_populates_editor() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Select users (index 1, after category header)
        explorer.move_down();

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

        // Select the table (index 1, after category header)
        explorer.move_down();

        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(
            matches!(action, Some(Action::PopulateEditor(ref sql)) if sql.contains("\"my\"\"table\"")),
            "Expected escaped double quotes, got {action:?}"
        );
    }

    // --- NEW TESTS: Category headers ---

    #[test]
    fn test_category_headers_have_correct_counts() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "posts"),
            make_schema_entry("table", "comments"),
            make_schema_entry("view", "active_users"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
            make_schema_entry_with_tbl("index", "idx_posts_author", "posts"),
            make_schema_entry_with_tbl("trigger", "trg_users_insert", "users"),
        ];
        explorer.set_schema(&entries);

        assert_eq!(category_count(&explorer), 4);

        // Check labels include counts
        assert!(matches!(
            &explorer.categories[0].0,
            TreeNode::Category { label, kind: CategoryKind::Tables, .. }
            if label == "Tables (3)"
        ));
        assert!(matches!(
            &explorer.categories[1].0,
            TreeNode::Category { label, kind: CategoryKind::Views, .. }
            if label == "Views (1)"
        ));
        assert!(matches!(
            &explorer.categories[2].0,
            TreeNode::Category { label, kind: CategoryKind::Indexes, .. }
            if label == "Indexes (2)"
        ));
        assert!(matches!(
            &explorer.categories[3].0,
            TreeNode::Category { label, kind: CategoryKind::Triggers, .. }
            if label == "Triggers (1)"
        ));
    }

    #[test]
    fn test_expanding_collapsing_categories() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "posts"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
        ];
        explorer.set_schema(&entries);

        // Tables expanded, Indexes collapsed
        // Visible: Tables header + users + posts + Indexes header = 4
        assert_eq!(explorer.visible.len(), 4);
        assert_eq!(count_visible_tables(&explorer, "users"), 1);
        assert_eq!(count_visible_tables(&explorer, "posts"), 1);

        // Collapse Tables category (select it, it's at index 0)
        explorer.selected = 0;
        explorer.toggle_expand();

        // Now: Tables header (collapsed) + Indexes header = 2
        assert_eq!(explorer.visible.len(), 2);
        assert_eq!(count_visible_tables(&explorer, "users"), 0);
        assert_eq!(count_visible_tables(&explorer, "posts"), 0);

        // Expand again
        explorer.selected = 0;
        explorer.toggle_expand();

        // Back to: Tables header + users + posts + Indexes header = 4
        assert_eq!(explorer.visible.len(), 4);

        // Expand Indexes category (at index 3)
        explorer.selected = 3;
        explorer.toggle_expand();

        // Now: Tables header + users + posts + Indexes header + idx_users_email = 5
        assert_eq!(explorer.visible.len(), 5);
    }

    #[test]
    fn test_index_and_trigger_nodes_under_categories() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
            make_schema_entry_with_tbl("trigger", "trg_users_insert", "users"),
        ];
        explorer.set_schema(&entries);

        // Initially: Tables expanded, Indexes and Triggers collapsed
        // Visible: Tables header, users, Indexes header, Triggers header = 4
        assert_eq!(explorer.visible.len(), 4);
        assert_eq!(count_visible_categories(&explorer), 3);

        // Expand Indexes (at position 2)
        explorer.selected = 2;
        explorer.toggle_expand();
        // Visible: Tables header, users, Indexes header, idx_users_email, Triggers header = 5
        assert_eq!(explorer.visible.len(), 5);
        assert!(matches!(
            &explorer.visible[3],
            TreeNode::Index { name, table_name }
            if name == "idx_users_email" && table_name == "users"
        ));

        // Expand Triggers (at position 4)
        explorer.selected = 4;
        explorer.toggle_expand();
        // Visible: 5 + trg_users_insert = 6
        assert_eq!(explorer.visible.len(), 6);
        assert!(matches!(
            &explorer.visible[5],
            TreeNode::Trigger { name, table_name }
            if name == "trg_users_insert" && table_name == "users"
        ));
    }

    #[test]
    fn test_o_key_noop_on_category_index_trigger() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
        ];
        explorer.set_schema(&entries);

        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);

        // On category header (index 0)
        explorer.selected = 0;
        let action = explorer.handle_key(key);
        assert!(action.is_none(), "o on category should be no-op");

        // On table (index 1) - should work
        explorer.selected = 1;
        let action = explorer.handle_key(key);
        assert!(
            matches!(action, Some(Action::PopulateEditor(_))),
            "o on table should produce PopulateEditor"
        );

        // Expand Indexes to get the index node visible
        explorer.selected = 2; // Indexes header
        explorer.toggle_expand();
        // Now index node at position 3
        explorer.selected = 3;
        let action = explorer.handle_key(key);
        assert!(action.is_none(), "o on index should be no-op");
    }

    #[test]
    fn test_only_nonempty_categories_are_created() {
        let mut explorer = SchemaExplorer::new();
        // Only tables, no views/indexes/triggers
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("table", "posts"),
        ]);
        assert_eq!(category_count(&explorer), 1);
        assert_eq!(category_info(&explorer, 0), Some((CategoryKind::Tables, 2)));
    }

    #[test]
    fn test_enter_on_index_trigger_is_noop() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
        ];
        explorer.set_schema(&entries);

        // Expand Indexes category
        explorer.selected = 2; // Indexes header
        explorer.toggle_expand();

        // Select the index node
        explorer.selected = 3;

        // Enter on index should be no-op
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(action.is_none(), "Enter on index should be no-op");
    }

    #[test]
    fn test_collapse_to_parent_from_index_via_h_key() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
        ];
        explorer.set_schema(&entries);

        // Expand Indexes category
        explorer.selected = 2;
        explorer.toggle_expand();
        // visible: Tables header, users, Indexes header, idx_users_email = 4
        assert_eq!(explorer.visible.len(), 4);

        // Select index node
        explorer.selected = 3;

        // Press h -> should move selection to Indexes category header
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(action.is_none());
        assert_eq!(explorer.selected, 2); // moved to Indexes header
    }

    #[test]
    fn test_views_category() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("view", "active_users"),
            make_schema_entry("view", "recent_posts"),
        ];
        explorer.set_schema(&entries);

        // 2 categories: Tables and Views, both expanded
        assert_eq!(category_count(&explorer), 2);
        // Visible: Tables header + users + Views header + active_users + recent_posts = 5
        assert_eq!(explorer.visible.len(), 5);

        // Views are represented as Table nodes with obj_type "view"
        assert!(matches!(
            &explorer.visible[3],
            TreeNode::Table { name, obj_type, .. }
            if name == "active_users" && obj_type == "view"
        ));
    }

    #[test]
    fn test_column_uses_table_name_not_index() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Select users
        explorer.move_down();
        explorer.toggle_expand();
        explorer.set_columns("users", vec![make_column("id", "INTEGER", true)]);

        // Column node should reference table by name
        assert!(matches!(
            &explorer.visible[2],
            TreeNode::Column { table_name, col }
            if table_name == "users" && col.name == "id"
        ));
    }

    #[test]
    fn test_h_key_on_view_navigates_to_views_category() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("view", "active_users"),
        ];
        explorer.set_schema(&entries);

        // Visible: Tables header (0), users (1), Views header (2), active_users (3)
        assert_eq!(explorer.visible.len(), 4);

        // Select the view node (index 3)
        explorer.selected = 3;

        // Press h -> should navigate to Views category (index 2), not Tables
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        let action = explorer.handle_key(key);
        assert!(action.is_none());
        assert_eq!(
            explorer.selected, 2,
            "h on a view should navigate to Views category header"
        );
        assert!(matches!(
            &explorer.visible[explorer.selected],
            TreeNode::Category {
                kind: CategoryKind::Views,
                ..
            }
        ));
    }

    #[test]
    fn test_scroll_offset_clamped_after_collapse() {
        let mut explorer = SchemaExplorer::new();

        // Create 30 tables so there are enough items to scroll
        let entries: Vec<SchemaEntry> = (0..30)
            .map(|i| make_schema_entry("table", &format!("table_{i}")))
            .collect();
        explorer.set_schema(&entries);

        // Visible: Tables header + 30 tables = 31 items
        assert_eq!(explorer.visible.len(), 31);

        // Simulate having scrolled down far
        explorer.scroll_offset = 25;
        explorer.selected = 25;

        // Collapse the Tables category (select header at index 0)
        explorer.selected = 0;
        explorer.toggle_expand();

        // After collapse: only category header visible = 1 item
        assert_eq!(explorer.visible.len(), 1);

        // scroll_offset must be clamped to valid range (0..visible.len())
        assert!(
            explorer.scroll_offset < explorer.visible.len(),
            "scroll_offset ({}) should be < visible.len() ({})",
            explorer.scroll_offset,
            explorer.visible.len()
        );
    }

    // --- Filter tests ---

    #[test]
    fn test_filter_narrows_visible() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
            make_schema_entry("table", "products"),
        ];
        explorer.set_schema(&entries);

        // Without filter: Tables header + 3 tables = 4
        assert_eq!(explorer.visible.len(), 4);

        // Apply filter "user"
        explorer.filter = Some("user".to_string());
        explorer.rebuild_visible();

        // Should show: Tables category + users only
        assert_eq!(explorer.visible.len(), 2);
        assert!(matches!(
            &explorer.visible[0],
            TreeNode::Category {
                kind: CategoryKind::Tables,
                ..
            }
        ));
        assert!(matches!(
            &explorer.visible[1],
            TreeNode::Table { name, .. } if name == "users"
        ));
    }

    #[test]
    fn test_filter_case_insensitive() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Filter with uppercase
        explorer.filter = Some("USER".to_string());
        explorer.rebuild_visible();

        // Should still match "users"
        assert_eq!(explorer.visible.len(), 2);
        assert!(matches!(
            &explorer.visible[1],
            TreeNode::Table { name, .. } if name == "users"
        ));
    }

    #[test]
    fn test_filter_clear_restores() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
            make_schema_entry("table", "products"),
        ];
        explorer.set_schema(&entries);

        let full_count = explorer.visible.len();
        assert_eq!(full_count, 4);

        // Apply filter
        explorer.filter = Some("user".to_string());
        explorer.rebuild_visible();
        assert_eq!(explorer.visible.len(), 2);

        // Clear filter
        explorer.filter = None;
        explorer.rebuild_visible();
        assert_eq!(explorer.visible.len(), full_count);
    }

    #[test]
    fn test_filter_empty_shows_all() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
        ];
        explorer.set_schema(&entries);

        let full_count = explorer.visible.len();

        // Set filter to empty string (user just pressed '/' but hasn't typed)
        explorer.filter = Some(String::new());
        explorer.rebuild_visible();

        assert_eq!(explorer.visible.len(), full_count);
    }

    #[test]
    fn test_filter_matches_indexes() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_email", "users"),
            make_schema_entry_with_tbl("index", "idx_name", "users"),
        ];
        explorer.set_schema(&entries);

        // Expand Indexes category so children are visible
        // Find the Indexes category position
        let idx_cat_pos = explorer
            .visible
            .iter()
            .position(|n| {
                matches!(
                    n,
                    TreeNode::Category {
                        kind: CategoryKind::Indexes,
                        ..
                    }
                )
            })
            .unwrap();
        explorer.selected = idx_cat_pos;
        explorer.toggle_expand();

        // Now filter for "idx_email"
        explorer.filter = Some("idx_email".to_string());
        explorer.rebuild_visible();

        // Should show: Tables category (has no matching children? No, "users" doesn't match)
        // Indexes category + idx_email
        // Tables category should be hidden since "users" doesn't match "idx_email"
        assert_eq!(explorer.visible.len(), 2);
        assert!(matches!(
            &explorer.visible[0],
            TreeNode::Category {
                kind: CategoryKind::Indexes,
                ..
            }
        ));
        assert!(matches!(
            &explorer.visible[1],
            TreeNode::Index { name, .. } if name == "idx_email"
        ));
    }

    #[test]
    fn test_filter_hides_categories_with_no_matches() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
            make_schema_entry_with_tbl("index", "idx_email", "users"),
        ];
        explorer.set_schema(&entries);

        // Filter for something that matches nothing
        explorer.filter = Some("zzzzz".to_string());
        explorer.rebuild_visible();

        assert_eq!(explorer.visible.len(), 0);
    }

    #[test]
    fn test_filter_key_activation() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        assert!(!explorer.filter_active);
        assert!(explorer.filter.is_none());

        // Press '/' to activate filter
        let key = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        explorer.handle_key(key);

        assert!(explorer.filter_active);
        assert_eq!(explorer.filter, Some(String::new()));
    }

    #[test]
    fn test_filter_key_typing_and_esc() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
        ];
        explorer.set_schema(&entries);

        // Activate filter
        let key = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        explorer.handle_key(key);

        // Type "u"
        let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        explorer.handle_key(key);
        assert_eq!(explorer.filter, Some("u".to_string()));
        // Should show Tables header + users = 2
        assert_eq!(explorer.visible.len(), 2);

        // Press Esc to clear
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        explorer.handle_key(key);
        assert!(!explorer.filter_active);
        assert!(explorer.filter.is_none());
        // All items restored: Tables header + users + orders = 3
        assert_eq!(explorer.visible.len(), 3);
    }

    #[test]
    fn test_filter_key_enter_accepts() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
        ]);

        // Activate filter and type "user"
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        explorer.handle_key(slash);
        for c in "user".chars() {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            explorer.handle_key(key);
        }

        // Press Enter to accept
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        explorer.handle_key(key);

        // Filter still applied but not active
        assert!(!explorer.filter_active);
        assert_eq!(explorer.filter, Some("user".to_string()));
        // Still filtered: Tables header + users = 2
        assert_eq!(explorer.visible.len(), 2);
    }

    #[test]
    fn test_filter_backspace_clears_on_empty() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Activate filter and type "u"
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        explorer.handle_key(slash);
        let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        explorer.handle_key(key);
        assert_eq!(explorer.filter, Some("u".to_string()));

        // Backspace removes the char; filter bar stays open with empty string
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        explorer.handle_key(key);
        assert_eq!(explorer.filter, Some(String::new()));
        assert!(explorer.filter_active);
    }

    #[test]
    fn test_esc_clears_accepted_filter_before_cycling_focus() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
            make_schema_entry("table", "products"),
        ];
        explorer.set_schema(&entries);

        let full_count = explorer.visible.len(); // 4

        // Activate filter and type "user"
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        explorer.handle_key(slash);
        for c in "user".chars() {
            let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            explorer.handle_key(key);
        }

        // Press Enter to accept filter (filter_active = false, filter = Some("user"))
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        explorer.handle_key(enter);
        assert!(!explorer.filter_active);
        assert_eq!(explorer.filter, Some("user".to_string()));

        // Press Esc -> should clear filter, NOT cycle focus
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let action = explorer.handle_key(esc);
        assert!(
            action.is_none(),
            "First Esc should clear filter, not cycle focus"
        );
        assert!(explorer.filter.is_none());
        assert_eq!(explorer.visible.len(), full_count);

        // Press Esc again -> now should cycle focus
        let action = explorer.handle_key(esc);
        assert!(
            matches!(action, Some(Action::CycleFocus(_))),
            "Second Esc should cycle focus"
        );
    }

    #[test]
    fn test_filter_collapsed_category_with_matches() {
        let mut explorer = SchemaExplorer::new();
        let entries = vec![
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
            make_schema_entry_with_tbl("index", "idx_posts_author", "posts"),
        ];
        explorer.set_schema(&entries);

        // Indexes category starts collapsed
        assert!(!is_category_expanded(&explorer, 1));

        // Apply a filter that matches an index name
        explorer.filter = Some("idx_users".to_string());
        explorer.rebuild_visible();

        // The Indexes category header should still appear even though collapsed,
        // because a child matches the filter.
        assert!(explorer.visible.iter().any(|n| matches!(
            n,
            TreeNode::Category {
                kind: CategoryKind::Indexes,
                ..
            }
        )));
    }

    #[test]
    fn test_filter_down_up_exits_filter() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
        ]);

        // Activate filter and type something
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        explorer.handle_key(slash);
        let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        explorer.handle_key(key);
        assert!(explorer.filter_active);

        let prev_selected = explorer.selected;

        // Press Down
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        explorer.handle_key(down);

        // filter_active should be false and selected should have changed
        assert!(!explorer.filter_active);
        assert_ne!(explorer.selected, prev_selected);
    }

    #[test]
    fn test_filter_backspace_on_empty_stays_open() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Activate filter (empty string)
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        explorer.handle_key(slash);
        assert_eq!(explorer.filter, Some(String::new()));
        assert!(explorer.filter_active);

        // Press Backspace on empty filter
        let bs = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        explorer.handle_key(bs);

        // Filter should still be Some("") and filter_active should still be true
        assert_eq!(explorer.filter, Some(String::new()));
        assert!(explorer.filter_active);
    }

    // --- reveal_and_select tests ---

    #[test]
    fn test_reveal_and_select_table() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
            make_schema_entry("table", "products"),
        ]);

        let found = explorer.reveal_and_select("orders", ObjectKind::Table);
        assert!(found);
        // Selected node should be the "orders" table
        assert!(matches!(
            &explorer.visible[explorer.selected],
            TreeNode::Table { name, .. } if name == "orders"
        ));
    }

    #[test]
    fn test_reveal_and_select_view() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("view", "active_users"),
        ]);

        let found = explorer.reveal_and_select("active_users", ObjectKind::View);
        assert!(found);
        assert!(matches!(
            &explorer.visible[explorer.selected],
            TreeNode::Table { name, obj_type, .. } if name == "active_users" && obj_type == "view"
        ));
    }

    #[test]
    fn test_reveal_and_select_index() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_users_email", "users"),
        ]);

        let found = explorer.reveal_and_select("idx_users_email", ObjectKind::Index);
        assert!(found);
        // Indexes category should now be expanded
        assert!(matches!(
            &explorer.visible[explorer.selected],
            TreeNode::Index { name, .. } if name == "idx_users_email"
        ));
    }

    #[test]
    fn test_reveal_and_select_trigger() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("trigger", "trg_insert", "users"),
        ]);

        let found = explorer.reveal_and_select("trg_insert", ObjectKind::Trigger);
        assert!(found);
        assert!(matches!(
            &explorer.visible[explorer.selected],
            TreeNode::Trigger { name, .. } if name == "trg_insert"
        ));
    }

    #[test]
    fn test_reveal_and_select_column() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        // Load columns
        explorer.set_columns(
            "users",
            vec![
                make_column("id", "INTEGER", true),
                make_column("email", "TEXT", false),
            ],
        );

        let found = explorer.reveal_and_select("email", ObjectKind::Column);
        assert!(found);
        assert!(matches!(
            &explorer.visible[explorer.selected],
            TreeNode::Column { col, table_name, .. }
            if col.name == "email" && table_name == "users"
        ));
    }

    #[test]
    fn test_reveal_and_select_nonexistent() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);

        let found = explorer.reveal_and_select("nonexistent", ObjectKind::Table);
        assert!(!found);
    }

    #[test]
    fn test_reveal_and_select_clears_filter() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry("table", "orders"),
        ]);

        // Apply a filter
        explorer.filter = Some("user".to_string());
        explorer.filter_active = true;
        explorer.rebuild_visible();
        assert_eq!(explorer.visible.len(), 2); // header + users

        // reveal_and_select should clear the filter
        let found = explorer.reveal_and_select("orders", ObjectKind::Table);
        assert!(found);
        assert!(explorer.filter.is_none());
        assert!(!explorer.filter_active);
    }

    #[test]
    fn test_reveal_and_select_expands_collapsed_category() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[
            make_schema_entry("table", "users"),
            make_schema_entry_with_tbl("index", "idx_email", "users"),
        ]);

        // Indexes category starts collapsed
        assert!(!is_category_expanded(&explorer, 1));

        // reveal_and_select should expand it
        let found = explorer.reveal_and_select("idx_email", ObjectKind::Index);
        assert!(found);
        assert!(is_category_expanded(&explorer, 1));
    }

    // --- Custom types tests ---

    fn make_custom_types() -> Vec<CustomTypeInfo> {
        vec![
            CustomTypeInfo {
                name: "email".to_string(),
                parent: "text".to_string(),
                builtin: false,
            },
            CustomTypeInfo {
                name: "positive_int".to_string(),
                parent: "integer".to_string(),
                builtin: false,
            },
            CustomTypeInfo {
                name: "currency".to_string(),
                parent: "real".to_string(),
                builtin: false,
            },
        ]
    }

    #[test]
    fn test_set_custom_types_creates_category() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&make_custom_types());

        // Should have 2 categories: Tables + Custom Types
        assert_eq!(explorer.categories.len(), 2);
        let (header, children) = &explorer.categories[1];
        assert!(matches!(
            header,
            TreeNode::Category {
                kind: CategoryKind::CustomTypes,
                ..
            }
        ));
        assert_eq!(children.len(), 3);
    }

    #[test]
    fn test_custom_types_collapsed_by_default() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&make_custom_types());

        // Custom Types category starts collapsed
        assert!(!is_category_expanded(&explorer, 1));
    }

    #[test]
    fn test_custom_types_visible_when_expanded() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&make_custom_types());

        // Expand the Custom Types category
        if let TreeNode::Category { expanded, .. } = &mut explorer.categories[1].0 {
            *expanded = true;
        }
        explorer.rebuild_visible();

        // Should see: Tables header, users, CustomTypes header, uuid, boolean, date
        let custom_type_nodes: Vec<_> = explorer
            .visible
            .iter()
            .filter(|n| matches!(n, TreeNode::CustomType { .. }))
            .collect();
        assert_eq!(custom_type_nodes.len(), 3);
    }

    #[test]
    fn test_custom_types_empty_creates_no_category() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&[]);

        // Should only have Tables category
        assert_eq!(explorer.categories.len(), 1);
    }

    #[test]
    fn test_custom_types_replaced_on_reload() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&make_custom_types());
        assert_eq!(explorer.categories.len(), 2);

        // Replace with fewer types
        explorer.set_custom_types(&[CustomTypeInfo {
            name: "email".to_string(),
            parent: "text".to_string(),
            builtin: false,
        }]);
        assert_eq!(explorer.categories.len(), 2);
        assert_eq!(explorer.categories[1].1.len(), 1);
    }

    #[test]
    fn test_custom_types_filter_matches() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&make_custom_types());

        // Expand Custom Types and set filter
        if let TreeNode::Category { expanded, .. } = &mut explorer.categories[1].0 {
            *expanded = true;
        }
        explorer.filter = Some("email".to_string());
        explorer.rebuild_visible();

        // Only email should be visible under Custom Types
        let custom_nodes: Vec<_> = explorer
            .visible
            .iter()
            .filter(|n| matches!(n, TreeNode::CustomType { .. }))
            .collect();
        assert_eq!(custom_nodes.len(), 1);
        assert!(matches!(
            custom_nodes[0],
            TreeNode::CustomType { name, .. } if name == "email"
        ));
    }

    #[test]
    fn test_reveal_and_select_custom_type() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&make_custom_types());

        // Category starts collapsed
        assert!(!is_category_expanded(&explorer, 1));

        let found = explorer.reveal_and_select("positive_int", ObjectKind::CustomType);
        assert!(found);
        // Should have expanded the category
        assert!(is_category_expanded(&explorer, 1));
        // Should be selected
        let selected = &explorer.visible[explorer.selected];
        assert!(matches!(
            selected,
            TreeNode::CustomType { name, parent_type }
            if name == "positive_int" && parent_type == "integer"
        ));
    }

    #[test]
    fn test_reveal_and_select_custom_type_not_found() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&make_custom_types());

        let found = explorer.reveal_and_select("nonexistent", ObjectKind::CustomType);
        assert!(!found);
    }

    #[test]
    fn test_custom_type_names_populated_for_column_annotation() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_custom_types(&make_custom_types());

        assert!(explorer.custom_type_names.contains("email"));
        assert!(explorer.custom_type_names.contains("positive_int"));
        assert!(explorer.custom_type_names.contains("currency"));
        // Base types should not be present
        assert!(!explorer.custom_type_names.contains("integer"));
        assert!(!explorer.custom_type_names.contains("text"));
    }

    #[test]
    fn test_custom_type_names_case_insensitive() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_custom_types(&[CustomTypeInfo {
            name: "UUID".to_string(),
            parent: "blob".to_string(),
            builtin: false,
        }]);

        // Stored lowercase for case-insensitive matching
        assert!(explorer.custom_type_names.contains("uuid"));
        assert!(!explorer.custom_type_names.contains("UUID"));
    }

    #[test]
    fn test_custom_type_names_cleared_on_empty() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_custom_types(&make_custom_types());
        assert_eq!(explorer.custom_type_names.len(), 3);

        explorer.set_custom_types(&[]);
        assert!(explorer.custom_type_names.is_empty());
    }

    #[test]
    fn test_builtin_types_annotated_but_not_in_tree() {
        let mut explorer = SchemaExplorer::new();
        explorer.set_schema(&[make_schema_entry("table", "users")]);
        explorer.set_custom_types(&[
            CustomTypeInfo {
                name: "uuid".to_string(),
                parent: "blob".to_string(),
                builtin: true,
            },
            CustomTypeInfo {
                name: "email".to_string(),
                parent: "text".to_string(),
                builtin: false,
            },
        ]);

        // Only user-defined type in tree category
        assert_eq!(explorer.categories.len(), 2);
        assert_eq!(explorer.categories[1].1.len(), 1);
        assert!(matches!(
            &explorer.categories[1].1[0],
            TreeNode::CustomType { name, .. } if name == "email"
        ));

        // Both types available for column annotation
        assert!(explorer.custom_type_names.contains("uuid"));
        assert!(explorer.custom_type_names.contains("email"));
    }
}
