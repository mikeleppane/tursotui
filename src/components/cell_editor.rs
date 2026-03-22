use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::Action;
use crate::theme::Theme;

/// Inline/modal cell editor widget for data editing.
pub(crate) struct CellEditor {
    /// Primary key of the row being edited.
    pub pk: Vec<Option<String>>,
    /// Row index in the results table.
    #[allow(dead_code)] // used for future inline rendering integration
    pub row: usize,
    /// Column index being edited.
    pub col: usize,
    /// Current edit buffer (always a UTF-8 string, even for NULL → empty).
    pub buffer: String,
    /// Cursor byte position within `buffer` (always on a char boundary).
    pub cursor_pos: usize,
    /// `true` → multi-line modal popup; `false` → inline single-line.
    pub modal: bool,
    /// Whether the column has a NOT NULL constraint.
    #[allow(dead_code)] // used for future validation / UI hint
    pub notnull: bool,
}

impl CellEditor {
    /// Create a new `CellEditor`.
    ///
    /// * `initial_value` — `None` means SQL NULL; the buffer starts empty.
    /// * `modal` — `true` for multi-line values (newlines or >80 chars).
    pub(crate) fn new(
        pk: Vec<Option<String>>,
        row: usize,
        col: usize,
        initial_value: Option<&str>,
        notnull: bool,
        modal: bool,
    ) -> Self {
        let buffer = initial_value.unwrap_or("").to_string();
        let cursor_pos = buffer.len();
        Self {
            pk,
            row,
            col,
            buffer,
            cursor_pos,
            modal,
            notnull,
        }
    }

    // ------------------------------------------------------------------
    // Cursor helpers
    // ------------------------------------------------------------------

    /// Move cursor left by one char (stays at 0).
    fn move_left(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        // Walk back to the previous char boundary.
        let mut pos = self.cursor_pos - 1;
        while pos > 0 && !self.buffer.is_char_boundary(pos) {
            pos -= 1;
        }
        self.cursor_pos = pos;
    }

    /// Move cursor right by one char (stays at `buffer.len()`).
    fn move_right(&mut self) {
        if self.cursor_pos >= self.buffer.len() {
            return;
        }
        let mut pos = self.cursor_pos + 1;
        while pos < self.buffer.len() && !self.buffer.is_char_boundary(pos) {
            pos += 1;
        }
        self.cursor_pos = pos;
    }

    /// Delete char before cursor (backspace semantics).
    fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let end = self.cursor_pos;
        let mut start = end - 1;
        while start > 0 && !self.buffer.is_char_boundary(start) {
            start -= 1;
        }
        self.buffer.drain(start..end);
        self.cursor_pos = start;
    }

    /// Delete char at cursor (Delete key semantics).
    fn delete_at_cursor(&mut self) {
        if self.cursor_pos >= self.buffer.len() {
            return;
        }
        let start = self.cursor_pos;
        let mut end = start + 1;
        while end < self.buffer.len() && !self.buffer.is_char_boundary(end) {
            end += 1;
        }
        self.buffer.drain(start..end);
    }

    /// Insert a char at the cursor position, then advance cursor.
    fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    // ------------------------------------------------------------------
    // Modal: line/column cursor navigation
    // ------------------------------------------------------------------

    /// In modal mode: move the logical cursor up one line.
    fn move_up(&mut self) {
        let (line, col_in_line) = self.cursor_to_line_col();
        if line == 0 {
            return;
        }
        self.cursor_pos = self.line_col_to_pos(line - 1, col_in_line);
    }

    /// In modal mode: move the logical cursor down one line.
    fn move_down(&mut self) {
        let (line, col_in_line) = self.cursor_to_line_col();
        let lines: Vec<&str> = self.buffer.split('\n').collect();
        if line + 1 >= lines.len() {
            return;
        }
        self.cursor_pos = self.line_col_to_pos(line + 1, col_in_line);
    }

    /// Convert `cursor_pos` (byte offset) to `(line_index, byte_col_in_line)`.
    fn cursor_to_line_col(&self) -> (usize, usize) {
        let before = &self.buffer[..self.cursor_pos];
        let line = before.chars().filter(|&c| c == '\n').count();
        let col = before
            .rfind('\n')
            .map_or(self.cursor_pos, |i| self.cursor_pos - i - 1);
        (line, col)
    }

    /// Convert `(line_index, byte_col_in_line)` back to a byte offset, clamping to line length.
    fn line_col_to_pos(&self, line: usize, col: usize) -> usize {
        let mut byte_pos = 0usize;
        let mut current_line = 0usize;

        for ch in self.buffer.chars() {
            if current_line == line {
                // We're on the target line; advance `col` bytes (clamped to line end).
                break;
            }
            if ch == '\n' {
                current_line += 1;
            }
            byte_pos += ch.len_utf8();
        }
        // `byte_pos` now points to the start of `line`.
        // Walk forward up to `col` bytes (stop at newline or end).
        let line_start = byte_pos;
        for ch in self.buffer[line_start..].chars() {
            if ch == '\n' {
                break;
            }
            if byte_pos - line_start >= col {
                break;
            }
            byte_pos += ch.len_utf8();
        }
        byte_pos
    }

    // ------------------------------------------------------------------
    // Key handling
    // ------------------------------------------------------------------

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if self.modal {
            self.handle_key_modal(key)
        } else {
            self.handle_key_inline(key)
        }
    }

    fn handle_key_inline(&mut self, key: KeyEvent) -> Option<Action> {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.move_left();
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                self.move_right();
                None
            }
            (KeyModifiers::NONE, KeyCode::Home) => {
                self.cursor_pos = 0;
                None
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                self.cursor_pos = self.buffer.len();
                None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.backspace();
                None
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                self.delete_at_cursor();
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                self.insert_char(c);
                None
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                Some(Action::ConfirmCellEdit(Some(self.buffer.clone())))
            }
            (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::CancelCellEdit),
            (KeyModifiers::CONTROL, KeyCode::Char('n')) => Some(Action::ConfirmCellEdit(None)),
            _ => None,
        }
    }

    fn handle_key_modal(&mut self, key: KeyEvent) -> Option<Action> {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.move_left();
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                self.move_right();
                None
            }
            (KeyModifiers::NONE, KeyCode::Home) => {
                self.cursor_pos = 0;
                None
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                self.cursor_pos = self.buffer.len();
                None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.backspace();
                None
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                self.delete_at_cursor();
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                self.insert_char(c);
                None
            }
            // Modal Enter inserts a newline
            (KeyModifiers::NONE, KeyCode::Enter) => {
                self.insert_char('\n');
                None
            }
            // Ctrl+Enter or F10 confirms
            (KeyModifiers::CONTROL, KeyCode::Enter) | (KeyModifiers::NONE, KeyCode::F(10)) => {
                Some(Action::ConfirmCellEdit(Some(self.buffer.clone())))
            }
            (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::CancelCellEdit),
            (KeyModifiers::CONTROL, KeyCode::Char('n')) => Some(Action::ConfirmCellEdit(None)),
            // Up/Down line navigation in modal mode
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.move_up();
                None
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.move_down();
                None
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    /// Render in inline mode — draws the buffer into `area`, placing the
    /// terminal cursor at the correct position.
    #[allow(dead_code)] // inline rendering integration requires ResultsTable changes (later task)
    pub(crate) fn render_inline(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let style = theme.edit_cell_active;
        let display = self.buffer.as_str();
        let para = Paragraph::new(display).style(style);
        frame.render_widget(para, area);

        // Place the terminal cursor
        let cursor_col = UnicodeWidthStr::width(&self.buffer[..self.cursor_pos]) as u16;
        let cursor_x = area
            .x
            .saturating_add(cursor_col)
            .min(area.x + area.width.saturating_sub(1));
        let cursor_y = area.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    /// Render as a modal popup (~60% terminal size), with a title bar showing
    /// the table and column name.
    pub(crate) fn render_modal(
        &self,
        frame: &mut Frame,
        area: Rect,
        table: &str,
        col_name: &str,
        theme: &Theme,
    ) {
        // Compute popup dimensions (~60% of terminal)
        let popup_w = (area.width * 6 / 10)
            .max(40)
            .min(area.width.saturating_sub(4));
        let popup_h = (area.height * 6 / 10)
            .max(6)
            .min(area.height.saturating_sub(4));
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        let popup_area = Rect::new(popup_x, popup_y, popup_w, popup_h);

        // Clear background
        frame.render_widget(Clear, popup_area);

        let title = format!("Edit {table}.{col_name} (Ctrl+Enter/F10 confirm, Esc cancel)");
        let block = super::overlay_block(&title, theme);

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        // Render buffer text
        let para = Paragraph::new(self.buffer.as_str())
            .style(theme.edit_cell_active)
            .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(para, inner);

        // Place cursor at current position within the popup
        let before_cursor = &self.buffer[..self.cursor_pos];
        let cursor_line = before_cursor.chars().filter(|&c| c == '\n').count() as u16;
        let col_in_line = before_cursor.rfind('\n').map_or_else(
            || UnicodeWidthStr::width(before_cursor),
            |i| UnicodeWidthStr::width(&before_cursor[i + 1..]),
        ) as u16;
        let cursor_x = (inner.x + col_in_line).min(inner.x + inner.width.saturating_sub(1));
        let cursor_y = (inner.y + cursor_line).min(inner.y + inner.height.saturating_sub(1));
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    fn make_key(modifiers: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    fn inline_editor(value: &str) -> CellEditor {
        CellEditor::new(vec![], 0, 0, Some(value), false, false)
    }

    fn modal_editor(value: &str) -> CellEditor {
        CellEditor::new(vec![], 0, 0, Some(value), false, true)
    }

    #[test]
    fn test_typing_inserts_chars() {
        let mut ed = inline_editor("");
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Char('a')));
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Char('b')));
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Char('c')));
        assert_eq!(ed.buffer, "abc");
        assert_eq!(ed.cursor_pos, 3);
    }

    #[test]
    fn test_backspace_deletes() {
        let mut ed = inline_editor("hello");
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Backspace));
        assert_eq!(ed.buffer, "hell");
        assert_eq!(ed.cursor_pos, 4);
    }

    #[test]
    fn test_enter_confirms_inline() {
        let mut ed = inline_editor("foo");
        let action = ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Enter));
        assert!(
            matches!(action, Some(Action::ConfirmCellEdit(Some(ref s))) if s == "foo"),
            "expected ConfirmCellEdit(Some(\"foo\")), got {action:?}"
        );
    }

    #[test]
    fn test_esc_cancels() {
        let mut ed = inline_editor("foo");
        let action = ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Esc));
        assert!(matches!(action, Some(Action::CancelCellEdit)));
    }

    #[test]
    fn test_ctrl_n_sets_null() {
        let mut ed = inline_editor("foo");
        let action = ed.handle_key(make_key(KeyModifiers::CONTROL, KeyCode::Char('n')));
        assert!(
            matches!(action, Some(Action::ConfirmCellEdit(None))),
            "expected ConfirmCellEdit(None), got {action:?}"
        );
    }

    #[test]
    fn test_cursor_movement_left_right() {
        let mut ed = inline_editor("hello");
        // cursor starts at end (5)
        assert_eq!(ed.cursor_pos, 5);
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Left));
        assert_eq!(ed.cursor_pos, 4);
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Right));
        assert_eq!(ed.cursor_pos, 5);
        // Can't go past end
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Right));
        assert_eq!(ed.cursor_pos, 5);
        // Can't go before start
        for _ in 0..10 {
            ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Left));
        }
        assert_eq!(ed.cursor_pos, 0);
    }

    #[test]
    fn test_home_end() {
        let mut ed = inline_editor("hello");
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Home));
        assert_eq!(ed.cursor_pos, 0);
        ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::End));
        assert_eq!(ed.cursor_pos, 5);
    }

    #[test]
    fn test_modal_enter_inserts_newline() {
        let mut ed = modal_editor("hello");
        let action = ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::Enter));
        assert!(action.is_none(), "modal Enter should not produce an action");
        assert_eq!(ed.buffer, "hello\n");
    }

    #[test]
    fn test_modal_ctrl_enter_confirms() {
        let mut ed = modal_editor("line1\nline2");
        let action = ed.handle_key(make_key(KeyModifiers::CONTROL, KeyCode::Enter));
        assert!(
            matches!(action, Some(Action::ConfirmCellEdit(Some(_)))),
            "expected ConfirmCellEdit, got {action:?}"
        );
    }

    #[test]
    fn test_modal_f10_confirms() {
        let mut ed = modal_editor("text");
        let action = ed.handle_key(make_key(KeyModifiers::NONE, KeyCode::F(10)));
        assert!(
            matches!(action, Some(Action::ConfirmCellEdit(Some(_)))),
            "expected ConfirmCellEdit, got {action:?}"
        );
    }
}
