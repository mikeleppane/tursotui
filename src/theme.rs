use ratatui::style::{Color, Modifier, Style};

// Catppuccin Mocha palette
const BASE: Color = Color::Rgb(30, 30, 46);
const MANTLE: Color = Color::Rgb(24, 24, 37);
const SURFACE0: Color = Color::Rgb(49, 50, 68);
const SURFACE1: Color = Color::Rgb(69, 71, 90);
const TEXT: Color = Color::Rgb(205, 214, 244);
const SUBTEXT0: Color = Color::Rgb(166, 173, 200);
const SUBTEXT1: Color = Color::Rgb(88, 91, 112);
const OVERLAY0: Color = Color::Rgb(108, 112, 134);
const OVERLAY1: Color = Color::Rgb(127, 132, 156);
const BLUE: Color = Color::Rgb(137, 180, 250);
const RED: Color = Color::Rgb(243, 139, 168);
const GREEN: Color = Color::Rgb(166, 227, 161);
const YELLOW: Color = Color::Rgb(249, 226, 175);
const TEAL: Color = Color::Rgb(148, 226, 213);
const LAVENDER: Color = Color::Rgb(180, 190, 254);
const MAUVE: Color = Color::Rgb(203, 166, 247);
const PEACH: Color = Color::Rgb(250, 179, 135);
const PINK: Color = Color::Rgb(245, 194, 231);

/// Visual theme for the entire application.
/// Every styled element references a field here — no hardcoded colors elsewhere.
#[derive(Clone, Copy)]
#[allow(
    dead_code,
    reason = "fields used incrementally as components are added"
)]
pub(crate) struct Theme {
    // Base
    pub bg: Color,
    pub fg: Color,
    pub mantle: Color,
    pub surface0: Color,
    pub surface1: Color,
    pub border: Color,
    pub border_focused: Color,
    pub accent: Color,
    pub accent2: Color,
    pub dim: Color,
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
    pub sql_type: Style,
    pub sql_parameter: Style,
    pub sql_field: Style,

    // ER diagram
    pub er_table_border: Style,
    pub er_pk_style: Style,
    pub er_fk_style: Style,
    pub er_relationship: Style,
    pub er_connected_border: Style,
    pub er_dimmed: Style,
    pub er_edge_label: Style,

    // Schema tree
    pub schema_table: Color,
    pub schema_view: Color,
    pub schema_index: Color,
    pub schema_trigger: Color,
    pub schema_column: Color,
    pub schema_pk: Color,
    pub schema_type: Color,

    // Results table
    pub row_alt_bg: Color,

    // Editor
    pub active_line_bg: Color,

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
    mantle: MANTLE,
    surface0: SURFACE0,
    surface1: SURFACE1,
    border: OVERLAY0,
    border_focused: BLUE,
    accent: BLUE,
    accent2: LAVENDER,
    dim: SUBTEXT1,
    error: RED,
    success: GREEN,
    warning: YELLOW,

    null_style: Style::new().fg(OVERLAY0).add_modifier(Modifier::ITALIC),
    header_style: Style::new()
        .fg(LAVENDER)
        .bg(SURFACE0)
        .add_modifier(Modifier::BOLD),
    selected_style: Style::new().fg(BASE).bg(BLUE),
    status_bar_style: Style::new().fg(SUBTEXT0).bg(MANTLE),

    sql_keyword: Style::new().fg(BLUE).add_modifier(Modifier::BOLD),
    sql_string: Style::new().fg(GREEN),
    sql_number: Style::new().fg(YELLOW),
    sql_comment: Style::new().fg(OVERLAY0),
    sql_function: Style::new().fg(TEAL),
    sql_operator: Style::new().fg(PEACH).add_modifier(Modifier::BOLD),
    sql_type: Style::new().fg(MAUVE),
    sql_parameter: Style::new().fg(PINK),
    sql_field: Style::new().fg(LAVENDER),

    er_table_border: Style::new().fg(BLUE),
    er_pk_style: Style::new().fg(YELLOW).add_modifier(Modifier::BOLD),
    er_fk_style: Style::new().fg(TEAL),
    er_relationship: Style::new().fg(OVERLAY0),
    er_connected_border: Style::new().fg(GREEN),
    er_dimmed: Style::new().fg(SURFACE0),
    er_edge_label: Style::new().fg(OVERLAY1),

    // Schema tree colors
    schema_table: BLUE,
    schema_view: MAUVE,
    schema_index: PEACH,
    schema_trigger: PINK,
    schema_column: SUBTEXT0,
    schema_pk: YELLOW,
    schema_type: OVERLAY0,

    // Results alternating row
    row_alt_bg: Color::Rgb(36, 36, 54),

    // Editor active line
    active_line_bg: Color::Rgb(40, 40, 60),

    edit_modified: Style::new().bg(Color::Rgb(62, 53, 18)),
    edit_inserted: Style::new().bg(Color::Rgb(26, 52, 26)),
    edit_deleted: Style::new()
        .fg(OVERLAY0)
        .add_modifier(Modifier::CROSSED_OUT),
    edit_cell_active: Style::new().fg(BASE).bg(TEAL),
    fk_indicator: Style::new().fg(BLUE),
};

// Catppuccin Latte palette constants
const LATTE_BASE: Color = Color::Rgb(239, 241, 245);
const LATTE_MANTLE: Color = Color::Rgb(230, 233, 239);
const LATTE_CRUST: Color = Color::Rgb(220, 224, 232);
const LATTE_SURFACE0: Color = Color::Rgb(204, 208, 218);
const LATTE_SURFACE1: Color = Color::Rgb(188, 192, 204);
const LATTE_OVERLAY0: Color = Color::Rgb(140, 143, 161);
const LATTE_TEXT: Color = Color::Rgb(76, 79, 105);
const LATTE_SUBTEXT0: Color = Color::Rgb(108, 111, 133);
const LATTE_BLUE: Color = Color::Rgb(30, 102, 245);
const LATTE_LAVENDER: Color = Color::Rgb(114, 135, 253);

/// Catppuccin Latte-inspired light theme.
pub(crate) const LIGHT_THEME: Theme = Theme {
    bg: LATTE_BASE,
    fg: LATTE_TEXT,
    mantle: LATTE_MANTLE,
    surface0: LATTE_SURFACE0,
    surface1: LATTE_SURFACE1,
    border: LATTE_OVERLAY0,
    border_focused: LATTE_BLUE,
    accent: LATTE_BLUE,
    accent2: LATTE_LAVENDER,
    dim: LATTE_SURFACE1,
    error: Color::Rgb(210, 15, 57),
    success: Color::Rgb(64, 160, 43),
    warning: Color::Rgb(223, 142, 29),

    null_style: Style::new()
        .fg(LATTE_OVERLAY0)
        .add_modifier(Modifier::ITALIC),
    header_style: Style::new()
        .fg(LATTE_TEXT)
        .bg(LATTE_MANTLE)
        .add_modifier(Modifier::BOLD),
    selected_style: Style::new().fg(LATTE_BASE).bg(LATTE_BLUE),
    status_bar_style: Style::new().fg(LATTE_SUBTEXT0).bg(LATTE_CRUST),

    sql_keyword: Style::new().fg(LATTE_BLUE).add_modifier(Modifier::BOLD),
    sql_string: Style::new().fg(Color::Rgb(64, 160, 43)),
    sql_number: Style::new().fg(Color::Rgb(223, 142, 29)),
    sql_comment: Style::new().fg(LATTE_OVERLAY0),
    sql_function: Style::new().fg(Color::Rgb(23, 146, 153)), // Latte Teal
    sql_operator: Style::new()
        .fg(Color::Rgb(254, 100, 11)) // Latte Peach
        .add_modifier(Modifier::BOLD),
    sql_type: Style::new().fg(Color::Rgb(136, 57, 239)), // Latte Mauve
    sql_parameter: Style::new().fg(Color::Rgb(234, 118, 203)), // Latte Pink
    sql_field: Style::new().fg(LATTE_LAVENDER),

    er_table_border: Style::new().fg(LATTE_BLUE),
    er_pk_style: Style::new()
        .fg(Color::Rgb(223, 142, 29))
        .add_modifier(Modifier::BOLD),
    er_fk_style: Style::new().fg(Color::Rgb(23, 146, 153)),
    er_relationship: Style::new().fg(LATTE_OVERLAY0),
    er_connected_border: Style::new().fg(Color::Rgb(64, 160, 43)),
    er_dimmed: Style::new().fg(LATTE_SURFACE1),
    er_edge_label: Style::new().fg(LATTE_OVERLAY0),

    // Schema tree (Catppuccin Latte colors)
    schema_table: LATTE_BLUE,
    schema_view: Color::Rgb(136, 57, 239),     // Latte Mauve
    schema_index: Color::Rgb(254, 100, 11),    // Latte Peach
    schema_trigger: Color::Rgb(234, 118, 203), // Latte Pink
    schema_column: LATTE_SUBTEXT0,
    schema_pk: Color::Rgb(223, 142, 29),
    schema_type: LATTE_OVERLAY0,

    // Results alternating row
    row_alt_bg: Color::Rgb(230, 233, 239),

    // Editor active line
    active_line_bg: Color::Rgb(228, 230, 238),

    edit_modified: Style::new().bg(Color::Rgb(255, 248, 195)),
    edit_inserted: Style::new().bg(Color::Rgb(212, 237, 218)),
    edit_deleted: Style::new()
        .fg(LATTE_OVERLAY0)
        .add_modifier(Modifier::CROSSED_OUT),
    edit_cell_active: Style::new().fg(LATTE_BASE).bg(Color::Rgb(23, 146, 153)),
    fk_indicator: Style::new().fg(LATTE_BLUE),
};
