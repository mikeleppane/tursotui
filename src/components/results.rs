#![allow(
    dead_code,
    reason = "ResultsTable is wired into main.rs in a later task"
)]

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Cell, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Table, TableState,
};

use crate::app::{Action, Direction};
use crate::db::{ColumnDef, QueryResult};
use crate::theme::Theme;

use super::Component;

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
    state: TableState,
}

impl ResultsTable {
    pub(crate) fn new() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            column_widths: Vec::new(),
            state: TableState::default(),
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
        // Select first row when there are results
        if self.rows.is_empty() {
            self.state.select(None);
        } else {
            self.state.select(Some(0));
        }
    }

    /// Reset all state — used when closing a database or clearing results.
    pub(crate) fn clear(&mut self) {
        self.columns.clear();
        self.rows.clear();
        self.column_widths.clear();
        self.state = TableState::default();
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

        // Build column constraints from widths
        let col_widths: Vec<Constraint> = self
            .column_widths
            .iter()
            .map(|&w| Constraint::Length(w))
            .collect();

        // Build header row
        let header_cells: Vec<Cell> = self
            .columns
            .iter()
            .map(|col| Cell::from(col.name.as_str()).style(theme.header_style))
            .collect();
        let header = Row::new(header_cells).height(1);

        // Build data rows — SQL NULLs (None) get a special style
        let data_rows: Vec<Row> = self
            .rows
            .iter()
            .map(|row_vals| {
                let cells: Vec<Cell> = row_vals
                    .iter()
                    .map(|val| match val {
                        None => Cell::from("NULL").style(theme.null_style),
                        Some(s) => Cell::from(s.as_str()),
                    })
                    .collect();
                Row::new(cells).height(1)
            })
            .collect();

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

        let table = Table::new(data_rows, col_widths)
            .header(header)
            .row_highlight_style(theme.selected_style)
            .highlight_symbol("▌ ")
            .highlight_spacing(HighlightSpacing::Always);

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

/// Auto-size column widths: `max(header_len, longest_value_in_first_50_rows, MIN)`, capped at MAX.
fn compute_column_widths(columns: &[ColumnDef], rows: &[Vec<Option<String>>]) -> Vec<u16> {
    columns
        .iter()
        .enumerate()
        .map(|(col_idx, col)| {
            let header_len = col.name.len().min(MAX_COL_WIDTH as usize) as u16;
            let max_val_len = rows
                .iter()
                .take(WIDTH_SAMPLE_ROWS)
                .filter_map(|row| row.get(col_idx))
                .map(|v| {
                    let len = match v {
                        Some(s) => s.len(),
                        None => 4, // "NULL" display width
                    };
                    len.min(MAX_COL_WIDTH as usize) as u16
                })
                .max()
                .unwrap_or(0);
            header_len
                .max(max_val_len)
                .clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
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
}
