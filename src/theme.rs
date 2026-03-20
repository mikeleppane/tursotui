use ratatui::style::{Color, Modifier, Style};

// Catppuccin Mocha palette
const BASE: Color = Color::Rgb(30, 30, 46);
const SURFACE0: Color = Color::Rgb(49, 50, 68);
const TEXT: Color = Color::Rgb(205, 214, 244);
const SUBTEXT0: Color = Color::Rgb(88, 91, 112);
const BLUE: Color = Color::Rgb(137, 180, 250);
const RED: Color = Color::Rgb(243, 139, 168);
const GREEN: Color = Color::Rgb(166, 227, 161);
const YELLOW: Color = Color::Rgb(249, 226, 175);
const TEAL: Color = Color::Rgb(148, 226, 213);

/// Visual theme for the entire application.
/// Every styled element references a field here — no hardcoded colors elsewhere.
#[allow(
    dead_code,
    reason = "fields used incrementally as components are added"
)]
pub(crate) struct Theme {
    // Base
    pub bg: Color,
    pub fg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub accent: Color,
    pub error: Color,
    pub success: Color,
    pub warning: Color,

    // Components
    pub null_style: Style,
    pub header_style: Style,
    pub selected_style: Style,
    pub status_bar_style: Style,

    // SQL highlighting
    pub sql_keyword: Style,
    pub sql_string: Style,
    pub sql_number: Style,
    pub sql_comment: Style,
    pub sql_function: Style,
    pub sql_operator: Style,

    // ER diagram
    pub er_table_border: Style,
    pub er_pk_style: Style,
    pub er_fk_style: Style,
    pub er_relationship: Style,
}

/// Catppuccin Mocha-inspired dark theme.
pub(crate) const DARK_THEME: Theme = Theme {
    bg: BASE,
    fg: TEXT,
    border: SUBTEXT0,
    border_focused: BLUE,
    accent: BLUE,
    error: RED,
    success: GREEN,
    warning: YELLOW,

    null_style: Style::new().fg(SUBTEXT0).add_modifier(Modifier::ITALIC),
    header_style: Style::new()
        .fg(TEXT)
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED),
    selected_style: Style::new().fg(BASE).bg(BLUE),
    status_bar_style: Style::new().fg(TEXT).bg(SURFACE0),

    sql_keyword: Style::new().fg(BLUE).add_modifier(Modifier::BOLD),
    sql_string: Style::new().fg(GREEN),
    sql_number: Style::new().fg(YELLOW),
    sql_comment: Style::new().fg(SUBTEXT0),
    sql_function: Style::new().fg(TEAL),
    // Inherits fg from the render context — operators use the surrounding text color
    sql_operator: Style::new().add_modifier(Modifier::BOLD),

    er_table_border: Style::new().fg(BLUE),
    er_pk_style: Style::new().fg(YELLOW).add_modifier(Modifier::BOLD),
    er_fk_style: Style::new().fg(TEAL),
    er_relationship: Style::new().fg(SUBTEXT0),
};
