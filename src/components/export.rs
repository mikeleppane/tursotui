//! Export popup overlay.
//!
//! Modal dialog for exporting the current result set to CSV, JSON, or SQL INSERT.
//! Supports clipboard and file targets. Shows a table name input for SQL INSERT.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};

use crate::app::Action;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    Csv,
    Json,
    SqlInsert,
}

impl ExportFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::SqlInsert => "sql",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::SqlInsert => "SQL INSERT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportTarget {
    Clipboard,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditingField {
    None,
    FilePath,
    TableName,
}

pub(crate) struct ExportPopup {
    pub(crate) format: ExportFormat,
    pub(crate) target: ExportTarget,
    pub(crate) file_path: String,
    pub(crate) table_name: String,
    pub(crate) row_count: usize,
    editing: EditingField,
    cursor_pos: usize,
}

impl ExportPopup {
    pub(crate) fn new(row_count: usize, table_name: String) -> Self {
        // Use seconds since epoch as timestamp (no chrono dependency needed)
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let safe_name: String = table_name
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let default_path = format!("./export_{safe_name}_{timestamp}.csv");
        Self {
            format: ExportFormat::Csv,
            target: ExportTarget::Clipboard,
            file_path: default_path,
            table_name,
            row_count,
            editing: EditingField::None,
            cursor_pos: 0,
        }
    }

    fn update_file_extension(&mut self) {
        // Update the extension portion of the file path if it ends with a known ext
        if let Some(dot_pos) = self.file_path.rfind('.') {
            let ext = &self.file_path[dot_pos + 1..];
            if matches!(ext, "csv" | "json" | "sql") {
                self.file_path =
                    format!("{}.{}", &self.file_path[..dot_pos], self.format.extension());
                // Reset cursor to end of path after extension change
                self.cursor_pos = self.file_path.chars().count();
            }
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        // If editing a text field, handle text input
        if self.editing != EditingField::None {
            return self.handle_editing_key(key);
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::ShowExport), // toggle off
            (KeyModifiers::NONE, KeyCode::Char('c')) => {
                self.format = ExportFormat::Csv;
                self.update_file_extension();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('j')) => {
                self.format = ExportFormat::Json;
                self.update_file_extension();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('s')) => {
                self.format = ExportFormat::SqlInsert;
                self.update_file_extension();
                None
            }
            (KeyModifiers::NONE, KeyCode::Tab) => {
                self.target = match self.target {
                    ExportTarget::Clipboard => ExportTarget::File,
                    ExportTarget::File => ExportTarget::Clipboard,
                };
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                // Enter file path editing mode
                self.editing = EditingField::FilePath;
                self.cursor_pos = self.file_path.chars().count();
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('t')) => {
                // Enter table name editing mode (only for SQL INSERT)
                if self.format == ExportFormat::SqlInsert {
                    self.editing = EditingField::TableName;
                    self.cursor_pos = self.table_name.chars().count();
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                // Signal that export should proceed -- the main loop handles the actual export
                Some(Action::ExecuteExport)
            }
            (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(Action::Quit),
            _ => None,
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> Option<Action> {
        let field = match self.editing {
            EditingField::FilePath => &mut self.file_path,
            EditingField::TableName => &mut self.table_name,
            EditingField::None => return None,
        };

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc | KeyCode::Enter) => {
                self.editing = EditingField::None;
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(ch)) => {
                let byte_idx = field
                    .char_indices()
                    .nth(self.cursor_pos)
                    .map_or(field.len(), |(idx, _)| idx);
                field.insert(byte_idx, ch);
                self.cursor_pos += 1;
                None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    let byte_idx = field
                        .char_indices()
                        .nth(self.cursor_pos)
                        .map_or(field.len(), |(idx, _)| idx);
                    field.remove(byte_idx);
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                let max = field.chars().count();
                if self.cursor_pos < max {
                    let byte_idx = field
                        .char_indices()
                        .nth(self.cursor_pos)
                        .map_or(field.len(), |(idx, _)| idx);
                    field.remove(byte_idx);
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.cursor_pos = self.cursor_pos.saturating_sub(1);
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                let max = field.chars().count();
                if self.cursor_pos < max {
                    self.cursor_pos += 1;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Home) => {
                self.cursor_pos = 0;
                None
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                self.cursor_pos = field.chars().count();
                None
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let popup_width = 60_u16.min(area.width.saturating_sub(4));
        let desired_height = if self.format == ExportFormat::SqlInsert {
            14_u16 // extra row for table name
        } else {
            12_u16
        };
        let popup_height = desired_height.min(area.height.saturating_sub(2));

        let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let title = format!("Export Results ({} rows)", self.row_count);
        let block = super::overlay_block(&title, theme);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let mut y_offset = 0;

        // Format options
        let formats = [
            ('c', ExportFormat::Csv),
            ('j', ExportFormat::Json),
            ('s', ExportFormat::SqlInsert),
        ];
        for (key, fmt) in &formats {
            let selected = *fmt == self.format;
            let bullet = if selected { ">" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            };
            let line = format!(" {bullet} [{key}] {}", fmt.label());
            let row_area = Rect::new(inner.x, inner.y + y_offset, inner.width, 1);
            frame.render_widget(Paragraph::new(line).style(style), row_area);
            y_offset += 1;
        }

        y_offset += 1; // spacer

        // Target
        let clipboard_marker = if self.target == ExportTarget::Clipboard {
            ">"
        } else {
            " "
        };
        let file_marker = if self.target == ExportTarget::File {
            ">"
        } else {
            " "
        };

        let target_label = format!(" {clipboard_marker} Clipboard  {file_marker} File [Tab]");
        let row_area = Rect::new(inner.x, inner.y + y_offset, inner.width, 1);
        frame.render_widget(
            Paragraph::new(target_label).style(Style::default().fg(theme.fg)),
            row_area,
        );
        y_offset += 1;

        // File path (shown when target is File)
        if self.target == ExportTarget::File {
            y_offset += 1;
            let editing = self.editing == EditingField::FilePath;
            let path_style = if editing {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.fg)
            };
            let label = if editing {
                format!(" Path: {}", insert_cursor(&self.file_path, self.cursor_pos))
            } else {
                format!(" [f]ile: {}", self.file_path)
            };
            let row_area = Rect::new(inner.x, inner.y + y_offset, inner.width, 1);
            frame.render_widget(Paragraph::new(label).style(path_style), row_area);
        }
        y_offset += 1;

        // Table name (SQL INSERT only)
        if self.format == ExportFormat::SqlInsert {
            y_offset += 1;
            let editing = self.editing == EditingField::TableName;
            let name_style = if editing {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.fg)
            };
            let label = if editing {
                format!(
                    " Table: {}",
                    insert_cursor(&self.table_name, self.cursor_pos)
                )
            } else {
                format!(" [t]able: {}", self.table_name)
            };
            let row_area = Rect::new(inner.x, inner.y + y_offset, inner.width, 1);
            frame.render_widget(Paragraph::new(label).style(name_style), row_area);
        }

        // Footer
        let footer_y = popup_area.y + popup_area.height.saturating_sub(2);
        let footer_area = Rect::new(inner.x, footer_y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(" Enter: Export  Esc: Cancel").style(
                Style::default()
                    .fg(theme.border)
                    .add_modifier(Modifier::DIM),
            ),
            footer_area,
        );
    }
}

/// Insert a cursor marker (`_`) at the given character position in a string.
fn insert_cursor(text: &str, cursor_pos: usize) -> String {
    let byte_idx = text
        .char_indices()
        .nth(cursor_pos)
        .map_or(text.len(), |(idx, _)| idx);
    format!("{}_{}", &text[..byte_idx], &text[byte_idx..])
}

/// Best-effort table name extraction for export file naming.
///
/// Unlike `detect_source_table` (which rejects complex queries), this always
/// tries to extract the first table name after FROM — even from JOINs, GROUP BYs,
/// etc. Falls back to `"table_name"` only when no FROM clause is found.
pub(crate) fn infer_table_name(sql: &str) -> String {
    tursotui_sql::parser::find_from_keyword(sql)
        .map(|pos| tursotui_sql::parser::extract_table_name(&sql[pos..]))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "table_name".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_from_simple_select() {
        assert_eq!(infer_table_name("SELECT * FROM users"), "users");
    }

    #[test]
    fn infer_from_select_with_where() {
        assert_eq!(
            infer_table_name("SELECT * FROM orders WHERE id > 5"),
            "orders"
        );
    }

    #[test]
    fn infer_fallback() {
        assert_eq!(infer_table_name("SELECT 1 + 2"), "table_name");
    }

    #[test]
    fn infer_quoted_table() {
        assert_eq!(infer_table_name("SELECT * FROM \"my table\""), "my table");
    }

    #[test]
    fn infer_join_query_extracts_first_table() {
        // Best-effort: extracts the first table even from JOINs
        assert_eq!(
            infer_table_name("SELECT * FROM users JOIN orders ON users.id = orders.user_id"),
            "users"
        );
    }

    #[test]
    fn infer_group_by_extracts_table() {
        // Best-effort: extracts table even from GROUP BY queries
        assert_eq!(
            infer_table_name("SELECT count(*) FROM orders GROUP BY status"),
            "orders"
        );
    }
}
