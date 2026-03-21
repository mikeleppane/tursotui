pub(crate) mod editor;
pub(crate) mod help;
pub(crate) mod placeholder;
pub(crate) mod record;
pub(crate) mod results;
pub(crate) mod schema;
pub(crate) mod status_bar;

use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::prelude::*;

use crate::app::Action;
use crate::theme::Theme;

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
