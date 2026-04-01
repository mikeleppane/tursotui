pub(crate) mod autocomplete;
pub(crate) mod bookmarks;
pub(crate) mod cell_editor;
pub(crate) mod data_editor;
pub(crate) mod db_info;
pub(crate) mod dml_preview;
pub(crate) mod editor;
pub(crate) mod er_diagram;
pub(crate) mod explain;
pub(crate) mod export;
pub(crate) mod file_picker;
pub(crate) mod goto_object;
pub(crate) mod help;
pub(crate) mod history;
pub(crate) mod pragmas;
pub(crate) mod profile;
pub(crate) mod record;
pub(crate) mod results;
pub(crate) mod schema;
pub(crate) mod schema_diff;
pub(crate) mod status_bar;
pub(crate) mod text_buffer;

use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType};

use crate::app::Action;
use crate::theme::Theme;

/// Create a consistently-styled panel block with rounded borders and padded title.
///
/// All main panels use this to ensure a cohesive look. Focused panels get an accent-
/// colored border and bold title; unfocused panels get a subtle border.
pub(crate) fn panel_block(title: &str, focused: bool, theme: &Theme) -> Block<'static> {
    let (border_style, title_style) = if focused {
        (
            Style::default().fg(theme.border_focused),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            Style::default().fg(theme.border),
            Style::default().fg(theme.dim),
        )
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(format!(" {title} "))
        .title_style(title_style)
}

/// Create a styled block for overlay popups (help, export, history, DML preview).
///
/// Overlays always use accent borders and centered titles.
pub(crate) fn overlay_block(title: &str, theme: &Theme) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(format!(" {title} "))
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(theme.bg).fg(theme.fg))
}

/// Every panel in the UI implements this trait.
pub(crate) trait Component {
    /// Handle a key event when this component has focus.
    /// Returns `Some(Action)` if the key produced a state change, `None` if ignored.
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action>;

    /// Handle a mouse event. Default: ignore.
    #[allow(dead_code)]
    fn handle_mouse(&mut self, _mouse: MouseEvent) -> Option<Action> {
        None
    }

    /// React to an action dispatched by the app. Default: no-op.
    #[allow(dead_code)]
    fn update(&mut self, _action: &Action) {}

    /// Render into the given area. `focused` indicates whether this panel has keyboard focus.
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme);
}
