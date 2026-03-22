//! Autocomplete popup UI component.
//!
//! Renders a floating dropdown of completion candidates anchored to the editor
//! cursor position. Handles navigation (Up/Down), acceptance (Tab/Enter), and
//! dismissal (Esc).

#![allow(
    dead_code,
    reason = "module wired incrementally -- popup used when editor integration lands"
)]

use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::autocomplete::{Candidate, CandidateKind};
use crate::theme::Theme;

/// Maximum number of candidates visible in the popup at once.
const MAX_VISIBLE: usize = 10;

/// Autocomplete floating popup state.
pub(crate) struct AutocompletePopup {
    /// Filtered and ranked completion candidates.
    candidates: Vec<Candidate>,
    /// Currently highlighted index into `candidates`.
    selected: usize,
    /// Scroll offset when candidates exceed `MAX_VISIBLE`.
    scroll_offset: usize,
    /// Current typing prefix used for filtering.
    pub(crate) prefix: String,
}

impl AutocompletePopup {
    /// Create a new empty popup with the given typing prefix.
    pub(crate) fn new(prefix: String) -> Self {
        Self {
            candidates: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            prefix,
        }
    }

    /// Replace the candidate list (e.g. after re-filtering). Resets selection
    /// and scroll to the top.
    pub(crate) fn update_candidates(&mut self, candidates: Vec<Candidate>) {
        self.candidates = candidates;
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Returns `true` when there are no candidates to show.
    pub(crate) fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Returns the text of the currently selected candidate, if any.
    pub(crate) fn selected_text(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(|c| c.text.as_str())
    }

    /// Move selection up by one, wrapping around to the bottom.
    pub(crate) fn move_up(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.candidates.len() - 1;
        } else {
            self.selected -= 1;
        }
        self.adjust_scroll();
    }

    /// Move selection down by one, wrapping around to the top.
    pub(crate) fn move_down(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.candidates.len();
        self.adjust_scroll();
    }

    /// Ensure the selected item is within the visible scroll window.
    fn adjust_scroll(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + MAX_VISIBLE {
            self.scroll_offset = self.selected + 1 - MAX_VISIBLE;
        }
    }

    /// Render the popup anchored near the given absolute screen coordinates.
    ///
    /// `cursor_x` and `cursor_y` are the absolute terminal cell coordinates of
    /// the editor cursor (not the editor-relative row/col).
    pub(crate) fn render(&self, frame: &mut Frame, cursor_x: u16, cursor_y: u16, theme: &Theme) {
        if self.candidates.is_empty() {
            return;
        }

        let terminal = frame.area();
        let visible_count = self.candidates.len().min(MAX_VISIBLE);

        // Measure the widest row to size the popup.
        // Each row: " X name  detail " (icon 1 char + spaces + text + detail)
        let (max_text_w, max_detail_w) = self.measure_columns();
        // icon(1) + space(1) + text + gap(2) + detail + padding(1)
        let content_width = 1 + 1 + max_text_w + 2 + max_detail_w + 1;
        // Add 2 for borders
        let popup_width = (content_width + 2).min(terminal.width as usize);

        // Height: visible rows + 2 for top/bottom border
        let popup_height = visible_count + 2;

        // Position: prefer below the cursor, flip above if not enough room.
        let below_space = terminal.height.saturating_sub(cursor_y + 1) as usize;
        let above_space = cursor_y as usize;

        let (popup_y, popup_h) = if below_space >= popup_height {
            // Render below cursor
            (cursor_y + 1, popup_height)
        } else if above_space >= popup_height {
            // Render above cursor
            (cursor_y.saturating_sub(popup_height as u16), popup_height)
        } else if below_space >= above_space {
            // Prefer below, clamp height
            let h = below_space.min(popup_height);
            (cursor_y + 1, h)
        } else {
            // Above, clamp height
            let h = above_space.min(popup_height);
            (cursor_y.saturating_sub(h as u16), h)
        };

        // Horizontal: start at cursor_x, clamp to terminal width
        let popup_x = if cursor_x as usize + popup_width <= terminal.width as usize {
            cursor_x
        } else {
            terminal.width.saturating_sub(popup_width as u16)
        };

        let popup_area = Rect::new(popup_x, popup_y, popup_width as u16, popup_h as u16);

        // Clear background behind the popup
        frame.render_widget(Clear, popup_area);

        let block = Block::bordered()
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.bg).fg(theme.fg));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Render each visible candidate row
        let inner_width = inner.width as usize;
        let end = (self.scroll_offset + inner.height as usize).min(self.candidates.len());
        for (i, candidate) in self.candidates[self.scroll_offset..end].iter().enumerate() {
            let abs_idx = self.scroll_offset + i;
            let is_selected = abs_idx == self.selected;

            let row_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);

            let line = Self::build_candidate_line(candidate, is_selected, inner_width, theme);
            let paragraph = Paragraph::new(line);
            frame.render_widget(paragraph, row_area);
        }
    }

    /// Measure the maximum display width of candidate text and detail columns.
    fn measure_columns(&self) -> (usize, usize) {
        let mut max_text: usize = 0;
        let mut max_detail: usize = 0;
        for c in &self.candidates {
            max_text = max_text.max(UnicodeWidthStr::width(c.text.as_str()));
            max_detail = max_detail.max(UnicodeWidthStr::width(c.detail.as_str()));
        }
        (max_text, max_detail)
    }

    /// Build a styled `Line` for a single candidate row.
    fn build_candidate_line(
        candidate: &Candidate,
        is_selected: bool,
        width: usize,
        theme: &Theme,
    ) -> Line<'static> {
        let icon = kind_icon(candidate.kind);
        let icon_style = if is_selected {
            Style::default().fg(theme.bg).bg(theme.accent)
        } else {
            kind_style(candidate.kind, theme)
        };

        let (text_style, detail_style, fill_style) = if is_selected {
            let sel = Style::default().fg(theme.bg).bg(theme.accent);
            let sel_detail = sel.add_modifier(Modifier::DIM);
            (sel, sel_detail, sel)
        } else {
            let normal = Style::default().fg(theme.fg).bg(theme.bg);
            let dimmed = Style::default()
                .fg(theme.border)
                .bg(theme.bg)
                .add_modifier(Modifier::DIM);
            (normal, dimmed, normal)
        };

        // Layout: icon + space + text + gap + detail, padded/truncated to `width`
        // We build spans character by character to respect unicode widths.
        let icon_str = format!("{icon} ");
        let icon_w = UnicodeWidthStr::width(icon_str.as_str());

        let remaining = width.saturating_sub(icon_w);

        let text_w = UnicodeWidthStr::width(candidate.text.as_str());
        let detail_w = UnicodeWidthStr::width(candidate.detail.as_str());

        // Allocate space: text gets priority, then gap(2), then detail
        let needed = text_w + 2 + detail_w;
        let (truncated_text, truncated_detail, gap) = if needed <= remaining {
            // Everything fits
            let gap = remaining.saturating_sub(text_w + detail_w);
            (candidate.text.clone(), candidate.detail.clone(), gap)
        } else if text_w + 2 <= remaining {
            // Text fits, truncate detail
            let detail_budget = remaining.saturating_sub(text_w + 2);
            let trunc_detail = truncate_to_width(&candidate.detail, detail_budget);
            (candidate.text.clone(), trunc_detail, 2)
        } else {
            // Truncate text, no detail
            let text_budget = remaining;
            let trunc_text = truncate_to_width(&candidate.text, text_budget);
            (trunc_text, String::new(), 0)
        };

        let gap_str = " ".repeat(gap);

        let mut spans = vec![
            Span::styled(icon_str, icon_style),
            Span::styled(truncated_text, text_style),
            Span::styled(gap_str, fill_style),
        ];

        if !truncated_detail.is_empty() {
            spans.push(Span::styled(truncated_detail, detail_style));
        }

        // Pad remaining width with spaces to fill the background
        let used: usize = spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        if used < width {
            spans.push(Span::styled(" ".repeat(width - used), fill_style));
        }

        Line::from(spans)
    }
}

/// Return the single-character icon for a candidate kind.
fn kind_icon(kind: CandidateKind) -> char {
    match kind {
        CandidateKind::Table => 'T',
        CandidateKind::View => 'V',
        CandidateKind::Column => 'C',
        CandidateKind::Keyword => 'K',
        CandidateKind::Function => 'F',
    }
}

/// Return a style tinted by candidate kind (for the icon in non-selected rows).
fn kind_style(kind: CandidateKind, theme: &Theme) -> Style {
    let fg = match kind {
        CandidateKind::Table => theme.accent,
        CandidateKind::View => theme.success,
        CandidateKind::Column => theme.warning,
        CandidateKind::Keyword => theme.border,
        CandidateKind::Function => theme.sql_function.fg.unwrap_or(theme.fg),
    };
    Style::default().fg(fg).bg(theme.bg)
}

/// Truncate a string to fit within `max_width` display columns, using
/// `unicode_width` for correct measurement. Never uses byte-based truncation.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut current_width: usize = 0;
    for ch in s.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > max_width {
            break;
        }
        result.push(ch);
        current_width += ch_width;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::{Candidate, CandidateKind};

    fn sample_candidates() -> Vec<Candidate> {
        vec![
            Candidate {
                text: "users".into(),
                kind: CandidateKind::Table,
                detail: "table".into(),
                score: 200,
            },
            Candidate {
                text: "user_id".into(),
                kind: CandidateKind::Column,
                detail: "INTEGER".into(),
                score: 150,
            },
            Candidate {
                text: "UPDATE".into(),
                kind: CandidateKind::Keyword,
                detail: "keyword".into(),
                score: 100,
            },
        ]
    }

    #[test]
    fn new_popup_is_empty() {
        let popup = AutocompletePopup::new(String::new());
        assert!(popup.is_empty());
        assert!(popup.selected_text().is_none());
    }

    #[test]
    fn update_candidates_resets_state() {
        let mut popup = AutocompletePopup::new("us".into());
        popup.selected = 2;
        popup.scroll_offset = 1;
        popup.update_candidates(sample_candidates());

        assert_eq!(popup.selected, 0);
        assert_eq!(popup.scroll_offset, 0);
        assert_eq!(popup.candidates.len(), 3);
    }

    #[test]
    fn selected_text_returns_current() {
        let mut popup = AutocompletePopup::new(String::new());
        popup.update_candidates(sample_candidates());

        assert_eq!(popup.selected_text(), Some("users"));
        popup.move_down();
        assert_eq!(popup.selected_text(), Some("user_id"));
    }

    #[test]
    fn move_down_wraps() {
        let mut popup = AutocompletePopup::new(String::new());
        popup.update_candidates(sample_candidates());

        popup.move_down(); // -> 1
        popup.move_down(); // -> 2
        popup.move_down(); // -> 0 (wrap)
        assert_eq!(popup.selected, 0);
    }

    #[test]
    fn move_up_wraps() {
        let mut popup = AutocompletePopup::new(String::new());
        popup.update_candidates(sample_candidates());

        popup.move_up(); // -> 2 (wrap from 0)
        assert_eq!(popup.selected, 2);
    }

    #[test]
    fn move_on_empty_is_noop() {
        let mut popup = AutocompletePopup::new(String::new());
        popup.move_up();
        popup.move_down();
        assert_eq!(popup.selected, 0);
    }

    #[test]
    fn scroll_adjusts_when_exceeding_visible() {
        let mut popup = AutocompletePopup::new(String::new());
        // Create more than MAX_VISIBLE candidates
        let many: Vec<Candidate> = (0..15)
            .map(|i| Candidate {
                text: format!("item_{i}"),
                kind: CandidateKind::Table,
                detail: String::new(),
                score: 100,
            })
            .collect();
        popup.update_candidates(many);

        // Move down past visible window
        for _ in 0..12 {
            popup.move_down();
        }
        assert!(popup.scroll_offset > 0);
        assert!(popup.selected >= popup.scroll_offset);
        assert!(popup.selected < popup.scroll_offset + MAX_VISIBLE);
    }

    #[test]
    fn truncate_to_width_ascii() {
        assert_eq!(truncate_to_width("hello world", 5), "hello");
        assert_eq!(truncate_to_width("hi", 10), "hi");
        assert_eq!(truncate_to_width("test", 0), "");
    }

    #[test]
    fn truncate_to_width_unicode() {
        // CJK characters are 2 columns wide
        let cjk = "\u{4e16}\u{754c}"; // "world" in Chinese, 4 columns
        assert_eq!(truncate_to_width(cjk, 3), "\u{4e16}"); // only first char fits
        assert_eq!(truncate_to_width(cjk, 4), cjk); // both fit
    }

    #[test]
    fn kind_icon_mapping() {
        assert_eq!(kind_icon(CandidateKind::Table), 'T');
        assert_eq!(kind_icon(CandidateKind::View), 'V');
        assert_eq!(kind_icon(CandidateKind::Column), 'C');
        assert_eq!(kind_icon(CandidateKind::Keyword), 'K');
        assert_eq!(kind_icon(CandidateKind::Function), 'F');
    }
}
