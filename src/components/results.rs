use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Cell, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Table, TableState,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, Direction};
use crate::db::{ColumnDef, QueryResult};
use crate::theme::Theme;

use super::Component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortOrder {
    Ascending,
    Descending,
}

/// Maximum number of rows to scan when calculating column widths.
const WIDTH_SAMPLE_ROWS: usize = 50;
/// Minimum column width in characters.
const MIN_COL_WIDTH: u16 = 4;
/// Maximum column width in characters.
const MAX_COL_WIDTH: u16 = 40;

/// Displays query results in a scrollable, navigable table.
pub(crate) struct ResultsTable {
    columns: Vec<ColumnDef>,
    /// Display strings for each cell. `None` = SQL NULL, `Some(s)` = display text.
    rows: Vec<Vec<Option<String>>>,
    column_widths: Vec<u16>,
    /// Per-column minimum width based on header display width (floor of `MIN_COL_WIDTH`).
    min_widths: Vec<u16>,
    state: TableState,
    /// Currently selected column index.
    selected_col: usize,
    /// First visible column index for horizontal scroll.
    col_offset: usize,
    /// Column currently sorted on, if any.
    sort_col: Option<usize>,
    /// Current sort direction.
    sort_order: SortOrder,
    /// Original row order so we can un-sort.
    original_rows: Vec<Vec<Option<String>>>,
}

impl ResultsTable {
    pub(crate) fn new() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            column_widths: Vec::new(),
            min_widths: Vec::new(),
            state: TableState::default(),
            selected_col: 0,
            col_offset: 0,
            sort_col: None,
            sort_order: SortOrder::Ascending,
            original_rows: Vec::new(),
        }
    }

    /// Populate the table from a `QueryResult`, converting `Value`s to display strings.
    /// Selects the first row automatically.
    pub(crate) fn set_results(&mut self, result: &QueryResult) {
        self.columns.clone_from(&result.columns);
        self.rows = result
            .rows
            .iter()
            .map(|row| row.iter().map(value_to_display).collect())
            .collect();
        self.column_widths = compute_column_widths(&self.columns, &self.rows);
        self.min_widths = self
            .columns
            .iter()
            .map(|col| {
                let header_w = UnicodeWidthStr::width(col.name.as_str()) as u16;
                header_w.max(MIN_COL_WIDTH)
            })
            .collect();
        self.original_rows = self.rows.clone();
        self.sort_col = None;
        self.sort_order = SortOrder::Ascending;
        self.selected_col = 0;
        self.col_offset = 0;
        debug_assert!(
            self.rows.iter().all(|r| r.len() == self.columns.len()),
            "row/column count mismatch"
        );
        // Select first row when there are results
        if self.rows.is_empty() {
            self.state.select(None);
        } else {
            self.state.select(Some(0));
        }
    }

    #[allow(dead_code)]
    pub(crate) fn selected_col(&self) -> usize {
        self.selected_col
    }

    pub(crate) fn selected_row(&self) -> Option<usize> {
        self.state.selected()
    }

    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Reset all state — used when closing a database or clearing results.
    #[allow(dead_code)]
    pub(crate) fn clear(&mut self) {
        self.columns.clear();
        self.rows.clear();
        self.column_widths.clear();
        self.min_widths.clear();
        self.state = TableState::default();
        self.selected_col = 0;
        self.col_offset = 0;
        self.sort_col = None;
        self.sort_order = SortOrder::Ascending;
        self.original_rows.clear();
    }

    fn next_row(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let next = match self.state.selected() {
            Some(i) => (i + 1) % self.rows.len(),
            None => 0,
        };
        self.state.select(Some(next));
    }

    fn prev_row(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let prev = match self.state.selected() {
            Some(0) => self.rows.len() - 1,
            Some(i) => i - 1,
            None => 0,
        };
        self.state.select(Some(prev));
    }

    fn first_row(&mut self) {
        if !self.rows.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn last_row(&mut self) {
        if !self.rows.is_empty() {
            self.state.select(Some(self.rows.len() - 1));
        }
    }

    fn next_col(&mut self) {
        if !self.columns.is_empty() && self.selected_col + 1 < self.columns.len() {
            self.selected_col += 1;
        }
    }

    fn prev_col(&mut self) {
        if !self.columns.is_empty() && self.selected_col > 0 {
            self.selected_col -= 1;
        }
    }

    fn grow_column(&mut self) {
        if let Some(w) = self.column_widths.get_mut(self.selected_col) {
            *w = (*w).saturating_add(1).min(MAX_COL_WIDTH);
        }
    }

    fn shrink_column(&mut self) {
        let min = self
            .min_widths
            .get(self.selected_col)
            .copied()
            .unwrap_or(MIN_COL_WIDTH);
        if let Some(w) = self.column_widths.get_mut(self.selected_col) {
            *w = (*w).saturating_sub(1).max(min);
        }
    }

    fn cycle_sort(&mut self) {
        if self.columns.is_empty() || self.rows.is_empty() {
            return;
        }
        let col = self.selected_col;
        match (self.sort_col, self.sort_order) {
            (Some(c), SortOrder::Ascending) if c == col => {
                self.sort_order = SortOrder::Descending;
                self.apply_sort();
            }
            (Some(c), SortOrder::Descending) if c == col => {
                self.sort_col = None;
                self.rows = self.original_rows.clone();
            }
            _ => {
                self.sort_col = Some(col);
                self.sort_order = SortOrder::Ascending;
                self.apply_sort();
            }
        }
        if !self.rows.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn apply_sort(&mut self) {
        let Some(col) = self.sort_col else {
            return;
        };
        let desc = self.sort_order == SortOrder::Descending;
        self.rows = self.original_rows.clone();
        self.rows.sort_by(|a, b| {
            let va = a.get(col).and_then(|v| v.as_deref());
            let vb = b.get(col).and_then(|v| v.as_deref());
            // NULLs always sort last, regardless of direction
            match (va, vb) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => {
                    let cmp = compare_non_null(a, b);
                    if desc { cmp.reverse() } else { cmp }
                }
            }
        });
    }

    /// Compute the range of column indices visible in the given width, adjusting
    /// `col_offset` so that `selected_col` stays on screen.
    ///
    /// **Note:** This method intentionally mutates scroll state (`col_offset`,
    /// `selected_col` clamping) from within `render`.  This is a standard ratatui
    /// pattern for stateful widgets — the viewport geometry is only known at render
    /// time, so scroll adjustment must happen here.  A terminal resize can shift the
    /// viewport without any user input; this is expected behaviour.
    fn visible_col_range(&mut self, available_width: u16) -> std::ops::Range<usize> {
        // Column spacing: ratatui uses 1-cell gap between columns by default.
        let col_spacing: u16 = 1;

        // Ensure selected_col is in range (defensive).
        if !self.columns.is_empty() {
            self.selected_col = self.selected_col.min(self.columns.len() - 1);
        }

        // Scroll left if selected column is before the viewport.
        if self.selected_col < self.col_offset {
            self.col_offset = self.selected_col;
        }

        // Walk forward from col_offset to find how many columns fit.
        // Termination: each iteration either breaks out of the loop or increments
        // `col_offset` by 1.  `col_offset` is bounded above by `selected_col`
        // (the `selected_col >= end` guard prevents advancing past it), so the
        // loop executes at most `selected_col - initial_col_offset + 1` times.
        loop {
            let mut used: u16 = 0;
            let mut end = self.col_offset;
            for (i, &w) in self.column_widths.iter().enumerate().skip(self.col_offset) {
                let needed = if i == self.col_offset {
                    w
                } else {
                    col_spacing + w
                };
                if used.saturating_add(needed) > available_width && i > self.col_offset {
                    break;
                }
                used = used.saturating_add(needed);
                end = i + 1;
            }
            // If selected_col is past the visible end, scroll right and retry.
            if self.selected_col >= end && end > self.col_offset {
                self.col_offset += 1;
                continue;
            }
            break self.col_offset..end;
        }
    }
}

impl Component for ResultsTable {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            // Row navigation
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                self.next_row();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.prev_row();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.first_row();
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.last_row();
                None
            }

            // Column navigation
            (KeyModifiers::NONE, KeyCode::Char('l') | KeyCode::Right) => {
                self.next_col();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('h') | KeyCode::Left) => {
                self.prev_col();
                None
            }

            // Column resize
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('>')) => {
                self.grow_column();
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('<')) => {
                self.shrink_column();
                None
            }

            // Sorting
            (KeyModifiers::NONE, KeyCode::Char('s')) => {
                self.cycle_sort();
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

        let has_results = !self.rows.is_empty();

        let title = if has_results {
            let selected = self.state.selected().map_or(0, |i| i + 1);
            format!("Results [{}/{}]", selected, self.rows.len())
        } else {
            "Results".to_string()
        };

        let block = Block::bordered()
            .border_style(border_style)
            .title(title)
            .title_style(title_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        if !has_results {
            // Centered "no results" message
            let msg = "No results. Execute a query with F5.";
            let msg_len = msg.len() as u16;
            let x_offset = inner.width.saturating_sub(msg_len) / 2;
            let y_offset = inner.height / 2;
            let msg_area = Rect {
                x: inner.x + x_offset,
                y: inner.y + y_offset,
                width: msg_len.min(inner.width),
                height: 1,
            };
            let hint = Paragraph::new(msg).style(Style::default().fg(theme.border));
            frame.render_widget(hint, msg_area);
            return;
        }

        // Calculate visible rows for the scrollbar (header takes 1 row)
        let visible_rows = inner.height.saturating_sub(1) as usize;
        let show_scrollbar = self.rows.len() > visible_rows;

        // Reserve 1 column on the right for the scrollbar when needed
        let table_area = if show_scrollbar {
            Rect {
                width: inner.width.saturating_sub(1),
                ..inner
            }
        } else {
            inner
        };

        // The highlight symbol "▌ " consumes 2 columns; account for it when
        // deciding how many data columns fit in the viewport.
        let highlight_symbol_width: u16 = 2;
        let col_spacing: u16 = 1; // ratatui default column_spacing
        let available_width = table_area
            .width
            .saturating_sub(highlight_symbol_width + col_spacing);

        let visible_range = self.visible_col_range(available_width);
        let sort_state = self.sort_col.map(|c| (c, self.sort_order));
        let table = build_visible_table(
            &self.columns,
            &self.rows,
            &self.column_widths,
            &visible_range,
            self.selected_col,
            sort_state,
            theme,
        );

        frame.render_stateful_widget(table, table_area, &mut self.state);

        if show_scrollbar {
            let scrollbar_area = Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            // Use viewport offset (not selection index) so the thumb tracks the visible window
            let mut scrollbar_state =
                ScrollbarState::new(self.rows.len()).position(self.state.offset());
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }
    }
}

/// Build a ratatui `Table` for the visible column range, highlighting the selected column header.
fn build_visible_table<'a>(
    columns: &'a [ColumnDef],
    rows: &'a [Vec<Option<String>>],
    column_widths: &[u16],
    visible_range: &std::ops::Range<usize>,
    selected_col: usize,
    sort_state: Option<(usize, SortOrder)>,
    theme: &Theme,
) -> Table<'a> {
    let col_widths: Vec<Constraint> = column_widths[visible_range.clone()]
        .iter()
        .enumerate()
        .map(|(vis_idx, &w)| {
            let abs_col_idx = visible_range.start + vis_idx;
            let is_sorted = sort_state.is_some_and(|(sc, _)| sc == abs_col_idx);
            let width = if is_sorted { w.saturating_add(2) } else { w };
            Constraint::Length(width)
        })
        .collect();

    let selected_header_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);

    let header_cells: Vec<Cell> = columns[visible_range.clone()]
        .iter()
        .enumerate()
        .map(|(vis_idx, col)| {
            let abs_idx = visible_range.start + vis_idx;
            let style = if abs_idx == selected_col {
                selected_header_style
            } else {
                theme.header_style
            };
            let is_sorted = sort_state.is_some_and(|(sc, _)| sc == abs_idx);
            if is_sorted {
                let arrow = match sort_state.unwrap().1 {
                    SortOrder::Ascending => " \u{25B2}",
                    SortOrder::Descending => " \u{25BC}",
                };
                Cell::from(format!("{}{arrow}", col.name)).style(style)
            } else {
                Cell::from(col.name.as_str()).style(style)
            }
        })
        .collect();
    let header = Row::new(header_cells).height(1);

    let data_rows: Vec<Row> = rows
        .iter()
        .map(|row_vals| {
            let cells: Vec<Cell> = row_vals[visible_range.clone()]
                .iter()
                .map(|val| match val {
                    None => Cell::from("NULL").style(theme.null_style),
                    Some(s) => Cell::from(s.as_str()),
                })
                .collect();
            Row::new(cells).height(1)
        })
        .collect();

    Table::new(data_rows, col_widths)
        .header(header)
        .row_highlight_style(theme.selected_style)
        .highlight_symbol("▌ ")
        .highlight_spacing(HighlightSpacing::Always)
}

/// Try to parse a string as a finite number for sorting.
/// Returns `None` for non-numeric strings, NaN, and infinity.
fn parse_number(s: &str) -> Option<f64> {
    let f: f64 = s.parse().ok()?;
    f.is_finite().then_some(f)
}

/// Compare two non-null cell values for sorting using class-aware comparison.
/// Numbers sort before strings; within each class, values are compared naturally.
/// This preserves transitivity across mixed-type columns.
fn compare_non_null(a: &str, b: &str) -> std::cmp::Ordering {
    let na = parse_number(a);
    let nb = parse_number(b);
    match (na, nb) {
        (Some(na), Some(nb)) => na.total_cmp(&nb),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Convert a `turso::Value` to a display string. Returns `None` for SQL NULL.
fn value_to_display(val: &turso::Value) -> Option<String> {
    match val {
        turso::Value::Null => None,
        turso::Value::Integer(n) => Some(n.to_string()),
        turso::Value::Real(f) => Some(f.to_string()),
        turso::Value::Text(s) => Some(s.clone()),
        turso::Value::Blob(b) => Some(format!("[BLOB {} B]", b.len())),
    }
}

/// Auto-size column widths: `max(header_width, longest_value_in_first_50_rows, MIN)`, capped at MAX.
/// Uses unicode display widths for correct handling of multi-byte/wide characters.
fn compute_column_widths(columns: &[ColumnDef], rows: &[Vec<Option<String>>]) -> Vec<u16> {
    columns
        .iter()
        .enumerate()
        .map(|(col_idx, col)| {
            let header_w = col.name.as_str().width().min(MAX_COL_WIDTH as usize) as u16;
            let max_val_w = rows
                .iter()
                .take(WIDTH_SAMPLE_ROWS)
                .filter_map(|row| row.get(col_idx))
                .map(|v| {
                    let w = match v {
                        Some(s) => s.as_str().width(),
                        None => 4, // "NULL" display width
                    };
                    w.min(MAX_COL_WIDTH as usize) as u16
                })
                .max()
                .unwrap_or(0);
            header_w.max(max_val_w).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::db::ColumnDef;

    fn make_column(name: &str) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            type_name: String::new(),
        }
    }

    fn make_result(columns: Vec<ColumnDef>, rows: Vec<Vec<turso::Value>>) -> QueryResult {
        QueryResult {
            columns,
            rows,
            execution_time: Duration::ZERO,
            truncated: false,
        }
    }

    // --- value_to_display tests ---

    #[test]
    fn test_value_to_display_null() {
        assert_eq!(value_to_display(&turso::Value::Null), None);
    }

    #[test]
    fn test_value_to_display_integer() {
        assert_eq!(
            value_to_display(&turso::Value::Integer(42)),
            Some("42".to_string())
        );
        assert_eq!(
            value_to_display(&turso::Value::Integer(-7)),
            Some("-7".to_string())
        );
    }

    #[test]
    fn test_value_to_display_real() {
        assert_eq!(
            value_to_display(&turso::Value::Real(1.5)),
            Some("1.5".to_string())
        );
    }

    #[test]
    fn test_value_to_display_text() {
        assert_eq!(
            value_to_display(&turso::Value::Text("hello".to_string())),
            Some("hello".to_string())
        );
    }

    #[test]
    fn test_value_to_display_blob() {
        assert_eq!(
            value_to_display(&turso::Value::Blob(vec![1, 2, 3])),
            Some("[BLOB 3 B]".to_string())
        );
        assert_eq!(
            value_to_display(&turso::Value::Blob(vec![])),
            Some("[BLOB 0 B]".to_string())
        );
    }

    #[test]
    fn test_text_null_not_styled_as_sql_null() {
        // A TEXT value "NULL" should be Some("NULL"), not None
        let val = turso::Value::Text("NULL".to_string());
        assert_eq!(value_to_display(&val), Some("NULL".to_string()));
    }

    // --- set_results tests ---

    #[test]
    fn test_set_results_selects_first_row() {
        let mut table = ResultsTable::new();
        let result = make_result(
            vec![make_column("id"), make_column("name")],
            vec![
                vec![
                    turso::Value::Integer(1),
                    turso::Value::Text("Alice".to_string()),
                ],
                vec![
                    turso::Value::Integer(2),
                    turso::Value::Text("Bob".to_string()),
                ],
            ],
        );
        table.set_results(&result);

        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.state.selected(), Some(0));
        assert_eq!(
            table.rows[0],
            vec![Some("1".to_string()), Some("Alice".to_string())]
        );
        assert_eq!(
            table.rows[1],
            vec![Some("2".to_string()), Some("Bob".to_string())]
        );
    }

    #[test]
    fn test_set_results_empty_no_selection() {
        let mut table = ResultsTable::new();
        let result = make_result(vec![make_column("id")], vec![]);
        table.set_results(&result);

        assert_eq!(table.rows.len(), 0);
        assert_eq!(table.state.selected(), None);
    }

    #[test]
    fn test_set_results_null_values() {
        let mut table = ResultsTable::new();
        let result = make_result(vec![make_column("val")], vec![vec![turso::Value::Null]]);
        table.set_results(&result);
        assert_eq!(table.rows[0][0], None);
    }

    #[test]
    fn test_set_results_twice_resets_selection() {
        let mut table = ResultsTable::new();
        let result1 = make_result(
            vec![make_column("id")],
            vec![
                vec![turso::Value::Integer(1)],
                vec![turso::Value::Integer(2)],
                vec![turso::Value::Integer(3)],
            ],
        );
        table.set_results(&result1);
        table.last_row();
        assert_eq!(table.state.selected(), Some(2));

        // Second set_results should reset selection to row 0
        let result2 = make_result(
            vec![make_column("val")],
            vec![vec![turso::Value::Text("x".to_string())]],
        );
        table.set_results(&result2);
        assert_eq!(table.state.selected(), Some(0));
        assert_eq!(table.rows.len(), 1);
    }

    // --- navigation tests ---

    #[test]
    fn test_navigation_next_and_prev() {
        let mut table = ResultsTable::new();
        let result = make_result(
            vec![make_column("id")],
            vec![
                vec![turso::Value::Integer(1)],
                vec![turso::Value::Integer(2)],
                vec![turso::Value::Integer(3)],
            ],
        );
        table.set_results(&result);

        // starts at 0
        assert_eq!(table.state.selected(), Some(0));
        table.next_row();
        assert_eq!(table.state.selected(), Some(1));
        table.next_row();
        assert_eq!(table.state.selected(), Some(2));

        // prev back down
        table.prev_row();
        assert_eq!(table.state.selected(), Some(1));
        table.prev_row();
        assert_eq!(table.state.selected(), Some(0));
    }

    #[test]
    fn test_navigation_last_row() {
        let mut table = ResultsTable::new();
        let result = make_result(
            vec![make_column("id")],
            vec![
                vec![turso::Value::Integer(1)],
                vec![turso::Value::Integer(2)],
                vec![turso::Value::Integer(3)],
            ],
        );
        table.set_results(&result);

        table.last_row();
        assert_eq!(table.state.selected(), Some(2));
    }

    #[test]
    fn test_navigation_wrap_forward() {
        let mut table = ResultsTable::new();
        let result = make_result(
            vec![make_column("id")],
            vec![
                vec![turso::Value::Integer(1)],
                vec![turso::Value::Integer(2)],
            ],
        );
        table.set_results(&result);

        table.last_row();
        assert_eq!(table.state.selected(), Some(1));
        // Wrap: next from last goes to first
        table.next_row();
        assert_eq!(table.state.selected(), Some(0));
    }

    #[test]
    fn test_navigation_wrap_backward() {
        let mut table = ResultsTable::new();
        let result = make_result(
            vec![make_column("id")],
            vec![
                vec![turso::Value::Integer(1)],
                vec![turso::Value::Integer(2)],
            ],
        );
        table.set_results(&result);

        // At row 0, going back wraps to last
        table.prev_row();
        assert_eq!(table.state.selected(), Some(1));
    }

    #[test]
    fn test_navigation_empty_table_is_noop() {
        let mut table = ResultsTable::new();
        // All navigation calls on an empty table should not panic
        table.next_row();
        table.prev_row();
        table.first_row();
        table.last_row();
        assert_eq!(table.state.selected(), None);
    }

    #[test]
    fn test_clear_resets_state() {
        let mut table = ResultsTable::new();
        let result = make_result(
            vec![make_column("id")],
            vec![vec![turso::Value::Integer(1)]],
        );
        table.set_results(&result);
        assert_eq!(table.rows.len(), 1);

        table.clear();
        assert_eq!(table.rows.len(), 0);
        assert!(table.columns.is_empty());
        assert!(table.rows.is_empty());
        assert_eq!(table.state.selected(), None);
    }

    // --- column width tests ---

    #[test]
    fn test_column_width_minimum() {
        let cols = vec![make_column("x")]; // header len = 1 < MIN_COL_WIDTH=4
        let rows: Vec<Vec<Option<String>>> = vec![];
        let widths = compute_column_widths(&cols, &rows);
        assert_eq!(widths, vec![MIN_COL_WIDTH]);
    }

    #[test]
    fn test_column_width_header_dominates() {
        let cols = vec![make_column("very_long_header_name")]; // 21 chars
        let rows: Vec<Vec<Option<String>>> = vec![vec![Some("hi".to_string())]];
        let widths = compute_column_widths(&cols, &rows);
        assert_eq!(widths, vec![21]);
    }

    #[test]
    fn test_column_width_capped_at_max() {
        let cols = vec![make_column("col")];
        let long_val = Some("a".repeat(100));
        let rows = vec![vec![long_val]];
        let widths = compute_column_widths(&cols, &rows);
        assert_eq!(widths, vec![MAX_COL_WIDTH]);
    }

    #[test]
    fn test_column_width_null_counts_as_four() {
        let cols = vec![make_column("x")]; // header len = 1
        let rows: Vec<Vec<Option<String>>> = vec![vec![None]]; // NULL = 4 chars
        let widths = compute_column_widths(&cols, &rows);
        assert_eq!(widths, vec![MIN_COL_WIDTH]); // max(1, 4) clamped = 4
    }

    // --- column navigation tests ---

    /// Create a `QueryResult` with `num_cols` columns (named `col_0..col_N`) and
    /// `num_rows` rows of integer values.
    fn make_test_result(num_cols: usize, num_rows: usize) -> QueryResult {
        let columns: Vec<ColumnDef> = (0..num_cols)
            .map(|i| make_column(&format!("col_{i}")))
            .collect();
        let rows: Vec<Vec<turso::Value>> = (0..num_rows)
            .map(|r| {
                (0..num_cols)
                    .map(|c| turso::Value::Integer((r * num_cols + c) as i64))
                    .collect()
            })
            .collect();
        make_result(columns, rows)
    }

    #[test]
    fn test_column_navigation() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 3);
        table.set_results(&result);
        assert_eq!(table.selected_col, 0);
        table.next_col();
        assert_eq!(table.selected_col, 1);
        table.next_col();
        assert_eq!(table.selected_col, 2);
        table.next_col(); // clamp — should not go past last column
        assert_eq!(table.selected_col, 2);
        table.prev_col();
        assert_eq!(table.selected_col, 1);
    }

    #[test]
    fn test_column_navigation_prev_at_zero() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 1);
        table.set_results(&result);
        table.prev_col(); // already at 0, should stay
        assert_eq!(table.selected_col, 0);
    }

    #[test]
    fn test_column_navigation_empty_table() {
        let mut table = ResultsTable::new();
        // No results loaded — navigation should not panic
        table.next_col();
        table.prev_col();
        assert_eq!(table.selected_col, 0);
    }

    #[test]
    fn test_column_reset_on_new_results() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 3);
        table.set_results(&result);
        table.next_col();
        table.next_col();
        assert_eq!(table.selected_col, 2);
        table.set_results(&result);
        assert_eq!(table.selected_col, 0);
        assert_eq!(table.col_offset, 0);
    }

    #[test]
    fn test_column_reset_on_clear() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 3);
        table.set_results(&result);
        table.next_col();
        table.clear();
        assert_eq!(table.selected_col, 0);
        assert_eq!(table.col_offset, 0);
    }

    // --- visible_col_range tests ---

    #[test]
    fn test_visible_col_range_all_fit() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 2);
        table.set_results(&result);

        // Each column is width 5 ("col_0" etc.), spacing 1 between them.
        // Total needed: 5 + 1+5 + 1+5 = 17.  Give plenty of room.
        let range = table.visible_col_range(200);
        assert_eq!(range, 0..3);
    }

    #[test]
    fn test_visible_col_range_scroll_right() {
        let mut table = ResultsTable::new();
        let result = make_test_result(5, 2);
        table.set_results(&result);

        // Navigate to the last column.
        for _ in 0..4 {
            table.next_col();
        }
        assert_eq!(table.selected_col, 4);

        // Give just enough width for ~2 columns (5 + 1 + 5 = 11).
        let range = table.visible_col_range(11);
        assert!(
            range.contains(&4),
            "selected_col 4 should be within range {range:?}"
        );
    }

    #[test]
    fn test_visible_col_range_zero_width() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 2);
        table.set_results(&result);

        // Should not panic and should return a valid (possibly single-element) range.
        let range = table.visible_col_range(0);
        assert!(range.start <= range.end);
    }

    #[test]
    fn test_visible_col_range_single_wide_column() {
        let mut table = ResultsTable::new();
        // Create a column with a very long header so it gets a wide computed width.
        let wide_col = make_column("a]a]a]a]a]a]a]a]a]a]a]a]a]a]a]a]a]a]a]a]");
        let result = make_result(
            vec![wide_col, make_column("b")],
            vec![vec![turso::Value::Integer(1), turso::Value::Integer(2)]],
        );
        table.set_results(&result);

        // The first column's computed width (40 = MAX_COL_WIDTH) exceeds viewport.
        // It should still be included in the visible range.
        let range = table.visible_col_range(10);
        assert!(
            range.contains(&0),
            "wide column 0 should still be included in range {range:?}"
        );
    }

    // --- column resize tests ---

    #[test]
    fn test_column_resize_grow() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 1);
        table.set_results(&result);
        let original_width = table.column_widths[0];
        table.grow_column();
        assert_eq!(table.column_widths[0], original_width + 1);
    }

    #[test]
    fn test_column_resize_shrink() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 1);
        table.set_results(&result);
        let original_width = table.column_widths[0];
        table.grow_column();
        table.shrink_column();
        assert_eq!(table.column_widths[0], original_width);
    }

    #[test]
    fn test_column_resize_clamp_min() {
        let mut table = ResultsTable::new();
        let result = make_test_result(1, 1);
        table.set_results(&result);
        let expected_min = table.min_widths[0]; // "col_0" → 5, which > MIN_COL_WIDTH
        for _ in 0..100 {
            table.shrink_column();
        }
        assert_eq!(table.column_widths[0], expected_min);
    }

    #[test]
    fn test_column_resize_clamp_max() {
        let mut table = ResultsTable::new();
        let result = make_test_result(1, 1);
        table.set_results(&result);
        for _ in 0..100 {
            table.grow_column();
        }
        assert_eq!(table.column_widths[0], MAX_COL_WIDTH);
    }

    #[test]
    fn test_column_resize_empty_table() {
        let mut table = ResultsTable::new();
        table.grow_column(); // should not panic
        table.shrink_column(); // should not panic
    }

    #[test]
    fn test_column_resize_non_zero_col() {
        let mut table = ResultsTable::new();
        let result = make_test_result(3, 1);
        table.set_results(&result);
        let orig_col0 = table.column_widths[0];
        let orig_col1 = table.column_widths[1];
        table.next_col(); // select column 1
        table.grow_column();
        assert_eq!(table.column_widths[0], orig_col0); // col 0 unchanged
        assert_eq!(table.column_widths[1], orig_col1 + 1); // col 1 grew
    }

    // --- sorting tests ---

    /// Helper: create a result with a single column of text values.
    fn make_sortable_result(col_name: &str, values: Vec<Option<&str>>) -> QueryResult {
        let columns = vec![make_column(col_name)];
        let rows: Vec<Vec<turso::Value>> = values
            .into_iter()
            .map(|v| {
                vec![match v {
                    Some(s) => turso::Value::Text(s.to_string()),
                    None => turso::Value::Null,
                }]
            })
            .collect();
        make_result(columns, rows)
    }

    /// Extract column 0 values from the table rows for easy assertion.
    fn col0_values(table: &ResultsTable) -> Vec<Option<String>> {
        table.rows.iter().map(|r| r[0].clone()).collect()
    }

    #[test]
    fn test_sort_ascending() {
        let mut table = ResultsTable::new();
        let result = make_sortable_result("val", vec![Some("3"), Some("1"), Some("2")]);
        table.set_results(&result);

        table.cycle_sort(); // None → Ascending on col 0
        assert_eq!(table.sort_col, Some(0));
        assert_eq!(table.sort_order, SortOrder::Ascending);
        assert_eq!(
            col0_values(&table),
            vec![
                Some("1".to_string()),
                Some("2".to_string()),
                Some("3".to_string()),
            ]
        );
    }

    #[test]
    fn test_sort_descending() {
        let mut table = ResultsTable::new();
        let result = make_sortable_result("val", vec![Some("3"), Some("1"), Some("2")]);
        table.set_results(&result);

        table.cycle_sort(); // Ascending
        table.cycle_sort(); // Descending
        assert_eq!(table.sort_order, SortOrder::Descending);
        assert_eq!(
            col0_values(&table),
            vec![
                Some("3".to_string()),
                Some("2".to_string()),
                Some("1".to_string()),
            ]
        );
    }

    #[test]
    fn test_sort_remove() {
        let mut table = ResultsTable::new();
        let result = make_sortable_result("val", vec![Some("3"), Some("1"), Some("2")]);
        table.set_results(&result);

        table.cycle_sort(); // Ascending
        table.cycle_sort(); // Descending
        table.cycle_sort(); // Back to original order
        assert_eq!(table.sort_col, None);
        assert_eq!(
            col0_values(&table),
            vec![
                Some("3".to_string()),
                Some("1".to_string()),
                Some("2".to_string()),
            ]
        );
    }

    #[test]
    fn test_sort_nulls_last() {
        let mut table = ResultsTable::new();
        let result = make_sortable_result("val", vec![None, Some("2"), Some("1"), None]);
        table.set_results(&result);

        table.cycle_sort(); // Ascending — NULLs should sort last
        assert_eq!(
            col0_values(&table),
            vec![Some("1".to_string()), Some("2".to_string()), None, None,]
        );
    }

    #[test]
    fn test_sort_different_column() {
        let mut table = ResultsTable::new();
        let result = make_result(
            vec![make_column("a"), make_column("b")],
            vec![
                vec![
                    turso::Value::Text("2".to_string()),
                    turso::Value::Text("y".to_string()),
                ],
                vec![
                    turso::Value::Text("1".to_string()),
                    turso::Value::Text("x".to_string()),
                ],
            ],
        );
        table.set_results(&result);

        // Sort col 0 ascending
        table.cycle_sort();
        assert_eq!(table.sort_col, Some(0));
        assert_eq!(table.rows[0][0], Some("1".to_string()));

        // Move to col 1 and sort — should reset to ascending on col 1
        table.next_col();
        table.cycle_sort();
        assert_eq!(table.sort_col, Some(1));
        assert_eq!(table.sort_order, SortOrder::Ascending);
        assert_eq!(table.rows[0][1], Some("x".to_string()));
        assert_eq!(table.rows[1][1], Some("y".to_string()));
    }

    #[test]
    fn test_compare_non_null_numeric() {
        // "2" < "10" numerically (not string comparison where "10" < "2")
        assert_eq!(compare_non_null("2", "10"), std::cmp::Ordering::Less);
        assert_eq!(compare_non_null("10", "2"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_compare_non_null_mixed_types_transitive() {
        // Numbers sort before strings — this preserves transitivity
        assert_eq!(compare_non_null("10", "abc"), std::cmp::Ordering::Less);
        assert_eq!(compare_non_null("2", "abc"), std::cmp::Ordering::Less);
        assert_eq!(compare_non_null("abc", "10"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_compare_non_null_nan_inf_treated_as_text() {
        // NaN and infinity should not parse as numbers
        assert_eq!(parse_number("NaN"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number("-inf"), None);
        assert_eq!(parse_number("infinity"), None);
        // Finite numbers should parse
        assert_eq!(parse_number("42"), Some(42.0));
        assert_eq!(parse_number("-2.5"), Some(-2.5));
    }

    #[test]
    fn test_sort_nulls_last_descending() {
        let mut table = ResultsTable::new();
        let result = make_sortable_result("val", vec![None, Some("2"), Some("1"), None]);
        table.set_results(&result);

        table.cycle_sort(); // Ascending
        table.cycle_sort(); // Descending — NULLs should still sort last
        assert_eq!(
            col0_values(&table),
            vec![Some("2".to_string()), Some("1".to_string()), None, None,]
        );
    }

    #[test]
    fn test_sort_mixed_types_column() {
        let mut table = ResultsTable::new();
        let result =
            make_sortable_result("val", vec![Some("abc"), Some("2"), Some("10"), Some("def")]);
        table.set_results(&result);

        table.cycle_sort(); // Ascending — numbers first, then strings
        assert_eq!(
            col0_values(&table),
            vec![
                Some("2".to_string()),
                Some("10".to_string()),
                Some("abc".to_string()),
                Some("def".to_string()),
            ]
        );
    }
}
