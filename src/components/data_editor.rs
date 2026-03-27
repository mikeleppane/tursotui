use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;

use tursotui_sql::quoting::{format_value, quote_identifier, quote_literal};

use super::Component;
use super::cell_editor::CellEditor;
use crate::app::Action;
use crate::theme::Theme;
use tursotui_db::{ColumnInfo, ForeignKeyInfo, QueryResult, SchemaEntry};

// ---------------------------------------------------------------------------
// Visual marker types — used by ResultsTable to decorate edited rows/cells
// ---------------------------------------------------------------------------

/// Row-level visual marker: how this row is annotated in the results table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowMarker {
    Modified,
    Deleted,
}

/// Cell-level visual marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellMarker {
    Modified,
}

/// Status snapshot consumed by the status bar render function.
pub(crate) struct DataEditorStatus {
    /// Whether the data editor is currently active.
    pub active: bool,
    /// The table being edited, if any.
    pub table: Option<String>,
    /// Pending change counts `(updates, inserts, deletes)`.
    pub pending: (usize, usize, usize),
    /// Whether a cell editor modal is open.
    #[allow(dead_code)] // available for future status bar hints
    pub editing_cell: bool,
    /// Ordered table names from the FK navigation stack.
    pub fk_breadcrumbs: Vec<String>,
}

/// Pre-computed render state passed to `ResultsTable` before each draw call.
///
/// This is the **only** interface `ResultsTable` should use to understand edit state.
/// It provides methods for PK extraction, row marker lookup, and cell modification
/// checks. `ResultsTable` should never access `DataEditor`, `ChangeLog`, or `RowEdit`
/// directly.
///
/// `ChangeLog` is exposed to `dispatch.rs` for DML generation only.
pub(crate) struct EditRenderState {
    /// PK column indices — exposed for action dispatch (delete/revert need PK).
    pub(crate) pk_columns: Vec<usize>,
    /// Row-level annotations keyed by PK tuple.
    row_markers: HashMap<Vec<Option<String>>, RowMarker>,
    /// Set of `(pk_tuple, column_index)` pairs that have been modified.
    modified_cells: HashSet<(Vec<Option<String>>, usize)>,
    /// Pending INSERT rows to append after query rows.
    pub(crate) pending_inserts: Vec<Vec<Option<String>>>,
    /// Column indices with FK relationships (for header accent indicator).
    pub(crate) fk_columns: HashSet<usize>,
}

impl EditRenderState {
    /// Extract PK values from a display row.
    pub(crate) fn extract_pk(&self, row: &[Option<String>]) -> Vec<Option<String>> {
        self.pk_columns
            .iter()
            .map(|&i| row.get(i).cloned().flatten())
            .collect()
    }

    /// Look up the row marker for a display row (extracts PK internally).
    #[allow(dead_code)] // convenience API for future callers outside the render hot path
    pub(crate) fn row_marker_for(&self, row: &[Option<String>]) -> Option<RowMarker> {
        let pk = self.extract_pk(row);
        self.row_markers.get(&pk).copied()
    }

    /// Look up the row marker for a pre-extracted PK (avoids re-extraction in hot loops).
    pub(crate) fn row_marker_for_pk(&self, pk: &[Option<String>]) -> Option<RowMarker> {
        self.row_markers.get(pk).copied()
    }

    /// Check whether a specific cell has been modified (extracts PK internally).
    #[allow(dead_code)] // convenience API for future callers outside the render hot path
    pub(crate) fn is_cell_modified(&self, row: &[Option<String>], col: usize) -> bool {
        let pk = self.extract_pk(row);
        self.modified_cells.contains(&(pk, col))
    }

    /// Check whether a specific cell has been modified using a pre-extracted PK.
    pub(crate) fn is_cell_modified_pk(&self, pk: &[Option<String>], col: usize) -> bool {
        // `to_vec()` needed: `HashSet<(Vec<T>, usize)>` doesn't impl `Borrow<(&[T], usize)>`,
        // so `contains()` requires an owned tuple.
        self.modified_cells.contains(&(pk.to_vec(), col))
    }
}

// ---------------------------------------------------------------------------

/// Returns `true` if `table` matches a view in `entries` (case-insensitive).
#[allow(dead_code)] // will be called when data editor UI lands in a later task
pub(crate) fn check_view_rejection(table: &str, entries: &[SchemaEntry]) -> bool {
    let table_lower = table.to_lowercase();
    entries
        .iter()
        .any(|e| e.obj_type == "view" && e.name.to_lowercase() == table_lower)
}

/// Returns the indices of columns that are part of the primary key.
/// An empty result means no PK is known, making the table read-only.
#[allow(dead_code)] // will be called when data editor UI lands in a later task
pub(crate) fn find_pk_columns(table_columns: &[ColumnInfo]) -> Vec<usize> {
    table_columns
        .iter()
        .enumerate()
        .filter_map(|(i, col)| if col.pk { Some(i) } else { None })
        .collect()
}

// ---------------------------------------------------------------------------
// ChangeLog — tracks pending edits before they are committed to the database
// ---------------------------------------------------------------------------

/// A single pending edit for one row.
#[derive(Clone)]
#[allow(dead_code)] // variants constructed in tests; will be used by editor UI in a later task
pub(crate) enum RowEdit {
    Update {
        pk: Vec<Option<String>>,
        #[allow(dead_code)] // stored for future display / conflict resolution
        original: Vec<Option<String>>,
        /// Keyed by column index.  `None` value = explicitly set to NULL.
        modified: HashMap<usize, Option<String>>,
    },
    Insert {
        values: Vec<Option<String>>,
    },
    Delete {
        pk: Vec<Option<String>>,
        #[allow(dead_code)] // stored for future display / conflict resolution
        original: Vec<Option<String>>,
        /// If the row had pending edits before deletion, they're preserved here
        /// so `toggle_delete` (undelete) can restore the prior `Update` state.
        #[allow(dead_code)]
        prior_modified: Option<HashMap<usize, Option<String>>>,
    },
}

/// Maintains at most ONE `RowEdit` per PK. Multiple edits to the same row are
/// merged into a single `Update` entry (the `modified` map accumulates columns).
#[derive(Clone)]
pub(crate) struct ChangeLog {
    edits: Vec<RowEdit>,
}

#[allow(dead_code)] // methods used in tests; will be called by editor UI in a later task
impl ChangeLog {
    pub(crate) fn new() -> Self {
        Self { edits: Vec::new() }
    }

    /// Record that `col` in the row identified by `pk` was changed to `value`.
    ///
    /// If an `Update` already exists for this PK it is merged; otherwise a new
    /// `Update` entry is created (carrying `original_row` for reference).
    pub(crate) fn log_cell_edit(
        &mut self,
        pk: &[Option<String>],
        col: usize,
        value: Option<String>,
        original_row: &[Option<String>],
    ) {
        // Try to merge into an existing Update for this PK.
        for edit in &mut self.edits {
            if let RowEdit::Update {
                pk: existing_pk,
                modified,
                ..
            } = edit
                && existing_pk.as_slice() == pk
            {
                modified.insert(col, value);
                return;
            }
        }

        // No existing Update — create one.
        let mut modified = HashMap::new();
        modified.insert(col, value);
        self.edits.push(RowEdit::Update {
            pk: pk.to_vec(),
            original: original_row.to_vec(),
            modified,
        });
    }

    /// Append an `Insert` entry.
    pub(crate) fn log_insert(&mut self, values: Vec<Option<String>>) {
        self.edits.push(RowEdit::Insert { values });
    }

    /// Mark a row for deletion.
    ///
    /// If an `Update` exists for this PK it is dropped first (the update is
    /// superseded by the delete).
    pub(crate) fn log_delete(&mut self, pk: &[Option<String>], original_row: &[Option<String>]) {
        // If a prior Update exists for this PK, extract its modified map
        // so toggle_delete (undelete) can restore it.
        let prior_modified = self.edits.iter().find_map(|e| match e {
            RowEdit::Update {
                pk: existing_pk,
                modified,
                ..
            } if existing_pk.as_slice() == pk => Some(modified.clone()),
            _ => None,
        });
        self.edits.retain(|e| {
            !matches!(e, RowEdit::Update { pk: existing_pk, .. } if existing_pk.as_slice() == pk)
        });
        self.edits.push(RowEdit::Delete {
            pk: pk.to_vec(),
            original: original_row.to_vec(),
            prior_modified,
        });
    }

    /// Toggle deletion: if the row is already marked for deletion, remove the
    /// mark; otherwise call `log_delete`.
    pub(crate) fn toggle_delete(&mut self, pk: &[Option<String>], original_row: &[Option<String>]) {
        // Check whether a Delete entry already exists for this PK.
        let delete_pos = self.edits.iter().position(|e| {
            matches!(e, RowEdit::Delete { pk: existing_pk, .. } if existing_pk.as_slice() == pk)
        });

        if let Some(pos) = delete_pos {
            // Undelete: remove the Delete entry and restore any prior edits.
            let removed = self.edits.remove(pos);
            if let RowEdit::Delete {
                pk: del_pk,
                original,
                prior_modified: Some(modified),
            } = removed
            {
                // Restore the prior Update entry.
                self.edits.push(RowEdit::Update {
                    pk: del_pk,
                    original,
                    modified,
                });
            }
        } else {
            self.log_delete(pk, original_row);
        }
    }

    /// Remove the `Insert` at position `index` in the edits list (counting only
    /// `Insert` entries).
    ///
    /// `index` is the ordinal among `Insert` entries, not the raw position in
    /// `self.edits`.
    pub(crate) fn remove_insert(&mut self, index: usize) {
        let mut insert_count = 0usize;
        let mut target_pos = None;
        for (pos, edit) in self.edits.iter().enumerate() {
            if matches!(edit, RowEdit::Insert { .. }) {
                if insert_count == index {
                    target_pos = Some(pos);
                    break;
                }
                insert_count += 1;
            }
        }
        if let Some(pos) = target_pos {
            self.edits.remove(pos);
        }
    }

    /// Remove one modified column from an `Update`. If the `modified` map
    /// becomes empty after the removal, the entire `Update` entry is dropped.
    pub(crate) fn revert_cell(&mut self, pk: &[Option<String>], col: usize) {
        let mut remove_idx = None;
        for (i, edit) in self.edits.iter_mut().enumerate() {
            if let RowEdit::Update {
                pk: existing_pk,
                modified,
                ..
            } = edit
                && existing_pk.as_slice() == pk
            {
                modified.remove(&col);
                if modified.is_empty() {
                    remove_idx = Some(i);
                }
                break;
            }
        }
        if let Some(i) = remove_idx {
            self.edits.remove(i);
        }
    }

    /// Remove the entire entry (Update or Delete) for the given PK.
    pub(crate) fn revert_row(&mut self, pk: &[Option<String>]) {
        self.edits.retain(|e| match e {
            RowEdit::Update {
                pk: existing_pk, ..
            }
            | RowEdit::Delete {
                pk: existing_pk, ..
            } => existing_pk.as_slice() != pk,
            RowEdit::Insert { .. } => true,
        });
    }

    /// Clear all pending edits.
    pub(crate) fn revert_all(&mut self) {
        self.edits.clear();
    }

    /// Return counts of `(updates, inserts, deletes)`.
    pub(crate) fn pending_count(&self) -> (usize, usize, usize) {
        let mut updates = 0usize;
        let mut inserts = 0usize;
        let mut deletes = 0usize;
        for edit in &self.edits {
            match edit {
                RowEdit::Update { .. } => updates += 1,
                RowEdit::Insert { .. } => inserts += 1,
                RowEdit::Delete { .. } => deletes += 1,
            }
        }
        (updates, inserts, deletes)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Read-only access for DML generation.
    pub(crate) fn edits(&self) -> &[RowEdit] {
        &self.edits
    }
}

// ---------------------------------------------------------------------------
// DML generation
// ---------------------------------------------------------------------------

/// Generate a list of SQL DML statements (UPDATE / INSERT / DELETE) for all
/// pending changes in `changes`, in chronological order.
#[allow(dead_code)] // will be called from data editor UI in a later task
pub(crate) fn generate_dml(
    table: &str,
    columns: &[ColumnInfo],
    pk_columns: &[usize],
    changes: &ChangeLog,
) -> Vec<String> {
    // No PK columns means we cannot generate WHERE clauses for UPDATE/DELETE.
    // Return early — the caller should not activate editing for PK-less tables.
    if pk_columns.is_empty() {
        return Vec::new();
    }
    let table_q = quote_identifier(table);
    let mut stmts = Vec::new();

    for edit in changes.edits() {
        match edit {
            RowEdit::Update { pk, modified, .. } => {
                // Build SET clause — preserve column order for determinism.
                let mut set_pairs: Vec<(usize, &Option<String>)> =
                    modified.iter().map(|(&col, val)| (col, val)).collect();
                set_pairs.sort_by_key(|(col, _)| *col);

                let set_clause: Vec<String> = set_pairs
                    .iter()
                    .filter_map(|(col_idx, val)| {
                        columns.get(*col_idx).map(|col| {
                            format!(
                                "{} = {}",
                                quote_identifier(&col.name),
                                format_value(val.as_deref())
                            )
                        })
                    })
                    .collect();

                if set_clause.is_empty() {
                    continue;
                }

                let where_clause = build_where_clause(columns, pk_columns, pk);

                stmts.push(format!(
                    "UPDATE {} SET {} WHERE {}",
                    table_q,
                    set_clause.join(", "),
                    where_clause,
                ));
            }

            RowEdit::Insert { values } => {
                // Include all columns; use NULL for any missing tail entries.
                let col_list: Vec<String> =
                    columns.iter().map(|c| quote_identifier(&c.name)).collect();
                let val_list: Vec<String> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format_value(values.get(i).and_then(Option::as_deref)))
                    .collect();

                stmts.push(format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    table_q,
                    col_list.join(", "),
                    val_list.join(", "),
                ));
            }

            RowEdit::Delete { pk, .. } => {
                let where_clause = build_where_clause(columns, pk_columns, pk);
                stmts.push(format!("DELETE FROM {table_q} WHERE {where_clause}"));
            }
        }
    }

    stmts
}

/// Build the `WHERE pk1 = v1 AND pk2 = v2 ...` clause from the PK column
/// indices and the PK value tuple.
fn build_where_clause(
    columns: &[ColumnInfo],
    pk_columns: &[usize],
    pk: &[Option<String>],
) -> String {
    debug_assert!(
        !pk_columns.is_empty(),
        "build_where_clause called with no PK columns"
    );
    pk_columns
        .iter()
        .enumerate()
        .filter_map(|(pk_pos, &col_idx)| {
            let col = columns.get(col_idx)?;
            let val = pk.get(pk_pos).and_then(Option::as_ref);
            // Use IS NULL for NULL PKs — `= NULL` is always false in SQL
            Some(match val {
                Some(v) => format!("{} = {}", quote_identifier(&col.name), quote_literal(v)),
                None => format!("{} IS NULL", quote_identifier(&col.name)),
            })
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

// ---------------------------------------------------------------------------
// DataEditor struct
// ---------------------------------------------------------------------------

/// Full FK nav stack entry — holds all state needed to restore the previous
/// table when navigating back.
pub(crate) struct FKNavEntry {
    pub table: String,
    pub result: QueryResult,
    pub selected_row: usize,
    pub selected_col: usize,
    pub col_offset: usize,
    pub changes: ChangeLog,
    pub pending_inserts: Vec<Vec<Option<String>>>,
    pub activating_query: String,
    pub columns: Vec<ColumnInfo>,
    pub pk_columns: Vec<usize>,
}

/// The data editor state machine. Activated when a query targets a single
/// editable table with a known primary key. Deactivated on any non-editable
/// result or explicit dismissal.
pub(crate) struct DataEditor {
    source_table: Option<String>,
    pk_columns: Vec<usize>,
    columns: Vec<ColumnInfo>,
    activating_query: String,
    changes: ChangeLog,
    pending_inserts: Vec<Vec<Option<String>>>,
    cell_editor: Option<CellEditor>,
    fk_nav_stack: Vec<FKNavEntry>,
    preview_dml: Vec<String>,
    preview_scroll: usize,
    /// Original row snapshot stored when cell editor opens — passed to `ChangeLog` on confirm.
    editing_original_row: Vec<Option<String>>,
    active: bool,
    /// FK columns computed from loaded FK info — indices into `self.columns`.
    fk_column_set: HashSet<usize>,
}

impl DataEditor {
    pub(crate) fn new() -> Self {
        Self {
            source_table: None,
            pk_columns: Vec::new(),
            columns: Vec::new(),
            activating_query: String::new(),
            changes: ChangeLog::new(),
            pending_inserts: Vec::new(),
            cell_editor: None,
            fk_nav_stack: Vec::new(),
            preview_dml: Vec::new(),
            preview_scroll: 0,
            editing_original_row: Vec::new(),
            active: false,
            fk_column_set: HashSet::new(),
        }
    }

    /// Activate the data editor for a new table.
    ///
    /// Clears the FK nav stack (fresh query context). For FK navigation
    /// activations that must preserve the stack, use `activate_for_fk_nav`.
    pub(crate) fn activate(
        &mut self,
        table: String,
        pk_columns: Vec<usize>,
        columns: Vec<ColumnInfo>,
        query: String,
        _result: QueryResult, // cached on ResultsTable::last_result, not here
    ) {
        self.source_table = Some(table);
        self.pk_columns = pk_columns;
        self.columns = columns;
        self.activating_query = query;
        self.changes = ChangeLog::new();
        self.pending_inserts.clear();
        self.cell_editor = None;
        self.fk_nav_stack.clear(); // Fresh query — stack has no prior context
        self.preview_dml.clear();
        self.preview_scroll = 0;
        self.active = true;
        self.fk_column_set.clear();
    }

    /// Activate after FK navigation — preserves the FK nav stack so
    /// back-navigation can restore the prior table's state.
    pub(crate) fn activate_for_fk_nav(
        &mut self,
        table: String,
        pk_columns: Vec<usize>,
        columns: Vec<ColumnInfo>,
        query: String,
        _result: QueryResult, // cached on ResultsTable::last_result, not here
    ) {
        self.source_table = Some(table);
        self.pk_columns = pk_columns;
        self.columns = columns;
        self.activating_query = query;
        self.changes = ChangeLog::new();
        self.pending_inserts.clear();
        self.cell_editor = None;
        // Do NOT clear fk_nav_stack — the prior state was pushed before this query
        self.preview_dml.clear();
        self.preview_scroll = 0;
        self.active = true;
        self.fk_column_set.clear();
    }

    pub(crate) fn deactivate(&mut self) {
        self.source_table = None;
        self.pk_columns.clear();
        self.columns.clear();
        self.activating_query.clear();
        self.changes = ChangeLog::new();
        self.pending_inserts.clear();
        self.cell_editor = None;
        self.fk_nav_stack.clear();
        self.preview_dml.clear();
        self.preview_scroll = 0;
        self.active = false;
        self.fk_column_set.clear();
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn source_table(&self) -> Option<&str> {
        self.source_table.as_deref()
    }

    pub(crate) fn pk_columns(&self) -> &[usize] {
        &self.pk_columns
    }

    #[allow(dead_code)] // called internally via status(); kept for direct use in tests
    pub(crate) fn pending_count(&self) -> (usize, usize, usize) {
        self.changes.pending_count()
    }

    #[allow(dead_code)] // used by status bar in later tasks
    pub(crate) fn fk_depth(&self) -> usize {
        self.fk_nav_stack.len()
    }

    /// Returns table names from FK nav stack entries.
    #[allow(dead_code)] // called internally via status(); kept for direct use in tests
    pub(crate) fn fk_breadcrumbs(&self) -> Vec<&str> {
        self.fk_nav_stack.iter().map(|e| e.table.as_str()).collect()
    }

    pub(crate) fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    #[allow(dead_code)] // used by DML preview in later tasks
    pub(crate) fn changes(&self) -> &ChangeLog {
        &self.changes
    }

    #[allow(dead_code)] // used by data editor key handler in later tasks
    pub(crate) fn activating_query(&self) -> &str {
        &self.activating_query
    }

    /// Returns `true` if a cell editor is currently open.
    #[allow(dead_code)] // used in tests and future inline rendering
    pub(crate) fn has_cell_editor(&self) -> bool {
        self.cell_editor.is_some()
    }

    /// Access the active cell editor (for rendering in main.rs).
    pub(crate) fn cell_editor(&self) -> Option<&CellEditor> {
        self.cell_editor.as_ref()
    }

    /// Snapshot of data editor state for the status bar.
    pub(crate) fn status(&self) -> DataEditorStatus {
        DataEditorStatus {
            active: self.active,
            table: self.source_table.clone(),
            pending: self.changes.pending_count(),
            editing_cell: self.cell_editor.is_some(),
            fk_breadcrumbs: self.fk_nav_stack.iter().map(|e| e.table.clone()).collect(),
        }
    }

    // ------------------------------------------------------------------
    // Visual marker helpers (Task 6)
    // ------------------------------------------------------------------

    /// Look up the row-level marker for a row identified by its PK values.
    #[allow(dead_code)] // used in tests; `build_render_state` is the production path
    pub(crate) fn row_marker(&self, pk: &[Option<String>]) -> Option<RowMarker> {
        for edit in self.changes.edits() {
            match edit {
                RowEdit::Update { pk: epk, .. } if epk.as_slice() == pk => {
                    return Some(RowMarker::Modified);
                }
                RowEdit::Delete { pk: epk, .. } if epk.as_slice() == pk => {
                    return Some(RowMarker::Deleted);
                }
                _ => {}
            }
        }
        None
    }

    /// Look up the cell-level marker for `(pk, col_index)`.
    #[allow(dead_code)] // used in tests; `build_render_state` is the production path
    pub(crate) fn cell_marker(&self, pk: &[Option<String>], col: usize) -> Option<CellMarker> {
        for edit in self.changes.edits() {
            if let RowEdit::Update {
                pk: epk, modified, ..
            } = edit
                && epk.as_slice() == pk
                && modified.contains_key(&col)
            {
                return Some(CellMarker::Modified);
            }
        }
        None
    }

    /// Pending insert rows (those appended to the bottom of the results view).
    #[allow(dead_code)] // exposed for tests and future detail view
    pub(crate) fn pending_inserts(&self) -> &[Vec<Option<String>>] {
        &self.pending_inserts
    }

    /// FK columns — column indices that have a foreign key relationship.
    pub(crate) fn fk_columns(&self) -> HashSet<usize> {
        self.fk_column_set.clone()
    }

    /// Update `fk_column_set` from a slice of `ForeignKeyInfo` for the active table.
    /// Maps `from_column` names to column indices using `self.columns`.
    pub(crate) fn update_fk_columns(&mut self, fk_info: &[ForeignKeyInfo]) {
        self.fk_column_set.clear();
        for fk in fk_info {
            if let Some(idx) = self.columns.iter().position(|c| c.name == fk.from_column) {
                self.fk_column_set.insert(idx);
            }
        }
    }

    /// Push the current editor state onto the FK nav stack before navigating to a linked table.
    /// Drops the oldest entry if the stack already has 10 entries.
    pub(crate) fn push_fk_state(
        &mut self,
        result: QueryResult,
        selected_row: usize,
        selected_col: usize,
        col_offset: usize,
    ) {
        if self.fk_nav_stack.len() >= 10 {
            self.fk_nav_stack.remove(0); // drop oldest
        }
        self.fk_nav_stack.push(FKNavEntry {
            table: self.source_table.clone().unwrap_or_default(),
            result,
            selected_row,
            selected_col,
            col_offset,
            changes: self.changes.clone(),
            pending_inserts: self.pending_inserts.clone(),
            activating_query: self.activating_query.clone(),
            columns: self.columns.clone(),
            pk_columns: self.pk_columns.clone(),
        });
    }

    /// Pop the top FK nav entry to restore the previous table's state.
    pub(crate) fn pop_fk_state(&mut self) -> Option<FKNavEntry> {
        self.fk_nav_stack.pop()
    }

    /// Restore editor state from a popped FK nav entry.
    pub(crate) fn restore_from_fk_entry(&mut self, entry: FKNavEntry) {
        self.source_table = Some(entry.table);
        self.activating_query = entry.activating_query;
        self.changes = entry.changes;
        self.pending_inserts = entry.pending_inserts;
        self.columns = entry.columns;
        self.pk_columns = entry.pk_columns;
        self.cell_editor = None;
        self.preview_dml.clear();
        self.preview_scroll = 0;
        self.editing_original_row.clear();
        self.active = true;
        self.fk_column_set.clear();
        // fk_nav_stack is NOT touched — it was already popped
    }

    // ------------------------------------------------------------------
    // DML preview accessors (Task 10)
    // ------------------------------------------------------------------

    /// The generated DML statements for the current preview.
    pub(crate) fn preview_dml(&self) -> &[String] {
        &self.preview_dml
    }

    /// Current scroll offset for the DML preview popup.
    pub(crate) fn preview_scroll(&self) -> usize {
        self.preview_scroll
    }

    /// Store generated DML statements.
    pub(crate) fn set_preview_dml(&mut self, stmts: Vec<String>) {
        self.preview_dml = stmts;
        self.preview_scroll = 0;
    }

    /// Scroll the preview down by one line.
    pub(crate) fn scroll_preview_down(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_add(1);
    }

    /// Scroll the preview up by one line.
    pub(crate) fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(1);
    }

    /// Build the `EditRenderState` snapshot passed to `ResultsTable` before render.
    pub(crate) fn build_render_state(&self) -> EditRenderState {
        let mut row_markers = HashMap::new();
        let mut modified_cells = HashSet::new();

        for edit in self.changes.edits() {
            match edit {
                RowEdit::Update { pk, modified, .. } => {
                    row_markers.insert(pk.clone(), RowMarker::Modified);
                    for &col in modified.keys() {
                        modified_cells.insert((pk.clone(), col));
                    }
                }
                RowEdit::Delete { pk, .. } => {
                    row_markers.insert(pk.clone(), RowMarker::Deleted);
                }
                RowEdit::Insert { .. } => {
                    // Pending inserts are surfaced via pending_inserts field
                }
            }
        }

        EditRenderState {
            pk_columns: self.pk_columns.clone(),
            row_markers,
            modified_cells,
            pending_inserts: self.pending_inserts.clone(),
            fk_columns: self.fk_columns(),
        }
    }

    // ------------------------------------------------------------------
    // Mutation methods (Task 9)
    // ------------------------------------------------------------------

    /// Open a cell editor for the given cell.
    ///
    /// `original_row` is the full row snapshot from `ResultsTable` — stored so
    /// `confirm_edit` can pass it to `ChangeLog::log_cell_edit` for the `original`
    /// field on first edit of this row.
    pub(crate) fn start_cell_edit(
        &mut self,
        pk: Vec<Option<String>>,
        row: usize,
        col: usize,
        value: Option<&str>,
        notnull: bool,
        original_row: Vec<Option<String>>,
    ) {
        // Always use modal mode — inline rendering is not yet wired into
        // ResultsTable's cell layout. Modal provides a visible popup for all edits.
        // TODO: wire inline render_inline() into ResultsTable for short values.
        let modal = true;
        self.cell_editor = Some(CellEditor::new(pk, row, col, value, notnull, modal));
        self.editing_original_row = original_row;
    }

    /// Confirm the current cell edit, writing the new value into the `ChangeLog`.
    pub(crate) fn confirm_edit(&mut self, value: Option<String>) {
        let Some(editor) = self.cell_editor.take() else {
            return;
        };
        self.changes
            .log_cell_edit(&editor.pk, editor.col, value, &self.editing_original_row);
    }

    /// Cancel the current cell edit without changes.
    pub(crate) fn cancel_edit(&mut self) {
        self.cell_editor = None;
    }

    /// Append a NULL-filled row to `pending_inserts` and `ChangeLog`.
    pub(crate) fn add_row(&mut self) {
        let null_row: Vec<Option<String>> = self.columns.iter().map(|_| None).collect();
        self.changes.log_insert(null_row.clone());
        self.pending_inserts.push(null_row);
    }

    /// Clone an existing row into `pending_inserts` and `ChangeLog`.
    pub(crate) fn clone_row(&mut self, values: Vec<Option<String>>) {
        self.changes.log_insert(values.clone());
        self.pending_inserts.push(values);
    }

    /// Toggle the delete mark for a row.
    pub(crate) fn toggle_delete_row(&mut self, pk: &[Option<String>], original: &[Option<String>]) {
        self.changes.toggle_delete(pk, original);
    }

    /// Revert the modified cell at `(pk, col)`.
    pub(crate) fn revert_cell_edit(&mut self, pk: &[Option<String>], col: usize) {
        self.changes.revert_cell(pk, col);
    }

    /// Revert all changes for a row (Update or Delete).
    pub(crate) fn revert_row_edit(&mut self, pk: &[Option<String>]) {
        self.changes.revert_row(pk);
    }

    /// Revert everything — clear `ChangeLog` and `pending_inserts`.
    pub(crate) fn revert_all_edits(&mut self) {
        self.changes.revert_all();
        self.pending_inserts.clear();
    }

    /// Remove a pending insert by index (for deleting an uncommitted row).
    pub(crate) fn remove_pending_insert(&mut self, insert_idx: usize) {
        if insert_idx < self.pending_inserts.len() {
            self.pending_inserts.remove(insert_idx);
            self.changes.remove_insert(insert_idx);
        }
    }
}

impl Component for DataEditor {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if !self.active {
            return None;
        }

        // Delegate ALL keys to cell editor when one is open
        if let Some(ref mut editor) = self.cell_editor {
            return editor.handle_key(key);
        }

        // Edit-mode keys
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('e') | KeyCode::F(2)) => Some(Action::StartCellEdit),
            (KeyModifiers::NONE, KeyCode::Char('a')) => Some(Action::AddRow),
            (KeyModifiers::NONE, KeyCode::Char('d')) => Some(Action::ToggleDeleteRow),
            (KeyModifiers::NONE, KeyCode::Char('c')) => {
                // Signal: main.rs will read actual row data and call clone_row()
                Some(Action::CloneRow)
            }
            (KeyModifiers::NONE, KeyCode::Char('u')) => Some(Action::RevertCell),
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('U')) => {
                Some(Action::RevertRow)
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) => Some(Action::RevertAll),
            (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(Action::ShowDmlPreview(false)),
            (KeyModifiers::CONTROL, KeyCode::Char('s')) => Some(Action::ShowDmlPreview(true)),
            (KeyModifiers::NONE, KeyCode::Char('f')) => Some(Action::FollowFK),
            (KeyModifiers::ALT, KeyCode::Left) => {
                if self.fk_nav_stack.is_empty() {
                    None // fall through to ResultsTable
                } else {
                    Some(Action::FKNavigateBack)
                }
            }
            _ => None, // fall through to ResultsTable
        }
    }

    fn update(&mut self, action: &Action) {
        match action {
            Action::ConfirmCellEdit(value) => {
                self.confirm_edit(value.clone());
            }
            Action::CancelCellEdit => {
                self.cancel_edit();
            }
            Action::AddRow => {
                self.add_row();
            }
            _ => {}
        }
    }

    fn render(&mut self, _frame: &mut Frame, _area: Rect, _focused: bool, _theme: &Theme) {
        // No-op — DataEditor injects state into ResultsTable, not direct rendering
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tursotui_db::{ColumnInfo, SchemaEntry};

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn make_col(pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: String::new(),
            col_type: String::new(),
            notnull: false,
            default_value: None,
            pk,
        }
    }

    fn make_entry(obj_type: &str, name: &str) -> SchemaEntry {
        SchemaEntry {
            obj_type: obj_type.to_string(),
            name: name.to_string(),
            tbl_name: name.to_string(),
            sql: None,
        }
    }

    // -------------------------------------------------------------------------
    // find_pk_columns  (detect_source_table tests live in tursotui-sql crate)
    // -------------------------------------------------------------------------

    #[test]
    fn test_find_pk_single_column() {
        let cols = vec![make_col(true), make_col(false)];
        assert_eq!(find_pk_columns(&cols), vec![0]);
    }

    #[test]
    fn test_find_pk_composite() {
        let cols = vec![make_col(true), make_col(false), make_col(true)];
        assert_eq!(find_pk_columns(&cols), vec![0, 2]);
    }

    #[test]
    fn test_find_pk_none() {
        let cols = vec![make_col(false), make_col(false)];
        let result: Vec<usize> = find_pk_columns(&cols);
        assert!(result.is_empty());
    }

    // -------------------------------------------------------------------------
    // check_view_rejection
    // -------------------------------------------------------------------------

    #[test]
    fn test_view_rejected() {
        let entries = vec![make_entry("view", "users")];
        assert!(check_view_rejection("users", &entries));
    }

    #[test]
    fn test_table_not_rejected() {
        let entries = vec![make_entry("table", "users")];
        assert!(!check_view_rejection("users", &entries));
    }

    #[test]
    fn test_case_insensitive_view_check() {
        let entries = vec![make_entry("view", "users")];
        assert!(check_view_rejection("USERS", &entries));
    }

    // -------------------------------------------------------------------------
    // ChangeLog helpers
    // -------------------------------------------------------------------------

    fn pk(vals: &[&str]) -> Vec<Option<String>> {
        vals.iter().map(|v| Some(v.to_string())).collect()
    }

    fn row(vals: &[&str]) -> Vec<Option<String>> {
        vals.iter().map(|v| Some(v.to_string())).collect()
    }

    fn named_col(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            col_type: String::new(),
            notnull: false,
            default_value: None,
            pk: false,
        }
    }

    fn pk_col(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            col_type: String::new(),
            notnull: false,
            default_value: None,
            pk: true,
        }
    }

    // -------------------------------------------------------------------------
    // ChangeLog tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_log_cell_edit_creates_update() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["1"]);
        let orig = row(&["1", "Alice"]);
        log.log_cell_edit(&pk_val, 1, Some("Bob".to_string()), &orig);

        assert_eq!(log.edits().len(), 1);
        let (updates, inserts, deletes) = log.pending_count();
        assert_eq!((updates, inserts, deletes), (1, 0, 0));

        if let RowEdit::Update { modified, .. } = &log.edits()[0] {
            assert_eq!(modified.len(), 1);
            assert_eq!(modified[&1], Some("Bob".to_string()));
        } else {
            panic!("expected Update");
        }
    }

    #[test]
    fn test_log_cell_edit_merges_same_row() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["1"]);
        let orig = row(&["1", "Alice", "NY"]);
        log.log_cell_edit(&pk_val, 1, Some("Bob".to_string()), &orig);
        log.log_cell_edit(&pk_val, 2, Some("LA".to_string()), &orig);

        // Still one entry, both columns present.
        assert_eq!(log.edits().len(), 1);
        if let RowEdit::Update { modified, .. } = &log.edits()[0] {
            assert_eq!(modified.len(), 2);
            assert_eq!(modified[&1], Some("Bob".to_string()));
            assert_eq!(modified[&2], Some("LA".to_string()));
        } else {
            panic!("expected Update");
        }
    }

    #[test]
    fn test_log_delete_marks_row() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["42"]);
        let orig = row(&["42", "Eve"]);
        log.toggle_delete(&pk_val, &orig);

        assert_eq!(log.edits().len(), 1);
        let (updates, inserts, deletes) = log.pending_count();
        assert_eq!((updates, inserts, deletes), (0, 0, 1));
        assert!(matches!(log.edits()[0], RowEdit::Delete { .. }));
    }

    #[test]
    fn test_log_delete_after_edit_coalesces() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["5"]);
        let orig = row(&["5", "Carol"]);

        // Edit the row first.
        log.log_cell_edit(&pk_val, 1, Some("Carrie".to_string()), &orig);
        assert_eq!(log.edits().len(), 1);

        // Now delete — the Update should be replaced by a Delete.
        log.log_delete(&pk_val, &orig);
        assert_eq!(log.edits().len(), 1);
        assert!(matches!(log.edits()[0], RowEdit::Delete { .. }));
    }

    #[test]
    fn test_log_insert_then_delete_cancels() {
        let mut log = ChangeLog::new();
        log.log_insert(row(&["", "new"]));
        assert_eq!(log.edits().len(), 1);

        log.remove_insert(0);
        assert!(log.is_empty());
    }

    #[test]
    fn test_toggle_delete_twice_restores() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["7"]);
        let orig = row(&["7", "Dave"]);

        log.toggle_delete(&pk_val, &orig);
        assert!(!log.is_empty());

        log.toggle_delete(&pk_val, &orig);
        assert!(log.is_empty());
    }

    #[test]
    fn test_edit_then_delete_then_undelete_restores_edit() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["1"]);
        let orig = row(&["1", "Alice"]);

        // Edit a cell
        log.log_cell_edit(&pk_val, 1, Some("Bob".to_string()), &orig);
        assert_eq!(log.pending_count(), (1, 0, 0));

        // Delete the row — edit should be preserved internally
        log.toggle_delete(&pk_val, &orig);
        assert_eq!(log.pending_count(), (0, 0, 1));

        // Undelete — the prior edit should be restored
        log.toggle_delete(&pk_val, &orig);
        assert_eq!(log.pending_count(), (1, 0, 0));

        // Verify the restored edit has the correct modified column
        let edits = log.edits();
        assert_eq!(edits.len(), 1);
        match &edits[0] {
            RowEdit::Update { modified, .. } => {
                assert_eq!(modified.get(&1), Some(&Some("Bob".to_string())));
            }
            _ => panic!("Expected Update after undelete"),
        }
    }

    #[test]
    fn test_null_pk_generates_is_null() {
        let columns = make_columns();
        let mut log = ChangeLog::new();
        // PK has a NULL value
        log.log_cell_edit(
            &[None],
            1,
            Some("updated".to_string()),
            &[None, Some("old".to_string()), Some("email".to_string())],
        );
        let stmts = generate_dml("users", &columns, &[0], &log);
        assert_eq!(stmts.len(), 1);
        assert!(
            stmts[0].contains("IS NULL"),
            "Expected IS NULL for NULL PK, got: {}",
            stmts[0]
        );
    }

    #[test]
    fn test_revert_cell() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["1"]);
        let orig = row(&["1", "Alice", "NY"]);
        log.log_cell_edit(&pk_val, 1, Some("Bob".to_string()), &orig);
        log.log_cell_edit(&pk_val, 2, Some("LA".to_string()), &orig);

        // Revert column 1, column 2 should remain.
        log.revert_cell(&pk_val, 1);
        assert_eq!(log.edits().len(), 1);
        if let RowEdit::Update { modified, .. } = &log.edits()[0] {
            assert!(!modified.contains_key(&1));
            assert_eq!(modified[&2], Some("LA".to_string()));
        } else {
            panic!("expected Update");
        }
    }

    #[test]
    fn test_revert_cell_removes_entry_when_empty() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["1"]);
        let orig = row(&["1", "Alice"]);
        log.log_cell_edit(&pk_val, 1, Some("Bob".to_string()), &orig);

        log.revert_cell(&pk_val, 1);
        assert!(log.is_empty());
    }

    #[test]
    fn test_revert_row() {
        let mut log = ChangeLog::new();
        let pk_val = pk(&["3"]);
        let orig = row(&["3", "Frank"]);
        log.log_cell_edit(&pk_val, 1, Some("Fred".to_string()), &orig);

        log.revert_row(&pk_val);
        assert!(log.is_empty());
    }

    #[test]
    fn test_revert_all() {
        let mut log = ChangeLog::new();
        let pk1 = pk(&["1"]);
        let orig1 = row(&["1", "A"]);
        let pk2 = pk(&["2"]);
        let orig2 = row(&["2", "B"]);

        log.log_cell_edit(&pk1, 1, Some("X".to_string()), &orig1);
        log.log_insert(row(&["3", "C"]));
        log.log_delete(&pk2, &orig2);

        assert!(!log.is_empty());
        log.revert_all();
        assert!(log.is_empty());
    }

    #[test]
    fn test_pending_count() {
        let mut log = ChangeLog::new();
        let pk1 = pk(&["1"]);
        let orig1 = row(&["1", "A"]);
        let pk2 = pk(&["2"]);
        let orig2 = row(&["2", "B"]);

        log.log_cell_edit(&pk1, 1, Some("X".to_string()), &orig1);
        log.log_insert(row(&["3", "C"]));
        log.log_insert(row(&["4", "D"]));
        log.log_delete(&pk2, &orig2);

        let (updates, inserts, deletes) = log.pending_count();
        assert_eq!(updates, 1);
        assert_eq!(inserts, 2);
        assert_eq!(deletes, 1);
    }

    #[test]
    fn test_is_empty() {
        let mut log = ChangeLog::new();
        assert!(log.is_empty());

        let pk_val = pk(&["1"]);
        let orig = row(&["1", "A"]);
        log.log_cell_edit(&pk_val, 0, Some("X".to_string()), &orig);
        assert!(!log.is_empty());
    }

    // -------------------------------------------------------------------------
    // DML generation tests
    // -------------------------------------------------------------------------

    /// Columns: id (pk), name, city
    fn make_columns() -> Vec<ColumnInfo> {
        vec![pk_col("id"), named_col("name"), named_col("city")]
    }

    #[test]
    fn test_generate_update_single_col() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        let pk_val = pk(&["1"]);
        let orig = row(&["1", "Alice", "NY"]);
        log.log_cell_edit(&pk_val, 1, Some("Bob".to_string()), &orig);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "name" = 'Bob' WHERE "id" = '1'"#
        );
    }

    #[test]
    fn test_generate_update_multiple_cols() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        let pk_val = pk(&["2"]);
        let orig = row(&["2", "Carol", "LA"]);
        log.log_cell_edit(&pk_val, 1, Some("Carrie".to_string()), &orig);
        log.log_cell_edit(&pk_val, 2, Some("SF".to_string()), &orig);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        // Columns ordered by index: name (1), city (2).
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "name" = 'Carrie', "city" = 'SF' WHERE "id" = '2'"#
        );
    }

    #[test]
    fn test_generate_insert() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        log.log_insert(vec![
            Some("10".to_string()),
            Some("Dan".to_string()),
            Some("Boston".to_string()),
        ]);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"INSERT INTO "users" ("id", "name", "city") VALUES ('10', 'Dan', 'Boston')"#
        );
    }

    #[test]
    fn test_generate_delete() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        let pk_val = pk(&["5"]);
        let orig = row(&["5", "Eve", "Miami"]);
        log.log_delete(&pk_val, &orig);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], r#"DELETE FROM "users" WHERE "id" = '5'"#);
    }

    #[test]
    fn test_generate_null_value() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        let pk_val = pk(&["3"]);
        let orig = row(&["3", "Frank", "Dallas"]);
        // Explicitly set city to NULL.
        log.log_cell_edit(&pk_val, 2, None, &orig);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "city" = NULL WHERE "id" = '3'"#
        );
    }

    #[test]
    fn test_generate_empty_string_value() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        let pk_val = pk(&["4"]);
        let orig = row(&["4", "Grace", "Phoenix"]);
        // Empty string — NOT NULL.
        log.log_cell_edit(&pk_val, 1, Some(String::new()), &orig);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "name" = '' WHERE "id" = '4'"#
        );
    }

    #[test]
    fn test_sql_escape_single_quote() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        let pk_val = pk(&["6"]);
        let orig = row(&["6", "normal", "normal"]);
        log.log_cell_edit(&pk_val, 1, Some("it's".to_string()), &orig);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "name" = 'it''s' WHERE "id" = '6'"#
        );
    }

    #[test]
    fn test_sql_escape_identifier() {
        // Table name with a space, column name with a double-quote.
        let cols = vec![pk_col("id"), named_col(r#"col"name"#)];
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();
        let pk_val = pk(&["1"]);
        let orig = row(&["1", "v"]);
        log.log_cell_edit(&pk_val, 1, Some("v2".to_string()), &orig);

        let stmts = generate_dml("my table", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        // Table: "my table"; column: "col""name"
        assert_eq!(
            stmts[0],
            r#"UPDATE "my table" SET "col""name" = 'v2' WHERE "id" = '1'"#
        );
    }

    #[test]
    fn test_generate_composite_pk() {
        // Two-column PK: (tenant_id, user_id).
        let cols = vec![pk_col("tenant_id"), pk_col("user_id"), named_col("email")];
        let pk_cols = vec![0usize, 1usize];
        let mut log = ChangeLog::new();
        let pk_val = vec![Some("10".to_string()), Some("99".to_string())];
        let orig = vec![
            Some("10".to_string()),
            Some("99".to_string()),
            Some("old@example.com".to_string()),
        ];
        log.log_cell_edit(&pk_val, 2, Some("new@example.com".to_string()), &orig);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "email" = 'new@example.com' WHERE "tenant_id" = '10' AND "user_id" = '99'"#
        );
    }

    #[test]
    fn test_generate_mixed_changes() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let mut log = ChangeLog::new();

        // Update row 1.
        let pk1 = pk(&["1"]);
        let orig1 = row(&["1", "A", "X"]);
        log.log_cell_edit(&pk1, 1, Some("B".to_string()), &orig1);

        // Insert row.
        log.log_insert(vec![
            Some("99".to_string()),
            Some("New".to_string()),
            Some("City".to_string()),
        ]);

        // Delete row 2.
        let pk2 = pk(&["2"]);
        let orig2 = row(&["2", "C", "Y"]);
        log.log_delete(&pk2, &orig2);

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert_eq!(stmts.len(), 3);

        // Verify statement types in order.
        assert!(stmts[0].starts_with("UPDATE"));
        assert!(stmts[1].starts_with("INSERT"));
        assert!(stmts[2].starts_with("DELETE"));
    }

    #[test]
    fn test_generate_empty_changelog() {
        let cols = make_columns();
        let pk_cols = vec![0usize];
        let log = ChangeLog::new();

        let stmts = generate_dml("users", &cols, &pk_cols, &log);
        assert!(stmts.is_empty());
    }

    // -------------------------------------------------------------------------
    // DataEditor visual markers (Task 6)
    // -------------------------------------------------------------------------

    fn make_empty_result() -> QueryResult {
        use std::time::Duration;
        use tursotui_db::QueryKind;
        QueryResult {
            columns: vec![],
            rows: vec![],
            execution_time: Duration::ZERO,
            truncated: false,
            sql: String::new(),
            rows_affected: 0,
            query_kind: QueryKind::Select,
            source_table: None,
        }
    }

    fn make_data_editor_activated() -> DataEditor {
        let columns = vec![
            ColumnInfo {
                name: "id".to_string(),
                col_type: "INTEGER".to_string(),
                notnull: true,
                default_value: None,
                pk: true,
            },
            ColumnInfo {
                name: "name".to_string(),
                col_type: "TEXT".to_string(),
                notnull: false,
                default_value: None,
                pk: false,
            },
        ];
        let mut de = DataEditor::new();
        de.activate(
            "users".to_string(),
            vec![0],
            columns,
            "SELECT * FROM users".to_string(),
            make_empty_result(),
        );
        de
    }

    fn some_pk(id: &str) -> Vec<Option<String>> {
        vec![Some(id.to_string())]
    }

    fn some_row(id: &str, name: &str) -> Vec<Option<String>> {
        vec![Some(id.to_string()), Some(name.to_string())]
    }

    #[test]
    fn test_row_marker_modified() {
        let mut de = make_data_editor_activated();
        let pk = some_pk("1");
        let orig = some_row("1", "Alice");
        de.changes
            .log_cell_edit(&pk, 1, Some("Alicia".to_string()), &orig);
        assert_eq!(de.row_marker(&pk), Some(RowMarker::Modified));
    }

    #[test]
    fn test_row_marker_deleted() {
        let mut de = make_data_editor_activated();
        let pk = some_pk("2");
        let orig = some_row("2", "Bob");
        de.changes.log_delete(&pk, &orig);
        assert_eq!(de.row_marker(&pk), Some(RowMarker::Deleted));
    }

    #[test]
    fn test_cell_marker_modified() {
        let mut de = make_data_editor_activated();
        let pk = some_pk("3");
        let orig = some_row("3", "Carol");
        de.changes
            .log_cell_edit(&pk, 1, Some("Caroline".to_string()), &orig);
        // Column 1 is modified
        assert_eq!(de.cell_marker(&pk, 1), Some(CellMarker::Modified));
        // Column 0 (PK) is NOT modified
        assert_eq!(de.cell_marker(&pk, 0), None);
    }

    #[test]
    fn test_row_marker_none_for_unchanged() {
        let de = make_data_editor_activated();
        let pk = some_pk("99");
        assert_eq!(de.row_marker(&pk), None);
        assert_eq!(de.cell_marker(&pk, 0), None);
    }

    // -------------------------------------------------------------------------
    // FK navigation stack tests (Task 13)
    // -------------------------------------------------------------------------

    #[test]
    fn test_push_pop_fk_stack() {
        let mut de = make_data_editor_activated();
        let result = make_empty_result();
        de.push_fk_state(result, 2, 1, 0);

        assert_eq!(de.fk_depth(), 1);
        let entry = de.pop_fk_state().unwrap();
        assert_eq!(entry.table, "users");
        assert_eq!(entry.activating_query, "SELECT * FROM users");
        assert_eq!(entry.selected_row, 2);
        assert_eq!(entry.selected_col, 1);
        assert_eq!(entry.col_offset, 0);
        assert_eq!(de.fk_depth(), 0);
    }

    #[test]
    fn test_fk_stack_max_10() {
        let mut de = make_data_editor_activated();
        // Push 11 entries
        for i in 0..11u32 {
            let result = make_empty_result();
            de.push_fk_state(result, i as usize, 0, 0);
        }
        // Only 10 remain — the oldest was dropped
        assert_eq!(de.fk_depth(), 10);
    }

    #[test]
    fn test_fk_stack_preserves_changes() {
        let mut de = make_data_editor_activated();
        // Edit a cell
        let pk_val = some_pk("1");
        let orig = some_row("1", "Alice");
        de.changes
            .log_cell_edit(&pk_val, 1, Some("Alicia".to_string()), &orig);
        assert_eq!(de.changes.pending_count(), (1, 0, 0));

        // Push state onto the stack
        let result = make_empty_result();
        de.push_fk_state(result, 0, 0, 0);

        // Pop back — changes should be restored
        let entry = de.pop_fk_state().unwrap();
        de.restore_from_fk_entry(entry);
        assert_eq!(de.changes.pending_count(), (1, 0, 0));
    }

    #[test]
    fn test_fk_breadcrumbs() {
        let mut de = make_data_editor_activated();

        // Push "users" state
        let result1 = make_empty_result();
        de.push_fk_state(result1, 0, 0, 0);

        // Simulate navigating to "departments"
        let dept_cols = vec![ColumnInfo {
            name: "dept_id".to_string(),
            col_type: "INTEGER".to_string(),
            notnull: true,
            default_value: None,
            pk: true,
        }];
        de.activate_for_fk_nav(
            "departments".to_string(),
            vec![0],
            dept_cols,
            "SELECT * FROM departments WHERE id = '1'".to_string(),
            make_empty_result(),
        );

        // Push "departments" state
        let result2 = make_empty_result();
        de.push_fk_state(result2, 0, 0, 0);

        let crumbs = de.fk_breadcrumbs();
        assert_eq!(crumbs, vec!["users", "departments"]);
    }

    #[test]
    fn test_update_fk_columns() {
        let mut de = make_data_editor_activated();
        // "users" table has columns: id (0), name (1)
        // Simulate FK on the "name" column (unusual but for test)
        let fk_info = vec![ForeignKeyInfo {
            from_column: "name".to_string(),
            to_table: "other".to_string(),
            to_column: "col".to_string(),
        }];
        de.update_fk_columns(&fk_info);
        let fk_cols = de.fk_columns();
        assert!(fk_cols.contains(&1)); // "name" is at index 1
        assert!(!fk_cols.contains(&0)); // "id" is not an FK
    }

    #[test]
    fn test_update_fk_columns_unknown_name() {
        let mut de = make_data_editor_activated();
        // FK references a column not in the schema — should be ignored
        let fk_info = vec![ForeignKeyInfo {
            from_column: "nonexistent".to_string(),
            to_table: "other".to_string(),
            to_column: "col".to_string(),
        }];
        de.update_fk_columns(&fk_info);
        assert!(de.fk_columns().is_empty());
    }

    // -------------------------------------------------------------------------
    // Integration tests — multi-step workflows
    // -------------------------------------------------------------------------

    /// Build a 3-column `DataEditor`: id (PK), name, email.
    fn make_editor_3col() -> DataEditor {
        use std::time::Duration;
        use tursotui_db::QueryKind;
        let columns = vec![
            ColumnInfo {
                name: "id".to_string(),
                col_type: "INTEGER".to_string(),
                notnull: true,
                default_value: None,
                pk: true,
            },
            ColumnInfo {
                name: "name".to_string(),
                col_type: "TEXT".to_string(),
                notnull: false,
                default_value: None,
                pk: false,
            },
            ColumnInfo {
                name: "email".to_string(),
                col_type: "TEXT".to_string(),
                notnull: false,
                default_value: None,
                pk: false,
            },
        ];
        let result = QueryResult {
            columns: vec![],
            rows: vec![],
            execution_time: Duration::ZERO,
            truncated: false,
            sql: String::new(),
            rows_affected: 0,
            query_kind: QueryKind::Select,
            source_table: None,
        };
        let mut de = DataEditor::new();
        de.activate(
            "users".to_string(),
            vec![0],
            columns,
            "SELECT * FROM users".to_string(),
            result,
        );
        de
    }

    #[test]
    fn test_full_edit_workflow() {
        let mut de = make_editor_3col();

        // Open cell editor for row 0, col 1 (name "Alice"), pk = ["1"]
        let pk_val = vec![Some("1".to_string())];
        let orig = vec![
            Some("1".to_string()),
            Some("Alice".to_string()),
            Some("alice@example.com".to_string()),
        ];
        de.start_cell_edit(pk_val, 0, 1, Some("Alice"), false, orig);

        // Confirm the edit with a new value
        de.confirm_edit(Some("Bob".to_string()));

        // ChangeLog should have exactly 1 Update
        let (updates, inserts, deletes) = de.changes.pending_count();
        assert_eq!((updates, inserts, deletes), (1, 0, 0));

        // Generate DML and verify UPDATE statement
        let pk_cols = de.pk_columns().to_vec();
        let stmts = generate_dml("users", de.columns(), &pk_cols, de.changes());
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "name" = 'Bob' WHERE "id" = '1'"#
        );
    }

    #[test]
    fn test_edit_then_delete_coalescing() {
        let mut de = make_editor_3col();

        let pk_val = vec![Some("5".to_string())];
        let orig = vec![
            Some("5".to_string()),
            Some("Carol".to_string()),
            Some("carol@example.com".to_string()),
        ];

        // Edit a cell — creates an Update
        de.start_cell_edit(pk_val.clone(), 0, 1, Some("Carol"), false, orig.clone());
        de.confirm_edit(Some("Carrie".to_string()));
        assert_eq!(de.changes.pending_count(), (1, 0, 0));

        // Toggle delete on the same row — Update should be replaced by Delete
        de.toggle_delete_row(&pk_val, &orig);
        assert_eq!(de.changes.pending_count(), (0, 0, 1));

        // DML should produce only 1 DELETE (no UPDATE)
        let pk_cols = de.pk_columns().to_vec();
        let stmts = generate_dml("users", de.columns(), &pk_cols, de.changes());
        assert_eq!(stmts.len(), 1);
        assert!(
            stmts[0].starts_with("DELETE"),
            "expected DELETE, got: {}",
            stmts[0]
        );
    }

    #[test]
    fn test_insert_then_delete_cancellation() {
        let mut de = make_editor_3col();

        // Add a row — creates Insert + pending_insert
        de.add_row();
        assert_eq!(de.changes.pending_count(), (0, 1, 0));
        assert_eq!(de.pending_inserts().len(), 1);

        // Remove the pending insert (simulates deleting the uncommitted row)
        de.remove_pending_insert(0);

        // ChangeLog should be empty, pending_inserts should be empty
        assert!(
            de.changes.is_empty(),
            "ChangeLog should be empty after cancellation"
        );
        assert!(
            de.pending_inserts().is_empty(),
            "pending_inserts should be empty"
        );
    }

    #[test]
    fn test_add_row_creates_null_filled_insert() {
        let mut de = make_editor_3col();

        de.add_row();

        // pending_inserts should have one row with 3 None values
        assert_eq!(de.pending_inserts().len(), 1);
        let inserted = &de.pending_inserts()[0];
        assert_eq!(inserted.len(), 3);
        assert!(
            inserted.iter().all(Option::is_none),
            "all values should be None"
        );

        // Generate DML — should be INSERT with NULLs
        let pk_cols = de.pk_columns().to_vec();
        let stmts = generate_dml("users", de.columns(), &pk_cols, de.changes());
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"INSERT INTO "users" ("id", "name", "email") VALUES (NULL, NULL, NULL)"#
        );
    }

    #[test]
    fn test_clone_row() {
        let mut de = make_editor_3col();

        let values = vec![
            Some("1".to_string()),
            Some("Alice".to_string()),
            Some("alice@example.com".to_string()),
        ];
        de.clone_row(values.clone());

        // pending_inserts should have the cloned values
        assert_eq!(de.pending_inserts().len(), 1);
        assert_eq!(de.pending_inserts()[0], values);

        // Generate DML — should be INSERT with those values
        let pk_cols = de.pk_columns().to_vec();
        let stmts = generate_dml("users", de.columns(), &pk_cols, de.changes());
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"INSERT INTO "users" ("id", "name", "email") VALUES ('1', 'Alice', 'alice@example.com')"#
        );
    }

    #[test]
    fn test_revert_all_clears_everything() {
        let mut de = make_editor_3col();

        let pk1 = vec![Some("1".to_string())];
        let orig1 = vec![
            Some("1".to_string()),
            Some("Alice".to_string()),
            Some("alice@example.com".to_string()),
        ];
        let pk2 = vec![Some("2".to_string())];
        let orig2 = vec![
            Some("2".to_string()),
            Some("Bob".to_string()),
            Some("bob@example.com".to_string()),
        ];

        // Cell edit
        de.start_cell_edit(pk1, 0, 1, Some("Alice"), false, orig1);
        de.confirm_edit(Some("Alicia".to_string()));

        // Add a row
        de.add_row();

        // Delete a row
        de.toggle_delete_row(&pk2, &orig2);

        assert!(!de.changes.is_empty());
        assert!(!de.pending_inserts().is_empty());

        // Revert everything
        de.revert_all_edits();

        assert!(
            de.changes.is_empty(),
            "ChangeLog should be empty after revert_all"
        );
        assert!(
            de.pending_inserts().is_empty(),
            "pending_inserts should be empty after revert_all"
        );
    }

    #[test]
    fn test_fk_nav_preserves_edits() {
        use std::time::Duration;
        use tursotui_db::QueryKind;

        let mut de = make_editor_3col();

        // Edit a cell
        let pk_val = vec![Some("1".to_string())];
        let orig = vec![
            Some("1".to_string()),
            Some("Alice".to_string()),
            Some("alice@example.com".to_string()),
        ];
        de.start_cell_edit(pk_val, 0, 1, Some("Alice"), false, orig);
        de.confirm_edit(Some("Bob".to_string()));
        assert_eq!(de.changes.pending_count(), (1, 0, 0));

        // Push current state onto the FK nav stack
        let nav_result = QueryResult {
            columns: vec![],
            rows: vec![],
            execution_time: Duration::ZERO,
            truncated: false,
            sql: String::new(),
            rows_affected: 0,
            query_kind: QueryKind::Select,
            source_table: None,
        };
        de.push_fk_state(nav_result, 0, 1, 0);

        // FK stack should have 1 entry with the changes preserved
        assert_eq!(de.fk_depth(), 1);

        // Pop and restore
        let entry = de.pop_fk_state().unwrap();
        assert_eq!(entry.changes.pending_count(), (1, 0, 0));
        de.restore_from_fk_entry(entry);

        // After restore, changes should be back
        assert_eq!(de.changes.pending_count(), (1, 0, 0));
    }

    #[test]
    fn test_multiple_edits_same_row_merge() {
        let mut de = make_editor_3col();

        let pk_val = vec![Some("1".to_string())];
        let orig = vec![
            Some("1".to_string()),
            Some("Alice".to_string()),
            Some("alice@example.com".to_string()),
        ];

        // Edit col 1 (name)
        de.start_cell_edit(pk_val.clone(), 0, 1, Some("Alice"), false, orig.clone());
        de.confirm_edit(Some("Bob".to_string()));

        // Edit col 2 (email) of the same row — should merge into the existing Update
        de.start_cell_edit(pk_val, 0, 2, Some("alice@example.com"), false, orig);
        de.confirm_edit(Some("bob@example.com".to_string()));

        // ChangeLog should have exactly 1 Update with both columns in modified
        let (updates, inserts, deletes) = de.changes.pending_count();
        assert_eq!((updates, inserts, deletes), (1, 0, 0));

        let edits = de.changes.edits();
        assert_eq!(edits.len(), 1);
        if let RowEdit::Update { modified, .. } = &edits[0] {
            assert_eq!(modified.len(), 2, "both columns should be in modified map");
            assert_eq!(modified.get(&1), Some(&Some("Bob".to_string())));
            assert_eq!(modified.get(&2), Some(&Some("bob@example.com".to_string())));
        } else {
            panic!("expected Update");
        }

        // Generate DML — should be a single UPDATE with both SET clauses (ordered by col index)
        let pk_cols = de.pk_columns().to_vec();
        let stmts = generate_dml("users", de.columns(), &pk_cols, de.changes());
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            r#"UPDATE "users" SET "name" = 'Bob', "email" = 'bob@example.com' WHERE "id" = '1'"#
        );
    }

    // -------------------------------------------------------------------------
    // EditRenderState method tests
    // -------------------------------------------------------------------------

    #[test]
    fn edit_render_state_extract_pk() {
        let state = EditRenderState {
            pk_columns: vec![0, 2],
            row_markers: HashMap::new(),
            modified_cells: HashSet::new(),
            pending_inserts: vec![],
            fk_columns: HashSet::new(),
        };
        let row = vec![
            Some("1".to_string()),
            Some("Alice".to_string()),
            Some("admin".to_string()),
        ];
        assert_eq!(
            state.extract_pk(&row),
            vec![Some("1".to_string()), Some("admin".to_string())]
        );
    }

    #[test]
    fn edit_render_state_row_marker_lookup() {
        let mut state = EditRenderState {
            pk_columns: vec![0],
            row_markers: HashMap::new(),
            modified_cells: HashSet::new(),
            pending_inserts: vec![],
            fk_columns: HashSet::new(),
        };
        let row = vec![Some("1".to_string()), Some("Alice".to_string())];
        assert_eq!(state.row_marker_for(&row), None);

        let pk = vec![Some("1".to_string())];
        state.row_markers.insert(pk, RowMarker::Modified);
        assert_eq!(state.row_marker_for(&row), Some(RowMarker::Modified));
    }

    #[test]
    fn edit_render_state_row_marker_for_pk() {
        let mut state = EditRenderState {
            pk_columns: vec![0],
            row_markers: HashMap::new(),
            modified_cells: HashSet::new(),
            pending_inserts: vec![],
            fk_columns: HashSet::new(),
        };
        let pk = vec![Some("1".to_string())];
        assert_eq!(state.row_marker_for_pk(&pk), None);

        state.row_markers.insert(pk.clone(), RowMarker::Deleted);
        assert_eq!(state.row_marker_for_pk(&pk), Some(RowMarker::Deleted));
    }

    #[test]
    fn edit_render_state_cell_modified_check() {
        let mut state = EditRenderState {
            pk_columns: vec![0],
            row_markers: HashMap::new(),
            modified_cells: HashSet::new(),
            pending_inserts: vec![],
            fk_columns: HashSet::new(),
        };
        let row = vec![Some("1".to_string()), Some("Alice".to_string())];
        assert!(!state.is_cell_modified(&row, 1));

        let pk = vec![Some("1".to_string())];
        state.modified_cells.insert((pk, 1));
        assert!(state.is_cell_modified(&row, 1));
    }

    #[test]
    fn edit_render_state_cell_modified_pk() {
        let mut state = EditRenderState {
            pk_columns: vec![0],
            row_markers: HashMap::new(),
            modified_cells: HashSet::new(),
            pending_inserts: vec![],
            fk_columns: HashSet::new(),
        };
        let pk = vec![Some("1".to_string())];
        assert!(!state.is_cell_modified_pk(&pk, 1));

        state.modified_cells.insert((pk.clone(), 1));
        assert!(state.is_cell_modified_pk(&pk, 1));
        assert!(!state.is_cell_modified_pk(&pk, 0));
    }
}
