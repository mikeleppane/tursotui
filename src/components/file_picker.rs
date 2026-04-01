//! File picker overlay.
//!
//! Popup dialog for opening additional database files. Supports path text input
//! with filesystem tab-completion. Filters for common `SQLite` extensions
//! (`.db`, `.sqlite`, `.sqlite3`, `.db3`) plus directories.

use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{Action, NavAction};
use crate::theme::Theme;

/// Recognized database file extensions for tab-completion filtering.
const DB_EXTENSIONS: &[&str] = &["db", "sqlite", "sqlite3", "db3"];

pub(crate) struct FilePicker {
    input: String,
    cursor: usize,            // character index (not byte offset)
    completions: Vec<String>, // cached tab-completion candidates
    completion_idx: usize,    // current position in completion cycle
}

impl FilePicker {
    /// Create a new file picker pre-populated with the starting directory.
    ///
    /// If `active_db_path` is a file path, uses its parent directory.
    /// If `:memory:`, uses the current working directory.
    pub(crate) fn new(active_db_path: &str) -> Self {
        let starting_dir = compute_starting_dir(active_db_path);
        let input =
            if starting_dir.ends_with('/') || starting_dir.ends_with(std::path::MAIN_SEPARATOR) {
                starting_dir
            } else {
                format!("{starting_dir}/")
            };
        let cursor = input.chars().count();
        Self {
            input,
            cursor,
            completions: Vec::new(),
            completion_idx: 0,
        }
    }

    /// Handle a key event. Returns `Some(Action)` to dismiss or open a database.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                // Dismiss the file picker via toggle
                Some(Action::Nav(NavAction::OpenFilePicker)) // toggle off
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let path_str = self.input.trim();
                if path_str.is_empty() {
                    return None;
                }
                Some(Action::Nav(NavAction::OpenDatabase(PathBuf::from(
                    path_str,
                ))))
            }
            (KeyModifiers::NONE, KeyCode::Tab) => {
                self.complete();
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(ch)) => {
                self.clear_completions();
                let byte_idx = char_to_byte_idx(&self.input, self.cursor);
                self.input.insert(byte_idx, ch);
                self.cursor += 1;
                None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.clear_completions();
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let byte_idx = char_to_byte_idx(&self.input, self.cursor);
                    self.input.remove(byte_idx);
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                self.clear_completions();
                let char_count = self.input.chars().count();
                if self.cursor < char_count {
                    let byte_idx = char_to_byte_idx(&self.input, self.cursor);
                    self.input.remove(byte_idx);
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.clear_completions();
                self.cursor = self.cursor.saturating_sub(1);
                None
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                self.clear_completions();
                let max = self.input.chars().count();
                if self.cursor < max {
                    self.cursor += 1;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Home) => {
                self.clear_completions();
                self.cursor = 0;
                None
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                self.clear_completions();
                self.cursor = self.input.chars().count();
                None
            }
            (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(Action::Quit),
            _ => None,
        }
    }

    /// Render the file picker overlay centered in the given area.
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Width: 60% of terminal, clamped to 40..80
        let raw_width = (u32::from(area.width) * 60 / 100) as u16;
        let popup_width = raw_width.clamp(40, 80).min(area.width.saturating_sub(2));
        let popup_height = 5_u16.min(area.height.saturating_sub(2));

        let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let block = super::overlay_block("Open Database", theme);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        // Path input line
        let path_display = insert_cursor(&self.input, self.cursor);
        let path_line = format!(" Path: {path_display}");
        let path_area = Rect::new(inner.x, inner.y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(path_line).style(Style::default().fg(theme.accent)),
            path_area,
        );

        // Completion hint (show current completion if cycling)
        if !self.completions.is_empty() {
            let hint = format!(" ({}/{})", self.completion_idx + 1, self.completions.len());
            let hint_area = Rect::new(inner.x, inner.y + 1, inner.width, 1);
            frame.render_widget(
                Paragraph::new(hint).style(Style::default().fg(theme.dim)),
                hint_area,
            );
        }

        // Footer
        let footer_y = inner.y + inner.height.saturating_sub(1);
        let footer_area = Rect::new(inner.x, footer_y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(" Tab: complete  Enter: open  Esc: cancel").style(
                Style::default()
                    .fg(theme.border)
                    .add_modifier(Modifier::DIM),
            ),
            footer_area,
        );
    }

    /// Perform filesystem tab-completion.
    fn complete(&mut self) {
        // If we already have completions, cycle to the next one
        if !self.completions.is_empty() {
            self.completion_idx = (self.completion_idx + 1) % self.completions.len();
            self.input
                .clone_from(&self.completions[self.completion_idx]);
            self.cursor = self.input.chars().count();
            return;
        }

        // Build completions from filesystem
        let candidates = build_completions(&self.input);
        if candidates.is_empty() {
            return;
        }

        if candidates.len() == 1 {
            // Single match -- apply directly, no cycling
            self.input.clone_from(&candidates[0]);
            self.cursor = self.input.chars().count();
            // Don't store as completions since there's nothing to cycle
            return;
        }

        // Multiple matches -- apply first and store for cycling
        self.completions = candidates;
        self.completion_idx = 0;
        self.input.clone_from(&self.completions[0]);
        self.cursor = self.input.chars().count();
    }

    fn clear_completions(&mut self) {
        self.completions.clear();
        self.completion_idx = 0;
    }
}

/// Helper to get CWD as a string, falling back to "." on error.
fn cwd_string() -> String {
    std::env::current_dir().map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())
}

/// Compute the starting directory for the file picker.
fn compute_starting_dir(active_db_path: &str) -> String {
    if active_db_path == ":memory:" {
        return cwd_string();
    }

    let path = Path::new(active_db_path);
    if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            // Relative path like "test.db" -- use CWD
            cwd_string()
        } else if parent.exists() {
            parent.to_string_lossy().to_string()
        } else {
            // Parent directory doesn't exist -- fall back to CWD
            cwd_string()
        }
    } else {
        cwd_string()
    }
}

/// Build filesystem completion candidates from the current input.
fn build_completions(input: &str) -> Vec<String> {
    let path = Path::new(input);

    // Split into directory and prefix portions
    let (dir, prefix) = if input.ends_with('/') || input.ends_with(std::path::MAIN_SEPARATOR) {
        // Input ends with separator -- list everything in that directory
        (path.to_path_buf(), String::new())
    } else {
        // Input like "/home/user/te" -> dir="/home/user", prefix="te"
        let dir = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let prefix = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        (dir, prefix)
    };

    // Try to read the directory
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let prefix_lower = prefix.to_lowercase();
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let name_lower = name.to_lowercase();

        // Skip hidden files/dirs
        if name.starts_with('.') {
            continue;
        }

        // Prefix match
        if !prefix.is_empty() && !name_lower.starts_with(&prefix_lower) {
            continue;
        }

        let is_dir = entry.path().is_dir(); // follows symlinks
        if is_dir {
            let full_path = dir.join(&name);
            let mut s = full_path.to_string_lossy().to_string();
            s.push('/');
            dirs.push(s);
        } else if has_db_extension(&name) {
            let full_path = dir.join(&name);
            files.push(full_path.to_string_lossy().to_string());
        }
    }

    // Sort alphabetically, directories first
    dirs.sort_unstable();
    files.sort_unstable();
    dirs.extend(files);
    dirs
}

/// Check if a filename has a recognized `SQLite` extension.
fn has_db_extension(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    DB_EXTENSIONS
        .iter()
        .any(|ext| name_lower.ends_with(&format!(".{ext}")))
}

/// Insert a cursor marker at the given character position in a string.
fn insert_cursor(text: &str, cursor_pos: usize) -> String {
    let byte_idx = char_to_byte_idx(text, cursor_pos);
    format!("{}_{}", &text[..byte_idx], &text[byte_idx..])
}

/// Convert a character index to a byte index.
fn char_to_byte_idx(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(idx, _)| idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_dir_for_memory() {
        let dir = compute_starting_dir(":memory:");
        assert!(!dir.is_empty());
    }

    #[test]
    fn starting_dir_for_file() {
        // Use a path whose parent actually exists (the temp dir)
        let tmp = std::env::temp_dir();
        let fake_db = tmp.join("test.db");
        let dir = compute_starting_dir(&fake_db.to_string_lossy());
        assert_eq!(dir, tmp.to_string_lossy());
    }

    #[test]
    fn starting_dir_for_nonexistent_parent_falls_back() {
        // When parent dir doesn't exist, falls back to CWD
        let dir = compute_starting_dir("/nonexistent_dir_12345/test.db");
        // Should be CWD, not the nonexistent parent
        let cwd = cwd_string();
        assert_eq!(dir, cwd);
    }

    #[test]
    fn starting_dir_for_relative() {
        let dir = compute_starting_dir("test.db");
        // Should resolve to CWD, not empty
        assert!(!dir.is_empty());
        assert_ne!(dir, "");
    }

    #[test]
    fn has_db_extension_positive() {
        assert!(has_db_extension("myfile.db"));
        assert!(has_db_extension("data.sqlite"));
        assert!(has_db_extension("store.sqlite3"));
        assert!(has_db_extension("backup.db3"));
    }

    #[test]
    fn has_db_extension_negative() {
        assert!(!has_db_extension("readme.txt"));
        assert!(!has_db_extension("image.png"));
        assert!(!has_db_extension("noext"));
    }

    #[test]
    fn has_db_extension_case_insensitive() {
        assert!(has_db_extension("MYFILE.DB"));
        assert!(has_db_extension("Data.SQLite3"));
    }

    #[test]
    fn insert_cursor_at_end() {
        assert_eq!(insert_cursor("hello", 5), "hello_");
    }

    #[test]
    fn insert_cursor_at_start() {
        assert_eq!(insert_cursor("hello", 0), "_hello");
    }

    #[test]
    fn insert_cursor_middle() {
        assert_eq!(insert_cursor("hello", 2), "he_llo");
    }

    #[test]
    fn char_to_byte_idx_ascii() {
        assert_eq!(char_to_byte_idx("hello", 2), 2);
        assert_eq!(char_to_byte_idx("hello", 5), 5);
    }

    #[test]
    fn char_to_byte_idx_beyond() {
        assert_eq!(char_to_byte_idx("hi", 10), 2);
    }

    #[test]
    fn build_completions_nonexistent_dir() {
        let result = build_completions("/nonexistent_dir_12345/");
        assert!(result.is_empty());
    }

    #[test]
    fn build_completions_skips_hidden() {
        // hidden files (.foo) should not appear in completions
        let dir = std::env::temp_dir().join("tursotui_test_hidden");
        let _ = std::fs::create_dir_all(&dir);
        let hidden = dir.join(".hidden.db");
        let visible = dir.join("visible.db");
        let _ = std::fs::write(&hidden, "");
        let _ = std::fs::write(&visible, "");

        let input = format!("{}/", dir.to_string_lossy());
        let results = build_completions(&input);
        assert!(results.iter().any(|r| r.contains("visible.db")));
        assert!(!results.iter().any(|r| r.contains(".hidden.db")));

        let _ = std::fs::remove_file(hidden);
        let _ = std::fs::remove_file(visible);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn build_completions_prefix_filter() {
        let dir = std::env::temp_dir().join("tursotui_test_prefix");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("alpha.db"), "");
        let _ = std::fs::write(dir.join("beta.db"), "");

        let input = format!("{}/al", dir.to_string_lossy());
        let results = build_completions(&input);
        assert!(results.iter().any(|r| r.contains("alpha.db")));
        assert!(!results.iter().any(|r| r.contains("beta.db")));

        let _ = std::fs::remove_file(dir.join("alpha.db"));
        let _ = std::fs::remove_file(dir.join("beta.db"));
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn file_picker_new_sets_trailing_slash() {
        let fp = FilePicker::new(":memory:");
        assert!(fp.input.ends_with('/'));
    }

    #[test]
    fn file_picker_cursor_starts_at_end() {
        let fp = FilePicker::new(":memory:");
        let char_count = fp.input.chars().count();
        assert_eq!(fp.cursor, char_count);
    }
}
