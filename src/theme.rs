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

    // Data editor
    pub edit_modified: Style,
    pub edit_inserted: Style,
    pub edit_deleted: Style,
    pub edit_cell_active: Style,
    pub fk_indicator: Style,
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

    edit_modified: Style::new().bg(Color::Rgb(62, 53, 18)),
    edit_inserted: Style::new().bg(Color::Rgb(26, 52, 26)),
    edit_deleted: Style::new()
        .fg(SUBTEXT0)
        .add_modifier(Modifier::CROSSED_OUT),
    edit_cell_active: Style::new().fg(BASE).bg(TEAL),
    fk_indicator: Style::new().fg(BLUE),
};

/// Catppuccin Latte-inspired light theme.
pub(crate) const LIGHT_THEME: Theme = Theme {
    bg: Color::Rgb(239, 241, 245),
    fg: Color::Rgb(76, 79, 105),
    border: Color::Rgb(172, 176, 190),
    border_focused: Color::Rgb(30, 102, 245),
    accent: Color::Rgb(30, 102, 245),
    error: Color::Rgb(210, 15, 57),
    success: Color::Rgb(64, 160, 43),
    warning: Color::Rgb(223, 142, 29),

    null_style: Style::new()
        .fg(Color::Rgb(172, 176, 190))
        .add_modifier(Modifier::ITALIC),
    header_style: Style::new()
        .fg(Color::Rgb(76, 79, 105))
        .add_modifier(Modifier::BOLD.union(Modifier::UNDERLINED)),
    selected_style: Style::new()
        .fg(Color::Rgb(239, 241, 245))
        .bg(Color::Rgb(30, 102, 245)),
    status_bar_style: Style::new()
        .fg(Color::Rgb(76, 79, 105))
        .bg(Color::Rgb(204, 208, 218)),

    sql_keyword: Style::new()
        .fg(Color::Rgb(30, 102, 245))
        .add_modifier(Modifier::BOLD),
    sql_string: Style::new().fg(Color::Rgb(64, 160, 43)),
    sql_number: Style::new().fg(Color::Rgb(223, 142, 29)),
    sql_comment: Style::new().fg(Color::Rgb(172, 176, 190)),
    sql_function: Style::new().fg(Color::Rgb(23, 146, 153)),
    sql_operator: Style::new().add_modifier(Modifier::BOLD),

    er_table_border: Style::new().fg(Color::Rgb(30, 102, 245)),
    er_pk_style: Style::new()
        .fg(Color::Rgb(223, 142, 29))
        .add_modifier(Modifier::BOLD),
    er_fk_style: Style::new().fg(Color::Rgb(23, 146, 153)),
    er_relationship: Style::new().fg(Color::Rgb(172, 176, 190)),

    edit_modified: Style::new().bg(Color::Rgb(255, 248, 195)),
    edit_inserted: Style::new().bg(Color::Rgb(212, 237, 218)),
    edit_deleted: Style::new()
        .fg(Color::Rgb(172, 176, 190))
        .add_modifier(Modifier::CROSSED_OUT),
    edit_cell_active: Style::new()
        .fg(Color::Rgb(239, 241, 245))
        .bg(Color::Rgb(23, 146, 153)),
    fk_indicator: Style::new().fg(Color::Rgb(30, 102, 245)),
};
