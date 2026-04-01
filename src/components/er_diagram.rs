use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use tursotui_sql::quoting::quote_identifier;

use crate::app::{Action, BottomTab, Direction, EditorAction, NavAction, TableId, UiAction};
use crate::theme::Theme;
use tursotui_db::{ColumnInfo, SchemaEntry};
use tursotui_sql::parser::parse_foreign_keys;

use super::Component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoomLevel {
    Overview,
    Normal,
    Detail,
}

// ─── Data model ──────────────────────────────────────────────────────────────

/// A column entry inside an ER table node.
pub(crate) struct ERColumn {
    pub(crate) name: String,
    pub(crate) type_name: String,
    pub(crate) is_pk: bool,
    pub(crate) is_fk: bool,
}

/// A table node in the ER diagram.
pub(crate) struct ERTable {
    pub(crate) name: String,
    pub(crate) columns: Vec<ERColumn>,
    /// (row, col) in the logical grid assigned by [`compute_grid_layout`].
    pub(crate) grid_pos: (usize, usize),
    /// Virtual (x, y) pixel-space position assigned by [`compute_positions`].
    pub(crate) pos: (i32, i32),
    /// (width, height) of the rendered box in virtual space.
    pub(crate) size: (i32, i32),
}

/// A directed FK relationship between two tables.
pub(crate) struct Relationship {
    /// Index into the `tables` slice of the owning [`ERDiagram`].
    pub(crate) from_table: usize,
    pub(crate) from_column: String,
    /// Index into the `tables` slice of the owning [`ERDiagram`].
    pub(crate) to_table: usize,
    pub(crate) to_column: String,
    /// When `true`, this edge was part of a cycle and should be drawn dashed.
    pub(crate) is_cycle: bool,
}

/// Root state object for the ER-diagram panel.
pub(crate) struct ERDiagram {
    pub(crate) tables: Vec<ERTable>,
    pub(crate) relationships: Vec<Relationship>,
    /// Pan offset (x, y) in virtual space.
    pub(crate) viewport: (i32, i32),
    pub(crate) zoom: ZoomLevel,
    pub(crate) focused_table: Option<usize>,
    /// Indices of tables whose column list is expanded in the diagram.
    pub(crate) expanded_tables: Vec<usize>,
    pub(crate) is_fullscreen: bool,
    pub(crate) connected_tables: HashSet<usize>,
    /// True when the layout needs to be recalculated.
    pub(crate) layout_dirty: bool,
    /// Last render area dimensions (width, height) — used by center-viewport logic.
    last_area: (u16, u16),
    /// False until the first successful build from `schema_cache`.
    pub(crate) loaded: bool,
    /// Grid layout metadata — x-origin of each grid column (set by `recalculate_layout`).
    col_origins: Vec<i32>,
    /// Max table width per grid column (set by `recalculate_layout`).
    col_widths: Vec<i32>,
}

impl ERDiagram {
    pub(crate) fn new() -> Self {
        Self {
            tables: Vec::new(),
            relationships: Vec::new(),
            viewport: (0, 0),
            zoom: ZoomLevel::Normal,
            focused_table: None,
            expanded_tables: Vec::new(),
            is_fullscreen: false,
            connected_tables: HashSet::new(),
            layout_dirty: true,
            last_area: (0, 0),
            loaded: false,
            col_origins: Vec::new(),
            col_widths: Vec::new(),
        }
    }

    /// Build (or rebuild) the ER diagram from schema metadata.
    ///
    /// `entries` are the `sqlite_schema` rows; only tables are kept.
    /// `columns` maps table name → column list (from PRAGMA `table_info`).
    ///
    /// FK relationships are parsed from each table's `CREATE TABLE` SQL via
    /// [`parse_foreign_keys`] (from `tursotui-sql` crate — no db query).
    pub(crate) fn build_from_schema(
        &mut self,
        entries: &[SchemaEntry],
        columns: &HashMap<TableId, Vec<ColumnInfo>>,
    ) {
        self.tables.clear();
        self.relationships.clear();
        self.expanded_tables.clear();

        // Build a name → index map so FK targets can be resolved to table indices.
        // Uses TableId keys so FK target names with different casing still resolve.
        let mut name_to_idx: HashMap<TableId, usize> = HashMap::new();

        for entry in entries {
            if entry.obj_type != "table" {
                continue;
            }

            let cols: Vec<ERColumn> = columns
                .get(&TableId::new(&entry.name))
                .map(|infos| {
                    infos
                        .iter()
                        .map(|ci| ERColumn {
                            name: ci.name.clone(),
                            type_name: ci.col_type.clone(),
                            is_pk: ci.pk,
                            is_fk: false, // filled in below once FKs are parsed
                        })
                        .collect()
                })
                .unwrap_or_default();

            let idx = self.tables.len();
            name_to_idx.insert(TableId::new(&entry.name), idx);

            self.tables.push(ERTable {
                name: entry.name.clone(),
                columns: cols,
                grid_pos: (0, 0),
                pos: (0, 0),
                size: (0, 0),
            });
        }

        // Parse FK relationships from CREATE TABLE SQL.
        for entry in entries {
            if entry.obj_type != "table" {
                continue;
            }
            let Some(sql) = entry.sql.as_deref() else {
                continue;
            };
            let Some(&from_idx) = name_to_idx.get(&TableId::new(&entry.name)) else {
                continue;
            };

            let fks = parse_foreign_keys(sql);
            for fk in fks {
                let Some(&to_idx) = name_to_idx.get(&TableId::new(&fk.to_table)) else {
                    continue; // FK target table not in schema (e.g., dropped)
                };

                // Mark the source column as FK.
                if let Some(col) = self.tables[from_idx]
                    .columns
                    .iter_mut()
                    .find(|c| c.name == fk.from_column)
                {
                    col.is_fk = true;
                }

                self.relationships.push(Relationship {
                    from_table: from_idx,
                    from_column: fk.from_column,
                    to_table: to_idx,
                    to_column: fk.to_column,
                    is_cycle: false,
                });
            }
        }

        // Run layout pipeline: cycle detection + grid assignment.
        // Position computation is deferred to the first render via layout_dirty.
        detect_and_break_cycles(&mut self.relationships);
        compute_grid_layout(&mut self.tables, &self.relationships);

        self.loaded = true;
        self.layout_dirty = true;
        // Reset focus to first table if any.
        self.focused_table = if self.tables.is_empty() {
            None
        } else {
            Some(0)
        };
        self.rebuild_connected_tables();
    }

    /// Rebuild the set of tables directly connected to the focused table via FK.
    fn rebuild_connected_tables(&mut self) {
        self.connected_tables.clear();
        let Some(focused) = self.focused_table else {
            return;
        };
        for rel in &self.relationships {
            if rel.from_table == focused {
                self.connected_tables.insert(rel.to_table);
            }
            if rel.to_table == focused {
                self.connected_tables.insert(rel.from_table);
            }
        }
    }
}

impl Default for ERDiagram {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Rendering helpers ───────────────────────────────────────────────────────

/// Pan step in virtual units per keypress.
const PAN_STEP: i32 = 3;

/// Convert virtual coordinates to screen coordinates, returning `None` if off-screen.
fn virtual_to_screen(viewport: (i32, i32), vx: i32, vy: i32, area: Rect) -> Option<(u16, u16)> {
    let sx = vx - viewport.0;
    let sy = vy - viewport.1;
    if sx < 0 || sy < 0 {
        return None;
    }
    let sx = sx as u16;
    let sy = sy as u16;
    if sx >= area.width || sy >= area.height {
        return None;
    }
    Some((area.x + sx, area.y + sy))
}

/// Write a string to the frame buffer at virtual coordinates, clipping to the area.
fn put_str(
    viewport: (i32, i32),
    buf: &mut Buffer,
    area: Rect,
    vx: i32,
    vy: i32,
    s: &str,
    style: Style,
) {
    if let Some((sx, sy)) = virtual_to_screen(viewport, vx, vy, area) {
        let remaining = (area.x + area.width).saturating_sub(sx) as usize;
        let mut drawn = 0;
        for ch in s.chars() {
            let cw = ch.width().unwrap_or(0);
            if drawn + cw > remaining {
                break;
            }
            let x = sx + drawn as u16;
            if x < area.x + area.width {
                buf[(x, sy)].set_char(ch).set_style(style);
            }
            drawn += cw;
        }
    }
}

/// Write a single character to the frame buffer at virtual coordinates.
fn put_char(
    viewport: (i32, i32),
    buf: &mut Buffer,
    area: Rect,
    vx: i32,
    vy: i32,
    ch: char,
    style: Style,
) {
    if let Some((sx, sy)) = virtual_to_screen(viewport, vx, vy, area) {
        buf[(sx, sy)].set_char(ch).set_style(style);
    }
}

impl ERDiagram {
    /// Render a centered placeholder message.
    fn render_centered(frame: &mut Frame, inner: Rect, msg: &str, theme: &Theme) {
        let msg_width = msg.width() as u16;
        let x = inner.x + inner.width.saturating_sub(msg_width) / 2;
        let y = inner.y + inner.height / 2;
        let msg_area = Rect::new(x, y, msg_width.min(inner.width), 1);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(msg).style(Style::default().fg(theme.border)),
            msg_area,
        );
    }

    /// Check whether a table's bounding box intersects the viewport.
    fn table_visible(&self, table: &ERTable, area: Rect) -> bool {
        let (vx, vy) = self.viewport;
        let vw = i32::from(area.width);
        let vh = i32::from(area.height);
        table.pos.0 + table.size.0 > vx
            && table.pos.0 < vx + vw
            && table.pos.1 + table.size.1 > vy
            && table.pos.1 < vy + vh
    }

    /// Render a single table box to the frame buffer.
    #[allow(clippy::too_many_lines)]
    fn render_table(&self, buf: &mut Buffer, area: Rect, table_idx: usize, theme: &Theme) {
        let table = &self.tables[table_idx];
        let is_focused_table = self.focused_table == Some(table_idx);
        let is_connected =
            self.focused_table.is_some() && self.connected_tables.contains(&table_idx);
        let border_style = if is_focused_table {
            Style::default().fg(theme.border_focused)
        } else if is_connected {
            theme.er_connected_border
        } else {
            theme.er_table_border
        };
        let vp = self.viewport;
        let (tx, ty) = table.pos;
        let (tw, th) = table.size;

        // Top border: ╭──...──╮
        put_char(vp, buf, area, tx, ty, '╭', border_style);
        for dx in 1..tw - 1 {
            put_char(vp, buf, area, tx + dx, ty, '─', border_style);
        }
        put_char(vp, buf, area, tx + tw - 1, ty, '╮', border_style);

        // Name row at ty + 1 (body row between top border and separator)
        let name = &table.name;
        let name_style = if is_focused_table {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        };
        let name_y = ty + 1;
        put_char(vp, buf, area, tx, name_y, '│', border_style);
        put_char(vp, buf, area, tx + tw - 1, name_y, '│', border_style);
        put_str(vp, buf, area, tx + 2, name_y, name, name_style);

        // At Overview zoom, skip separator and columns entirely (box is just 3 rows).
        let mut row_y = if self.zoom == ZoomLevel::Overview {
            name_y + 1
        } else {
            // Separator: ├──...──┤
            let sep_y = ty + 2;
            put_char(vp, buf, area, tx, sep_y, '├', border_style);
            for dx in 1..tw - 1 {
                put_char(vp, buf, area, tx + dx, sep_y, '─', border_style);
            }
            put_char(vp, buf, area, tx + tw - 1, sep_y, '┤', border_style);

            let mut row_y = sep_y + 1;

            let is_expanded = self.expanded_tables.contains(&table_idx);
            let indices_to_show: Vec<usize> = match self.zoom {
                ZoomLevel::Detail => (0..table.columns.len()).collect(),
                ZoomLevel::Normal => {
                    if is_expanded {
                        (0..table.columns.len()).collect()
                    } else {
                        table
                            .columns
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.is_pk)
                            .map(|(i, _)| i)
                            .collect()
                    }
                }
                ZoomLevel::Overview => Vec::new(),
            };

            for &col_idx in &indices_to_show {
                let col = &table.columns[col_idx];
                put_char(vp, buf, area, tx, row_y, '│', border_style);
                put_char(vp, buf, area, tx + tw - 1, row_y, '│', border_style);

                let (marker, marker_style) = if col.is_pk && col.is_fk {
                    ("🔑", theme.er_fk_style)
                } else if col.is_pk {
                    ("🔑", theme.er_pk_style)
                } else if col.is_fk {
                    ("🔗", theme.er_fk_style)
                } else {
                    ("  ", Style::default().fg(theme.fg))
                };
                put_str(vp, buf, area, tx + 1, row_y, marker, marker_style);

                let col_text = format!("{}: {}", col.name, col.type_name);
                let col_style = if col.is_pk && col.is_fk {
                    theme.er_fk_style
                } else if col.is_pk {
                    theme.er_pk_style
                } else if col.is_fk {
                    theme.er_fk_style
                } else {
                    Style::default().fg(theme.fg)
                };
                put_str(vp, buf, area, tx + 3, row_y, &col_text, col_style);
                row_y += 1;
            }

            // (+N) indicator only shows at Normal zoom when not expanded
            if self.zoom == ZoomLevel::Normal && !is_expanded {
                let pk_count = table.columns.iter().filter(|c| c.is_pk).count();
                let hidden = table.columns.len().saturating_sub(pk_count);
                if hidden > 0 {
                    put_char(vp, buf, area, tx, row_y, '│', border_style);
                    put_char(vp, buf, area, tx + tw - 1, row_y, '│', border_style);
                    let indicator = format!("(+{hidden})");
                    let indicator_style = Style::default().fg(theme.dim);
                    put_str(vp, buf, area, tx + 3, row_y, &indicator, indicator_style);
                    row_y += 1;
                }
            }

            row_y
        };

        while row_y < ty + th - 1 {
            put_char(vp, buf, area, tx, row_y, '│', border_style);
            put_char(vp, buf, area, tx + tw - 1, row_y, '│', border_style);
            row_y += 1;
        }

        let bot_y = ty + th - 1;
        put_char(vp, buf, area, tx, bot_y, '╰', border_style);
        for dx in 1..tw - 1 {
            put_char(vp, buf, area, tx + dx, bot_y, '─', border_style);
        }
        put_char(vp, buf, area, tx + tw - 1, bot_y, '╯', border_style);
    }

    /// Number of visible rows for a table in the current display mode.
    fn display_row_count(&self, table_idx: usize) -> i32 {
        let table = &self.tables[table_idx];
        match self.zoom {
            ZoomLevel::Overview => 0,
            ZoomLevel::Detail => table.columns.len() as i32,
            ZoomLevel::Normal => {
                if self.expanded_tables.contains(&table_idx) {
                    table.columns.len() as i32
                } else {
                    let pk_count = table.columns.iter().filter(|c| c.is_pk).count() as i32;
                    let hidden = table.columns.len() as i32 - pk_count;
                    pk_count + i32::from(hidden > 0)
                }
            }
        }
    }

    /// Recalculate box sizes and positions when layout is dirty.
    fn recalculate_layout(&mut self) {
        if !self.layout_dirty || self.tables.is_empty() {
            return;
        }
        // Compute display-mode-aware sizes before positioning.
        for idx in 0..self.tables.len() {
            let table = &self.tables[idx];
            let name_width = table.name.width() as i32;
            let max_col_width = table
                .columns
                .iter()
                .map(|c| (c.name.width() + 2 + c.type_name.width()) as i32)
                .max()
                .unwrap_or(0);
            let content_width = name_width.max(max_col_width);
            let box_width = content_width + 5; // borders + padding + marker column

            let visible_rows = self.display_row_count(idx);
            let box_height = if self.zoom == ZoomLevel::Overview {
                3 // border + name + border (no separator)
            } else {
                visible_rows + 4
            };

            let min_h = if self.zoom == ZoomLevel::Overview {
                3
            } else {
                4
            };
            self.tables[idx].size = (box_width.max(6), box_height.max(min_h));
        }
        // Assign positions using pre-computed sizes and grid_pos.
        let (gap_x, gap_y): (i32, i32) = match self.zoom {
            ZoomLevel::Overview => (4, 2),
            ZoomLevel::Normal => (14, 4),
            ZoomLevel::Detail => (12, 6),
        };

        let max_col = self.tables.iter().map(|t| t.grid_pos.1).max().unwrap_or(0);
        let max_row = self.tables.iter().map(|t| t.grid_pos.0).max().unwrap_or(0);
        let num_cols = max_col + 1;
        let num_rows = max_row + 1;

        let mut col_widths = vec![0i32; num_cols];
        for table in &self.tables {
            let c = table.grid_pos.1;
            col_widths[c] = col_widths[c].max(table.size.0);
        }
        let mut row_heights = vec![0i32; num_rows];
        for table in &self.tables {
            let r = table.grid_pos.0;
            row_heights[r] = row_heights[r].max(table.size.1);
        }
        let mut col_origins = vec![0i32; num_cols];
        for c in 1..num_cols {
            col_origins[c] = col_origins[c - 1] + col_widths[c - 1] + gap_x;
        }
        let mut row_origins = vec![0i32; num_rows];
        for r in 1..num_rows {
            row_origins[r] = row_origins[r - 1] + row_heights[r - 1] + gap_y;
        }
        for table in &mut self.tables {
            let (r, c) = table.grid_pos;
            table.pos = (col_origins[c], row_origins[r]);
        }

        self.col_origins = col_origins;
        self.col_widths = col_widths;
        self.layout_dirty = false;
    }

    /// Return the Y coordinate for an edge endpoint on a given table.
    ///
    /// If the FK/PK column is visible (expanded in Normal zoom, or Detail zoom),
    /// returns that column's row Y. Otherwise returns the header row Y.
    fn column_anchor_y(&self, table_idx: usize, col_name: &str, is_pk: bool) -> i32 {
        let table = &self.tables[table_idx];
        let header_y = table.pos.1 + 1;
        if self.zoom == ZoomLevel::Overview {
            return header_y;
        }
        // Overview returns early above, so only Normal and Detail remain.
        let columns_visible = match self.zoom {
            ZoomLevel::Detail => true,
            ZoomLevel::Normal => self.expanded_tables.contains(&table_idx),
            ZoomLevel::Overview => unreachable!(),
        };
        if !columns_visible && !is_pk {
            return header_y;
        }
        let sep_y = table.pos.1 + 2;
        if columns_visible {
            if let Some(idx) = table.columns.iter().position(|c| c.name == col_name) {
                return sep_y + 1 + idx as i32;
            }
        } else {
            // Normal zoom, collapsed — only PK rows visible
            let pk_idx = table
                .columns
                .iter()
                .filter(|c| c.is_pk)
                .position(|c| c.name == col_name);
            if let Some(idx) = pk_idx {
                return sep_y + 1 + idx as i32;
            }
        }
        header_y
    }

    /// Find the x-coordinate for a vertical edge segment between two grid columns.
    ///
    /// Picks the inter-column gap whose center is closest to `ideal_x`.
    /// Falls back to `ideal_x` when layout metadata is unavailable.
    fn find_gap_x(&self, col_a: usize, col_b: usize, ideal_x: i32) -> i32 {
        if self.col_origins.is_empty() {
            return ideal_x;
        }
        let (lo, hi) = if col_a < col_b {
            (col_a, col_b)
        } else {
            (col_b, col_a)
        };
        let mut best_x = ideal_x;
        let mut best_dist = i32::MAX;
        for c in lo..hi {
            if c + 1 < self.col_origins.len() {
                let right_edge = self.col_origins[c] + self.col_widths[c];
                let left_edge = self.col_origins[c + 1];
                let center = i32::midpoint(right_edge, left_edge);
                let dist = (center - ideal_x).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best_x = center;
                }
            }
        }
        best_x
    }

    /// Find the x-coordinate for routing same-column edges (right-side bypass).
    ///
    /// Uses the inter-column gap to the right of `col`, or adds padding past the
    /// rightmost table edge if this is the last column.
    fn same_col_gap_x(&self, col: usize) -> i32 {
        let (center, _) = self.gap_bounds_for_col(col);
        center
    }

    /// Return `(center, usable_half_width)` for the gap to the right of `col`.
    ///
    /// `usable_half_width` is the maximum distance an edge can be offset from
    /// the center without overlapping a table border (includes 1-cell padding).
    fn gap_bounds_for_col(&self, col: usize) -> (i32, i32) {
        if !self.col_origins.is_empty() && col + 1 < self.col_origins.len() {
            let right_edge = self.col_origins[col] + self.col_widths[col];
            let left_edge = self.col_origins[col + 1];
            let center = i32::midpoint(right_edge, left_edge);
            // 1-cell clearance from each table border
            let half = ((left_edge - right_edge) / 2).max(1) - 1;
            (center, half)
        } else {
            // Last column — generous space to the right.
            let right_edge = self
                .tables
                .iter()
                .filter(|t| t.grid_pos.1 == col)
                .map(|t| t.pos.0 + t.size.0)
                .max()
                .unwrap_or(0);
            let center = right_edge + 6;
            (center, 5)
        }
    }

    /// Pre-compute the gap x-coordinate for each relationship's vertical segment,
    /// spreading edges that share a corridor so they don't overlap.
    ///
    /// Returns a Vec parallel to `self.relationships`; `None` for edges that have
    /// no vertical segment (straight horizontal or self-referential).
    fn compute_edge_gap_assignments(&self) -> Vec<Option<i32>> {
        let n = self.relationships.len();
        let mut base_gap: Vec<Option<i32>> = vec![None; n];

        // First pass: compute the base (un-separated) gap_x for each edge.
        for (i, rel) in self.relationships.iter().enumerate() {
            if rel.from_table >= self.tables.len() || rel.to_table >= self.tables.len() {
                continue;
            }
            if rel.from_table == rel.to_table {
                continue;
            }
            let from = &self.tables[rel.from_table];
            let to = &self.tables[rel.to_table];

            if from.grid_pos.1 == to.grid_pos.1 {
                base_gap[i] = Some(self.same_col_gap_x(from.grid_pos.1));
            } else {
                let from_is_pk = from
                    .columns
                    .iter()
                    .any(|c| c.name == rel.from_column && c.is_pk);
                let from_y = self.column_anchor_y(rel.from_table, &rel.from_column, from_is_pk);
                let to_y = self.column_anchor_y(rel.to_table, &rel.to_column, true);

                if from_y != to_y {
                    let from_cx = from.pos.0 + from.size.0 / 2;
                    let to_cx = to.pos.0 + to.size.0 / 2;
                    let target_right = to_cx >= from_cx;
                    let (fx, tx) = if target_right {
                        (from.pos.0 + from.size.0, to.pos.0)
                    } else {
                        (from.pos.0, to.pos.0 + to.size.0)
                    };
                    let ideal = i32::midpoint(fx, tx);
                    base_gap[i] = Some(self.find_gap_x(from.grid_pos.1, to.grid_pos.1, ideal));
                }
            }
        }

        // Second pass: group edges that share the same base gap_x, sort by
        // average Y to minimise visual crossings, then spread with 2-cell gaps.
        let mut groups: HashMap<i32, Vec<usize>> = HashMap::new();
        for (i, gx) in base_gap.iter().enumerate() {
            if let Some(x) = gx {
                groups.entry(*x).or_default().push(i);
            }
        }

        // Pre-compute gap bounds for each grid column so the spread can be clamped.
        let num_cols = self.col_origins.len();
        let gap_bounds: Vec<(i32, i32)> = (0..num_cols.max(1))
            .map(|c| self.gap_bounds_for_col(c))
            .collect();

        let mut result: Vec<Option<i32>> = vec![None; n];
        for (center, group) in &mut groups {
            // Sort by average Y of endpoints so adjacent vertical segments
            // don't cross each other's horizontal stubs.
            group.sort_by_key(|&ri| {
                let rel = &self.relationships[ri];
                let from = &self.tables[rel.from_table];
                let to = &self.tables[rel.to_table];
                let fy = from.pos.1 + from.size.1 / 2;
                let ty = to.pos.1 + to.size.1 / 2;
                fy + ty // proportional to average Y
            });

            // Find the usable half-width for the gap containing this center.
            let half = gap_bounds
                .iter()
                .find(|(c, _)| (*c - *center).abs() <= 1)
                .map_or(6, |&(_, h)| h);

            let count = group.len() as i32;
            // Reduce spacing when many edges share a corridor so they fit.
            let spacing = if count <= 1 {
                0
            } else {
                let max_span = half * 2;
                (max_span / (count - 1)).clamp(1, 2)
            };
            let span = (count - 1) * spacing;
            let start = *center - span / 2;
            for (slot, &rel_idx) in group.iter().enumerate() {
                result[rel_idx] = Some(start + slot as i32 * spacing);
            }
        }
        result
    }

    /// Render all FK edges (behind table boxes).
    ///
    /// Unified approach: every edge exits the right side of the source table
    /// and enters the left side of the target table.  When source is to the
    /// right of target the roles swap.  An L-shaped route is used when the
    /// two endpoints have different Y values; the turn point sits at the
    /// horizontal midpoint between the two tables.
    /// Draw edge line segments and return deferred endpoint markers.
    ///
    /// Markers must be drawn after tables so arrows aren't overwritten by
    /// table border characters.
    #[allow(clippy::too_many_lines)]
    fn render_edges(
        &self,
        buf: &mut Buffer,
        area: Rect,
        theme: &Theme,
    ) -> Vec<(i32, i32, char, Style)> {
        let edge_gap_x = self.compute_edge_gap_assignments();
        let mut markers: Vec<(i32, i32, char, Style)> = Vec::new();

        for (rel_idx, rel) in self.relationships.iter().enumerate() {
            if rel.from_table >= self.tables.len() || rel.to_table >= self.tables.len() {
                continue;
            }
            let from_vis = self.table_visible(&self.tables[rel.from_table], area);
            let to_vis = self.table_visible(&self.tables[rel.to_table], area);
            if !from_vis && !to_vis {
                continue;
            }

            // ── styling ──────────────────────────────────────────────
            let involves_focused = self
                .focused_table
                .is_some_and(|f| rel.from_table == f || rel.to_table == f);
            let edge_style = if involves_focused {
                theme.er_connected_border
            } else if rel.is_cycle {
                theme.er_relationship.add_modifier(Modifier::DIM)
            } else {
                theme.er_relationship
            };
            let h_char = if rel.is_cycle { '╌' } else { '─' };
            let v_char = if rel.is_cycle { '╎' } else { '│' };

            // ── self-referential FK ──────────────────────────────────
            if rel.from_table == rel.to_table {
                let t = &self.tables[rel.from_table];
                let rx = t.pos.0 + t.size.0;
                let ry = t.pos.1 + 1;
                put_char(self.viewport, buf, area, rx, ry, h_char, edge_style);
                put_char(self.viewport, buf, area, rx + 1, ry, '╮', edge_style);
                put_char(self.viewport, buf, area, rx + 1, ry + 1, '╰', edge_style);
                put_str(
                    self.viewport,
                    buf,
                    area,
                    rx + 2,
                    ry + 1,
                    "[self]",
                    edge_style,
                );
                continue;
            }

            let from = &self.tables[rel.from_table];
            let to = &self.tables[rel.to_table];

            // ── anchor Y ─────────────────────────────────────────────
            let from_is_pk = from
                .columns
                .iter()
                .any(|c| c.name == rel.from_column && c.is_pk);
            let from_y = self.column_anchor_y(rel.from_table, &rel.from_column, from_is_pk);
            let to_y = self.column_anchor_y(rel.to_table, &rel.to_column, true);

            // ── determine exit/entry sides ───────────────────────────
            // Compare table centers to decide direction.
            let from_center_x = from.pos.0 + from.size.0 / 2;
            let to_center_x = to.pos.0 + to.size.0 / 2;
            let target_right = to_center_x >= from_center_x;

            let (from_x, to_x) = if target_right {
                (from.pos.0 + from.size.0, to.pos.0) // exit right, enter left
            } else {
                (from.pos.0, to.pos.0 + to.size.0) // exit left, enter right
            };

            // ── routing ───────────────────────────────────────────────
            //
            // Same-column tables (shared grid_pos.1): both exits use the right
            // side, routing around the column into the gap on the right.
            //
            // Cross-column edges:
            //   Straight: from_y == to_y → single horizontal line.
            //   Z-shape: horizontal stub → vertical in the inter-column gap
            //            (midpoint of from_x and to_x) → horizontal stub.

            if from.grid_pos.1 == to.grid_pos.1 {
                // Both tables in the same grid column — route via the right-side
                // inter-column gap so the vertical segment never crosses a table.
                let from_exit_x = from.pos.0 + from.size.0;
                let to_exit_x = to.pos.0 + to.size.0;
                // Ensure the vertical segment is always past both tables' right edges.
                let min_corner = from_exit_x.max(to_exit_x) + 1;
                let corner_x = edge_gap_x[rel_idx]
                    .unwrap_or_else(|| self.same_col_gap_x(from.grid_pos.1))
                    .max(min_corner);
                // Horizontal stubs from each exit to the corner
                for x in from_exit_x..=corner_x {
                    put_char(self.viewport, buf, area, x, from_y, h_char, edge_style);
                }
                for x in to_exit_x..=corner_x {
                    put_char(self.viewport, buf, area, x, to_y, h_char, edge_style);
                }
                // Vertical segment in the column gap
                for y in from_y.min(to_y)..=from_y.max(to_y) {
                    put_char(self.viewport, buf, area, corner_x, y, v_char, edge_style);
                }
                // Corners — only when there is a horizontal stub before the turn
                if from_exit_x < corner_x {
                    let c = if from_y < to_y { '╮' } else { '╯' };
                    put_char(self.viewport, buf, area, corner_x, from_y, c, edge_style);
                }
                if to_exit_x < corner_x {
                    let c = if from_y < to_y { '╯' } else { '╮' };
                    put_char(self.viewport, buf, area, corner_x, to_y, c, edge_style);
                }
                // Defer endpoint markers to the final pass.
                markers.push((from_exit_x, from_y, '┤', edge_style));
                markers.push((to_exit_x, to_y, '◄', edge_style));
                continue;
            } else if from_y == to_y {
                // Straight horizontal
                for x in from_x.min(to_x)..=from_x.max(to_x) {
                    put_char(self.viewport, buf, area, x, from_y, h_char, edge_style);
                }
            } else {
                // Z-shape: horizontal stub → vertical in gap → horizontal stub.
                // Use the pre-computed gap_x (with separation offset applied).
                let gap_x = edge_gap_x[rel_idx].unwrap_or_else(|| {
                    let ideal = i32::midpoint(from_x, to_x);
                    self.find_gap_x(from.grid_pos.1, to.grid_pos.1, ideal)
                });

                // Segment 1: horizontal from source border to gap
                for x in from_x.min(gap_x)..=from_x.max(gap_x) {
                    put_char(self.viewport, buf, area, x, from_y, h_char, edge_style);
                }
                // Segment 2: vertical from from_y to to_y in the gap
                for y in from_y.min(to_y)..=from_y.max(to_y) {
                    put_char(self.viewport, buf, area, gap_x, y, v_char, edge_style);
                }
                // Segment 3: horizontal from gap to target border
                for x in gap_x.min(to_x)..=gap_x.max(to_x) {
                    put_char(self.viewport, buf, area, x, to_y, h_char, edge_style);
                }
                // Corner 1: source side (gap_x, from_y)
                let c1 = if from_x <= gap_x {
                    if from_y < to_y { '╮' } else { '╯' }
                } else if from_y < to_y {
                    '╭'
                } else {
                    '╰'
                };
                put_char(self.viewport, buf, area, gap_x, from_y, c1, edge_style);
                // Corner 2: target side (gap_x, to_y)
                let c2 = if gap_x <= to_x {
                    if from_y < to_y { '╰' } else { '╭' }
                } else if from_y < to_y {
                    '╯'
                } else {
                    '╮'
                };
                put_char(self.viewport, buf, area, gap_x, to_y, c2, edge_style);
            }

            // ── endpoint markers (deferred) ──────────────────────────
            let src_char = if target_right { '┤' } else { '├' };
            markers.push((from_x, from_y, src_char, edge_style));
            let tgt_char = if target_right { '►' } else { '◄' };
            markers.push((to_x, to_y, tgt_char, edge_style));

            if rel.is_cycle {
                let mid_x = i32::midpoint(from_x, to_x);
                let mid_y = i32::midpoint(from_y, to_y);
                put_str(self.viewport, buf, area, mid_x, mid_y, "[cyc]", edge_style);
            }
        }

        markers
    }
}

impl Component for ERDiagram {
    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            // Pan viewport
            (KeyModifiers::NONE, KeyCode::Char('h') | KeyCode::Left) => {
                self.viewport.0 -= PAN_STEP;
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('l') | KeyCode::Right) => {
                self.viewport.0 += PAN_STEP;
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.viewport.1 -= PAN_STEP;
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                self.viewport.1 += PAN_STEP;
                None
            }

            // Toggle expand focused table (Normal zoom only)
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.zoom == ZoomLevel::Normal
                    && let Some(idx) = self.focused_table
                {
                    if let Some(pos) = self.expanded_tables.iter().position(|&i| i == idx) {
                        self.expanded_tables.remove(pos);
                    } else {
                        self.expanded_tables.push(idx);
                    }
                    self.layout_dirty = true;
                }
                None
            }

            // Zoom in
            (KeyModifiers::NONE, KeyCode::Char('+' | '=')) => {
                match self.zoom {
                    ZoomLevel::Overview => {
                        self.zoom = ZoomLevel::Normal;
                        self.layout_dirty = true;
                    }
                    ZoomLevel::Normal => {
                        self.zoom = ZoomLevel::Detail;
                        self.layout_dirty = true;
                    }
                    ZoomLevel::Detail => {}
                }
                None
            }
            // Zoom out
            (KeyModifiers::NONE, KeyCode::Char('-')) => {
                match self.zoom {
                    ZoomLevel::Detail => {
                        self.zoom = ZoomLevel::Normal;
                        self.layout_dirty = true;
                    }
                    ZoomLevel::Normal => {
                        self.zoom = ZoomLevel::Overview;
                        self.layout_dirty = true;
                    }
                    ZoomLevel::Overview => {}
                }
                None
            }

            // Cycle focus between tables — consumed by component, does NOT emit CycleFocus
            (KeyModifiers::NONE, KeyCode::Tab) => {
                if !self.tables.is_empty() {
                    let next = match self.focused_table {
                        Some(i) => (i + 1) % self.tables.len(),
                        None => 0,
                    };
                    self.focused_table = Some(next);
                    self.rebuild_connected_tables();
                }
                // Return Some to consume Tab — prevents global CycleFocus
                Some(Action::Nav(NavAction::SwitchBottomTab(
                    BottomTab::ERDiagram,
                )))
            }

            // Reverse cycle focus between tables
            (KeyModifiers::SHIFT, KeyCode::BackTab) => {
                if !self.tables.is_empty() {
                    let prev = match self.focused_table {
                        Some(0) | None => self.tables.len() - 1,
                        Some(i) => i - 1,
                    };
                    self.focused_table = Some(prev);
                    self.rebuild_connected_tables();
                }
                Some(Action::Nav(NavAction::SwitchBottomTab(
                    BottomTab::ERDiagram,
                )))
            }

            // Open focused table in query editor
            (KeyModifiers::NONE, KeyCode::Char('o')) => {
                if let Some(idx) = self.focused_table {
                    let table_name = &self.tables[idx].name;
                    let sql = format!("SELECT * FROM {} LIMIT 100;", quote_identifier(table_name));
                    return Some(Action::Editor(EditorAction::PopulateEditor(sql)));
                }
                None
            }

            // Center viewport: on focused table, or fit entire diagram
            (KeyModifiers::NONE, KeyCode::Char('c')) => {
                if self.tables.is_empty() || self.last_area == (0, 0) {
                    return None;
                }
                let (aw, ah) = (i32::from(self.last_area.0), i32::from(self.last_area.1));
                if let Some(idx) = self.focused_table {
                    // Center on focused table
                    let t = &self.tables[idx];
                    self.viewport.0 = t.pos.0 + t.size.0 / 2 - aw / 2;
                    self.viewport.1 = t.pos.1 + t.size.1 / 2 - ah / 2;
                } else {
                    // Fit entire diagram: compute bounding box of all tables
                    let min_x = self.tables.iter().map(|t| t.pos.0).min().unwrap_or(0);
                    let min_y = self.tables.iter().map(|t| t.pos.1).min().unwrap_or(0);
                    let max_x = self
                        .tables
                        .iter()
                        .map(|t| t.pos.0 + t.size.0)
                        .max()
                        .unwrap_or(0);
                    let max_y = self
                        .tables
                        .iter()
                        .map(|t| t.pos.1 + t.size.1)
                        .max()
                        .unwrap_or(0);
                    let diagram_w = max_x - min_x;
                    let diagram_h = max_y - min_y;
                    self.viewport.0 = min_x + diagram_w / 2 - aw / 2;
                    self.viewport.1 = min_y + diagram_h / 2 - ah / 2;
                }
                None
            }

            // Toggle fullscreen overlay
            (KeyModifiers::NONE, KeyCode::Char('f')) => Some(Action::Ui(UiAction::ShowERDiagram)),

            // Esc releases focus
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.focused_table = None;
                self.connected_tables.clear();
                Some(Action::Nav(NavAction::CycleFocus(Direction::Forward)))
            }

            _ => None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let inner = if self.is_fullscreen {
            area
        } else {
            let block = super::panel_block("ER Diagram", focused, theme);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            inner
        };

        if inner.height == 0 || inner.width == 0 {
            return;
        }
        self.last_area = (inner.width, inner.height);

        // Check empty states
        if !self.loaded {
            Self::render_centered(frame, inner, "Loading schema...", theme);
            return;
        }
        if self.tables.is_empty() {
            Self::render_centered(frame, inner, "No tables in database", theme);
            return;
        }

        // Recalculate layout if dirty (e.g., after zoom change or expand toggle)
        self.recalculate_layout();

        let buf = frame.buffer_mut();

        // Render edges first (behind tables), then tables, then arrow markers
        // on top of everything so table borders don't overwrite them.
        let markers = self.render_edges(buf, inner, theme);
        for idx in 0..self.tables.len() {
            if self.table_visible(&self.tables[idx], inner) {
                self.render_table(buf, inner, idx, theme);
            }
        }
        for (mx, my, ch, style) in markers {
            put_char(self.viewport, buf, inner, mx, my, ch, style);
        }
    }
}

// ─── Layout helpers ───────────────────────────────────────────────────────────

/// Assign `grid_pos` to every table in `tables`.
///
/// Strategy
/// --------
/// 1. Determine the number of grid columns: `ceil(sqrt(N))`.
/// 2. Identify tables that participate in FK relationships and group connected
///    components together so that related tables end up adjacent.
/// 3. Orphan tables (no FK edges) go last, sorted alphabetically.
/// 4. Within each group tables are also sorted alphabetically for a deterministic
///    layout across reloads.
/// 5. Assign positions left-to-right, top-to-bottom.
pub(crate) fn compute_grid_layout(tables: &mut [ERTable], relationships: &[Relationship]) {
    let n = tables.len();
    if n == 0 {
        return;
    }

    // Compute ceil(sqrt(n)) without floating-point precision issues.
    let num_cols = integer_sqrt_ceil(n);

    // Build an adjacency list (undirected) so we can find connected components.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for rel in relationships {
        if rel.from_table < n && rel.to_table < n {
            adj[rel.from_table].push(rel.to_table);
            adj[rel.to_table].push(rel.from_table);
        }
    }

    // Find connected components via BFS, collecting table indices.
    let mut visited = vec![false; n];
    let mut components: Vec<Vec<usize>> = Vec::new();

    for start in 0..n {
        if visited[start] {
            continue;
        }
        let mut component = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(start);
        visited[start] = true;
        while let Some(node) = queue.pop_front() {
            component.push(node);
            for &neighbour in &adj[node] {
                if !visited[neighbour] {
                    visited[neighbour] = true;
                    queue.push_back(neighbour);
                }
            }
        }
        // Sort indices within each component alphabetically by table name.
        component.sort_by(|&a, &b| tables[a].name.cmp(&tables[b].name));
        components.push(component);
    }

    // Separate components that have FK relationships from orphans (size==1, no edges).
    let (mut connected, mut orphans): (Vec<Vec<usize>>, Vec<Vec<usize>>) =
        components.into_iter().partition(|comp| {
            comp.len() > 1
                || comp.iter().any(|&idx| {
                    relationships
                        .iter()
                        .any(|r| r.from_table == idx || r.to_table == idx)
                })
        });

    // Sort groups alphabetically by the name of their first (lowest-name) table.
    connected.sort_by(|a, b| tables[a[0]].name.cmp(&tables[b[0]].name));
    orphans.sort_by(|a, b| tables[a[0]].name.cmp(&tables[b[0]].name));

    // Flatten into an ordered placement list.
    let order: Vec<usize> = connected.into_iter().chain(orphans).flatten().collect();

    // Assign grid positions left-to-right, wrapping at `num_cols`.
    for (placement_idx, &table_idx) in order.iter().enumerate() {
        let row = placement_idx / num_cols;
        let col = placement_idx % num_cols;
        tables[table_idx].grid_pos = (row, col);
    }
}

/// Detect cycles in the directed FK graph and mark one edge per cycle as
/// `is_cycle = true`.
///
/// Uses a standard DFS with a recursion-stack set.  When a back edge is found
/// (target is already on the current path) the edge is marked as a cycle edge.
pub(crate) fn detect_and_break_cycles(relationships: &mut [Relationship]) {
    // Collect the set of table indices referenced.
    let max_idx = relationships
        .iter()
        .flat_map(|r| [r.from_table, r.to_table])
        .max()
        .map_or(0, |m| m + 1);

    if max_idx == 0 {
        return;
    }

    // Build adjacency: node → list of (neighbour, relationship_index).
    let mut adj: Vec<Vec<(usize, usize)>> = vec![Vec::new(); max_idx];
    for (rel_idx, rel) in relationships.iter().enumerate() {
        if rel.from_table < max_idx && rel.to_table < max_idx {
            adj[rel.from_table].push((rel.to_table, rel_idx));
        }
    }

    let mut visited = vec![false; max_idx];
    let mut on_stack = vec![false; max_idx];

    // Iterative DFS to avoid stack overflow on large schemas.
    // Stack entries: (node, cursor) where cursor is the next index into adj[node].
    for start in 0..max_idx {
        if visited[start] {
            continue;
        }

        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        visited[start] = true;
        on_stack[start] = true;

        while let Some(frame) = stack.last_mut() {
            let (node, cursor) = *frame;

            if cursor < adj[node].len() {
                frame.1 += 1;
                let (neighbour, rel_idx) = adj[node][cursor];

                if !visited[neighbour] {
                    visited[neighbour] = true;
                    on_stack[neighbour] = true;
                    stack.push((neighbour, 0));
                } else if on_stack[neighbour] {
                    // Back edge → cycle detected; mark this relationship.
                    relationships[rel_idx].is_cycle = true;
                }
            } else {
                // Done with this node — pop and clear on_stack.
                on_stack[node] = false;
                stack.pop();
            }
        }
    }
}

/// Convert `grid_pos` coordinates into virtual (x, y) positions and compute
/// each table's rendered `size` (full column list, no zoom/expand modes).
///
/// Used by tests that need a standalone layout pipeline. In production,
/// [`ERDiagram::recalculate_layout`] computes display-mode-aware sizes and
/// positions inline.
#[cfg(test)]
fn compute_positions(tables: &mut [ERTable], spacing: u8) {
    if tables.is_empty() {
        return;
    }

    let spacing = spacing.clamp(1, 3);
    let gap_x = 4 * i32::from(spacing);
    let gap_y = 2 * i32::from(spacing);

    // First pass: compute each table's size.
    for table in tables.iter_mut() {
        let name_width = table.name.width() as i32;
        let max_col_width = table
            .columns
            .iter()
            .map(|c| {
                // 2 for the ": " separator (always ASCII).
                (c.name.width() + 2 + c.type_name.width()) as i32
            })
            .max()
            .unwrap_or(0);

        let content_width = name_width.max(max_col_width);
        // +5 for left/right border characters, padding, and marker column.
        let box_width = content_width + 5;
        // +4 for top border + name row + separator + bottom border.
        let box_height = table.columns.len() as i32 + 4;

        table.size = (box_width.max(6), box_height.max(4));
    }

    // Determine per-column max width and per-row max height for grid positioning.
    let max_col = tables.iter().map(|t| t.grid_pos.1).max().unwrap_or(0);
    let max_row = tables.iter().map(|t| t.grid_pos.0).max().unwrap_or(0);

    let num_cols = max_col + 1;
    let num_rows = max_row + 1;

    // col_widths[c] = max box width of all tables in grid column c.
    let mut col_widths = vec![0i32; num_cols];
    for table in tables.iter() {
        let c = table.grid_pos.1;
        col_widths[c] = col_widths[c].max(table.size.0);
    }

    // row_heights[r] = max box height of all tables in grid row r.
    let mut row_heights = vec![0i32; num_rows];
    for table in tables.iter() {
        let r = table.grid_pos.0;
        row_heights[r] = row_heights[r].max(table.size.1);
    }

    // Prefix-sum to get the x origin of each column and y origin of each row.
    let mut col_origins = vec![0i32; num_cols];
    for c in 1..num_cols {
        col_origins[c] = col_origins[c - 1] + col_widths[c - 1] + gap_x;
    }

    let mut row_origins = vec![0i32; num_rows];
    for r in 1..num_rows {
        row_origins[r] = row_origins[r - 1] + row_heights[r - 1] + gap_y;
    }

    // Second pass: assign positions.
    for table in tables.iter_mut() {
        let (r, c) = table.grid_pos;
        table.pos = (col_origins[c], row_origins[r]);
    }
}

// ─── Internal utilities ───────────────────────────────────────────────────────

/// Integer-only `ceil(sqrt(n))` — avoids any floating-point arithmetic and the
/// associated `cast_precision_loss` lint.
///
/// Uses a simple binary search over [0, n] to find the smallest integer `s`
/// satisfying `s * s >= n`.
fn integer_sqrt_ceil(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    // Binary search for the smallest s such that s*s >= n.
    let mut lo: usize = 1;
    let mut hi: usize = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if mid.saturating_mul(mid) >= n {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_er_table(name: &str, cols: &[&str]) -> ERTable {
        ERTable {
            name: name.to_string(),
            columns: cols
                .iter()
                .map(|c| ERColumn {
                    name: c.to_string(),
                    type_name: "TEXT".to_string(),
                    is_pk: *c == "id",
                    is_fk: c.ends_with("_id") && *c != "id",
                })
                .collect(),
            grid_pos: (0, 0),
            pos: (0, 0),
            size: (0, 0),
        }
    }

    #[test]
    fn layout_single_table() {
        let mut tables = vec![make_er_table("users", &["id", "name"])];
        let rels = vec![];
        compute_grid_layout(&mut tables, &rels);
        assert_eq!(tables[0].grid_pos, (0, 0));
    }

    #[test]
    fn layout_grid_dimensions() {
        // 5 tables → ceil(sqrt(5)) = 3 columns
        let mut tables: Vec<_> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| make_er_table(n, &["id"]))
            .collect();
        compute_grid_layout(&mut tables, &[]);
        let max_col = tables.iter().map(|t| t.grid_pos.1).max().unwrap();
        assert!(max_col <= 2, "max_col={max_col}, expected <= 2 (3 columns)");
    }

    #[test]
    fn layout_related_tables_adjacent() {
        let mut tables = vec![
            make_er_table("users", &["id"]),
            make_er_table("orders", &["id", "user_id"]),
        ];
        let rels = vec![Relationship {
            from_table: 1,
            from_column: "user_id".into(),
            to_table: 0,
            to_column: "id".into(),
            is_cycle: false,
        }];
        compute_grid_layout(&mut tables, &rels);
        let d_row = tables[0].grid_pos.0.abs_diff(tables[1].grid_pos.0);
        let d_col = tables[0].grid_pos.1.abs_diff(tables[1].grid_pos.1);
        assert!(d_row + d_col <= 1, "Related tables should be adjacent");
    }

    #[test]
    fn cycle_detection_breaks_cycles() {
        let mut rels = vec![
            Relationship {
                from_table: 0,
                from_column: "b_id".into(),
                to_table: 1,
                to_column: "id".into(),
                is_cycle: false,
            },
            Relationship {
                from_table: 1,
                from_column: "a_id".into(),
                to_table: 0,
                to_column: "id".into(),
                is_cycle: false,
            },
        ];
        detect_and_break_cycles(&mut rels);
        assert!(
            rels.iter().any(|r| r.is_cycle),
            "At least one edge should be marked as cycle"
        );
    }

    #[test]
    fn compute_positions_with_spacing() {
        let mut tables = vec![
            make_er_table("users", &["id", "name"]),
            make_er_table("orders", &["id"]),
        ];
        tables[0].grid_pos = (0, 0);
        tables[1].grid_pos = (0, 1);
        compute_positions(&mut tables, 2);
        // Second table should be to the right of first.
        assert!(
            tables[1].pos.0 > tables[0].pos.0,
            "orders.pos.x={} should be > users.pos.x={}",
            tables[1].pos.0,
            tables[0].pos.0,
        );
        // Both should have positive sizes.
        assert!(
            tables[0].size.0 > 0 && tables[0].size.1 > 0,
            "users size should be positive, got {:?}",
            tables[0].size,
        );
        assert!(
            tables[1].size.0 > 0 && tables[1].size.1 > 0,
            "orders size should be positive, got {:?}",
            tables[1].size,
        );
    }

    // ── Additional edge-case tests ────────────────────────────────────────────

    #[test]
    fn new_has_correct_defaults() {
        let er = ERDiagram::new();
        assert_eq!(er.zoom, ZoomLevel::Normal);
        assert_eq!(er.viewport, (0, 0));
        assert!(!er.loaded);
        assert!(er.layout_dirty);
        assert!(er.tables.is_empty());
        assert!(er.relationships.is_empty());
        assert!(!er.is_fullscreen);
        assert!(er.connected_tables.is_empty());
    }

    #[test]
    fn cycle_detection_no_cycles() {
        let mut rels = vec![Relationship {
            from_table: 0,
            from_column: "b_id".into(),
            to_table: 1,
            to_column: "id".into(),
            is_cycle: false,
        }];
        detect_and_break_cycles(&mut rels);
        assert!(
            rels.iter().all(|r| !r.is_cycle),
            "No edge should be marked as cycle in a DAG"
        );
    }

    #[test]
    fn compute_positions_spacing_increases_gap() {
        let make_tables = || {
            let mut tables = vec![make_er_table("a", &["id"]), make_er_table("b", &["id"])];
            tables[0].grid_pos = (0, 0);
            tables[1].grid_pos = (0, 1);
            tables
        };

        let mut tight = make_tables();
        compute_positions(&mut tight, 1);

        let mut spacious = make_tables();
        compute_positions(&mut spacious, 3);

        // Spacing=3 should push the second table further right than spacing=1.
        assert!(
            spacious[1].pos.0 > tight[1].pos.0,
            "Higher spacing should produce a larger x gap"
        );
    }

    #[test]
    fn layout_empty_tables() {
        let mut tables: Vec<ERTable> = Vec::new();
        compute_grid_layout(&mut tables, &[]);
        // Should not panic.
        assert!(tables.is_empty());
    }

    #[test]
    fn compute_positions_empty() {
        let mut tables: Vec<ERTable> = Vec::new();
        compute_positions(&mut tables, 2);
        // Should not panic.
        assert!(tables.is_empty());
    }

    #[test]
    fn layout_four_tables_two_by_two() {
        // ceil(sqrt(4)) = 2 columns → should produce a 2×2 grid.
        let mut tables: Vec<_> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| make_er_table(n, &["id"]))
            .collect();
        compute_grid_layout(&mut tables, &[]);
        let max_col = tables.iter().map(|t| t.grid_pos.1).max().unwrap();
        let max_row = tables.iter().map(|t| t.grid_pos.0).max().unwrap();
        assert!(
            max_col <= 1,
            "4 tables in 2 cols, max_col should be <=1, got {max_col}"
        );
        assert!(
            max_row <= 1,
            "4 tables in 2 cols, max_row should be <=1, got {max_row}"
        );
    }

    #[test]
    fn integer_sqrt_ceil_values() {
        assert_eq!(integer_sqrt_ceil(0), 0);
        assert_eq!(integer_sqrt_ceil(1), 1);
        assert_eq!(integer_sqrt_ceil(4), 2);
        assert_eq!(integer_sqrt_ceil(5), 3); // ceil(sqrt(5)) = ceil(2.23) = 3
        assert_eq!(integer_sqrt_ceil(9), 3);
        assert_eq!(integer_sqrt_ceil(10), 4); // ceil(sqrt(10)) = ceil(3.16) = 4
    }

    // ── build_from_schema tests ──────────────────────────────────────────────

    use tursotui_db::ColumnInfo;

    fn make_schema_entry(name: &str, sql: Option<&str>) -> SchemaEntry {
        SchemaEntry {
            obj_type: "table".into(),
            name: name.into(),
            tbl_name: name.into(),
            sql: sql.map(String::from),
        }
    }

    fn make_column_info(name: &str, col_type: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            col_type: col_type.into(),
            notnull: false,
            default_value: None,
            pk,
        }
    }

    #[test]
    fn build_from_schema_data() {
        let entries = vec![
            make_schema_entry(
                "users",
                Some("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"),
            ),
            make_schema_entry(
                "orders",
                Some(
                    "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, FOREIGN KEY (user_id) REFERENCES users(id))",
                ),
            ),
        ];
        let columns = HashMap::from([
            (
                "users".into(),
                vec![
                    make_column_info("id", "INTEGER", true),
                    make_column_info("name", "TEXT", false),
                ],
            ),
            (
                "orders".into(),
                vec![
                    make_column_info("id", "INTEGER", true),
                    make_column_info("user_id", "INTEGER", false),
                ],
            ),
        ]);
        let mut diagram = ERDiagram::new();
        diagram.build_from_schema(&entries, &columns);
        assert_eq!(diagram.tables.len(), 2);
        assert_eq!(diagram.relationships.len(), 1);
        assert_eq!(diagram.relationships[0].from_column, "user_id");
        assert_eq!(diagram.relationships[0].to_column, "id");
        assert!(diagram.loaded);
        assert!(
            diagram.layout_dirty,
            "layout_dirty stays true — positions computed on first render"
        );
        assert_eq!(diagram.focused_table, Some(0));
    }

    #[test]
    fn build_skips_non_tables() {
        let entries = vec![SchemaEntry {
            obj_type: "index".into(),
            name: "idx_foo".into(),
            tbl_name: "users".into(),
            sql: None,
        }];
        let columns = HashMap::new();
        let mut diagram = ERDiagram::new();
        diagram.build_from_schema(&entries, &columns);
        assert_eq!(diagram.tables.len(), 0);
        assert!(diagram.loaded);
        assert!(diagram.focused_table.is_none());
    }

    #[test]
    fn build_marks_fk_columns() {
        let entries = vec![
            make_schema_entry("users", Some("CREATE TABLE users (id INTEGER PRIMARY KEY)")),
            make_schema_entry(
                "orders",
                Some(
                    "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, FOREIGN KEY (user_id) REFERENCES users(id))",
                ),
            ),
        ];
        let columns = HashMap::from([
            (
                "users".into(),
                vec![make_column_info("id", "INTEGER", true)],
            ),
            (
                "orders".into(),
                vec![
                    make_column_info("id", "INTEGER", true),
                    make_column_info("user_id", "INTEGER", false),
                ],
            ),
        ]);
        let mut diagram = ERDiagram::new();
        diagram.build_from_schema(&entries, &columns);

        let orders = &diagram.tables[1];
        let user_id_col = orders.columns.iter().find(|c| c.name == "user_id").unwrap();
        assert!(user_id_col.is_fk, "user_id should be marked as FK");

        let id_col = orders.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(!id_col.is_fk, "id should NOT be marked as FK");
    }

    #[test]
    fn build_handles_missing_target_table() {
        // FK references a table not in the schema — should not panic.
        let entries = vec![make_schema_entry(
            "orders",
            Some(
                "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, FOREIGN KEY (user_id) REFERENCES nonexistent(id))",
            ),
        )];
        let columns = HashMap::from([(
            "orders".into(),
            vec![
                make_column_info("id", "INTEGER", true),
                make_column_info("user_id", "INTEGER", false),
            ],
        )]);
        let mut diagram = ERDiagram::new();
        diagram.build_from_schema(&entries, &columns);
        assert_eq!(diagram.tables.len(), 1);
        assert_eq!(
            diagram.relationships.len(),
            0,
            "FK to missing table should be skipped"
        );
    }

    #[test]
    fn build_with_no_sql() {
        // Tables without SQL (e.g., internal tables) should still appear.
        let entries = vec![make_schema_entry("internal_table", None)];
        let columns = HashMap::from([(
            "internal_table".into(),
            vec![make_column_info("id", "INTEGER", true)],
        )]);
        let mut diagram = ERDiagram::new();
        diagram.build_from_schema(&entries, &columns);
        assert_eq!(diagram.tables.len(), 1);
        assert_eq!(diagram.relationships.len(), 0);
    }

    #[test]
    fn build_defers_layout_to_first_render() {
        let entries = vec![
            make_schema_entry("users", Some("CREATE TABLE users (id INTEGER PRIMARY KEY)")),
            make_schema_entry(
                "orders",
                Some("CREATE TABLE orders (id INTEGER PRIMARY KEY)"),
            ),
        ];
        let columns = HashMap::from([
            (
                "users".into(),
                vec![make_column_info("id", "INTEGER", true)],
            ),
            (
                "orders".into(),
                vec![make_column_info("id", "INTEGER", true)],
            ),
        ]);
        let mut diagram = ERDiagram::new();
        diagram.build_from_schema(&entries, &columns);

        // Sizes are (0, 0) after build — layout is deferred to first render.
        assert!(
            diagram.layout_dirty,
            "layout_dirty should be true after build"
        );

        // Manually trigger recalculate_layout to verify it produces valid sizes.
        diagram.recalculate_layout();
        assert!(!diagram.layout_dirty);
        for table in &diagram.tables {
            assert!(
                table.size.0 > 0 && table.size.1 > 0,
                "Table {} should have positive size after layout, got {:?}",
                table.name,
                table.size,
            );
        }
    }

    // ── handle_key tests ─────────────────────────────────────────────────────

    fn make_diagram_with_tables() -> ERDiagram {
        let mut er = ERDiagram::new();
        er.tables = vec![
            make_er_table("alpha", &["id", "name"]),
            make_er_table("beta", &["id", "value"]),
            make_er_table("gamma", &["id"]),
        ];
        er.focused_table = Some(0);
        er.loaded = true;
        er.layout_dirty = false;
        er
    }

    #[test]
    fn tab_cycles_focused_table() {
        let mut er = make_diagram_with_tables();
        // Trigger layout so positions are set (Tab autopans using table positions).
        er.layout_dirty = true;
        er.recalculate_layout();

        assert_eq!(er.focused_table, Some(0));
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

        let action = er.handle_key(key);
        assert!(action.is_some(), "Tab should return Some to consume event");
        assert_eq!(er.focused_table, Some(1));

        er.handle_key(key);
        assert_eq!(er.focused_table, Some(2));

        er.handle_key(key);
        assert_eq!(er.focused_table, Some(0), "Tab should wrap around");
    }

    #[test]
    fn o_returns_populate_editor() {
        let mut er = make_diagram_with_tables();
        er.focused_table = Some(1); // "beta"
        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        let action = er.handle_key(key);
        match action {
            Some(Action::Editor(EditorAction::PopulateEditor(sql))) => {
                assert!(
                    sql.contains("\"beta\""),
                    "SQL should reference the focused table name, got: {sql}"
                );
            }
            other => panic!("Expected PopulateEditor, got: {other:?}"),
        }
    }

    #[test]
    fn o_without_focus_returns_none() {
        let mut er = make_diagram_with_tables();
        er.focused_table = None;
        let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        let action = er.handle_key(key);
        assert!(action.is_none(), "o without focus should return None");
    }

    #[test]
    fn plus_minus_cycle_zoom() {
        let mut er = make_diagram_with_tables();
        assert_eq!(er.zoom, ZoomLevel::Normal);

        let plus = KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE);
        let minus = KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE);

        // Normal → Detail
        er.handle_key(plus);
        assert_eq!(er.zoom, ZoomLevel::Detail);
        assert!(er.layout_dirty);

        // At Detail max — should stay Detail
        er.layout_dirty = false;
        er.handle_key(plus);
        assert_eq!(er.zoom, ZoomLevel::Detail);

        // Detail → Normal
        er.handle_key(minus);
        assert_eq!(er.zoom, ZoomLevel::Normal);

        // Normal → Overview
        er.handle_key(minus);
        assert_eq!(er.zoom, ZoomLevel::Overview);

        // At Overview min — should stay Overview
        er.layout_dirty = false;
        er.handle_key(minus);
        assert_eq!(er.zoom, ZoomLevel::Overview);
    }

    #[test]
    fn enter_toggles_expand_and_dirties_layout() {
        let mut er = make_diagram_with_tables();
        er.focused_table = Some(1);
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        er.handle_key(key);
        assert!(
            er.expanded_tables.contains(&1),
            "Enter should expand focused table"
        );
        assert!(er.layout_dirty);

        er.layout_dirty = false;
        er.handle_key(key);
        assert!(
            !er.expanded_tables.contains(&1),
            "Enter again should collapse focused table"
        );
        assert!(er.layout_dirty);
    }

    #[test]
    fn enter_noop_at_overview_and_detail() {
        let mut er = make_diagram_with_tables();
        er.focused_table = Some(0);
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        er.zoom = ZoomLevel::Overview;
        er.layout_dirty = false;
        er.handle_key(key);
        assert!(
            er.expanded_tables.is_empty(),
            "Enter should be no-op at Overview"
        );
        assert!(!er.layout_dirty);

        er.zoom = ZoomLevel::Detail;
        er.layout_dirty = false;
        er.handle_key(key);
        assert!(
            er.expanded_tables.is_empty(),
            "Enter should be no-op at Detail"
        );
        assert!(!er.layout_dirty);
    }

    #[test]
    fn zoom_boundary_does_not_dirty_layout() {
        let mut er = make_diagram_with_tables();

        er.zoom = ZoomLevel::Detail;
        er.layout_dirty = false;
        er.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
        assert_eq!(er.zoom, ZoomLevel::Detail);
        assert!(!er.layout_dirty, "+ at Detail should not dirty layout");

        er.zoom = ZoomLevel::Overview;
        er.layout_dirty = false;
        er.handle_key(KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE));
        assert_eq!(er.zoom, ZoomLevel::Overview);
        assert!(!er.layout_dirty, "- at Overview should not dirty layout");
    }

    #[test]
    fn rebuild_connected_tables_finds_direct_neighbors() {
        let mut er = ERDiagram::new();
        er.tables.push(make_er_table("a", &["id"]));
        er.tables.push(make_er_table("b", &["id", "a_id"]));
        er.tables.push(make_er_table("c", &["id", "b_id"]));
        er.relationships.push(Relationship {
            from_table: 1,
            from_column: "a_id".to_string(),
            to_table: 0,
            to_column: "id".to_string(),
            is_cycle: false,
        });
        er.relationships.push(Relationship {
            from_table: 2,
            from_column: "b_id".to_string(),
            to_table: 1,
            to_column: "id".to_string(),
            is_cycle: false,
        });

        // Focus on table b (index 1) — connected to a and c
        er.focused_table = Some(1);
        er.rebuild_connected_tables();
        assert!(
            er.connected_tables.contains(&0),
            "a should be connected to b"
        );
        assert!(
            er.connected_tables.contains(&2),
            "c should be connected to b"
        );
        assert!(
            !er.connected_tables.contains(&1),
            "b should not be in its own connected set"
        );

        // Focus on table a (index 0) — connected to b only (not transitive to c)
        er.focused_table = Some(0);
        er.rebuild_connected_tables();
        assert!(
            er.connected_tables.contains(&1),
            "b should be connected to a"
        );
        assert!(
            !er.connected_tables.contains(&2),
            "c should NOT be connected to a (not transitive)"
        );

        // No focus — connected set empty
        er.focused_table = None;
        er.rebuild_connected_tables();
        assert!(er.connected_tables.is_empty());
    }

    #[test]
    fn column_anchor_y_overview_returns_header() {
        let mut er = ERDiagram::new();
        let mut table = make_er_table("t", &["id", "name", "ref_id"]);
        table.pos = (0, 0);
        table.size = (20, 8);
        er.tables.push(table);
        er.zoom = ZoomLevel::Overview;

        // At Overview, all columns return header Y regardless of column name
        let y = er.column_anchor_y(0, "ref_id", false);
        assert_eq!(y, 1, "Overview should return header_y = pos.1 + 1");
    }

    #[test]
    fn column_anchor_y_detail_returns_column_row() {
        let mut er = ERDiagram::new();
        let mut table = make_er_table("t", &["id", "name", "ref_id"]);
        table.pos = (0, 0);
        table.size = (20, 8);
        er.tables.push(table);
        er.zoom = ZoomLevel::Detail;

        // At Detail, column should be at sep_y + 1 + column_index
        // sep_y = pos.1 + 2 = 2, so ref_id (index 2) should be at 2 + 1 + 2 = 5
        let y = er.column_anchor_y(0, "ref_id", false);
        assert_eq!(y, 5, "Detail should return column's row position");
    }

    #[test]
    fn shift_tab_cycles_backward() {
        let mut er = make_diagram_with_tables();
        er.focused_table = Some(0);
        // Assign positions so viewport snap works
        for (i, t) in er.tables.iter_mut().enumerate() {
            t.pos = (i as i32 * 20, 0);
            t.size = (15, 6);
        }

        let key = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        er.handle_key(key);
        assert_eq!(
            er.focused_table,
            Some(er.tables.len() - 1),
            "Shift+Tab from 0 should wrap to last table"
        );
    }

    #[test]
    fn esc_clears_focus_and_connected() {
        let mut er = make_diagram_with_tables();
        er.focused_table = Some(1);
        er.connected_tables.insert(0);
        er.connected_tables.insert(2);

        er.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(er.focused_table, None, "Esc should clear focused_table");
        assert!(
            er.connected_tables.is_empty(),
            "Esc should clear connected_tables"
        );
    }
}
