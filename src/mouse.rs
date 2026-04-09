use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use unicode_width::UnicodeWidthStr;

use crate::GlobalFeatures;
use crate::app::{
    Action, AppState, BottomTab, DragState, DragTarget, NavAction, PanelId, SubTab, UiAction,
};
use crate::components::Component;

pub(crate) fn handle_mouse_event(
    mouse: MouseEvent,
    app: &mut AppState,
    _global_ui: &mut GlobalFeatures,
) {
    let col = mouse.column;
    let row = mouse.row;

    // Phase 1: Global overlays capture all mouse events
    if let Some(overlay) = app.global_overlay {
        handle_global_overlay_mouse(mouse, app, overlay);
        return;
    }

    // Phase 2: Per-database overlays — click dismisses
    let active_idx = app.active_db;
    let db = &app.databases[active_idx];
    if db.db_overlay.is_some() || db.ddl_viewer.is_some() {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let db = &mut app.databases[active_idx];
            db.db_overlay = None;
            db.ddl_viewer = None;
        }
        return;
    }

    // Handle drag continuation
    if app.drag_state.is_some() {
        handle_drag(mouse, app);
        return;
    }

    // Phase 3: Panel routing
    let rects = app.layout_rects.clone();

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            handle_click(col, row, mouse, app, &rects);
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            handle_scroll(mouse, col, row, app, &rects);
        }
        _ => {}
    }
}

fn handle_global_overlay_mouse(
    mouse: MouseEvent,
    app: &mut AppState,
    overlay: crate::app::GlobalOverlay,
) {
    use crate::app::GlobalOverlay;
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if matches!(overlay, GlobalOverlay::Help) {
                app.help_scroll = app.help_scroll.saturating_sub(3);
            }
        }
        MouseEventKind::ScrollDown => {
            if matches!(overlay, GlobalOverlay::Help) {
                app.help_scroll = app.help_scroll.saturating_add(3);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Click dismisses the overlay. TODO: once interactive overlays
            // (History, Bookmarks, etc.) gain handle_mouse(), only dismiss
            // on clicks outside the popup area.
            app.global_overlay = None;
        }
        _ => {}
    }
}

fn handle_click(
    col: u16,
    row: u16,
    mouse: MouseEvent,
    app: &mut AppState,
    rects: &crate::app::LayoutRects,
) {
    let active_idx = app.active_db;

    // Check db tabs first
    if let Some(db_tab_rect) = rects.db_tabs
        && contains(db_tab_rect, col, row)
    {
        let labels: Vec<String> = crate::layout::build_tab_labels(&app.databases)
            .iter()
            .enumerate()
            .map(|(i, label)| {
                if i == active_idx {
                    format!(" \u{25c6} {label} ")
                } else {
                    format!(" {label} ")
                }
            })
            .collect();
        if let Some(idx) = resolve_tab_click(col, db_tab_rect, &labels) {
            // Last label is the [+] button
            if idx < app.databases.len() {
                let action = Action::Nav(NavAction::SwitchDatabase(idx));
                app.update(&action);
            }
        }
        return;
    }

    // Check sub-tabs
    if contains(rects.sub_tabs, col, row) {
        let labels = [" Query ".to_string(), " Admin ".to_string()];
        if let Some(idx) = resolve_tab_click(col, rects.sub_tabs, &labels) {
            let tab = if idx == 0 {
                SubTab::Query
            } else {
                SubTab::Admin
            };
            let action = Action::Nav(NavAction::SwitchSubTab(tab));
            app.update(&action);
        }
        return;
    }

    // Route based on active sub-tab
    if app.databases[active_idx].sub_tab == SubTab::Admin {
        // Admin tab: DbInfo (left) and Pragmas (right)
        if contains(rects.admin_left, col, row) {
            app.databases[active_idx].focus = PanelId::DbInfo;
            return;
        }
        if contains(rects.admin_right, col, row) {
            app.databases[active_idx].focus = PanelId::Pragmas;
            let db = &mut app.databases[active_idx];
            if let Some(action) = db.pragmas.handle_mouse(mouse, rects.admin_right) {
                app.update(&action);
            }
            return;
        }
        return;
    }

    // Query tab panels below

    // Check bottom tabs
    if contains(rects.bottom_tabs, col, row) {
        let labels = [
            " 1:Results ".to_string(),
            " 2:Explain ".to_string(),
            " 3:Detail ".to_string(),
            " 4:ER ".to_string(),
            " 5:Profile ".to_string(),
        ];
        if let Some(idx) = resolve_tab_click(col, rects.bottom_tabs, &labels) {
            let tab = match idx {
                0 => BottomTab::Results,
                1 => BottomTab::Explain,
                2 => BottomTab::Detail,
                3 => BottomTab::ERDiagram,
                _ => BottomTab::Profile,
            };
            let action = Action::Nav(NavAction::SwitchBottomTab(tab));
            app.update(&action);
            app.databases[active_idx].focus = PanelId::Bottom;
        }
        return;
    }

    // Check sidebar
    if let Some(sidebar_rect) = rects.sidebar
        && contains(sidebar_rect, col, row)
    {
        app.databases[active_idx].focus = PanelId::Schema;
        let db = &mut app.databases[active_idx];
        if let Some(action) = db.schema.handle_mouse(mouse, sidebar_rect) {
            app.update(&action);
        }
        return;
    }

    // Check editor
    if contains(rects.editor, col, row) {
        app.databases[active_idx].focus = PanelId::Editor;
        return;
    }

    // Check bottom panel
    if contains(rects.bottom, col, row) {
        app.databases[active_idx].focus = PanelId::Bottom;
        if let Some(action) = dispatch_to_bottom_tab(app, active_idx, mouse, rects.bottom) {
            app.update(&action);
        }
        return;
    }

    // Check drag hit zones on panel borders
    check_drag_start(col, row, app, rects);
}

fn handle_scroll(
    mouse: MouseEvent,
    col: u16,
    row: u16,
    app: &mut AppState,
    rects: &crate::app::LayoutRects,
) {
    let active_idx = app.active_db;

    // Admin tab scroll routing
    if app.databases[active_idx].sub_tab == SubTab::Admin {
        if contains(rects.admin_right, col, row) {
            let db = &mut app.databases[active_idx];
            if let Some(action) = db.pragmas.handle_mouse(mouse, rects.admin_right) {
                app.update(&action);
            }
        }
        return;
    }

    // Route scroll to component under cursor WITHOUT changing focus
    if let Some(sidebar_rect) = rects.sidebar
        && contains(sidebar_rect, col, row)
    {
        let db = &mut app.databases[active_idx];
        if let Some(action) = db.schema.handle_mouse(mouse, sidebar_rect) {
            app.update(&action);
        }
        return;
    }

    if contains(rects.editor, col, row) {
        let db = &mut app.databases[active_idx];
        if let Some(action) = db.editor.handle_mouse(mouse, rects.editor) {
            app.update(&action);
        }
        return;
    }

    if contains(rects.bottom, col, row)
        && let Some(action) = dispatch_to_bottom_tab(app, active_idx, mouse, rects.bottom)
    {
        app.update(&action);
    }
}

/// Route a mouse event to the active bottom-tab component.
fn dispatch_to_bottom_tab(
    app: &mut AppState,
    db_idx: usize,
    mouse: MouseEvent,
    area: Rect,
) -> Option<Action> {
    let db = &mut app.databases[db_idx];
    match db.bottom_tab {
        BottomTab::Results => db.results.handle_mouse(mouse, area),
        BottomTab::Explain => db.explain.handle_mouse(mouse, area),
        BottomTab::Detail => db.record_detail.handle_mouse(mouse, area),
        BottomTab::ERDiagram => db.er_diagram.handle_mouse(mouse, area),
        BottomTab::Profile => db.profile.handle_mouse(mouse, area),
    }
}

fn handle_drag(mouse: MouseEvent, app: &mut AppState) {
    match mouse.kind {
        MouseEventKind::Drag(_) => {
            if let Some(ref drag) = app.drag_state {
                let active_idx = app.active_db;
                match drag.target {
                    DragTarget::SidebarBorder => {
                        let total_width = app.layout_rects.editor.width
                            + app.layout_rects.sidebar.map_or(0, |r| r.width);
                        if total_width > 0 {
                            let new_pct = (mouse.column * 100 / total_width).clamp(10, 50);
                            #[allow(clippy::cast_possible_wrap)]
                            let delta =
                                new_pct as i16 - app.databases[active_idx].sidebar_pct as i16;
                            if delta != 0 {
                                let action = Action::Ui(UiAction::ResizeSidebar(delta));
                                app.update(&action);
                            }
                        }
                    }
                    DragTarget::EditorBorder => {
                        let total_height =
                            app.layout_rects.editor.height + app.layout_rects.bottom.height;
                        if total_height > 0 {
                            let editor_top = app.layout_rects.editor.y;
                            let relative_y = mouse.row.saturating_sub(editor_top);
                            let new_pct = (relative_y * 100 / total_height).clamp(20, 80);
                            #[allow(clippy::cast_possible_wrap)]
                            let delta =
                                new_pct as i16 - app.databases[active_idx].editor_pct as i16;
                            if delta != 0 {
                                let action = Action::Ui(UiAction::ResizeEditor(delta));
                                app.update(&action);
                            }
                        }
                    }
                    DragTarget::ColumnHeader(col_idx) => {
                        let start_pos = drag.start_pos;
                        let start_value = drag.start_value;
                        #[allow(clippy::cast_possible_wrap)]
                        let delta = mouse.column as i16 - start_pos as i16;
                        let new_width = (start_value as i16 + delta).max(4) as u16;
                        app.databases[active_idx]
                            .results
                            .set_column_width_override(col_idx, new_width);
                    }
                }
            }
        }
        // Both Up and other events end the drag
        _ => {
            app.drag_state = None;
        }
    }
}

fn check_drag_start(col: u16, row: u16, app: &mut AppState, rects: &crate::app::LayoutRects) {
    // Sidebar border: vertical line at sidebar.right()
    if let Some(ref sidebar) = rects.sidebar {
        let border_x = sidebar.x + sidebar.width;
        if col >= border_x.saturating_sub(1)
            && col <= border_x + 1
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
        {
            let active_idx = app.active_db;
            app.drag_state = Some(DragState {
                target: DragTarget::SidebarBorder,
                start_pos: col,
                start_value: app.databases[active_idx].sidebar_pct,
            });
            return;
        }
    }

    // Editor border: horizontal line at editor.y + editor.height
    let border_y = rects.editor.y + rects.editor.height;
    if row >= border_y.saturating_sub(1)
        && row <= border_y + 1
        && col >= rects.editor.x
        && col < rects.editor.x + rects.editor.width
    {
        let active_idx = app.active_db;
        app.drag_state = Some(DragState {
            target: DragTarget::EditorBorder,
            start_pos: row,
            start_value: app.databases[active_idx].editor_pct,
        });
    }
}

/// Resolve a click x-coordinate within a tab bar rect to a tab index.
fn resolve_tab_click(col: u16, rect: Rect, labels: &[String]) -> Option<usize> {
    let relative_x = col.saturating_sub(rect.x) as usize;
    let mut cumulative = 0;
    for (i, label) in labels.iter().enumerate() {
        let label_width = label.width();
        if relative_x < cumulative + label_width {
            return Some(i);
        }
        cumulative += label_width;
    }
    None
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.contains(Position::new(col, row))
}
