use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::highlight::highlight_line;
use crate::theme::Theme;

/// Render the DML preview popup as a centered floating popup.
///
/// This is a render function, not a `Component` — it draws on top of existing content.
#[allow(clippy::too_many_lines)]
pub(crate) fn render_dml_preview(
    frame: &mut Frame,
    area: Rect,
    statements: &[String],
    scroll: usize,
    submit_enabled: bool,
    theme: &Theme,
) {
    // 80% width, 70% height, centered
    let popup_width = (area.width * 80 / 100).max(40);
    let popup_height = (area.height * 70 / 100).max(10);
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    let n = statements.len();
    let title = format!(
        "DML Preview ({n} statement{})",
        if n == 1 { "" } else { "s" }
    );

    let block = super::overlay_block(&title, theme);

    let inner = block.inner(popup_area);
    frame.render_widget(Clear, popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Compute statement type counts
    let mut n_updates = 0usize;
    let mut n_inserts = 0usize;
    let mut n_deletes = 0usize;
    for stmt in statements {
        let upper = stmt.trim_start().to_uppercase();
        if upper.starts_with("UPDATE") {
            n_updates += 1;
        } else if upper.starts_with("INSERT") {
            n_inserts += 1;
        } else if upper.starts_with("DELETE") {
            n_deletes += 1;
        }
    }

    // Build content lines, grouped by type
    let comment_style = theme.sql_comment;
    let mut lines: Vec<Line<'static>> = Vec::new();

    let add_group = |lines: &mut Vec<Line<'static>>, header: String, stmts: Vec<String>| {
        if stmts.is_empty() {
            return;
        }
        lines.push(Line::from(Span::styled(header, comment_style)));
        for stmt in stmts {
            lines.push(highlight_line(&stmt, theme));
            lines.push(Line::from(""));
        }
    };

    let updates: Vec<String> = statements
        .iter()
        .filter(|s| s.trim_start().to_uppercase().starts_with("UPDATE"))
        .cloned()
        .collect();
    let inserts: Vec<String> = statements
        .iter()
        .filter(|s| s.trim_start().to_uppercase().starts_with("INSERT"))
        .cloned()
        .collect();
    let deletes: Vec<String> = statements
        .iter()
        .filter(|s| s.trim_start().to_uppercase().starts_with("DELETE"))
        .cloned()
        .collect();

    add_group(&mut lines, format!("-- {n_updates} UPDATE(s)"), updates);
    add_group(&mut lines, format!("-- {n_inserts} INSERT(s)"), inserts);
    add_group(&mut lines, format!("-- {n_deletes} DELETE(s)"), deletes);

    // Summary line
    let summary = format!(
        "{n} statement{} ({n_updates} update, {n_inserts} insert, {n_deletes} delete)",
        if n == 1 { "" } else { "s" }
    );
    lines.push(Line::from(Span::styled(
        summary,
        Style::default()
            .fg(theme.border)
            .add_modifier(Modifier::DIM),
    )));

    // Footer key hint
    let footer = if submit_enabled {
        "  [Enter] Submit  [Esc] Cancel  [j/k] Scroll"
    } else {
        "  [Esc] Close  [j/k] Scroll"
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(theme.accent),
    )));

    let total_lines = lines.len();

    // Reserve last 2 lines of the inner area for summary + footer
    // Content area = inner minus the last 2 lines for footer display
    let content_height = inner.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(content_height);
    let clamped_scroll = scroll.min(max_scroll);

    let paragraph = Paragraph::new(lines)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((u16::try_from(clamped_scroll).unwrap_or(u16::MAX), 0))
        .style(Style::default().fg(theme.fg));
    frame.render_widget(paragraph, inner);

    // Scrollbar when content overflows
    if total_lines > content_height {
        let scrollbar_area = Rect {
            x: popup_area.x + popup_area.width - 1,
            y: popup_area.y + 1,
            width: 1,
            height: popup_area.height.saturating_sub(2),
        };
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll.saturating_add(1)).position(clamped_scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}
