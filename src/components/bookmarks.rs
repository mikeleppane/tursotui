//! Bookmarks popup overlay.
//!
//! A modal overlay (NOT a Component trait implementor) that shows saved/bookmarked
//! queries with search, name editing, SQL preview with syntax highlighting, and recall/execute.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, QueryAction, UiAction};
use crate::highlight;
use crate::history::BookmarkEntry;
use crate::theme::Theme;

pub(crate) struct BookmarkPanel {
    entries: Vec<BookmarkEntry>,
    /// Indices into `entries` after filtering.
    filtered: Vec<usize>,
    /// Index into `filtered`.
    selected: usize,
    search_buffer: String,
    /// `true` when the search input line is active (typing mode).
    searching: bool,
    preview_scroll: usize,
    /// When `Some`, the name-input line is active (saving or renaming).
    name_input: Option<String>,
    /// Cursor position within `name_input`.
    name_cursor: usize,
    /// SQL captured from the editor when "n" (new bookmark) was pressed.
    pending_sql: Option<String>,
    /// When editing an existing bookmark's name, holds that bookmark's id.
    editing_id: Option<i64>,
    /// Current editor content, set before rendering.
    editor_content: String,
    /// Current database path, used to scope bookmarks.
    database_path: Option<String>,
}

impl BookmarkPanel {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            search_buffer: String::new(),
            searching: false,
            preview_scroll: 0,
            name_input: None,
            name_cursor: 0,
            pending_sql: None,
            editing_id: None,
            editor_content: String::new(),
            database_path: None,
        }
    }

    pub(crate) fn set_database_path(&mut self, path: &str) {
        self.database_path = Some(path.to_string());
    }

    pub(crate) fn set_entries(&mut self, entries: Vec<BookmarkEntry>) {
        self.entries = entries;
        self.refilter();
    }

    pub(crate) fn set_editor_content(&mut self, sql: &str) {
        self.editor_content = sql.to_string();
    }

    /// Rebuild `filtered` from `entries` based on current search state.
    fn refilter(&mut self) {
        let search_lower = self.search_buffer.to_lowercase();
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if search_lower.is_empty() {
                    return true;
                }
                e.name.to_lowercase().contains(&search_lower)
                    || e.sql.to_lowercase().contains(&search_lower)
            })
            .map(|(i, _)| i)
            .collect();

        // Clamp selected
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
        self.preview_scroll = 0;
    }

    // -- Key handling --

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.name_input.is_some() {
            return self.handle_name_key(key);
        }
        if self.searching {
            return self.handle_search_key(key);
        }
        self.handle_normal_key(key)
    }

    fn handle_name_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.name_input = None;
                self.name_cursor = 0;
                self.pending_sql = None;
                self.editing_id = None;
                None
            }
            KeyCode::Enter => {
                let name = self.name_input.take().unwrap_or_default();
                self.name_cursor = 0;
                if name.trim().is_empty() {
                    return None;
                }
                if let Some(id) = self.editing_id.take() {
                    // Rename existing bookmark
                    Some(Action::Query(QueryAction::UpdateBookmark {
                        id,
                        name: name.trim().to_string(),
                    }))
                } else {
                    self.pending_sql.take().map(|sql| {
                        Action::Query(QueryAction::SaveBookmark {
                            name: name.trim().to_string(),
                            sql,
                            database_path: self.database_path.clone(),
                        })
                    })
                }
            }
            KeyCode::Backspace => {
                if let Some(ref mut input) = self.name_input
                    && self.name_cursor > 0
                {
                    let byte_idx = input
                        .char_indices()
                        .nth(self.name_cursor - 1)
                        .map_or(0, |(i, _)| i);
                    let end_idx = input
                        .char_indices()
                        .nth(self.name_cursor)
                        .map_or(input.len(), |(i, _)| i);
                    input.replace_range(byte_idx..end_idx, "");
                    self.name_cursor -= 1;
                }
                None
            }
            KeyCode::Left => {
                self.name_cursor = self.name_cursor.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                if let Some(ref input) = self.name_input {
                    let max = input.chars().count();
                    if self.name_cursor < max {
                        self.name_cursor += 1;
                    }
                }
                None
            }
            KeyCode::Home => {
                self.name_cursor = 0;
                None
            }
            KeyCode::End => {
                if let Some(ref input) = self.name_input {
                    self.name_cursor = input.chars().count();
                }
                None
            }
            KeyCode::Delete => {
                if let Some(ref mut input) = self.name_input {
                    let max = input.chars().count();
                    if self.name_cursor < max {
                        let byte_idx = input
                            .char_indices()
                            .nth(self.name_cursor)
                            .map_or(input.len(), |(i, _)| i);
                        let end_idx = input
                            .char_indices()
                            .nth(self.name_cursor + 1)
                            .map_or(input.len(), |(i, _)| i);
                        input.replace_range(byte_idx..end_idx, "");
                    }
                }
                None
            }
            KeyCode::Char(c) => {
                if let Some(ref mut input) = self.name_input {
                    let byte_idx = input
                        .char_indices()
                        .nth(self.name_cursor)
                        .map_or(input.len(), |(i, _)| i);
                    input.insert(byte_idx, c);
                    self.name_cursor += 1;
                }
                None
            }
            _ => None,
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.search_buffer.clear();
                self.searching = false;
                self.refilter();
                None
            }
            KeyCode::Enter => {
                self.searching = false;
                None
            }
            KeyCode::Backspace => {
                self.search_buffer.pop();
                self.refilter();
                None
            }
            KeyCode::Char(c) => {
                self.search_buffer.push(c);
                self.refilter();
                None
            }
            _ => None,
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Option<Action> {
        match (key.modifiers, key.code) {
            // Navigation
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                if !self.filtered.is_empty() && self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                    self.preview_scroll = 0;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.preview_scroll = 0;
                }
                None
            }

            // Recall into editor
            (KeyModifiers::NONE, KeyCode::Enter) => self
                .selected_sql()
                .map(|s| Action::Query(QueryAction::RecallBookmark(s))),

            // Recall and execute
            (KeyModifiers::NONE, KeyCode::Char('x')) => self
                .selected_sql()
                .map(|s| Action::Query(QueryAction::RecallAndExecuteBookmark(s))),

            // New bookmark from editor content
            (KeyModifiers::NONE, KeyCode::Char('n')) => {
                let sql = self.editor_content.clone();
                if sql.trim().is_empty() {
                    return Some(Action::SetTransient(
                        "Editor is empty -- nothing to bookmark".to_string(),
                        false,
                    ));
                }
                self.pending_sql = Some(sql);
                self.editing_id = None;
                self.name_input = Some(String::new());
                self.name_cursor = 0;
                None
            }

            // Edit (rename) selected bookmark
            (KeyModifiers::NONE, KeyCode::Char('e')) => {
                if let Some(entry) = self.selected_entry() {
                    let name = entry.name.clone();
                    let id = entry.id;
                    self.editing_id = Some(id);
                    self.pending_sql = None;
                    self.name_cursor = name.chars().count();
                    self.name_input = Some(name);
                }
                None
            }

            // Delete
            (KeyModifiers::NONE, KeyCode::Char('d') | KeyCode::Delete) => self
                .selected_entry()
                .map(|e| Action::Query(QueryAction::DeleteBookmark(e.id))),

            // Search mode
            (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.searching = true;
                None
            }

            // Dismiss
            (KeyModifiers::NONE, KeyCode::Esc | KeyCode::F(3)) => {
                Some(Action::Ui(UiAction::ShowBookmarks))
            }

            // Preview scroll
            (KeyModifiers::CONTROL, KeyCode::Down) => {
                if let Some(entry) = self.selected_entry() {
                    let line_count = entry.sql.lines().count();
                    self.preview_scroll = self.preview_scroll.saturating_add(1).min(line_count);
                }
                None
            }
            (KeyModifiers::CONTROL, KeyCode::Up) => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                None
            }

            // Quit
            (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(Action::Quit),

            _ => None,
        }
    }

    fn selected_sql(&self) -> Option<String> {
        self.selected_entry().map(|e| e.sql.clone())
    }

    fn selected_entry(&self) -> Option<&BookmarkEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.entries.get(idx))
    }

    // -- Rendering --

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = super::overlay_block("Bookmarks (F3)", theme);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width < 4 || inner.height < 3 {
            return;
        }

        // If name input is active, reserve a line at top
        let (name_area, content_area) = if self.name_input.is_some() {
            let [name, content] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
            (Some(name), content)
        } else {
            (None, inner)
        };

        // Render name input line
        if let Some(name_area) = name_area {
            self.render_name_input(frame, name_area, theme);
        }

        // Split: bottom status line, then content above
        let [main_area, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(content_area);

        // Split content: left 40% (bookmark list), right 60% (preview)
        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(main_area);

        self.render_list(frame, list_area, theme);
        self.render_preview(frame, preview_area, theme);
        self.render_status(frame, status_area, theme);
    }

    fn render_name_input(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let input = self.name_input.as_deref().unwrap_or("");
        let label = if self.editing_id.is_some() {
            "Rename: "
        } else {
            "Name: "
        };
        // Render cursor at actual position, not always at end
        let before: String = input.chars().take(self.name_cursor).collect();
        let after: String = input.chars().skip(self.name_cursor).collect();
        let display = format!("{label}{before}\u{258E}{after}");
        let line = Paragraph::new(display).style(Style::default().fg(theme.accent));
        frame.render_widget(line, area);
    }

    #[allow(clippy::too_many_lines)]
    fn render_list(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let count = self.filtered.len();
        let total = self.entries.len();
        let title = if count == total {
            format!("Bookmarks ({count})")
        } else {
            format!("Bookmarks ({count}/{total})")
        };
        let block = super::panel_block(&title, true, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Search bar at top (if searching or have search text)
        let list_inner = if self.searching || !self.search_buffer.is_empty() {
            let [search_area, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
            let search_text = if self.searching {
                format!("/{}_", self.search_buffer)
            } else {
                format!("/{}", self.search_buffer)
            };
            let search_line = Paragraph::new(search_text).style(Style::default().fg(theme.accent));
            frame.render_widget(search_line, search_area);
            rest
        } else {
            inner
        };

        if self.filtered.is_empty() {
            let msg = if self.entries.is_empty() {
                "No bookmarks"
            } else {
                "No matching bookmarks"
            };
            let p = Paragraph::new(msg)
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.border));
            frame.render_widget(p, list_inner);
            return;
        }

        let visible_height = list_inner.height as usize;
        let max_width = list_inner.width as usize;

        // Scroll the list to keep the selected item visible
        let list_scroll = if self.selected >= visible_height {
            self.selected.saturating_sub(visible_height - 1)
        } else {
            0
        };

        for (i, &idx) in self
            .filtered
            .iter()
            .skip(list_scroll)
            .take(visible_height)
            .enumerate()
        {
            let y = list_inner.y + i as u16;
            let entry = &self.entries[idx];
            let is_selected = list_scroll + i == self.selected;

            // Show name (bold) + truncated SQL (dimmed)
            let name_part = truncate_to_width(&entry.name, max_width);
            let name_w = UnicodeWidthStr::width(name_part.as_str());
            let remaining = max_width.saturating_sub(name_w + 1);
            let sql_oneliner = entry.sql.lines().next().unwrap_or("").to_string();
            let sql_part = if remaining > 3 {
                format!(" {}", truncate_to_width(&sql_oneliner, remaining))
            } else {
                String::new()
            };

            let mut spans = vec![
                Span::styled(
                    name_part,
                    if is_selected {
                        theme.selected_style.add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
                    },
                ),
                Span::styled(
                    sql_part.clone(),
                    if is_selected {
                        theme.selected_style
                    } else {
                        Style::default().fg(theme.dim)
                    },
                ),
            ];

            // Pad to full width for highlight
            let total_display_w = UnicodeWidthStr::width(spans[0].content.as_ref())
                + UnicodeWidthStr::width(spans[1].content.as_ref());
            if total_display_w < max_width {
                spans.push(Span::styled(
                    " ".repeat(max_width - total_display_w),
                    if is_selected {
                        theme.selected_style
                    } else {
                        Style::default()
                    },
                ));
            }

            let line_area = Rect::new(list_inner.x, y, list_inner.width, 1);
            frame.render_widget(Paragraph::new(Line::from(spans)), line_area);
        }

        // Scrollbar (use inner area, consistent with results.rs)
        if self.filtered.len() > visible_height {
            let scrollbar_area = Rect {
                x: list_inner.x + list_inner.width.saturating_sub(1),
                y: list_inner.y,
                width: 1,
                height: list_inner.height,
            };
            let max_scroll = self.filtered.len().saturating_sub(visible_height);
            let mut scrollbar_state =
                ScrollbarState::new(max_scroll.saturating_add(1)).position(list_scroll);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = super::panel_block("Preview", false, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let Some(entry) = self.selected_entry() else {
            let msg = Paragraph::new("No bookmark selected")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.border));
            frame.render_widget(msg, inner);
            return;
        };

        // Syntax-highlighted SQL lines
        let sql_lines: Vec<Line<'static>> = entry
            .sql
            .lines()
            .map(|line| highlight::highlight_line(line, theme))
            .collect();

        let total_lines = sql_lines.len();
        let visible_height = inner.height as usize;
        let max_scroll = total_lines.saturating_sub(visible_height);
        let clamped_scroll = self.preview_scroll.min(max_scroll);

        let paragraph = Paragraph::new(sql_lines)
            .scroll((u16::try_from(clamped_scroll).unwrap_or(u16::MAX), 0))
            .style(Style::default().fg(theme.fg));
        frame.render_widget(paragraph, inner);

        // Preview scrollbar (use inner area, consistent with results.rs)
        if total_lines > visible_height {
            let scrollbar_area = Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            let mut scrollbar_state =
                ScrollbarState::new(max_scroll.saturating_add(1)).position(clamped_scroll);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }
    }

    fn render_status(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let status = if let Some(entry) = self.selected_entry() {
            format!(
                "Created: {}  Updated: {}",
                entry.created_at, entry.updated_at
            )
        } else {
            String::new()
        };

        let hints = " /search  n:new  e:rename  d:del  x:exec  Enter:recall ";
        let status_width = area.width as usize;
        let hints_w = UnicodeWidthStr::width(hints);
        let status_display = truncate_to_width(&status, status_width.saturating_sub(hints_w));

        let status_w = UnicodeWidthStr::width(status_display.as_str());
        let padding = status_width.saturating_sub(status_w + hints_w);

        let line = Line::from(vec![
            Span::styled(status_display, Style::default().fg(theme.fg)),
            Span::raw(" ".repeat(padding)),
            Span::styled(hints, Style::default().fg(theme.border)),
        ]);

        frame.render_widget(Paragraph::new(line), area);
    }
}

/// Truncate a string to fit within `max_width` display columns.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let full_width = UnicodeWidthStr::width(s);
    if full_width <= max_width {
        return s.to_string();
    }

    // Need to truncate; reserve space for ellipsis
    let target = max_width.saturating_sub(1);
    let mut width = 0;
    let mut result = String::new();
    for ch in s.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > target {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result.push('\u{2026}'); // ellipsis
    result
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEventKind;

    use super::*;

    fn make_bookmark(id: i64, name: &str, sql: &str) -> BookmarkEntry {
        BookmarkEntry {
            id,
            name: name.to_string(),
            sql: sql.to_string(),
            database_path: Some("test.db".to_string()),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
        }
    }

    #[test]
    fn refilter_search() {
        let mut panel = BookmarkPanel::new();
        panel.set_entries(vec![
            make_bookmark(1, "Users query", "SELECT * FROM users"),
            make_bookmark(2, "Product list", "SELECT * FROM products"),
            make_bookmark(3, "User count", "SELECT count(*) FROM users"),
        ]);
        assert_eq!(panel.filtered.len(), 3);

        panel.search_buffer = "user".to_string();
        panel.refilter();
        assert_eq!(panel.filtered.len(), 2); // "Users query" and "User count"
    }

    #[test]
    fn refilter_search_by_sql() {
        let mut panel = BookmarkPanel::new();
        panel.set_entries(vec![
            make_bookmark(1, "Alpha", "SELECT * FROM users"),
            make_bookmark(2, "Beta", "INSERT INTO logs VALUES(1)"),
        ]);
        panel.search_buffer = "insert".to_string();
        panel.refilter();
        assert_eq!(panel.filtered.len(), 1);
        assert_eq!(panel.filtered, vec![1]);
    }

    #[test]
    fn navigate_down_up() {
        let mut panel = BookmarkPanel::new();
        panel.set_entries(vec![
            make_bookmark(1, "A", "SELECT 1"),
            make_bookmark(2, "B", "SELECT 2"),
            make_bookmark(3, "C", "SELECT 3"),
        ]);
        assert_eq!(panel.selected, 0);

        // Move down
        panel.handle_normal_key(KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        assert_eq!(panel.selected, 1);

        // Move up
        panel.handle_normal_key(KeyEvent::new_with_kind(
            KeyCode::Char('k'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn selected_clamps_on_filter() {
        let mut panel = BookmarkPanel::new();
        panel.set_entries(vec![
            make_bookmark(1, "Alpha", "SELECT 1"),
            make_bookmark(2, "Beta", "SELECT 2"),
        ]);
        panel.selected = 1;
        panel.search_buffer = "alpha".to_string();
        panel.refilter();
        assert_eq!(panel.selected, 0);
    }
}
