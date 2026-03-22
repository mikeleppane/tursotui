use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{Action, BottomTab, Direction};
use crate::components::data_editor::quote_identifier;
use crate::db::{ColumnInfo, DatabaseHandle, SchemaEntry};
use crate::theme::Theme;

use super::Component;

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
    #[allow(dead_code)] // retained for edge labels and tooltip display (planned v2 feature)
    pub(crate) from_column: String,
    /// Index into the `tables` slice of the owning [`ERDiagram`].
    pub(crate) to_table: usize,
    #[allow(dead_code)] // retained for edge labels and tooltip display (planned v2 feature)
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
    /// Spacing multiplier: 1 = tight, 2 = normal, 3 = spacious.
    pub(crate) spacing: u8,
    pub(crate) focused_table: Option<usize>,
    pub(crate) compact_mode: bool,
    /// Indices of tables whose column list is expanded in the diagram.
    pub(crate) expanded_tables: Vec<usize>,
    /// True when the layout needs to be recalculated.
    pub(crate) layout_dirty: bool,
    /// False until the first successful build from `schema_cache`.
    pub(crate) loaded: bool,
}

impl ERDiagram {
    pub(crate) fn new() -> Self {
        Self {
            tables: Vec::new(),
            relationships: Vec::new(),
            viewport: (0, 0),
            spacing: 2,
            focused_table: None,
            compact_mode: false,
            expanded_tables: Vec::new(),
            layout_dirty: true,
            loaded: false,
        }
    }

    /// Build (or rebuild) the ER diagram from schema metadata.
    ///
    /// `entries` are the `sqlite_schema` rows; only tables are kept.
    /// `columns` maps table name → column list (from PRAGMA `table_info`).
    ///
    /// FK relationships are parsed from each table's `CREATE TABLE` SQL via
    /// [`DatabaseHandle::parse_foreign_keys`] (static method — no db query).
    pub(crate) fn build_from_schema(
        &mut self,
        entries: &[SchemaEntry],
        columns: &HashMap<String, Vec<ColumnInfo>>,
    ) {
        self.tables.clear();
        self.relationships.clear();
        self.expanded_tables.clear();

        // Build a name → index map so FK targets can be resolved to table indices.
        let mut name_to_idx: HashMap<&str, usize> = HashMap::new();

        for entry in entries {
            if entry.obj_type != "table" {
                continue;
            }

            let cols: Vec<ERColumn> = columns
                .get(&entry.name)
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
            name_to_idx.insert(&entry.name, idx);

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
            let Some(&from_idx) = name_to_idx.get(entry.name.as_str()) else {
                continue;
            };

            let fks = DatabaseHandle::parse_foreign_keys(sql);
            for fk in fks {
                let Some(&to_idx) = name_to_idx.get(fk.to_table.as_str()) else {
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
        let is_focused = self.focused_table == Some(table_idx);
        let border_style = if is_focused {
            Style::default().fg(theme.border_focused)
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
        let name_style = if is_focused {
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

        // Separator: ├──...──┤
        let sep_y = ty + 2;
        put_char(vp, buf, area, tx, sep_y, '├', border_style);
        for dx in 1..tw - 1 {
            put_char(vp, buf, area, tx + dx, sep_y, '─', border_style);
        }
        put_char(vp, buf, area, tx + tw - 1, sep_y, '┤', border_style);

        let is_expanded = self.expanded_tables.contains(&table_idx);
        let mut row_y = sep_y + 1;

        if self.compact_mode {
            put_char(vp, buf, area, tx, row_y, '│', border_style);
            put_char(vp, buf, area, tx + tw - 1, row_y, '│', border_style);
            row_y += 1;
        } else {
            let pk_cols: Vec<usize> = table
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.is_pk)
                .map(|(i, _)| i)
                .collect();
            let indices_to_show: Vec<usize> = if is_expanded {
                (0..table.columns.len()).collect()
            } else {
                pk_cols.clone()
            };

            for &col_idx in &indices_to_show {
                let col = &table.columns[col_idx];
                put_char(vp, buf, area, tx, row_y, '│', border_style);
                put_char(vp, buf, area, tx + tw - 1, row_y, '│', border_style);

                let (marker, marker_style) = if col.is_pk {
                    ("ⓟ", theme.er_pk_style)
                } else if col.is_fk {
                    ("ⓕ", theme.er_fk_style)
                } else {
                    (" ", Style::default().fg(theme.fg))
                };
                put_str(vp, buf, area, tx + 1, row_y, marker, marker_style);

                let col_text = format!("{}: {}", col.name, col.type_name);
                let col_style = if col.is_pk {
                    theme.er_pk_style
                } else if col.is_fk {
                    theme.er_fk_style
                } else {
                    Style::default().fg(theme.fg)
                };
                put_str(vp, buf, area, tx + 2, row_y, &col_text, col_style);
                row_y += 1;
            }

            if !is_expanded {
                let hidden = table.columns.len().saturating_sub(pk_cols.len());
                if hidden > 0 {
                    put_char(vp, buf, area, tx, row_y, '│', border_style);
                    put_char(vp, buf, area, tx + tw - 1, row_y, '│', border_style);
                    let indicator = format!("(+{hidden})");
                    put_str(
                        vp,
                        buf,
                        area,
                        tx + 2,
                        row_y,
                        &indicator,
                        Style::default().fg(theme.dim),
                    );
                    row_y += 1;
                }
            }
        }

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
        if self.compact_mode {
            1 // just a spacer row below header
        } else if self.expanded_tables.contains(&table_idx) {
            table.columns.len() as i32
        } else {
            // PK columns + optional (+N) indicator
            let pk_count = table.columns.iter().filter(|c| c.is_pk).count() as i32;
            let hidden = table.columns.len() as i32 - pk_count;
            pk_count + i32::from(hidden > 0) // +1 for (+N) indicator
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
            let box_width = content_width + 4; // borders + padding

            let visible_rows = self.display_row_count(idx);
            // +4 for top border + name row + separator + bottom border
            let box_height = visible_rows + 4;

            self.tables[idx].size = (box_width.max(6), box_height.max(4));
        }
        // Assign positions using pre-computed sizes and grid_pos.
        let sp = self.spacing.clamp(1, 3);
        let gap_x = 4 * i32::from(sp);
        let gap_y = 2 * i32::from(sp);

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

        self.layout_dirty = false;
    }

    /// Render all FK edges (behind table boxes).
    fn render_edges(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        for rel in &self.relationships {
            if rel.from_table >= self.tables.len() || rel.to_table >= self.tables.len() {
                continue;
            }
            let from_vis = self.table_visible(&self.tables[rel.from_table], area);
            let to_vis = self.table_visible(&self.tables[rel.to_table], area);
            if !from_vis && !to_vis {
                continue;
            }

            let from = &self.tables[rel.from_table];
            let to = &self.tables[rel.to_table];
            let edge_style = if rel.is_cycle {
                theme.er_relationship.add_modifier(Modifier::DIM)
            } else {
                theme.er_relationship
            };
            let dash_char = if rel.is_cycle { '╌' } else { '─' };

            // Self-referential FK: draw a small loop on the right side of the table.
            if rel.from_table == rel.to_table {
                let rx = from.pos.0 + from.size.0; // right edge
                let ry = from.pos.1 + 1; // name row
                put_char(self.viewport, buf, area, rx, ry, dash_char, edge_style);
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

            let (from_x, from_y) = (from.pos.0 + from.size.0, from.pos.1 + 1);
            let (to_x, to_y) = (to.pos.0, to.pos.1 + 1);

            if from_y == to_y {
                // Straight horizontal line
                let (start_x, end_x) = if from_x <= to_x {
                    (from_x, to_x)
                } else {
                    (to_x + to.size.0, from.pos.0)
                };
                for x in start_x..end_x {
                    put_char(self.viewport, buf, area, x, from_y, dash_char, edge_style);
                }
                let marker_x = if from_x <= to_x { to_x - 4 } else { start_x };
                let label = if rel.is_cycle { "[cyc]" } else { "1──*" };
                put_str(
                    self.viewport,
                    buf,
                    area,
                    marker_x.max(start_x),
                    from_y,
                    label,
                    edge_style,
                );
            } else {
                // L-shaped: horizontal then vertical
                let mid_x = if from_x <= to_x { to_x } else { from.pos.0 };
                let (h_start, h_end) = if from_x <= mid_x {
                    (from_x, mid_x)
                } else {
                    (mid_x, from_x)
                };
                for x in h_start..=h_end {
                    put_char(self.viewport, buf, area, x, from_y, dash_char, edge_style);
                }
                let (v_start, v_end) = if from_y <= to_y {
                    (from_y, to_y)
                } else {
                    (to_y, from_y)
                };
                let v_char = if rel.is_cycle { '╎' } else { '│' };
                for y in v_start..=v_end {
                    put_char(self.viewport, buf, area, mid_x, y, v_char, edge_style);
                }
                let corner = if from_x <= mid_x {
                    if from_y < to_y { '╮' } else { '╯' }
                } else if from_y < to_y {
                    '╭'
                } else {
                    '╰'
                };
                put_char(self.viewport, buf, area, mid_x, from_y, corner, edge_style);
                if rel.is_cycle {
                    put_str(
                        self.viewport,
                        buf,
                        area,
                        mid_x + 1,
                        to_y,
                        "[cyc]",
                        edge_style,
                    );
                } else {
                    put_str(self.viewport, buf, area, mid_x + 1, to_y, "*", edge_style);
                    put_str(self.viewport, buf, area, h_start, from_y, "1", edge_style);
                }
            }
        }
    }
}

impl Component for ERDiagram {
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

            // Toggle expand focused table
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if let Some(idx) = self.focused_table {
                    if let Some(pos) = self.expanded_tables.iter().position(|&i| i == idx) {
                        self.expanded_tables.remove(pos);
                    } else {
                        self.expanded_tables.push(idx);
                    }
                    self.layout_dirty = true;
                }
                None
            }

            // Toggle compact mode
            (KeyModifiers::NONE, KeyCode::Char('c')) => {
                self.compact_mode = !self.compact_mode;
                self.layout_dirty = true;
                None
            }

            // Adjust spacing
            (KeyModifiers::NONE, KeyCode::Char('+' | '=')) => {
                if self.spacing < 3 {
                    self.spacing += 1;
                    self.layout_dirty = true;
                }
                None
            }
            (KeyModifiers::NONE, KeyCode::Char('-')) => {
                if self.spacing > 1 {
                    self.spacing -= 1;
                    self.layout_dirty = true;
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
                    // Auto-pan viewport to show focused table
                    let table = &self.tables[next];
                    self.viewport.0 = table.pos.0 - 2;
                    self.viewport.1 = table.pos.1 - 1;
                }
                // Return Some to consume Tab — prevents global CycleFocus
                Some(Action::SwitchBottomTab(BottomTab::ERDiagram))
            }

            // Open focused table in query editor
            (KeyModifiers::NONE, KeyCode::Char('o')) => {
                if let Some(idx) = self.focused_table {
                    let table_name = &self.tables[idx].name;
                    let sql = format!("SELECT * FROM {} LIMIT 100;", quote_identifier(table_name));
                    return Some(Action::PopulateEditor(sql));
                }
                None
            }

            // Esc releases focus
            (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::CycleFocus(Direction::Forward)),

            _ => None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let block = super::panel_block("ER Diagram", focused, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Check empty states
        if !self.loaded {
            Self::render_centered(frame, inner, "Loading schema...", theme);
            return;
        }
        if self.tables.is_empty() {
            Self::render_centered(frame, inner, "No tables in database", theme);
            return;
        }

        // Recalculate layout if dirty (e.g., after spacing change or expand/compact toggle)
        self.recalculate_layout();

        let buf = frame.buffer_mut();

        // Render edges first (behind tables), then tables on top.
        self.render_edges(buf, inner, theme);
        for idx in 0..self.tables.len() {
            if self.table_visible(&self.tables[idx], inner) {
                self.render_table(buf, inner, idx, theme);
            }
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
/// each table's rendered `size` (full column list, no compact/expand modes).
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
        // +4 for left/right border characters plus one padding space each side.
        let box_width = content_width + 4;
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
        assert_eq!(er.spacing, 2);
        assert_eq!(er.viewport, (0, 0));
        assert!(!er.loaded);
        assert!(er.layout_dirty);
        assert!(er.tables.is_empty());
        assert!(er.relationships.is_empty());
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

    use crate::db::ColumnInfo;

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
            Some(Action::PopulateEditor(sql)) => {
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
    fn plus_minus_clamp_spacing() {
        let mut er = make_diagram_with_tables();
        assert_eq!(er.spacing, 2);

        let plus = KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE);
        let minus = KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE);

        er.handle_key(plus);
        assert_eq!(er.spacing, 3);
        assert!(er.layout_dirty);

        // At max — should not increase further
        er.layout_dirty = false;
        er.handle_key(plus);
        assert_eq!(er.spacing, 3);
        assert!(!er.layout_dirty, "No change should not dirty layout");

        er.handle_key(minus);
        assert_eq!(er.spacing, 2);

        er.handle_key(minus);
        assert_eq!(er.spacing, 1);

        // At min — should not decrease further
        er.layout_dirty = false;
        er.handle_key(minus);
        assert_eq!(er.spacing, 1);
        assert!(!er.layout_dirty, "No change should not dirty layout");
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
    fn c_toggles_compact_mode() {
        let mut er = make_diagram_with_tables();
        assert!(!er.compact_mode);

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);

        er.handle_key(key);
        assert!(er.compact_mode);
        assert!(er.layout_dirty);

        er.layout_dirty = false;
        er.handle_key(key);
        assert!(!er.compact_mode);
        assert!(er.layout_dirty);
    }
}
