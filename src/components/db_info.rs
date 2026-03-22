use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, Direction};
use crate::db::DbInfo;
use crate::theme::Theme;

use super::Component;

/// Pre-computed layout dimensions for key-value rendering.
struct KvLayout {
    label_width: u16,
    value_start: u16,
    value_width: u16,
    content_width: u16,
}

/// Format a byte count as a human-readable string (e.g., "2.4 MB", "156 KB").
#[allow(clippy::cast_precision_loss)] // precision loss acceptable for display formatting
fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} bytes")
    }
}

/// Database Info panel showing metadata from PRAGMAs and file system.
///
/// Loaded lazily on first Admin tab switch, refreshed on `r`.
/// Does NOT auto-refresh on every tab switch.
pub(crate) struct DbInfoPanel {
    info: Option<DbInfo>,
    loading: bool,
    checkpointing: bool,
    scroll_offset: usize,
}

impl DbInfoPanel {
    pub(crate) fn new() -> Self {
        Self {
            info: None,
            loading: false,
            checkpointing: false,
            scroll_offset: 0,
        }
    }

    /// Attempt initial load. Returns true (and sets `loading = true`) only if
    /// data hasn't been loaded yet and no load is in progress.
    /// Used by `SwitchSubTab(Admin)` for lazy one-time initialization.
    pub(crate) fn try_start_load(&mut self) -> bool {
        if self.loading || self.info.is_some() {
            return false;
        }
        self.loading = true;
        true
    }

    /// Force a refresh even when data is already loaded. Returns true (and sets
    /// `loading = true`) if no load is already in progress.
    /// Used by the `r` key (`RefreshDbInfo` action).
    pub(crate) fn try_start_refresh(&mut self) -> bool {
        if self.loading {
            return false;
        }
        self.loading = true;
        true
    }

    /// Store loaded database info.
    pub(crate) fn set_info(&mut self, info: DbInfo) {
        self.info = Some(info);
        self.loading = false;
    }

    /// Clear loading flag on failure (info stays None or stale).
    pub(crate) fn set_loading_failed(&mut self) {
        self.loading = false;
    }

    /// Whether the panel is currently running a checkpoint task.
    pub(crate) fn checkpointing(&self) -> bool {
        self.checkpointing
    }

    /// Set the checkpointing flag.
    pub(crate) fn set_checkpointing(&mut self, v: bool) {
        self.checkpointing = v;
    }

    /// Whether info has been loaded.
    pub(crate) fn info(&self) -> Option<&DbInfo> {
        self.info.as_ref()
    }

    /// Build the key-value lines to display.
    /// Returns a list of (label, value) pairs.
    fn info_lines(info: &DbInfo) -> Vec<(&'static str, String)> {
        let mut lines = Vec::with_capacity(12);

        lines.push(("File path", info.file_path.clone()));
        lines.push((
            "File size",
            info.file_size
                .map_or_else(|| "N/A".to_string(), format_file_size),
        ));
        lines.push(("Page count", info.page_count.to_string()));
        lines.push(("Page size", format!("{} bytes", info.page_size)));
        lines.push(("Encoding", info.encoding.clone()));
        let journal_display = if info.journal_mode.eq_ignore_ascii_case("mvcc") {
            format!("{} (Turso concurrent writes)", info.journal_mode)
        } else {
            info.journal_mode.clone()
        };
        lines.push(("Journal mode", journal_display));
        lines.push(("Schema version", info.schema_version.to_string()));
        lines.push(("Freelist pages", info.freelist_count.to_string()));
        lines.push(("Turso version", info.turso_version.to_string()));

        // WAL section: only if journal_mode is "wal" or "mvcc" (case-insensitive)
        if info.journal_mode.eq_ignore_ascii_case("wal")
            || info.journal_mode.eq_ignore_ascii_case("mvcc")
        {
            let frames = info
                .wal_frames
                .map_or_else(|| "0".to_string(), |f| f.to_string());
            lines.push(("WAL frames", frames));
        }

        lines
    }

    /// Clamp `scroll_offset` to valid range for the content.
    fn clamp_scroll(&mut self, content_height: usize, viewport_height: usize) {
        if viewport_height == 0 || content_height == 0 {
            self.scroll_offset = 0;
            return;
        }
        let max_scroll = content_height.saturating_sub(viewport_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
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

    /// Render the key-value info content and optional checkpoint hint.
    fn render_info_content(
        &self,
        frame: &mut Frame,
        inner: Rect,
        scroll_offset: usize,
        info: &DbInfo,
        theme: &Theme,
    ) {
        let lines = Self::info_lines(info);
        let is_wal = info.journal_mode.eq_ignore_ascii_case("wal")
            || info.journal_mode.eq_ignore_ascii_case("mvcc");

        // Total content lines: info lines + optional blank + checkpoint hint
        let extra_lines = usize::from(is_wal) * 2; // blank line + hint line
        let content_height = lines.len() + extra_lines;

        let viewport_height = inner.height as usize;

        let has_scrollbar = content_height > viewport_height;
        let content_width = if has_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        // Calculate layout dimensions
        let label_width = lines
            .iter()
            .map(|(label, _)| UnicodeWidthStr::width(*label) as u16)
            .max()
            .unwrap_or(0)
            .min(content_width / 2);
        let gap: u16 = 2;
        let layout = KvLayout {
            label_width,
            value_start: label_width + gap,
            value_width: content_width.saturating_sub(label_width + gap),
            content_width,
        };

        // Render visible lines
        let visible_end = (scroll_offset + viewport_height).min(content_height);
        for draw_idx in scroll_offset..visible_end {
            let y = inner.y + (draw_idx - scroll_offset) as u16;

            if draw_idx < lines.len() {
                Self::render_kv_line(frame, inner.x, y, &lines[draw_idx], &layout, theme);
            } else if draw_idx == lines.len() + 1 && is_wal {
                // Checkpoint hint line (after blank line)
                Self::render_hint_line(frame, inner.x, y, content_width, self.checkpointing, theme);
            }
            // Blank line between info and hint is implicitly empty
        }

        // Scrollbar
        if has_scrollbar {
            let mut scrollbar_state = ScrollbarState::new(content_height)
                .position(scroll_offset)
                .viewport_content_length(viewport_height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                inner,
                &mut scrollbar_state,
            );
        }
    }

    /// Render a single key-value line.
    fn render_kv_line(
        frame: &mut Frame,
        x: u16,
        y: u16,
        (label, value): &(&str, String),
        layout: &KvLayout,
        theme: &Theme,
    ) {
        let label_area = Rect::new(x, y, layout.label_width.min(layout.content_width), 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                *label,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            label_area,
        );

        if layout.value_width > 0 {
            let value_area = Rect::new(x + layout.value_start, y, layout.value_width, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(value.as_str(), Style::default().fg(theme.fg))),
                value_area,
            );
        }
    }

    /// Render the checkpoint hint line, reflecting in-progress state.
    fn render_hint_line(
        frame: &mut Frame,
        x: u16,
        y: u16,
        content_width: u16,
        checkpointing: bool,
        theme: &Theme,
    ) {
        let hint = if checkpointing {
            "[c] Checkpointing..."
        } else {
            "[c] Checkpoint"
        };
        let hint_style = if checkpointing {
            Style::default().fg(theme.warning)
        } else {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::DIM)
        };
        let hint_area = Rect::new(
            x,
            y,
            (UnicodeWidthStr::width(hint) as u16).min(content_width),
            1,
        );
        frame.render_widget(Paragraph::new(Span::styled(hint, hint_style)), hint_area);
    }
}

impl Component for DbInfoPanel {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('r')) => Some(Action::RefreshDbInfo),
            (KeyModifiers::NONE, KeyCode::Char('c')) => Some(Action::WalCheckpoint),
            (KeyModifiers::NONE, KeyCode::Char('i')) => Some(Action::IntegrityCheck),
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.scroll_offset = 0;
                None
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.scroll_offset = usize::MAX;
                None
            }
            (KeyModifiers::NONE, KeyCode::Tab | KeyCode::Esc) => {
                Some(Action::CycleFocus(Direction::Forward))
            }
            _ => None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let block = super::panel_block("Database Info", focused, theme);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Clamp scroll before borrowing self.info to avoid &mut self conflict
        if let Some(info) = &self.info {
            let lines = Self::info_lines(info);
            let is_wal = info.journal_mode.eq_ignore_ascii_case("wal")
                || info.journal_mode.eq_ignore_ascii_case("mvcc");
            let extra = usize::from(is_wal) * 2;
            self.clamp_scroll(lines.len() + extra, inner.height as usize);
        }

        match &self.info {
            None if self.loading => {
                Self::render_centered(frame, inner, "Loading...", theme);
            }
            None => {
                Self::render_centered(frame, inner, "Press r to load", theme);
            }
            Some(info) => {
                self.render_info_content(frame, inner, self.scroll_offset, info, theme);
            }
        }
    }
}
