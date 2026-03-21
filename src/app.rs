use std::time::{Duration, Instant};

use crate::db::{ColumnInfo, DatabaseHandle, DbInfo, PragmaEntry, QueryResult, SchemaEntry};
use crate::theme::{DARK_THEME, LIGHT_THEME, Theme};

#[derive(Debug, Clone)]
pub(crate) struct TransientMessage {
    pub text: String,
    pub created_at: Instant,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubTab {
    Query,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BottomTab {
    Results,
    #[allow(dead_code)] // used by bottom tab routing (M4 Task 7)
    Explain,
    #[allow(dead_code)] // used by bottom tab routing (M4 Task 7)
    Detail,
    #[allow(dead_code)] // stays placeholder until ER Diagram milestone
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
#[derive(Debug)]
pub(crate) enum Action {
    SwitchSubTab(SubTab),
    #[allow(dead_code)] // constructed when click-to-focus lands (later milestone)
    FocusPanel(PanelId),
    CycleFocus(Direction),
    ToggleSidebar,
    #[allow(dead_code)] // constructed by bottom tab number-key routing (M4 Task 7)
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
    SetTransient(String, bool),
    #[allow(dead_code)] // constructed by ExplainView (M4 Task 4)
    GenerateExplain(String),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    ExplainCompleted(Vec<Vec<String>>, Vec<String>),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    ExplainFailed(String),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    DbInfoLoaded(DbInfo),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    DbInfoFailed(String),
    #[allow(dead_code)] // constructed by DbInfoPanel (M4 Task 5)
    RefreshDbInfo,
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    PragmasLoaded(Vec<PragmaEntry>),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    PragmasFailed(String),
    #[allow(dead_code)] // constructed by PragmaDashboard (M4 Task 6)
    RefreshPragmas,
    #[allow(dead_code)] // constructed by PragmaDashboard (M4 Task 6)
    SetPragma(String, String),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    PragmaSet(String, String),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    PragmaFailed(String, String), // (pragma_name, error_message)
    #[allow(dead_code)] // constructed by DbInfoPanel (M4 Task 5)
    WalCheckpoint,
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    WalCheckpointed(String),
    #[allow(dead_code)] // mapped from QueryMessage (M4 Task 7)
    WalCheckpointFailed(String),
}

/// Per-database workspace.
pub(crate) struct DatabaseContext {
    pub handle: DatabaseHandle,
    #[allow(dead_code)] // used by load_db_info (M4 Task 7)
    pub path: String,
    pub label: String,
    pub sub_tab: SubTab,
    pub focus: PanelId,
    pub sidebar_visible: bool,
    pub bottom_tab: BottomTab,
    pub executing: bool,
    pub last_execution_time: Option<Duration>,
    pub last_row_count: Option<usize>,
    pub last_truncated: bool,
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
            executing: false,
            last_execution_time: None,
            last_row_count: None,
            last_truncated: false,
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
    pub transient_message: Option<TransientMessage>,
    pub should_quit: bool,
    pub help_visible: bool,
    pub help_scroll: usize,
}

impl AppState {
    pub fn new(db_context: DatabaseContext) -> Self {
        Self {
            databases: vec![db_context],
            active_db: 0,
            theme: DARK_THEME,
            transient_message: None,
            should_quit: false,
            help_visible: false,
            help_scroll: 0,
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
            Action::ExecuteQuery(sql) => {
                if !sql.trim().is_empty() {
                    self.active_db_mut().executing = true;
                }
            }
            Action::QueryCompleted(result) => {
                let db = self.active_db_mut();
                db.executing = false;
                db.last_execution_time = Some(result.execution_time);
                db.last_row_count = Some(result.rows.len());
                db.last_truncated = result.truncated;
            }
            Action::QueryFailed(_) => {
                let db = self.active_db_mut();
                db.executing = false;
                db.last_execution_time = None;
                db.last_row_count = None;
                db.last_truncated = false;
            }
            Action::SetTransient(msg, is_error) => {
                self.transient_message = Some(TransientMessage {
                    text: msg.clone(),
                    created_at: Instant::now(),
                    is_error: *is_error,
                });
            }
            Action::ToggleTheme => {
                self.theme = if self.theme.bg == DARK_THEME.bg {
                    LIGHT_THEME
                } else {
                    DARK_THEME
                };
            }
            Action::ShowHelp => {
                self.help_visible = !self.help_visible;
                if self.help_visible {
                    self.help_scroll = 0;
                }
            }
            Action::SchemaLoaded(_)
            | Action::ColumnsLoaded(_, _)
            | Action::LoadColumns(_)
            | Action::GenerateExplain(_)
            | Action::ExplainCompleted(_, _)
            | Action::ExplainFailed(_)
            | Action::DbInfoLoaded(_)
            | Action::DbInfoFailed(_)
            | Action::RefreshDbInfo
            | Action::PragmasLoaded(_)
            | Action::PragmasFailed(_)
            | Action::RefreshPragmas
            | Action::SetPragma(_, _)
            | Action::PragmaSet(_, _)
            | Action::PragmaFailed(_, _)
            | Action::WalCheckpoint
            | Action::WalCheckpointed(_)
            | Action::WalCheckpointFailed(_) => {
                // No AppState mutation needed; dispatched to components in M4 Tasks 3-7
            }
        }
    }
}
