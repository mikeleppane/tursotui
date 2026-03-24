//! Go to Object popup overlay.
//!
//! Fuzzy search across all open databases' schema objects (tables, indexes,
//! views, triggers, columns). Provides instant filtering with ranked results
//! and navigates to the selected object in the schema explorer.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::app::{Action, DatabaseContext, ObjectKind, ObjectRef};
use crate::theme::Theme;

/// Maximum number of results to display.
const MAX_RESULTS: usize = 50;

#[derive(Debug, Clone)]
pub(crate) struct ObjectMatch {
    pub(crate) name: String,
    pub(crate) kind: ObjectKind,
    /// Container or context for this object. Table name for columns,
    /// base storage type for custom types (e.g. "blob" for uuid).
    pub(crate) parent: Option<String>,
    pub(crate) database_path: String,
    pub(crate) database_label: String,
    pub(crate) score: u32,
}

pub(crate) struct GoToObject {
    query: String,
    cursor: usize,
    results: Vec<ObjectMatch>,
    selected: usize,
    scroll_offset: usize,
}

/// Helper: get byte index for the nth char in a string.
fn char_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

impl GoToObject {
    /// Create a new Go to Object popup, pre-populated with initial results.
    pub(crate) fn new(databases: &[DatabaseContext], active_db_path: &str) -> Self {
        let mut goto = Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
            scroll_offset: 0,
        };
        goto.update_results(databases, active_db_path);
        goto
    }

    /// Rebuild results from all databases based on current query.
    pub(crate) fn update_results(&mut self, databases: &[DatabaseContext], active_db_path: &str) {
        let mut candidates = build_candidates(databases);
        score_and_sort(&mut candidates, &self.query, active_db_path);
        self.results = candidates;
        self.scroll_offset = 0;
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
    }

    /// Handle a key event. Returns `Some(Action)` if the key produced a state change.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        databases: &[DatabaseContext],
        active_db_path: &str,
    ) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            // Dismiss: Esc or Ctrl+P toggle
            (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::CONTROL, KeyCode::Char('p')) => {
                Some(Action::OpenGoToObject) // toggles overlay off via update()
            }

            // Navigate results
            (KeyModifiers::NONE, KeyCode::Up) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.scroll_offset {
                        self.scroll_offset = self.selected;
                    }
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if !self.results.is_empty() && self.selected + 1 < self.results.len() {
                    self.selected += 1;
                    // render() will fine-tune the downward scroll adjustment
                }
                None
            }

            // Select result
            (KeyModifiers::NONE, KeyCode::Enter) => self.results.get(self.selected).map(|m| {
                Action::GoToObject(ObjectRef {
                    name: m.name.clone(),
                    kind: m.kind,
                    database_path: m.database_path.clone(),
                })
            }),

            // Backspace: delete char before cursor
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                if self.cursor > 0 {
                    let idx = char_byte_index(&self.query, self.cursor - 1);
                    let end = char_byte_index(&self.query, self.cursor);
                    self.query.replace_range(idx..end, "");
                    self.cursor -= 1;
                    self.update_results(databases, active_db_path);
                }
                None
            }

            // Delete: remove char at cursor
            (KeyModifiers::NONE, KeyCode::Delete) => {
                let char_count = self.query.chars().count();
                if self.cursor < char_count {
                    let idx = char_byte_index(&self.query, self.cursor);
                    let end = char_byte_index(&self.query, self.cursor + 1);
                    self.query.replace_range(idx..end, "");
                    self.update_results(databases, active_db_path);
                }
                None
            }

            // Home/End for cursor movement
            (KeyModifiers::NONE, KeyCode::Home) => {
                self.cursor = 0;
                None
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                self.cursor = self.query.chars().count();
                None
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                let char_count = self.query.chars().count();
                if self.cursor < char_count {
                    self.cursor += 1;
                }
                None
            }

            // All printable chars go to input (no j/k navigation)
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                let byte_pos = char_byte_index(&self.query, self.cursor);
                self.query.insert(byte_pos, c);
                self.cursor += 1;
                self.update_results(databases, active_db_path);
                None
            }

            _ => None,
        }
    }

    /// Render the Go to Object overlay.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Calculate popup dimensions
        let popup_width = {
            let pct = (u32::from(area.width) * 70 / 100) as u16;
            pct.clamp(50.min(area.width), 90.min(area.width))
        };
        let max_height = (u32::from(area.height) * 50 / 100) as u16;
        // Input line + at least 2 result rows, up to result count + 1 (for input)
        let desired_rows = (self.results.len() as u16).saturating_add(1);
        let popup_height = desired_rows
            .clamp(3, max_height)
            .min(area.height.saturating_sub(2));

        let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let block = super::overlay_block("Go to Object", theme);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Input line
        let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let input_display = format!("> {}", self.query);
        let input_para = Paragraph::new(input_display).style(Style::default().fg(theme.fg));
        frame.render_widget(input_para, input_area);

        // Set cursor position (after "> " prefix)
        let cursor_x = inner.x + 2 + self.cursor as u16;
        frame.set_cursor_position((cursor_x, inner.y));

        // Results area
        let visible_rows = inner.height.saturating_sub(1) as usize;
        if visible_rows == 0 || self.results.is_empty() {
            if self.results.is_empty() && !self.query.is_empty() {
                let no_results =
                    Paragraph::new("  No matches").style(Style::default().fg(theme.dim));
                let no_area = Rect::new(inner.x, inner.y + 1, inner.width, 1);
                frame.render_widget(no_results, no_area);
            }
            return;
        }

        // Adjust scroll_offset for rendering (don't mutate self since &self)
        let scroll = {
            let mut s = self.scroll_offset;
            if self.selected < s {
                s = self.selected;
            } else if self.selected >= s + visible_rows {
                s = self.selected - visible_rows + 1;
            }
            let max_scroll = self.results.len().saturating_sub(visible_rows);
            s.min(max_scroll)
        };

        let results_area_width = inner.width as usize;

        for (i, m) in self
            .results
            .iter()
            .skip(scroll)
            .take(visible_rows)
            .enumerate()
        {
            let row_y = inner.y + 1 + i as u16;
            let is_selected = scroll + i == self.selected;
            let row_area = Rect::new(inner.x, row_y, inner.width, 1);

            let sel_fg = theme.selected_style.fg.unwrap_or(theme.bg);

            if is_selected {
                let highlight = Style::default()
                    .fg(sel_fg)
                    .bg(theme.selected_style.bg.unwrap_or(theme.accent));
                frame.render_widget(
                    Paragraph::new(" ".repeat(results_area_width)).style(highlight),
                    row_area,
                );
            }

            let icon = kind_icon(m.kind);
            let icon_style = if is_selected {
                Style::default().fg(sel_fg).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(kind_color(m.kind, theme))
                    .add_modifier(Modifier::BOLD)
            };

            let name_style = if is_selected {
                Style::default().fg(sel_fg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
            };

            let dim_style = if is_selected {
                Style::default().fg(sel_fg)
            } else {
                Style::default().fg(theme.dim)
            };

            let type_label = kind_label(m.kind);
            let parent_text = m
                .parent
                .as_ref()
                .map_or_else(String::new, |p| format!("  {p}"));
            let db_label = format!("  [{}]", m.database_label);

            // Build spans
            let mut spans = vec![
                Span::styled(format!("  {icon} "), icon_style),
                Span::styled(m.name.clone(), name_style),
                Span::styled(format!("  {type_label}"), dim_style),
            ];
            if !parent_text.is_empty() {
                spans.push(Span::styled(parent_text, dim_style));
            }
            spans.push(Span::styled(db_label, dim_style));

            let line = Line::from(spans);
            frame.render_widget(Paragraph::new(line), row_area);
        }

        // Scrollbar
        if self.results.len() > visible_rows {
            let scrollbar_area = Rect::new(
                popup_area.x + popup_area.width - 1,
                inner.y + 1,
                1,
                visible_rows as u16,
            );
            let mut scrollbar_state = ScrollbarState::new(self.results.len())
                .position(scroll)
                .viewport_content_length(visible_rows);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some(" "))
                    .thumb_style(Style::default().fg(theme.border_focused)),
                scrollbar_area,
                &mut scrollbar_state,
            );
        }
    }
}

/// Build candidates from all databases' schema caches.
fn build_candidates(databases: &[DatabaseContext]) -> Vec<ObjectMatch> {
    let mut candidates = Vec::new();

    for db in databases {
        let cache = &db.schema_cache;

        // Tables, views, indexes, triggers from entries
        for entry in &cache.entries {
            let kind = match entry.obj_type.as_str() {
                "table" => ObjectKind::Table,
                "view" => ObjectKind::View,
                "index" => ObjectKind::Index,
                "trigger" => ObjectKind::Trigger,
                _ => continue,
            };
            candidates.push(ObjectMatch {
                name: entry.name.clone(),
                kind,
                parent: None,
                database_path: db.path.clone(),
                database_label: db.label.clone(),
                score: 0,
            });
        }

        // Custom types from `schema_cache.custom_types`
        for ct in &cache.custom_types {
            candidates.push(ObjectMatch {
                name: ct.name.clone(),
                kind: ObjectKind::CustomType,
                parent: Some(ct.parent.clone()),
                database_path: db.path.clone(),
                database_label: db.label.clone(),
                score: 0,
            });
        }

        // Columns from `schema_cache.columns` (NOT from entries)
        for (table_name, columns) in &cache.columns {
            for col in columns {
                candidates.push(ObjectMatch {
                    name: col.name.clone(),
                    kind: ObjectKind::Column,
                    parent: Some(table_name.clone()),
                    database_path: db.path.clone(),
                    database_label: db.label.clone(),
                    score: 0,
                });
            }
        }
    }

    candidates
}

/// Compute fuzzy match score for a name against a query.
/// Returns 0 if no match.
pub(crate) fn fuzzy_score(name: &str, query: &str) -> u32 {
    if query.is_empty() {
        // Empty query matches everything
        return 100;
    }

    let name_lower = name.to_lowercase();
    let query_lower = query.to_lowercase();

    // Exact prefix match: name starts with query
    if name_lower.starts_with(&query_lower) {
        return 1000;
    }

    // Word-boundary match: each character of the query starts a word segment.
    // Words are split on `_` or transitions. We check if query chars appear
    // at word boundaries in order.
    if word_boundary_match(&name_lower, &query_lower) {
        return 500;
    }

    // Plain substring match
    if name_lower.contains(&query_lower) {
        return 100;
    }

    0
}

/// Check if query characters match at word boundaries in name.
/// Words are separated by `_`. For example, `ur` matches `user_roles` (`u` at
/// start of `user`, `r` at start of `roles`).
fn word_boundary_match(name: &str, query: &str) -> bool {
    let segments: Vec<&str> = name.split('_').collect();
    let mut query_chars = query.chars().peekable();

    for segment in &segments {
        if query_chars.peek().is_none() {
            break;
        }
        if segment
            .chars()
            .next()
            .is_some_and(|sc| query_chars.peek() == Some(&sc))
        {
            query_chars.next();
        }
    }

    query_chars.peek().is_none()
}

/// Score and sort candidates based on query and active database.
pub(crate) fn score_and_sort(candidates: &mut Vec<ObjectMatch>, query: &str, active_db_path: &str) {
    for m in candidates.iter_mut() {
        let base = fuzzy_score(&m.name, query);
        if base == 0 && !query.is_empty() {
            m.score = 0;
            continue;
        }

        let mut score = base;

        // Table/View bonus
        if matches!(m.kind, ObjectKind::Table | ObjectKind::View) {
            score += 50;
        }

        // Active database bonus
        if m.database_path == active_db_path {
            score += 200;
        }

        // Column penalty
        if m.kind == ObjectKind::Column {
            score = score.saturating_sub(50);
        }

        m.score = score;
    }

    // Remove non-matches when there's an actual query
    if query.is_empty() {
        // Empty query: show only tables and views (not indexes, triggers, columns)
        candidates.retain(|m| matches!(m.kind, ObjectKind::Table | ObjectKind::View));
    } else {
        candidates.retain(|m| m.score > 0);
    }

    // Sort descending by score, then alphabetically by name for stability
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    // Cap at `MAX_RESULTS`
    candidates.truncate(MAX_RESULTS);
}

/// Get the single-character icon for an object kind.
fn kind_icon(kind: ObjectKind) -> char {
    match kind {
        ObjectKind::Table => 'T',
        ObjectKind::Index => 'I',
        ObjectKind::View => 'V',
        ObjectKind::Trigger => '!',
        ObjectKind::Column => '.',
        ObjectKind::CustomType => 'Y', // tYpe — 'T' taken by Table
    }
}

/// Get the type label for display.
fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "table",
        ObjectKind::Index => "index",
        ObjectKind::View => "view",
        ObjectKind::Trigger => "trigger",
        ObjectKind::Column => "column",
        ObjectKind::CustomType => "type",
    }
}

/// Get the color for an object kind icon.
fn kind_color(kind: ObjectKind, theme: &Theme) -> Color {
    match kind {
        ObjectKind::Table => theme.schema_table,
        ObjectKind::Index => theme.schema_index,
        ObjectKind::View => theme.schema_view,
        ObjectKind::Trigger => theme.schema_trigger,
        ObjectKind::Column => theme.schema_column,
        ObjectKind::CustomType => theme.schema_custom_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ObjectKind;

    #[test]
    fn score_exact_prefix() {
        let score = fuzzy_score("users", "use");
        assert!(score >= 1000);
    }

    #[test]
    fn score_substring() {
        let score = fuzzy_score("my_users_table", "user");
        assert!(score >= 100);
        assert!(score < 1000);
    }

    #[test]
    fn score_no_match() {
        let score = fuzzy_score("orders", "xyz");
        assert_eq!(score, 0);
    }

    #[test]
    fn score_word_boundary() {
        let score = fuzzy_score("user_roles", "ur");
        // "u" at start of "user", "r" at start of "roles"
        assert!(score >= 500);
        assert!(score < 1000);
    }

    #[test]
    fn ranking_prefix_beats_substring() {
        let prefix_score = fuzzy_score("users", "use");
        let substring_score = fuzzy_score("all_users", "use");
        assert!(prefix_score > substring_score);
    }

    #[test]
    fn score_case_insensitive() {
        let score = fuzzy_score("Users", "use");
        assert!(score >= 1000);
    }

    #[test]
    fn score_empty_query_matches() {
        let score = fuzzy_score("anything", "");
        assert!(score > 0);
    }

    #[test]
    fn search_results_sorted_by_score() {
        let mut matches = vec![
            ObjectMatch {
                name: "all_users".into(),
                kind: ObjectKind::Table,
                parent: None,
                database_path: "a.db".into(),
                database_label: "a.db".into(),
                score: 0,
            },
            ObjectMatch {
                name: "users".into(),
                kind: ObjectKind::Table,
                parent: None,
                database_path: "a.db".into(),
                database_label: "a.db".into(),
                score: 0,
            },
        ];
        score_and_sort(&mut matches, "use", "a.db");
        assert_eq!(matches[0].name, "users"); // prefix match ranks first
    }

    #[test]
    fn active_db_bonus_applied() {
        let mut matches = vec![
            ObjectMatch {
                name: "users".into(),
                kind: ObjectKind::Table,
                parent: None,
                database_path: "other.db".into(),
                database_label: "other.db".into(),
                score: 0,
            },
            ObjectMatch {
                name: "users".into(),
                kind: ObjectKind::Table,
                parent: None,
                database_path: "active.db".into(),
                database_label: "active.db".into(),
                score: 0,
            },
        ];
        score_and_sort(&mut matches, "use", "active.db");
        assert_eq!(matches[0].database_path, "active.db");
    }

    #[test]
    fn column_penalty_applied() {
        let mut matches = vec![
            ObjectMatch {
                name: "user_id".into(),
                kind: ObjectKind::Column,
                parent: Some("orders".into()),
                database_path: "a.db".into(),
                database_label: "a.db".into(),
                score: 0,
            },
            ObjectMatch {
                name: "user_id".into(),
                kind: ObjectKind::Table,
                parent: None,
                database_path: "a.db".into(),
                database_label: "a.db".into(),
                score: 0,
            },
        ];
        score_and_sort(&mut matches, "user", "a.db");
        // Table should rank higher than column with same name
        assert_eq!(matches[0].kind, ObjectKind::Table);
    }

    #[test]
    fn empty_query_shows_tables_and_views_only() {
        let mut matches = vec![
            ObjectMatch {
                name: "users".into(),
                kind: ObjectKind::Table,
                parent: None,
                database_path: "a.db".into(),
                database_label: "a.db".into(),
                score: 0,
            },
            ObjectMatch {
                name: "idx_users".into(),
                kind: ObjectKind::Index,
                parent: None,
                database_path: "a.db".into(),
                database_label: "a.db".into(),
                score: 0,
            },
            ObjectMatch {
                name: "user_stats".into(),
                kind: ObjectKind::View,
                parent: None,
                database_path: "a.db".into(),
                database_label: "a.db".into(),
                score: 0,
            },
        ];
        score_and_sort(&mut matches, "", "a.db");
        // Only tables and views, no indexes
        assert_eq!(matches.len(), 2);
        assert!(
            matches
                .iter()
                .all(|m| matches!(m.kind, ObjectKind::Table | ObjectKind::View))
        );
    }

    #[test]
    fn no_match_filtered_out() {
        let mut matches = vec![ObjectMatch {
            name: "orders".into(),
            kind: ObjectKind::Table,
            parent: None,
            database_path: "a.db".into(),
            database_label: "a.db".into(),
            score: 0,
        }];
        score_and_sort(&mut matches, "xyz", "a.db");
        assert!(matches.is_empty());
    }

    #[test]
    fn kind_metadata_for_custom_type() {
        assert_eq!(kind_icon(ObjectKind::CustomType), 'Y');
        assert_eq!(kind_label(ObjectKind::CustomType), "type");
    }
}
