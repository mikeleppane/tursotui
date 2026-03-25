use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::components::Component;
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
use crate::theme::{DARK_THEME, LIGHT_THEME, Theme};
use tursotui_db::{
    ColumnInfo, CustomTypeInfo, DatabaseHandle, DbInfo, ForeignKeyInfo, IndexDetail, PragmaEntry,
    QueryKind, QueryResult, SchemaEntry,
};

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
    /// Index metadata keyed by table name.
    pub(crate) index_details: HashMap<String, Vec<IndexDetail>>,
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
pub(crate) enum ObjectKind {
    Table,
    Index,
    View,
    Trigger,
    Column,
    CustomType,
}

#[derive(Debug, Clone)]
pub(crate) struct ObjectRef {
    pub(crate) name: String,
    pub(crate) kind: ObjectKind,
    pub(crate) database_path: String,
}

/// All state mutations flow through actions.
#[derive(Debug)]
pub(crate) enum Action {
    /// Key was consumed by a component; suppress global fallback.
    /// Intentionally a no-op in `update()` and `dispatch_action_to_db()` (falls to `_ => {}`).
    Consumed,
    SwitchSubTab(SubTab),
    CycleFocus(Direction),
    ToggleSidebar,
    SwitchBottomTab(BottomTab),
    ToggleTheme,
    ShowHelp,
    Quit,
    ClearEditor,
    ExecuteQuery {
        sql: String,
        source: ExecutionSource,
        source_table: Option<String>,
        params: Option<tursotui_db::QueryParams>,
    },
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
    #[allow(dead_code)] // planned: activated via QueryCompleted detection
    DataEditorActivated {
        table: String,
        pk_columns: Vec<usize>,
    },
    #[allow(dead_code)] // planned: deactivation on tab switch
    DataEditorDeactivated,
    StartCellEdit,
    ConfirmCellEdit(Option<String>),
    CancelCellEdit,
    AddRow,
    ToggleDeleteRow,
    CloneRow,
    RevertCell,
    RevertRow,
    RevertAll,
    ShowDmlPreview(bool),
    SubmitDataEdits,
    DataEditsCommitted,
    DataEditsFailed(String),
    FollowFK,
    FKNavigateBack,
    FKLoaded(String, Vec<tursotui_db::ForeignKeyInfo>),
    // Multi-database actions
    SwitchDatabase(usize),
    NextDatabase,
    PrevDatabase,
    CloseActiveDatabase,
    OpenDatabase(std::path::PathBuf),
    OpenFilePicker,
    OpenGoToObject,
    ResizeSidebar(i16), // delta in percentage points
    ResizeEditor(i16),  // delta in percentage points
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
    IndexDetailsLoaded(String, Vec<tursotui_db::IndexDetail>),
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
    /// JSON-serialized parameters from the last `ExecuteQuery` action.
    /// Stored here so the `QueryCompleted` / `QueryFailed` handlers can include
    /// params in the history log entry without needing to re-derive them.
    pub(crate) last_executed_params_json: Option<String>,
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
            last_executed_params_json: None,
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

    /// Broadcast an action to all per-database components.
    /// Each component ignores actions it doesn't care about (default `update()` is no-op).
    pub(crate) fn broadcast_update(&mut self, action: &Action) {
        self.schema.update(action);
        self.editor.update(action);
        self.results.update(action);
        self.explain.update(action);
        self.record_detail.update(action);
        self.db_info.update(action);
        self.pragmas.update(action);
        self.data_editor.update(action);
        self.er_diagram.update(action);
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

/// Serialize `QueryParams` to a compact JSON string for history storage.
///
/// Positional params become `["42", null, "hello"]` (values as strings).
/// Named params become `{":name": "Alice", ":age": "30"}`.
/// Returns an error if JSON serialization fails (should never happen in practice).
/// Convert a single `turso::Value` to a `serde_json::Value` for history serialization.
fn turso_value_to_json(v: &turso::Value) -> serde_json::Value {
    match v {
        turso::Value::Null => serde_json::Value::Null,
        turso::Value::Integer(n) => serde_json::Value::String(n.to_string()),
        turso::Value::Real(f) => serde_json::Value::String(f.to_string()),
        turso::Value::Text(s) => serde_json::Value::String(s.clone()),
        turso::Value::Blob(b) => serde_json::Value::String(format!("[BLOB {} B]", b.len())),
    }
}

pub(crate) fn params_to_json(params: &tursotui_db::QueryParams) -> Result<String, String> {
    use tursotui_db::QueryParams;

    match params {
        QueryParams::Positional(vals) => {
            let arr: Vec<serde_json::Value> = vals.iter().map(turso_value_to_json).collect();
            serde_json::to_string(&arr).map_err(|e| e.to_string())
        }
        QueryParams::Named(pairs) => {
            let mut map = serde_json::Map::new();
            for (name, val) in pairs {
                map.insert(name.clone(), turso_value_to_json(val));
            }
            serde_json::to_string(&serde_json::Value::Object(map)).map_err(|e| e.to_string())
        }
    }
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
            Action::ExecuteQuery {
                sql,
                source,
                source_table: _,
                params,
            } => {
                if !sql.trim().is_empty() {
                    db.executing = true;
                    db.last_execution_source = *source;
                    db.last_executed_sql = Some(sql.clone());
                    db.last_executed_params_json =
                        params.as_ref().and_then(|p| params_to_json(p).ok());
                }
            }
            Action::QueryCompleted(result) => {
                db.executing = false;
                db.last_execution_time = Some(result.execution_time);
                db.last_row_count = Some(result.rows.len());
                db.last_truncated = result.truncated;
                db.last_query_kind = Some(result.query_kind);
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
            Action::IndexDetailsLoaded(table, indexes) => {
                db.schema_cache
                    .index_details
                    .insert(table.clone(), indexes.clone());
                // Refresh results table indicators if it's displaying this table.
                // Closes the first-query timing gap: IndexDetailsLoaded often arrives
                // after QueryCompleted, so indicators would be missing on initial display.
                let displaying_table = db
                    .results
                    .current_result()
                    .and_then(|r| r.source_table.as_deref())
                    .map(str::to_lowercase);
                if displaying_table.as_deref() == Some(&table.to_lowercase()) {
                    let leading_cols: std::collections::HashSet<String> = indexes
                        .iter()
                        .filter_map(|idx| idx.columns.first().cloned())
                        .collect();
                    db.results.set_indexed_columns(leading_cols);
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    async fn test_app_state() -> AppState {
        let handle = tursotui_db::DatabaseHandle::open(":memory:").await.unwrap();
        let config = AppConfig::default();
        let db_ctx = DatabaseContext::new(handle, ":memory:".into(), &config);
        AppState::new(vec![db_ctx], config, None)
    }

    async fn two_db_app_state() -> AppState {
        let h1 = tursotui_db::DatabaseHandle::open(":memory:").await.unwrap();
        let h2 = tursotui_db::DatabaseHandle::open(":memory:").await.unwrap();
        let config = AppConfig::default();
        let db1 = DatabaseContext::new(h1, "db1.sqlite".into(), &config);
        let db2 = DatabaseContext::new(h2, "db2.sqlite".into(), &config);
        AppState::new(vec![db1, db2], config, None)
    }

    #[tokio::test]
    async fn update_quit_sets_should_quit() {
        let mut app = test_app_state().await;
        assert!(!app.should_quit);
        app.update(&Action::Quit);
        assert!(app.should_quit, "Quit action should set should_quit");
    }

    #[tokio::test]
    async fn update_show_help_toggles_overlay() {
        let mut app = test_app_state().await;
        assert!(app.active_overlay.is_none());

        app.update(&Action::ShowHelp);
        assert_eq!(app.active_overlay, Some(Overlay::Help));

        app.update(&Action::ShowHelp);
        assert!(app.active_overlay.is_none(), "ShowHelp again should close");
    }

    #[tokio::test]
    async fn update_show_history_toggles_overlay() {
        let mut app = test_app_state().await;
        app.update(&Action::ShowHistory);
        assert_eq!(app.active_overlay, Some(Overlay::History));

        app.update(&Action::ShowHistory);
        assert!(app.active_overlay.is_none());
    }

    #[tokio::test]
    async fn update_set_transient_stores_message() {
        let mut app = test_app_state().await;
        app.update(&Action::SetTransient("test msg".into(), false));
        let tm = app.transient_message.as_ref().unwrap();
        assert_eq!(tm.text, "test msg");
        assert!(!tm.is_error);
    }

    #[tokio::test]
    async fn update_set_transient_error_flag() {
        let mut app = test_app_state().await;
        app.update(&Action::SetTransient("error!".into(), true));
        assert!(app.transient_message.as_ref().unwrap().is_error);
    }

    #[tokio::test]
    async fn update_switch_sub_tab() {
        let mut app = test_app_state().await;
        assert_eq!(app.active_db().sub_tab, SubTab::Query);
        app.update(&Action::SwitchSubTab(SubTab::Admin));
        assert_eq!(app.active_db().sub_tab, SubTab::Admin);
    }

    #[tokio::test]
    async fn update_switch_bottom_tab() {
        let mut app = test_app_state().await;
        assert_eq!(app.active_db().bottom_tab, BottomTab::Results);
        app.update(&Action::SwitchBottomTab(BottomTab::Explain));
        assert_eq!(app.active_db().bottom_tab, BottomTab::Explain);
    }

    #[tokio::test]
    async fn update_toggle_sidebar() {
        let mut app = test_app_state().await;
        let initial = app.active_db().sidebar_visible;
        app.update(&Action::ToggleSidebar);
        assert_ne!(app.active_db().sidebar_visible, initial);
    }

    #[tokio::test]
    async fn update_toggle_theme_changes_theme() {
        let mut app = test_app_state().await;
        let original_bg = app.theme.bg;
        app.update(&Action::ToggleTheme);
        assert_ne!(
            original_bg, app.theme.bg,
            "theme background should change after toggle"
        );
    }

    #[tokio::test]
    async fn update_toggle_theme_round_trips() {
        let mut app = test_app_state().await;
        let original_bg = app.theme.bg;
        app.update(&Action::ToggleTheme);
        app.update(&Action::ToggleTheme);
        assert_eq!(
            original_bg, app.theme.bg,
            "double toggle should restore original theme"
        );
    }

    #[tokio::test]
    async fn update_switch_database() {
        let mut app = two_db_app_state().await;
        assert_eq!(app.active_db, 0);
        app.update(&Action::SwitchDatabase(1));
        assert_eq!(app.active_db, 1);
    }

    #[tokio::test]
    async fn update_switch_database_out_of_bounds_ignored() {
        let mut app = two_db_app_state().await;
        app.update(&Action::SwitchDatabase(99));
        assert_eq!(app.active_db, 0, "out-of-bounds index should be ignored");
    }

    #[tokio::test]
    async fn update_next_database_wraps() {
        let mut app = two_db_app_state().await;
        app.active_db = 1;
        app.update(&Action::NextDatabase);
        assert_eq!(app.active_db, 0, "should wrap around to first database");
    }

    #[tokio::test]
    async fn update_prev_database_wraps() {
        let mut app = two_db_app_state().await;
        assert_eq!(app.active_db, 0);
        app.update(&Action::PrevDatabase);
        assert_eq!(app.active_db, 1, "should wrap around to last database");
    }

    #[tokio::test]
    async fn update_close_active_database_prevents_closing_last() {
        let mut app = test_app_state().await;
        assert_eq!(app.databases.len(), 1);
        app.update(&Action::CloseActiveDatabase);
        assert_eq!(app.databases.len(), 1, "should not close the last database");
        assert!(
            app.transient_message.is_some(),
            "should set a transient message"
        );
    }

    #[tokio::test]
    async fn update_close_active_database_removes_db() {
        let mut app = two_db_app_state().await;
        assert_eq!(app.databases.len(), 2);
        app.update(&Action::CloseActiveDatabase);
        assert_eq!(app.databases.len(), 1);
    }

    #[tokio::test]
    async fn update_resize_sidebar_clamps() {
        let mut app = test_app_state().await;
        assert_eq!(app.active_db().sidebar_pct, 20);

        // Shrink below minimum (10)
        app.update(&Action::ResizeSidebar(-50));
        assert_eq!(app.active_db().sidebar_pct, 10, "should clamp at 10");

        // Grow above maximum (50)
        app.update(&Action::ResizeSidebar(100));
        assert_eq!(app.active_db().sidebar_pct, 50, "should clamp at 50");
    }

    #[tokio::test]
    async fn update_resize_editor_clamps() {
        let mut app = test_app_state().await;
        assert_eq!(app.active_db().editor_pct, 40);

        app.update(&Action::ResizeEditor(-100));
        assert_eq!(app.active_db().editor_pct, 20, "should clamp at 20");

        app.update(&Action::ResizeEditor(200));
        assert_eq!(app.active_db().editor_pct, 80, "should clamp at 80");
    }

    #[tokio::test]
    async fn update_show_ddl_sets_viewer_and_overlay() {
        let mut app = test_app_state().await;
        app.update(&Action::ShowDdl {
            name: "users".into(),
            sql: "CREATE TABLE users (id INT)".into(),
        });
        assert_eq!(app.active_overlay, Some(Overlay::DdlViewer));
        let viewer = app.ddl_viewer.as_ref().unwrap();
        assert_eq!(viewer.object_name, "users");
        assert_eq!(viewer.sql, "CREATE TABLE users (id INT)");
        assert_eq!(viewer.scroll, 0);
    }

    #[tokio::test]
    async fn update_recall_history_clears_overlay() {
        let mut app = test_app_state().await;
        app.active_overlay = Some(Overlay::History);
        app.update(&Action::RecallHistory("SELECT 1".into()));
        assert!(app.active_overlay.is_none(), "overlay should be cleared");
    }

    #[tokio::test]
    async fn update_show_bookmarks_requires_history_db() {
        let mut app = test_app_state().await;
        // history_db is None in test — ShowBookmarks should be a no-op
        app.update(&Action::ShowBookmarks);
        assert!(
            app.active_overlay.is_none(),
            "bookmarks should not open without history_db"
        );
    }
}
