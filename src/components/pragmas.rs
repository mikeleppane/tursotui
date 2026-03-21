use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, Direction};
use crate::db::PragmaEntry;
use crate::theme::Theme;

use super::Component;

/// Pre-computed layout dimensions for the two-column pragma table.
struct TableLayout {
    name_width: u16,
    value_start: u16,
    value_width: u16,
    content_width: u16,
}

/// Inline text editor state for editing a pragma value.
struct InlineEdit {
    /// Index into the pragmas vec being edited.
    index: usize,
    /// Current text buffer.
    buffer: String,
    /// Cursor position (byte offset into `buffer`, always on a char boundary).
    cursor: usize,
}

/// Validate a proposed value for a writable pragma.
/// Returns `Ok(())` if valid, `Err(message)` if invalid.
/// NOTE: The pragma names here must stay in sync with `db::WRITABLE_PRAGMAS`.
fn validate_pragma_value(name: &str, value: &str) -> Result<(), String> {
    match name {
        "cache_size" | "busy_timeout" => value
            .parse::<i64>()
            .map(|_| ())
            .map_err(|_| format!("{name} must be a number")),
        "synchronous" => {
            if ["0", "1", "2", "3"].contains(&value) {
                Ok(())
            } else {
                Err("synchronous must be 0-3".to_string())
            }
        }
        "temp_store" => {
            if ["0", "1", "2"].contains(&value) {
                Ok(())
            } else {
                Err("temp_store must be 0-2".to_string())
            }
        }
        "foreign_keys" => {
            if ["0", "1"].contains(&value) {
                Ok(())
            } else {
                Err("foreign_keys must be 0 or 1".to_string())
            }
        }
        _ => Err(format!("{name} is not writable")),
    }
}

/// PRAGMA Dashboard with scrollable list and inline editing for writable pragmas.
///
/// Loaded lazily on first Admin tab switch, refreshed on `r`.
/// Writable pragmas can be edited inline with `Enter`, validated, and
/// dispatched via `SetPragma`. A `pragma_in_flight` flag blocks concurrent edits.
pub(crate) struct PragmaDashboard {
    pragmas: Vec<PragmaEntry>,
    selected: usize,
    scroll_offset: usize,
    loading: bool,
    pragma_in_flight: bool,
    /// Index of the pragma whose `set_pragma` is in flight (for DIM styling).
    in_flight_index: Option<usize>,
    editing: Option<InlineEdit>,
}

impl PragmaDashboard {
    pub(crate) fn new() -> Self {
        Self {
            pragmas: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            loading: false,
            pragma_in_flight: false,
            in_flight_index: None,
            editing: None,
        }
    }

    /// Attempt initial load. Returns true (and sets `loading = true`) only if
    /// pragmas haven't been loaded yet and no load is in progress.
    /// Used by `SwitchSubTab(Admin)` for lazy one-time initialization.
    pub(crate) fn try_start_load(&mut self) -> bool {
        if self.loading || !self.pragmas.is_empty() {
            return false;
        }
        self.loading = true;
        true
    }

    /// Force a refresh even when data is already loaded. Returns true (and sets
    /// `loading = true`) if no load is already in progress.
    /// Used by the `r` key (`RefreshPragmas` action).
    pub(crate) fn try_start_refresh(&mut self) -> bool {
        if self.loading {
            return false;
        }
        self.loading = true;
        true
    }

    /// Store loaded pragma entries. Clears any active edit and clamps selection.
    pub(crate) fn set_pragmas(&mut self, entries: Vec<PragmaEntry>) {
        self.editing = None;
        self.pragmas = entries;
        self.loading = false;
        self.selected = self.selected.min(self.pragmas.len().saturating_sub(1));
        self.scroll_offset = 0;
    }

    /// Confirm a successful pragma edit: update the matching entry's value.
    pub(crate) fn confirm_edit(&mut self, name: &str, value: String) {
        if let Some(entry) = self.pragmas.iter_mut().find(|e| e.name == name) {
            entry.value = value;
        }
        self.pragma_in_flight = false;
        self.in_flight_index = None;
    }

    /// Cancel an in-progress edit and clear the in-flight guard.
    /// Called from dispatch on `PragmaFailed`.
    pub(crate) fn cancel_edit(&mut self) {
        self.editing = None;
        self.pragma_in_flight = false;
        self.in_flight_index = None;
    }

    /// Clear only the in-flight guard without touching edit state.
    #[allow(dead_code)] // available for future use; confirm_edit/cancel_edit cover current needs
    pub(crate) fn clear_in_flight(&mut self) {
        self.pragma_in_flight = false;
        self.in_flight_index = None;
    }

    /// Clear only the editing state without affecting the in-flight guard.
    /// Used by `RefreshPragmas` to discard a stale edit buffer while
    /// preserving `pragma_in_flight` for any pending `set_pragma` response.
    pub(crate) fn clear_editing(&mut self) {
        self.editing = None;
    }

    /// Clear loading flag on failure (pragmas stay empty or stale).
    pub(crate) fn set_loading_failed(&mut self) {
        self.loading = false;
    }

    /// Ensure `scroll_offset` keeps `selected` visible within `viewport_height` rows.
    fn clamp_scroll(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }
        // Clamp the sentinel value from `G` key
        if self.scroll_offset == usize::MAX {
            let max_scroll = self.pragmas.len().saturating_sub(viewport_height);
            self.scroll_offset = max_scroll;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selected + 1 - viewport_height;
        }
    }

    /// Render a centered placeholder message.
    fn render_centered(frame: &mut Frame, inner: Rect, msg: &str, theme: &Theme) {
        let msg_width = UnicodeWidthStr::width(msg) as u16;
        let x = inner.x + inner.width.saturating_sub(msg_width) / 2;
        let y = inner.y + inner.height / 2;
        let msg_area = Rect::new(x, y, msg_width.min(inner.width), 1);
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(theme.border)),
            msg_area,
        );
    }

    /// Compute the name column width from the widest pragma name.
    fn name_column_width(&self, max_width: u16) -> u16 {
        self.pragmas
            .iter()
            .map(|p| UnicodeWidthStr::width(p.name.as_str()) as u16)
            .max()
            .unwrap_or(0)
            .min(max_width / 2)
    }

    /// Handle key events when NOT in editing mode.
    fn handle_key_normal(&mut self, key: KeyEvent) -> Option<Action> {
        let last = self.pragmas.len().saturating_sub(1);

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                if self.selected < last {
                    self.selected += 1;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
                self.scroll_offset = 0;
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = last;
                // Sentinel: clamp_scroll will pin to last valid offset on next render().
                self.scroll_offset = usize::MAX;
                None
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.pragma_in_flight {
                    return Some(Action::SetTransient(
                        "Waiting for pragma update...".to_string(),
                        false,
                    ));
                }
                if let Some(entry) = self.pragmas.get(self.selected) {
                    if entry.writable {
                        let buffer = entry.value.clone();
                        let cursor = buffer.len();
                        self.editing = Some(InlineEdit {
                            index: self.selected,
                            buffer,
                            cursor,
                        });
                        None
                    } else {
                        Some(Action::SetTransient("read-only pragma".to_string(), false))
                    }
                } else {
                    None
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => Some(Action::RefreshPragmas),
            (KeyModifiers::NONE, KeyCode::Tab | KeyCode::Esc) => {
                Some(Action::CycleFocus(Direction::Forward))
            }
            _ => None,
        }
    }

    /// Handle key events when in editing mode.
    fn handle_key_editing(&mut self, key: KeyEvent) -> Option<Action> {
        let edit = self.editing.as_mut().expect("called only when editing");

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char(ch)) => {
                edit.buffer.insert(edit.cursor, ch);
                edit.cursor += ch.len_utf8();
                None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                if edit.cursor > 0 {
                    // Find the previous char boundary
                    let prev = edit.buffer[..edit.cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                    edit.buffer.drain(prev..edit.cursor);
                    edit.cursor = prev;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                if edit.cursor < edit.buffer.len() {
                    // Find the next char boundary
                    let next = edit.buffer[edit.cursor..]
                        .char_indices()
                        .nth(1)
                        .map_or(edit.buffer.len(), |(i, _)| edit.cursor + i);
                    edit.buffer.drain(edit.cursor..next);
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                if edit.cursor > 0 {
                    let prev = edit.buffer[..edit.cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                    edit.cursor = prev;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                if edit.cursor < edit.buffer.len() {
                    let next = edit.buffer[edit.cursor..]
                        .char_indices()
                        .nth(1)
                        .map_or(edit.buffer.len(), |(i, _)| edit.cursor + i);
                    edit.cursor = next;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Home) => {
                edit.cursor = 0;
                None
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                edit.cursor = edit.buffer.len();
                None
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let index = edit.index;
                let value = edit.buffer.clone();
                if let Some(entry) = self.pragmas.get(index) {
                    let name = entry.name.clone();
                    match validate_pragma_value(&name, &value) {
                        Ok(()) => {
                            self.in_flight_index = Some(index);
                            self.editing = None;
                            self.pragma_in_flight = true;
                            Some(Action::SetPragma(name, value))
                        }
                        Err(err) => Some(Action::SetTransient(err, true)),
                    }
                } else {
                    // Should not happen -- index was valid at edit start
                    self.editing = None;
                    None
                }
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.editing = None;
                None
            }
            _ => None,
        }
    }

    /// Render a single pragma row (name + value or inline editor).
    fn render_pragma_row(
        &self,
        frame: &mut Frame,
        inner: Rect,
        pragma_idx: usize,
        y: u16,
        layout: &TableLayout,
        theme: &Theme,
    ) {
        let entry = &self.pragmas[pragma_idx];
        let is_selected = pragma_idx == self.selected;
        let is_editing = self.editing.as_ref().is_some_and(|e| e.index == pragma_idx);
        let is_in_flight = self.in_flight_index == Some(pragma_idx);

        // Determine style based on writable/read-only and in-flight state
        let base_style = if !entry.writable {
            Style::default()
                .fg(theme.border)
                .add_modifier(Modifier::DIM)
        } else if is_in_flight {
            // Show DIM while the set_pragma async task is in progress
            Style::default().fg(theme.fg).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(theme.fg)
        };

        // Render name column
        let name_area = Rect::new(inner.x, y, layout.name_width.min(layout.content_width), 1);
        frame.render_widget(
            Paragraph::new(Span::styled(entry.name.as_str(), base_style)),
            name_area,
        );

        // Render value column
        if layout.value_width > 0 {
            let value_area = Rect::new(inner.x + layout.value_start, y, layout.value_width, 1);

            if is_editing {
                if let Some(edit) = &self.editing {
                    Self::render_inline_edit(frame, value_area, edit, theme);
                }
            } else {
                // Build value text, optionally appending note for read-only pragmas
                let value_text = match &entry.note {
                    Some(note) => format!("{} {note}", entry.value),
                    None => entry.value.clone(),
                };

                let value_style = if entry.writable {
                    Style::default().fg(theme.fg)
                } else {
                    base_style
                };

                frame.render_widget(
                    Paragraph::new(Span::styled(value_text, value_style)),
                    value_area,
                );
            }
        }

        // Highlight selected row
        if is_selected {
            let row_area = Rect::new(inner.x, y, layout.content_width, 1);
            frame.buffer_mut().set_style(row_area, theme.selected_style);
        }
    }

    /// Render the inline text editor with cursor indicator.
    fn render_inline_edit(frame: &mut Frame, area: Rect, edit: &InlineEdit, theme: &Theme) {
        let before_cursor = &edit.buffer[..edit.cursor];
        let after_cursor = &edit.buffer[edit.cursor..];

        // The cursor character we insert visually
        let cursor_char = "\u{2502}"; // vertical bar

        let spans = vec![
            Span::styled(
                before_cursor.to_string(),
                Style::default().fg(theme.fg).bg(theme.bg),
            ),
            Span::styled(
                cursor_char,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                after_cursor.to_string(),
                Style::default().fg(theme.fg).bg(theme.bg),
            ),
        ];

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Render the full pragma table content.
    fn render_table_content(&mut self, frame: &mut Frame, inner: Rect, theme: &Theme) {
        let viewport_height = inner.height as usize;
        self.clamp_scroll(viewport_height);

        let has_scrollbar = self.pragmas.len() > viewport_height;
        let content_width = if has_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        let name_width = self.name_column_width(content_width);
        let gap: u16 = 2;
        let value_start = name_width + gap;
        let layout = TableLayout {
            name_width,
            value_start,
            value_width: content_width.saturating_sub(value_start),
            content_width,
        };

        let visible_end = (self.scroll_offset + viewport_height).min(self.pragmas.len());
        for (draw_idx, pragma_idx) in (self.scroll_offset..visible_end).enumerate() {
            let y = inner.y + draw_idx as u16;
            self.render_pragma_row(frame, inner, pragma_idx, y, &layout, theme);
        }

        // Scrollbar
        if has_scrollbar {
            let mut scrollbar_state = ScrollbarState::new(self.pragmas.len())
                .position(self.scroll_offset)
                .viewport_content_length(viewport_height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                inner,
                &mut scrollbar_state,
            );
        }
    }
}

impl Component for PragmaDashboard {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        // If pragmas are empty (not yet loaded), only allow r and Esc
        if self.pragmas.is_empty() {
            return match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Char('r')) => Some(Action::RefreshPragmas),
                (KeyModifiers::NONE, KeyCode::Tab | KeyCode::Esc) => {
                    Some(Action::CycleFocus(Direction::Forward))
                }
                _ => None,
            };
        }

        if self.editing.is_some() {
            // `r` during editing cancels the edit and triggers refresh (spec §5)
            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Char('r') {
                self.editing = None;
                return Some(Action::RefreshPragmas);
            }
            self.handle_key_editing(key)
        } else {
            self.handle_key_normal(key)
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
            .title("PRAGMA Dashboard")
            .title_style(title_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        if self.pragmas.is_empty() {
            if self.loading {
                Self::render_centered(frame, inner, "Loading...", theme);
            } else {
                Self::render_centered(frame, inner, "Press r to load", theme);
            }
            return;
        }

        self.render_table_content(frame, inner, theme);
    }
}
