use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use crate::theme::Theme;

/// Build the help content lines with section headers and keybinding entries.
#[allow(clippy::too_many_lines)]
fn help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let header_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(138);

    // --- Global ---
    lines.push(Line::from(Span::styled("Global", header_style)));
    lines.push(Line::from("  Ctrl+Q          Quit"));
    lines.push(Line::from("  Ctrl+Tab        Cycle focus between panels"));
    lines.push(Line::from("  Ctrl+B          Toggle schema sidebar"));
    lines.push(Line::from("  Alt+1 / Alt+2   Switch Query / Admin tab"));
    lines.push(Line::from("  Ctrl+T          Toggle dark/light theme"));
    lines.push(Line::from("  F1 / ?          Toggle this help overlay"));
    lines.push(Line::from("  F3              Bookmarks"));
    lines.push(Line::from("  Ctrl+Shift+E    Export results popup"));
    lines.push(Line::from("  Ctrl+Shift+C    Quick copy results (TSV)"));
    lines.push(Line::from("  Ctrl+M          Toggle mouse mode"));
    lines.push(Line::from(
        "  Shift+Click     Terminal text selection (when mouse enabled)",
    ));
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

    // --- Parameter Bar ---
    lines.push(Line::from(Span::styled(
        "Parameter Bar (when ?1, :name etc. detected)",
        header_style,
    )));
    lines.push(Line::from(
        "  Tab             Focus param bar / next field (overrides indent)",
    ));
    lines.push(Line::from("  Shift+Tab       Previous field"));
    lines.push(Line::from("  Ctrl+N          Set field to NULL"));
    lines.push(Line::from("  Esc             Return focus to editor"));
    lines.push(Line::from(
        "  (values auto-coerce: 42 → integer, 3.14 → real, else text)",
    ));
    lines.push(Line::from(""));

    // --- Schema Explorer ---
    lines.push(Line::from(Span::styled("Schema Explorer", header_style)));
    lines.push(Line::from("  j/k or Up/Down  Navigate tree"));
    lines.push(Line::from("  Enter/Space/l   Expand/collapse"));
    lines.push(Line::from("  h / Left / Bksp Collapse / parent"));
    lines.push(Line::from("  o               Query table (SELECT *)"));
    lines.push(Line::from("  /               Filter by name"));
    lines.push(Line::from("  Shift+D         View DDL"));
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
    lines.push(Line::from("  w               WHERE filter"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from("  (· after column name = leading index column)"));
    lines.push(Line::from(""));

    // --- Data Editor ---
    lines.push(Line::from(Span::styled(
        "Data Editor (when results are editable)",
        header_style,
    )));
    lines.push(Line::from("  e / F2          Edit current cell"));
    lines.push(Line::from(
        "  Enter           Confirm cell edit / Record Detail",
    ));
    lines.push(Line::from("  Esc             Cancel cell edit"));
    lines.push(Line::from("  Ctrl+N          Set cell to NULL"));
    lines.push(Line::from("  Ctrl+Enter / F10  Confirm modal edit"));
    lines.push(Line::from("  a               Add new row"));
    lines.push(Line::from("  d               Toggle delete mark"));
    lines.push(Line::from("  c               Clone row"));
    lines.push(Line::from("  u               Revert current cell"));
    lines.push(Line::from("  U               Revert current row"));
    lines.push(Line::from("  Ctrl+U          Revert all changes"));
    lines.push(Line::from("  Ctrl+D          Preview DML"));
    lines.push(Line::from("  Ctrl+S          Submit changes"));
    lines.push(Line::from("  f               Follow FK reference"));
    lines.push(Line::from("  Alt+\u{2190}           FK back-navigation"));
    lines.push(Line::from(""));

    // --- Bottom Panel ---
    lines.push(Line::from(Span::styled("Bottom Panel", header_style)));
    lines.push(Line::from(
        "  1-5             Switch Results / Explain / Detail / ER / Profile",
    ));
    lines.push(Line::from(""));

    // --- ER Diagram ---
    lines.push(Line::from(Span::styled("ER Diagram", header_style)));
    lines.push(Line::from(
        "  h/\u{2190}, l/\u{2192}, k/\u{2191}, j/\u{2193}  Pan viewport",
    ));
    lines.push(Line::from(
        "  + / -            Zoom in / out (Overview \u{2192} Normal \u{2192} Detail)",
    ));
    lines.push(Line::from("  Tab / Shift+Tab  Cycle focus between tables"));
    lines.push(Line::from(
        "  Enter            Expand/collapse table columns (Normal zoom)",
    ));
    lines.push(Line::from(
        "  o                Open focused table in query editor",
    ));
    lines.push(Line::from(
        "  c                Center viewport (focused table or fit all)",
    ));
    lines.push(Line::from("  f / F6           Toggle fullscreen mode"));
    lines.push(Line::from(""));

    // --- EXPLAIN View ---
    lines.push(Line::from(Span::styled("EXPLAIN View", header_style)));
    lines.push(Line::from("  Tab             Toggle Bytecode / Query Plan"));
    lines.push(Line::from(
        "  Enter           Generate EXPLAIN / send suggestion to editor",
    ));
    lines.push(Line::from(
        "  y               Copy index suggestion to clipboard",
    ));
    lines.push(Line::from("  j/k or Up/Down  Scroll rows"));
    lines.push(Line::from("  g / G           Jump to first / last"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from(
        "  (plan lines color-coded: red=full scan, yellow=temp/subquery, green=index)",
    ));
    lines.push(Line::from(""));

    // --- Record Detail ---
    lines.push(Line::from(Span::styled("Record Detail", header_style)));
    lines.push(Line::from("  j/k or Up/Down  Scroll fields"));
    lines.push(Line::from("  g / G           Jump to first / last"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from(""));

    // --- Profile View ---
    lines.push(Line::from(Span::styled("Profile View", header_style)));
    lines.push(Line::from("  Enter           Generate profile"));
    lines.push(Line::from("  r               Refresh profile"));
    lines.push(Line::from("  j/k or Up/Down  Navigate columns"));
    lines.push(Line::from("  g / G           Jump to first / last column"));
    lines.push(Line::from("  Esc             Release focus"));
    lines.push(Line::from(
        "  (completeness: \u{25cf}=0% null, \u{25d0}=<50% null, \u{25cb}=\u{2265}50% null, \u{2205}=all null)",
    ));
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
    lines.push(Line::from(""));

    // --- Schema Diff ---
    lines.push(Line::from(Span::styled("Schema Diff", header_style)));
    lines.push(Line::from(
        "  F7              Compare schemas (2+ databases)",
    ));
    lines.push(Line::from("  j/k or Up/Down  Navigate objects"));
    lines.push(Line::from("  Enter           Expand/collapse column diffs"));
    lines.push(Line::from("  g / G           Jump to first / last"));
    lines.push(Line::from("  i               Toggle identical objects"));
    lines.push(Line::from("  y               Copy DDL to clipboard"));
    lines.push(Line::from(
        "  m               Copy migration SQL to clipboard",
    ));
    lines.push(Line::from("  Esc             Close overlay"));
    lines.push(Line::from(""));

    // --- Multi-Database ---
    lines.push(Line::from(Span::styled("Multi-Database", header_style)));
    lines.push(Line::from("  Ctrl+PgDn       Switch to next database tab"));
    lines.push(Line::from(
        "  Ctrl+PgUp       Switch to previous database tab",
    ));
    lines.push(Line::from("  Ctrl+W          Close current database tab"));
    lines.push(Line::from("  Ctrl+O          Open database file"));
    lines.push(Line::from("  Ctrl+P          Go to Object (fuzzy search)"));
    lines.push(Line::from(
        "  Ctrl+←/→        Resize sidebar (narrower/wider)",
    ));
    lines.push(Line::from(
        "  Ctrl+↑/↓        Resize editor (shorter/taller)",
    ));

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

    let block = super::overlay_block("Help (F1 to close)", theme);

    let inner = block.inner(popup_area);
    frame.render_widget(Clear, popup_area);
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
