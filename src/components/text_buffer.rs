//! Pure text buffer with cursor, selection, undo/redo, and scroll management.

#![allow(dead_code)] // Items are used after QueryEditor migration (next task).

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

const MAX_UNDO: usize = 100;

/// Word-character predicate for SQL identifiers: alphanumeric + underscore.
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Convert a char offset to a byte offset within a string.
/// Panics if `char_idx` > number of chars (same contract as `String::insert`).
pub(crate) fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map_or(s.len(), |(byte_idx, _)| byte_idx)
}

/// Number of chars in a string (not bytes).
pub(crate) fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// How many visual rows a line occupies when wrapped to `width` display columns.
pub(crate) fn visual_line_height(line: &str, width: usize) -> usize {
    if width == 0 || line.is_empty() {
        return 1;
    }
    let mut rows = 1;
    let mut col = 0;
    for ch in line.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col + w > width {
            rows += 1;
            col = w;
        } else {
            col += w;
        }
    }
    rows
}

/// Map a char-offset cursor column to `(sub_row, display_col)` within a wrapped line.
pub(crate) fn cursor_visual_pos(line: &str, col: usize, width: usize) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }
    let mut sub_row = 0;
    let mut display_col = 0;
    for (i, ch) in line.chars().enumerate() {
        if i == col {
            // When accumulated width fills the row exactly, this char starts the next
            // visual row (matches ratatui's Paragraph::wrap behaviour).
            if display_col >= width {
                return (sub_row + 1, 0);
            }
            return (sub_row, display_col);
        }
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if display_col + w > width {
            sub_row += 1;
            display_col = w;
        } else {
            display_col += w;
        }
    }
    // col == line length — cursor at end of line (render clamps to area)
    (sub_row, display_col)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Selection {
    pub(crate) anchor: (usize, usize), // (row, col)
}

// pub(super): accessible to editor.rs during migration (Task 5).
// Revert to private once QueryEditor delegates all buffer access.
#[derive(Debug)]
pub(crate) struct TextBuffer {
    pub(super) buffer: Vec<String>,
    pub(super) cursor: (usize, usize), // (row, col)
    scroll_offset: usize,
    undo_stack: VecDeque<Vec<String>>,
    redo_stack: Vec<Vec<String>>,
    tab_size: usize,
    selection: Option<Selection>,
    dirty: bool,
    last_save: std::time::Instant,
}

impl TextBuffer {
    pub(crate) fn new(tab_size: usize) -> Self {
        Self {
            buffer: vec![String::new()],
            cursor: (0, 0),
            scroll_offset: 0,
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            tab_size,
            selection: None,
            dirty: false,
            last_save: std::time::Instant::now(),
        }
    }

    // ─── Content accessors ─────────────────────────────────────────────

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

    pub(crate) fn clear(&mut self) {
        self.save_undo();
        self.buffer = vec![String::new()];
        self.cursor = (0, 0);
        self.scroll_offset = 0;
        self.selection = None;
        self.dirty = false;
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn mark_saved(&mut self) {
        self.dirty = false;
        self.last_save = std::time::Instant::now();
    }

    pub(crate) fn last_save_elapsed(&self) -> std::time::Duration {
        self.last_save.elapsed()
    }

    pub(crate) fn cursor_position(&self) -> (usize, usize) {
        self.cursor
    }

    pub(crate) fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor = (row, col);
    }

    pub(crate) fn buffer_lines(&self) -> &[String] {
        &self.buffer
    }

    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub(crate) fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub(crate) fn tab_size(&self) -> usize {
        self.tab_size
    }

    // ─── Undo infrastructure ───────────────────────────────────────────

    fn save_undo(&mut self) {
        self.undo_stack.push_back(self.buffer.clone());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
        self.dirty = true;
    }

    // ─── Undo / redo ──────────────────────────────────────────────────

    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop_back() {
            self.redo_stack.push(self.buffer.clone());
            self.buffer = prev;
            self.selection = None;
            self.dirty = true;
            self.clamp_cursor();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push_back(self.buffer.clone());
            self.buffer = next;
            self.selection = None;
            self.dirty = true;
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

    // ─── Editing operations ───────────────────────────────────────────

    pub(crate) fn insert_char(&mut self, ch: char) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        self.buffer[row].insert(byte_idx, ch);
        self.cursor.1 += 1;
    }

    pub(crate) fn insert_newline(&mut self) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        let remainder = self.buffer[row].split_off(byte_idx);
        self.buffer.insert(row + 1, remainder);
        self.cursor = (row + 1, 0);
    }

    pub(crate) fn backspace(&mut self) {
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

    pub(crate) fn delete(&mut self) {
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

    pub(crate) fn insert_tab(&mut self) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        let spaces = " ".repeat(self.tab_size);
        self.buffer[row].insert_str(byte_idx, &spaces);
        self.cursor.1 += self.tab_size;
    }

    /// Remove up to `tab_size` leading spaces from the current line (Shift+Tab dedent).
    pub(crate) fn dedent(&mut self) {
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

    /// Replace the word before the cursor with the given replacement text.
    /// `prefix_len` is the number of chars before the cursor to replace.
    /// Used by autocomplete to swap a typed prefix with the full completion.
    pub(crate) fn replace_word_before_cursor(&mut self, prefix_len: usize, replacement: &str) {
        debug_assert!(
            prefix_len <= self.cursor.1,
            "prefix_len {prefix_len} > cursor col {}",
            self.cursor.1
        );
        self.save_undo();
        let (row, col) = self.cursor;
        let start_col = col.saturating_sub(prefix_len);
        let start_byte = char_to_byte(&self.buffer[row], start_col);
        let end_byte = char_to_byte(&self.buffer[row], col);
        self.buffer[row].replace_range(start_byte..end_byte, replacement);
        self.cursor.1 = start_col + replacement.chars().count();
    }

    // ─── Cursor movement ──────────────────────────────────────────────

    pub(crate) fn move_cursor_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            let max_col = char_len(&self.buffer[self.cursor.0]);
            if self.cursor.1 > max_col {
                self.cursor.1 = max_col;
            }
        }
    }

    pub(crate) fn move_cursor_down(&mut self) {
        if self.cursor.0 + 1 < self.buffer.len() {
            self.cursor.0 += 1;
            let max_col = char_len(&self.buffer[self.cursor.0]);
            if self.cursor.1 > max_col {
                self.cursor.1 = max_col;
            }
        }
    }

    pub(crate) fn move_cursor_left(&mut self) {
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        } else if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = char_len(&self.buffer[self.cursor.0]);
        }
    }

    pub(crate) fn move_cursor_right(&mut self) {
        let (row, col) = self.cursor;
        if col < char_len(&self.buffer[row]) {
            self.cursor.1 += 1;
        } else if row + 1 < self.buffer.len() {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
    }

    pub(crate) fn move_home(&mut self) {
        self.cursor.1 = 0;
    }

    pub(crate) fn move_end(&mut self) {
        let row = self.cursor.0;
        self.cursor.1 = char_len(&self.buffer[row]);
    }

    /// Move cursor left by one word boundary.
    pub(crate) fn move_word_left(&mut self) {
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
    pub(crate) fn move_word_right(&mut self) {
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

    // ─── Selection ────────────────────────────────────────────────────

    pub(crate) fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub(crate) fn start_or_extend_selection(&mut self) {
        if self.selection.is_none() {
            self.selection = Some(Selection {
                anchor: self.cursor,
            });
        }
    }

    /// Get ordered selection bounds: (start, end) where start <= end.
    pub(crate) fn selection_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
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
        // Zero-width selection -> None
        if text.is_empty() { None } else { Some(text) }
    }

    /// Delete the selected range and collapse cursor to start of range.
    pub(crate) fn delete_selection(&mut self) -> bool {
        let Some(((sr, sc), (er, ec))) = self.selection_bounds() else {
            return false;
        };
        if sr == er && sc == ec {
            return false; // zero-width selection — nothing to delete
        }
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
    pub(crate) fn select_all(&mut self) {
        self.selection = Some(Selection { anchor: (0, 0) });
        let last_row = self.buffer.len().saturating_sub(1);
        self.cursor = (last_row, char_len(&self.buffer[last_row]));
    }

    /// Compute the selection column range for a given line.
    /// Returns `(start_col, end_col)` in char units, or `(0, 0)` if no selection on this line.
    pub(crate) fn line_selection_cols(&self, line_idx: usize) -> (usize, usize) {
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

    // ─── Scroll ───────────────────────────────────────────────────────

    pub(crate) fn adjust_scroll(&mut self, visible_height: usize, content_width: usize) {
        let row = self.cursor.0;

        // Cursor above visible area — snap scroll to cursor line
        if row < self.scroll_offset {
            self.scroll_offset = row;
            return;
        }

        // Compute total visual rows from scroll_offset to cursor position
        let mut total: usize = 0;
        for i in self.scroll_offset..row {
            total += visual_line_height(&self.buffer[i], content_width);
        }
        let (sub, _) = cursor_visual_pos(&self.buffer[row], self.cursor.1, content_width);
        total += sub;

        // Evict lines from the top until the cursor fits on screen
        while total >= visible_height && self.scroll_offset < row {
            total -= visual_line_height(&self.buffer[self.scroll_offset], content_width);
            self.scroll_offset += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_has_one_empty_line() {
        let buf = TextBuffer::new(4);
        assert_eq!(buf.buffer.len(), 1);
        assert_eq!(buf.buffer[0], "");
        assert_eq!(buf.cursor, (0, 0));
        assert!(!buf.dirty);
    }

    #[test]
    fn contents_joins_lines() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["SELECT *".into(), "FROM t".into()];
        assert_eq!(buf.contents(), "SELECT *\nFROM t");
    }

    #[test]
    fn set_contents_splits_on_newline() {
        let mut buf = TextBuffer::new(4);
        buf.set_contents("line1\nline2\nline3");
        assert_eq!(buf.buffer, vec!["line1", "line2", "line3"]);
        assert_eq!(buf.cursor, (0, 0));
        assert!(buf.is_dirty());
    }

    #[test]
    fn set_contents_empty_gives_one_empty_line() {
        let mut buf = TextBuffer::new(4);
        buf.set_contents("");
        assert_eq!(buf.buffer.len(), 1);
        assert_eq!(buf.buffer[0], "");
    }

    #[test]
    fn clear_resets_everything() {
        let mut buf = TextBuffer::new(4);
        buf.set_contents("hello\nworld");
        buf.cursor = (1, 3);
        buf.clear();
        assert_eq!(buf.buffer, vec![""]);
        assert_eq!(buf.cursor, (0, 0));
        assert!(!buf.is_dirty());
    }

    #[test]
    fn clear_pushes_undo_then_resets_dirty() {
        let mut buf = TextBuffer::new(4);
        buf.set_contents("old content");
        buf.dirty = false; // simulate saved state
        buf.clear();
        // clear should NOT be dirty
        assert!(!buf.is_dirty());
        // but undo should restore old content and mark dirty
        buf.undo();
        assert!(buf.is_dirty());
        assert_eq!(buf.contents(), "old content");
    }

    // ─── Editing operations ───────────────────────────────────────────

    #[test]
    fn insert_char_at_cursor() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hllo".into()];
        buf.cursor = (0, 1);
        buf.insert_char('e');
        assert_eq!(buf.buffer[0], "hello");
        assert_eq!(buf.cursor, (0, 2));
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello world".into()];
        buf.cursor = (0, 5);
        buf.insert_newline();
        assert_eq!(buf.buffer, vec!["hello", " world"]);
        assert_eq!(buf.cursor, (1, 0));
    }

    #[test]
    fn backspace_joins_lines() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into(), " world".into()];
        buf.cursor = (1, 0);
        buf.backspace();
        assert_eq!(buf.buffer, vec!["hello world"]);
        assert_eq!(buf.cursor, (0, 5));
    }

    #[test]
    fn backspace_deletes_char() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into()];
        buf.cursor = (0, 3);
        buf.backspace();
        assert_eq!(buf.buffer[0], "helo");
        assert_eq!(buf.cursor, (0, 2));
    }

    #[test]
    fn delete_forward_removes_char() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into()];
        buf.cursor = (0, 1);
        buf.delete();
        assert_eq!(buf.buffer[0], "hllo");
    }

    #[test]
    fn delete_at_end_joins_next_line() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into(), " world".into()];
        buf.cursor = (0, 5);
        buf.delete();
        assert_eq!(buf.buffer, vec!["hello world"]);
    }

    #[test]
    fn insert_tab_adds_spaces() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["SELECT".into()];
        buf.cursor = (0, 0);
        buf.insert_tab();
        assert_eq!(buf.buffer[0], "    SELECT");
        assert_eq!(buf.cursor, (0, 4));
    }

    #[test]
    fn dedent_removes_leading_spaces() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["    SELECT".into()];
        buf.cursor = (0, 6);
        buf.dedent();
        assert_eq!(buf.buffer[0], "SELECT");
        assert_eq!(buf.cursor, (0, 2));
    }

    #[test]
    fn undo_restores_previous() {
        let mut buf = TextBuffer::new(4);
        buf.insert_char('a');
        buf.insert_char('b');
        assert_eq!(buf.contents(), "ab");
        buf.undo();
        assert_eq!(buf.contents(), "a");
    }

    #[test]
    fn redo_after_undo() {
        let mut buf = TextBuffer::new(4);
        buf.insert_char('a');
        buf.insert_char('b');
        buf.undo();
        assert_eq!(buf.contents(), "a");
        buf.redo();
        assert_eq!(buf.contents(), "ab");
    }

    #[test]
    fn replace_word_before_cursor() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["SELECT col".into()];
        buf.cursor = (0, 10); // after "col"
        buf.replace_word_before_cursor(3, "column_name");
        assert_eq!(buf.buffer[0], "SELECT column_name");
        assert_eq!(buf.cursor.1, 18); // 7 ("SELECT ") + 11 ("column_name")
    }

    #[test]
    fn replace_word_before_cursor_saves_undo() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["SELECT col".into()];
        buf.cursor = (0, 10);
        buf.dirty = false;
        buf.replace_word_before_cursor(3, "column_name");
        assert!(buf.is_dirty());
        buf.undo();
        assert_eq!(buf.buffer[0], "SELECT col");
    }

    // ─── Cursor movement ──────────────────────────────────────────────

    #[test]
    fn move_cursor_up_clamps_col() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hi".into(), "hello".into()];
        buf.cursor = (1, 4);
        buf.move_cursor_up();
        assert_eq!(buf.cursor, (0, 2)); // clamped to "hi" length
    }

    #[test]
    fn move_cursor_down_wraps() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into(), "hi".into()];
        buf.cursor = (0, 4);
        buf.move_cursor_down();
        assert_eq!(buf.cursor, (1, 2)); // clamped to "hi" length
    }

    #[test]
    fn move_left_wraps_to_prev_line() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["abc".into(), "def".into()];
        buf.cursor = (1, 0);
        buf.move_cursor_left();
        assert_eq!(buf.cursor, (0, 3)); // end of previous line
    }

    #[test]
    fn move_right_wraps_to_next_line() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["abc".into(), "def".into()];
        buf.cursor = (0, 3);
        buf.move_cursor_right();
        assert_eq!(buf.cursor, (1, 0)); // start of next line
    }

    #[test]
    fn move_word_left_skips_non_word() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello world".into()];
        buf.cursor = (0, 6); // at 'w'
        buf.move_word_left();
        assert_eq!(buf.cursor, (0, 0)); // back to start of "hello"
    }

    #[test]
    fn move_word_right_skips_to_next() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello world".into()];
        buf.cursor = (0, 0);
        buf.move_word_right();
        assert_eq!(buf.cursor, (0, 6)); // start of "world"
    }

    // ─── Selection ────────────────────────────────────────────────────

    #[test]
    fn select_all_covers_buffer() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into(), "world".into()];
        buf.select_all();
        let sel = buf.selection.unwrap();
        assert_eq!(sel.anchor, (0, 0));
        assert_eq!(buf.cursor, (1, 5));
    }

    #[test]
    fn selected_text_single_line() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello world".into()];
        buf.selection = Some(Selection { anchor: (0, 0) });
        buf.cursor = (0, 5);
        assert_eq!(buf.selected_text(), Some("hello".into()));
    }

    #[test]
    fn delete_selection_collapses() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello world".into()];
        buf.selection = Some(Selection { anchor: (0, 0) });
        buf.cursor = (0, 5);
        let deleted = buf.delete_selection();
        assert!(deleted);
        assert_eq!(buf.buffer[0], " world");
        assert_eq!(buf.cursor, (0, 0));
        assert!(buf.selection.is_none());
    }

    #[test]
    fn line_selection_cols_partial_selection() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into(), "world".into(), "foo".into()];
        buf.selection = Some(Selection { anchor: (0, 2) });
        buf.cursor = (1, 3);
        // Line 0: starts at anchor col 2, ends at line length 5
        assert_eq!(buf.line_selection_cols(0), (2, 5));
        // Line 1: starts at 0, ends at cursor col 3
        assert_eq!(buf.line_selection_cols(1), (0, 3));
        // Line 2: outside selection
        assert_eq!(buf.line_selection_cols(2), (0, 0));
    }

    #[test]
    fn line_selection_cols_no_selection() {
        let buf = TextBuffer::new(4);
        assert_eq!(buf.line_selection_cols(0), (0, 0));
    }

    // ─── Multiline selection/deletion ────────────────────────────────

    #[test]
    fn selected_text_multiline() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into(), "world".into()];
        buf.selection = Some(Selection { anchor: (0, 3) });
        buf.cursor = (1, 2);
        assert_eq!(buf.selected_text(), Some("lo\nwo".to_string()));
    }

    #[test]
    fn selected_text_multiline_three_lines() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["aaa".into(), "bbb".into(), "ccc".into()];
        buf.selection = Some(Selection { anchor: (0, 1) });
        buf.cursor = (2, 2);
        assert_eq!(buf.selected_text(), Some("aa\nbbb\ncc".to_string()));
    }

    #[test]
    fn delete_selection_multiline() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["abc".into(), "def".into(), "ghi".into()];
        buf.selection = Some(Selection { anchor: (0, 1) });
        buf.cursor = (2, 2);
        // Deletes: "bc" + "def" + "gh" → leaves "a" + "i" = "ai"
        buf.delete_selection();
        assert_eq!(buf.buffer, vec!["ai"]);
        assert_eq!(buf.cursor, (0, 1));
    }

    #[test]
    fn delete_selection_zero_width_is_noop() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into()];
        buf.selection = Some(Selection { anchor: (0, 2) });
        buf.cursor = (0, 2); // same as anchor — zero-width
        let deleted = buf.delete_selection();
        assert!(!deleted);
        assert_eq!(buf.buffer[0], "hello"); // unchanged
    }

    // ─── No-op boundary cases ────────────────────────────────────────

    #[test]
    fn backspace_at_origin_is_noop() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into()];
        buf.cursor = (0, 0);
        buf.backspace();
        assert_eq!(buf.buffer[0], "hello");
        assert_eq!(buf.cursor, (0, 0));
        assert!(!buf.is_dirty()); // no undo pushed
    }

    #[test]
    fn delete_at_end_of_last_line_is_noop() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into()];
        buf.cursor = (0, 5);
        buf.delete();
        assert_eq!(buf.buffer[0], "hello");
        assert!(!buf.is_dirty()); // no undo pushed
    }

    #[test]
    fn undo_on_empty_stack_is_noop() {
        let mut buf = TextBuffer::new(4);
        buf.undo();
        assert_eq!(buf.buffer, vec![""]);
        assert!(!buf.is_dirty());
    }

    #[test]
    fn redo_on_empty_stack_is_noop() {
        let mut buf = TextBuffer::new(4);
        buf.redo();
        assert_eq!(buf.buffer, vec![""]);
        assert!(!buf.is_dirty());
    }

    #[test]
    fn move_cursor_up_at_row_zero_stays() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into(), "world".into()];
        buf.cursor = (0, 3);
        buf.move_cursor_up();
        assert_eq!(buf.cursor, (0, 3)); // unchanged
    }

    #[test]
    fn move_cursor_down_at_last_row_stays() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["hello".into()];
        buf.cursor = (0, 3);
        buf.move_cursor_down();
        assert_eq!(buf.cursor, (0, 3)); // unchanged
    }

    // ─── adjust_scroll ───────────────────────────────────────────────

    #[test]
    fn adjust_scroll_cursor_below_visible_area() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec![
            "line 1".into(),
            "line 2".into(),
            "line 3".into(),
            "line 4".into(),
            "line 5".into(),
        ];
        buf.cursor = (4, 0); // last line
        buf.adjust_scroll(3, 80); // only 3 visible rows
        // scroll_offset should advance so cursor is visible
        assert!(buf.scroll_offset > 0);
        assert!(buf.scroll_offset <= 4);
    }

    #[test]
    fn adjust_scroll_cursor_above_visible_area() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["line 1".into(), "line 2".into(), "line 3".into()];
        buf.scroll_offset = 2; // scrolled past first two lines
        buf.cursor = (0, 0); // cursor at top
        buf.adjust_scroll(3, 80);
        assert_eq!(buf.scroll_offset, 0); // scrolled back to show cursor
    }

    #[test]
    fn adjust_scroll_cursor_already_visible() {
        let mut buf = TextBuffer::new(4);
        buf.buffer = vec!["line 1".into(), "line 2".into()];
        buf.cursor = (1, 0);
        buf.scroll_offset = 0;
        buf.adjust_scroll(5, 80); // plenty of room
        assert_eq!(buf.scroll_offset, 0); // unchanged
    }
}
