use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::config::{AppConfig, ThemeMode};
use crate::db::{
    ColumnInfo, DatabaseHandle, DbInfo, PragmaEntry, QueryKind, QueryResult, SchemaEntry,
};
use crate::theme::{DARK_THEME, LIGHT_THEME, Theme};

/// In-memory cache of schema metadata for autocomplete.
/// Populated eagerly after schema loads — all columns for all tables/views.
#[derive(Debug, Clone, Default)]
pub(crate) struct SchemaCache {
    /// All table and view names from `sqlite_schema`.
    pub(crate) entries: Vec<SchemaEntry>,
    /// Column info keyed by table/view name. Populated via PRAGMA `table_info`.
    pub(crate) columns: HashMap<String, Vec<ColumnInfo>>,
    /// True once all tables have had their columns loaded.
    pub(crate) fully_loaded: bool,
}

impl SchemaCache {
    /// Case-insensitive column lookup. Tries exact match first, then
    /// falls back to a linear scan comparing lowercased names.
    pub(crate) fn get_columns(&self, table: &str) -> Option<&Vec<ColumnInfo>> {
        self.columns.get(table).or_else(|| {
            let lower = table.to_lowercase();
            self.columns
                .iter()
                .find(|(k, _)| k.to_lowercase() == lower)
                .map(|(_, v)| v)
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TransientMessage {
    pub text: String,
    pub created_at: Instant,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    Help,
    History,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubTab {
    Query,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BottomTab {
    Results,
    Explain,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionSource {
    FullBuffer,
    Selection,
    StatementAtCursor,
}

/// All state mutations flow through actions.
#[derive(Debug)]
pub(crate) enum Action {
    SwitchSubTab(SubTab),
    #[allow(dead_code)] // constructed when click-to-focus lands (later milestone)
    FocusPanel(PanelId),
    CycleFocus(Direction),
    ToggleSidebar,
    SwitchBottomTab(BottomTab),
    ToggleTheme,
    ShowHelp,
    Quit,
    ClearEditor,
    ExecuteQuery(String, ExecutionSource),
    QueryCompleted(QueryResult),
    QueryFailed(String),
    SchemaLoaded(Vec<SchemaEntry>),
    ColumnsLoaded(String, Vec<ColumnInfo>),
    PopulateEditor(String),
    LoadColumns(String),
    SetTransient(String, bool),
    GenerateExplain(String),
    ExplainCompleted(Vec<Vec<String>>, Vec<String>),
    ExplainFailed(String),
    DbInfoLoaded(DbInfo),
    DbInfoFailed(String),
    RefreshDbInfo,
    PragmasLoaded(Vec<PragmaEntry>),
    PragmasFailed(String),
    RefreshPragmas,
    SetPragma(String, String),
    PragmaSet(String, String),
    PragmaFailed(String, String), // (pragma_name, error_message)
    WalCheckpoint,
    WalCheckpointed(String),
    WalCheckpointFailed(String),
    IntegrityCheck,
    IntegrityCheckCompleted(String),
    IntegrityCheckFailed(String),
    HistoryLoaded(Vec<crate::history::HistoryEntry>),
    ShowHistory,
    RecallHistory(String),
    RecallAndExecute(String),
    DeleteHistoryEntry(i64),
    HistoryReloadRequested,
    TriggerAutocomplete,
    AcceptCompletion(#[allow(dead_code)] String),
    #[allow(dead_code)] // editor handles dismissal internally without emitting this action
    DismissAutocomplete,
    ShowExport,
    ExecuteExport,
    CopyAllResults,
}

/// Per-database workspace.
pub(crate) struct DatabaseContext {
    pub handle: DatabaseHandle,
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
    pub last_query_kind: Option<QueryKind>,
    pub last_rows_affected: u64,
    pub last_execution_source: ExecutionSource,
    pub last_executed_sql: Option<String>,
    pub schema_cache: SchemaCache,
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
            last_query_kind: None,
            last_rows_affected: 0,
            last_execution_source: ExecutionSource::FullBuffer,
            last_executed_sql: None,
            schema_cache: SchemaCache::default(),
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
    pub config: AppConfig,
    pub transient_message: Option<TransientMessage>,
    pub should_quit: bool,
    pub active_overlay: Option<Overlay>,
    pub help_scroll: usize,
    pub history_db: Option<crate::history::HistoryDb>,
}

impl AppState {
    pub fn new(
        db_context: DatabaseContext,
        config: AppConfig,
        history_db: Option<crate::history::HistoryDb>,
    ) -> Self {
        let theme = match config.theme.mode {
            ThemeMode::Dark => DARK_THEME,
            ThemeMode::Light => LIGHT_THEME,
        };
        Self {
            databases: vec![db_context],
            active_db: 0,
            theme,
            config,
            transient_message: None,
            should_quit: false,
            active_overlay: None,
            help_scroll: 0,
            history_db,
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
    #[allow(clippy::too_many_lines)]
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
            Action::ExecuteQuery(sql, source) => {
                if !sql.trim().is_empty() {
                    let db = self.active_db_mut();
                    db.executing = true;
                    db.last_execution_source = *source;
                    db.last_executed_sql = Some(sql.clone());
                }
            }
            Action::QueryCompleted(result) => {
                let db = self.active_db_mut();
                db.executing = false;
                db.last_execution_time = Some(result.execution_time);
                db.last_row_count = Some(result.rows.len());
                db.last_truncated = result.truncated;
                db.last_query_kind = Some(result.query_kind.clone());
                db.last_rows_affected = result.rows_affected;
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
                if self.theme.bg == DARK_THEME.bg {
                    self.theme = LIGHT_THEME;
                    self.config.theme.mode = ThemeMode::Light;
                } else {
                    self.theme = DARK_THEME;
                    self.config.theme.mode = ThemeMode::Dark;
                }
            }
            Action::ShowHelp => {
                self.active_overlay = if let Some(Overlay::Help) = self.active_overlay {
                    None
                } else {
                    self.help_scroll = 0;
                    Some(Overlay::Help)
                };
            }
            Action::ShowHistory => {
                self.active_overlay = if let Some(Overlay::History) = self.active_overlay {
                    None
                } else {
                    Some(Overlay::History)
                };
            }
            Action::RecallHistory(_) | Action::RecallAndExecute(_) => {
                self.active_overlay = None;
            }
            Action::ShowExport => {
                self.active_overlay = if let Some(Overlay::Export) = self.active_overlay {
                    None
                } else {
                    Some(Overlay::Export)
                };
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
            | Action::WalCheckpointFailed(_)
            | Action::IntegrityCheck
            | Action::IntegrityCheckCompleted(_)
            | Action::IntegrityCheckFailed(_)
            | Action::HistoryLoaded(_)
            | Action::DeleteHistoryEntry(_)
            | Action::HistoryReloadRequested
            | Action::ClearEditor
            | Action::TriggerAutocomplete
            | Action::AcceptCompletion(_)
            | Action::DismissAutocomplete
            | Action::ExecuteExport
            | Action::CopyAllResults => {
                // No AppState mutation needed; dispatched to components
            }
        }
    }
}
