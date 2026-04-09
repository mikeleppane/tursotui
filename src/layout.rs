use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Tabs};

use crate::GlobalFeatures;
use crate::app::{AppState, BottomTab, DatabaseContext, PanelId, SubTab};
use crate::components;
use crate::components::Component;
use crate::highlight;
use crate::theme::Theme;

/// Build disambiguated tab labels when duplicate filenames exist.
pub(crate) fn build_tab_labels(databases: &[DatabaseContext]) -> Vec<String> {
    let labels: Vec<String> = databases.iter().map(|db| db.label.clone()).collect();
    let mut result = labels.clone();

    for (i, label) in labels.iter().enumerate() {
        // Check if this label appears more than once
        let count = labels.iter().filter(|l| *l == label).count();
        if count > 1 {
            if label == "[in-memory]" {
                // Disambiguate :memory: databases with sequential index
                let nth = labels[..=i].iter().filter(|l| *l == label).count();
                result[i] = format!("[in-memory] #{nth}");
            } else {
                // Disambiguate file-backed databases with parent directory
                let path = std::path::Path::new(&databases[i].path);
                if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
                    result[i] = format!("{} [{}/]", label, parent.to_string_lossy());
                }
            }
        }
    }
    result
}

#[allow(clippy::too_many_lines)]
pub(crate) fn render_ui(frame: &mut Frame, app: &mut AppState, global_ui: &mut GlobalFeatures) {
    // Copy theme to avoid holding a borrow on app while we mutate databases
    let theme = app.theme;
    let area = frame.area();

    // Minimum terminal size check
    if area.width < 80 || area.height < 24 {
        let msg = Paragraph::new(format!(
            "Terminal is {}x{}, minimum is 80x24",
            area.width, area.height
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(theme.error));
        let [_, center, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        frame.render_widget(msg, center);
        return;
    }

    let multi_db = app.databases.len() > 1;
    let active_idx = app.active_db;
    let global_overlay = app.global_overlay;
    let db_overlay = app.databases[active_idx].db_overlay;
    let help_scroll = app.help_scroll;

    // Top level layout depends on whether we have multiple databases
    let (db_tabs_area, sub_tabs_area, content_area, status_area) = if multi_db {
        let [db_tabs, sub_tabs, content, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        (Some(db_tabs), sub_tabs, content, status)
    } else {
        let [sub_tabs, content, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        (None, sub_tabs, content, status)
    };

    // Store layout rects for mouse hit-testing
    app.layout_rects.status_bar = status_area;
    app.layout_rects.sub_tabs = sub_tabs_area;
    app.layout_rects.db_tabs = db_tabs_area;

    // Database tab bar (only when multiple databases open)
    if let Some(db_tabs_area) = db_tabs_area {
        let tab_labels = build_tab_labels(&app.databases);
        let mut tab_items: Vec<String> = tab_labels
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
        tab_items.push(" [+] ".to_string());

        let db_tabs = Tabs::new(tab_items)
            .select(active_idx)
            .style(Style::default().fg(theme.dim).bg(theme.mantle))
            .highlight_style(
                Style::default()
                    .fg(theme.accent2)
                    .bg(theme.mantle)
                    .add_modifier(Modifier::BOLD),
            )
            .divider("");
        frame.render_widget(db_tabs, db_tabs_area);
    }

    // Sub-tab bar — surface0 background, accent underlined active tab
    let sub_tab_index = match app.databases[active_idx].sub_tab {
        SubTab::Query => 0,
        SubTab::Admin => 1,
    };
    let sub_tabs = Tabs::new(vec![" Query ", " Admin "])
        .select(sub_tab_index)
        .style(Style::default().fg(theme.dim).bg(theme.surface0))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .bg(theme.surface0)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::styled(
            " \u{2502} ",
            Style::default().fg(theme.border),
        ));
    frame.render_widget(sub_tabs, sub_tabs_area);

    // Content area — needs &mut access to database for component rendering
    let sub_tab = app.databases[active_idx].sub_tab;
    let focus = app.databases[active_idx].focus;
    let sidebar_visible = app.databases[active_idx].sidebar_visible;
    let bottom_tab = app.databases[active_idx].bottom_tab;
    match sub_tab {
        SubTab::Query => {
            render_query_tab(
                frame,
                &theme,
                content_area,
                focus,
                sidebar_visible,
                bottom_tab,
                &mut app.databases[active_idx],
                &mut app.layout_rects,
            );
        }
        SubTab::Admin => {
            // Reset layout rects for admin tab (no sidebar/editor/bottom)
            app.layout_rects.sidebar = None;
            app.layout_rects.editor = Rect::default();
            app.layout_rects.bottom = Rect::default();
            app.layout_rects.bottom_content = Rect::default();
            app.layout_rects.bottom_tabs = Rect::default();
            render_admin_tab(
                frame,
                &theme,
                content_area,
                focus,
                &mut app.databases[active_idx],
                &mut app.layout_rects,
            );
        }
    }

    // Status bar — needs &AppState for read access
    let de_status = app.databases[active_idx].data_editor.status();
    let selected_row = app.databases[active_idx].results.selected_row();
    let row_count = app.databases[active_idx].results.row_count();
    components::status_bar::render(
        frame,
        status_area,
        app,
        selected_row,
        row_count,
        &theme,
        &de_status,
    );

    // JSON overlay (renders on top of everything except help)
    if app.databases[active_idx].bottom_tab == BottomTab::Detail
        && app.databases[active_idx].record_detail.has_overlay()
    {
        app.databases[active_idx]
            .record_detail
            .render_overlay(frame, area, &theme);
    }

    // --- Per-db overlays (rendered first, below global) ---

    // ER Diagram fullscreen overlay
    if db_overlay == Some(crate::app::DbOverlay::ERDiagram) {
        let full = frame.area();
        let popup_w = full.width * 95 / 100;
        let popup_h = full.height * 95 / 100;
        let x = (full.width.saturating_sub(popup_w)) / 2;
        let y = (full.height.saturating_sub(popup_h)) / 2;
        let popup_area = Rect::new(x, y, popup_w, popup_h);

        frame.render_widget(Clear, popup_area);
        let block = components::overlay_block("ER Diagram (fullscreen)", &theme);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let db = &mut app.databases[active_idx];
        db.er_diagram.is_fullscreen = true;
        db.er_diagram.render(frame, inner, true, &theme);
        db.er_diagram.is_fullscreen = false;
    }

    // Export overlay
    if db_overlay == Some(crate::app::DbOverlay::Export)
        && let Some(ref popup) = app.databases[active_idx].export_popup
    {
        popup.render(frame, area, &theme);
    }

    // DML preview overlay
    if db_overlay == Some(crate::app::DbOverlay::DmlPreview) {
        let submit_enabled = app.databases[active_idx].dml_submit_enabled;
        components::dml_preview::render_dml_preview(
            frame,
            area,
            app.databases[active_idx].data_editor.preview_dml(),
            app.databases[active_idx].data_editor.preview_scroll(),
            submit_enabled,
            &theme,
        );
    }

    // DDL viewer overlay
    if db_overlay == Some(crate::app::DbOverlay::DdlViewer)
        && let Some(ref viewer) = app.databases[active_idx].ddl_viewer
    {
        let full = frame.area();
        let popup_w = full.width * 70 / 100;
        let popup_h = full.height * 80 / 100;
        let x = (full.width.saturating_sub(popup_w)) / 2;
        let y = (full.height.saturating_sub(popup_h)) / 2;
        let popup_area = Rect::new(x, y, popup_w, popup_h);

        frame.render_widget(Clear, popup_area);
        let title = format!("DDL \u{2014} {}", viewer.object_name);
        let block = components::overlay_block(&title, &theme);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let lines: Vec<Line> = viewer
            .sql
            .lines()
            .map(|line| highlight::highlight_line(line, &theme))
            .collect();
        let total_lines = lines.len();
        let max_scroll = total_lines.saturating_sub(inner.height as usize);
        let effective_scroll = viewer.scroll.min(max_scroll);
        let para = Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((effective_scroll as u16, 0));
        frame.render_widget(para, inner);
    }

    // --- Global overlays (rendered on top) ---

    // History overlay
    if global_overlay == Some(crate::app::GlobalOverlay::History) {
        global_ui.history.render(frame, area, &theme);
    }

    // Bookmarks overlay
    if global_overlay == Some(crate::app::GlobalOverlay::Bookmarks) {
        let db = &app.databases[active_idx];
        global_ui
            .bookmarks
            .set_editor_content(&db.editor.contents());
        global_ui.bookmarks.set_database_path(&db.path);
        let full = frame.area();
        let popup_w = full.width * 70 / 100;
        let popup_h = full.height * 80 / 100;
        let x = (full.width.saturating_sub(popup_w)) / 2;
        let y = (full.height.saturating_sub(popup_h)) / 2;
        let popup_area = Rect::new(x, y, popup_w, popup_h);
        frame.render_widget(Clear, popup_area);
        global_ui.bookmarks.render(frame, popup_area, &theme);
    }

    // File picker overlay
    if global_overlay == Some(crate::app::GlobalOverlay::FilePicker)
        && let Some(ref picker) = global_ui.file_picker
    {
        picker.render(frame, area, &theme);
    }

    // Go to Object overlay
    if global_overlay == Some(crate::app::GlobalOverlay::GoToObject)
        && let Some(ref goto) = global_ui.goto_object
    {
        goto.render(frame, area, &theme);
    }

    // Schema diff overlay
    if global_overlay == Some(crate::app::GlobalOverlay::SchemaDiff)
        && let Some(ref mut diff_state) = app.schema_diff_state
    {
        components::schema_diff::render(frame, diff_state, &theme);
    }

    // Modal cell editor overlay (renders above content, below help)
    if let Some(editor) = app.databases[active_idx].data_editor.cell_editor()
        && editor.modal
    {
        let table = app.databases[active_idx]
            .data_editor
            .source_table()
            .unwrap_or("table");
        let col_name = app.databases[active_idx]
            .data_editor
            .columns()
            .get(editor.col)
            .map_or("col", |c| c.name.as_str());
        editor.render_modal(frame, area, table, col_name, &theme);
    }

    // Help overlay (rendered last so it floats on top)
    if global_overlay == Some(crate::app::GlobalOverlay::Help) {
        components::help::render(frame, help_scroll, &theme);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_query_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    sidebar_visible: bool,
    bottom_tab: BottomTab,
    db: &mut DatabaseContext,
    rects: &mut crate::app::LayoutRects,
) {
    if sidebar_visible {
        let [sidebar_area, main_area] = Layout::horizontal([
            Constraint::Percentage(db.sidebar_pct),
            Constraint::Percentage(100 - db.sidebar_pct),
        ])
        .areas(area);

        rects.sidebar = Some(sidebar_area);

        db.schema.set_row_counts(&db.schema_cache.row_counts);
        db.schema
            .render(frame, sidebar_area, focus == PanelId::Schema, theme);

        let [editor_area, bottom_area] = Layout::vertical([
            Constraint::Percentage(db.editor_pct),
            Constraint::Percentage(100 - db.editor_pct),
        ])
        .areas(main_area);

        rects.editor = editor_area;
        rects.bottom = bottom_area;

        db.editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        render_autocomplete_popup(frame, &db.editor, editor_area, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, db, rects);
    } else {
        rects.sidebar = None;

        let [editor_area, bottom_area] = Layout::vertical([
            Constraint::Percentage(db.editor_pct),
            Constraint::Percentage(100 - db.editor_pct),
        ])
        .areas(area);

        rects.editor = editor_area;
        rects.bottom = bottom_area;

        db.editor
            .render(frame, editor_area, focus == PanelId::Editor, theme);
        render_autocomplete_popup(frame, &db.editor, editor_area, theme);
        render_bottom_panel(frame, theme, bottom_area, focus, bottom_tab, db, rects);
    }
}

/// Render the autocomplete popup over the editor if active.
pub(crate) fn render_autocomplete_popup(
    frame: &mut Frame,
    editor: &components::editor::QueryEditor,
    editor_area: Rect,
    theme: &Theme,
) {
    let Some(ref popup) = editor.autocomplete_popup else {
        return;
    };
    let (cursor_row, cursor_col) = editor.cursor_position();
    let line_count = editor.buffer_lines().len();
    let gutter_digits = line_count.to_string().len();
    let gutter_width = (gutter_digits + 1) as u16;
    // editor_area inner (subtract border)
    let inner_x = editor_area.x + 1 + gutter_width;
    let inner_y = editor_area.y + 1;
    // Use char index (not display width) to match the editor's own cursor
    // placement at editor.rs:920. Both are wrong for wide chars (CJK/emoji)
    // but must stay consistent so the popup aligns with the terminal cursor.
    let cursor_x = inner_x + cursor_col as u16;
    let cursor_y = inner_y + cursor_row.saturating_sub(editor.scroll_offset()) as u16;
    popup.render(frame, cursor_x, cursor_y, theme);
}

pub(crate) fn render_bottom_panel(
    frame: &mut Frame,
    theme: &Theme,
    bottom_area: Rect,
    focus: PanelId,
    bottom_tab: BottomTab,
    db: &mut DatabaseContext,
    rects: &mut crate::app::LayoutRects,
) {
    let [bottom_tabs_area, bottom_content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(bottom_area);
    rects.bottom_tabs = bottom_tabs_area;
    rects.bottom_content = bottom_content_area;

    // Render bottom sub-tab bar
    let tab_index = match bottom_tab {
        BottomTab::Results => 0,
        BottomTab::Explain => 1,
        BottomTab::Detail => 2,
        BottomTab::ERDiagram => 3,
        BottomTab::Profile => 4,
    };
    let profile_label = if db.profile.is_stale() {
        " 5:Profile* "
    } else {
        " 5:Profile "
    };
    let bottom_tabs = Tabs::new(vec![
        " 1:Results ",
        " 2:Explain ",
        " 3:Detail ",
        " 4:ER ",
        profile_label,
    ])
    .select(tab_index)
    .style(Style::default().fg(theme.dim).bg(theme.surface0))
    .highlight_style(
        Style::default()
            .fg(theme.accent)
            .bg(theme.surface0)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    )
    .divider(Span::styled("\u{2502}", Style::default().fg(theme.border)));
    frame.render_widget(bottom_tabs, bottom_tabs_area);

    // Inject edit state into ResultsTable before rendering
    if db.data_editor.is_active() {
        db.results
            .set_edit_state(Some(db.data_editor.build_render_state()));
    } else {
        db.results.set_edit_state(None);
    }

    // Render the active bottom component
    let is_focused = focus == PanelId::Bottom;
    match bottom_tab {
        BottomTab::Results => {
            db.results
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::Explain => {
            db.explain
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::Detail => {
            db.record_detail
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::ERDiagram => {
            db.er_diagram
                .render(frame, bottom_content_area, is_focused, theme);
        }
        BottomTab::Profile => {
            db.profile
                .render(frame, bottom_content_area, is_focused, theme);
        }
    }
}

pub(crate) fn render_admin_tab(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    focus: PanelId,
    db: &mut DatabaseContext,
    rects: &mut crate::app::LayoutRects,
) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

    rects.admin_left = left;
    rects.admin_right = right;

    db.db_info
        .render(frame, left, focus == PanelId::DbInfo, theme);
    db.pragmas
        .render(frame, right, focus == PanelId::Pragmas, theme);
}
