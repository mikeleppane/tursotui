#![allow(
    dead_code,
    reason = "QueryEditor is wired into main.rs in a later task"
)]

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use crate::app::{Action, Direction, ExecutionSource};
use crate::highlight;
use crate::theme::Theme;

use super::Component;

const MAX_UNDO: usize = 100;
const TAB_SIZE: usize = 4;

/// Word-character predicate for SQL identifiers: alphanumeric + underscore.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Convert a char offset to a byte offset within a string.
/// Panics if `char_idx` > number of chars (same contract as `String::insert`).
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map_or(s.len(), |(byte_idx, _)| byte_idx)
}

/// Number of chars in a string (not bytes).
fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Apply selection highlighting to a syntax-highlighted line.
/// Splits spans at selection boundaries and patches the selected region with `sel_style`.
fn apply_selection(
    line: Line<'static>,
    sel_start: usize,
    sel_end: usize,
    sel_style: Style,
) -> Line<'static> {
    let mut result = Vec::new();
    let mut char_pos: usize = 0;
    for span in line.spans {
        let span_chars = span.content.chars().count();
        let span_start = char_pos;
        let span_end = char_pos + span_chars;

        if span_end <= sel_start || span_start >= sel_end {
            // Entirely outside selection
            result.push(span);
        } else if span_start >= sel_start && span_end <= sel_end {
            // Entirely inside selection
            result.push(Span::styled(span.content, span.style.patch(sel_style)));
        } else {
            // Partially overlapping — split
            let chars: Vec<char> = span.content.chars().collect();
            let rel_start = sel_start.saturating_sub(span_start);
            let rel_end = (sel_end - span_start).min(span_chars);
            if rel_start > 0 {
                let before: String = chars[..rel_start].iter().collect();
                result.push(Span::styled(before, span.style));
            }
            let selected: String = chars[rel_start..rel_end].iter().collect();
            result.push(Span::styled(selected, span.style.patch(sel_style)));
            if rel_end < span_chars {
                let after: String = chars[rel_end..].iter().collect();
                result.push(Span::styled(after, span.style));
            }
        }
        char_pos = span_end;
    }
    Line::from(result)
}

#[derive(Debug, Clone, Copy)]
struct Selection {
    anchor: (usize, usize), // (row, col)
}

pub(crate) struct QueryEditor {
    buffer: Vec<String>,
    cursor: (usize, usize), // (row, col)
    scroll_offset: usize,
    undo_stack: Vec<Vec<String>>,
    redo_stack: Vec<Vec<String>>,
    tab_size: usize,
    selection: Option<Selection>,
}

impl QueryEditor {
    pub(crate) fn new() -> Self {
        Self {
            buffer: vec![String::new()],
            cursor: (0, 0),
            scroll_offset: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            tab_size: TAB_SIZE,
            selection: None,
        }
    }

    pub(crate) fn with_tab_size(tab_size: usize) -> Self {
        Self {
            tab_size,
            ..Self::new()
        }
    }

    pub(crate) fn contents(&self) -> String {
        self.buffer.join("\n")
    }

    pub(crate) fn set_contents(&mut self, text: &str) {
        self.save_undo();
        self.selection = None;
        self.buffer = text.split('\n').map(String::from).collect::<Vec<_>>();
        if self.buffer.is_empty() {
            self.buffer.push(String::new());
        }
        self.cursor = (0, 0);
        self.scroll_offset = 0;
    }

    fn save_undo(&mut self) {
        self.undo_stack.push(self.buffer.clone());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.buffer.clone());
            self.buffer = prev;
            self.selection = None;
            self.clamp_cursor();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.buffer.clone());
            self.buffer = next;
            self.selection = None;
            self.clamp_cursor();
        }
    }

    fn clamp_cursor(&mut self) {
        let max_row = self.buffer.len().saturating_sub(1);
        if self.cursor.0 > max_row {
            self.cursor.0 = max_row;
        }
        let max_col = char_len(&self.buffer[self.cursor.0]);
        if self.cursor.1 > max_col {
            self.cursor.1 = max_col;
        }
        // Also clamp scroll_offset to prevent blank flash after undo/redo
        self.scroll_offset = self.scroll_offset.min(self.buffer.len().saturating_sub(1));
    }

    fn insert_char(&mut self, ch: char) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        self.buffer[row].insert(byte_idx, ch);
        self.cursor.1 += 1;
    }

    fn insert_newline(&mut self) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        let remainder = self.buffer[row].split_off(byte_idx);
        self.buffer.insert(row + 1, remainder);
        self.cursor = (row + 1, 0);
    }

    fn backspace(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            self.save_undo();
            let byte_idx = char_to_byte(&self.buffer[row], col - 1);
            self.buffer[row].remove(byte_idx);
            self.cursor.1 -= 1;
        } else if row > 0 {
            self.save_undo();
            let current_line = self.buffer.remove(row);
            let prev_char_len = char_len(&self.buffer[row - 1]);
            self.buffer[row - 1].push_str(&current_line);
            self.cursor = (row - 1, prev_char_len);
        }
    }

    fn delete(&mut self) {
        let (row, col) = self.cursor;
        let line_char_len = char_len(&self.buffer[row]);
        if col < line_char_len {
            self.save_undo();
            let byte_idx = char_to_byte(&self.buffer[row], col);
            self.buffer[row].remove(byte_idx);
        } else if row + 1 < self.buffer.len() {
            self.save_undo();
            let next_line = self.buffer.remove(row + 1);
            self.buffer[row].push_str(&next_line);
        }
    }

    fn insert_tab(&mut self) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        let spaces = " ".repeat(self.tab_size);
        self.buffer[row].insert_str(byte_idx, &spaces);
        self.cursor.1 += self.tab_size;
    }

    /// Remove up to `tab_size` leading spaces from the current line (Shift+Tab dedent).
    fn dedent(&mut self) {
        let row = self.cursor.0;
        let leading_spaces = self.buffer[row].chars().take_while(|c| *c == ' ').count();
        let remove_count = leading_spaces.min(self.tab_size);
        if remove_count > 0 {
            self.save_undo();
            let byte_offset = char_to_byte(&self.buffer[row], remove_count);
            self.buffer[row] = self.buffer[row][byte_offset..].to_string();
            // Adjust cursor: move left by removed amount, but don't go below 0
            self.cursor.1 = self.cursor.1.saturating_sub(remove_count);
        }
    }

    fn move_cursor_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            let max_col = char_len(&self.buffer[self.cursor.0]);
            if self.cursor.1 > max_col {
                self.cursor.1 = max_col;
            }
        }
    }

    fn move_cursor_down(&mut self) {
        if self.cursor.0 + 1 < self.buffer.len() {
            self.cursor.0 += 1;
            let max_col = char_len(&self.buffer[self.cursor.0]);
            if self.cursor.1 > max_col {
                self.cursor.1 = max_col;
            }
        }
    }

    fn move_cursor_left(&mut self) {
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        } else if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = char_len(&self.buffer[self.cursor.0]);
        }
    }

    fn move_cursor_right(&mut self) {
        let (row, col) = self.cursor;
        if col < char_len(&self.buffer[row]) {
            self.cursor.1 += 1;
        } else if row + 1 < self.buffer.len() {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
    }

    fn move_home(&mut self) {
        self.cursor.1 = 0;
    }

    fn move_end(&mut self) {
        let row = self.cursor.0;
        self.cursor.1 = char_len(&self.buffer[row]);
    }

    fn clear_selection(&mut self) {
        self.selection = None;
    }

    fn start_or_extend_selection(&mut self) {
        if self.selection.is_none() {
            self.selection = Some(Selection {
                anchor: self.cursor,
            });
        }
    }

    /// Get ordered selection bounds: (start, end) where start <= end.
    fn selection_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
        let sel = self.selection?;
        let a = sel.anchor;
        let b = self.cursor;
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    /// Get the selected text, if any.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let ((sr, sc), (er, ec)) = self.selection_bounds()?;
        let text = if sr == er {
            let line = &self.buffer[sr];
            let start = char_to_byte(line, sc);
            let end = char_to_byte(line, ec);
            line[start..end].to_string()
        } else {
            let mut result = String::new();
            let first = &self.buffer[sr];
            let start = char_to_byte(first, sc);
            result.push_str(&first[start..]);
            for row in (sr + 1)..er {
                result.push('\n');
                result.push_str(&self.buffer[row]);
            }
            result.push('\n');
            let last = &self.buffer[er];
            let end = char_to_byte(last, ec);
            result.push_str(&last[..end]);
            result
        };
        // Zero-width selection → None so text_to_execute falls through to statement_at_cursor
        if text.is_empty() { None } else { Some(text) }
    }

    /// Delete the selected range and collapse cursor to start of range.
    fn delete_selection(&mut self) -> bool {
        let Some(((sr, sc), (er, ec))) = self.selection_bounds() else {
            return false;
        };
        self.save_undo();
        if sr == er {
            let start = char_to_byte(&self.buffer[sr], sc);
            let end = char_to_byte(&self.buffer[sr], ec);
            self.buffer[sr].replace_range(start..end, "");
        } else {
            let start_byte = char_to_byte(&self.buffer[sr], sc);
            let end_byte = char_to_byte(&self.buffer[er], ec);
            let tail = self.buffer[er][end_byte..].to_string();
            self.buffer[sr].truncate(start_byte);
            self.buffer[sr].push_str(&tail);
            self.buffer.drain((sr + 1)..=er);
        }
        self.cursor = (sr, sc);
        self.clear_selection();
        true
    }

    /// Select the entire buffer contents.
    fn select_all(&mut self) {
        self.selection = Some(Selection { anchor: (0, 0) });
        let last_row = self.buffer.len().saturating_sub(1);
        self.cursor = (last_row, char_len(&self.buffer[last_row]));
    }

    /// Move cursor left by one word boundary.
    fn move_word_left(&mut self) {
        let (mut row, mut col) = self.cursor;
        if col == 0 {
            if row > 0 {
                row -= 1;
                col = char_len(&self.buffer[row]);
            }
        } else {
            let chars: Vec<char> = self.buffer[row].chars().collect();
            // Skip whitespace
            while col > 0 && !is_word_char(chars[col - 1]) {
                col -= 1;
            }
            // Skip word chars
            while col > 0 && is_word_char(chars[col - 1]) {
                col -= 1;
            }
        }
        self.cursor = (row, col);
    }

    /// Move cursor right by one word boundary.
    fn move_word_right(&mut self) {
        let (mut row, mut col) = self.cursor;
        let line_len = char_len(&self.buffer[row]);
        if col >= line_len {
            if row + 1 < self.buffer.len() {
                row += 1;
                col = 0;
            }
        } else {
            let chars: Vec<char> = self.buffer[row].chars().collect();
            // Skip word chars
            while col < chars.len() && is_word_char(chars[col]) {
                col += 1;
            }
            // Skip whitespace/punctuation
            while col < chars.len() && !is_word_char(chars[col]) {
                col += 1;
            }
        }
        self.cursor = (row, col);
    }

    /// Compute the selection column range for a given line.
    /// Returns `(start_col, end_col)` in char units, or `(0, 0)` if no selection on this line.
    fn line_selection_cols(&self, line_idx: usize) -> (usize, usize) {
        let Some(((sr, sc), (er, ec))) = self.selection_bounds() else {
            return (0, 0);
        };
        if line_idx < sr || line_idx > er {
            return (0, 0);
        }
        let start_col = if line_idx == sr { sc } else { 0 };
        let end_col = if line_idx == er {
            ec
        } else {
            char_len(&self.buffer[line_idx])
        };
        (start_col, end_col)
    }

    /// Detect the SQL statement at the cursor position.
    pub(crate) fn statement_at_cursor(&self) -> String {
        let full = self.contents();
        let statements = crate::db::detect_statements(&full);
        if statements.is_empty() {
            return full;
        }

        // Compute cursor byte offset in the joined buffer.
        // Row lengths use .len() (bytes) intentionally — detect_statements operates on bytes.
        let mut cursor_byte = 0;
        for row in 0..self.cursor.0 {
            cursor_byte += self.buffer[row].len() + 1; // +1 for newline
        }
        cursor_byte += char_to_byte(&self.buffer[self.cursor.0], self.cursor.1);

        // Find which statement contains the cursor byte offset.
        // Invariant: detect_statements returns &str slices borrowed from `full`,
        // so pointer subtraction yields valid byte offsets within the same allocation.
        for stmt in &statements {
            let stmt_start = stmt.as_ptr() as usize - full.as_ptr() as usize;
            let stmt_end = stmt_start + stmt.len();
            if cursor_byte >= stmt_start && cursor_byte <= stmt_end {
                return (*stmt).to_string();
            }
        }

        // Fallback: last statement
        statements.last().unwrap().to_string()
    }

    /// Returns (text, source) — selection text if present, otherwise statement at cursor.
    pub(crate) fn text_to_execute(&self) -> (String, ExecutionSource) {
        if let Some(text) = self.selected_text() {
            (text, ExecutionSource::Selection)
        } else {
            (
                self.statement_at_cursor(),
                ExecutionSource::StatementAtCursor,
            )
        }
    }

    fn adjust_scroll(&mut self, visible_height: usize) {
        let row = self.cursor.0;
        if row < self.scroll_offset {
            self.scroll_offset = row;
        } else if row >= self.scroll_offset + visible_height {
            self.scroll_offset = row - visible_height + 1;
        }
    }
}

impl Component for QueryEditor {
    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            // Execute selection or statement at cursor: Ctrl+Shift+Enter
            (m, KeyCode::Enter) if m == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
                let (text, source) = self.text_to_execute();
                Some(Action::ExecuteQuery(text, source))
            }

            // Execute full buffer: F5 or Ctrl+Enter
            (_, KeyCode::F(5)) | (KeyModifiers::CONTROL, KeyCode::Enter) => Some(
                Action::ExecuteQuery(self.contents(), ExecutionSource::FullBuffer),
            ),

            // Release focus
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.clear_selection();
                Some(Action::CycleFocus(Direction::Forward))
            }

            // Undo / redo
            (KeyModifiers::CONTROL, KeyCode::Char('z')) => {
                self.undo();
                None
            }
            (KeyModifiers::CONTROL, KeyCode::Char('y')) => {
                self.redo();
                None
            }

            // Select all: Ctrl+Shift+A
            (m, KeyCode::Char('a' | 'A')) if m == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
                self.select_all();
                None
            }

            // Clear buffer
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                self.save_undo();
                self.selection = None;
                self.buffer = vec![String::new()];
                self.cursor = (0, 0);
                self.scroll_offset = 0;
                None
            }

            // Shift+Arrow: extend selection
            (KeyModifiers::SHIFT, KeyCode::Up) => {
                self.start_or_extend_selection();
                self.move_cursor_up();
                None
            }
            (KeyModifiers::SHIFT, KeyCode::Down) => {
                self.start_or_extend_selection();
                self.move_cursor_down();
                None
            }
            (KeyModifiers::SHIFT, KeyCode::Left) => {
                self.start_or_extend_selection();
                self.move_cursor_left();
                None
            }
            (KeyModifiers::SHIFT, KeyCode::Right) => {
                self.start_or_extend_selection();
                self.move_cursor_right();
                None
            }
            (KeyModifiers::SHIFT, KeyCode::Home) => {
                self.start_or_extend_selection();
                self.move_home();
                None
            }
            (KeyModifiers::SHIFT, KeyCode::End) => {
                self.start_or_extend_selection();
                self.move_end();
                None
            }

            // Ctrl+Shift+Arrow: word selection
            (m, KeyCode::Left) if m == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
                self.start_or_extend_selection();
                self.move_word_left();
                None
            }
            (m, KeyCode::Right) if m == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
                self.start_or_extend_selection();
                self.move_word_right();
                None
            }

            // Ctrl+Arrow: word movement (no selection)
            (KeyModifiers::CONTROL, KeyCode::Left) => {
                self.clear_selection();
                self.move_word_left();
                None
            }
            (KeyModifiers::CONTROL, KeyCode::Right) => {
                self.clear_selection();
                self.move_word_right();
                None
            }

            // Plain cursor movement (clears selection)
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.clear_selection();
                self.move_cursor_up();
                None
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.clear_selection();
                self.move_cursor_down();
                None
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.clear_selection();
                self.move_cursor_left();
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                self.clear_selection();
                self.move_cursor_right();
                None
            }

            // Line start/end (clears selection)
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                self.clear_selection();
                self.move_home();
                None
            }
            (KeyModifiers::NONE, KeyCode::End) | (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                self.clear_selection();
                self.move_end();
                None
            }

            // Enter → replace selection or newline
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.selection.is_some() {
                    self.delete_selection();
                }
                self.insert_newline();
                None
            }

            // Backspace / Delete → delete selection or single char
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                if self.selection.is_some() {
                    self.delete_selection();
                } else {
                    self.backspace();
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                if self.selection.is_some() {
                    self.delete_selection();
                } else {
                    self.delete();
                }
                None
            }

            // Tab → indent, Shift+Tab → dedent
            (KeyModifiers::NONE, KeyCode::Tab) => {
                if self.selection.is_some() {
                    self.delete_selection();
                }
                self.insert_tab();
                None
            }
            (_, KeyCode::BackTab) => {
                self.clear_selection();
                self.dedent();
                None
            }

            // Regular character input (replaces selection if active)
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(ch)) => {
                if self.selection.is_some() {
                    self.delete_selection();
                }
                self.insert_char(ch);
                None
            }

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

        let block = Block::bordered()
            .border_style(border_style)
            .title("Query Editor")
            .title_style(title_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let visible_height = inner.height as usize;
        self.adjust_scroll(visible_height);

        let line_count = self.buffer.len();
        // Number of digits in the largest line number
        let gutter_digits = line_count.to_string().len();
        // gutter = digits + 1 space separator
        let gutter_width = (gutter_digits + 1) as u16;

        if inner.width <= gutter_width {
            return;
        }
        let content_width = inner.width - gutter_width;

        let gutter_style = Style::default()
            .fg(theme.border)
            .add_modifier(Modifier::DIM);

        for (display_idx, line_idx) in
            (self.scroll_offset..self.scroll_offset + visible_height).enumerate()
        {
            let y = inner.y + display_idx as u16;
            if y >= inner.y + inner.height {
                break;
            }

            // Render gutter (line number), only for actual lines
            if line_idx < self.buffer.len() {
                let line_num = line_idx + 1;
                let num_str = format!("{line_num:>gutter_digits$} ");
                let gutter_area = Rect {
                    x: inner.x,
                    y,
                    width: gutter_width,
                    height: 1,
                };
                let gutter_widget = Paragraph::new(num_str).style(gutter_style);
                frame.render_widget(gutter_widget, gutter_area);

                // Render syntax-highlighted line content, with selection overlay
                let line_text = &self.buffer[line_idx];
                let mut highlighted = highlight::highlight_line(line_text, theme);
                let (sel_start, sel_end) = self.line_selection_cols(line_idx);
                if sel_start < sel_end {
                    highlighted =
                        apply_selection(highlighted, sel_start, sel_end, theme.selected_style);
                } else if sel_start == 0
                    && sel_end == 0
                    && line_text.is_empty()
                    && let Some(((sr, _), (er, _))) = self.selection_bounds()
                    && line_idx > sr
                    && line_idx < er
                {
                    // Empty line within a multi-line selection: show a highlighted space
                    highlighted = Line::from(Span::styled(" ", theme.selected_style));
                }

                let content_area = Rect {
                    x: inner.x + gutter_width,
                    y,
                    width: content_width,
                    height: 1,
                };
                let line_widget = Paragraph::new(highlighted);
                frame.render_widget(line_widget, content_area);
            }
        }

        // Set terminal cursor position when focused
        if focused {
            let (row, col) = self.cursor;
            if row >= self.scroll_offset {
                let screen_row = row - self.scroll_offset;
                if screen_row < visible_height {
                    let cursor_x = inner.x + gutter_width + col as u16;
                    let cursor_y = inner.y + screen_row as u16;
                    // Clamp to content area bounds
                    let max_x = inner.x + gutter_width + content_width - 1;
                    let clamped_x = cursor_x.min(max_x);
                    frame.set_cursor_position((clamped_x, cursor_y));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    fn ctrl_press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn test_new_editor_has_empty_buffer() {
        let editor = QueryEditor::new();
        assert_eq!(editor.buffer.len(), 1);
        assert_eq!(editor.buffer[0], "");
        assert_eq!(editor.cursor, (0, 0));
        assert_eq!(editor.contents(), "");
    }

    #[test]
    fn test_insert_char() {
        let mut editor = QueryEditor::new();
        editor.handle_key(press(KeyCode::Char('S')));
        editor.handle_key(press(KeyCode::Char('Q')));
        editor.handle_key(press(KeyCode::Char('L')));
        assert_eq!(editor.contents(), "SQL");
        assert_eq!(editor.cursor, (0, 3));
    }

    #[test]
    fn test_insert_newline() {
        let mut editor = QueryEditor::new();
        editor.handle_key(press(KeyCode::Char('a')));
        editor.handle_key(press(KeyCode::Enter));
        editor.handle_key(press(KeyCode::Char('b')));
        assert_eq!(editor.contents(), "a\nb");
        assert_eq!(editor.cursor, (1, 1));
    }

    #[test]
    fn test_backspace() {
        let mut editor = QueryEditor::new();
        editor.handle_key(press(KeyCode::Char('a')));
        editor.handle_key(press(KeyCode::Char('b')));
        editor.handle_key(press(KeyCode::Backspace));
        assert_eq!(editor.contents(), "a");
        assert_eq!(editor.cursor, (0, 1));
    }

    #[test]
    fn test_backspace_joins_lines() {
        let mut editor = QueryEditor::new();
        editor.handle_key(press(KeyCode::Char('a')));
        editor.handle_key(press(KeyCode::Enter));
        editor.handle_key(press(KeyCode::Char('b')));
        // cursor is at (1,1); go to start of line 1
        editor.handle_key(press(KeyCode::Home));
        editor.handle_key(press(KeyCode::Backspace));
        assert_eq!(editor.contents(), "ab");
        assert_eq!(editor.buffer.len(), 1);
        assert_eq!(editor.cursor, (0, 1));
    }

    #[test]
    fn test_undo_redo() {
        let mut editor = QueryEditor::new();
        editor.handle_key(press(KeyCode::Char('a')));
        editor.handle_key(press(KeyCode::Char('b')));
        // undo removes 'b'
        editor.handle_key(ctrl_press(KeyCode::Char('z')));
        assert_eq!(editor.contents(), "a");
        // undo removes 'a'
        editor.handle_key(ctrl_press(KeyCode::Char('z')));
        assert_eq!(editor.contents(), "");
        // redo re-inserts 'a'
        editor.handle_key(ctrl_press(KeyCode::Char('y')));
        assert_eq!(editor.contents(), "a");
        // redo re-inserts 'b'
        editor.handle_key(ctrl_press(KeyCode::Char('y')));
        assert_eq!(editor.contents(), "ab");
    }

    #[test]
    fn test_set_contents() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT\nFROM t");
        assert_eq!(editor.buffer.len(), 2);
        assert_eq!(editor.buffer[0], "SELECT");
        assert_eq!(editor.buffer[1], "FROM t");
        assert_eq!(editor.cursor, (0, 0));
        // undo should restore empty buffer
        editor.handle_key(ctrl_press(KeyCode::Char('z')));
        assert_eq!(editor.contents(), "");
    }

    #[test]
    fn test_cursor_movement() {
        let mut editor = QueryEditor::new();
        editor.set_contents("ab\ncd");
        // start at (0,0) — move right twice
        editor.handle_key(press(KeyCode::Right));
        editor.handle_key(press(KeyCode::Right));
        assert_eq!(editor.cursor, (0, 2));
        // move right wraps to next line
        editor.handle_key(press(KeyCode::Right));
        assert_eq!(editor.cursor, (1, 0));
        // move left from col 0 of line 1 wraps to end of line 0
        editor.handle_key(press(KeyCode::Left));
        assert_eq!(editor.cursor, (0, 2));
        // move down from line 0
        editor.handle_key(press(KeyCode::Down));
        assert_eq!(editor.cursor.0, 1);
        // move down is clamped (already on last line)
        editor.handle_key(press(KeyCode::Down));
        assert_eq!(editor.cursor.0, 1);
        // move up returns to line 0
        editor.handle_key(press(KeyCode::Up));
        assert_eq!(editor.cursor.0, 0);
        // left at col 0, row 0 is a no-op
        editor.handle_key(press(KeyCode::Home));
        editor.handle_key(press(KeyCode::Left));
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn test_dedent() {
        let mut editor = QueryEditor::new();
        editor.set_contents("    SELECT * FROM t;");
        editor.cursor = (0, 8); // somewhere in the line
        editor.dedent();
        assert_eq!(editor.contents(), "SELECT * FROM t;");
        assert_eq!(editor.cursor.1, 4); // moved left by 4
    }

    #[test]
    fn test_dedent_partial() {
        let mut editor = QueryEditor::new();
        editor.set_contents("  SELECT");
        editor.cursor = (0, 3);
        editor.dedent();
        assert_eq!(editor.contents(), "SELECT");
        assert_eq!(editor.cursor.1, 1); // only 2 spaces removed
    }

    #[test]
    fn test_dedent_no_indent() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT");
        editor.cursor = (0, 3);
        editor.dedent();
        assert_eq!(editor.contents(), "SELECT");
        assert_eq!(editor.cursor.1, 3); // unchanged
    }

    #[test]
    fn test_delete_within_line() {
        let mut editor = QueryEditor::new();
        editor.set_contents("abc");
        editor.cursor = (0, 1);
        editor.delete();
        assert_eq!(editor.contents(), "ac");
    }

    #[test]
    fn test_delete_joins_lines() {
        let mut editor = QueryEditor::new();
        editor.set_contents("ab\ncd");
        editor.cursor = (0, 2); // end of first line
        editor.delete();
        assert_eq!(editor.contents(), "abcd");
    }

    #[test]
    fn test_clear_and_undo() {
        let mut editor = QueryEditor::new();
        editor.set_contents("hello");
        editor.handle_key(ctrl_press(KeyCode::Char('l')));
        assert_eq!(editor.contents(), "");
        assert_eq!(editor.cursor, (0, 0));
        // undo restores
        editor.handle_key(ctrl_press(KeyCode::Char('z')));
        assert_eq!(editor.contents(), "hello");
    }

    #[test]
    fn test_new_edit_clears_redo() {
        let mut editor = QueryEditor::new();
        editor.insert_char('a');
        editor.insert_char('b');
        editor.undo(); // back to "a"
        assert_eq!(editor.contents(), "a");
        // new edit should kill redo stack
        editor.insert_char('c');
        assert_eq!(editor.contents(), "ac");
        editor.redo(); // should be no-op
        assert_eq!(editor.contents(), "ac");
    }

    #[test]
    fn test_f5_returns_execute_action() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1");
        let action = editor.handle_key(press(KeyCode::F(5)));
        assert!(
            matches!(action, Some(Action::ExecuteQuery(ref s, ExecutionSource::FullBuffer)) if s == "SELECT 1")
        );
    }

    #[test]
    fn test_unicode_insert_and_navigate() {
        let mut editor = QueryEditor::new();
        // Insert multi-byte char
        editor.insert_char('é');
        assert_eq!(editor.contents(), "é");
        assert_eq!(editor.cursor, (0, 1));
        editor.insert_char('x');
        assert_eq!(editor.contents(), "éx");
        // Navigate left past the multi-byte char
        editor.move_cursor_left();
        editor.move_cursor_left();
        assert_eq!(editor.cursor, (0, 0));
        // Navigate right and delete
        editor.move_cursor_right();
        assert_eq!(editor.cursor, (0, 1));
        editor.backspace();
        assert_eq!(editor.contents(), "x");
    }

    #[test]
    fn test_home_end() {
        let mut editor = QueryEditor::new();
        editor.set_contents("hello world");
        editor.handle_key(press(KeyCode::End));
        assert_eq!(editor.cursor, (0, 11));
        editor.handle_key(press(KeyCode::Home));
        assert_eq!(editor.cursor, (0, 0));
        // Ctrl+E goes to end
        editor.handle_key(ctrl_press(KeyCode::Char('e')));
        assert_eq!(editor.cursor, (0, 11));
    }

    fn shift_press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn selected_text_single_line() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM users");
        editor.selection = Some(Selection { anchor: (0, 0) });
        editor.cursor = (0, 6);
        assert_eq!(editor.selected_text(), Some("SELECT".to_string()));
    }

    #[test]
    fn selected_text_multi_line() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT *\nFROM users");
        editor.selection = Some(Selection { anchor: (0, 7) });
        editor.cursor = (1, 4);
        assert_eq!(editor.selected_text(), Some("*\nFROM".to_string()));
    }

    #[test]
    fn selected_text_reversed_anchor() {
        let mut editor = QueryEditor::new();
        editor.set_contents("ABCDEF");
        // Cursor before anchor (backward selection)
        editor.selection = Some(Selection { anchor: (0, 4) });
        editor.cursor = (0, 1);
        assert_eq!(editor.selected_text(), Some("BCD".to_string()));
    }

    #[test]
    fn delete_selection_single_line() {
        let mut editor = QueryEditor::new();
        editor.set_contents("ABCDEF");
        editor.selection = Some(Selection { anchor: (0, 1) });
        editor.cursor = (0, 4);
        editor.delete_selection();
        assert_eq!(editor.contents(), "AEF");
        assert_eq!(editor.cursor, (0, 1));
        assert!(editor.selection.is_none());
    }

    #[test]
    fn delete_selection_multi_line() {
        let mut editor = QueryEditor::new();
        editor.set_contents("abc\ndef\nghi");
        editor.selection = Some(Selection { anchor: (0, 1) });
        editor.cursor = (2, 2);
        editor.delete_selection();
        assert_eq!(editor.contents(), "ai");
        assert_eq!(editor.cursor, (0, 1));
    }

    #[test]
    fn set_contents_clears_selection() {
        let mut editor = QueryEditor::new();
        editor.selection = Some(Selection { anchor: (0, 0) });
        editor.set_contents("new content");
        assert!(editor.selection.is_none());
        assert_eq!(editor.cursor, (0, 0));
    }

    #[test]
    fn undo_clears_selection() {
        let mut editor = QueryEditor::new();
        editor.insert_char('a');
        editor.selection = Some(Selection { anchor: (0, 0) });
        editor.undo();
        assert!(editor.selection.is_none());
    }

    #[test]
    fn shift_arrow_creates_selection() {
        let mut editor = QueryEditor::new();
        editor.set_contents("hello");
        editor.handle_key(shift_press(KeyCode::Right));
        editor.handle_key(shift_press(KeyCode::Right));
        assert!(editor.selection.is_some());
        assert_eq!(editor.selected_text(), Some("he".to_string()));
    }

    #[test]
    fn plain_arrow_clears_selection() {
        let mut editor = QueryEditor::new();
        editor.set_contents("hello");
        editor.selection = Some(Selection { anchor: (0, 0) });
        editor.cursor = (0, 3);
        editor.handle_key(press(KeyCode::Right));
        assert!(editor.selection.is_none());
    }

    #[test]
    fn typing_replaces_selection() {
        let mut editor = QueryEditor::new();
        editor.set_contents("hello world");
        editor.selection = Some(Selection { anchor: (0, 0) });
        editor.cursor = (0, 5);
        editor.handle_key(press(KeyCode::Char('X')));
        assert_eq!(editor.contents(), "X world");
        assert!(editor.selection.is_none());
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut editor = QueryEditor::new();
        editor.set_contents("ABCDEF");
        editor.selection = Some(Selection { anchor: (0, 1) });
        editor.cursor = (0, 4);
        editor.handle_key(press(KeyCode::Backspace));
        assert_eq!(editor.contents(), "AEF");
        assert!(editor.selection.is_none());
    }

    #[test]
    fn ctrl_shift_a_selects_all() {
        let mut editor = QueryEditor::new();
        editor.set_contents("line1\nline2");
        // Ctrl+Shift+A for select-all (Ctrl+A is move_home)
        editor.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('A'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyEventKind::Press,
        ));
        assert!(editor.selection.is_some());
        assert_eq!(editor.selected_text(), Some("line1\nline2".to_string()));
    }

    #[test]
    fn move_word_left_basic() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM users");
        editor.cursor = (0, 19); // end
        editor.move_word_left();
        assert_eq!(editor.cursor, (0, 14)); // before "users"
        editor.move_word_left();
        assert_eq!(editor.cursor, (0, 9)); // before "FROM"
    }

    #[test]
    fn move_word_right_basic() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM users");
        editor.cursor = (0, 0);
        editor.move_word_right();
        assert_eq!(editor.cursor, (0, 9)); // start of "FROM" (skips "SELECT * ")
        editor.move_word_right();
        assert_eq!(editor.cursor, (0, 14)); // start of "users" (skips "FROM ")
    }

    #[test]
    fn select_all_then_delete() {
        let mut editor = QueryEditor::new();
        editor.set_contents("hello\nworld");
        editor.select_all();
        editor.handle_key(press(KeyCode::Backspace));
        assert_eq!(editor.contents(), "");
    }

    #[test]
    fn statement_at_cursor_single() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1");
        editor.cursor = (0, 3);
        assert_eq!(editor.statement_at_cursor(), "SELECT 1");
    }

    #[test]
    fn statement_at_cursor_multi() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1;\nSELECT 2");
        editor.cursor = (1, 3); // inside "SELECT 2"
        assert_eq!(editor.statement_at_cursor(), "SELECT 2");
    }

    #[test]
    fn statement_at_cursor_first_of_two() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1;\nSELECT 2");
        editor.cursor = (0, 3); // inside "SELECT 1"
        assert_eq!(editor.statement_at_cursor(), "SELECT 1");
    }

    #[test]
    fn statement_at_cursor_semicolon_in_string() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 'a;b';\nSELECT 2");
        editor.cursor = (0, 5);
        assert_eq!(editor.statement_at_cursor(), "SELECT 'a;b'");
    }

    #[test]
    fn text_to_execute_prefers_selection() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1;\nSELECT 2");
        editor.selection = Some(Selection { anchor: (1, 0) });
        editor.cursor = (1, 8);
        let (text, source) = editor.text_to_execute();
        assert_eq!(text, "SELECT 2");
        assert!(matches!(source, ExecutionSource::Selection));
    }

    #[test]
    fn text_to_execute_falls_back_to_statement() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1;\nSELECT 2");
        editor.cursor = (1, 3);
        let (text, source) = editor.text_to_execute();
        assert_eq!(text, "SELECT 2");
        assert!(matches!(source, ExecutionSource::StatementAtCursor));
    }

    #[test]
    fn selected_text_unicode() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SÉLECT * FROM café");
        // Select "SÉLECT" (6 chars, but É is multi-byte)
        editor.selection = Some(Selection { anchor: (0, 0) });
        editor.cursor = (0, 6);
        assert_eq!(editor.selected_text(), Some("SÉLECT".to_string()));
        // Select "café" (4 chars, é is multi-byte)
        editor.selection = Some(Selection { anchor: (0, 14) });
        editor.cursor = (0, 18);
        assert_eq!(editor.selected_text(), Some("café".to_string()));
    }

    #[test]
    fn selected_text_zero_width_returns_none() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1");
        editor.selection = Some(Selection { anchor: (0, 3) });
        editor.cursor = (0, 3);
        assert_eq!(editor.selected_text(), None);
    }

    #[test]
    fn statement_at_cursor_between_statements() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1;\n\nSELECT 2");
        // Cursor on the blank line between statements — falls through to last statement
        editor.cursor = (1, 0);
        let stmt = editor.statement_at_cursor();
        assert_eq!(stmt, "SELECT 2");
    }

    #[test]
    fn move_word_right_with_underscore() {
        let mut editor = QueryEditor::new();
        editor.set_contents("user_id FROM");
        editor.cursor = (0, 0);
        editor.move_word_right();
        // Should skip entire "user_id" as one word, then skip the space → land at "FROM"
        assert_eq!(editor.cursor.1, 8);
    }

    #[test]
    fn move_word_left_with_underscore() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT user_id");
        editor.cursor = (0, 14); // end of line
        editor.move_word_left();
        // Should jump back over "user_id" as one word
        assert_eq!(editor.cursor.1, 7);
    }
}
