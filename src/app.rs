use std::time::Instant;

use crate::db::{ColumnInfo, DatabaseHandle, QueryResult, SchemaEntry};
use crate::theme::{DARK_THEME, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubTab {
    Query,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BottomTab {
    Results,
    #[allow(dead_code)]
    Explain,
    #[allow(dead_code)]
    Detail,
    #[allow(dead_code)]
    ERDiagram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelId {
    Schema,
    Editor,
    Bottom,
    DbInfo,
    Pragmas,
}

impl std::fmt::Display for PanelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema => write!(f, "Schema"),
            Self::Editor => write!(f, "Editor"),
            Self::Bottom => write!(f, "Results"),
            Self::DbInfo => write!(f, "Database Info"),
            Self::Pragmas => write!(f, "Pragmas"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Forward,
    Backward,
}

/// All state mutations flow through actions.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum Action {
    SwitchSubTab(SubTab),
    FocusPanel(PanelId),
    CycleFocus(Direction),
    ToggleSidebar,
    SwitchBottomTab(BottomTab),
    ToggleTheme,
    ShowHelp,
    Quit,
    ExecuteQuery(String),
    QueryCompleted(QueryResult),
    QueryFailed(String),
    SchemaLoaded(Vec<SchemaEntry>),
    ColumnsLoaded(String, Vec<ColumnInfo>),
    PopulateEditor(String),
    LoadColumns(String),
}

/// Per-database workspace.
pub(crate) struct DatabaseContext {
    #[allow(dead_code)]
    pub handle: DatabaseHandle,
    #[allow(dead_code)]
    pub path: String,
    pub label: String,
    pub sub_tab: SubTab,
    pub focus: PanelId,
    pub sidebar_visible: bool,
    pub bottom_tab: BottomTab,
}

impl DatabaseContext {
    pub fn new(handle: DatabaseHandle, path: String) -> Self {
        let label = if path == ":memory:" {
            "[in-memory]".to_string()
        } else {
            std::path::Path::new(&path)
                .file_name()
                .map_or_else(|| path.clone(), |f| f.to_string_lossy().to_string())
        };

        Self {
            handle,
            path,
            label,
            sub_tab: SubTab::Query,
            focus: PanelId::Editor,
            sidebar_visible: true,
            bottom_tab: BottomTab::Results,
        }
    }

    /// Returns the ordered list of focusable panels for the current sub-tab.
    pub fn focusable_panels(&self) -> Vec<PanelId> {
        match self.sub_tab {
            SubTab::Query => {
                let mut panels = vec![];
                if self.sidebar_visible {
                    panels.push(PanelId::Schema);
                }
                panels.push(PanelId::Editor);
                panels.push(PanelId::Bottom);
                panels
            }
            SubTab::Admin => vec![PanelId::DbInfo, PanelId::Pragmas],
        }
    }

    /// Cycle focus to the next/previous panel.
    pub fn cycle_focus(&mut self, direction: Direction) {
        let panels = self.focusable_panels();
        if panels.is_empty() {
            return;
        }
        // Fallback to 0 is safe: it selects the first panel, which is always valid
        // since we return early above when panels is empty. The debug_assert catches
        // logic bugs where focus drifts out of the focusable set (e.g., a new action
        // handler forgets to update focus after hiding a panel).
        let current = panels
            .iter()
            .position(|p| *p == self.focus)
            .unwrap_or_else(|| {
                debug_assert!(
                    false,
                    "focus {:?} not in focusable_panels {:?}",
                    self.focus, panels
                );
                0
            });
        let next = match direction {
            Direction::Forward => (current + 1) % panels.len(),
            Direction::Backward => (current + panels.len() - 1) % panels.len(),
        };
        self.focus = panels[next];
    }
}

/// Global application state.
pub(crate) struct AppState {
    pub databases: Vec<DatabaseContext>,
    pub active_db: usize,
    pub theme: Theme,
    #[allow(dead_code)]
    pub transient_message: Option<(String, Instant)>,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(db_context: DatabaseContext) -> Self {
        Self {
            databases: vec![db_context],
            active_db: 0,
            theme: DARK_THEME,
            transient_message: None,
            should_quit: false,
        }
    }

    pub fn active_db(&self) -> &DatabaseContext {
        debug_assert!(self.active_db < self.databases.len());
        &self.databases[self.active_db]
    }

    pub fn active_db_mut(&mut self) -> &mut DatabaseContext {
        debug_assert!(self.active_db < self.databases.len());
        &mut self.databases[self.active_db]
    }

    /// Process an action and update state.
    pub fn update(&mut self, action: &Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::CycleFocus(dir) => self.active_db_mut().cycle_focus(*dir),
            Action::FocusPanel(panel) => self.active_db_mut().focus = *panel,
            Action::SwitchSubTab(tab) => {
                let db = self.active_db_mut();
                db.sub_tab = *tab;
                let panels = db.focusable_panels();
                if let Some(&first) = panels.first() {
                    db.focus = first;
                }
            }
            Action::ToggleSidebar => {
                let db = self.active_db_mut();
                db.sidebar_visible = !db.sidebar_visible;
                if !db.sidebar_visible && db.focus == PanelId::Schema {
                    db.focus = PanelId::Editor;
                }
            }
            Action::SwitchBottomTab(tab) => {
                self.active_db_mut().bottom_tab = *tab;
            }
            Action::PopulateEditor(_) => {
                self.active_db_mut().focus = PanelId::Editor;
            }
            Action::ExecuteQuery(_)
            | Action::QueryCompleted(_)
            | Action::QueryFailed(_)
            | Action::SchemaLoaded(_)
            | Action::ColumnsLoaded(_, _)
            | Action::LoadColumns(_)
            | Action::ToggleTheme
            | Action::ShowHelp => {
                // Handled elsewhere (main.rs) or implemented in later milestones
            }
        }
    }
}
