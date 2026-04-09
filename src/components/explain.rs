use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AdminAction, BottomTab, Direction, EditorAction, NavAction, TableId};
use crate::theme::Theme;

use super::Component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExplainMode {
    Bytecode,
    QueryPlan,
}

/// Classification of a query plan line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanType {
    FullTableScan,
    IndexSeek,
    CoveringIndex,
    RowidLookup,
    TempBTree,
    CorrelatedSubquery,
    Subquery,
    Unknown,
}

/// Severity levels for plan warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WarningSeverity {
    Critical,
    Warning,
    Info,
}

/// A warning generated from query plan analysis.
#[derive(Debug, Clone)]
pub(crate) struct PlanWarning {
    pub(crate) severity: WarningSeverity,
    pub(crate) message: String,
    pub(crate) suggestion: Option<String>,
}

/// Classify an EXPLAIN QUERY PLAN line by scan type.
///
/// Uses simple string matching on plan output (not regex — plan format is stable).
/// Unrecognized lines pass through as `Unknown` rather than being misclassified.
pub(crate) fn classify_plan_line(line: &str) -> ScanType {
    let trimmed =
        line.trim_start_matches(|c: char| c == '|' || c == '-' || c == ' ' || c.is_ascii_digit());
    let upper = trimmed.to_uppercase();
    // SQLite plan lines may or may not include "TABLE" — e.g. "SCAN TABLE t" vs "SCAN t AS alias"
    let has_scan = upper.starts_with("SCAN");
    let has_search = upper.starts_with("SEARCH");
    if upper.contains("USING COVERING INDEX") {
        ScanType::CoveringIndex
    } else if upper.contains("USING INTEGER PRIMARY KEY") {
        ScanType::RowidLookup
    } else if has_search && upper.contains("USING INDEX") {
        ScanType::IndexSeek
    } else if has_scan {
        ScanType::FullTableScan
    } else if upper.contains("USE TEMP B-TREE") {
        ScanType::TempBTree
    } else if upper.contains("CORRELATED SCALAR SUBQUERY") {
        ScanType::CorrelatedSubquery
    } else if upper.contains("COMPOUND SUBQUERY") || upper.contains("SUBQUERY") {
        ScanType::Subquery
    } else {
        ScanType::Unknown
    }
}

/// Extract column candidates from SQL WHERE / ORDER BY clauses using simple
/// token heuristics. Not grammar-aware — may produce false positives for
/// aliased columns, function arguments, or subquery references. Suggestions
/// generated from these candidates should be treated as best-effort guidance.
fn extract_where_columns(sql: &str) -> Vec<String> {
    let upper = sql.to_uppercase();
    let mut columns = Vec::new();

    // Find tokens between WHERE and the next clause keyword (GROUP BY, ORDER BY,
    // HAVING, LIMIT, UNION, etc.) or end of string
    let clause_ends = [
        "GROUP BY",
        "ORDER BY",
        "HAVING",
        "LIMIT",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        ";",
    ];
    if let Some(where_pos) = upper.find("WHERE") {
        let after_where = &sql[where_pos + 5..];
        let upper_after = &upper[where_pos + 5..];
        let end = clause_ends
            .iter()
            .filter_map(|kw| upper_after.find(kw))
            .min()
            .unwrap_or(after_where.len());
        let clause = &after_where[..end];
        extract_identifiers_from_clause(clause, &mut columns);
    }

    // Also check ORDER BY columns for potential index benefit
    if let Some(order_pos) = upper.find("ORDER BY") {
        let after_order = &sql[order_pos + 8..];
        let upper_after = &upper[order_pos + 8..];
        let end = clause_ends
            .iter()
            .filter(|kw| **kw != "ORDER BY")
            .filter_map(|kw| upper_after.find(kw))
            .min()
            .unwrap_or(after_order.len());
        let clause = &after_order[..end];
        extract_identifiers_from_clause(clause, &mut columns);
    }

    columns
}

/// Extract identifier-like tokens from a SQL clause fragment.
/// Skips SQL keywords, string literals, numbers, and function calls (tokens followed by `(`).
fn extract_identifiers_from_clause(clause: &str, columns: &mut Vec<String>) {
    let keywords = [
        "AND", "OR", "NOT", "IN", "IS", "NULL", "LIKE", "GLOB", "BETWEEN", "EXISTS", "ASC", "DESC",
        "COLLATE", "NOCASE", "BINARY", "RTRIM", "TRUE", "FALSE",
    ];

    let tokens: Vec<&str> = clause
        .split(|c: char| {
            c == ' '
                || c == ','
                || c == '='
                || c == '<'
                || c == '>'
                || c == '!'
                || c == '\n'
                || c == '\r'
                || c == '\t'
        })
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();

    for (i, token) in tokens.iter().enumerate() {
        let clean = token
            .trim_matches('"')
            .trim_matches('`')
            .trim_matches('[')
            .trim_matches(']');
        if clean.is_empty() {
            continue;
        }
        // Skip if it contains ( — likely a function call
        if clean.contains('(') || clean.contains(')') {
            continue;
        }
        // Skip string literals
        if clean.starts_with('\'')
            || clean.starts_with('?')
            || clean.starts_with(':')
            || clean.starts_with('$')
        {
            continue;
        }
        // Skip numbers
        if clean.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
            continue;
        }
        // Skip SQL keywords
        if keywords.contains(&clean.to_uppercase().as_str()) {
            continue;
        }
        // Skip if next token is ( — this is a function name
        if tokens.get(i + 1).is_some_and(|next| next.starts_with('(')) {
            continue;
        }
        // Handle table.column — extract the column part
        let col = if let Some(dot_pos) = clean.rfind('.') {
            &clean[dot_pos + 1..]
        } else {
            clean
        };
        if !col.is_empty() && !columns.iter().any(|c| c.eq_ignore_ascii_case(col)) {
            columns.push(col.to_string());
        }
    }
}

/// Generate warnings from EXPLAIN QUERY PLAN output.
///
/// Correlates plan lines with row counts and SQL text to produce actionable
/// warnings with optional index suggestions.
pub(crate) fn generate_plan_warnings(
    plan_lines: &[String],
    sql: &str,
    row_counts: &std::collections::HashMap<TableId, u64>,
    index_details: &std::collections::HashMap<TableId, Vec<tursotui_db::IndexDetail>>,
) -> Vec<PlanWarning> {
    let mut warnings = Vec::new();

    for line in plan_lines {
        let scan = classify_plan_line(line);
        match scan {
            ScanType::FullTableScan => {
                // Extract table name from "SCAN TABLE tablename"
                let table_name = extract_table_from_plan(line);
                let row_count = table_name
                    .as_ref()
                    .and_then(|t| row_counts.get(&TableId::new(t.as_str())))
                    .copied()
                    .unwrap_or(0);

                let severity = if row_count > 10_000 {
                    WarningSeverity::Critical
                } else if row_count > 1_000 {
                    WarningSeverity::Warning
                } else {
                    WarningSeverity::Info
                };

                let table_display = table_name.as_deref().unwrap_or("unknown");
                let count_display = if row_count > 0 {
                    format!(" ({row_count} rows)")
                } else {
                    String::new()
                };

                let suggestion = build_index_suggestion(sql, table_name.as_deref(), index_details);

                warnings.push(PlanWarning {
                    severity,
                    message: format!("Full table scan on \"{table_display}\"{count_display}"),
                    suggestion,
                });
            }
            ScanType::TempBTree => {
                warnings.push(PlanWarning {
                    severity: WarningSeverity::Warning,
                    message: "Temp B-Tree used (ORDER BY / GROUP BY without index)".to_string(),
                    suggestion: None,
                });
            }
            ScanType::CorrelatedSubquery => {
                warnings.push(PlanWarning {
                    severity: WarningSeverity::Warning,
                    message: "Correlated scalar subquery — runs once per outer row".to_string(),
                    suggestion: None,
                });
            }
            _ => {}
        }
    }

    warnings
}

/// Extract table name from a plan line.
///
/// Handles both forms:
/// - `SCAN TABLE orders` / `SEARCH TABLE orders USING INDEX ...`
/// - `SCAN orders AS o` / `SEARCH orders USING INDEX ...` (no TABLE keyword)
fn extract_table_from_plan(line: &str) -> Option<String> {
    let upper = line.to_uppercase();
    // Try with TABLE keyword first (more specific)
    let markers_with_table = ["SCAN TABLE ", "SEARCH TABLE "];
    for marker in markers_with_table {
        if let Some(pos) = upper.find(marker) {
            let after = &line[pos + marker.len()..];
            let name = after
                .split(|c: char| c.is_whitespace() || c == '(')
                .next()?;
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    // Fall back to without TABLE keyword: "SCAN name" / "SEARCH name"
    let markers_bare = ["SCAN ", "SEARCH "];
    for marker in markers_bare {
        if upper.starts_with(marker) {
            let after = &line[marker.len()..];
            // Skip "TABLE" if present (shouldn't reach here, but be safe)
            let after = after.strip_prefix("TABLE ").unwrap_or(after);
            let name = after
                .split(|c: char| c.is_whitespace() || c == '(')
                .next()?;
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Build a CREATE INDEX suggestion for a full table scan.
///
/// Column ordering in composite suggestions is best-effort — the leftmost-prefix
/// rule means column order matters, but we can't determine optimal order from
/// a simple token scan. Suggestions should be verified by the user.
fn build_index_suggestion(
    sql: &str,
    table_name: Option<&str>,
    index_details: &std::collections::HashMap<TableId, Vec<tursotui_db::IndexDetail>>,
) -> Option<String> {
    let table = table_name?;
    let columns = extract_where_columns(sql);
    if columns.is_empty() {
        return None;
    }

    // Filter out columns that are already the leading column of an existing index
    let existing_leading: Vec<String> = index_details
        .get(&TableId::new(table))
        .map(|indexes| {
            indexes
                .iter()
                .filter_map(|idx| idx.columns.first().cloned())
                .collect()
        })
        .unwrap_or_default();

    let unindexed: Vec<&str> = columns
        .iter()
        .filter(|c| !existing_leading.iter().any(|e| e.eq_ignore_ascii_case(c)))
        .map(String::as_str)
        .collect();

    if unindexed.is_empty() {
        return None;
    }

    let col_list = unindexed
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let idx_name = format!(
        "idx_{}_{}",
        table.to_lowercase(),
        unindexed.join("_").to_lowercase()
    );
    let comment = if unindexed.len() > 1 {
        " -- verify column order"
    } else {
        ""
    };
    Some(format!(
        "CREATE INDEX \"{idx_name}\" ON \"{table}\" ({col_list});{comment}"
    ))
}

/// EXPLAIN view with bytecode table and query plan modes.
///
/// Lazily generates EXPLAIN output: on `QueryCompleted`, the view is marked
/// stale via `mark_stale(sql)`. The user presses Enter to generate the
/// EXPLAIN from `last_query`, which is the SQL that was actually executed
/// (not the current editor buffer).
pub(crate) struct ExplainView {
    mode: ExplainMode,
    bytecode_rows: Vec<Vec<String>>,
    plan_lines: Vec<String>,
    /// Classification of each plan line (parallel to `plan_lines`).
    plan_classifications: Vec<ScanType>,
    /// Generated warnings from plan analysis.
    warnings: Vec<PlanWarning>,
    selected_row: usize,
    scroll_offset: usize,
    stale: bool,
    loading: bool,
    last_query: Option<String>,
}

/// Column headers for EXPLAIN bytecode output.
const BYTECODE_HEADERS: &[&str] = &["addr", "opcode", "p1", "p2", "p3", "p4", "p5", "comment"];

/// Fixed widths for bytecode columns (all except comment, which fills remaining width).
const BYTECODE_MIN_WIDTHS: &[u16] = &[5, 16, 6, 6, 6, 20, 4];

// Ensure the last column (comment) is the one that fills remaining width.
const _: () = assert!(BYTECODE_MIN_WIDTHS.len() + 1 == BYTECODE_HEADERS.len());

impl ExplainView {
    pub(crate) fn new() -> Self {
        Self {
            mode: ExplainMode::Bytecode,
            bytecode_rows: Vec::new(),
            plan_lines: Vec::new(),
            plan_classifications: Vec::new(),
            warnings: Vec::new(),
            selected_row: 0,
            scroll_offset: 0,
            stale: true,
            loading: false,
            last_query: None,
        }
    }

    /// Mark the view as stale after a new query execution.
    /// Stores the SQL for later EXPLAIN generation and clears old data.
    pub(crate) fn mark_stale(&mut self, sql: String) {
        self.stale = true;
        self.last_query = Some(sql);
        self.bytecode_rows.clear();
        self.plan_lines.clear();
        self.plan_classifications.clear();
        self.warnings.clear();
        self.selected_row = 0;
        self.scroll_offset = 0;
    }

    /// Store EXPLAIN results from the async task and generate plan analysis.
    pub(crate) fn set_results(
        &mut self,
        bytecode: Vec<Vec<String>>,
        plan: Vec<String>,
        sql: &str,
        row_counts: &std::collections::HashMap<TableId, u64>,
        index_details: &std::collections::HashMap<TableId, Vec<tursotui_db::IndexDetail>>,
    ) {
        self.plan_classifications = plan.iter().map(|l| classify_plan_line(l)).collect();
        self.warnings = generate_plan_warnings(&plan, sql, row_counts, index_details);
        self.bytecode_rows = bytecode;
        self.plan_lines = plan;
        self.stale = false;
        self.loading = false;
        self.selected_row = 0;
        self.scroll_offset = 0;
    }

    /// Mark as loading to prevent duplicate EXPLAIN tasks.
    pub(crate) fn set_loading(&mut self) {
        self.loading = true;
    }

    /// Clear loading flag on failure (data stays stale).
    pub(crate) fn set_loading_failed(&mut self) {
        self.loading = false;
    }

    /// Get the last query SQL as a reference for use in warning generation.
    pub(crate) fn last_query_ref(&self) -> Option<&str> {
        self.last_query.as_deref()
    }

    /// Number of content rows in the current mode (plan lines + warnings header + warnings).
    fn row_count(&self) -> usize {
        match self.mode {
            ExplainMode::Bytecode => self.bytecode_rows.len(),
            ExplainMode::QueryPlan => {
                let plan = self.plan_lines.len();
                if self.warnings.is_empty() {
                    plan
                } else {
                    // plan lines + separator + warnings (each warning = message line + optional suggestion)
                    plan + 1 + self.warning_line_count()
                }
            }
        }
    }

    /// Total display lines for the warnings section.
    fn warning_line_count(&self) -> usize {
        self.warnings
            .iter()
            .map(|w| if w.suggestion.is_some() { 2 } else { 1 })
            .sum()
    }

    /// Ensure `scroll_offset` keeps `selected_row` visible within `viewport_height` rows.
    fn clamp_scroll(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }
        if self.selected_row < self.scroll_offset {
            self.scroll_offset = self.selected_row;
        } else if self.selected_row >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selected_row + 1 - viewport_height;
        }
    }

    /// Render a centered placeholder message.
    fn render_centered(frame: &mut Frame, inner: Rect, msg: &str, theme: &Theme) {
        let msg_width = UnicodeWidthStr::width(msg) as u16;
        let x = inner.x + inner.width.saturating_sub(msg_width) / 2;
        let y = inner.y + inner.height / 2;
        let msg_area = Rect::new(x, y, msg_width.min(inner.width), 1);
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(theme.border)),
            msg_area,
        );
    }

    /// Build the title string including mode indicator and optional query snippet.
    fn title_text(&self) -> String {
        let mode_label = match self.mode {
            ExplainMode::Bytecode => "Bytecode",
            ExplainMode::QueryPlan => "Query Plan",
        };
        match &self.last_query {
            Some(sql) if !self.stale && !self.loading => {
                // Truncate the SQL for the title bar
                let max_sql_len = 40;
                let truncated = if UnicodeWidthStr::width(sql.as_str()) > max_sql_len {
                    let mut end = 0;
                    let mut w = 0;
                    for (i, ch) in sql.char_indices() {
                        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]));
                        if w + cw > max_sql_len - 1 {
                            break;
                        }
                        w += cw;
                        end = i + ch.len_utf8();
                    }
                    format!("{}\u{2026}", &sql[..end])
                } else {
                    sql.clone()
                };
                format!("EXPLAIN [{mode_label}]: {truncated}")
            }
            _ => format!("EXPLAIN [{mode_label}]"),
        }
    }

    /// Render bytecode rows as a table with fixed columns.
    fn render_bytecode(&mut self, frame: &mut Frame, inner: Rect, theme: &Theme) {
        if self.bytecode_rows.is_empty() {
            Self::render_centered(frame, inner, "No bytecode data", theme);
            return;
        }

        // Reserve 1 row for header
        let header_height: u16 = 1;
        if inner.height <= header_height {
            return;
        }
        let data_height = (inner.height - header_height) as usize;

        self.clamp_scroll(data_height);

        let has_scrollbar = self.bytecode_rows.len() > data_height;
        let content_width = if has_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        // Calculate column widths: fixed minimums, last column gets remaining space
        let fixed_count = BYTECODE_MIN_WIDTHS.len();
        let gap: u16 = 1; // space between columns
        let fixed_total: u16 =
            BYTECODE_MIN_WIDTHS.iter().sum::<u16>() + (fixed_count as u16).saturating_sub(1) * gap;
        let comment_width = content_width.saturating_sub(fixed_total + gap);

        // Render header
        let header_y = inner.y;
        let mut x = inner.x;
        for (i, &header) in BYTECODE_HEADERS.iter().enumerate() {
            let col_w = if i < fixed_count {
                BYTECODE_MIN_WIDTHS[i]
            } else {
                comment_width
            };
            let header_area = Rect::new(
                x,
                header_y,
                col_w.min(content_width.saturating_sub(x - inner.x)),
                1,
            );
            frame.render_widget(
                Paragraph::new(Span::styled(header, theme.header_style)),
                header_area,
            );
            x += col_w + gap;
            if x >= inner.x + content_width {
                break;
            }
        }

        // Render data rows
        let visible_end = (self.scroll_offset + data_height).min(self.bytecode_rows.len());
        for (draw_idx, row_idx) in (self.scroll_offset..visible_end).enumerate() {
            let y = inner.y + header_height + draw_idx as u16;
            let row = &self.bytecode_rows[row_idx];

            let mut x = inner.x;
            for (col_idx, _header) in BYTECODE_HEADERS.iter().enumerate() {
                let col_w = if col_idx < fixed_count {
                    BYTECODE_MIN_WIDTHS[col_idx]
                } else {
                    comment_width
                };
                let cell_text = row.get(col_idx).map_or("", String::as_str);
                let available = col_w.min(content_width.saturating_sub(x - inner.x));
                let cell_area = Rect::new(x, y, available, 1);
                frame.render_widget(
                    Paragraph::new(Span::styled(cell_text, Style::default().fg(theme.fg))),
                    cell_area,
                );
                x += col_w + gap;
                if x >= inner.x + content_width {
                    break;
                }
            }

            // Highlight selected row
            if row_idx == self.selected_row {
                let row_area = Rect::new(inner.x, y, content_width, 1);
                frame.buffer_mut().set_style(row_area, theme.selected_style);
            }
        }

        // Scrollbar
        if has_scrollbar {
            let mut scrollbar_state = ScrollbarState::new(self.bytecode_rows.len())
                .position(self.scroll_offset)
                .viewport_content_length(data_height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                inner,
                &mut scrollbar_state,
            );
        }
    }

    /// Color for a scan type classification.
    fn scan_color(scan: ScanType, theme: &Theme) -> Color {
        match scan {
            ScanType::FullTableScan => theme.error,
            ScanType::TempBTree | ScanType::CorrelatedSubquery => theme.warning,
            ScanType::IndexSeek | ScanType::CoveringIndex | ScanType::RowidLookup => theme.success,
            ScanType::Subquery | ScanType::Unknown => theme.dim,
        }
    }

    /// Color for a warning severity.
    fn severity_color(severity: WarningSeverity, theme: &Theme) -> Color {
        match severity {
            WarningSeverity::Critical => theme.error,
            WarningSeverity::Warning => theme.warning,
            WarningSeverity::Info => theme.dim,
        }
    }

    /// Severity prefix for display.
    fn severity_prefix(severity: WarningSeverity) -> &'static str {
        match severity {
            WarningSeverity::Critical => "\u{26a0} ",
            WarningSeverity::Warning => "\u{25b3} ",
            WarningSeverity::Info => "\u{2139} ",
        }
    }

    /// Build all display lines for `QueryPlan` mode (plan + warnings).
    fn build_plan_display_lines(&self, theme: &Theme) -> Vec<(Line<'static>, bool)> {
        let mut lines: Vec<(Line<'static>, bool)> = Vec::new();

        // Plan lines with color coding
        for (i, plan_line) in self.plan_lines.iter().enumerate() {
            let scan = self
                .plan_classifications
                .get(i)
                .copied()
                .unwrap_or(ScanType::Unknown);
            let color = Self::scan_color(scan, theme);
            lines.push((
                Line::from(Span::styled(plan_line.clone(), Style::default().fg(color))),
                false, // not a suggestion line
            ));
        }

        // Warnings section
        if !self.warnings.is_empty() {
            // Separator
            let separator = "\u{2500}\u{2500} Warnings \u{2500}\u{2500}";
            lines.push((
                Line::from(Span::styled(
                    separator,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                false,
            ));

            for warning in &self.warnings {
                let prefix = Self::severity_prefix(warning.severity);
                let color = Self::severity_color(warning.severity, theme);
                lines.push((
                    Line::from(Span::styled(
                        format!("{prefix}{}", warning.message),
                        Style::default().fg(color),
                    )),
                    false,
                ));
                if let Some(ref suggestion) = warning.suggestion {
                    lines.push((
                        Line::from(Span::styled(
                            format!("  \u{2192} Consider: {suggestion}"),
                            Style::default().fg(theme.accent),
                        )),
                        true, // suggestion line — Enter/y can act on it
                    ));
                }
            }
        }

        lines
    }

    /// Render query plan lines as a scrollable list with color coding and warnings.
    fn render_query_plan(&mut self, frame: &mut Frame, inner: Rect, theme: &Theme) {
        if self.plan_lines.is_empty() {
            Self::render_centered(frame, inner, "No query plan data", theme);
            return;
        }

        let display_lines = self.build_plan_display_lines(theme);
        let total = display_lines.len();
        let viewport_height = inner.height as usize;
        self.clamp_scroll(viewport_height);

        let has_scrollbar = total > viewport_height;
        let content_width = if has_scrollbar {
            inner.width.saturating_sub(1)
        } else {
            inner.width
        };

        let visible_end = (self.scroll_offset + viewport_height).min(total);
        for (draw_idx, line_idx) in (self.scroll_offset..visible_end).enumerate() {
            let y = inner.y + draw_idx as u16;
            let (ref line, _) = display_lines[line_idx];
            let line_area = Rect::new(inner.x, y, content_width, 1);
            frame.render_widget(Paragraph::new(line.clone()), line_area);

            // Highlight selected row
            if line_idx == self.selected_row {
                let row_area = Rect::new(inner.x, y, content_width, 1);
                frame.buffer_mut().set_style(row_area, theme.selected_style);
            }
        }

        // Scrollbar
        if has_scrollbar {
            let mut scrollbar_state = ScrollbarState::new(total)
                .position(self.scroll_offset)
                .viewport_content_length(viewport_height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                inner,
                &mut scrollbar_state,
            );
        }
    }

    /// Get the suggestion text at the current selection, if any.
    fn selected_suggestion(&self) -> Option<String> {
        let display_lines = self.build_plan_display_lines_meta();
        display_lines.get(self.selected_row).and_then(Clone::clone)
    }

    /// Build suggestion metadata parallel to display lines (None = no suggestion, Some = suggestion text).
    fn build_plan_display_lines_meta(&self) -> Vec<Option<String>> {
        let mut meta = Vec::new();

        // Plan lines — no suggestions
        for _ in &self.plan_lines {
            meta.push(None);
        }

        if !self.warnings.is_empty() {
            // Separator
            meta.push(None);
            for warning in &self.warnings {
                meta.push(None); // warning message line
                if let Some(ref suggestion) = warning.suggestion {
                    meta.push(Some(suggestion.clone()));
                }
            }
        }

        meta
    }
}

impl Component for ExplainView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        match (key.modifiers, key.code) {
            // Tab toggles between Bytecode and QueryPlan modes.
            // Returns Some(Action) to consume the key and prevent global Tab from
            // cycling focus. SwitchBottomTab(Explain) is a no-op since we're already
            // on this tab.
            (KeyModifiers::NONE, KeyCode::Tab) => {
                self.mode = match self.mode {
                    ExplainMode::Bytecode => ExplainMode::QueryPlan,
                    ExplainMode::QueryPlan => ExplainMode::Bytecode,
                };
                // Reset scroll position — row counts differ between modes
                self.selected_row = 0;
                self.scroll_offset = 0;
                // Must return Some to consume Tab and prevent global focus cycling
                // (event.rs maps bare Tab → CycleFocus). SwitchBottomTab(Explain)
                // is idempotent since we're already on this tab.
                Some(Action::Nav(NavAction::SwitchBottomTab(BottomTab::Explain)))
            }
            // Enter: generate EXPLAIN when stale, or populate editor with suggestion
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.stale
                    && !self.loading
                    && let Some(sql) = self.last_query.clone()
                {
                    // loading flag set by dispatch calling set_loading()
                    return Some(Action::Admin(AdminAction::GenerateExplain(sql)));
                }
                // If on a suggestion line in QueryPlan mode, populate editor
                if self.mode == ExplainMode::QueryPlan
                    && let Some(suggestion) = self.selected_suggestion()
                {
                    return Some(Action::Editor(EditorAction::PopulateEditor(suggestion)));
                }
                None
            }
            // y: copy suggestion to clipboard
            (KeyModifiers::NONE, KeyCode::Char('y')) => {
                if self.mode == ExplainMode::QueryPlan
                    && let Some(suggestion) = self.selected_suggestion()
                {
                    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&suggestion)) {
                        Ok(()) => {
                            return Some(Action::SetTransient(
                                "Copied index suggestion to clipboard".to_string(),
                                false,
                            ));
                        }
                        Err(e) => {
                            return Some(Action::SetTransient(
                                format!("Clipboard error: {e}"),
                                true,
                            ));
                        }
                    }
                }
                None
            }
            // Esc releases focus.
            (KeyModifiers::NONE, KeyCode::Esc) => {
                Some(Action::Nav(NavAction::CycleFocus(Direction::Forward)))
            }
            // Navigation: j/Down scroll down
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                let count = self.row_count();
                if count > 0 && self.selected_row < count - 1 {
                    self.selected_row += 1;
                }
                None
            }
            // Navigation: k/Up scroll up
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                self.selected_row = self.selected_row.saturating_sub(1);
                None
            }
            // g: jump to first
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected_row = 0;
                self.scroll_offset = 0;
                None
            }
            // G: jump to last
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                let count = self.row_count();
                if count > 0 {
                    self.selected_row = count - 1;
                }
                // scroll_offset adjusted by clamp_scroll() on next render()
                None
            }
            _ => None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, _area: Rect) -> Option<Action> {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.selected_row = self.selected_row.saturating_sub(1);
                Some(Action::Consumed)
            }
            MouseEventKind::ScrollDown => {
                let count = self.row_count();
                if count > 0 && self.selected_row < count - 1 {
                    self.selected_row += 1;
                }
                Some(Action::Consumed)
            }
            _ => None,
        }
    }

    fn update(&mut self, _action: &Action) {
        // ExplainCompleted is handled by dispatch (needs schema cache context)
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let title = self.title_text();
        let block = super::panel_block(&title, focused, theme);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Empty states (checked in priority order):
        // 1. No query has been executed yet
        if self.last_query.is_none() {
            Self::render_centered(
                frame,
                inner,
                "No query to explain \u{2014} execute a query first",
                theme,
            );
            return;
        }

        // 2. Currently loading
        if self.loading {
            Self::render_centered(frame, inner, "Generating EXPLAIN...", theme);
            return;
        }

        // 3. Data is stale (query changed since last EXPLAIN)
        if self.stale {
            Self::render_centered(frame, inner, "Press Enter to generate EXPLAIN", theme);
            return;
        }

        // Render based on current mode
        match self.mode {
            ExplainMode::Bytecode => self.render_bytecode(frame, inner, theme),
            ExplainMode::QueryPlan => self.render_query_plan(frame, inner, theme),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // --- classify_plan_line tests ---

    #[test]
    fn classify_full_table_scan() {
        assert_eq!(
            classify_plan_line("SCAN TABLE orders"),
            ScanType::FullTableScan
        );
    }

    #[test]
    fn classify_full_table_scan_with_prefix() {
        assert_eq!(
            classify_plan_line("|--SCAN TABLE orders"),
            ScanType::FullTableScan
        );
    }

    #[test]
    fn classify_index_seek() {
        assert_eq!(
            classify_plan_line("SEARCH TABLE orders USING INDEX idx_orders_date (date>?)"),
            ScanType::IndexSeek
        );
    }

    #[test]
    fn classify_covering_index() {
        assert_eq!(
            classify_plan_line("SEARCH TABLE t USING COVERING INDEX idx_t_a (a=?)"),
            ScanType::CoveringIndex
        );
    }

    #[test]
    fn classify_rowid_lookup() {
        assert_eq!(
            classify_plan_line("SEARCH TABLE t USING INTEGER PRIMARY KEY (rowid=?)"),
            ScanType::RowidLookup
        );
    }

    #[test]
    fn classify_temp_btree() {
        assert_eq!(
            classify_plan_line("USE TEMP B-TREE FOR ORDER BY"),
            ScanType::TempBTree
        );
    }

    #[test]
    fn classify_correlated_subquery() {
        assert_eq!(
            classify_plan_line("CORRELATED SCALAR SUBQUERY 1"),
            ScanType::CorrelatedSubquery
        );
    }

    #[test]
    fn classify_scan_without_table_keyword() {
        // SQLite omits TABLE when aliases are used: "SCAN employees AS e"
        assert_eq!(
            classify_plan_line("SCAN employees AS e"),
            ScanType::FullTableScan
        );
    }

    #[test]
    fn classify_search_without_table_keyword() {
        // "SEARCH p USING INDEX idx_projects_department (department_id=?)"
        assert_eq!(
            classify_plan_line("SEARCH p USING INDEX idx_projects_department (department_id=?)"),
            ScanType::IndexSeek
        );
    }

    #[test]
    fn classify_scan_bare_table_name() {
        // No TABLE keyword, no alias: "SCAN orders"
        assert_eq!(classify_plan_line("SCAN orders"), ScanType::FullTableScan);
    }

    #[test]
    fn classify_unknown_line() {
        assert_eq!(
            classify_plan_line("SOME NEW TURSO THING"),
            ScanType::Unknown
        );
    }

    // --- generate_plan_warnings tests ---

    #[test]
    fn generate_warnings_for_full_scan() {
        let plan_lines = vec!["SCAN TABLE orders".to_string()];
        let sql = "SELECT * FROM orders WHERE status = 'active'";
        let mut row_counts = HashMap::new();
        row_counts.insert(TableId::new("orders"), 50_000u64);
        let warnings = generate_plan_warnings(&plan_lines, sql, &row_counts, &HashMap::new());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, WarningSeverity::Critical);
        assert!(warnings[0].message.contains("Full table scan"));
        assert!(
            warnings[0]
                .suggestion
                .as_ref()
                .unwrap()
                .contains("CREATE INDEX")
        );
        assert!(warnings[0].suggestion.as_ref().unwrap().contains("status"));
    }

    #[test]
    fn generate_warnings_small_table_info_severity() {
        let plan_lines = vec!["SCAN TABLE small".to_string()];
        let sql = "SELECT * FROM small WHERE x = 1";
        let mut row_counts = HashMap::new();
        row_counts.insert(TableId::new("small"), 500u64);
        let warnings = generate_plan_warnings(&plan_lines, sql, &row_counts, &HashMap::new());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, WarningSeverity::Info);
    }

    #[test]
    fn no_warnings_for_index_seek() {
        let plan_lines = vec!["SEARCH TABLE orders USING INDEX idx_date (date>?)".to_string()];
        let sql = "SELECT * FROM orders WHERE date > '2026-01-01'";
        let warnings = generate_plan_warnings(&plan_lines, sql, &HashMap::new(), &HashMap::new());
        assert!(warnings.is_empty());
    }

    #[test]
    fn temp_btree_warning() {
        let plan_lines = vec![
            "SCAN TABLE t".to_string(),
            "USE TEMP B-TREE FOR ORDER BY".to_string(),
        ];
        let sql = "SELECT * FROM t ORDER BY name";
        let warnings = generate_plan_warnings(&plan_lines, sql, &HashMap::new(), &HashMap::new());
        assert_eq!(warnings.len(), 2);
        assert!(warnings[1].message.contains("Temp B-Tree"));
    }

    // --- extract_where_columns tests ---

    #[test]
    fn extract_simple_where() {
        let cols = extract_where_columns("SELECT * FROM t WHERE status = 'active'");
        assert_eq!(cols, vec!["status"]);
    }

    #[test]
    fn extract_multiple_where() {
        let cols = extract_where_columns("SELECT * FROM t WHERE a = 1 AND b = 2");
        assert!(cols.contains(&"a".to_string()));
        assert!(cols.contains(&"b".to_string()));
    }

    #[test]
    fn extract_order_by() {
        let cols = extract_where_columns("SELECT * FROM t ORDER BY name");
        assert!(cols.contains(&"name".to_string()));
    }

    #[test]
    fn extract_no_where() {
        let cols = extract_where_columns("SELECT * FROM t");
        assert!(cols.is_empty());
    }

    #[test]
    fn extract_table_dot_column() {
        let cols = extract_where_columns("SELECT * FROM t WHERE t.status = 1");
        assert!(cols.contains(&"status".to_string()));
    }

    // --- extract_table_from_plan tests ---

    #[test]
    fn extract_table_scan() {
        assert_eq!(
            extract_table_from_plan("SCAN TABLE orders"),
            Some("orders".to_string())
        );
    }

    #[test]
    fn extract_table_search() {
        assert_eq!(
            extract_table_from_plan("SEARCH TABLE users USING INDEX idx_email"),
            Some("users".to_string())
        );
    }

    #[test]
    fn extract_table_scan_with_alias() {
        // "SCAN employees AS e" → "employees"
        assert_eq!(
            extract_table_from_plan("SCAN employees AS e"),
            Some("employees".to_string())
        );
    }

    #[test]
    fn extract_table_search_alias_only() {
        // "SEARCH p USING INDEX ..." — returns alias "p", not the real table name
        // (the real name isn't in the plan line when aliased this way)
        assert_eq!(
            extract_table_from_plan("SEARCH p USING INDEX idx_projects_dept (dept_id=?)"),
            Some("p".to_string())
        );
    }

    #[test]
    fn extract_table_bare_scan() {
        assert_eq!(
            extract_table_from_plan("SCAN orders"),
            Some("orders".to_string())
        );
    }

    // --- build_index_suggestion tests ---

    #[test]
    fn suggestion_skips_already_indexed_column() {
        let sql = "SELECT * FROM t WHERE a = 1";
        let mut indexes = HashMap::new();
        indexes.insert(
            TableId::new("t"),
            vec![tursotui_db::IndexDetail {
                name: "idx_t_a".to_string(),
                table_name: "t".to_string(),
                unique: false,
                columns: vec!["a".to_string()],
            }],
        );
        let result = build_index_suggestion(sql, Some("t"), &indexes);
        assert!(
            result.is_none(),
            "already indexed column should not get a suggestion"
        );
    }

    #[test]
    fn suggestion_for_unindexed_column() {
        let sql = "SELECT * FROM orders WHERE status = 'active'";
        let result = build_index_suggestion(sql, Some("orders"), &HashMap::new());
        assert!(result.is_some());
        let suggestion = result.unwrap();
        assert!(suggestion.contains("CREATE INDEX"));
        assert!(suggestion.contains("status"));
        assert!(suggestion.contains("orders"));
    }
}
