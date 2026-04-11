//! Query History popup overlay.
//!
//! A modal overlay (NOT a Component trait implementor) that shows recent queries
//! with search/filter, SQL preview with syntax highlighting, and recall/execute.

use std::fmt::Write as _;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, QueryAction, UiAction};
use crate::highlight;
use crate::history::HistoryEntry;
use crate::theme::Theme;

#[allow(clippy::struct_excessive_bools)] // independent boolean states, not a state machine
pub(crate) struct QueryHistoryPanel {
    entries: Vec<HistoryEntry>,
    /// Indices into `entries` after filtering.
    filtered: Vec<usize>,
    /// Index into `filtered`.
    selected: usize,
    loading: bool,
    search_buffer: String,
    /// `true` when the search input line is active (typing mode).
    searching: bool,
    errors_only: bool,
    preview_scroll: usize,
    /// `true` when `history_db` is None (backend unavailable).
    history_unavailable: bool,
    /// Origin filter: when true, show all origins; when false, show user+ddl only.
    show_all_origins: bool,
    /// Show only queries that exceeded the slow-query threshold.
    show_slow_only: bool,
    /// Sort entries by execution time (descending) instead of chronological.
    sort_by_time: bool,
    /// Slow-query threshold in milliseconds (from config).
    slow_threshold_ms: u64,
}

impl QueryHistoryPanel {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            loading: false,
            search_buffer: String::new(),
            searching: false,
            errors_only: false,
            preview_scroll: 0,
            history_unavailable: false,
            show_all_origins: false,
            show_slow_only: false,
            sort_by_time: false,
            slow_threshold_ms: 500,
        }
    }

    /// Update the slow-query threshold (called when config is loaded).
    pub(crate) const fn set_slow_threshold(&mut self, ms: u64) {
        self.slow_threshold_ms = ms;
    }

    pub(crate) const fn set_unavailable(&mut self) {
        self.history_unavailable = true;
    }

    pub(crate) const fn set_loading(&mut self) {
        self.loading = true;
    }

    pub(crate) fn set_entries(&mut self, entries: Vec<HistoryEntry>) {
        self.entries = entries;
        self.refilter();
        self.loading = false;
    }

    /// Rebuild `filtered` from `entries` based on current search/filter state.
    fn refilter(&mut self) {
        let search_lower = self.search_buffer.to_lowercase();
        let threshold = self.slow_threshold_ms;
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if self.errors_only && !e.is_error() {
                    return false;
                }
                if self.show_slow_only {
                    let is_slow = e.execution_time_ms.is_some_and(|ms| ms > threshold);
                    if !is_slow {
                        return false;
                    }
                }
                // Origin filter: default (user+ddl) hides pragma/other origins
                if !self.show_all_origins && e.origin != "user" && e.origin != "ddl" {
                    return false;
                }
                if !search_lower.is_empty() && !e.sql.to_lowercase().contains(&search_lower) {
                    return false;
                }
                true
            })
            .map(|(i, _)| i)
            .collect();

        // Sort by execution time (descending) if enabled
        if self.sort_by_time {
            self.filtered.sort_by(|&a, &b| {
                let ta = self.entries[a].execution_time_ms.unwrap_or(0);
                let tb = self.entries[b].execution_time_ms.unwrap_or(0);
                tb.cmp(&ta)
            });
        }

        // Clamp selected
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
        self.preview_scroll = 0;
    }

    // -- Filter getters for dispatch --

    #[allow(clippy::unused_self)] // will read self.db_filter when M8 lands
    pub(crate) const fn db_filter_value(&self) -> Option<&str> {
        None // deferred to M8
    }

    #[allow(clippy::unused_self)] // will use self.show_all_origins for server-side filter in M8
    pub(crate) const fn origin_filter(&self) -> Option<&str> {
        // Origin filtering is done client-side in refilter() because the default
        // filter (user+ddl) needs IN ('user', 'ddl') which the current API doesn't support.
        None
    }

    pub(crate) fn search_text(&self) -> Option<&str> {
        if self.search_buffer.is_empty() {
            None
        } else {
            Some(&self.search_buffer)
        }
    }

    pub(crate) const fn errors_only(&self) -> bool {
        self.errors_only
    }

    // -- Key handling --

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.searching {
            return self.handle_search_key(key);
        }
        self.handle_normal_key(key)
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                // Clear search and exit search mode
                self.search_buffer.clear();
                self.searching = false;
                self.refilter();
                None
            }
            KeyCode::Enter => {
                // Accept search, exit search mode
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
                self.move_selection_down();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.move_selection_up();
                None
            }

            // Recall into editor
            (KeyModifiers::NONE, KeyCode::Enter) => self
                .selected_sql()
                .map(|s| Action::Query(QueryAction::RecallHistory(s))),

            // Recall and execute
            (KeyModifiers::SHIFT, KeyCode::Enter) => self
                .selected_sql()
                .map(|s| Action::Query(QueryAction::RecallAndExecute(s))),

            // Dismiss
            (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::CONTROL, KeyCode::Char('h')) => {
                Some(Action::Ui(UiAction::ShowHistory))
            }

            // Search mode
            (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.searching = true;
                None
            }

            // Cycle origin filter: user+ddl (default) → all → user+ddl
            (KeyModifiers::NONE, KeyCode::Tab) => {
                self.show_all_origins = !self.show_all_origins;
                self.refilter();
                None
            }

            // Toggle errors-only filter
            (KeyModifiers::NONE, KeyCode::Char('e')) => {
                self.errors_only = !self.errors_only;
                self.refilter();
                None
            }

            // Toggle slow-queries-only filter
            (KeyModifiers::NONE, KeyCode::Char('s')) => {
                self.show_slow_only = !self.show_slow_only;
                self.refilter();
                None
            }

            // Toggle sort by execution time (descending)
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('S')) => {
                self.sort_by_time = !self.sort_by_time;
                self.refilter();
                None
            }

            // Delete entry
            (KeyModifiers::NONE, KeyCode::Char('d')) => self.selected_entry().map(|e| {
                let id = e.id;
                Action::Query(QueryAction::DeleteHistoryEntry(id))
            }),

            // Copy SQL to clipboard
            (KeyModifiers::NONE, KeyCode::Char('y')) => {
                if let Some(sql) = self.selected_sql() {
                    match set_clipboard(&sql) {
                        Ok(()) => Some(Action::SetTransient(
                            "Copied to clipboard".to_string(),
                            false,
                        )),
                        Err(e) => Some(Action::SetTransient(format!("Clipboard error: {e}"), true)),
                    }
                } else {
                    None
                }
            }

            // Preview scroll (clamped to entry line count)
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

    fn selected_entry(&self) -> Option<&HistoryEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.entries.get(idx))
    }

    /// Move selection down, skipping duplicate entries within a group.
    fn move_selection_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let current_sql = self.selected_entry().map(|e| e.sql.as_str());
        let mut next = self.selected + 1;
        // Skip entries with the same SQL (duplicate group)
        while next < self.filtered.len() {
            let idx = self.filtered[next];
            if Some(self.entries[idx].sql.as_str()) != current_sql {
                break;
            }
            next += 1;
        }
        if next < self.filtered.len() {
            self.selected = next;
            self.preview_scroll = 0;
        }
    }

    /// Move selection up, skipping duplicate entries within a group.
    fn move_selection_up(&mut self) {
        if self.filtered.is_empty() || self.selected == 0 {
            return;
        }
        // First, skip backward past any entries with the same SQL as current
        let current_sql = self.selected_entry().map(|e| e.sql.as_str());
        let mut prev = self.selected.saturating_sub(1);
        while prev > 0 {
            let idx = self.filtered[prev];
            if Some(self.entries[idx].sql.as_str()) != current_sql {
                break;
            }
            prev = prev.saturating_sub(1);
        }
        // Now `prev` is in the previous group. Find the START of that group.
        let target_sql = self.entries[self.filtered[prev]].sql.as_str();
        while prev > 0 {
            let before = self.filtered[prev - 1];
            if self.entries[before].sql.as_str() != target_sql {
                break;
            }
            prev -= 1;
        }
        self.selected = prev;
        self.preview_scroll = 0;
    }

    // -- Rendering --

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // 80% width, 80% height, centered
        let popup_width = area.width * 80 / 100;
        let popup_height = area.height * 80 / 100;
        let x = (area.width.saturating_sub(popup_width)) / 2;
        let y = (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        // Clear background
        frame.render_widget(Clear, popup_area);

        let block = super::overlay_block("Query History", theme);

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if inner.width < 4 || inner.height < 3 {
            return;
        }

        // Unavailable state
        if self.history_unavailable {
            let msg = Paragraph::new("History unavailable \u{2014} see status bar for details")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.error));
            frame.render_widget(msg, inner);
            return;
        }

        // Loading state
        if self.loading && self.entries.is_empty() {
            let msg = Paragraph::new("Loading...")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.fg));
            frame.render_widget(msg, inner);
            return;
        }

        // Split: bottom status line, then content above
        let [content_area, status_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

        // Split content: left 40% (query list), right 60% (preview)
        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(content_area);

        self.render_list(frame, list_area, theme);
        self.render_preview(frame, preview_area, theme);
        self.render_status(frame, status_area, theme);
    }

    fn render_list(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let title = self.list_title();
        let block = super::panel_block(&title, true, theme);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Search bar at top (if searching or have search text)
        let (list_inner, search_height) = if self.searching || !self.search_buffer.is_empty() {
            let [search_area, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
            let search_text = if self.searching {
                format!("/{}_", self.search_buffer)
            } else {
                format!("/{}", self.search_buffer)
            };
            let search_line = Paragraph::new(search_text).style(Style::default().fg(theme.accent));
            frame.render_widget(search_line, search_area);
            (rest, 1)
        } else {
            (inner, 0)
        };

        if self.filtered.is_empty() {
            let msg = if self.entries.is_empty() {
                "No history"
            } else {
                "No matching entries"
            };
            let p = Paragraph::new(msg)
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.border));
            frame.render_widget(p, list_inner);
            return;
        }

        let visible_height = list_inner.height as usize;

        // Build display items (grouped by consecutive duplicate SQL)
        let display_items = self.build_display_items();

        // Determine which display item is selected
        let selected_display_idx = display_items
            .iter()
            .position(|item| item.filtered_idx == self.selected)
            .unwrap_or(0);

        // Scroll the list to keep the selected item visible
        let list_scroll = if selected_display_idx >= visible_height {
            selected_display_idx.saturating_sub(visible_height - 1)
        } else {
            0
        };

        // Render visible items
        let max_width = list_inner.width as usize;
        for (i, item) in display_items
            .iter()
            .skip(list_scroll)
            .take(visible_height)
            .enumerate()
        {
            let y = list_inner.y + i as u16;
            let is_selected = item.filtered_idx == self.selected;

            let mut display = truncate_to_width(&item.label, max_width);

            // Pad to full width for highlight
            let display_width = UnicodeWidthStr::width(display.as_str());
            if display_width < max_width {
                display.push_str(&" ".repeat(max_width - display_width));
            }

            let style = if is_selected {
                theme.selected_style
            } else if item.is_error {
                Style::default().fg(theme.error)
            } else {
                Style::default().fg(theme.fg)
            };

            let line_area = Rect::new(list_inner.x, y, list_inner.width, 1);
            let span = Span::styled(display, style);
            frame.render_widget(Paragraph::new(Line::from(span)), line_area);
        }

        // Scrollbar
        let _ = search_height;
        if display_items.len() > visible_height {
            let scrollbar_area = Rect {
                x: area.x + area.width - 1,
                y: list_inner.y,
                width: 1,
                height: list_inner.height,
            };
            let max_scroll = display_items.len().saturating_sub(visible_height);
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
            let msg = Paragraph::new("No entry selected")
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

        // Preview scrollbar
        if total_lines > visible_height {
            let scrollbar_area = Rect {
                x: area.x + area.width - 1,
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
            let mut parts = Vec::new();
            parts.push(entry.timestamp.clone());
            if let Some(ms) = entry.execution_time_ms {
                parts.push(format!("{ms}ms"));
            }
            if let Some(rows) = entry.row_count {
                parts.push(format!("{rows} rows"));
            }
            if let Some(ref err) = entry.error_message {
                let err_preview = truncate_to_width(err, 40);
                parts.push(format!("ERR: {err_preview}"));
            }
            parts.push(entry.database_path.clone());
            parts.join(" | ")
        } else {
            String::new()
        };

        let origin_label = if self.show_all_origins {
            "all"
        } else {
            "user+ddl"
        };
        let slow_label = if self.show_slow_only { "on" } else { "off" };
        let sort_label = if self.sort_by_time { "time" } else { "chrono" };
        let hints = format!(
            " /search  Tab:{origin_label}  e:errors  s:slow({slow_label})  S:sort({sort_label})  d:del  y:copy  Enter:recall "
        );
        let status_width = area.width as usize;
        let hints_w = UnicodeWidthStr::width(hints.as_str());
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

    fn list_title(&self) -> String {
        let count = self.filtered.len();
        let total = self.entries.len();
        if count == total {
            format!("Queries ({count})")
        } else {
            format!("Queries ({count}/{total})")
        }
    }

    /// Build display items from filtered entries, grouping consecutive duplicates.
    fn build_display_items(&self) -> Vec<DisplayItem> {
        let mut items = Vec::new();
        let mut i = 0;
        while i < self.filtered.len() {
            let idx = self.filtered[i];
            let entry = &self.entries[idx];
            let sql = &entry.sql;

            // Count consecutive duplicates
            let mut count = 1usize;
            let mut j = i + 1;
            while j < self.filtered.len() {
                let next_idx = self.filtered[j];
                if self.entries[next_idx].sql == *sql {
                    count += 1;
                    j += 1;
                } else {
                    break;
                }
            }

            // Build label: first line of SQL + optional badges
            let first_line = sql.lines().next().unwrap_or("");
            let is_slow = entry
                .execution_time_ms
                .is_some_and(|ms| ms > self.slow_threshold_ms);
            let mut label = if is_slow {
                format!("\u{23f1} {first_line}")
            } else {
                first_line.to_string()
            };

            if entry.is_error() {
                label.push_str(" [err]");
            }
            if count > 1 {
                let _ = write!(label, " \u{00d7}{count}");
            }

            items.push(DisplayItem {
                filtered_idx: i,
                label,
                is_error: entry.is_error(),
            });

            i = j;
        }
        items
    }
}

/// A rendered list item with grouping info.
struct DisplayItem {
    /// Index into `filtered` for the first entry in this group.
    filtered_idx: usize,
    label: String,
    is_error: bool,
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
    let target = max_width.saturating_sub(1); // 1 char for the ellipsis
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

/// Copy text to the system clipboard via `arboard`.
fn set_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: i64, sql: &str, origin: &str) -> HistoryEntry {
        HistoryEntry {
            id,
            sql: sql.to_string(),
            database_path: "test.db".to_string(),
            timestamp: "2026-01-01".to_string(),
            execution_time_ms: Some(10),
            row_count: Some(5),
            error_message: None,
            origin: origin.to_string(),
            params_json: None,
        }
    }

    fn make_error_entry(id: i64, sql: &str) -> HistoryEntry {
        HistoryEntry {
            error_message: Some("syntax error".to_string()),
            ..make_entry(id, sql, "user")
        }
    }

    #[test]
    fn truncate_to_width_short() {
        assert_eq!(truncate_to_width("abc", 10), "abc");
    }

    #[test]
    fn truncate_to_width_exact() {
        assert_eq!(truncate_to_width("abcde", 5), "abcde");
    }

    #[test]
    fn truncate_to_width_long() {
        let result = truncate_to_width("abcdefgh", 5);
        assert_eq!(result, "abcd\u{2026}"); // 4 chars + ellipsis
    }

    #[test]
    fn truncate_to_width_empty() {
        assert_eq!(truncate_to_width("abc", 0), "");
    }

    #[test]
    fn truncate_to_width_unicode() {
        // "café" — é is 1 display column
        assert_eq!(truncate_to_width("café", 10), "café");
        let result = truncate_to_width("café test", 5);
        assert_eq!(result, "caf\u{00e9}\u{2026}"); // 4 chars + ellipsis
    }

    #[test]
    fn refilter_search() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "INSERT INTO t", "user"),
            make_entry(3, "SELECT 2", "user"),
        ]);
        assert_eq!(panel.filtered.len(), 3);

        panel.search_buffer = "select".to_string();
        panel.refilter();
        assert_eq!(panel.filtered.len(), 2);
        assert_eq!(panel.filtered, vec![0, 2]);
    }

    #[test]
    fn refilter_errors_only() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_error_entry(2, "SELEC 1"),
            make_entry(3, "SELECT 2", "user"),
        ]);
        panel.errors_only = true;
        panel.refilter();
        assert_eq!(panel.filtered.len(), 1);
        assert_eq!(panel.filtered, vec![1]);
    }

    #[test]
    fn refilter_origin_filter() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "PRAGMA table_info", "pragma"),
            make_entry(3, "CREATE TABLE t", "ddl"),
        ]);
        // Default: user+ddl only
        assert_eq!(panel.filtered.len(), 2);
        assert_eq!(panel.filtered, vec![0, 2]);

        // Show all origins
        panel.show_all_origins = true;
        panel.refilter();
        assert_eq!(panel.filtered.len(), 3);
    }

    #[test]
    fn build_display_items_groups_duplicates() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "SELECT 1", "user"),
            make_entry(3, "SELECT 1", "user"),
            make_entry(4, "SELECT 2", "user"),
        ]);
        let items = panel.build_display_items();
        assert_eq!(items.len(), 2);
        assert!(items[0].label.contains("\u{00d7}3")); // ×3
        assert!(!items[1].label.contains("\u{00d7}")); // no badge
    }

    #[test]
    fn move_selection_down_skips_duplicates() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "SELECT 1", "user"),
            make_entry(3, "SELECT 2", "user"),
        ]);
        assert_eq!(panel.selected, 0);
        panel.move_selection_down();
        assert_eq!(panel.selected, 2); // skipped index 1
    }

    #[test]
    fn move_selection_up_lands_on_group_start() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "SELECT 1", "user"),
            make_entry(3, "SELECT 1", "user"),
            make_entry(4, "SELECT 2", "user"),
        ]);
        panel.selected = 3; // on "SELECT 2"
        panel.move_selection_up();
        // Should land on index 0 (start of "SELECT 1" group), not index 2
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn move_selection_up_from_second_group() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "A", "user"),
            make_entry(2, "B", "user"),
            make_entry(3, "B", "user"),
            make_entry(4, "C", "user"),
        ]);
        panel.selected = 3; // on "C"
        panel.move_selection_up();
        assert_eq!(panel.selected, 1); // start of "B" group
        panel.move_selection_up();
        assert_eq!(panel.selected, 0); // "A"
    }

    #[test]
    fn selected_clamps_on_filter() {
        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "INSERT 1", "user"),
        ]);
        panel.selected = 1;
        panel.search_buffer = "SELECT".to_string();
        panel.refilter();
        assert_eq!(panel.selected, 0); // clamped
    }

    #[test]
    fn search_key_appends_and_filters() {
        use ratatui::crossterm::event::KeyEventKind;

        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "DELETE FROM t", "user"),
        ]);
        panel.searching = true;
        // Type "sel" to narrow to SELECT only
        for c in ['s', 'e', 'l'] {
            panel.handle_search_key(KeyEvent::new_with_kind(
                KeyCode::Char(c),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            ));
        }
        assert_eq!(panel.search_buffer, "sel");
        assert_eq!(panel.filtered.len(), 1); // only SELECT matches
    }

    #[test]
    fn search_esc_clears() {
        use ratatui::crossterm::event::KeyEventKind;

        let mut panel = QueryHistoryPanel::new();
        panel.set_entries(vec![
            make_entry(1, "SELECT 1", "user"),
            make_entry(2, "INSERT 1", "user"),
        ]);
        panel.searching = true;
        panel.search_buffer = "SELECT".to_string();
        panel.refilter();
        assert_eq!(panel.filtered.len(), 1);

        panel.handle_search_key(KeyEvent::new_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        assert!(!panel.searching);
        assert!(panel.search_buffer.is_empty());
        assert_eq!(panel.filtered.len(), 2); // restored
    }
}
