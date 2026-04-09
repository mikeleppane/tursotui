use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{Action, Direction, NavAction, SubTab, UiAction};

// Note: Multi-database key bindings (Ctrl+PgUp/PgDn/W) are documented in
// the help overlay (components/help.rs).

/// Poll for a crossterm event with the given timeout.
/// Returns `None` if no event occurred within the timeout.
pub(crate) fn poll_event(timeout: Duration) -> std::io::Result<Option<Event>> {
    if event::poll(timeout)? {
        Ok(Some(event::read()?))
    } else {
        Ok(None)
    }
}

/// Map a key event to a global Action (fallback handler).
///
/// Called AFTER the focused component's `handle_key`. If the component consumed
/// the key (returned `Some(Action)`), this function is never reached. Only keys
/// that the focused component ignored arrive here.
///
/// Bare `Tab`, `Shift+Tab`, and `Esc` are intentionally absent — those are
/// handled by focused components. The editor uses `Tab` for indentation and
/// `Esc` to release focus. Non-editor components emit `CycleFocus` from their
/// own `handle_key` when they receive `Tab`/`Esc`.
///
/// `Ctrl+Tab` is the only unconditional focus-cycle binding — it always works
/// regardless of which component is focused.
pub(crate) fn map_global_key(key: KeyEvent) -> Option<Action> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match (key.modifiers, key.code) {
        // Quit
        (KeyModifiers::CONTROL, KeyCode::Char('q')) => Some(Action::Quit),

        // Focus cycling — only Ctrl+Tab is global; bare Tab/Esc are component-handled.
        // Note: Ctrl+Tab is not reliably transmitted by all terminals (xterm, older tmux).
        // Terminals supporting the kitty keyboard protocol deliver it correctly.
        // Bare Tab/Esc in component handle_key provides the fallback for non-editor panels.
        (KeyModifiers::CONTROL, KeyCode::Tab) => {
            Some(Action::Nav(NavAction::CycleFocus(Direction::Forward)))
        }

        // Sidebar toggle
        (KeyModifiers::CONTROL, KeyCode::Char('b')) => Some(Action::Nav(NavAction::ToggleSidebar)),

        // Sub-tab switching
        (KeyModifiers::ALT, KeyCode::Char('1')) => {
            Some(Action::Nav(NavAction::SwitchSubTab(SubTab::Query)))
        }
        (KeyModifiers::ALT, KeyCode::Char('2')) => {
            Some(Action::Nav(NavAction::SwitchSubTab(SubTab::Admin)))
        }

        // Theme toggle
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => Some(Action::Ui(UiAction::ToggleTheme)),

        // Help
        (KeyModifiers::NONE, KeyCode::F(1))
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('?')) => {
            Some(Action::Ui(UiAction::ShowHelp))
        }

        // History
        (KeyModifiers::CONTROL, KeyCode::Char('h')) => Some(Action::Ui(UiAction::ShowHistory)),

        // Bookmarks
        (KeyModifiers::NONE, KeyCode::F(3)) => Some(Action::Ui(UiAction::ShowBookmarks)),

        // ER Diagram fullscreen
        (KeyModifiers::NONE, KeyCode::F(6)) => Some(Action::Ui(UiAction::ShowERDiagram)),

        // Schema Diff
        (KeyModifiers::NONE, KeyCode::F(7)) => Some(Action::Ui(UiAction::ShowSchemaDiff)),

        // Export popup — Ctrl+E (traditional terminals can't distinguish Ctrl+Shift+E).
        // When editor is focused, Ctrl+E is consumed as end-of-line and never reaches here.
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => Some(Action::Ui(UiAction::ShowExport)),
        // Ctrl+Shift+E also works in kitty-protocol terminals
        (m, KeyCode::Char('E')) if m == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
            Some(Action::Ui(UiAction::ShowExport))
        }

        // Quick export: copy all results as TSV — Ctrl+Shift+C or plain Ctrl+C with shift.
        // Note: Ctrl+Shift+C is the standard terminal copy shortcut on Linux.
        // Some terminals intercept it before the application receives it.
        // Terminals supporting the kitty keyboard protocol deliver it correctly.
        (m, KeyCode::Char('c' | 'C')) if m == KeyModifiers::CONTROL | KeyModifiers::SHIFT => {
            Some(Action::Ui(UiAction::CopyAllResults))
        }

        // File picker
        (KeyModifiers::CONTROL, KeyCode::Char('o')) => Some(Action::Nav(NavAction::OpenFilePicker)),

        // Go to Object
        (KeyModifiers::CONTROL, KeyCode::Char('p')) => Some(Action::Nav(NavAction::OpenGoToObject)),

        // Panel resizing
        (KeyModifiers::CONTROL, KeyCode::Left) => Some(Action::Ui(UiAction::ResizeSidebar(-5))),
        (KeyModifiers::CONTROL, KeyCode::Right) => Some(Action::Ui(UiAction::ResizeSidebar(5))),
        (KeyModifiers::CONTROL, KeyCode::Up) => Some(Action::Ui(UiAction::ResizeEditor(-5))),
        (KeyModifiers::CONTROL, KeyCode::Down) => Some(Action::Ui(UiAction::ResizeEditor(5))),

        // Multi-database tab switching
        (KeyModifiers::CONTROL, KeyCode::PageDown) => Some(Action::Nav(NavAction::NextDatabase)),
        (KeyModifiers::CONTROL, KeyCode::PageUp) => Some(Action::Nav(NavAction::PrevDatabase)),
        (KeyModifiers::CONTROL, KeyCode::Char('w')) => {
            Some(Action::Nav(NavAction::CloseActiveDatabase))
        }

        // Mouse mode toggle — kitty keyboard protocol only.
        // In standard VT terminals, Ctrl+M sends \r (same as Enter) and is not
        // distinguishable. crossterm maps it to KeyCode::Enter, not Char('m').
        (KeyModifiers::CONTROL, KeyCode::Char('m')) => Some(Action::Ui(UiAction::ToggleMouseMode)),

        _ => None,
    }
}
