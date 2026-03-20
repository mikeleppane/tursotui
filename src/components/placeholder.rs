use ratatui::crossterm::event::KeyEvent;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use crate::app::Action;
use crate::theme::Theme;

use super::Component;

/// Temporary placeholder panel -- renders a bordered box with a label.
/// Replaced by real components in later milestones.
pub(crate) struct Placeholder {
    label: String,
}

impl Placeholder {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl Component for Placeholder {
    fn handle_key(&mut self, _key: KeyEvent) -> Option<Action> {
        None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let border_style = if focused {
            Style::default().fg(theme.border_focused)
        } else {
            Style::default().fg(theme.border)
        };

        let block = Block::bordered()
            .border_style(border_style)
            .title(self.label.as_str())
            .title_style(if focused {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg)
            });

        let content = Paragraph::new(format!("[{}]", self.label))
            .style(Style::default().fg(theme.fg))
            .alignment(Alignment::Center)
            .block(block);

        frame.render_widget(content, area);
    }
}
