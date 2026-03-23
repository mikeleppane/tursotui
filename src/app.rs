use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::components::data_editor::DataEditor;
use crate::components::db_info::DbInfoPanel;
use crate::components::editor::QueryEditor;
use crate::components::er_diagram::ERDiagram;
use crate::components::explain::ExplainView;
use crate::components::export::ExportPopup;
use crate::components::pragmas::PragmaDashboard;
use crate::components::record::RecordDetail;
use crate::components::results::ResultsTable;
use crate::components::schema::SchemaExplorer;
use crate::config::{AppConfig, ThemeMode};
use crate::db::{
    ColumnInfo, CustomTypeInfo, DatabaseHandle, DbInfo, ForeignKeyInfo, PragmaEntry, QueryKind,
    QueryResult, SchemaEntry,
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
    /// Foreign key info keyed by table name.
    pub(crate) fk_info: HashMap<String, Vec<ForeignKeyInfo>>,
    /// Approximate row counts keyed by lowercase table name.
    pub(crate) row_counts: HashMap<String, u64>,
    /// Custom types from `PRAGMA list_types` (non-base types only).
    pub(crate) custom_types: Vec<CustomTypeInfo>,
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
    pub(crate) text: String,
    pub(crate) created_at: Instant,
    pub(crate) is_error: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DdlViewerState {
    pub(crate) object_name: String,
    pub(crate) sql: String,
    pub(crate) scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    Help,
    History,
    Export,
    DmlPreview { submit_enabled: bool },
    FilePicker,
    GoToObject,
    DdlViewer,
    Bookmarks,
    ERDiagram,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Phase 4: Go to Object
pub(crate) enum ObjectKind {
    Table,
    Index,
    View,
    Trigger,
    Column,
    CustomType,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Phase 4: Go to Object
pub(crate) struct ObjectRef {
    pub(crate) name: String,
    pub(crate) kind: ObjectKind,
    pub(crate) parent: Option<String>,
    pub(crate) database_path: String,
}

/// All state mutations flow through actions.
#[derive(Debug)]
pub(crate) enum Action {
    /// Key was consumed by a component; suppress global fallback.
    /// Intentionally a no-op in `update()` and `dispatch_action_to_db()` (falls to `_ => {}`).
    Consumed,
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
    ExecuteQuery(String, ExecutionSource, Option<String>), // third = source_table hint
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
    #[allow(dead_code)]
    DataEditorActivated {
        table: String,
        pk_columns: Vec<usize>,
    },
    #[allow(dead_code)]
    DataEditorDeactivated,
    #[allow(dead_code)]
    StartCellEdit,
    #[allow(dead_code)]
    ConfirmCellEdit(Option<String>),
    #[allow(dead_code)]
    CancelCellEdit,
    #[allow(dead_code)]
    AddRow,
    #[allow(dead_code)]
    ToggleDeleteRow,
    #[allow(dead_code)]
    CloneRow(Vec<Option<String>>),
    #[allow(dead_code)]
    RevertCell,
    #[allow(dead_code)]
    RevertRow,
    #[allow(dead_code)]
    RevertAll,
    #[allow(dead_code)]
    ShowDmlPreview(bool),
    #[allow(dead_code)]
    SubmitDataEdits,
    #[allow(dead_code)]
    DataEditsCommitted,
    #[allow(dead_code)]
    DataEditsFailed(String),
    #[allow(dead_code)]
    FollowFK,
    #[allow(dead_code)]
    FKNavigateBack,
    #[allow(dead_code)]
    FKLoaded(String, Vec<crate::db::ForeignKeyInfo>),
    // Multi-database actions
    #[allow(dead_code)] // constructed by file picker (Phase 3)
    SwitchDatabase(usize),
    NextDatabase,
    PrevDatabase,
    CloseActiveDatabase,
    #[allow(dead_code)] // constructed by file picker (Phase 3)
    OpenDatabase(std::path::PathBuf),
    OpenFilePicker,
    OpenGoToObject,
    ResizeSidebar(i16), // delta in percentage points
    ResizeEditor(i16),  // delta in percentage points
    #[allow(dead_code)] // Phase 4: Go to Object
    GoToObject(ObjectRef),
    ShowDdl {
        name: String,
        sql: String,
    },
    ExecuteFilteredQuery {
        table: String,
        where_clause: String,
    },
    ShowBookmarks,
    ShowERDiagram,
    SaveBookmark {
        name: String,
        sql: String,
        database_path: Option<String>,
    },
    UpdateBookmark {
        id: i64,
        name: String,
    },
    RecallBookmark(String),
    RecallAndExecuteBookmark(String),
    DeleteBookmark(i64),
    BookmarksLoaded(Vec<crate::history::BookmarkEntry>),
    BookmarkReloadRequested,
}

/// Per-database workspace.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct DatabaseContext {
    pub(crate) handle: DatabaseHandle,
    pub(crate) path: String,
    pub(crate) label: String,
    pub(crate) sub_tab: SubTab,
    pub(crate) focus: PanelId,
    pub(crate) sidebar_visible: bool,
    pub(crate) bottom_tab: BottomTab,
    pub(crate) executing: bool,
    pub(crate) last_execution_time: Option<Duration>,
    pub(crate) last_row_count: Option<usize>,
    pub(crate) last_truncated: bool,
    pub(crate) last_query_kind: Option<QueryKind>,
    pub(crate) last_rows_affected: u64,
    pub(crate) last_execution_source: ExecutionSource,
    pub(crate) last_executed_sql: Option<String>,
    /// True when the last query was from the WHERE filter bar.
    pub(crate) last_filter_query: bool,
    pub(crate) schema_cache: SchemaCache,
    // Components (owned per-database)
    pub(crate) schema: SchemaExplorer,
    pub(crate) editor: QueryEditor,
    pub(crate) results: ResultsTable,
    pub(crate) explain: ExplainView,
    pub(crate) record_detail: RecordDetail,
    pub(crate) db_info: DbInfoPanel,
    pub(crate) pragmas: PragmaDashboard,
    pub(crate) data_editor: DataEditor,
    pub(crate) er_diagram: ERDiagram,
    pub(crate) export_popup: Option<ExportPopup>,
    // Layout percentages (adjustable at runtime)
    pub(crate) sidebar_pct: u16,
    pub(crate) editor_pct: u16,
    #[allow(dead_code)]
    pub(crate) pending_edit_table: Option<(String, String)>, // (table_name, activating_sql)
    /// Set to true by `FollowFK` before dispatching `ExecuteQuery`,
    /// cleared by `QueryCompleted`. Used to distinguish FK navigation
    /// activations (preserve stack) from manual queries (clear stack).
    pub(crate) pending_fk_activation: bool,
}

impl DatabaseContext {
    pub(crate) fn new(handle: DatabaseHandle, path: String, config: &AppConfig) -> Self {
        let label = if path == ":memory:" {
            "[in-memory]".to_string()
        } else {
            std::path::Path::new(&path)
                .file_name()
                .map_or_else(|| path.clone(), |f| f.to_string_lossy().to_string())
        };

        let mut editor = QueryEditor::with_tab_size(config.editor.tab_size);
        editor.set_autocomplete_config(
            config.editor.autocomplete,
            config.editor.autocomplete_min_chars,
        );

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
            last_filter_query: false,
            schema_cache: SchemaCache::default(),
            schema: SchemaExplorer::new(),
            editor,
            results: ResultsTable::with_config(
                config.results.max_column_width,
                config.results.null_display.clone(),
            ),
            explain: ExplainView::new(),
            record_detail: RecordDetail::new(),
            db_info: DbInfoPanel::new(),
            pragmas: PragmaDashboard::new(),
            data_editor: DataEditor::new(),
            er_diagram: ERDiagram::new(),
            export_popup: None,
            sidebar_pct: 20,
            editor_pct: 40,
            pending_edit_table: None,
            pending_fk_activation: false,
        }
    }

    /// Returns the ordered list of focusable panels for the current sub-tab.
    pub(crate) fn focusable_panels(&self) -> Vec<PanelId> {
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
    pub(crate) fn cycle_focus(&mut self, direction: Direction) {
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
    pub(crate) databases: Vec<DatabaseContext>,
    pub(crate) active_db: usize,
    pub(crate) theme: Theme,
    pub(crate) config: AppConfig,
    pub(crate) transient_message: Option<TransientMessage>,
    pub(crate) should_quit: bool,
    pub(crate) active_overlay: Option<Overlay>,
    pub(crate) help_scroll: usize,
    pub(crate) ddl_viewer: Option<DdlViewerState>,
    pub(crate) history_db: Option<crate::history::HistoryDb>,
}

impl AppState {
    pub(crate) fn new(
        databases: Vec<DatabaseContext>,
        config: AppConfig,
        history_db: Option<crate::history::HistoryDb>,
    ) -> Self {
        let theme = match config.theme.mode {
            ThemeMode::Dark => DARK_THEME,
            ThemeMode::Light => LIGHT_THEME,
        };
        Self {
            databases,
            active_db: 0,
            theme,
            config,
            transient_message: None,
            should_quit: false,
            active_overlay: None,
            help_scroll: 0,
            ddl_viewer: None,
            history_db,
        }
    }

    pub(crate) fn active_db(&self) -> &DatabaseContext {
        debug_assert!(self.active_db < self.databases.len());
        &self.databases[self.active_db]
    }

    pub(crate) fn active_db_mut(&mut self) -> &mut DatabaseContext {
        debug_assert!(self.active_db < self.databases.len());
        &mut self.databases[self.active_db]
    }

    /// Process an action targeting a specific database by index.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn update_for_db(&mut self, db_idx: usize, action: &Action) {
        let db = &mut self.databases[db_idx];
        match action {
            Action::CycleFocus(dir) => db.cycle_focus(*dir),
            Action::FocusPanel(panel) => db.focus = *panel,
            Action::SwitchSubTab(tab) => {
                db.sub_tab = *tab;
                let panels = db.focusable_panels();
                if let Some(&first) = panels.first() {
                    db.focus = first;
                }
            }
            Action::ToggleSidebar => {
                db.sidebar_visible = !db.sidebar_visible;
                if !db.sidebar_visible && db.focus == PanelId::Schema {
                    db.focus = PanelId::Editor;
                }
            }
            Action::SwitchBottomTab(tab) => {
                db.bottom_tab = *tab;
            }
            Action::PopulateEditor(_) => {
                db.focus = PanelId::Editor;
            }
            Action::ExecuteQuery(sql, source, _source_table) => {
                if !sql.trim().is_empty() {
                    db.executing = true;
                    db.last_execution_source = *source;
                    db.last_executed_sql = Some(sql.clone());
                }
            }
            Action::QueryCompleted(result) => {
                db.executing = false;
                db.last_execution_time = Some(result.execution_time);
                db.last_row_count = Some(result.rows.len());
                db.last_truncated = result.truncated;
                db.last_query_kind = Some(result.query_kind.clone());
                db.last_rows_affected = result.rows_affected;
            }
            Action::QueryFailed(_) => {
                db.executing = false;
                db.last_execution_time = None;
                db.last_row_count = None;
                db.last_truncated = false;
            }
            Action::FKLoaded(table, fks) => {
                db.schema_cache.fk_info.insert(table.clone(), fks.clone());
            }
            // Actions that only mutate global state — handled below
            _ => {}
        }
        // Global state mutations (not per-database)
        match action {
            Action::Quit => self.should_quit = true,
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
            Action::ShowBookmarks => {
                if self.history_db.is_none() {
                    return;
                }
                self.active_overlay = if let Some(Overlay::Bookmarks) = self.active_overlay {
                    None
                } else {
                    Some(Overlay::Bookmarks)
                };
            }
            Action::ShowERDiagram => {
                if self.active_overlay == Some(Overlay::ERDiagram) {
                    self.active_overlay = None;
                } else {
                    self.active_overlay = Some(Overlay::ERDiagram);
                }
            }
            Action::RecallHistory(_)
            | Action::RecallAndExecute(_)
            | Action::RecallBookmark(_)
            | Action::RecallAndExecuteBookmark(_)
            | Action::DataEditsCommitted
            | Action::DataEditsFailed(_) => {
                self.active_overlay = None;
            }
            Action::ShowExport => {
                self.active_overlay = if let Some(Overlay::Export) = self.active_overlay {
                    None
                } else {
                    Some(Overlay::Export)
                };
            }
            Action::OpenFilePicker => {
                self.active_overlay = if let Some(Overlay::FilePicker) = self.active_overlay {
                    None
                } else {
                    Some(Overlay::FilePicker)
                };
            }
            Action::ShowDmlPreview(b) => {
                self.active_overlay = Some(Overlay::DmlPreview { submit_enabled: *b });
            }
            Action::SwitchDatabase(idx) => {
                if *idx < self.databases.len() && *idx != self.active_db {
                    // Auto-save outgoing tab's editor buffer
                    let db = &self.databases[self.active_db];
                    if !db.editor.contents().is_empty() {
                        let _ = crate::persistence::save_buffer(&db.path, &db.editor.contents());
                    }
                    self.active_overlay = None;
                    self.ddl_viewer = None;
                    self.active_db = *idx;
                }
            }
            Action::NextDatabase => {
                if self.databases.len() > 1 {
                    // Auto-save outgoing tab's editor buffer
                    let db = &self.databases[self.active_db];
                    if !db.editor.contents().is_empty() {
                        let _ = crate::persistence::save_buffer(&db.path, &db.editor.contents());
                    }
                    self.active_overlay = None;
                    self.ddl_viewer = None;
                    self.active_db = (self.active_db + 1) % self.databases.len();
                }
            }
            Action::PrevDatabase => {
                if self.databases.len() > 1 {
                    // Auto-save outgoing tab's editor buffer
                    let db = &self.databases[self.active_db];
                    if !db.editor.contents().is_empty() {
                        let _ = crate::persistence::save_buffer(&db.path, &db.editor.contents());
                    }
                    self.active_overlay = None;
                    self.ddl_viewer = None;
                    self.active_db =
                        (self.active_db + self.databases.len() - 1) % self.databases.len();
                }
            }
            Action::CloseActiveDatabase => {
                if self.databases.len() <= 1 {
                    self.transient_message = Some(TransientMessage {
                        text: "Cannot close last database".to_string(),
                        created_at: Instant::now(),
                        is_error: false,
                    });
                } else {
                    // Clear any open overlay/DDL viewer before removing the database
                    self.active_overlay = None;
                    self.ddl_viewer = None;
                    // Auto-save editor buffer before removal so we target the
                    // correct database (after remove(), active_db points elsewhere).
                    let db = &self.databases[self.active_db];
                    if !db.editor.contents().is_empty() {
                        let _ = crate::persistence::save_buffer(&db.path, &db.editor.contents());
                    }
                    self.databases.remove(self.active_db);
                    if self.active_db >= self.databases.len() {
                        self.active_db = self.databases.len() - 1;
                    }
                }
            }
            Action::OpenDatabase(_path) => {
                // Handled in dispatch — requires async DatabaseHandle::open
            }
            Action::OpenGoToObject => {
                self.active_overlay = if self.active_overlay == Some(Overlay::GoToObject) {
                    None
                } else {
                    Some(Overlay::GoToObject)
                };
            }
            Action::ResizeSidebar(delta) => {
                let db = &mut self.databases[db_idx];
                #[allow(clippy::cast_possible_wrap)]
                let current = db.sidebar_pct as i16;
                db.sidebar_pct = (current + delta).clamp(10, 50) as u16;
            }
            Action::ResizeEditor(delta) => {
                let db = &mut self.databases[db_idx];
                #[allow(clippy::cast_possible_wrap)]
                let current = db.editor_pct as i16;
                db.editor_pct = (current + delta).clamp(20, 80) as u16;
            }
            Action::GoToObject(obj_ref) => {
                // Switch to target database, switch to Query sub-tab
                if let Some(idx) = self
                    .databases
                    .iter()
                    .position(|db| db.path == obj_ref.database_path)
                {
                    if idx != self.active_db {
                        // Auto-save outgoing tab's editor buffer
                        let outgoing = &self.databases[self.active_db];
                        if !outgoing.editor.contents().is_empty() {
                            let _ = crate::persistence::save_buffer(
                                &outgoing.path,
                                &outgoing.editor.contents(),
                            );
                        }
                        self.active_db = idx;
                    }
                    // Switch to Query sub-tab
                    let target_db = &mut self.databases[idx];
                    if target_db.sub_tab != SubTab::Query {
                        target_db.sub_tab = SubTab::Query;
                        let panels = target_db.focusable_panels();
                        if let Some(&first) = panels.first() {
                            target_db.focus = first;
                        }
                    }
                }
                self.active_overlay = None;
            }
            Action::ShowDdl { name, sql } => {
                self.ddl_viewer = Some(DdlViewerState {
                    object_name: name.clone(),
                    sql: sql.clone(),
                    scroll: 0,
                });
                self.active_overlay = Some(Overlay::DdlViewer);
            }
            _ => {}
        }
    }

    /// Process an action and update state (routes to active database).
    pub(crate) fn update(&mut self, action: &Action) {
        let active = self.active_db;
        self.update_for_db(active, action);
    }
}
