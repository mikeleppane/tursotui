use std::fmt::Write as _;
use std::time::Duration;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, BottomTab, PanelId, SubTab};
use crate::components::data_editor::DataEditorStatus;
use crate::db::QueryKind;
use crate::theme::Theme;

/// Format a `Duration` for human-readable display in the status bar.
///
/// - Under 1ms: "500us"
/// - Under 10ms: "1.23ms" (2 decimal places)
/// - Under 1s: "123ms" (whole milliseconds)
/// - Otherwise: "3.46s" (2 decimal places)
fn format_duration(d: Duration) -> String {
    let micros = d.as_micros();
    if micros < 1_000 {
        return format!("{micros}us");
    }

    let millis_f = d.as_secs_f64() * 1_000.0;
    if millis_f < 10.0 {
        return format!("{millis_f:.2}ms");
    }

    let total_millis = d.as_millis();
    if total_millis < 1_000 {
        return format!("{total_millis}ms");
    }

    let secs = d.as_secs_f64();
    format!("{secs:.2}s")
}

/// Format a row count with thousands separators for readability.
fn format_count(n: usize) -> String {
    if n < 1_000 {
        return n.to_string();
    }

    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Short label for the currently focused panel.
fn panel_label(sub_tab: SubTab, focus: PanelId, bottom_tab: BottomTab) -> &'static str {
    match (sub_tab, focus) {
        (SubTab::Query, PanelId::Editor) => "Editor",
        (SubTab::Query, PanelId::Schema) => "Schema",
        (SubTab::Query, PanelId::Bottom) => match bottom_tab {
            BottomTab::Results => "Results",
            BottomTab::Explain => "Explain",
            BottomTab::Detail => "Detail",
            BottomTab::ERDiagram => "ER Diagram",
        },
        (SubTab::Admin, PanelId::DbInfo) => "Database Info",
        (SubTab::Admin, PanelId::Pragmas) => "Pragmas",
        _ => "",
    }
}

/// Truncate a string to fit within `max_width` display columns,
/// respecting character boundaries and multi-byte/wide characters.
fn truncate_to_width(s: &str, max_width: usize) -> &str {
    if s.width() <= max_width {
        return s;
    }
    let mut current_width = 0;
    for (idx, ch) in s.char_indices() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > max_width {
            return &s[..idx];
        }
        current_width += ch_width;
    }
    s
}

/// Transient message TTL.
pub(crate) const TRANSIENT_TTL: Duration = Duration::from_secs(3);

/// Render the status bar at the bottom of the screen.
///
/// This is a render function, not a `Component` — it does not handle keys.
/// It reads from `AppState` and supplementary data to produce a styled line.
#[allow(clippy::too_many_lines)]
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    selected_row: Option<usize>,
    total_rows: usize,
    theme: &Theme,
    de_status: &DataEditorStatus,
) {
    // Transient messages take over the entire bar for TRANSIENT_TTL
    if let Some(ref tm) = app.transient_message
        && tm.created_at.elapsed() < TRANSIENT_TTL
    {
        let (prefix, fg) = if tm.is_error {
            ("Error: ", theme.error)
        } else {
            ("", theme.success)
        };
        let style = Style::default()
            .fg(fg)
            .bg(theme.status_bar_style.bg.unwrap_or(Color::Reset))
            .add_modifier(Modifier::BOLD);
        let line = Paragraph::new(format!(" {prefix}{}", tm.text)).style(style);
        frame.render_widget(line, area);
        return;
    }

    let db = app.active_db();
    let width = area.width as usize;

    // Left: focused panel name (with optional database label in multi-db mode)
    let label = panel_label(db.sub_tab, db.focus, db.bottom_tab);
    let left = if app.databases.len() > 1 {
        format!(" [{}] {label}", db.label)
    } else {
        format!(" {label}")
    };

    let center = if db.executing {
        match db.last_execution_source {
            crate::app::ExecutionSource::FullBuffer => "Executing...",
            crate::app::ExecutionSource::Selection => "Executing selection...",
            crate::app::ExecutionSource::StatementAtCursor => "Executing statement...",
        }
        .to_string()
    } else if let Some(ref kind) = db.last_query_kind {
        let time = db
            .last_execution_time
            .map(format_duration)
            .unwrap_or_default();
        match kind {
            QueryKind::Select => {
                let count = db.last_row_count.unwrap_or(0);
                if db.last_truncated {
                    format!("{}+ rows in {} (truncated)", format_count(count), time)
                } else {
                    format!("{} rows in {}", format_count(count), time)
                }
            }
            QueryKind::Explain => {
                let count = db.last_row_count.unwrap_or(0);
                format!("EXPLAIN: {} rows ({})", format_count(count), time)
            }
            QueryKind::Insert => {
                let n = db.last_rows_affected;
                let word = if n == 1 { "row" } else { "rows" };
                format!("{n} {word} inserted ({time})")
            }
            QueryKind::Update => {
                let n = db.last_rows_affected;
                let word = if n == 1 { "row" } else { "rows" };
                format!("{n} {word} updated ({time})")
            }
            QueryKind::Delete => {
                let n = db.last_rows_affected;
                let word = if n == 1 { "row" } else { "rows" };
                format!("{n} {word} deleted ({time})")
            }
            QueryKind::Ddl => format!("DDL executed ({time})"),
            QueryKind::Pragma => {
                let count = db.last_row_count.unwrap_or(0);
                if count > 0 {
                    format!("PRAGMA: {} rows ({})", format_count(count), time)
                } else {
                    format!("PRAGMA executed ({time})")
                }
            }
            QueryKind::Batch {
                statement_count,
                has_trailing_select,
            } => {
                if *has_trailing_select {
                    let n = statement_count - 1;
                    let word = if n == 1 { "statement" } else { "statements" };
                    format!("Batch: {n} {word} + SELECT ({time})")
                } else {
                    let word = if *statement_count == 1 {
                        "statement"
                    } else {
                        "statements"
                    };
                    format!("Batch: {statement_count} {word} executed ({time})")
                }
            }
            QueryKind::Other => format!("Query executed ({time})"),
        }
    } else {
        String::new()
    };

    // Append WHERE filter indicator to center when a filter is active
    let center = if let Some(ref filter) = db.results.filter_input {
        if filter.is_empty() {
            center
        } else {
            let truncated: String = if filter.width() > 30 {
                let mut w = 0;
                let s: String = filter
                    .chars()
                    .take_while(|c| {
                        w += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
                        w <= 30
                    })
                    .collect();
                format!("{s}\u{2026}")
            } else {
                filter.clone()
            };
            format!("{center} [WHERE: {truncated}]")
        }
    } else {
        center
    };

    // Right: row position when results focused + F1 Help
    let base_right = if db.focus == PanelId::Bottom && total_rows > 0 {
        if let Some(sel) = selected_row {
            format!("Row {} of {}  F1 Help ", sel + 1, format_count(total_rows))
        } else {
            format!("{} rows  F1 Help ", format_count(total_rows))
        }
    } else {
        "F1 Help ".to_string()
    };

    // Build plain-text data editor status segment for layout width calculation.
    // It is prepended to base_right so edit info appears left of the global hints.
    let edit_plain = build_edit_status_plain(de_status);
    let right = format!("{edit_plain}{base_right}");

    // Compose plain bar for exact-width layout
    let bar = compose_status_line(&left, &center, &right, width);

    // Always render as styled Line — accent the panel label on the left
    let styled_line = build_styled_status(bar, label, de_status, &edit_plain, theme);
    let status = Paragraph::new(styled_line).style(theme.status_bar_style);
    frame.render_widget(status, area);
}

/// Build the plain-text edit-status segment (used for layout width calculation).
///
/// Returns an empty string when the data editor is inactive.
fn build_edit_status_plain(de: &DataEditorStatus) -> String {
    if !de.active {
        return String::new();
    }

    let mut s = String::new();

    // FK breadcrumb trail (compact): "users>depts | "
    if !de.fk_breadcrumbs.is_empty() {
        s.push_str(&de.fk_breadcrumbs.join(">"));
        s.push('|');
    }

    // Editable table indicator (compact)
    if let Some(table) = &de.table {
        write!(s, "[{table}]").unwrap();
    }

    // Pending changes (compact)
    let (upd, ins, del) = de.pending;
    let total = upd + ins + del;
    if total > 0 {
        write!(s, " {total}\u{0394}").unwrap(); // Δ = delta symbol
    }

    // Trailing gap before base_right
    s.push(' ');
    s
}

/// Build a styled `Line` from the composed plain bar.
///
/// Styling applied:
/// - Panel label on the left: accent + bold
/// - Edit-info segment (if present): FK breadcrumbs dimmed, table accent, changes highlighted
/// - Everything else inherits the base `status_bar_style` from the caller.
fn build_styled_status(
    bar: String,
    panel_label: &str,
    de: &DataEditorStatus,
    edit_plain: &str,
    theme: &Theme,
) -> Line<'static> {
    let dim_style = Style::default().add_modifier(Modifier::DIM);
    let accent_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);

    // Step 1: Accent the panel label on the left (format is " Label")
    let label_needle = format!(" {panel_label}");
    let (label_prefix, rest_after_label) = if !panel_label.is_empty()
        && let Some(label_start) = bar.find(&label_needle)
    {
        let before_label = bar[..label_start].to_owned();
        let after_label = bar[label_start + label_needle.len()..].to_owned();
        (
            vec![
                Span::raw(before_label),
                Span::raw(" "),
                Span::styled(panel_label.to_string(), accent_style),
            ],
            after_label,
        )
    } else {
        (Vec::new(), bar)
    };

    // Step 2: If edit_plain is non-empty, find and style it within rest_after_label
    if !edit_plain.is_empty()
        && let Some(edit_start) = rest_after_label.find(edit_plain)
    {
        let before_edit = rest_after_label[..edit_start].to_owned();
        let after_edit = rest_after_label[edit_start + edit_plain.len()..].to_owned();

        let mut spans: Vec<Span<'static>> = label_prefix;
        if !before_edit.is_empty() {
            spans.push(Span::raw(before_edit));
        }

        // FK breadcrumbs
        if !de.fk_breadcrumbs.is_empty() {
            let crumb = de.fk_breadcrumbs.join(">");
            spans.push(Span::styled(crumb, dim_style));
            spans.push(Span::raw("|"));
        }

        // [table] with accent
        if let Some(table) = &de.table {
            spans.push(Span::raw("["));
            spans.push(Span::styled(table.clone(), accent_style));
            spans.push(Span::raw("]"));
        }

        // Pending changes with warning color
        let (upd, ins, del) = de.pending;
        let total = upd + ins + del;
        if total > 0 {
            spans.push(Span::styled(
                format!(" {total}\u{0394}"),
                Style::default().fg(theme.warning),
            ));
        }

        spans.push(Span::raw(" "));

        if !after_edit.is_empty() {
            spans.push(Span::raw(after_edit));
        }

        Line::from(spans)
    } else {
        // No edit info — just accent the panel label
        let mut spans = label_prefix;
        if !rest_after_label.is_empty() {
            spans.push(Span::raw(rest_after_label));
        }
        Line::from(spans)
    }
}

/// Compose left, center, and right sections into a fixed-width line.
///
/// Layout: left is flush-left, center is screen-centered, right is flush-right.
/// Uses unicode display widths for correct handling of multi-byte characters.
fn compose_status_line(left: &str, center: &str, right: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let left_w = left.width();
    let center_w = center.width();
    let right_w = right.width();
    let total_content = left_w + center_w + right_w;

    let result = if total_content <= width {
        // Everything fits — true centering: place center at (width - center_w) / 2
        let center_start = (width - center_w) / 2;
        // Ensure center doesn't overlap left
        let center_start = center_start.max(left_w);
        // Ensure center + right don't exceed width
        let center_start = center_start.min(width.saturating_sub(center_w + right_w));
        let right_start = width - right_w;

        let gap1 = center_start - left_w;
        let gap2 = right_start - (center_start + center_w);

        let mut buf = String::with_capacity(width + 4); // small extra for multi-byte
        buf.push_str(left);
        for _ in 0..gap1 {
            buf.push(' ');
        }
        buf.push_str(center);
        for _ in 0..gap2 {
            buf.push(' ');
        }
        buf.push_str(right);
        buf
    } else {
        // Truncate: prioritize left, then right, center gets squeezed
        let mut buf = String::with_capacity(width + 4);
        let trunc_left = truncate_to_width(left, width);
        buf.push_str(trunc_left);
        let used = trunc_left.width();
        if used < width {
            let remaining = width - used;
            if remaining >= right_w {
                let gap = remaining - right_w;
                let trunc_center = truncate_to_width(center, gap);
                let center_used = trunc_center.width();
                for _ in 0..(gap - center_used) {
                    buf.push(' ');
                }
                buf.push_str(trunc_center);
                buf.push_str(right);
            } else {
                // Even right doesn't fit fully
                let trunc_right = truncate_to_width(right, remaining);
                let right_used = trunc_right.width();
                for _ in 0..(remaining - right_used) {
                    buf.push(' ');
                }
                buf.push_str(trunc_right);
            }
        }
        buf
    };

    debug_assert_eq!(
        result.width(),
        width,
        "compose_status_line output width mismatch: got {}, expected {width}",
        result.width()
    );

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- format_duration tests ---

    #[test]
    fn test_format_duration_microseconds() {
        assert_eq!(format_duration(Duration::from_micros(0)), "0us");
        assert_eq!(format_duration(Duration::from_micros(1)), "1us");
        assert_eq!(format_duration(Duration::from_micros(500)), "500us");
        assert_eq!(format_duration(Duration::from_micros(999)), "999us");
    }

    #[test]
    fn test_format_duration_small_milliseconds() {
        // 1ms = 1000us, should show as "1.00ms"
        assert_eq!(format_duration(Duration::from_micros(1_000)), "1.00ms");
        assert_eq!(format_duration(Duration::from_micros(1_234)), "1.23ms");
    }

    #[test]
    fn test_format_duration_boundary_near_10ms() {
        // 9ms exactly
        assert_eq!(format_duration(Duration::from_micros(9_000)), "9.00ms");
        // 9.5ms
        assert_eq!(format_duration(Duration::from_micros(9_500)), "9.50ms");
        // 10ms exactly — should cross into whole-ms branch
        assert_eq!(format_duration(Duration::from_micros(10_000)), "10ms");
    }

    #[test]
    fn test_format_duration_whole_milliseconds() {
        assert_eq!(format_duration(Duration::from_millis(10)), "10ms");
        assert_eq!(format_duration(Duration::from_millis(123)), "123ms");
        assert_eq!(format_duration(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_millis(1_000)), "1.00s");
        assert_eq!(format_duration(Duration::from_millis(3_456)), "3.46s");
        assert_eq!(format_duration(Duration::from_secs(10)), "10.00s");
    }

    // --- format_count tests ---

    #[test]
    fn test_format_count_small() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(1), "1");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn test_format_count_thousands() {
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(10_000), "10,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    // --- truncate_to_width tests ---

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate_to_width("hello", 3), "hel");
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    #[test]
    fn test_truncate_multibyte() {
        // "données" has multi-byte chars (é = 2 bytes, 1 display column)
        let s = "données.db";
        assert_eq!(truncate_to_width(s, 5), "donné");
        assert_eq!(truncate_to_width(s, 10), "données.db");
    }

    #[test]
    fn test_truncate_wide_chars() {
        // CJK characters are 2 display columns each
        let s = "データ.db";
        // "デ" = 2 cols, "ー" = 2 cols, "タ" = 2 cols => 6 cols + ".db" = 3 => total 9
        assert_eq!(truncate_to_width(s, 4), "デー");
        assert_eq!(truncate_to_width(s, 5), "デー"); // can't fit half of タ
    }

    // --- compose_status_line tests ---

    #[test]
    fn test_compose_fits_and_centers() {
        // width=20, left="LEFT"(4), center="MID"(3), right="RIGHT"(5)
        // center should start at (20-3)/2 = 8
        let result = compose_status_line("LEFT", "MID", "RIGHT", 20);
        assert_eq!(result.width(), 20);
        assert!(result.starts_with("LEFT"));
        assert!(result.ends_with("RIGHT"));
        assert!(result.contains("MID"));
        // Center at column 8: "LEFT    MID    RIGHT"
        // left=4, gap1=4, center=3, gap2=4, right=5 = 20
        assert_eq!(result, "LEFT    MID    RIGHT");
    }

    #[test]
    fn test_compose_zero_width() {
        assert_eq!(compose_status_line("L", "M", "R", 0), "");
    }

    #[test]
    fn test_compose_overflow_small_width() {
        // Width smaller than left+center+right combined: content is truncated
        let left = "LEFTLEFT";
        let center = "CENTER";
        let right = "RIGHT";
        // total content = 8 + 6 + 5 = 19, use width = 10
        let result = compose_status_line(left, center, right, 10);
        assert_eq!(result.width(), 10, "output must exactly fill the width");
        // Left section should appear first (prioritized)
        assert!(result.starts_with("LEFTLEFT"), "left section is preserved");

        // Width smaller than even left alone: left is truncated to width
        let result_tiny = compose_status_line(left, center, right, 5);
        assert_eq!(result_tiny.width(), 5);
        assert_eq!(&result_tiny, "LEFTL");

        // Width fits left + right but squeezes center
        let result_mid = compose_status_line("AB", "LONGCENTER", "YZ", 8);
        assert_eq!(result_mid.width(), 8);
        assert!(result_mid.starts_with("AB"), "left preserved");
        assert!(result_mid.ends_with("YZ"), "right preserved");
    }

    #[test]
    fn test_compose_unicode_no_panic() {
        // Multi-byte label should not panic
        let result = compose_status_line(" F5 Execute", "3 rows", "données.db ", 40);
        assert_eq!(result.width(), 40);

        // Wide CJK characters
        let result = compose_status_line(" F5 Execute", "3 rows", "データ.db ", 40);
        assert_eq!(result.width(), 40);
    }

    #[test]
    fn test_compose_empty_center() {
        let result = compose_status_line("LEFT", "", "RIGHT", 20);
        assert_eq!(result.width(), 20);
        assert!(result.starts_with("LEFT"));
        assert!(result.ends_with("RIGHT"));
    }
}
