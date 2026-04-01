use ratatui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::GlobalFeatures;
use crate::app::{self, AppState, BottomTab, PanelId};
use crate::components::Component;
use crate::dispatch;
use crate::event;

/// Handle a single key press: route to help overlay, focused component, or global handler.
#[allow(clippy::too_many_lines)]
pub(crate) fn handle_key_event(
    key: ratatui::crossterm::event::KeyEvent,
    app: &mut AppState,
    global_ui: &mut GlobalFeatures,
) {
    let active_idx = app.active_db;

    // Phase 1: Global overlays take priority
    if let Some(global) = app.global_overlay {
        match global {
            app::GlobalOverlay::Help => {
                handle_help_key(key, app);
                return;
            }
            app::GlobalOverlay::History => {
                if let Some(action) = global_ui.history.handle_key(key) {
                    app.update(&action);
                    app.databases[active_idx].broadcast_update(&action);
                    dispatch::dispatch_action_to_db(active_idx, &action, app, global_ui);
                }
                return;
            }
            app::GlobalOverlay::Bookmarks => {
                if let Some(action) = global_ui.bookmarks.handle_key(key) {
                    app.update(&action);
                    app.databases[active_idx].broadcast_update(&action);
                    dispatch::dispatch_action_to_db(active_idx, &action, app, global_ui);
                }
                return;
            }
            app::GlobalOverlay::FilePicker => {
                if let Some(ref mut picker) = global_ui.file_picker
                    && let Some(action) = picker.handle_key(key)
                {
                    match &action {
                        app::Action::Nav(app::NavAction::OpenDatabase(_)) => {
                            // Dispatch OpenDatabase; picker dismissal happens on
                            // success inside dispatch_action_to_db.
                            app.update(&action);
                            app.databases[active_idx].broadcast_update(&action);
                            dispatch::dispatch_action_to_db(active_idx, &action, app, global_ui);
                        }
                        app::Action::Nav(app::NavAction::OpenFilePicker) => {
                            // Esc — toggle off via update()
                            app.update(&action);
                            global_ui.file_picker = None;
                        }
                        app::Action::Quit => {
                            app.should_quit = true;
                        }
                        _ => {}
                    }
                }
                return;
            }
            app::GlobalOverlay::GoToObject => {
                if let Some(ref mut goto) = global_ui.goto_object {
                    let active_db_path = app.databases[active_idx].path.clone();
                    if let Some(action) = goto.handle_key(key, &app.databases, &active_db_path) {
                        match &action {
                            app::Action::Nav(app::NavAction::GoToObject(obj_ref)) => {
                                let obj_ref_clone = obj_ref.clone();
                                app.global_overlay = None;
                                global_ui.goto_object = None;
                                app.update(&action);
                                // After switching database, reveal_and_select on the target db
                                let target_idx = app.active_db;
                                let db = &mut app.databases[target_idx];
                                db.schema
                                    .reveal_and_select(&obj_ref_clone.name, obj_ref_clone.kind);
                                // Ensure sidebar is visible so user can see the selection
                                if !db.sidebar_visible {
                                    db.sidebar_visible = true;
                                }
                                db.focus = PanelId::Schema;
                            }
                            app::Action::Nav(app::NavAction::OpenGoToObject) => {
                                // Toggle off (Esc or Ctrl+P)
                                app.global_overlay = None;
                                global_ui.goto_object = None;
                            }
                            _ => {
                                app.update(&action);
                                app.databases[active_idx].broadcast_update(&action);
                                dispatch::dispatch_action_to_db(
                                    active_idx, &action, app, global_ui,
                                );
                            }
                        }
                    }
                }
                return;
            }
            app::GlobalOverlay::SchemaDiff => {
                if let Some(ref mut diff_state) = app.schema_diff_state
                    && let Some(action) =
                        crate::components::schema_diff::handle_key(diff_state, key)
                {
                    app.update(&action);
                    app.databases[active_idx].broadcast_update(&action);
                    dispatch::dispatch_action_to_db(active_idx, &action, app, global_ui);
                }
                return;
            }
        }
    }

    // Phase 2: Per-database overlays
    if let Some(db_overlay) = app.databases[active_idx].db_overlay {
        match db_overlay {
            app::DbOverlay::Export => {
                let db = &mut app.databases[active_idx];
                if let Some(ref mut popup) = db.export_popup
                    && let Some(action) = popup.handle_key(key)
                {
                    if matches!(&action, app::Action::Ui(app::UiAction::ExecuteExport)) {
                        dispatch::execute_export(app, global_ui);
                        app.databases[active_idx].db_overlay = None;
                        app.databases[active_idx].export_popup = None;
                    } else {
                        app.update(&action);
                        app.databases[active_idx].broadcast_update(&action);
                        dispatch::dispatch_action_to_db(active_idx, &action, app, global_ui);
                    }
                }
                return;
            }
            app::DbOverlay::DmlPreview => {
                let submit_enabled = app.databases[active_idx].dml_submit_enabled;
                let db = &mut app.databases[active_idx];
                match key.code {
                    KeyCode::Esc => {
                        db.db_overlay = None;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        db.data_editor.scroll_preview_down();
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        db.data_editor.scroll_preview_up();
                    }
                    KeyCode::Enter if submit_enabled => {
                        let action = app::Action::Data(app::DataAction::SubmitDataEdits);
                        app.update(&action);
                        app.databases[active_idx].broadcast_update(&action);
                        dispatch::dispatch_action_to_db(active_idx, &action, app, global_ui);
                    }
                    _ => {}
                }
                return;
            }
            app::DbOverlay::DdlViewer => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.databases[active_idx].db_overlay = None;
                        app.databases[active_idx].ddl_viewer = None;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        if let Some(ref mut viewer) = app.databases[active_idx].ddl_viewer {
                            // With word wrapping, wrapped line count exceeds raw line count.
                            // Use a generous upper bound; render-time clamp refines it.
                            let max = viewer.sql.len();
                            viewer.scroll = viewer.scroll.saturating_add(1).min(max);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if let Some(ref mut viewer) = app.databases[active_idx].ddl_viewer {
                            viewer.scroll = viewer.scroll.saturating_sub(1);
                        }
                    }
                    KeyCode::Char('y') => {
                        if let Some(ref viewer) = app.databases[active_idx].ddl_viewer {
                            if let Some(ref mut clip) = global_ui.clipboard {
                                let _ = clip.set_text(viewer.sql.clone());
                            }
                            let action = app::Action::SetTransient(
                                "DDL copied to clipboard".to_string(),
                                false,
                            );
                            app.update(&action);
                        }
                    }
                    _ => {}
                }
                return;
            }
            app::DbOverlay::ERDiagram => {
                if key.kind == KeyEventKind::Press {
                    if let (KeyModifiers::NONE, KeyCode::Esc | KeyCode::Char('f') | KeyCode::F(6)) =
                        (key.modifiers, key.code)
                    {
                        app.databases[active_idx].db_overlay = None;
                    } else {
                        let db = &mut app.databases[active_idx];
                        if let Some(action) = db.er_diagram.handle_key(key) {
                            app.update(&action);
                            app.databases[active_idx].broadcast_update(&action);
                            dispatch::dispatch_action_to_db(active_idx, &action, app, global_ui);
                        }
                    }
                }
                return;
            }
        }
    }

    // Route to focused component first
    let focused = app.active_db().focus;
    let component_action = route_key_to_component(key, focused, app);

    let action = component_action.or_else(|| event::map_global_key(key));
    if let Some(ref action) = action {
        app.update(action);
        app.databases[app.active_db].broadcast_update(action);
        dispatch::dispatch_action_to_db(app.active_db, action, app, global_ui);
    }

    // Refresh or auto-trigger autocomplete after buffer-modifying keys
    // (typing, backspace, delete). Navigation keys (Up/Down/Esc/Tab) are handled
    // by the popup interceptor and should NOT trigger a refresh.
    let buffer_changed = matches!(
        (key.modifiers, key.code),
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(_))
            | (KeyModifiers::NONE, KeyCode::Backspace | KeyCode::Delete)
    );
    if buffer_changed && app.active_db().focus == PanelId::Editor {
        let db = &mut app.databases[app.active_db];
        let schema = &db.schema_cache;
        if db.editor.autocomplete_popup.is_some() {
            db.editor.refresh_autocomplete(schema);
        } else if db.editor.autocomplete_enabled() {
            // Auto-trigger: open the popup when enabled and the user types
            // enough characters to meet the min_chars threshold.
            db.editor.auto_trigger_autocomplete(schema);
        }
    }
}

/// Handle key events when the help overlay is visible.
fn handle_help_key(key: ratatui::crossterm::event::KeyEvent, app: &mut AppState) {
    match key.code {
        KeyCode::F(1) | KeyCode::Esc | KeyCode::Char('?') => {
            app.global_overlay = None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.help_scroll = app.help_scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.help_scroll = app.help_scroll.saturating_sub(1);
        }
        KeyCode::Char('g') => {
            app.help_scroll = 0;
        }
        KeyCode::Char('G') => {
            app.help_scroll = usize::MAX; // clamped in render
        }
        KeyCode::Char('q') if key.modifiers == KeyModifiers::CONTROL => {
            app.should_quit = true;
        }
        _ => {}
    }
}

/// Route a key event to the appropriate focused component.
/// When Bottom is focused, number keys switch sub-tabs; other keys go to the active bottom component.
fn route_key_to_component(
    key: ratatui::crossterm::event::KeyEvent,
    focused: PanelId,
    app: &mut AppState,
) -> Option<app::Action> {
    let db = app.active_db_mut();
    if focused == PanelId::Bottom {
        // Filter bar and cell editor take absolute priority over tab switching
        if db.bottom_tab == BottomTab::Results {
            if db.results.filter_bar_active {
                return db.results.handle_key(key);
            }
            if db.data_editor.is_active() && db.data_editor.cell_editor().is_some() {
                return db.data_editor.handle_key(key);
            }
        }
        match key.code {
            KeyCode::Char('1') if key.modifiers == KeyModifiers::NONE => Some(app::Action::Nav(
                app::NavAction::SwitchBottomTab(BottomTab::Results),
            )),
            KeyCode::Char('2') if key.modifiers == KeyModifiers::NONE => Some(app::Action::Nav(
                app::NavAction::SwitchBottomTab(BottomTab::Explain),
            )),
            KeyCode::Char('3') if key.modifiers == KeyModifiers::NONE => Some(app::Action::Nav(
                app::NavAction::SwitchBottomTab(BottomTab::Detail),
            )),
            KeyCode::Char('4') if key.modifiers == KeyModifiers::NONE => Some(app::Action::Nav(
                app::NavAction::SwitchBottomTab(BottomTab::ERDiagram),
            )),
            KeyCode::Char('5') if key.modifiers == KeyModifiers::NONE => Some(app::Action::Nav(
                app::NavAction::SwitchBottomTab(BottomTab::Profile),
            )),
            _ => match db.bottom_tab {
                BottomTab::Results => {
                    // DataEditor intercepts before ResultsTable when active
                    if db.data_editor.is_active()
                        && let Some(action) = db.data_editor.handle_key(key)
                    {
                        return Some(action);
                    }
                    db.results.handle_key(key)
                }
                BottomTab::Explain => db.explain.handle_key(key),
                BottomTab::Detail => db.record_detail.handle_key(key),
                BottomTab::ERDiagram => db.er_diagram.handle_key(key),
                BottomTab::Profile => db.profile.handle_key(key),
            },
        }
    } else {
        match focused {
            PanelId::Schema => db.schema.handle_key(key),
            PanelId::Editor => db.editor.handle_key(key),
            PanelId::Bottom => unreachable!(), // handled by outer `if focused == PanelId::Bottom`
            PanelId::DbInfo => db.db_info.handle_key(key),
            PanelId::Pragmas => db.pragmas.handle_key(key),
        }
    }
}
