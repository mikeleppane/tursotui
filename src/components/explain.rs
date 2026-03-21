use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, BottomTab, Direction};
use crate::theme::Theme;

use super::Component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExplainMode {
    Bytecode,
    QueryPlan,
}

/// EXPLAIN view with bytecode table and query plan modes.
///
/// Lazily generates EXPLAIN output: on `QueryCompleted`, the view is marked
/// stale via `mark_stale(sql)`. The user presses Enter to generate the
/// EXPLAIN from `last_query`, which is the SQL that was actually executed
/// (not the current editor buffer).
pub(crate) struct ExplainView {
    mode: ExplainMode,
    bytecode_rows: Vec<Vec<String>>,
    plan_lines: Vec<String>,
    selected_row: usize,
    scroll_offset: usize,
    stale: bool,
    loading: bool,
    last_query: Option<String>,
}

/// Column headers for EXPLAIN bytecode output.
const BYTECODE_HEADERS: &[&str] = &["addr", "opcode", "p1", "p2", "p3", "p4", "p5", "comment"];

/// Fixed widths for bytecode columns (all except comment, which fills remaining width).
const BYTECODE_MIN_WIDTHS: &[u16] = &[5, 16, 6, 6, 6, 20, 4];

// Ensure the last column (comment) is the one that fills remaining width.
const _: () = assert!(BYTECODE_MIN_WIDTHS.len() + 1 == BYTECODE_HEADERS.len());

impl ExplainView {
    #[allow(dead_code)] // constructed in UiPanels::new (M4 Task 7)
    pub(crate) fn new() -> Self {
        Self {
            mode: ExplainMode::Bytecode,
            bytecode_rows: Vec::new(),
            plan_lines: Vec::new(),
            selected_row: 0,
            scroll_offset: 0,
            stale: true,
            loading: false,
            last_query: None,
        }
    }

    /// Mark the view as stale after a new query execution.
    /// Stores the SQL for later EXPLAIN generation and clears old data.
    #[allow(dead_code)] // called from dispatch_action_to_components (M4 Task 7)
    pub(crate) fn mark_stale(&mut self, sql: String) {
        self.stale = true;
        self.last_query = Some(sql);
        self.bytecode_rows.clear();
        self.plan_lines.clear();
        self.selected_row = 0;
        self.scroll_offset = 0;
    }

    /// Store EXPLAIN results from the async task.
    #[allow(dead_code)] // called from dispatch_action_to_components (M4 Task 7)
    pub(crate) fn set_results(&mut self, bytecode: Vec<Vec<String>>, plan: Vec<String>) {
        self.bytecode_rows = bytecode;
        self.plan_lines = plan;
        self.stale = false;
        self.loading = false;
        self.selected_row = 0;
        self.scroll_offset = 0;
    }

    /// Mark as loading to prevent duplicate EXPLAIN tasks.
    #[allow(dead_code)] // called from dispatch_action_to_components (M4 Task 7)
    pub(crate) fn set_loading(&mut self) {
        self.loading = true;
    }

    /// Number of content rows in the current mode.
    fn row_count(&self) -> usize {
        match self.mode {
            ExplainMode::Bytecode => self.bytecode_rows.len(),
            ExplainMode::QueryPlan => self.plan_lines.len(),
        }
    }

    /// Ensure `scroll_offset` keeps `selected_row` visible within `viewport_height` rows.
    fn clamp_scroll(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }
        if self.selected_row < self.scroll_offset {
            self.scroll_offset = self.selected_row;
        } else if self.selected_row >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selected_row + 1 - viewport_height;
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

    /// Build the title string including mode indicator and optional query snippet.
    fn title_text(&self) -> String {
        let mode_label = match self.mode {
            ExplainMode::Bytecode => "Bytecode",
            ExplainMode::QueryPlan => "Query Plan",
        };
        match &self.last_query {
            Some(sql) if !self.stale && !self.loading => {
                // Truncate the SQL for the title bar
                let max_sql_len = 40;
                let truncated = if UnicodeWidthStr::width(sql.as_str()) > max_sql_len {
                    let mut end = 0;
                    let mut w = 0;
                    for (i, ch) in sql.char_indices() {
                        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]));
                        if w + cw > max_sql_len - 1 {
                            break;
                        }
                        w += cw;
                        end = i + ch.len_utf8();
                    }
                    format!("{}\u{2026}", &sql[..end])
                } else {
                    sql.clone()
                };
                format!("EXPLAIN [{mode_label}]: {truncated}")
            }
            _ => format!("EXPLAIN [{mode_label}]"),
        }
    }

    /// Render bytecode rows as a table with fixed columns.
    fn render_bytecode(&mut self, frame: &mut Frame, inner: Rect, theme: &Theme) {
        if self.bytecode_rows.is_empty() {
            Self::render_centered(frame, inner, "No bytecode data", theme);
            return;
        }

        // Reserve 1 row for header
        let header_height: u16 = 1;
        if inner.height <= header_height {
            return;
        }
        let data_height = (inner.height - header_height) as usize;

        self.clamp_scroll(data_height);

        let has_scrollbar = self.bytecode_rows.len() > data_height;
        let content_width = if has_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        // Calculate column widths: fixed minimums, last column gets remaining space
        let fixed_count = BYTECODE_MIN_WIDTHS.len();
        let gap: u16 = 1; // space between columns
        let fixed_total: u16 =
            BYTECODE_MIN_WIDTHS.iter().sum::<u16>() + (fixed_count as u16).saturating_sub(1) * gap;
        let comment_width = content_width.saturating_sub(fixed_total + gap);

        // Render header
        let header_y = inner.y;
        let mut x = inner.x;
        for (i, &header) in BYTECODE_HEADERS.iter().enumerate() {
            let col_w = if i < fixed_count {
                BYTECODE_MIN_WIDTHS[i]
            } else {
                comment_width
            };
            let header_area = Rect::new(
                x,
                header_y,
                col_w.min(content_width.saturating_sub(x - inner.x)),
                1,
            );
            frame.render_widget(
                Paragraph::new(Span::styled(header, theme.header_style)),
                header_area,
            );
            x += col_w + gap;
            if x >= inner.x + content_width {
                break;
            }
        }

        // Render data rows
        let visible_end = (self.scroll_offset + data_height).min(self.bytecode_rows.len());
        for (draw_idx, row_idx) in (self.scroll_offset..visible_end).enumerate() {
            let y = inner.y + header_height + draw_idx as u16;
            let row = &self.bytecode_rows[row_idx];

            let mut x = inner.x;
            for (col_idx, _header) in BYTECODE_HEADERS.iter().enumerate() {
                let col_w = if col_idx < fixed_count {
                    BYTECODE_MIN_WIDTHS[col_idx]
                } else {
                    comment_width
                };
                let cell_text = row.get(col_idx).map_or("", String::as_str);
                let available = col_w.min(content_width.saturating_sub(x - inner.x));
                let cell_area = Rect::new(x, y, available, 1);
                frame.render_widget(
                    Paragraph::new(Span::styled(cell_text, Style::default().fg(theme.fg))),
                    cell_area,
                );
                x += col_w + gap;
                if x >= inner.x + content_width {
                    break;
                }
            }

            // Highlight selected row
            if row_idx == self.selected_row {
                let row_area = Rect::new(inner.x, y, content_width, 1);
                frame.buffer_mut().set_style(row_area, theme.selected_style);
            }
        }

        // Scrollbar
        if has_scrollbar {
            let mut scrollbar_state = ScrollbarState::new(self.bytecode_rows.len())
                .position(self.scroll_offset)
                .viewport_content_length(data_height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                inner,
                &mut scrollbar_state,
            );
        }
    }

    /// Render query plan lines as a scrollable list.
    fn render_query_plan(&mut self, frame: &mut Frame, inner: Rect, theme: &Theme) {
        if self.plan_lines.is_empty() {
            Self::render_centered(frame, inner, "No query plan data", theme);
            return;
        }

        let viewport_height = inner.height as usize;
        self.clamp_scroll(viewport_height);

        let has_scrollbar = self.plan_lines.len() > viewport_height;
        let content_width = if has_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        let visible_end = (self.scroll_offset + viewport_height).min(self.plan_lines.len());
        for (draw_idx, line_idx) in (self.scroll_offset..visible_end).enumerate() {
            let y = inner.y + draw_idx as u16;
            let line = &self.plan_lines[line_idx];
            let line_area = Rect::new(inner.x, y, content_width, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(line.as_str(), Style::default().fg(theme.fg))),
                line_area,
            );

            // Highlight selected row
            if line_idx == self.selected_row {
                let row_area = Rect::new(inner.x, y, content_width, 1);
                frame.buffer_mut().set_style(row_area, theme.selected_style);
            }
        }

        // Scrollbar
        if has_scrollbar {
            let mut scrollbar_state = ScrollbarState::new(self.plan_lines.len())
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

impl Component for ExplainView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            // Tab toggles between Bytecode and QueryPlan modes.
            // Returns Some(Action) to consume the key and prevent global Tab from
            // cycling focus. SwitchBottomTab(Explain) is a no-op since we're already
            // on this tab.
            (KeyModifiers::NONE, KeyCode::Tab) => {
                self.mode = match self.mode {
                    ExplainMode::Bytecode => ExplainMode::QueryPlan,
                    ExplainMode::QueryPlan => ExplainMode::Bytecode,
                };
                // Reset scroll position — row counts differ between modes
                self.selected_row = 0;
                self.scroll_offset = 0;
                // Must return Some to consume Tab and prevent global focus cycling
                // (event.rs maps bare Tab → CycleFocus). SwitchBottomTab(Explain)
                // is idempotent since we're already on this tab.
                Some(Action::SwitchBottomTab(BottomTab::Explain))
            }
            // Enter generates EXPLAIN when stale and not already loading.
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.stale
                    && !self.loading
                    && let Some(sql) = self.last_query.clone()
                {
                    // loading flag set by dispatch calling set_loading()
                    return Some(Action::GenerateExplain(sql));
                }
                None
            }
            // Esc releases focus.
            (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::CycleFocus(Direction::Forward)),
            // Navigation: j/Down scroll down
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                let count = self.row_count();
                if count > 0 && self.selected_row < count - 1 {
                    self.selected_row += 1;
                }
                None
            }
            // Navigation: k/Up scroll up
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.selected_row = self.selected_row.saturating_sub(1);
                None
            }
            // g: jump to first
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected_row = 0;
                self.scroll_offset = 0;
                None
            }
            // G: jump to last
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                let count = self.row_count();
                if count > 0 {
                    self.selected_row = count - 1;
                }
                // scroll_offset adjusted by clamp_scroll() on next render()
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

        let title = self.title_text();
        let block = Block::bordered()
            .border_style(border_style)
            .title(title)
            .title_style(title_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Empty states (checked in priority order):
        // 1. No query has been executed yet
        if self.last_query.is_none() {
            Self::render_centered(
                frame,
                inner,
                "No query to explain \u{2014} execute a query first",
                theme,
            );
            return;
        }

        // 2. Currently loading
        if self.loading {
            Self::render_centered(frame, inner, "Generating EXPLAIN...", theme);
            return;
        }

        // 3. Data is stale (query changed since last EXPLAIN)
        if self.stale {
            Self::render_centered(frame, inner, "Press Enter to generate EXPLAIN", theme);
            return;
        }

        // Render based on current mode
        match self.mode {
            ExplainMode::Bytecode => self.render_bytecode(frame, inner, theme),
            ExplainMode::QueryPlan => self.render_query_plan(frame, inner, theme),
        }
    }
}
