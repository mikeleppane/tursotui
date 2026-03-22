//! Schema-aware SQL autocomplete engine.
//!
//! Pure logic — no UI, no ratatui dependency. Takes buffer text, cursor position,
//! and schema cache; returns ranked completion candidates.

#![allow(
    dead_code,
    reason = "module wired incrementally — public API used when editor integration lands"
)]

use std::collections::HashMap;

use crate::app::SchemaCache;

/// A single completion candidate.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    /// The text to insert at the cursor.
    pub(crate) text: String,
    /// What kind of thing this candidate represents.
    pub(crate) kind: CandidateKind,
    /// Extra info shown dimmed in the popup (type, parent table, etc.).
    pub(crate) detail: String,
    /// Ranking score (higher = better match). Used for sorting.
    pub(crate) score: u32,
}

/// The kind of completion candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateKind {
    Table,
    View,
    Column,
    Keyword,
    Function,
}

/// The syntactic context at the cursor, determining which candidates to offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompletionContext {
    /// After FROM/JOIN/INTO/UPDATE/TABLE — suggest table and view names.
    TableName,
    /// After SELECT/WHERE/ON/SET/AND/OR/HAVING/ORDER BY/GROUP BY — suggest columns.
    /// `tables` lists the table names referenced in the query (resolved from aliases).
    ColumnName { tables: Vec<String> },
    /// After `alias.` — suggest columns from one specific table.
    QualifiedColumn { table: String },
    /// Start of statement or after `;` — suggest keywords.
    Keyword,
    /// General expression context (after `(`, in function args) — columns + functions.
    Expression { tables: Vec<String> },
    /// After AS — user is defining an alias, no suggestions appropriate.
    NoSuggestion,
}

/// SQL keywords that introduce a table name context.
const TABLE_CONTEXT_KEYWORDS: &[&str] = &[
    "FROM", "JOIN", "INTO", "UPDATE", "TABLE", "INNER", "LEFT", "RIGHT", "OUTER", "CROSS",
    "NATURAL",
];

/// SQL keywords that introduce a column/expression context.
const COLUMN_CONTEXT_KEYWORDS: &[&str] = &[
    "SELECT", "WHERE", "ON", "SET", "AND", "OR", "HAVING", "ORDER", "GROUP", "BY",
];

/// SQL keywords offered at statement start.
const STATEMENT_KEYWORDS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "WITH", "EXPLAIN", "PRAGMA",
    "BEGIN", "COMMIT", "ROLLBACK", "ATTACH", "DETACH", "VACUUM", "ANALYZE", "REINDEX",
];

/// SQL clause keywords offered in expression context.
const CLAUSE_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "OUTER",
    "CROSS",
    "ON",
    "AS",
    "IN",
    "IS",
    "NOT",
    "NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "AND",
    "OR",
    "ORDER",
    "BY",
    "GROUP",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "DISTINCT",
    "ALL",
    "ASC",
    "DESC",
    "INTO",
    "VALUES",
    "SET",
    "DEFAULT",
];

/// SQL functions offered in expression context.
const SQL_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "TOTAL",
    "GROUP_CONCAT",
    "ABS",
    "COALESCE",
    "IFNULL",
    "IIF",
    "NULLIF",
    "TYPEOF",
    "LENGTH",
    "LOWER",
    "UPPER",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "REPLACE",
    "SUBSTR",
    "SUBSTRING",
    "INSTR",
    "HEX",
    "QUOTE",
    "RANDOM",
    "ROUND",
    "CAST",
    "DATE",
    "TIME",
    "DATETIME",
    "STRFTIME",
    "JULIANDAY",
    "JSON",
    "JSON_EXTRACT",
    "JSON_ARRAY",
    "JSON_OBJECT",
    "JSON_TYPE",
    "JSON_VALID",
    "JSON_GROUP_ARRAY",
    "JSON_GROUP_OBJECT",
    "JSON_EACH",
    "JSON_TREE",
    "PRINTF",
    "UNICODE",
    "ZEROBLOB",
    "RANDOMBLOB",
    "LIKELIHOOD",
    "LIKELY",
    "UNLIKELY",
];

// ─── Context Detection ──────────────────────────────────────────────────────

/// Detect the completion context from the buffer text and cursor position.
///
/// `cursor_row` and `cursor_col` are zero-based indices into the buffer lines.
pub(crate) fn detect_context(
    lines: &[String],
    cursor_row: usize,
    cursor_col: usize,
    schema: &SchemaCache,
) -> (CompletionContext, String) {
    // Flatten buffer up to cursor for context detection (what keyword is before cursor)
    let text_before_cursor = flatten_to_cursor(lines, cursor_row, cursor_col);
    // Flatten the ENTIRE buffer for alias resolution — FROM/JOIN clauses typically
    // appear after SELECT, so we must scan the whole query to find alias mappings.
    let full_text = flatten_all(lines);

    // Extract the prefix (partial word being typed)
    let prefix = extract_prefix(&text_before_cursor);

    // Check for qualified column: `alias.prefix`
    if let Some(qualifier) = extract_qualifier(&text_before_cursor) {
        let alias_map = build_alias_map(&full_text, schema);
        let resolved = alias_map
            .get(&qualifier.to_lowercase())
            .cloned()
            .unwrap_or(qualifier);
        return (
            CompletionContext::QualifiedColumn { table: resolved },
            prefix,
        );
    }

    // Find the last significant keyword before the prefix
    let before_prefix = text_before_cursor
        .strip_suffix(prefix.as_str())
        .unwrap_or(&text_before_cursor)
        .trim_end();

    let last_keyword = extract_last_keyword(before_prefix);

    let context = match last_keyword.as_deref() {
        Some("AS") => CompletionContext::NoSuggestion,
        Some(kw) if TABLE_CONTEXT_KEYWORDS.contains(&kw) => CompletionContext::TableName,
        Some(kw) if COLUMN_CONTEXT_KEYWORDS.contains(&kw) => {
            let tables = extract_referenced_tables(&full_text, schema);
            CompletionContext::ColumnName { tables }
        }
        _ => {
            if is_at_statement_start(before_prefix) {
                CompletionContext::Keyword
            } else if is_in_table_list(before_prefix) {
                // After a comma in a FROM/JOIN clause: `FROM t1, |` → still table context
                CompletionContext::TableName
            } else {
                let tables = extract_referenced_tables(&full_text, schema);
                CompletionContext::Expression { tables }
            }
        }
    };

    (context, prefix)
}

/// Flatten buffer lines up to cursor position into a single string.
fn flatten_to_cursor(lines: &[String], row: usize, col: usize) -> String {
    let mut result = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > row {
            break;
        }
        if i > 0 {
            result.push(' ');
        }
        if i == row {
            let byte_idx = line
                .char_indices()
                .nth(col)
                .map_or(line.len(), |(idx, _)| idx);
            result.push_str(&line[..byte_idx]);
        } else {
            result.push_str(line);
        }
    }
    result
}

/// Flatten ALL buffer lines into a single string (for alias resolution).
/// Unlike `flatten_to_cursor`, this includes the entire buffer so that
/// FROM/JOIN clauses after the cursor are also scanned.
fn flatten_all(lines: &[String]) -> String {
    lines.join(" ")
}

/// Extract the partial word being typed (the prefix to filter candidates by).
/// A word is alphanumeric + underscore.
fn extract_prefix(text: &str) -> String {
    text.chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Check if the character before the prefix is a `.` and extract the qualifier.
/// Returns `Some("alias")` for `alias.prefix`, `None` otherwise.
fn extract_qualifier(text: &str) -> Option<String> {
    let before_prefix: &str = text.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    if !before_prefix.ends_with('.') {
        return None;
    }
    let before_dot = &before_prefix[..before_prefix.len() - 1];
    let qualifier: String = before_dot
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if qualifier.is_empty() {
        None
    } else {
        Some(qualifier)
    }
}

/// Extract the last SQL keyword from the text (case-insensitive, returned uppercase).
fn extract_last_keyword(text: &str) -> Option<String> {
    let trimmed = text.trim_end();
    let last_word: String = trimmed
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if last_word.is_empty() {
        return None;
    }
    let upper = last_word.to_uppercase();
    if TABLE_CONTEXT_KEYWORDS.contains(&upper.as_str())
        || COLUMN_CONTEXT_KEYWORDS.contains(&upper.as_str())
        || upper == "AS"
    {
        Some(upper)
    } else {
        None
    }
}

/// Check if the cursor is at the start of a statement (beginning of buffer or after `;`).
fn is_at_statement_start(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.is_empty() || trimmed.ends_with(';')
}

/// Check if cursor is in a comma-separated table list (e.g., `FROM t1, |`).
/// Scans backward from the trailing comma through words (table names, aliases,
/// AS keywords) and commas, looking for a FROM/JOIN keyword.
fn is_in_table_list(text: &str) -> bool {
    let trimmed = text.trim_end();
    if !trimmed.ends_with(',') {
        return false;
    }
    // Strip the comma and scan backward through tokens
    let mut remaining = trimmed[..trimmed.len() - 1].trim_end();
    loop {
        if remaining.is_empty() {
            return false;
        }
        // Extract the last word
        let word_start = remaining
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map_or(0, |i| i + 1);
        let word = &remaining[word_start..];
        if word.is_empty() {
            return false;
        }
        let upper = word.to_uppercase();

        // Found a table-context keyword — we're in a table list
        if TABLE_CONTEXT_KEYWORDS.contains(&upper.as_str()) {
            return true;
        }

        // If it's a non-table keyword (SELECT, WHERE, etc.), stop — not in a table list
        if COLUMN_CONTEXT_KEYWORDS.contains(&upper.as_str()) || upper == "AS" {
            return false;
        }

        // Otherwise it's a table name or alias — keep scanning backward
        remaining = remaining[..word_start].trim_end();

        // Skip commas between table entries
        if let Some(stripped) = remaining.strip_suffix(',') {
            remaining = stripped.trim_end();
        }
    }
}

// ─── Alias Resolution ───────────────────────────────────────────────────────

/// Build a map of alias -> table name from the query text.
///
/// Recognizes patterns:
/// - `FROM table alias` / `FROM table AS alias`
/// - `JOIN table alias` / `JOIN table AS alias`
///
/// Case-insensitive matching. Returns lowercase alias keys -> original table names.
fn build_alias_map(text: &str, schema: &SchemaCache) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let tokens = tokenize_simple(text);

    let table_names: Vec<String> = schema
        .entries
        .iter()
        .filter(|e| e.obj_type == "table" || e.obj_type == "view")
        .map(|e| e.name.to_lowercase())
        .collect();

    let mut i = 0;
    while i < tokens.len() {
        let upper = tokens[i].to_uppercase();
        if TABLE_CONTEXT_KEYWORDS.contains(&upper.as_str()) {
            // After a table-context keyword, consume one or more comma-separated tables.
            i += 1;
            i = consume_table_list(&tokens, i, &table_names, &mut map);
        } else {
            i += 1;
        }
    }
    map
}

/// Consume a comma-separated list of table references starting at `i`.
/// Returns the index past the last consumed token.
fn consume_table_list(
    tokens: &[String],
    mut i: usize,
    table_names: &[String],
    map: &mut HashMap<String, String>,
) -> usize {
    loop {
        if i >= tokens.len() || !table_names.contains(&tokens[i].to_lowercase()) {
            break;
        }
        let table = tokens[i].clone();
        i += 1;

        // Check for alias: `table AS alias` or `table alias`
        if i < tokens.len() {
            if tokens[i].to_uppercase() == "AS" {
                if i + 1 < tokens.len() {
                    let alias = &tokens[i + 1];
                    map.insert(alias.to_lowercase(), table.clone());
                    i += 2;
                }
            } else {
                let next = &tokens[i];
                let next_upper = next.to_uppercase();
                if !is_sql_keyword(&next_upper)
                    && next != ","
                    && next != "("
                    && next != ")"
                    && next != ";"
                {
                    map.insert(next.to_lowercase(), table.clone());
                    i += 1;
                }
            }
        }

        map.insert(table.to_lowercase(), table);

        // If next token is a comma, skip it and continue for more tables
        if i < tokens.len() && tokens[i] == "," {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Simple word tokenizer for alias resolution. Splits on whitespace and common
/// SQL punctuation, preserving words and single-char punctuation as tokens.
/// Handles SQL escaped quotes (`''` and `""`) inside string literals.
fn tokenize_simple(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut string_char = '\'';
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];
        if in_string {
            if ch == string_char {
                // Check for escaped quote (doubled: '' or "")
                if i + 1 < len && chars[i + 1] == string_char {
                    i += 2; // skip both quotes, stay in string
                } else {
                    in_string = false;
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                in_string = true;
                string_char = ch;
            }
            ' ' | '\t' | '\n' | '\r' => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            ',' | '(' | ')' | ';' | '.' => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                tokens.push(ch.to_string());
            }
            _ => current.push(ch),
        }
        i += 1;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Check if a string (already uppercased) is a SQL keyword.
fn is_sql_keyword(word: &str) -> bool {
    TABLE_CONTEXT_KEYWORDS.contains(&word)
        || COLUMN_CONTEXT_KEYWORDS.contains(&word)
        || matches!(
            word,
            "AS" | "ON"
                | "WHERE"
                | "HAVING"
                | "LIMIT"
                | "OFFSET"
                | "ORDER"
                | "GROUP"
                | "UNION"
                | "INTERSECT"
                | "EXCEPT"
                | "VALUES"
                | "SET"
                | "DEFAULT"
                | "NOT"
                | "NULL"
                | "AND"
                | "OR"
                | "IN"
                | "IS"
                | "LIKE"
                | "BETWEEN"
                | "EXISTS"
                | "CASE"
                | "WHEN"
                | "THEN"
                | "ELSE"
                | "END"
        )
}

/// Extract table names referenced in the query (resolved from aliases).
fn extract_referenced_tables(text: &str, schema: &SchemaCache) -> Vec<String> {
    let alias_map = build_alias_map(text, schema);
    let mut tables: Vec<String> = alias_map.values().cloned().collect();
    tables.sort();
    tables.dedup();
    tables
}

// ─── Candidate Generation ───────────────────────────────────────────────────

/// Generate ranked completion candidates for the given context and prefix.
pub(crate) fn generate_candidates(
    context: &CompletionContext,
    prefix: &str,
    schema: &SchemaCache,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let prefix_lower = prefix.to_lowercase();

    match context {
        CompletionContext::TableName => {
            for entry in &schema.entries {
                if (entry.obj_type == "table" || entry.obj_type == "view")
                    && matches_prefix(&entry.name, &prefix_lower)
                {
                    let kind = if entry.obj_type == "view" {
                        CandidateKind::View
                    } else {
                        CandidateKind::Table
                    };
                    candidates.push(Candidate {
                        text: entry.name.clone(),
                        kind,
                        detail: entry.obj_type.clone(),
                        score: score_match(&entry.name, &prefix_lower),
                    });
                }
            }
        }
        CompletionContext::ColumnName { tables } | CompletionContext::Expression { tables } => {
            add_column_candidates(&mut candidates, tables, &prefix_lower, schema);
            add_function_candidates(&mut candidates, &prefix_lower, 50);
            add_keyword_candidates(&mut candidates, CLAUSE_KEYWORDS, &prefix_lower, 10);
        }
        CompletionContext::QualifiedColumn { table } => {
            if let Some(cols) = schema.get_columns(table) {
                for col in cols {
                    if matches_prefix(&col.name, &prefix_lower) {
                        candidates.push(Candidate {
                            text: col.name.clone(),
                            kind: CandidateKind::Column,
                            detail: col.col_type.clone(),
                            score: score_match(&col.name, &prefix_lower) + 200,
                        });
                    }
                }
            }
        }
        CompletionContext::Keyword => {
            add_keyword_candidates(&mut candidates, STATEMENT_KEYWORDS, &prefix_lower, 100);
        }
        CompletionContext::NoSuggestion => {}
    }

    candidates.sort_by(|a, b| b.score.cmp(&a.score));
    candidates
}

/// Check if a name matches the prefix (case-insensitive prefix match).
fn matches_prefix(name: &str, prefix_lower: &str) -> bool {
    if prefix_lower.is_empty() {
        return true;
    }
    name.to_lowercase().starts_with(prefix_lower)
}

/// Score a candidate match. Higher = better.
fn score_match(name: &str, prefix_lower: &str) -> u32 {
    if prefix_lower.is_empty() {
        return 100;
    }
    let name_lower = name.to_lowercase();
    if name_lower == *prefix_lower {
        300 // exact match
    } else if name_lower.starts_with(prefix_lower) {
        200 // prefix match
    } else {
        50 // fallback
    }
}

fn add_column_candidates(
    candidates: &mut Vec<Candidate>,
    tables: &[String],
    prefix_lower: &str,
    schema: &SchemaCache,
) {
    for table in tables {
        if let Some(cols) = schema.get_columns(table) {
            for col in cols {
                if matches_prefix(&col.name, prefix_lower) {
                    candidates.push(Candidate {
                        text: col.name.clone(),
                        kind: CandidateKind::Column,
                        detail: format!("{} ({})", col.col_type, table),
                        score: score_match(&col.name, prefix_lower) + 100,
                    });
                }
            }
        }
    }
}

fn add_function_candidates(candidates: &mut Vec<Candidate>, prefix_lower: &str, base_score: u32) {
    for &func in SQL_FUNCTIONS {
        if matches_prefix(func, prefix_lower) {
            candidates.push(Candidate {
                text: func.to_string(),
                kind: CandidateKind::Function,
                detail: "function".into(),
                score: score_match(func, prefix_lower).saturating_sub(50) + base_score,
            });
        }
    }
}

fn add_keyword_candidates(
    candidates: &mut Vec<Candidate>,
    keywords: &[&str],
    prefix_lower: &str,
    base_score: u32,
) {
    for &kw in keywords {
        if matches_prefix(kw, prefix_lower) {
            candidates.push(Candidate {
                text: kw.to_string(),
                kind: CandidateKind::Keyword,
                detail: "keyword".into(),
                score: score_match(kw, prefix_lower).saturating_sub(100) + base_score,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SchemaCache;
    use crate::db::{ColumnInfo, SchemaEntry};

    fn test_schema() -> SchemaCache {
        SchemaCache {
            entries: vec![
                SchemaEntry {
                    obj_type: "table".into(),
                    name: "users".into(),
                    tbl_name: "users".into(),
                    sql: None,
                },
                SchemaEntry {
                    obj_type: "table".into(),
                    name: "orders".into(),
                    tbl_name: "orders".into(),
                    sql: None,
                },
                SchemaEntry {
                    obj_type: "view".into(),
                    name: "active_users".into(),
                    tbl_name: "active_users".into(),
                    sql: None,
                },
            ],
            columns: HashMap::from([
                (
                    "users".into(),
                    vec![
                        ColumnInfo {
                            name: "id".into(),
                            col_type: "INTEGER".into(),
                            notnull: true,
                            default_value: None,
                            pk: true,
                        },
                        ColumnInfo {
                            name: "name".into(),
                            col_type: "TEXT".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                        ColumnInfo {
                            name: "email".into(),
                            col_type: "TEXT".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                    ],
                ),
                (
                    "orders".into(),
                    vec![
                        ColumnInfo {
                            name: "id".into(),
                            col_type: "INTEGER".into(),
                            notnull: true,
                            default_value: None,
                            pk: true,
                        },
                        ColumnInfo {
                            name: "user_id".into(),
                            col_type: "INTEGER".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                        ColumnInfo {
                            name: "total".into(),
                            col_type: "REAL".into(),
                            notnull: false,
                            default_value: None,
                            pk: false,
                        },
                    ],
                ),
            ]),
            fully_loaded: true,
        }
    }

    // ─── Context detection ──────────────────────────────────────────────

    #[test]
    fn context_after_from() {
        let schema = test_schema();
        let lines = vec!["SELECT * FROM ".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 14, &schema);
        assert_eq!(ctx, CompletionContext::TableName);
        assert_eq!(prefix, "");
    }

    #[test]
    fn context_after_from_with_prefix() {
        let schema = test_schema();
        let lines = vec!["SELECT * FROM us".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 16, &schema);
        assert_eq!(ctx, CompletionContext::TableName);
        assert_eq!(prefix, "us");
    }

    #[test]
    fn context_after_select() {
        let schema = test_schema();
        let lines = vec!["SELECT ".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 7, &schema);
        assert!(matches!(ctx, CompletionContext::ColumnName { .. }));
        assert_eq!(prefix, "");
    }

    #[test]
    fn context_after_where() {
        let schema = test_schema();
        let lines = vec!["SELECT * FROM users WHERE na".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 28, &schema);
        assert!(matches!(
            ctx,
            CompletionContext::ColumnName { tables } if tables.contains(&"users".to_string())
        ));
        assert_eq!(prefix, "na");
    }

    #[test]
    fn context_qualified_column() {
        let schema = test_schema();
        // FROM clause is on line 1 (after cursor on line 0) — full buffer scan resolves it
        let lines = vec!["SELECT u.na".into(), "FROM users u".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 11, &schema);
        assert!(matches!(ctx, CompletionContext::QualifiedColumn { table } if table == "users"));
        assert_eq!(prefix, "na");
    }

    #[test]
    fn context_statement_start() {
        let schema = test_schema();
        let lines = vec!["SEL".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 3, &schema);
        assert_eq!(ctx, CompletionContext::Keyword);
        assert_eq!(prefix, "SEL");
    }

    #[test]
    fn context_after_semicolon() {
        let schema = test_schema();
        let lines = vec!["SELECT 1;".into(), "SEL".into()];
        let (ctx, prefix) = detect_context(&lines, 1, 3, &schema);
        assert_eq!(ctx, CompletionContext::Keyword);
        assert_eq!(prefix, "SEL");
    }

    #[test]
    fn context_after_as_suppresses() {
        let schema = test_schema();
        let lines = vec!["SELECT name AS ".into()];
        let (ctx, _) = detect_context(&lines, 0, 15, &schema);
        assert_eq!(ctx, CompletionContext::NoSuggestion);
    }

    #[test]
    fn context_after_comma_in_from() {
        let schema = test_schema();
        let lines = vec!["SELECT * FROM users, ".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 21, &schema);
        assert_eq!(ctx, CompletionContext::TableName);
        assert_eq!(prefix, "");
    }

    #[test]
    fn context_after_comma_in_from_with_prefix() {
        let schema = test_schema();
        let lines = vec!["SELECT * FROM users, or".into()];
        let (ctx, prefix) = detect_context(&lines, 0, 23, &schema);
        assert_eq!(ctx, CompletionContext::TableName);
        assert_eq!(prefix, "or");
    }

    #[test]
    fn context_after_comma_with_alias() {
        let schema = test_schema();
        let lines = vec!["SELECT * FROM users u, ".into()];
        let (ctx, _) = detect_context(&lines, 0, 23, &schema);
        assert_eq!(ctx, CompletionContext::TableName);
    }

    // ─── Helper functions ───────────────────────────────────────────────

    #[test]
    fn extract_prefix_basic() {
        assert_eq!(extract_prefix("SELECT * FROM us"), "us");
        assert_eq!(extract_prefix("SELECT "), "");
        assert_eq!(extract_prefix("user_id"), "user_id");
    }

    #[test]
    fn extract_qualifier_basic() {
        assert_eq!(extract_qualifier("u.na"), Some("u".into()));
        assert_eq!(extract_qualifier("users."), Some("users".into()));
        assert_eq!(extract_qualifier("SELECT "), None);
        assert_eq!(extract_qualifier(".name"), None);
    }

    #[test]
    fn tokenizer_handles_escaped_quotes() {
        let tokens = tokenize_simple("WHERE name = 'O''Brien' AND id = 1");
        // The string 'O''Brien' should be skipped entirely
        assert!(tokens.contains(&"WHERE".into()));
        assert!(tokens.contains(&"name".into()));
        assert!(tokens.contains(&"AND".into()));
        assert!(tokens.contains(&"id".into()));
        // The string content should NOT appear as tokens
        assert!(!tokens.iter().any(|t| t.contains("Brien")));
    }

    // ─── Alias resolution ───────────────────────────────────────────────

    #[test]
    fn alias_map_from_as() {
        let schema = test_schema();
        let text = "SELECT * FROM users AS u JOIN orders AS o";
        let map = build_alias_map(text, &schema);
        assert_eq!(map.get("u"), Some(&"users".to_string()));
        assert_eq!(map.get("o"), Some(&"orders".to_string()));
    }

    #[test]
    fn alias_map_implicit() {
        let schema = test_schema();
        let text = "SELECT * FROM users u JOIN orders o";
        let map = build_alias_map(text, &schema);
        assert_eq!(map.get("u"), Some(&"users".to_string()));
        assert_eq!(map.get("o"), Some(&"orders".to_string()));
    }

    #[test]
    fn alias_map_comma_separated() {
        let schema = test_schema();
        let text = "SELECT * FROM users, orders WHERE";
        let map = build_alias_map(text, &schema);
        assert_eq!(map.get("users"), Some(&"users".to_string()));
        assert_eq!(map.get("orders"), Some(&"orders".to_string()));
    }

    #[test]
    fn alias_map_comma_with_aliases() {
        let schema = test_schema();
        let text = "SELECT * FROM users u, orders o WHERE";
        let map = build_alias_map(text, &schema);
        assert_eq!(map.get("u"), Some(&"users".to_string()));
        assert_eq!(map.get("o"), Some(&"orders".to_string()));
    }

    #[test]
    fn alias_map_no_alias() {
        let schema = test_schema();
        let text = "SELECT * FROM users WHERE";
        let map = build_alias_map(text, &schema);
        assert_eq!(map.get("users"), Some(&"users".to_string()));
    }

    // ─── Candidate generation ───────────────────────────────────────────

    #[test]
    fn candidates_table_name() {
        let schema = test_schema();
        let candidates = generate_candidates(&CompletionContext::TableName, "us", &schema);
        assert!(candidates.iter().any(|c| c.text == "users"));
        assert!(!candidates.iter().any(|c| c.text == "orders"));
    }

    #[test]
    fn candidates_column_from_tables() {
        let schema = test_schema();
        let candidates = generate_candidates(
            &CompletionContext::ColumnName {
                tables: vec!["users".into()],
            },
            "na",
            &schema,
        );
        assert!(
            candidates
                .iter()
                .any(|c| c.text == "name" && c.kind == CandidateKind::Column)
        );
    }

    #[test]
    fn candidates_qualified_column() {
        let schema = test_schema();
        let candidates = generate_candidates(
            &CompletionContext::QualifiedColumn {
                table: "users".into(),
            },
            "",
            &schema,
        );
        assert_eq!(
            candidates
                .iter()
                .filter(|c| c.kind == CandidateKind::Column)
                .count(),
            3 // id, name, email
        );
    }

    #[test]
    fn candidates_keyword_at_start() {
        let schema = test_schema();
        let candidates = generate_candidates(&CompletionContext::Keyword, "SEL", &schema);
        assert!(candidates.iter().any(|c| c.text == "SELECT"));
        assert!(!candidates.iter().any(|c| c.text == "INSERT"));
    }

    #[test]
    fn candidates_empty_for_none_context() {
        let schema = test_schema();
        let candidates = generate_candidates(&CompletionContext::NoSuggestion, "", &schema);
        assert!(candidates.is_empty());
    }

    #[test]
    fn candidates_sorted_by_score() {
        let schema = test_schema();
        let candidates = generate_candidates(&CompletionContext::TableName, "", &schema);
        for window in candidates.windows(2) {
            assert!(window[0].score >= window[1].score);
        }
    }
}
