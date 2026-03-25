use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use tursotui_db::{ColumnInfo, SchemaEntry};
use tursotui_sql::quoting::quote_identifier;
use unicode_width::UnicodeWidthStr;

use crate::app::Action;
use crate::theme::Theme;

/// Classification of schema objects for diffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SchemaObjectType {
    Table,
    View,
    Index,
    Trigger,
}

impl SchemaObjectType {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "table" => Some(Self::Table),
            "view" => Some(Self::View),
            "index" => Some(Self::Index),
            "trigger" => Some(Self::Trigger),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Table => "TABLE",
            Self::View => "VIEW",
            Self::Index => "INDEX",
            Self::Trigger => "TRIGGER",
        }
    }
}

/// Status of an object in the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffStatus {
    Added,
    Removed,
    Modified,
    Identical,
}

impl DiffStatus {
    fn icon(self) -> &'static str {
        match self {
            Self::Added => "+",
            Self::Removed => "-",
            Self::Modified => "~",
            Self::Identical => "=",
        }
    }

    fn sort_order(self) -> u8 {
        match self {
            Self::Removed => 0,
            Self::Modified => 1,
            Self::Added => 2,
            Self::Identical => 3,
        }
    }
}

/// Column-level diff for modified tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColumnDiff {
    pub(crate) name: String,
    pub(crate) status: DiffStatus,
    pub(crate) source_type: Option<String>,
    pub(crate) target_type: Option<String>,
}

/// Diff entry for a single schema object.
#[derive(Debug, Clone)]
pub(crate) struct ObjectDiff {
    pub(crate) name: String,
    pub(crate) obj_type: SchemaObjectType,
    pub(crate) status: DiffStatus,
    pub(crate) source_ddl: Option<String>,
    pub(crate) target_ddl: Option<String>,
    pub(crate) column_diffs: Vec<ColumnDiff>,
}

/// State for the schema diff overlay.
pub(crate) struct SchemaDiffState {
    pub(crate) diffs: Vec<ObjectDiff>,
    pub(crate) source_label: String,
    pub(crate) target_label: String,
    pub(crate) selected: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) expanded: HashSet<usize>,
    pub(crate) show_identical: bool,
}

impl SchemaDiffState {
    pub(crate) fn new(diffs: Vec<ObjectDiff>, source_label: String, target_label: String) -> Self {
        Self {
            diffs,
            source_label,
            target_label,
            selected: 0,
            scroll_offset: 0,
            expanded: HashSet::new(),
            show_identical: false,
        }
    }

    /// Return a filtered view of diffs based on `show_identical`.
    fn visible_indices(&self) -> Vec<usize> {
        self.diffs
            .iter()
            .enumerate()
            .filter(|(_, d)| self.show_identical || d.status != DiffStatus::Identical)
            .map(|(i, _)| i)
            .collect()
    }
}

/// Compute a schema diff between two sets of schema entries and column maps.
///
/// Matches objects by (lowercase name, object type). Detects Added, Removed,
/// Modified, and Identical. For Modified tables, produces column-level diffs.
/// Result is sorted: Removed, Modified, Added, Identical.
pub(crate) fn compute_schema_diff(
    source_entries: &[SchemaEntry],
    target_entries: &[SchemaEntry],
    source_columns: &HashMap<String, Vec<ColumnInfo>>,
    target_columns: &HashMap<String, Vec<ColumnInfo>>,
) -> Vec<ObjectDiff> {
    // Build maps: (lowercase_name, obj_type) -> (name, sql)
    let mut source_map: HashMap<(String, SchemaObjectType), (&str, Option<&str>)> = HashMap::new();
    for entry in source_entries {
        if let Some(obj_type) = SchemaObjectType::from_str(&entry.obj_type) {
            source_map.insert(
                (entry.name.to_lowercase(), obj_type),
                (&entry.name, entry.sql.as_deref()),
            );
        }
    }

    let mut target_map: HashMap<(String, SchemaObjectType), (&str, Option<&str>)> = HashMap::new();
    for entry in target_entries {
        if let Some(obj_type) = SchemaObjectType::from_str(&entry.obj_type) {
            target_map.insert(
                (entry.name.to_lowercase(), obj_type),
                (&entry.name, entry.sql.as_deref()),
            );
        }
    }

    let all_keys: HashSet<(String, SchemaObjectType)> = source_map
        .keys()
        .chain(target_map.keys())
        .cloned()
        .collect();

    let mut diffs: Vec<ObjectDiff> = all_keys
        .into_iter()
        .map(|(lower_name, obj_type)| {
            let source = source_map.get(&(lower_name.clone(), obj_type));
            let target = target_map.get(&(lower_name.clone(), obj_type));

            match (source, target) {
                (Some(&(name, src_sql)), None) => ObjectDiff {
                    name: name.to_string(),
                    obj_type,
                    status: DiffStatus::Removed,
                    source_ddl: src_sql.map(String::from),
                    target_ddl: None,
                    column_diffs: Vec::new(),
                },
                (None, Some(&(name, tgt_sql))) => ObjectDiff {
                    name: name.to_string(),
                    obj_type,
                    status: DiffStatus::Added,
                    source_ddl: None,
                    target_ddl: tgt_sql.map(String::from),
                    column_diffs: Vec::new(),
                },
                (Some(&(name, src_sql)), Some(&(_tgt_name, tgt_sql))) => {
                    let ddl_match = normalize_ddl(src_sql) == normalize_ddl(tgt_sql);
                    let col_diffs = if obj_type == SchemaObjectType::Table {
                        compute_column_diffs(&lower_name, source_columns, target_columns)
                    } else {
                        Vec::new()
                    };

                    let status = if ddl_match
                        && col_diffs.iter().all(|c| c.status == DiffStatus::Identical)
                    {
                        DiffStatus::Identical
                    } else {
                        DiffStatus::Modified
                    };

                    ObjectDiff {
                        name: name.to_string(),
                        obj_type,
                        status,
                        source_ddl: src_sql.map(String::from),
                        target_ddl: tgt_sql.map(String::from),
                        column_diffs: col_diffs,
                    }
                }
                (None, None) => unreachable!("key must exist in at least one map"),
            }
        })
        .collect();

    // Sort: Removed, Modified, Added, Identical; then alphabetically within each group
    diffs.sort_by(|a, b| {
        a.status
            .sort_order()
            .cmp(&b.status.sort_order())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    diffs
}

/// Normalize DDL for comparison: trim whitespace, collapse runs of whitespace.
fn normalize_ddl(sql: Option<&str>) -> String {
    match sql {
        None => String::new(),
        Some(s) => s.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

/// Compute column-level diffs for a table.
fn compute_column_diffs(
    table_lower: &str,
    source_columns: &HashMap<String, Vec<ColumnInfo>>,
    target_columns: &HashMap<String, Vec<ColumnInfo>>,
) -> Vec<ColumnDiff> {
    let empty = Vec::new();
    let src_cols = find_columns(table_lower, source_columns).unwrap_or(&empty);
    let tgt_cols = find_columns(table_lower, target_columns).unwrap_or(&empty);

    let src_map: HashMap<String, &ColumnInfo> = src_cols
        .iter()
        .map(|c| (c.name.to_lowercase(), c))
        .collect();
    let tgt_map: HashMap<String, &ColumnInfo> = tgt_cols
        .iter()
        .map(|c| (c.name.to_lowercase(), c))
        .collect();

    let all_col_names: HashSet<String> = src_map.keys().chain(tgt_map.keys()).cloned().collect();

    let mut col_diffs: Vec<ColumnDiff> = all_col_names
        .into_iter()
        .map(|col_lower| {
            let src = src_map.get(&col_lower);
            let tgt = tgt_map.get(&col_lower);
            match (src, tgt) {
                (Some(s), None) => ColumnDiff {
                    name: s.name.clone(),
                    status: DiffStatus::Removed,
                    source_type: Some(s.col_type.clone()),
                    target_type: None,
                },
                (None, Some(t)) => ColumnDiff {
                    name: t.name.clone(),
                    status: DiffStatus::Added,
                    source_type: None,
                    target_type: Some(t.col_type.clone()),
                },
                (Some(s), Some(t)) => {
                    let status = if s.col_type.to_lowercase() == t.col_type.to_lowercase()
                        && s.notnull == t.notnull
                        && s.default_value == t.default_value
                        && s.pk == t.pk
                    {
                        DiffStatus::Identical
                    } else {
                        DiffStatus::Modified
                    };
                    ColumnDiff {
                        name: s.name.clone(),
                        status,
                        source_type: Some(s.col_type.clone()),
                        target_type: Some(t.col_type.clone()),
                    }
                }
                (None, None) => unreachable!(),
            }
        })
        .collect();

    col_diffs.sort_by(|a, b| {
        a.status
            .sort_order()
            .cmp(&b.status.sort_order())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    col_diffs
}

/// Case-insensitive column lookup in a column map.
fn find_columns<'a>(
    table_lower: &str,
    columns: &'a HashMap<String, Vec<ColumnInfo>>,
) -> Option<&'a Vec<ColumnInfo>> {
    columns.get(table_lower).or_else(|| {
        columns
            .iter()
            .find(|(k, _)| k.to_lowercase() == table_lower)
            .map(|(_, v)| v)
    })
}

/// Generate a migration SQL statement for a single diff entry.
pub(crate) fn generate_migration(diff: &ObjectDiff) -> String {
    match diff.status {
        DiffStatus::Identical => String::from("-- No changes needed"),
        DiffStatus::Added => diff.target_ddl.as_deref().map_or_else(
            || {
                format!(
                    "-- No DDL available for added {} {}",
                    diff.obj_type.label(),
                    quote_identifier(&diff.name)
                )
            },
            |ddl| format!("{ddl};"),
        ),
        DiffStatus::Removed => {
            format!(
                "DROP {} IF EXISTS {};",
                diff.obj_type.label(),
                quote_identifier(&diff.name),
            )
        }
        DiffStatus::Modified => generate_modified_migration(diff),
    }
}

/// Generate migration for a modified object.
fn generate_modified_migration(diff: &ObjectDiff) -> String {
    match diff.obj_type {
        SchemaObjectType::Table => generate_table_migration(diff),
        SchemaObjectType::View | SchemaObjectType::Index | SchemaObjectType::Trigger => {
            let drop_stmt = format!(
                "DROP {} IF EXISTS {};",
                diff.obj_type.label(),
                quote_identifier(&diff.name),
            );
            match &diff.target_ddl {
                Some(ddl) => format!("{drop_stmt}\n{ddl};"),
                None => format!(
                    "{drop_stmt}\n-- No target DDL available for {}",
                    quote_identifier(&diff.name)
                ),
            }
        }
    }
}

/// Generate migration for a modified table.
///
/// If only columns were added, emit ALTER TABLE ADD COLUMN statements.
/// If columns were removed or had type changes, emit a 12-step rebuild comment.
fn generate_table_migration(diff: &ObjectDiff) -> String {
    let has_removed = diff
        .column_diffs
        .iter()
        .any(|c| c.status == DiffStatus::Removed);
    let has_type_change = diff
        .column_diffs
        .iter()
        .any(|c| c.status == DiffStatus::Modified);

    if !has_removed && !has_type_change {
        // Only added columns -- use ALTER TABLE ADD COLUMN
        let stmts: Vec<String> = diff
            .column_diffs
            .iter()
            .filter(|c| c.status == DiffStatus::Added)
            .map(|c| {
                let col_type = c.target_type.as_deref().unwrap_or("TEXT");
                format!(
                    "ALTER TABLE {} ADD COLUMN {} {};",
                    quote_identifier(&diff.name),
                    quote_identifier(&c.name),
                    col_type,
                )
            })
            .collect();
        if stmts.is_empty() {
            // DDL changed but columns are identical (e.g., constraint change)
            return format!(
                "-- Table {} DDL changed but column-level diff is identical.\n\
                 -- Manual review required. Consider a 12-step table rebuild.",
                quote_identifier(&diff.name),
            );
        }
        stmts.join("\n")
    } else {
        // Type changes or removed columns require 12-step rebuild
        format!(
            "-- Table {} requires a 12-step rebuild:\n\
             -- 1. CREATE TABLE new_{name}_temp (... new schema ...);\n\
             -- 2. INSERT INTO new_{name}_temp SELECT ... FROM {quoted};\n\
             -- 3. DROP TABLE {quoted};\n\
             -- 4. ALTER TABLE new_{name}_temp RENAME TO {quoted};\n\
             -- 5. Recreate indexes, triggers, and views.\n\
             -- See: https://www.sqlite.org/lang_altertable.html#otheralter",
            quote_identifier(&diff.name),
            name = diff.name,
            quoted = quote_identifier(&diff.name),
        )
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the schema diff overlay.
#[allow(clippy::too_many_lines)]
pub(crate) fn render(frame: &mut Frame, state: &mut SchemaDiffState, theme: &Theme) {
    let full = frame.area();
    let popup_w = full.width * 80 / 100;
    let popup_h = full.height * 80 / 100;
    let x = (full.width.saturating_sub(popup_w)) / 2;
    let y = (full.height.saturating_sub(popup_h)) / 2;
    let popup_area = Rect::new(x, y, popup_w, popup_h);

    frame.render_widget(Clear, popup_area);

    let title = format!(
        "Schema Diff: {} \u{2192} {}",
        state.source_label, state.target_label
    );
    let block = super::overlay_block(&title, theme);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 4 || inner.width < 20 {
        return;
    }

    // Split into summary line, content, and hints bar
    let [summary_area, content_area, hints_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    // Summary line
    let visible = state.visible_indices();
    let added = state
        .diffs
        .iter()
        .filter(|d| d.status == DiffStatus::Added)
        .count();
    let removed = state
        .diffs
        .iter()
        .filter(|d| d.status == DiffStatus::Removed)
        .count();
    let modified = state
        .diffs
        .iter()
        .filter(|d| d.status == DiffStatus::Modified)
        .count();
    let identical = state
        .diffs
        .iter()
        .filter(|d| d.status == DiffStatus::Identical)
        .count();

    let summary = Line::from(vec![
        Span::styled(format!(" +{added} "), Style::default().fg(theme.success)),
        Span::styled(format!("-{removed} "), Style::default().fg(theme.error)),
        Span::styled(format!("~{modified} "), Style::default().fg(theme.warning)),
        Span::styled(format!("={identical}"), Style::default().fg(theme.dim)),
        Span::styled(
            format!("  ({} shown)", visible.len()),
            Style::default().fg(theme.dim),
        ),
    ]);
    frame.render_widget(Paragraph::new(summary), summary_area);

    // Build displayable rows
    let mut rows: Vec<DiffRow> = Vec::new();
    for (vi, &diff_idx) in visible.iter().enumerate() {
        rows.push(DiffRow::Object {
            visible_idx: vi,
            diff_idx,
        });
        if state.expanded.contains(&diff_idx) {
            for col in &state.diffs[diff_idx].column_diffs {
                // Skip identical columns in expanded view to reduce noise
                if col.status == DiffStatus::Identical {
                    continue;
                }
                rows.push(DiffRow::Column {
                    col_name: col.name.clone(),
                    col_status: col.status,
                    source_type: col.source_type.clone(),
                    target_type: col.target_type.clone(),
                });
            }
        }
    }

    // Find selected row position
    let sel_row_pos = rows
        .iter()
        .position(
            |r| matches!(r, DiffRow::Object { visible_idx, .. } if *visible_idx == state.selected),
        )
        .unwrap_or(0);

    let content_height = content_area.height as usize;
    // Adjust scroll to keep selected visible
    if sel_row_pos < state.scroll_offset {
        state.scroll_offset = sel_row_pos;
    } else if sel_row_pos >= state.scroll_offset + content_height {
        state.scroll_offset = sel_row_pos.saturating_sub(content_height - 1);
    }

    // Render visible rows
    let max_width = content_area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for row in rows.iter().skip(state.scroll_offset).take(content_height) {
        let is_selected =
            matches!(row, DiffRow::Object { visible_idx, .. } if *visible_idx == state.selected);
        let line = render_diff_row(
            row,
            &state.diffs,
            &state.expanded,
            is_selected,
            max_width,
            theme,
        );
        lines.push(line);
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, content_area);

    // Scrollbar
    if rows.len() > content_height {
        let scrollbar_area = Rect {
            x: content_area.x + content_area.width.saturating_sub(1),
            y: content_area.y,
            width: 1,
            height: content_area.height,
        };
        let max_scroll = rows.len().saturating_sub(content_height);
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll.saturating_add(1)).position(state.scroll_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    // Hints bar
    let hints = Line::from(vec![
        Span::styled(" j/k", Style::default().fg(theme.accent)),
        Span::styled(" nav ", Style::default().fg(theme.dim)),
        Span::styled("Enter", Style::default().fg(theme.accent)),
        Span::styled(" expand ", Style::default().fg(theme.dim)),
        Span::styled("i", Style::default().fg(theme.accent)),
        Span::styled(" identical ", Style::default().fg(theme.dim)),
        Span::styled("y", Style::default().fg(theme.accent)),
        Span::styled(" copy DDL ", Style::default().fg(theme.dim)),
        Span::styled("m", Style::default().fg(theme.accent)),
        Span::styled(" migration ", Style::default().fg(theme.dim)),
        Span::styled("Esc", Style::default().fg(theme.accent)),
        Span::styled(" close", Style::default().fg(theme.dim)),
    ]);
    frame.render_widget(Paragraph::new(hints), hints_area);
}

/// A row in the rendered diff list.
enum DiffRow {
    Object {
        visible_idx: usize,
        diff_idx: usize,
    },
    Column {
        col_name: String,
        col_status: DiffStatus,
        source_type: Option<String>,
        target_type: Option<String>,
    },
}

fn render_diff_row<'a>(
    row: &DiffRow,
    diffs: &[ObjectDiff],
    expanded: &HashSet<usize>,
    is_selected: bool,
    max_width: usize,
    theme: &Theme,
) -> Line<'a> {
    match row {
        DiffRow::Object { diff_idx, .. } => {
            let diff = &diffs[*diff_idx];
            let status_color = status_color(diff.status, theme);
            let icon = diff.status.icon();
            let expand_indicator = if diff.status == DiffStatus::Modified
                && diff.obj_type == SchemaObjectType::Table
                && !diff.column_diffs.is_empty()
            {
                if expanded.contains(diff_idx) {
                    "\u{25bc} "
                } else {
                    "\u{25b6} "
                }
            } else {
                "  "
            };

            let text = format!(
                " {icon} {expand}{type_label} {name}",
                expand = expand_indicator,
                type_label = diff.obj_type.label(),
                name = diff.name,
            );
            let truncated = truncate_to_width(text, max_width);

            let style = if is_selected {
                theme.selected_style
            } else {
                Style::default().fg(status_color)
            };

            Line::from(Span::styled(truncated, style))
        }
        DiffRow::Column {
            col_name,
            col_status,
            source_type,
            target_type,
            ..
        } => {
            let status_color = status_color(*col_status, theme);
            let icon = col_status.icon();
            let type_info = match col_status {
                DiffStatus::Added => {
                    format!(" ({})", target_type.as_deref().unwrap_or("?"))
                }
                DiffStatus::Removed | DiffStatus::Identical => {
                    format!(" ({})", source_type.as_deref().unwrap_or("?"))
                }
                DiffStatus::Modified => {
                    format!(
                        " ({} \u{2192} {})",
                        source_type.as_deref().unwrap_or("?"),
                        target_type.as_deref().unwrap_or("?"),
                    )
                }
            };

            let text = format!("     {icon} {col_name}{type_info}");
            let truncated = truncate_to_width(text, max_width);

            Line::from(Span::styled(truncated, Style::default().fg(status_color)))
        }
    }
}

/// Truncate a string to fit within `max_width` display columns using unicode widths.
fn truncate_to_width(text: String, max_width: usize) -> String {
    let display_width = UnicodeWidthStr::width(text.as_str());
    if display_width <= max_width {
        return text;
    }
    let mut w = 0;
    let mut end = 0;
    for (i, ch) in text.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width {
            break;
        }
        w += cw;
        end = i + ch.len_utf8();
    }
    text[..end].to_string()
}

fn status_color(status: DiffStatus, theme: &Theme) -> Color {
    match status {
        DiffStatus::Added => theme.success,
        DiffStatus::Removed => theme.error,
        DiffStatus::Modified => theme.warning,
        DiffStatus::Identical => theme.dim,
    }
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Handle key events in the schema diff overlay.
///
/// Returns `Some(Action)` if an action should be dispatched, `None` if handled
/// internally or ignored.
pub(crate) fn handle_key(state: &mut SchemaDiffState, key: KeyEvent) -> Option<Action> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    let visible = state.visible_indices();
    let visible_len = visible.len();

    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Esc) => {
            return Some(Action::CloseSchemaDiff);
        }
        (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
            if visible_len > 0 {
                state.selected = (state.selected + 1).min(visible_len - 1);
            }
        }
        (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
            state.selected = state.selected.saturating_sub(1);
        }
        (KeyModifiers::NONE, KeyCode::Char('g')) => {
            state.selected = 0;
            state.scroll_offset = 0;
        }
        (KeyModifiers::SHIFT | KeyModifiers::NONE, KeyCode::Char('G')) => {
            if visible_len > 0 {
                state.selected = visible_len - 1;
            }
        }
        (KeyModifiers::NONE, KeyCode::Enter) => {
            // Toggle expand/collapse for modified tables
            if let Some(&diff_idx) = visible.get(state.selected) {
                let diff = &state.diffs[diff_idx];
                if diff.status == DiffStatus::Modified
                    && diff.obj_type == SchemaObjectType::Table
                    && !diff.column_diffs.is_empty()
                {
                    if state.expanded.contains(&diff_idx) {
                        state.expanded.remove(&diff_idx);
                    } else {
                        state.expanded.insert(diff_idx);
                    }
                }
            }
        }
        (KeyModifiers::NONE, KeyCode::Char('i')) => {
            state.show_identical = !state.show_identical;
            // Clamp selection after toggle
            let new_visible = state.visible_indices();
            if state.selected >= new_visible.len() {
                state.selected = new_visible.len().saturating_sub(1);
            }
        }
        (KeyModifiers::NONE, KeyCode::Char('y')) => {
            // Copy DDL — return action for clipboard
            if let Some(&diff_idx) = visible.get(state.selected) {
                let diff = &state.diffs[diff_idx];
                let ddl = diff
                    .target_ddl
                    .as_deref()
                    .or(diff.source_ddl.as_deref())
                    .unwrap_or("-- No DDL available");
                return Some(Action::CopyText(ddl.to_string()));
            }
        }
        (KeyModifiers::NONE, KeyCode::Char('m')) => {
            // Copy migration SQL
            if let Some(&diff_idx) = visible.get(state.selected) {
                let migration = generate_migration(&state.diffs[diff_idx]);
                return Some(Action::CopyText(migration));
            }
        }
        _ => {}
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(obj_type: &str, name: &str, sql: Option<&str>) -> SchemaEntry {
        SchemaEntry {
            obj_type: obj_type.to_string(),
            name: name.to_string(),
            tbl_name: name.to_string(),
            sql: sql.map(String::from),
        }
    }

    fn make_column(name: &str, col_type: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            col_type: col_type.to_string(),
            notnull: false,
            default_value: None,
            pk: false,
        }
    }

    fn make_column_pk(name: &str, col_type: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            col_type: col_type.to_string(),
            notnull: true,
            default_value: None,
            pk: true,
        }
    }

    #[test]
    fn diff_detects_added_table() {
        let source = vec![];
        let target = vec![make_entry(
            "table",
            "users",
            Some("CREATE TABLE users (id INTEGER)"),
        )];
        let diffs = compute_schema_diff(&source, &target, &HashMap::new(), &HashMap::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, DiffStatus::Added);
        assert_eq!(diffs[0].name, "users");
        assert_eq!(diffs[0].obj_type, SchemaObjectType::Table);
    }

    #[test]
    fn diff_detects_removed_table() {
        let source = vec![make_entry(
            "table",
            "users",
            Some("CREATE TABLE users (id INTEGER)"),
        )];
        let target = vec![];
        let diffs = compute_schema_diff(&source, &target, &HashMap::new(), &HashMap::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, DiffStatus::Removed);
        assert_eq!(diffs[0].name, "users");
    }

    #[test]
    fn diff_detects_modified_table() {
        let source = vec![make_entry(
            "table",
            "users",
            Some("CREATE TABLE users (id INTEGER)"),
        )];
        let target = vec![make_entry(
            "table",
            "users",
            Some("CREATE TABLE users (id INTEGER, name TEXT)"),
        )];
        let src_cols: HashMap<String, Vec<ColumnInfo>> =
            [("users".into(), vec![make_column("id", "INTEGER")])].into();
        let tgt_cols: HashMap<String, Vec<ColumnInfo>> = [(
            "users".into(),
            vec![make_column("id", "INTEGER"), make_column("name", "TEXT")],
        )]
        .into();
        let diffs = compute_schema_diff(&source, &target, &src_cols, &tgt_cols);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, DiffStatus::Modified);
        assert!(
            diffs[0]
                .column_diffs
                .iter()
                .any(|c| c.name == "name" && c.status == DiffStatus::Added)
        );
    }

    #[test]
    fn diff_identical_schemas() {
        let ddl = "CREATE TABLE users (id INTEGER, name TEXT)";
        let source = vec![make_entry("table", "users", Some(ddl))];
        let target = vec![make_entry("table", "users", Some(ddl))];
        let cols: HashMap<String, Vec<ColumnInfo>> = [(
            "users".into(),
            vec![make_column("id", "INTEGER"), make_column("name", "TEXT")],
        )]
        .into();
        let diffs = compute_schema_diff(&source, &target, &cols, &cols);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, DiffStatus::Identical);
    }

    #[test]
    fn diff_detects_added_column() {
        let source = vec![make_entry("table", "t", Some("CREATE TABLE t (a INT)"))];
        let target = vec![make_entry(
            "table",
            "t",
            Some("CREATE TABLE t (a INT, b TEXT)"),
        )];
        let src_cols: HashMap<String, Vec<ColumnInfo>> =
            [("t".into(), vec![make_column("a", "INT")])].into();
        let tgt_cols: HashMap<String, Vec<ColumnInfo>> = [(
            "t".into(),
            vec![make_column("a", "INT"), make_column("b", "TEXT")],
        )]
        .into();
        let diffs = compute_schema_diff(&source, &target, &src_cols, &tgt_cols);
        assert_eq!(diffs[0].status, DiffStatus::Modified);
        let added_cols: Vec<_> = diffs[0]
            .column_diffs
            .iter()
            .filter(|c| c.status == DiffStatus::Added)
            .collect();
        assert_eq!(added_cols.len(), 1);
        assert_eq!(added_cols[0].name, "b");
    }

    #[test]
    fn diff_detects_removed_column() {
        let source = vec![make_entry(
            "table",
            "t",
            Some("CREATE TABLE t (a INT, b TEXT)"),
        )];
        let target = vec![make_entry("table", "t", Some("CREATE TABLE t (a INT)"))];
        let src_cols: HashMap<String, Vec<ColumnInfo>> = [(
            "t".into(),
            vec![make_column("a", "INT"), make_column("b", "TEXT")],
        )]
        .into();
        let tgt_cols: HashMap<String, Vec<ColumnInfo>> =
            [("t".into(), vec![make_column("a", "INT")])].into();
        let diffs = compute_schema_diff(&source, &target, &src_cols, &tgt_cols);
        assert_eq!(diffs[0].status, DiffStatus::Modified);
        let removed_cols: Vec<_> = diffs[0]
            .column_diffs
            .iter()
            .filter(|c| c.status == DiffStatus::Removed)
            .collect();
        assert_eq!(removed_cols.len(), 1);
        assert_eq!(removed_cols[0].name, "b");
    }

    #[test]
    fn diff_detects_type_change() {
        let source = vec![make_entry("table", "t", Some("CREATE TABLE t (a INT)"))];
        let target = vec![make_entry("table", "t", Some("CREATE TABLE t (a TEXT)"))];
        let src_cols: HashMap<String, Vec<ColumnInfo>> =
            [("t".into(), vec![make_column("a", "INT")])].into();
        let tgt_cols: HashMap<String, Vec<ColumnInfo>> =
            [("t".into(), vec![make_column("a", "TEXT")])].into();
        let diffs = compute_schema_diff(&source, &target, &src_cols, &tgt_cols);
        assert_eq!(diffs[0].status, DiffStatus::Modified);
        let mod_cols: Vec<_> = diffs[0]
            .column_diffs
            .iter()
            .filter(|c| c.status == DiffStatus::Modified)
            .collect();
        assert_eq!(mod_cols.len(), 1);
        assert_eq!(mod_cols[0].source_type.as_deref(), Some("INT"));
        assert_eq!(mod_cols[0].target_type.as_deref(), Some("TEXT"));
    }

    #[test]
    fn migration_for_added_table() {
        let diff = ObjectDiff {
            name: "users".into(),
            obj_type: SchemaObjectType::Table,
            status: DiffStatus::Added,
            source_ddl: None,
            target_ddl: Some("CREATE TABLE users (id INTEGER PRIMARY KEY)".into()),
            column_diffs: vec![],
        };
        let sql = generate_migration(&diff);
        assert!(
            sql.contains("CREATE TABLE users"),
            "should contain CREATE: {sql}"
        );
    }

    #[test]
    fn migration_for_dropped_table() {
        let diff = ObjectDiff {
            name: "old_table".into(),
            obj_type: SchemaObjectType::Table,
            status: DiffStatus::Removed,
            source_ddl: Some("CREATE TABLE old_table (id INTEGER)".into()),
            target_ddl: None,
            column_diffs: vec![],
        };
        let sql = generate_migration(&diff);
        assert!(sql.contains("DROP TABLE"), "should contain DROP: {sql}");
        assert!(
            sql.contains("\"old_table\""),
            "should quote identifier: {sql}"
        );
    }

    #[test]
    fn migration_for_added_column() {
        let diff = ObjectDiff {
            name: "users".into(),
            obj_type: SchemaObjectType::Table,
            status: DiffStatus::Modified,
            source_ddl: Some("CREATE TABLE users (id INTEGER)".into()),
            target_ddl: Some("CREATE TABLE users (id INTEGER, email TEXT)".into()),
            column_diffs: vec![
                ColumnDiff {
                    name: "id".into(),
                    status: DiffStatus::Identical,
                    source_type: Some("INTEGER".into()),
                    target_type: Some("INTEGER".into()),
                },
                ColumnDiff {
                    name: "email".into(),
                    status: DiffStatus::Added,
                    source_type: None,
                    target_type: Some("TEXT".into()),
                },
            ],
        };
        let sql = generate_migration(&diff);
        assert!(
            sql.contains("ALTER TABLE") && sql.contains("ADD COLUMN"),
            "should use ALTER TABLE ADD COLUMN: {sql}"
        );
        assert!(sql.contains("\"email\""), "should quote column: {sql}");
    }

    #[test]
    fn migration_for_type_change_rebuild() {
        let diff = ObjectDiff {
            name: "users".into(),
            obj_type: SchemaObjectType::Table,
            status: DiffStatus::Modified,
            source_ddl: Some("CREATE TABLE users (id INTEGER, age TEXT)".into()),
            target_ddl: Some("CREATE TABLE users (id INTEGER, age INTEGER)".into()),
            column_diffs: vec![
                ColumnDiff {
                    name: "id".into(),
                    status: DiffStatus::Identical,
                    source_type: Some("INTEGER".into()),
                    target_type: Some("INTEGER".into()),
                },
                ColumnDiff {
                    name: "age".into(),
                    status: DiffStatus::Modified,
                    source_type: Some("TEXT".into()),
                    target_type: Some("INTEGER".into()),
                },
            ],
        };
        let sql = generate_migration(&diff);
        assert!(
            sql.contains("12-step rebuild"),
            "type change should suggest rebuild: {sql}"
        );
    }

    #[test]
    fn migration_for_modified_view() {
        let diff = ObjectDiff {
            name: "user_summary".into(),
            obj_type: SchemaObjectType::View,
            status: DiffStatus::Modified,
            source_ddl: Some("CREATE VIEW user_summary AS SELECT id FROM users".into()),
            target_ddl: Some("CREATE VIEW user_summary AS SELECT id, name FROM users".into()),
            column_diffs: vec![],
        };
        let sql = generate_migration(&diff);
        assert!(sql.contains("DROP VIEW"), "should DROP VIEW: {sql}");
        assert!(
            sql.contains("CREATE VIEW user_summary"),
            "should CREATE VIEW: {sql}"
        );
    }

    #[test]
    fn diff_sort_order_removed_first() {
        let source = vec![
            make_entry("table", "alpha", Some("CREATE TABLE alpha (id INT)")),
            make_entry("table", "beta", Some("CREATE TABLE beta (id INT)")),
        ];
        let target = vec![
            make_entry("table", "beta", Some("CREATE TABLE beta (id INT, x TEXT)")),
            make_entry("table", "gamma", Some("CREATE TABLE gamma (id INT)")),
        ];
        let src_cols: HashMap<String, Vec<ColumnInfo>> = [
            ("alpha".into(), vec![make_column("id", "INT")]),
            ("beta".into(), vec![make_column("id", "INT")]),
        ]
        .into();
        let tgt_cols: HashMap<String, Vec<ColumnInfo>> = [
            (
                "beta".into(),
                vec![make_column("id", "INT"), make_column("x", "TEXT")],
            ),
            ("gamma".into(), vec![make_column("id", "INT")]),
        ]
        .into();
        let diffs = compute_schema_diff(&source, &target, &src_cols, &tgt_cols);
        assert_eq!(
            diffs[0].status,
            DiffStatus::Removed,
            "first should be removed"
        );
        assert_eq!(
            diffs[1].status,
            DiffStatus::Modified,
            "second should be modified"
        );
        assert_eq!(diffs[2].status, DiffStatus::Added, "third should be added");
    }

    #[test]
    fn diff_case_insensitive_matching() {
        let source = vec![make_entry(
            "table",
            "Users",
            Some("CREATE TABLE Users (id INT)"),
        )];
        let target = vec![make_entry(
            "table",
            "users",
            Some("CREATE TABLE Users (id INT)"),
        )];
        let cols: HashMap<String, Vec<ColumnInfo>> =
            [("Users".into(), vec![make_column("id", "INT")])].into();
        let tgt_cols: HashMap<String, Vec<ColumnInfo>> =
            [("users".into(), vec![make_column("id", "INT")])].into();
        let diffs = compute_schema_diff(&source, &target, &cols, &tgt_cols);
        assert_eq!(diffs.len(), 1, "should match case-insensitively");
        assert_eq!(diffs[0].status, DiffStatus::Identical);
    }

    #[test]
    fn diff_pk_change_detected_as_modified() {
        let source = vec![make_entry("table", "t", Some("CREATE TABLE t (id INT)"))];
        let target = vec![make_entry(
            "table",
            "t",
            Some("CREATE TABLE t (id INT PRIMARY KEY)"),
        )];
        let src_cols: HashMap<String, Vec<ColumnInfo>> =
            [("t".into(), vec![make_column("id", "INT")])].into();
        let tgt_cols: HashMap<String, Vec<ColumnInfo>> =
            [("t".into(), vec![make_column_pk("id", "INT")])].into();
        let diffs = compute_schema_diff(&source, &target, &src_cols, &tgt_cols);
        assert_eq!(diffs[0].status, DiffStatus::Modified);
        assert!(
            diffs[0]
                .column_diffs
                .iter()
                .any(|c| c.status == DiffStatus::Modified)
        );
    }
}
