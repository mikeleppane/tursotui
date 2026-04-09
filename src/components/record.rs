use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, Direction, NavAction};
use crate::theme::Theme;
use tursotui_db::ColumnDef;

use super::Component;

/// Apply basic JSON syntax coloring to a pre-formatted line.
/// Handles: keys (accent), string values (green/success), numbers (yellow/warning),
/// null/true/false (dimmed), structural chars (fg).
#[allow(clippy::too_many_lines)]
fn json_color_line<'a>(line: &str, theme: &Theme) -> Line<'a> {
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            b'"' => {
                // Find end of string
                let start = i;
                i += 1;
                while i < len && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1; // skip escaped char
                    }
                    i += 1;
                }
                if i < len {
                    i += 1; // skip closing quote
                }
                let s = &line[start..i];
                // Check if this is a key (followed by ':')
                let rest = line[i..].trim_start();
                let color = if rest.starts_with(':') {
                    theme.accent
                } else {
                    theme.success
                };
                spans.push(Span::styled(s.to_string(), Style::default().fg(color)));
            }
            b'0'..=b'9' | b'-'
                if matches!(bytes.get(i + 1), Some(b'0'..=b'9') | None) || bytes[i] != b'-' =>
            {
                // Number
                let start = i;
                while i < len
                    && (bytes[i].is_ascii_digit()
                        || bytes[i] == b'.'
                        || bytes[i] == b'-'
                        || bytes[i] == b'+'
                        || bytes[i] == b'e'
                        || bytes[i] == b'E')
                {
                    i += 1;
                }
                // Only color as number if we actually advanced past the start for digits
                if i > start && line[start..i].parse::<f64>().is_ok() {
                    spans.push(Span::styled(
                        line[start..i].to_string(),
                        Style::default().fg(theme.warning),
                    ));
                } else {
                    spans.push(Span::styled(
                        line[start..i.max(start + 1)].to_string(),
                        Style::default().fg(theme.fg),
                    ));
                    if i == start {
                        i += 1;
                    }
                }
            }
            b'n' if line[i..].starts_with("null") => {
                spans.push(Span::styled(
                    "null".to_string(),
                    Style::default()
                        .fg(theme.border)
                        .add_modifier(Modifier::DIM),
                ));
                i += 4;
            }
            b't' if line[i..].starts_with("true") => {
                spans.push(Span::styled(
                    "true".to_string(),
                    Style::default()
                        .fg(theme.border)
                        .add_modifier(Modifier::DIM),
                ));
                i += 4;
            }
            b'f' if line[i..].starts_with("false") => {
                spans.push(Span::styled(
                    "false".to_string(),
                    Style::default()
                        .fg(theme.border)
                        .add_modifier(Modifier::DIM),
                ));
                i += 5;
            }
            _ => {
                // Structural characters and whitespace
                let start = i;
                while i < len
                    && bytes[i] != b'"'
                    && !(bytes[i] >= b'0' && bytes[i] <= b'9')
                    && bytes[i] != b'-'
                    && !line[i..].starts_with("null")
                    && !line[i..].starts_with("true")
                    && !line[i..].starts_with("false")
                {
                    i += 1;
                }
                if i > start {
                    spans.push(Span::styled(
                        line[start..i].to_string(),
                        Style::default().fg(theme.fg),
                    ));
                }
                if i == start {
                    // Safety: advance at least one character
                    spans.push(Span::styled(
                        line[i..=i].to_string(),
                        Style::default().fg(theme.fg),
                    ));
                    i += 1;
                }
            }
        }
    }

    Line::from(spans)
}

/// Check if a value looks like JSON. Returns the parsed Value if it is.
fn try_parse_json(value: &str, type_name: &str) -> Option<serde_json::Value> {
    // Tier 1: column type contains "json"
    if type_name.to_lowercase().contains("json") {
        return serde_json::from_str(value).ok();
    }
    // Tier 2: heuristic — starts with { or [ and parses
    let trimmed = value.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).ok();
    }
    None
}

/// Build a compact JSON indicator: "{...} (N keys)" or "[...] (N items)"
fn json_compact_indicator(json: &serde_json::Value) -> String {
    match json {
        serde_json::Value::Object(map) => format!("{{...}} ({} keys)", map.len()),
        serde_json::Value::Array(arr) => format!("[...] ({} items)", arr.len()),
        _ => json.to_string(),
    }
}

/// Pre-computed layout dimensions for rendering field rows.
struct FieldLayout {
    label_width: u16,
    value_start: u16,
    value_width: u16,
}

/// State for the JSON pretty-print overlay popup.
struct JsonOverlay {
    field_name: String,
    raw_lines: Vec<String>,
    scroll: usize,
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
    /// When Some, a JSON overlay is shown for the field at this index.
    json_overlay: Option<JsonOverlay>,
}

impl RecordDetail {
    pub(crate) fn new() -> Self {
        Self {
            columns: Vec::new(),
            values: Vec::new(),
            selected_field: 0,
            scroll_offset: 0,
            json_overlay: None,
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
        self.json_overlay = None;
    }

    /// Clear all data (e.g. when there are no results).
    pub(crate) fn clear(&mut self) {
        self.columns.clear();
        self.values.clear();
        self.selected_field = 0;
        self.scroll_offset = 0;
        self.json_overlay = None;
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
                Some(Some(s)) => {
                    let type_name = &self.columns[field_idx].type_name;
                    if let Some(json) = try_parse_json(s, type_name) {
                        (
                            json_compact_indicator(&json),
                            Style::default().fg(theme.accent),
                        )
                    } else {
                        (s.clone(), Style::default().fg(theme.fg))
                    }
                }
                Some(None) | None => ("NULL".to_string(), theme.null_style),
            };
            let value_area = Rect::new(inner.x + layout.value_start, y, layout.value_width, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(&*display_text, value_style)),
                value_area,
            );
        }

        if field_idx == self.selected_field {
            let row_area = Rect::new(inner.x, y, inner.width, 1);
            frame.buffer_mut().set_style(row_area, theme.selected_style);
        }
    }

    /// Returns true if the JSON overlay is active and should be rendered on top.
    pub(crate) fn has_overlay(&self) -> bool {
        self.json_overlay.is_some()
    }

    /// Render the JSON overlay as a centered popup (similar to help overlay).
    pub(crate) fn render_overlay(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let Some(ref overlay) = self.json_overlay else {
            return;
        };

        // Use 80% of terminal area
        let width = (area.width * 4 / 5).max(40).min(area.width);
        let height = (area.height * 4 / 5).max(10).min(area.height);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        // Clear background
        frame.render_widget(ratatui::widgets::Clear, popup_area);

        let title = format!("JSON \u{2014} {} (Esc to close)", overlay.field_name);
        let block = super::overlay_block(&title, theme);

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let viewport_height = inner.height as usize;

        // Clamp scroll (read-only local variable — self is &self)
        let max_scroll = overlay.raw_lines.len().saturating_sub(viewport_height);
        let scroll = overlay.scroll.min(max_scroll);

        // Build visible lines with JSON syntax coloring
        let visible_lines: Vec<Line> = overlay
            .raw_lines
            .iter()
            .skip(scroll)
            .take(viewport_height)
            .map(|line| json_color_line(line, theme))
            .collect();

        frame.render_widget(Paragraph::new(visible_lines), inner);

        // Scrollbar if content exceeds viewport
        if overlay.raw_lines.len() > viewport_height {
            let mut scrollbar_state = ScrollbarState::new(overlay.raw_lines.len())
                .position(scroll)
                .viewport_content_length(viewport_height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                inner,
                &mut scrollbar_state,
            );
        }
    }
}

impl Component for RecordDetail {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        // JSON overlay consumes all keys when active
        if let Some(ref mut overlay) = self.json_overlay {
            match key.code {
                KeyCode::Esc => {
                    self.json_overlay = None;
                    return None;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    overlay.scroll = overlay.scroll.saturating_add(1);
                    return None;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    overlay.scroll = overlay.scroll.saturating_sub(1);
                    return None;
                }
                KeyCode::Char('g') => {
                    overlay.scroll = 0;
                    return None;
                }
                KeyCode::Char('G') => {
                    overlay.scroll = overlay.raw_lines.len().saturating_sub(1);
                    return None;
                }
                _ => return None,
            }
        }

        if self.columns.is_empty() {
            return match key.code {
                KeyCode::Tab | KeyCode::Esc => {
                    Some(Action::Nav(NavAction::CycleFocus(Direction::Forward)))
                }
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
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if let Some(Some(val)) = self.values.get(self.selected_field) {
                    let type_name = &self.columns[self.selected_field].type_name;
                    if let Some(json) = try_parse_json(val, type_name) {
                        let field_name = self.columns[self.selected_field].name.clone();
                        let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();
                        let raw_lines: Vec<String> = pretty.lines().map(String::from).collect();
                        self.json_overlay = Some(JsonOverlay {
                            field_name,
                            raw_lines,
                            scroll: 0,
                        });
                        return None;
                    }
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Tab | KeyCode::Esc) => {
                Some(Action::Nav(NavAction::CycleFocus(Direction::Forward)))
            }
            _ => None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, _area: Rect) -> Option<Action> {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.selected_field > 0 {
                    self.selected_field -= 1;
                }
                Some(Action::Consumed)
            }
            MouseEventKind::ScrollDown => {
                let last = self.columns.len().saturating_sub(1);
                if self.selected_field < last {
                    self.selected_field += 1;
                }
                Some(Action::Consumed)
            }
            _ => None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let block = super::panel_block("Record Detail", focused, theme);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_detection_by_column_type() {
        let val = r#"{"key": "value"}"#;
        assert!(try_parse_json(val, "json").is_some());
        assert!(try_parse_json(val, "JSON").is_some());
        assert!(try_parse_json(val, "jsonb").is_some());
    }

    #[test]
    fn json_detection_by_heuristic() {
        assert!(try_parse_json(r#"{"a": 1}"#, "TEXT").is_some());
        assert!(try_parse_json("[1, 2, 3]", "TEXT").is_some());
    }

    #[test]
    fn json_detection_invalid() {
        assert!(try_parse_json("{not json", "TEXT").is_none());
        assert!(try_parse_json("hello", "TEXT").is_none());
        assert!(try_parse_json("", "TEXT").is_none());
        // Unclosed array bracket
        assert!(try_parse_json("[1, 2", "TEXT").is_none());
    }

    #[test]
    fn json_detection_null_not_json() {
        assert!(try_parse_json("NULL", "json").is_none());
    }

    #[test]
    fn json_compact_indicator_object() {
        let json: serde_json::Value = serde_json::from_str(r#"{"a": 1, "b": 2}"#).unwrap();
        assert_eq!(json_compact_indicator(&json), "{...} (2 keys)");
    }

    #[test]
    fn json_compact_indicator_array() {
        let json: serde_json::Value = serde_json::from_str("[1, 2, 3]").unwrap();
        assert_eq!(json_compact_indicator(&json), "[...] (3 items)");
    }

    #[test]
    fn json_detection_scalar_in_json_column() {
        // Scalars in a json-typed column: valid JSON but not object/array
        assert!(try_parse_json(r#""hello""#, "json").is_some());
        assert!(try_parse_json("42", "json").is_some());
        assert!(try_parse_json("true", "json").is_some());
    }

    #[test]
    fn json_detection_whitespace_padded() {
        // Tier 2 heuristic trims before checking
        assert!(try_parse_json(r#"  {"a": 1}  "#, "TEXT").is_some());
        assert!(try_parse_json("  [1, 2]  ", "TEXT").is_some());
    }

    #[test]
    fn json_compact_indicator_scalar() {
        // Scalar values fall through to json.to_string()
        let json: serde_json::Value = serde_json::from_str("42").unwrap();
        assert_eq!(json_compact_indicator(&json), "42");
        let json: serde_json::Value = serde_json::from_str(r#""hello""#).unwrap();
        assert_eq!(json_compact_indicator(&json), r#""hello""#);
    }
}
