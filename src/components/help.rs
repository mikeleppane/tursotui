use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use crate::theme::Theme;

/// Build the help content lines with section headers and keybinding entries.
fn help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let header_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(112);

    // --- Global ---
    lines.push(Line::from(Span::styled("Global", header_style)));
    lines.push(Line::from("  Ctrl+Q          Quit"));
    lines.push(Line::from("  Ctrl+Tab        Cycle focus between panels"));
    lines.push(Line::from("  Ctrl+B          Toggle schema sidebar"));
    lines.push(Line::from("  Alt+1 / Alt+2   Switch Query / Admin tab"));
    lines.push(Line::from("  Ctrl+T          Toggle dark/light theme"));
    lines.push(Line::from("  F1 / ?          Toggle this help overlay"));
    lines.push(Line::from("  Ctrl+Shift+E    Export results popup"));
    lines.push(Line::from("  Ctrl+Shift+C    Quick copy results (TSV)"));
    lines.push(Line::from(""));

    // --- Query Editor ---
    lines.push(Line::from(Span::styled("Query Editor", header_style)));
    lines.push(Line::from("  F5 / Ctrl+Enter Execute query"));
    lines.push(Line::from(
        "  Ctrl+Shift+Enter Execute selection / statement at cursor",
    ));
    lines.push(Line::from("  Ctrl+Z / Ctrl+Y Undo / redo"));
    lines.push(Line::from("  Ctrl+L          Clear editor buffer"));
    lines.push(Line::from("  Ctrl+H          Query history"));
    lines.push(Line::from("  Ctrl+Space      Trigger autocomplete"));
    lines.push(Line::from(
        "  Tab / Shift+Tab Accept autocomplete / indent / dedent",
    ));
    lines.push(Line::from("  Shift+Arrow     Extend selection"));
    lines.push(Line::from("  Ctrl+Shift+A    Select all"));
    lines.push(Line::from("  Ctrl+Arrow      Word movement"));
    lines.push(Line::from(
        "  Esc             Dismiss autocomplete / release focus",
    ));
    lines.push(Line::from(""));

    // --- Schema Explorer ---
    lines.push(Line::from(Span::styled("Schema Explorer", header_style)));
    lines.push(Line::from("  j/k or Up/Down  Navigate tree"));
    lines.push(Line::from("  Enter/Space/l   Expand/collapse"));
    lines.push(Line::from("  h / Left / Bksp Collapse / parent"));
    lines.push(Line::from("  o               Query table (SELECT *)"));
    lines.push(Line::from("  /               Filter by name"));
    lines.push(Line::from(
        "  Esc             Release focus (clears filter first)",
    ));
    lines.push(Line::from(""));

    // --- Results Table ---
    lines.push(Line::from(Span::styled("Results Table", header_style)));
    lines.push(Line::from("  j/k or Up/Down  Navigate rows"));
    lines.push(Line::from("  h/l or Left/Right Navigate columns"));
    lines.push(Line::from("  g / G           First / last row"));
    lines.push(Line::from("  s               Cycle sort on column"));
    lines.push(Line::from("  < / >           Shrink / grow column"));
    lines.push(Line::from("  y               Copy cell to clipboard"));
    lines.push(Line::from("  Y               Copy row to clipboard"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from(""));

    // --- Bottom Panel ---
    lines.push(Line::from(Span::styled("Bottom Panel", header_style)));
    lines.push(Line::from(
        "  1 / 2 / 3 / 4   Switch Results / Explain / Detail / ER",
    ));
    lines.push(Line::from(""));

    // --- EXPLAIN View ---
    lines.push(Line::from(Span::styled("EXPLAIN View", header_style)));
    lines.push(Line::from("  Tab             Toggle Bytecode / Query Plan"));
    lines.push(Line::from("  Enter           Generate EXPLAIN"));
    lines.push(Line::from("  j/k or Up/Down  Scroll rows"));
    lines.push(Line::from("  g / G           Jump to first / last"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from(""));

    // --- Record Detail ---
    lines.push(Line::from(Span::styled("Record Detail", header_style)));
    lines.push(Line::from("  j/k or Up/Down  Scroll fields"));
    lines.push(Line::from("  g / G           Jump to first / last"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from(""));

    // --- Database Info (Admin Tab) ---
    lines.push(Line::from(Span::styled(
        "Database Info (Admin Tab)",
        header_style,
    )));
    lines.push(Line::from("  r               Refresh database info"));
    lines.push(Line::from("  c               WAL checkpoint (passive)"));
    lines.push(Line::from("  i               Run integrity check"));
    lines.push(Line::from("  j/k or Up/Down  Scroll"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from(""));

    // --- PRAGMA Dashboard (Admin Tab) ---
    lines.push(Line::from(Span::styled(
        "PRAGMA Dashboard (Admin Tab)",
        header_style,
    )));
    lines.push(Line::from("  Enter           Edit selected pragma"));
    lines.push(Line::from("  Esc             Cancel edit / release focus"));
    lines.push(Line::from("  r               Refresh all values"));
    lines.push(Line::from("  j/k or Up/Down  Navigate"));
    lines.push(Line::from("  g / G           Jump to first / last"));

    lines
}

/// Render the help overlay as a centered floating popup.
///
/// This is a render function, not a `Component` -- it draws on top of existing content.
pub(crate) fn render(frame: &mut Frame, scroll: usize, theme: &Theme) {
    let area = frame.area();

    // 60% width, 80% height, centered
    let popup_width = area.width * 60 / 100;
    let popup_height = area.height * 80 / 100;
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    let block = Block::bordered()
        .border_style(Style::default().fg(theme.accent))
        .title(" Help (F1 to close) ")
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(theme.bg).fg(theme.fg));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let lines = help_lines(theme);
    let total_lines = lines.len();

    // Clamp scroll so we don't scroll past content
    let visible_height = inner.height as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);
    let clamped_scroll = scroll.min(max_scroll);

    let paragraph = Paragraph::new(lines)
        .scroll((u16::try_from(clamped_scroll).unwrap_or(u16::MAX), 0))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.fg));
    frame.render_widget(paragraph, inner);

    // Scrollbar when content overflows
    if total_lines > visible_height {
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
