#![allow(
    dead_code,
    reason = "QueryEditor is wired into main.rs in a later task"
)]

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use crate::app::{Action, Direction};
use crate::highlight;
use crate::theme::Theme;

use super::Component;

const MAX_UNDO: usize = 100;
const TAB_SIZE: usize = 4;

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

pub(crate) struct QueryEditor {
    buffer: Vec<String>,
    cursor: (usize, usize), // (row, col)
    scroll_offset: usize,
    undo_stack: Vec<Vec<String>>,
    redo_stack: Vec<Vec<String>>,
    tab_size: usize,
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
            self.clamp_cursor();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.buffer.clone());
            self.buffer = next;
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
            // Execute query: F5 or Ctrl+Enter
            // Note: Ctrl+Enter is indistinguishable from Enter in many terminals
            // (xterm, macOS Terminal). F5 is the reliable binding.
            (_, KeyCode::F(5)) | (KeyModifiers::CONTROL, KeyCode::Enter) => {
                Some(Action::ExecuteQuery(self.contents()))
            }

            // Release focus
            (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::CycleFocus(Direction::Forward)),

            // Undo / redo
            (KeyModifiers::CONTROL, KeyCode::Char('z')) => {
                self.undo();
                None
            }
            (KeyModifiers::CONTROL, KeyCode::Char('y')) => {
                self.redo();
                None
            }

            // Clear buffer
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                self.save_undo();
                self.buffer = vec![String::new()];
                self.cursor = (0, 0);
                self.scroll_offset = 0;
                None
            }

            // Cursor movement
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.move_cursor_up();
                None
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.move_cursor_down();
                None
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.move_cursor_left();
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                self.move_cursor_right();
                None
            }

            // Line start/end
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                self.move_home();
                None
            }
            (KeyModifiers::NONE, KeyCode::End) | (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                self.move_end();
                None
            }

            // Enter → newline
            (KeyModifiers::NONE, KeyCode::Enter) => {
                self.insert_newline();
                None
            }

            // Backspace / Delete
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.backspace();
                None
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                self.delete();
                None
            }

            // Tab → indent, Shift+Tab → dedent
            (KeyModifiers::NONE, KeyCode::Tab) => {
                self.insert_tab();
                None
            }
            (_, KeyCode::BackTab) => {
                self.dedent();
                None
            }

            // Regular character input (no modifier or shift only)
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(ch)) => {
                self.insert_char(ch);
                None
            }

            // All other keys consumed, not passed to global handler
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

                // Render syntax-highlighted line content
                let line_text = &self.buffer[line_idx];
                let highlighted = highlight::highlight_line(line_text, theme);

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
        assert!(matches!(action, Some(Action::ExecuteQuery(ref s)) if s == "SELECT 1"));
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
        // Ctrl+A goes to home
        editor.handle_key(ctrl_press(KeyCode::Char('a')));
        assert_eq!(editor.cursor, (0, 0));
    }
}
