use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AdminAction, DataAction, NavAction};
use crate::theme::Theme;
use tursotui_db::ProfileData;

use super::Component;

/// Data profiling view: column statistics and top-value distributions.
pub(crate) struct ProfileView {
    data: Option<ProfileData>,
    selected_col: usize,
    scroll_offset: usize,
    /// Scroll offset for the right-side stats panel.
    detail_scroll: usize,
    stale: bool,
    loading: bool,
    /// Cache key: (table, rows) — used for stale detection.
    cached_key: Option<(String, u64)>,
}

impl ProfileView {
    pub(crate) fn new() -> Self {
        Self {
            data: None,
            selected_col: 0,
            scroll_offset: 0,
            detail_scroll: 0,
            stale: true,
            loading: false,
            cached_key: None,
        }
    }

    /// Set profile data from a completed profile query.
    pub(crate) fn set_data(&mut self, data: ProfileData) {
        self.cached_key = Some((data.table_name.clone(), data.total_rows));
        self.data = Some(data);
        self.loading = false;
        self.stale = false;
        self.selected_col = 0;
        self.scroll_offset = 0;
        self.detail_scroll = 0;
    }

    /// Mark the profile as stale (data has changed).
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
        self.loading = false;
    }

    /// Mark as loading.
    pub(crate) fn set_loading(&mut self) {
        self.loading = true;
    }

    /// Whether the profile is stale (data changed since last profile).
    pub(crate) fn is_stale(&self) -> bool {
        self.stale
    }

    /// Check if the profile should be invalidated due to a table change.
    /// Returns true if the cached profile is for `table_name` and the row count changed.
    pub(crate) fn should_invalidate(&self, table_name: &str, new_row_count: u64) -> bool {
        self.cached_key.as_ref().is_some_and(|(name, count)| {
            name.eq_ignore_ascii_case(table_name) && *count != new_row_count
        })
    }
}

impl Component for ProfileView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                if let Some(ref data) = self.data {
                    let max = data.columns.len().saturating_sub(1);
                    if self.selected_col < max {
                        self.selected_col += 1;
                        self.detail_scroll = 0;
                    }
                }
                Some(Action::Consumed)
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                if self.selected_col > 0 {
                    self.selected_col -= 1;
                    self.detail_scroll = 0;
                }
                Some(Action::Consumed)
            }
            // Ctrl+Down / Ctrl+Up: scroll detail panel
            (KeyModifiers::CONTROL, KeyCode::Down) => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
                Some(Action::Consumed)
            }
            (KeyModifiers::CONTROL, KeyCode::Up) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                Some(Action::Consumed)
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected_col = 0;
                self.scroll_offset = 0;
                self.detail_scroll = 0;
                Some(Action::Consumed)
            }
            (KeyModifiers::SHIFT | KeyModifiers::NONE, KeyCode::Char('G')) => {
                if let Some(ref data) = self.data {
                    self.selected_col = data.columns.len().saturating_sub(1);
                    self.detail_scroll = 0;
                }
                Some(Action::Consumed)
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.stale || self.data.is_none() {
                    Some(Action::Admin(AdminAction::RequestProfile))
                } else {
                    None
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => {
                Some(Action::Admin(AdminAction::RequestProfile))
            }
            (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::Nav(NavAction::CycleFocus(
                crate::app::Direction::Forward,
            ))),
            _ => None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, _area: Rect) -> Option<Action> {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.selected_col > 0 {
                    self.selected_col -= 1;
                    self.detail_scroll = 0;
                }
                Some(Action::Consumed)
            }
            MouseEventKind::ScrollDown => {
                if let Some(ref data) = self.data {
                    let max = data.columns.len().saturating_sub(1);
                    if self.selected_col < max {
                        self.selected_col += 1;
                        self.detail_scroll = 0;
                    }
                }
                Some(Action::Consumed)
            }
            _ => None,
        }
    }

    fn update(&mut self, action: &Action) {
        if let Action::Data(DataAction::DataEditsCommitted) = action {
            self.mark_stale();
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let stale_mark = if self.stale { "*" } else { "" };
        let title = if let Some(ref data) = self.data {
            if data.sampled {
                format!(
                    "Profile: {} (sampled ~{} rows){stale_mark}",
                    data.table_name,
                    format_u64(data.total_rows.min(10_000))
                )
            } else {
                format!("Profile: {}{stale_mark}", data.table_name)
            }
        } else {
            "Profile".to_string()
        };
        let block = super::panel_block(&title, focused, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.loading {
            let msg = Paragraph::new("Profiling...")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.dim));
            let center_y = inner.y + inner.height / 2;
            let center_area = Rect::new(inner.x, center_y, inner.width, 1);
            frame.render_widget(msg, center_area);
            return;
        }

        let Some(ref data) = self.data else {
            let stale_hint = if self.stale { " (stale)" } else { "" };
            let msg = Paragraph::new(format!("Press Enter to generate profile{stale_hint}"))
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.dim));
            let center_y = inner.y + inner.height / 2;
            let center_area = Rect::new(inner.x, center_y, inner.width, 1);
            frame.render_widget(msg, center_area);
            return;
        };

        if data.columns.is_empty() {
            let msg = Paragraph::new("No columns to profile")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.dim));
            frame.render_widget(msg, inner);
            return;
        }

        // Two-column layout: left 30% = column list, right 70% = stats
        let left_width = inner.width * 30 / 100;
        let right_width = inner.width.saturating_sub(left_width);
        let left_area = Rect::new(inner.x, inner.y, left_width, inner.height);
        let right_area = Rect::new(inner.x + left_width, inner.y, right_width, inner.height);

        // === Left: column list with completeness indicators ===
        render_column_list(
            frame,
            left_area,
            data,
            self.selected_col,
            &mut self.scroll_offset,
            theme,
        );

        // === Right: stats for selected column ===
        let selected = self.selected_col.min(data.columns.len().saturating_sub(1));
        let col = &data.columns[selected];
        render_column_stats(
            frame,
            right_area,
            col,
            data.total_rows,
            &mut self.detail_scroll,
            theme,
        );
    }
}

/// Render the column list with completeness indicators on the left side.
#[allow(clippy::cast_precision_loss)]
fn render_column_list(
    frame: &mut Frame,
    area: Rect,
    data: &ProfileData,
    selected_col: usize,
    scroll_offset: &mut usize,
    theme: &Theme,
) {
    let visible_height = area.height as usize;
    // Ensure selected column is visible
    if selected_col < *scroll_offset {
        *scroll_offset = selected_col;
    } else if selected_col >= *scroll_offset + visible_height {
        *scroll_offset = selected_col.saturating_sub(visible_height - 1);
    }

    for (i, col) in data
        .columns
        .iter()
        .enumerate()
        .skip(*scroll_offset)
        .take(visible_height)
    {
        let y = area.y + (i - *scroll_offset) as u16;
        let row_area = Rect::new(area.x, y, area.width, 1);

        let null_pct = if col.total_count == 0 {
            100.0
        } else {
            (col.null_count as f64 / col.total_count as f64) * 100.0
        };

        let (indicator, indicator_color) =
            if col.total_count == 0 || col.null_count == col.total_count {
                ("\u{2205}", theme.dim) // empty set — all null
            } else if col.null_count == 0 {
                ("\u{25cf}", theme.success) // filled circle — no nulls (green)
            } else if null_pct < 50.0 {
                ("\u{25d0}", theme.warning) // half circle — <50% null (yellow)
            } else {
                ("\u{25cb}", theme.error) // empty circle — ≥50% null (red)
            };

        let is_selected = i == selected_col;
        let display_name = truncate_display(&col.name, area.width.saturating_sub(3) as usize);

        let name_style = if is_selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
                .bg(theme.surface0)
        } else {
            Style::default().fg(theme.fg)
        };
        let bg = if is_selected {
            Some(theme.surface0)
        } else {
            None
        };
        let mut ind_style = Style::default().fg(indicator_color);
        if let Some(bg_color) = bg {
            ind_style = ind_style.bg(bg_color);
        }

        let line = Line::from(vec![
            Span::styled(indicator, ind_style),
            Span::styled(format!(" {display_name}"), name_style),
        ]);
        let pad_area = Rect::new(area.x, y, area.width, 1);
        if is_selected {
            // Fill the full row with background for selected style
            frame
                .buffer_mut()
                .set_style(pad_area, Style::default().bg(theme.surface0));
        }
        frame.render_widget(Paragraph::new(line), row_area);
    }
}

/// Render statistics for a single column profile with vertical scrolling.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
fn render_column_stats(
    frame: &mut Frame,
    area: Rect,
    col: &tursotui_db::ColumnProfile,
    total_rows: u64,
    detail_scroll: &mut usize,
    theme: &Theme,
) {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(30);

    // Header
    let col_type_display = if col.col_type.is_empty() {
        "(no type)".to_string()
    } else {
        col.col_type.clone()
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", col.name),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" [{col_type_display}]"),
            Style::default().fg(theme.dim),
        ),
    ]));
    lines.push(Line::from(""));

    // Core stats
    let null_pct = if col.total_count == 0 {
        0.0
    } else {
        (col.null_count as f64 / col.total_count as f64) * 100.0
    };
    let completeness = 100.0 - null_pct;

    lines.push(stat_line("  Rows", &format_u64(col.total_count), theme));
    lines.push(stat_line(
        "  Nulls",
        &format!("{} ({:.1}%)", format_u64(col.null_count), null_pct),
        theme,
    ));
    lines.push(stat_line(
        "  Completeness",
        &format!("{completeness:.1}%"),
        theme,
    ));
    lines.push(stat_line(
        "  Distinct",
        &format_u64(col.distinct_count),
        theme,
    ));
    // Uniqueness ratio
    let non_null = col.total_count.saturating_sub(col.null_count);
    if non_null > 0 {
        let uniqueness = (col.distinct_count as f64 / non_null as f64) * 100.0;
        lines.push(stat_line(
            "  Uniqueness",
            &format!("{uniqueness:.1}%"),
            theme,
        ));
    }
    if let Some(ref min) = col.min {
        lines.push(stat_line("  Min", min, theme));
    }
    if let Some(ref max) = col.max {
        lines.push(stat_line("  Max", max, theme));
    }

    // Numeric stats
    if let Some(avg) = col.avg {
        lines.push(stat_line("  Avg", &format!("{avg:.4}"), theme));
    }
    if let Some(sum) = col.sum {
        lines.push(stat_line("  Sum", &format!("{sum:.4}"), theme));
    }
    if let Some(stddev) = col.stddev {
        lines.push(stat_line("  StdDev", &format!("{stddev:.4}"), theme));
    }

    // Text length stats
    if let Some(min_len) = col.min_length {
        lines.push(stat_line("  Min Length", &format_u64(min_len), theme));
    }
    if let Some(max_len) = col.max_length {
        lines.push(stat_line("  Max Length", &format_u64(max_len), theme));
    }
    if let Some(avg_len) = col.avg_length {
        lines.push(stat_line("  Avg Length", &format!("{avg_len:.1}"), theme));
    }

    // Top values bar chart — hidden for high-cardinality columns (>50 distinct)
    if col.distinct_count <= 50 {
        render_top_values(&mut lines, col, total_rows, area.width, theme);
    } else if !col.top_values.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "  ({} distinct values — top values hidden)",
                format_u64(col.distinct_count)
            ),
            Style::default().fg(theme.dim),
        )));
    }

    let total_lines = lines.len();
    let visible_height = area.height as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);
    *detail_scroll = (*detail_scroll).min(max_scroll);

    let paragraph =
        Paragraph::new(lines).scroll((u16::try_from(*detail_scroll).unwrap_or(u16::MAX), 0));
    frame.render_widget(paragraph, area);
}

/// Render the top-values bar chart section.
#[allow(clippy::cast_precision_loss)]
fn render_top_values(
    lines: &mut Vec<Line<'static>>,
    col: &tursotui_db::ColumnProfile,
    total_rows: u64,
    area_width: u16,
    theme: &Theme,
) {
    if col.top_values.is_empty() {
        return;
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Top Values",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::UNDERLINED),
    )));

    let max_count = col.top_values.iter().map(|(_, c)| *c).max().unwrap_or(1);
    let bar_max_width = area_width.saturating_sub(30) as usize;

    for (val, count) in &col.top_values {
        let bar_width = if max_count > 0 {
            (*count as usize * bar_max_width) / max_count as usize
        } else {
            0
        }
        .max(1);

        let pct = if total_rows > 0 {
            (*count as f64 / total_rows as f64) * 100.0
        } else {
            0.0
        };

        let truncated_val = truncate_display(val, 15);
        let bar = "\u{2588}".repeat(bar_width);
        let label = format!("  {truncated_val:<15} ");

        lines.push(Line::from(vec![
            Span::styled(label, Style::default().fg(theme.fg)),
            Span::styled(bar, Style::default().fg(theme.accent2)),
            Span::styled(
                format!(" {} ({pct:.1}%)", format_u64(*count)),
                Style::default().fg(theme.dim),
            ),
        ]));
    }
}

/// Create a stat line with label and value.
fn stat_line<'a>(label: &str, value: &str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(theme.dim)),
        Span::styled(value.to_string(), Style::default().fg(theme.fg)),
    ])
}

/// Format a u64 with thousands separators.
fn format_u64(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Truncate a string to fit within `max_width` display columns using unicode widths.
fn truncate_display(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_string();
    }
    let mut current_width = 0;
    let mut result = String::new();
    for ch in s.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > max_width.saturating_sub(1) {
            result.push('\u{2026}'); // ellipsis
            break;
        }
        current_width += ch_width;
        result.push(ch);
    }
    result
}
