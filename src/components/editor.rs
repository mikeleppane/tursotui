use std::collections::VecDeque;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::app::{Action, Direction, ExecutionSource, SchemaCache};
use crate::autocomplete;
use crate::components::autocomplete::AutocompletePopup;
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

/// How many visual rows a line occupies when wrapped to `width` display columns.
fn visual_line_height(line: &str, width: usize) -> usize {
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
fn cursor_visual_pos(line: &str, col: usize, width: usize) -> (usize, usize) {
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

#[allow(clippy::struct_excessive_bools)] // independent boolean states, not a state machine
pub(crate) struct QueryEditor {
    buffer: Vec<String>,
    cursor: (usize, usize), // (row, col)
    scroll_offset: usize,
    undo_stack: VecDeque<Vec<String>>,
    redo_stack: Vec<Vec<String>>,
    tab_size: usize,
    selection: Option<Selection>,
    dirty: bool,
    last_save: std::time::Instant,
    pub(crate) autocomplete_popup: Option<AutocompletePopup>,
    autocomplete_enabled: bool,
    autocomplete_min_chars: usize,
    // Parameter bar state.
    // Invariant: param_bar_focused → param_bar_active (focused implies visible).
    // Maintained by: sync_params() clears both; handle_param_bar_key only clears focused.
    param_fields: Vec<(String, Option<String>)>, // (placeholder_name, value: None=NULL)
    param_focused_idx: usize,
    param_bar_active: bool,
    param_bar_focused: bool, // true = keyboard input goes to param bar, not editor text area
}

/// Convert a string value to the most appropriate `turso::Value` type.
/// Tries integer first, then real, falls back to text.
fn string_to_value(s: &str) -> tursotui_db::Value {
    if s.is_empty() {
        return tursotui_db::Value::Text(String::new());
    }
    if let Ok(n) = s.parse::<i64>() {
        return tursotui_db::Value::Integer(n);
    }
    if let Ok(f) = s.parse::<f64>() {
        return tursotui_db::Value::Real(f);
    }
    tursotui_db::Value::Text(s.to_string())
}

/// Convert parameter bar fields to `QueryParams` for database execution.
/// Returns `None` if no parameters are present or if positional and named
/// params are mixed (which `SQLite` does not support).
fn build_query_params(fields: &[(String, Option<String>)]) -> Option<tursotui_db::QueryParams> {
    if fields.is_empty() {
        return None;
    }

    let has_positional = fields.iter().any(|(name, _)| name.starts_with('?'));
    let has_named = fields
        .iter()
        .any(|(name, _)| name.starts_with(':') || name.starts_with('$') || name.starts_with('@'));

    // SQLite doesn't support mixing positional and named params — return None
    // so the query executes without bindings (the DB will report the real error).
    if has_positional && has_named {
        return None;
    }

    let is_positional = has_positional;

    let values: Vec<tursotui_db::Value> = fields
        .iter()
        .map(|(_, v)| match v {
            None => tursotui_db::Value::Null,
            Some(s) => string_to_value(s),
        })
        .collect();

    if is_positional {
        Some(tursotui_db::QueryParams::Positional(values))
    } else {
        let named: Vec<(String, tursotui_db::Value)> = fields
            .iter()
            .zip(values)
            .map(|((name, _), val)| (name.clone(), val))
            .collect();
        Some(tursotui_db::QueryParams::Named(named))
    }
}

impl QueryEditor {
    pub(crate) fn new() -> Self {
        Self {
            buffer: vec![String::new()],
            cursor: (0, 0),
            scroll_offset: 0,
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            tab_size: TAB_SIZE,
            selection: None,
            dirty: false,
            last_save: std::time::Instant::now(),
            autocomplete_popup: None,
            autocomplete_enabled: true,
            autocomplete_min_chars: 1,
            param_fields: Vec::new(),
            param_focused_idx: 0,
            param_bar_active: false,
            param_bar_focused: false,
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
        self.sync_params();
    }

    pub(crate) fn clear(&mut self) {
        self.save_undo();
        self.buffer = vec![String::new()];
        self.cursor = (0, 0);
        self.scroll_offset = 0;
        self.selection = None;
        self.dirty = false;
        self.sync_params();
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

    fn save_undo(&mut self) {
        self.undo_stack.push_back(self.buffer.clone());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
        self.dirty = true;
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop_back() {
            self.redo_stack.push(self.buffer.clone());
            self.buffer = prev;
            self.selection = None;
            self.dirty = true;
            self.clamp_cursor();
            self.sync_params();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push_back(self.buffer.clone());
            self.buffer = next;
            self.selection = None;
            self.dirty = true;
            self.clamp_cursor();
            self.sync_params();
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
        self.sync_params();
    }

    fn insert_newline(&mut self) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        let remainder = self.buffer[row].split_off(byte_idx);
        self.buffer.insert(row + 1, remainder);
        self.cursor = (row + 1, 0);
        self.sync_params();
    }

    fn backspace(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            self.save_undo();
            let byte_idx = char_to_byte(&self.buffer[row], col - 1);
            self.buffer[row].remove(byte_idx);
            self.cursor.1 -= 1;
            self.sync_params();
        } else if row > 0 {
            self.save_undo();
            let current_line = self.buffer.remove(row);
            let prev_char_len = char_len(&self.buffer[row - 1]);
            self.buffer[row - 1].push_str(&current_line);
            self.cursor = (row - 1, prev_char_len);
            self.sync_params();
        }
    }

    fn delete(&mut self) {
        let (row, col) = self.cursor;
        let line_char_len = char_len(&self.buffer[row]);
        if col < line_char_len {
            self.save_undo();
            let byte_idx = char_to_byte(&self.buffer[row], col);
            self.buffer[row].remove(byte_idx);
            self.sync_params();
        } else if row + 1 < self.buffer.len() {
            self.save_undo();
            let next_line = self.buffer.remove(row + 1);
            self.buffer[row].push_str(&next_line);
            self.sync_params();
        }
    }

    fn insert_tab(&mut self) {
        self.save_undo();
        let (row, col) = self.cursor;
        let byte_idx = char_to_byte(&self.buffer[row], col);
        let spaces = " ".repeat(self.tab_size);
        self.buffer[row].insert_str(byte_idx, &spaces);
        self.cursor.1 += self.tab_size;
        self.sync_params();
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
            self.sync_params();
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
        self.sync_params();
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
        let statements = tursotui_sql::parser::detect_statements(&full);
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

    fn adjust_scroll(&mut self, visible_height: usize, content_width: usize) {
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

    // ─── Autocomplete ───────────────────────────────────────────────────

    pub(crate) fn set_autocomplete_config(&mut self, enabled: bool, min_chars: usize) {
        self.autocomplete_enabled = enabled;
        self.autocomplete_min_chars = min_chars;
    }

    /// Whether automatic autocomplete triggering is enabled.
    pub(crate) fn autocomplete_enabled(&self) -> bool {
        self.autocomplete_enabled
    }

    /// Trigger autocomplete at the current cursor position (explicit Ctrl+Space).
    /// Always works regardless of `autocomplete_enabled` — that flag gates
    /// automatic triggering only, not explicit invocation.
    pub(crate) fn trigger_autocomplete(&mut self, schema: &SchemaCache) {
        let (context, prefix) =
            autocomplete::detect_context(&self.buffer, self.cursor.0, self.cursor.1, schema);
        let candidates = autocomplete::generate_candidates(&context, &prefix, schema);
        if candidates.is_empty() {
            self.autocomplete_popup = None;
        } else {
            let mut popup = AutocompletePopup::new(prefix);
            popup.update_candidates(candidates);
            self.autocomplete_popup = Some(popup);
        }
    }

    /// Auto-trigger autocomplete when enabled and prefix meets `min_chars`.
    /// Called by the event loop after buffer-modifying keys when no popup is open.
    pub(crate) fn auto_trigger_autocomplete(&mut self, schema: &SchemaCache) {
        let (context, prefix) =
            autocomplete::detect_context(&self.buffer, self.cursor.0, self.cursor.1, schema);
        if prefix.chars().count() < self.autocomplete_min_chars {
            return;
        }
        let candidates = autocomplete::generate_candidates(&context, &prefix, schema);
        if !candidates.is_empty() {
            let mut popup = AutocompletePopup::new(prefix);
            popup.update_candidates(candidates);
            self.autocomplete_popup = Some(popup);
        }
    }

    /// Refresh the autocomplete popup with updated candidates (after typing).
    pub(crate) fn refresh_autocomplete(&mut self, schema: &SchemaCache) {
        let (context, prefix) =
            autocomplete::detect_context(&self.buffer, self.cursor.0, self.cursor.1, schema);
        if prefix.chars().count() < self.autocomplete_min_chars {
            self.autocomplete_popup = None;
            return;
        }
        let candidates = autocomplete::generate_candidates(&context, &prefix, schema);
        if let Some(ref mut popup) = self.autocomplete_popup {
            popup.prefix = prefix;
            popup.update_candidates(candidates);
            if popup.is_empty() {
                self.autocomplete_popup = None;
            }
        }
    }

    /// Accept the currently selected completion candidate.
    /// Replaces the prefix with the full completion text.
    pub(crate) fn accept_completion(&mut self) -> Option<String> {
        let popup = self.autocomplete_popup.take()?;
        let text = popup.selected_text()?.to_string();
        let prefix_len = popup.prefix.chars().count();

        self.save_undo();
        let (row, col) = self.cursor;
        let start_col = col.saturating_sub(prefix_len);
        let start_byte = char_to_byte(&self.buffer[row], start_col);
        let end_byte = char_to_byte(&self.buffer[row], col);
        self.buffer[row].replace_range(start_byte..end_byte, &text);
        self.cursor.1 = start_col + text.chars().count();
        self.sync_params();

        Some(text)
    }

    /// Dismiss the autocomplete popup without accepting.
    pub(crate) fn dismiss_autocomplete(&mut self) {
        self.autocomplete_popup = None;
    }

    /// Returns the cursor position for autocomplete popup rendering.
    pub(crate) fn cursor_position(&self) -> (usize, usize) {
        self.cursor
    }

    /// Returns a reference to the buffer lines (for autocomplete engine).
    pub(crate) fn buffer_lines(&self) -> &[String] {
        &self.buffer
    }

    /// Returns the current scroll offset.
    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    // ─── Parameter bar ──────────────────────────────────────────────────

    /// Extract unique parameter placeholders from current editor content.
    /// Returns placeholders in order of first appearance, deduplicated.
    pub(crate) fn extract_params(&self) -> Vec<String> {
        use crate::highlight::{TokenKind, tokenize};
        let tokens = tokenize(&self.contents());
        let mut seen = std::collections::HashSet::new();
        let mut params = Vec::new();
        for token in &tokens {
            if token.kind == TokenKind::Parameter && seen.insert(token.text.clone()) {
                params.push(token.text.clone());
            }
        }
        params
    }

    /// Synchronize parameter bar fields with current editor content.
    /// Preserves values for parameters that still exist, removes stale ones,
    /// adds new ones with None (NULL).
    fn sync_params(&mut self) {
        // Short-circuit: skip full retokenization if no parameter chars exist in the buffer.
        // This avoids the cost of tokenize() + HashSet + Vec rebuild on every keystroke
        // for the common case where the query has no parameters at all.
        let has_param_chars = self
            .buffer
            .iter()
            .any(|line| line.contains('?') || line.contains(':') || line.contains('$') || line.contains('@'));
        if !has_param_chars {
            if self.param_bar_active {
                self.param_fields.clear();
                self.param_bar_active = false;
                self.param_bar_focused = false;
                self.param_focused_idx = 0;
            }
            return;
        }
        let current_params = self.extract_params();
        if current_params.is_empty() {
            self.param_fields.clear();
            self.param_bar_active = false;
            self.param_bar_focused = false;
            self.param_focused_idx = 0;
            return;
        }
        // Build new fields, preserving existing values
        let old_values: std::collections::HashMap<String, Option<String>> =
            self.param_fields.iter().cloned().collect();
        self.param_fields = current_params
            .into_iter()
            .map(|name| {
                let value = old_values.get(&name).cloned().flatten();
                (name, value)
            })
            .collect();
        // Clamp focused index
        if self.param_focused_idx >= self.param_fields.len() {
            self.param_focused_idx = 0;
        }
        // Auto-show bar when params exist
        if !self.param_fields.is_empty() {
            self.param_bar_active = true;
        }
    }

    #[allow(dead_code)] // used in tests; called from dispatch/layout when param persistence is wired
    pub(crate) fn param_bar_active(&self) -> bool {
        self.param_bar_active
    }

    #[allow(dead_code)] // used in tests; called from dispatch/layout when param persistence is wired
    pub(crate) fn param_fields(&self) -> &[(String, Option<String>)] {
        &self.param_fields
    }

    #[allow(dead_code)] // used in tests; called from dispatch/layout when param persistence is wired
    pub(crate) fn param_focused_idx(&self) -> usize {
        self.param_focused_idx
    }

    #[allow(dead_code)] // used by tests
    pub(crate) fn param_bar_focused(&self) -> bool {
        self.param_bar_focused
    }

    /// Handle keyboard input when the parameter bar has focus.
    /// Always returns `Action::Consumed` — the param bar absorbs all keys.
    fn handle_param_bar_key(&mut self, key: KeyEvent) -> Action {
        match (key.modifiers, key.code) {
            // Tab → next field (wraps)
            (KeyModifiers::NONE, KeyCode::Tab) => {
                if !self.param_fields.is_empty() {
                    self.param_focused_idx = (self.param_focused_idx + 1) % self.param_fields.len();
                }
            }
            // Shift+Tab → prev field (wraps)
            (KeyModifiers::SHIFT, KeyCode::BackTab) => {
                if !self.param_fields.is_empty() {
                    self.param_focused_idx = if self.param_focused_idx == 0 {
                        self.param_fields.len() - 1
                    } else {
                        self.param_focused_idx - 1
                    };
                }
            }
            // Ctrl+N → set current field to NULL
            (KeyModifiers::CONTROL, KeyCode::Char('n')) => {
                if let Some(field) = self.param_fields.get_mut(self.param_focused_idx) {
                    field.1 = None;
                }
            }
            // Esc → return focus to editor text area
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.param_bar_focused = false;
            }
            // Backspace → delete last char (keeps as empty string, not NULL)
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                if let Some(field) = self.param_fields.get_mut(self.param_focused_idx)
                    && let Some(ref mut v) = field.1
                {
                    v.pop();
                }
            }
            // Printable chars → append to value (NULL → string on first keystroke)
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                if let Some(field) = self.param_fields.get_mut(self.param_focused_idx) {
                    match &mut field.1 {
                        Some(v) => v.push(c),
                        None => field.1 = Some(c.to_string()),
                    }
                }
            }
            // Everything else → no-op
            _ => {}
        }
        Action::Consumed
    }

    /// Render the parameter bar showing current parameter values.
    fn render_param_bar(&self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        if !self.param_bar_active || self.param_fields.is_empty() {
            return;
        }

        // Build spans for each parameter field
        let mut spans = Vec::new();
        for (i, (name, value)) in self.param_fields.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  ")); // separator between fields
            }
            // Parameter name
            spans.push(Span::styled(format!("{name}: "), theme.sql_parameter));
            // Value display: only highlight the focused field when both the panel has focus
            // AND the param bar itself has keyboard focus.
            let is_focused = focused && self.param_bar_focused && i == self.param_focused_idx;
            match value {
                None => {
                    let style = if is_focused {
                        Style::default().fg(theme.dim).bg(theme.surface1)
                    } else {
                        Style::default().fg(theme.dim)
                    };
                    spans.push(Span::styled("NULL", style));
                }
                Some(v) => {
                    let display = if v.is_empty() { "\"\"" } else { v.as_str() };
                    let style = if is_focused {
                        Style::default().fg(theme.fg).bg(theme.surface1)
                    } else {
                        Style::default().fg(theme.fg)
                    };
                    spans.push(Span::styled(display.to_string(), style));
                }
            }
        }

        let line = Line::from(spans);
        let block = super::panel_block("Parameters", focused, theme);
        let paragraph = Paragraph::new(line).block(block);
        frame.render_widget(paragraph, area);
    }
}

/// Render the gutter (line number + blank continuation rows) for a buffer line.
#[allow(clippy::too_many_arguments)]
fn render_gutter(
    frame: &mut Frame,
    x: u16,
    y: u16,
    gutter_width: u16,
    gutter_digits: usize,
    line_num: usize,
    rows: usize,
    is_cursor_line: bool,
    gutter_style: Style,
    theme: &Theme,
) {
    let primary_style = if is_cursor_line {
        Style::default().fg(theme.accent).bg(theme.active_line_bg)
    } else {
        gutter_style
    };
    let num_str = format!("{line_num:>gutter_digits$} ");
    frame.render_widget(
        Paragraph::new(num_str).style(primary_style),
        Rect {
            x,
            y,
            width: gutter_width,
            height: 1,
        },
    );

    let cont_style = if is_cursor_line {
        Style::default().bg(theme.active_line_bg)
    } else {
        gutter_style
    };
    let blank = " ".repeat(gutter_width as usize);
    for sub in 1..rows {
        frame.render_widget(
            Paragraph::new(blank.clone()).style(cont_style),
            Rect {
                x,
                y: y + sub as u16,
                width: gutter_width,
                height: 1,
            },
        );
    }
}

impl Component for QueryEditor {
    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        // Parameter bar has keyboard focus — route most keys there,
        // but let execution keys (F5, Ctrl+Enter) fall through to the
        // normal handler so the user can execute directly from the param bar.
        // Focus stays in the param bar after execution so the user can
        // quickly tweak values and re-execute without Tab-ing back in.
        if self.param_bar_focused {
            let is_execute_key = matches!(
                (key.modifiers, key.code),
                (_, KeyCode::F(5)) | (KeyModifiers::CONTROL, KeyCode::Enter)
            );
            if !is_execute_key {
                return Some(self.handle_param_bar_key(key));
            }
            // Fall through to execution handling — param_bar_focused stays true
        }

        // When param bar is active and Tab is pressed, focus the param bar
        // regardless of autocomplete state. This takes priority over completion
        // acceptance — the user can always Esc to dismiss autocomplete first
        // if they want Tab to accept a completion instead.
        if matches!((key.modifiers, key.code), (KeyModifiers::NONE, KeyCode::Tab))
            && self.param_bar_active
            && !self.param_fields.is_empty()
        {
            self.dismiss_autocomplete();
            self.param_bar_focused = true;
            return Some(Action::Consumed);
        }

        // Autocomplete popup intercepts keys when active
        if self.autocomplete_popup.is_some() {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Up) => {
                    if let Some(ref mut popup) = self.autocomplete_popup {
                        popup.move_up();
                    }
                    return None;
                }
                (KeyModifiers::NONE, KeyCode::Down) => {
                    if let Some(ref mut popup) = self.autocomplete_popup {
                        popup.move_down();
                    }
                    return None;
                }
                (KeyModifiers::NONE, KeyCode::Tab | KeyCode::Enter) => {
                    if let Some(text) = self.accept_completion() {
                        return Some(Action::AcceptCompletion(text));
                    }
                    // No completion to accept — dismiss and fall through.
                    self.dismiss_autocomplete();
                }
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    self.dismiss_autocomplete();
                    return None;
                }
                // Character input, backspace, and delete: fall through to normal
                // handling. Autocomplete is refreshed by main.rs after the action.
                (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(_))
                | (KeyModifiers::NONE, KeyCode::Backspace | KeyCode::Delete) => {}
                // Any other key dismisses autocomplete and falls through
                _ => {
                    self.dismiss_autocomplete();
                }
            }
        }

        match (key.modifiers, key.code) {
            // Trigger autocomplete — Ctrl+Space arrives as Char(' ') with CONTROL
            // in kitty-protocol terminals, but as Char('\0') (NUL) in traditional
            // terminals that send ^@ for Ctrl+Space.
            (KeyModifiers::CONTROL, KeyCode::Char(' ' | '\0')) => Some(Action::TriggerAutocomplete),

            // Execute selection or statement at cursor: Ctrl+Shift+Enter
            (m, KeyCode::Enter) if m == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
                let (text, source) = self.text_to_execute();
                Some(Action::ExecuteQuery {
                    sql: text,
                    source,
                    source_table: None,
                    params: build_query_params(&self.param_fields),
                })
            }

            // Execute full buffer: F5 or Ctrl+Enter
            (_, KeyCode::F(5)) | (KeyModifiers::CONTROL, KeyCode::Enter) => {
                Some(Action::ExecuteQuery {
                    sql: self.contents(),
                    source: ExecutionSource::FullBuffer,
                    source_table: None,
                    params: build_query_params(&self.param_fields),
                })
            }

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
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => Some(Action::ClearEditor),

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
            (KeyModifiers::NONE, KeyCode::End) => {
                self.clear_selection();
                self.move_end();
                None
            }
            // Ctrl+E: open export popup (not end-of-line — use End key or Home/End instead).
            // Traditional terminals can't distinguish Ctrl+E from Ctrl+Shift+E,
            // so Ctrl+E triggers export even from the editor.
            (KeyModifiers::CONTROL, KeyCode::Char('e')) => Some(Action::ShowExport),

            // Enter → replace selection or newline
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.selection.is_some() {
                    self.delete_selection();
                }
                self.insert_newline();
                Some(Action::Consumed)
            }

            // Backspace / Delete → delete selection or single char
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                if self.selection.is_some() {
                    self.delete_selection();
                } else {
                    self.backspace();
                }
                Some(Action::Consumed)
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                if self.selection.is_some() {
                    self.delete_selection();
                } else {
                    self.delete();
                }
                Some(Action::Consumed)
            }

            // Tab → indent (param bar focus is handled earlier, before autocomplete)
            (KeyModifiers::NONE, KeyCode::Tab) => {
                if self.selection.is_some() {
                    self.delete_selection();
                }
                self.insert_tab();
                Some(Action::Consumed)
            }
            (_, KeyCode::BackTab) => {
                self.clear_selection();
                self.dedent();
                Some(Action::Consumed)
            }

            // Regular character input (replaces selection if active)
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(ch)) => {
                if self.selection.is_some() {
                    self.delete_selection();
                }
                self.insert_char(ch);
                Some(Action::Consumed)
            }

            _ => None,
        }
    }

    fn update(&mut self, action: &Action) {
        match action {
            Action::PopulateEditor(sql)
            | Action::RecallHistory(sql)
            | Action::RecallBookmark(sql) => {
                self.set_contents(sql);
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        // Split area to accommodate the parameter bar when active and space permits.
        // Require at least 6 rows total (3 for editor minimum + 3 for param bar).
        let (editor_area, param_area) =
            if self.param_bar_active && !self.param_fields.is_empty() && area.height >= 6 {
                let chunks = Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(3)])
                    .split(area);
                (chunks[0], Some(chunks[1]))
            } else {
                (area, None)
            };

        // When param bar has keyboard focus, the editor text area is visually unfocused
        let editor_focused = focused && !self.param_bar_focused;
        let block = super::panel_block("SQL Editor", editor_focused, theme);

        let inner = block.inner(editor_area);
        frame.render_widget(block, editor_area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let visible_height = inner.height as usize;

        let line_count = self.buffer.len();
        let gutter_digits = line_count.to_string().len();
        let gutter_width = (gutter_digits + 1) as u16;

        if inner.width <= gutter_width {
            return;
        }
        let content_width = inner.width - gutter_width;
        let cw = content_width as usize;

        self.adjust_scroll(visible_height, cw);

        let gutter_style = Style::default()
            .fg(theme.border)
            .add_modifier(Modifier::DIM);

        let mut screen_row: usize = 0;
        let mut buf_line = self.scroll_offset;

        while screen_row < visible_height && buf_line < self.buffer.len() {
            let line_text = &self.buffer[buf_line];
            let vh = visual_line_height(line_text, cw);
            let rows_available = visible_height - screen_row;
            let rows_to_render = vh.min(rows_available);
            let y = inner.y + screen_row as u16;
            let is_cursor_line = focused && buf_line == self.cursor.0;

            render_gutter(
                frame,
                inner.x,
                y,
                gutter_width,
                gutter_digits,
                buf_line + 1,
                rows_to_render,
                is_cursor_line,
                gutter_style,
                theme,
            );

            // Syntax-highlighted content with selection overlay and wrapping
            let mut highlighted = highlight::highlight_line(line_text, theme);
            let (sel_start, sel_end) = self.line_selection_cols(buf_line);
            if sel_start < sel_end {
                highlighted =
                    apply_selection(highlighted, sel_start, sel_end, theme.selected_style);
            } else if sel_start == 0
                && sel_end == 0
                && line_text.is_empty()
                && let Some(((sr, _), (er, _))) = self.selection_bounds()
                && buf_line > sr
                && buf_line < er
            {
                highlighted = Line::from(Span::styled(" ", theme.selected_style));
            }

            let content_area = Rect {
                x: inner.x + gutter_width,
                y,
                width: content_width,
                height: rows_to_render as u16,
            };
            let mut line_widget = Paragraph::new(highlighted).wrap(Wrap { trim: false });
            if is_cursor_line {
                line_widget = line_widget.style(Style::default().bg(theme.active_line_bg));
            }
            frame.render_widget(line_widget, content_area);

            screen_row += rows_to_render;
            buf_line += 1;
        }

        // Set terminal cursor position when focused (but not when param bar has keyboard focus)
        if editor_focused {
            let (row, col) = self.cursor;
            if row >= self.scroll_offset {
                let mut cursor_screen_row: usize = 0;
                for i in self.scroll_offset..row {
                    cursor_screen_row += visual_line_height(&self.buffer[i], cw);
                }
                let (sub_row, sub_col) = cursor_visual_pos(&self.buffer[row], col, cw);
                cursor_screen_row += sub_row;

                if cursor_screen_row < visible_height {
                    let cursor_x = inner.x + gutter_width + sub_col as u16;
                    let cursor_y = inner.y + cursor_screen_row as u16;
                    let max_x = inner.x + gutter_width + content_width - 1;
                    frame.set_cursor_position((cursor_x.min(max_x), cursor_y));
                }
            }
        }

        // Render the parameter bar in the reserved space below the editor.
        if let Some(param_area) = param_area {
            self.render_param_bar(frame, param_area, focused, theme);
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

    // ─── Word-wrap helpers ─────────────────────────────────────────────

    #[test]
    fn test_visual_line_height_empty() {
        assert_eq!(visual_line_height("", 40), 1);
    }

    #[test]
    fn test_visual_line_height_fits() {
        assert_eq!(visual_line_height("hello", 40), 1);
    }

    #[test]
    fn test_visual_line_height_exact_fit() {
        assert_eq!(visual_line_height("12345", 5), 1);
    }

    #[test]
    fn test_visual_line_height_wraps() {
        // 10 chars in width 4 → 3 visual rows (4+4+2)
        assert_eq!(visual_line_height("1234567890", 4), 3);
    }

    #[test]
    fn test_visual_line_height_zero_width() {
        assert_eq!(visual_line_height("hello", 0), 1);
    }

    #[test]
    fn test_cursor_visual_pos_no_wrap() {
        assert_eq!(cursor_visual_pos("hello", 3, 40), (0, 3));
    }

    #[test]
    fn test_cursor_visual_pos_at_wrap_boundary() {
        // Width 5: "12345|67890" — col 5 is first char of second row
        assert_eq!(cursor_visual_pos("1234567890", 5, 5), (1, 0));
    }

    #[test]
    fn test_cursor_visual_pos_second_row() {
        // Width 4: "1234|5678|90" — col 6 is on row 1, display_col 2
        assert_eq!(cursor_visual_pos("1234567890", 6, 4), (1, 2));
    }

    #[test]
    fn test_cursor_visual_pos_end_of_line() {
        // Cursor past last char
        assert_eq!(cursor_visual_pos("abc", 3, 40), (0, 3));
    }

    // ─── Editor tests ────────────────────────────────────────────────

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
        // Ctrl+L now returns Action::ClearEditor; dispatch calls editor.clear()
        editor.clear();
        assert_eq!(editor.contents(), "");
        assert_eq!(editor.cursor, (0, 0));
        assert!(!editor.is_dirty());
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
        assert!(matches!(
            action,
            Some(Action::ExecuteQuery {
                ref sql,
                source: ExecutionSource::FullBuffer,
                source_table: None,
                params: None,
            }) if sql == "SELECT 1"
        ));
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
        // Ctrl+E now opens export popup (not end-of-line)
        let action = editor.handle_key(ctrl_press(KeyCode::Char('e')));
        assert!(matches!(action, Some(Action::ShowExport)));
        // Cursor stays at 0 since Ctrl+E no longer moves it
        assert_eq!(editor.cursor, (0, 0));
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

    // ─── Autocomplete integration tests ──────────────────────────────────

    use crate::app::SchemaCache;
    use std::collections::HashMap;
    use tursotui_db::{ColumnInfo, SchemaEntry};

    fn test_schema() -> SchemaCache {
        SchemaCache {
            entries: vec![
                SchemaEntry {
                    obj_type: "table".into(),
                    name: "users".into(),
                    tbl_name: "users".into(),
                    sql: None,
                },
                SchemaEntry {
                    obj_type: "table".into(),
                    name: "orders".into(),
                    tbl_name: "orders".into(),
                    sql: None,
                },
                SchemaEntry {
                    obj_type: "view".into(),
                    name: "active_users".into(),
                    tbl_name: "active_users".into(),
                    sql: None,
                },
            ],
            columns: HashMap::from([
                (
                    "users".into(),
                    vec![
                        ColumnInfo {
                            name: "id".into(),
                            col_type: "INTEGER".into(),
                            notnull: true,
                            default_value: None,
                            pk: true,
                        },
                        ColumnInfo {
                            name: "name".into(),
                            col_type: "TEXT".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                        ColumnInfo {
                            name: "email".into(),
                            col_type: "TEXT".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                    ],
                ),
                (
                    "orders".into(),
                    vec![
                        ColumnInfo {
                            name: "id".into(),
                            col_type: "INTEGER".into(),
                            notnull: true,
                            default_value: None,
                            pk: true,
                        },
                        ColumnInfo {
                            name: "user_id".into(),
                            col_type: "INTEGER".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                        ColumnInfo {
                            name: "total".into(),
                            col_type: "REAL".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                    ],
                ),
            ]),
            fully_loaded: true,
            fk_info: HashMap::new(),
            row_counts: HashMap::new(),
            custom_types: Vec::new(),
            index_details: HashMap::new(),
        }
    }

    #[test]
    fn trigger_autocomplete_opens_popup() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM ");
        editor.cursor = (0, 14);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());
        let popup = editor.autocomplete_popup.as_ref().unwrap();
        assert!(!popup.is_empty());
        assert_eq!(popup.prefix, "");
    }

    #[test]
    fn trigger_autocomplete_with_prefix_filters() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);
        let popup = editor.autocomplete_popup.as_ref().unwrap();
        assert_eq!(popup.prefix, "us");
        // "users" should match, "orders" should not
        assert_eq!(popup.selected_text(), Some("users"));
    }

    #[test]
    fn trigger_autocomplete_no_matches_closes_popup() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM zzz");
        editor.cursor = (0, 17);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn refresh_autocomplete_updates_candidates() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM u");
        editor.cursor = (0, 15);

        // First trigger to open popup
        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());

        // Simulate typing 's' — update buffer and cursor, then refresh
        editor.buffer[0] = "SELECT * FROM us".into();
        editor.cursor = (0, 16);
        editor.refresh_autocomplete(&schema);

        let popup = editor.autocomplete_popup.as_ref().unwrap();
        assert_eq!(popup.prefix, "us");
        assert_eq!(popup.selected_text(), Some("users"));
    }

    #[test]
    fn refresh_autocomplete_dismisses_when_no_match() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM u");
        editor.cursor = (0, 15);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());

        // Simulate typing so prefix no longer matches anything
        editor.buffer[0] = "SELECT * FROM uzz".into();
        editor.cursor = (0, 17);
        editor.refresh_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn refresh_autocomplete_dismisses_when_prefix_below_min_chars() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_autocomplete_config(true, 2);
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());

        // Simulate backspace: prefix drops to 1 char, below min_chars=2
        editor.buffer[0] = "SELECT * FROM u".into();
        editor.cursor = (0, 15);
        editor.refresh_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn accept_completion_replaces_prefix() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);
        let result = editor.accept_completion();

        assert_eq!(result, Some("users".into()));
        assert_eq!(editor.contents(), "SELECT * FROM users");
        assert_eq!(editor.cursor, (0, 19)); // cursor at end of "users"
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn accept_completion_undoable() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);
        editor.accept_completion();
        assert_eq!(editor.contents(), "SELECT * FROM users");

        // Undo should restore the pre-completion state
        editor.handle_key(ctrl_press(KeyCode::Char('z')));
        assert_eq!(editor.contents(), "SELECT * FROM us");
    }

    #[test]
    fn accept_completion_empty_prefix() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM ");
        editor.cursor = (0, 14);

        editor.trigger_autocomplete(&schema);
        let popup = editor.autocomplete_popup.as_ref().unwrap();
        let first_table = popup.selected_text().unwrap().to_string();

        let result = editor.accept_completion();
        assert_eq!(result, Some(first_table.clone()));
        assert!(editor.contents().ends_with(&first_table));
    }

    #[test]
    fn dismiss_autocomplete_clears_popup() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM ");
        editor.cursor = (0, 14);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());

        editor.dismiss_autocomplete();
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn popup_esc_dismisses_via_handle_key() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM ");
        editor.cursor = (0, 14);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());

        // Esc should dismiss without accepting
        editor.handle_key(press(KeyCode::Esc));
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn popup_tab_accepts_via_handle_key() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);
        let action = editor.handle_key(press(KeyCode::Tab));

        assert!(matches!(action, Some(Action::AcceptCompletion(ref t)) if t == "users"));
        assert!(editor.autocomplete_popup.is_none());
        assert_eq!(editor.contents(), "SELECT * FROM users");
    }

    // ─── Parameter bar tests ──────────────────────────────────────────────

    #[test]
    fn extract_params_positional_and_named() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM t WHERE id = ?1 AND name = :name AND id > ?1");
        let params = editor.extract_params();
        assert_eq!(params, vec!["?1", ":name"]); // deduplicated, order preserved
    }

    #[test]
    fn sync_params_preserves_existing_values() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM t WHERE id = ?1");
        // Set a value
        editor.param_fields = vec![("?1".to_string(), Some("42".to_string()))];
        // Modify SQL to add another param
        editor.set_contents("SELECT * FROM t WHERE id = ?1 AND name = ?2");
        // ?1 should preserve its value, ?2 should be None
        assert_eq!(editor.param_fields.len(), 2);
        assert_eq!(
            editor.param_fields[0],
            ("?1".to_string(), Some("42".to_string()))
        );
        assert_eq!(editor.param_fields[1], ("?2".to_string(), None));
    }

    #[test]
    fn sync_params_clears_when_no_params() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM t WHERE id = ?1");
        assert!(editor.param_bar_active());
        editor.set_contents("SELECT * FROM t");
        assert!(!editor.param_bar_active());
        assert!(editor.param_fields().is_empty());
    }

    #[test]
    fn extract_params_no_params_returns_empty() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM t");
        assert!(editor.extract_params().is_empty());
    }

    #[test]
    fn param_bar_active_after_set_contents_with_params() {
        let mut editor = QueryEditor::new();
        assert!(!editor.param_bar_active());
        editor.set_contents("SELECT * FROM t WHERE id = :id");
        assert!(editor.param_bar_active());
        assert_eq!(editor.param_fields().len(), 1);
        assert_eq!(editor.param_fields()[0].0, ":id");
        assert_eq!(editor.param_fields()[0].1, None);
    }

    #[test]
    fn param_focused_idx_clamped_after_param_removal() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT ?1, ?2, ?3 FROM t");
        editor.param_focused_idx = 2;
        // Remove params by clearing
        editor.set_contents("SELECT ?1 FROM t");
        // index was 2 but only 1 param now — should clamp to 0
        assert_eq!(editor.param_focused_idx(), 0);
    }

    #[test]
    fn param_bar_active_accessors() {
        let editor = QueryEditor::new();
        assert!(!editor.param_bar_active());
        assert!(editor.param_fields().is_empty());
        assert_eq!(editor.param_focused_idx(), 0);
    }

    #[test]
    fn popup_enter_accepts_via_handle_key() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);
        let action = editor.handle_key(press(KeyCode::Enter));

        assert!(matches!(action, Some(Action::AcceptCompletion(ref t)) if t == "users"));
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn popup_up_down_navigates() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM ");
        editor.cursor = (0, 14);

        editor.trigger_autocomplete(&schema);
        let first = editor
            .autocomplete_popup
            .as_ref()
            .unwrap()
            .selected_text()
            .unwrap()
            .to_string();

        // Move down to second candidate
        editor.handle_key(press(KeyCode::Down));
        let second = editor
            .autocomplete_popup
            .as_ref()
            .unwrap()
            .selected_text()
            .unwrap()
            .to_string();
        assert_ne!(first, second);

        // Move back up to first candidate
        editor.handle_key(press(KeyCode::Up));
        let back = editor
            .autocomplete_popup
            .as_ref()
            .unwrap()
            .selected_text()
            .unwrap()
            .to_string();
        assert_eq!(first, back);
    }

    #[test]
    fn popup_char_input_falls_through() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM u");
        editor.cursor = (0, 15);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());

        // Typing a character should fall through to normal insertion
        let action = editor.handle_key(press(KeyCode::Char('s')));
        // The char insertion happens, popup still exists (refresh happens in main.rs)
        assert!(editor.autocomplete_popup.is_some());
        assert_eq!(editor.contents(), "SELECT * FROM us");
        // Returns Consumed to block global key fallback
        assert!(matches!(action, Some(Action::Consumed)));
    }

    #[test]
    fn popup_backspace_falls_through() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);

        // Backspace should fall through — popup stays, char removed
        editor.handle_key(press(KeyCode::Backspace));
        assert!(editor.autocomplete_popup.is_some());
        assert_eq!(editor.contents(), "SELECT * FROM u");
    }

    #[test]
    fn popup_delete_falls_through() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM users WHERE");
        editor.cursor = (0, 19); // after "users" before " WHERE"

        editor.trigger_autocomplete(&schema);

        // Delete key should fall through — popup stays, forward-delete happens
        editor.handle_key(press(KeyCode::Delete));
        assert!(editor.autocomplete_popup.is_some());
        assert_eq!(editor.contents(), "SELECT * FROM usersWHERE");
    }

    #[test]
    fn popup_left_arrow_dismisses() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        editor.trigger_autocomplete(&schema);
        editor.handle_key(press(KeyCode::Left));
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn auto_trigger_opens_popup_when_enabled() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_autocomplete_config(true, 1);
        editor.set_contents("SELECT * FROM u");
        editor.cursor = (0, 15);

        editor.auto_trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());
    }

    #[test]
    fn auto_trigger_respects_min_chars() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_autocomplete_config(true, 3);
        editor.set_contents("SELECT * FROM us");
        editor.cursor = (0, 16);

        // Prefix "us" has 2 chars, min_chars is 3 — should not open
        editor.auto_trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_none());

        // Now with 3 chars
        editor.buffer[0] = "SELECT * FROM use".into();
        editor.cursor = (0, 17);
        editor.auto_trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());
    }

    #[test]
    fn auto_trigger_does_not_open_when_disabled() {
        let mut editor = QueryEditor::new();
        editor.set_autocomplete_config(false, 1);
        assert!(!editor.autocomplete_enabled());

        // When disabled, main.rs won't call auto_trigger_autocomplete —
        // verify the flag is correctly stored and returned.
        editor.set_autocomplete_config(true, 1);
        assert!(editor.autocomplete_enabled());
    }

    #[test]
    fn auto_trigger_no_candidates_does_not_open() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_autocomplete_config(true, 1);
        editor.set_contents("SELECT * FROM zzz");
        editor.cursor = (0, 17);

        editor.auto_trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn accept_completion_multiline_buffer() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT u.na\nFROM users u");
        editor.cursor = (0, 11); // after "u.na"

        editor.trigger_autocomplete(&schema);
        // Should be QualifiedColumn context, suggests "name"
        let popup = editor.autocomplete_popup.as_ref().unwrap();
        assert_eq!(popup.selected_text(), Some("name"));

        let result = editor.accept_completion();
        assert_eq!(result, Some("name".into()));
        assert_eq!(editor.buffer[0], "SELECT u.name");
        assert_eq!(editor.cursor, (0, 13));
    }

    #[test]
    fn trigger_autocomplete_keyword_context() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SEL");
        editor.cursor = (0, 3);

        editor.trigger_autocomplete(&schema);
        let popup = editor.autocomplete_popup.as_ref().unwrap();
        assert_eq!(popup.prefix, "SEL");
        assert_eq!(popup.selected_text(), Some("SELECT"));
    }

    #[test]
    fn trigger_autocomplete_after_as_gives_no_popup() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT name AS ");
        editor.cursor = (0, 15);

        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_none());
    }

    #[test]
    fn full_cycle_trigger_type_accept() {
        let schema = test_schema();
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM ");
        editor.cursor = (0, 14);

        // 1. Trigger autocomplete
        editor.trigger_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());

        // 2. Type 'u' — simulate char insertion + refresh
        editor.handle_key(press(KeyCode::Char('u')));
        editor.refresh_autocomplete(&schema);
        assert!(editor.autocomplete_popup.is_some());
        assert_eq!(editor.autocomplete_popup.as_ref().unwrap().prefix, "u");

        // 3. Type 's' — simulate another char + refresh
        editor.handle_key(press(KeyCode::Char('s')));
        editor.refresh_autocomplete(&schema);
        let popup = editor.autocomplete_popup.as_ref().unwrap();
        assert_eq!(popup.prefix, "us");
        assert_eq!(popup.selected_text(), Some("users"));

        // 4. Accept via Tab
        let action = editor.handle_key(press(KeyCode::Tab));
        assert!(matches!(action, Some(Action::AcceptCompletion(ref t)) if t == "users"));
        assert_eq!(editor.contents(), "SELECT * FROM users");
        assert!(editor.autocomplete_popup.is_none());
    }

    // ─── build_query_params / string_to_value tests ───────────────────────

    #[test]
    fn param_value_conversion_null_vs_empty() {
        let fields = vec![
            ("?1".to_string(), None),
            ("?2".to_string(), Some(String::new())),
            ("?3".to_string(), Some("42".to_string())),
            ("?4".to_string(), Some("1.5".to_string())),
            ("?5".to_string(), Some("hello".to_string())),
        ];
        let params = build_query_params(&fields).unwrap();
        match params {
            tursotui_db::QueryParams::Positional(vals) => {
                assert_eq!(vals.len(), 5);
                assert!(matches!(vals[0], tursotui_db::Value::Null));
                assert!(matches!(&vals[1], tursotui_db::Value::Text(s) if s.is_empty()));
                assert!(matches!(vals[2], tursotui_db::Value::Integer(42)));
                if let tursotui_db::Value::Real(f) = vals[3] {
                    assert!((f - 1.5_f64).abs() < f64::EPSILON);
                } else {
                    panic!("expected Real for 1.5");
                }
                assert!(matches!(&vals[4], tursotui_db::Value::Text(s) if s == "hello"));
            }
            tursotui_db::QueryParams::Named(_) => panic!("expected positional params"),
        }
    }

    #[test]
    fn build_query_params_named() {
        let fields = vec![
            (":name".to_string(), Some("alice".to_string())),
            (":age".to_string(), Some("30".to_string())),
        ];
        let params = build_query_params(&fields).unwrap();
        match params {
            tursotui_db::QueryParams::Named(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].0, ":name");
                assert!(matches!(&pairs[0].1, tursotui_db::Value::Text(s) if s == "alice"));
                assert!(matches!(pairs[1].1, tursotui_db::Value::Integer(30)));
            }
            tursotui_db::QueryParams::Positional(_) => panic!("expected named params"),
        }
    }

    #[test]
    fn build_query_params_empty_returns_none() {
        let fields: Vec<(String, Option<String>)> = vec![];
        assert!(build_query_params(&fields).is_none());
    }

    #[test]
    fn build_query_params_mixed_positional_named_returns_none() {
        let fields = vec![
            ("?1".to_string(), Some("42".to_string())),
            (":name".to_string(), Some("alice".to_string())),
        ];
        // Mixed positional + named is not supported by SQLite — returns None
        assert!(build_query_params(&fields).is_none());
    }

    #[test]
    fn f5_with_param_fields_includes_params() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT * FROM t WHERE id = ?1");
        // Set a value for the param
        editor.param_fields[0].1 = Some("99".to_string());

        let action = editor.handle_key(press(KeyCode::F(5)));
        match action {
            Some(Action::ExecuteQuery {
                params: Some(tursotui_db::QueryParams::Positional(vals)),
                ..
            }) => {
                assert_eq!(vals.len(), 1);
                assert!(matches!(vals[0], tursotui_db::Value::Integer(99)));
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn f5_without_param_fields_has_no_params() {
        let mut editor = QueryEditor::new();
        editor.set_contents("SELECT 1");
        let action = editor.handle_key(press(KeyCode::F(5)));
        assert!(matches!(
            action,
            Some(Action::ExecuteQuery { params: None, .. })
        ));
    }
}
