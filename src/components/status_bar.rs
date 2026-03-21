use std::time::Duration;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, PanelId, SubTab};
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

/// Keybinding hints for each focused panel, varying by sub-tab context.
fn keybindings_for(sub_tab: SubTab, focus: PanelId) -> &'static str {
    match (sub_tab, focus) {
        // Query-tab panels
        (SubTab::Query, PanelId::Editor) => "F5 Execute  Esc Release",
        (SubTab::Query, PanelId::Schema) => "Enter Expand  o Open  Esc Release",
        (SubTab::Query, PanelId::Bottom) => "j/k Navigate  g/G First/Last  Esc Release",
        // Admin-tab panels and fallback
        _ => "Tab Cycle",
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
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    selected_row: Option<usize>,
    total_rows: usize,
    theme: &Theme,
) {
    // Transient messages (errors) take over the entire bar
    if let Some((ref msg, at)) = app.transient_message
        && at.elapsed() < TRANSIENT_TTL
    {
        let style = Style::default()
            .fg(theme.error)
            .bg(theme.status_bar_style.bg.unwrap_or(Color::Reset))
            .add_modifier(Modifier::BOLD);
        let line = Paragraph::new(format!(" Error: {msg}")).style(style);
        frame.render_widget(line, area);
        return;
    }

    let db = app.active_db();
    let width = area.width as usize;

    // Left: keybinding hints
    let left = format!(" {}", keybindings_for(db.sub_tab, db.focus));

    // Center: execution status
    let center = if db.executing {
        "Executing...".to_string()
    } else if let (Some(count), Some(time)) = (db.last_row_count, db.last_execution_time) {
        if db.last_truncated {
            format!(
                "{}+ rows in {} (truncated)",
                format_count(count),
                format_duration(time)
            )
        } else {
            format!("{} rows in {}", format_count(count), format_duration(time))
        }
    } else {
        String::new()
    };

    // Right: row position when results focused, otherwise DB label
    let right = if db.focus == PanelId::Bottom && total_rows > 0 {
        if let Some(sel) = selected_row {
            format!("Row {} of {} ", sel + 1, format_count(total_rows))
        } else {
            format!("{} rows ", format_count(total_rows))
        }
    } else {
        format!("{} ", db.label)
    };

    // Compose the three sections into a single line
    let bar = compose_status_line(&left, &center, &right, width);

    let status = Paragraph::new(bar).style(theme.status_bar_style);
    frame.render_widget(status, area);
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
