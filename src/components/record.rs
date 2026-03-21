use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, Direction};
use crate::db::ColumnDef;
use crate::theme::Theme;

use super::Component;

/// Pre-computed layout dimensions for rendering field rows.
struct FieldLayout {
    label_width: u16,
    value_start: u16,
    value_width: u16,
}

/// Vertical key-value display of a single result row.
///
/// Receives copies of row data from `ResultsTable` via `set_row()`.
/// Does not own query data -- always reads pre-rendered display strings.
pub(crate) struct RecordDetail {
    /// Column definitions (name + type) from the query result.
    columns: Vec<ColumnDef>,
    /// Display values for the selected row. `None` = SQL NULL.
    values: Vec<Option<String>>,
    /// Which field is highlighted.
    selected_field: usize,
    /// Viewport scroll offset for when fields exceed visible area.
    scroll_offset: usize,
}

impl RecordDetail {
    pub(crate) fn new() -> Self {
        Self {
            columns: Vec::new(),
            values: Vec::new(),
            selected_field: 0,
            scroll_offset: 0,
        }
    }

    /// Populate the detail view with a row's data.
    /// Columns come from `ColumnDef` (name + `type_name`).
    /// Values are `Option<String>` — `None` = SQL NULL, `Some(s)` = display text.
    pub(crate) fn set_row(&mut self, columns: &[ColumnDef], values: &[Option<String>]) {
        self.columns = columns.to_vec();
        self.values = values.to_vec();
        self.selected_field = 0;
        self.scroll_offset = 0;
    }

    /// Clear all data (e.g. when there are no results).
    pub(crate) fn clear(&mut self) {
        self.columns.clear();
        self.values.clear();
        self.selected_field = 0;
        self.scroll_offset = 0;
    }

    /// Ensure `scroll_offset` keeps `selected_field` visible within `viewport_height` rows.
    fn clamp_scroll(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }
        if self.selected_field < self.scroll_offset {
            self.scroll_offset = self.selected_field;
        } else if self.selected_field >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selected_field + 1 - viewport_height;
        }
    }

    /// Compute the label column width from the widest field name + type annotation.
    fn label_column_width(&self, max_width: u16) -> u16 {
        self.columns
            .iter()
            .map(|col| {
                let w = if col.type_name.is_empty() {
                    UnicodeWidthStr::width(col.name.as_str())
                } else {
                    // "name (TYPE)"
                    UnicodeWidthStr::width(col.name.as_str())
                        + 1
                        + UnicodeWidthStr::width(col.type_name.as_str())
                        + 2
                };
                w as u16
            })
            .max()
            .unwrap_or(0)
            .min(max_width / 2)
    }

    /// Render a single field row (label + value).
    fn render_field(
        &self,
        frame: &mut Frame,
        inner: Rect,
        field_idx: usize,
        y: u16,
        layout: &FieldLayout,
        theme: &Theme,
    ) {
        let col = &self.columns[field_idx];

        let label_line = if col.type_name.is_empty() {
            Line::from(Span::styled(
                col.name.clone(),
                Style::default().fg(theme.fg),
            ))
        } else {
            Line::from(vec![
                Span::styled(col.name.clone(), Style::default().fg(theme.fg)),
                Span::styled(
                    format!(" ({})", col.type_name),
                    Style::default()
                        .fg(theme.border)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        };

        let label_area = Rect::new(inner.x, y, layout.label_width.min(inner.width), 1);
        frame.render_widget(Paragraph::new(label_line), label_area);

        if layout.value_width > 0 {
            let (display_text, value_style) = match self.values.get(field_idx) {
                Some(Some(s)) => (s.as_str(), Style::default().fg(theme.fg)),
                Some(None) | None => ("NULL", theme.null_style),
            };
            let value_area = Rect::new(inner.x + layout.value_start, y, layout.value_width, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(display_text, value_style)),
                value_area,
            );
        }

        if field_idx == self.selected_field {
            let row_area = Rect::new(inner.x, y, inner.width, 1);
            frame.buffer_mut().set_style(row_area, theme.selected_style);
        }
    }
}

impl Component for RecordDetail {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if self.columns.is_empty() {
            return match key.code {
                KeyCode::Tab | KeyCode::Esc => Some(Action::CycleFocus(Direction::Forward)),
                _ => None,
            };
        }

        let last = self.columns.len().saturating_sub(1);

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                if self.selected_field < last {
                    self.selected_field += 1;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.selected_field = self.selected_field.saturating_sub(1);
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected_field = 0;
                self.scroll_offset = 0;
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected_field = last;
                // scroll_offset adjusted by clamp_scroll() on next render()
                None
            }
            (KeyModifiers::NONE, KeyCode::Tab | KeyCode::Esc) => {
                Some(Action::CycleFocus(Direction::Forward))
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

        let block = Block::bordered()
            .border_style(border_style)
            .title("Record Detail")
            .title_style(title_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        if self.columns.is_empty() {
            let msg = "No results \u{2014} run a query first";
            let msg_width = UnicodeWidthStr::width(msg) as u16;
            let x = inner.x + inner.width.saturating_sub(msg_width) / 2;
            let y = inner.y + inner.height / 2;
            let msg_area = Rect::new(x, y, msg_width.min(inner.width), 1);
            frame.render_widget(
                Paragraph::new(msg).style(Style::default().fg(theme.border)),
                msg_area,
            );
            return;
        }

        let viewport_height = inner.height as usize;
        self.clamp_scroll(viewport_height);

        let has_scrollbar = self.columns.len() > viewport_height;
        // Reserve 1 column for the scrollbar track when content overflows
        let content_width = if has_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };
        let label_width = self.label_column_width(content_width);
        let gap: u16 = 2;
        let value_start = label_width + gap;
        let layout = FieldLayout {
            label_width,
            value_start,
            value_width: content_width.saturating_sub(value_start),
        };

        let visible_end = (self.scroll_offset + viewport_height).min(self.columns.len());
        for (draw_idx, field_idx) in (self.scroll_offset..visible_end).enumerate() {
            let y = inner.y + draw_idx as u16;
            self.render_field(frame, inner, field_idx, y, &layout, theme);
        }

        // Scrollbar (only when content exceeds viewport)
        if self.columns.len() > viewport_height {
            let mut scrollbar_state = ScrollbarState::new(self.columns.len())
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
