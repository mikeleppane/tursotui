//! Result set export formatting.
//!
//! Pure functions: take column names + row data and return a formatted `String`.
//! No I/O, no UI -- just formatting logic.

#![allow(
    dead_code,
    reason = "module wired incrementally -- public API used when export UI lands"
)]

use tursotui_sql::quoting::{quote_identifier, quote_literal};

use tursotui_db::ColumnDef;

/// Format rows as CSV (RFC 4180 quoting rules, LF line endings).
///
/// - Header row from column names.
/// - Values quoted when they contain commas, double-quotes, or newlines.
/// - Quotes escaped by doubling (`"` -> `""`).
/// - `None` (NULL) emitted as an empty unquoted field.
pub(crate) fn format_csv(columns: &[ColumnDef], rows: &[Vec<Option<String>>]) -> String {
    let mut out = String::new();

    // Header row.
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        csv_field(&mut out, &col.name);
    }
    out.push('\n');

    // Data rows.
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if let Some(val) = cell {
                csv_field(&mut out, val);
            }
            // None -> empty (no quotes)
        }
        out.push('\n');
    }

    out
}

/// Write a single CSV field, quoting only when necessary.
fn csv_field(out: &mut String, value: &str) {
    let needs_quoting =
        value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r');
    if needs_quoting {
        out.push('"');
        for ch in value.chars() {
            if ch == '"' {
                out.push_str("\"\"");
            } else {
                out.push(ch);
            }
        }
        out.push('"');
    } else {
        out.push_str(value);
    }
}

/// Format rows as a JSON array of objects.
///
/// - Column names become object keys.
/// - `None` (NULL) becomes JSON `null`.
/// - Values that look like numbers are emitted as JSON numbers.
/// - All other values are emitted as JSON strings with proper escaping.
pub(crate) fn format_json(columns: &[ColumnDef], rows: &[Vec<Option<String>>]) -> String {
    let mut out = String::from("[\n");

    for (row_idx, row) in rows.iter().enumerate() {
        out.push_str("  {");
        for (col_idx, cell) in row.iter().enumerate() {
            if col_idx > 0 {
                out.push_str(", ");
            }
            out.push('"');
            json_escape_string(&mut out, &columns[col_idx].name);
            out.push_str("\": ");
            match cell {
                None => out.push_str("null"),
                Some(val) => {
                    if looks_like_number(val) {
                        out.push_str(val);
                    } else {
                        out.push('"');
                        json_escape_string(&mut out, val);
                        out.push('"');
                    }
                }
            }
        }
        out.push('}');
        if row_idx + 1 < rows.len() {
            out.push(',');
        }
        out.push('\n');
    }

    out.push(']');
    out
}

/// Return `true` if `s` parses as a finite JSON number.
///
/// Excludes NaN, +inf, -inf since they are not valid JSON values.
fn looks_like_number(s: &str) -> bool {
    s.parse::<f64>().is_ok_and(f64::is_finite)
}

/// Append `s` to `out` with JSON string escaping (does NOT emit surrounding quotes).
fn json_escape_string(out: &mut String, s: &str) {
    use std::fmt::Write as _;

    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                // \u00XX for other control characters.
                for unit in c.encode_utf16(&mut [0; 2]) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
}

/// Format rows as SQL `INSERT` statements.
///
/// - One `INSERT INTO ... VALUES (...)` per row.
/// - Table and column names quoted with `"` (escaped by doubling).
/// - `None` (NULL) becomes SQL `NULL`.
/// - Values that look like numbers are emitted unquoted; others are `'`-quoted
///   with single-quotes escaped by doubling.
pub(crate) fn format_sql_insert(
    columns: &[ColumnDef],
    rows: &[Vec<Option<String>>],
    table_name: &str,
) -> String {
    let mut out = String::new();

    // Build the column list once: INSERT INTO "table" ("col1", "col2")
    let mut prefix = String::from("INSERT INTO ");
    prefix.push_str(&quote_identifier(table_name));
    prefix.push_str(" (");
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            prefix.push_str(", ");
        }
        prefix.push_str(&quote_identifier(&col.name));
    }
    prefix.push_str(") VALUES (");

    for row in rows {
        out.push_str(&prefix);
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            match cell {
                None => out.push_str("NULL"),
                Some(val) => {
                    if looks_like_number(val) {
                        out.push_str(val);
                    } else {
                        out.push_str(&quote_literal(val));
                    }
                }
            }
        }
        out.push_str(");\n");
    }

    out
}

/// Format rows as TSV (tab-separated values).
///
/// - Header row from column names.
/// - `None` (NULL) rendered as the literal string `NULL`.
pub(crate) fn format_tsv(columns: &[ColumnDef], rows: &[Vec<Option<String>>]) -> String {
    let mut out = String::new();

    // Header row.
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            out.push('\t');
        }
        out.push_str(&col.name);
    }
    out.push('\n');

    // Data rows.
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push('\t');
            }
            match cell {
                Some(val) => out.push_str(val),
                None => out.push_str("NULL"),
            }
        }
        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_columns() -> Vec<ColumnDef> {
        vec![
            ColumnDef {
                name: "name".into(),
                type_name: "TEXT".into(),
            },
            ColumnDef {
                name: "age".into(),
                type_name: "INTEGER".into(),
            },
        ]
    }

    fn test_rows() -> Vec<Vec<Option<String>>> {
        vec![
            vec![Some("Alice".into()), Some("30".into())],
            vec![Some("Bob".into()), None],
        ]
    }

    // ---- CSV ----

    #[test]
    fn csv_basic() {
        let csv = format_csv(&test_columns(), &test_rows());
        assert_eq!(csv, "name,age\nAlice,30\nBob,\n");
    }

    #[test]
    fn csv_quoting() {
        let rows = vec![vec![Some("O'Brien, Jr.".into()), Some("42".into())]];
        let csv = format_csv(&test_columns(), &rows);
        // Comma in value triggers quoting.
        assert_eq!(csv, "name,age\n\"O'Brien, Jr.\",42\n");
    }

    #[test]
    fn csv_quote_escaping() {
        let rows = vec![vec![Some("say \"hello\"".into()), Some("1".into())]];
        let csv = format_csv(&test_columns(), &rows);
        assert_eq!(csv, "name,age\n\"say \"\"hello\"\"\",1\n");
    }

    #[test]
    fn csv_newline_in_value() {
        let rows = vec![vec![Some("line1\nline2".into()), Some("5".into())]];
        let csv = format_csv(&test_columns(), &rows);
        assert_eq!(csv, "name,age\n\"line1\nline2\",5\n");
    }

    #[test]
    fn csv_empty_rows() {
        let csv = format_csv(&test_columns(), &[]);
        assert_eq!(csv, "name,age\n");
    }

    // ---- JSON ----

    #[test]
    fn json_basic() {
        let json = format_json(&test_columns(), &test_rows());
        let expected = "[\n\
                          \x20\x20{\"name\": \"Alice\", \"age\": 30},\n\
                          \x20\x20{\"name\": \"Bob\", \"age\": null}\n\
                          ]";
        assert_eq!(json, expected);
    }

    #[test]
    fn json_escaping() {
        let rows = vec![vec![Some("say \"hi\"\nnewline".into()), Some("1".into())]];
        let json = format_json(&test_columns(), &rows);
        assert!(json.contains(r#""say \"hi\"\nnewline""#));
    }

    #[test]
    fn json_nan_inf_as_strings() {
        let rows = vec![
            vec![Some("NaN".into()), Some("NaN".into())],
            vec![Some("inf".into()), Some("inf".into())],
            vec![Some("-inf".into()), Some("-inf".into())],
        ];
        let json = format_json(&test_columns(), &rows);
        // NaN, inf, -inf should be quoted as strings, not bare values.
        assert!(json.contains("\"NaN\""));
        assert!(json.contains("\"inf\""));
        assert!(json.contains("\"-inf\""));
    }

    #[test]
    fn json_empty_rows() {
        let json = format_json(&test_columns(), &[]);
        assert_eq!(json, "[\n]");
    }

    // ---- SQL INSERT ----

    #[test]
    fn sql_insert_basic() {
        let sql = format_sql_insert(&test_columns(), &test_rows(), "users");
        let expected = "INSERT INTO \"users\" (\"name\", \"age\") VALUES ('Alice', 30);\n\
                        INSERT INTO \"users\" (\"name\", \"age\") VALUES ('Bob', NULL);\n";
        assert_eq!(sql, expected);
    }

    #[test]
    fn sql_insert_escaping() {
        let rows = vec![vec![Some("O'Reilly".into()), Some("55".into())]];
        let sql = format_sql_insert(&test_columns(), &rows, "my\"table");
        assert!(sql.contains("'O''Reilly'"));
        assert!(sql.contains("\"my\"\"table\""));
    }

    #[test]
    fn sql_insert_empty_rows() {
        let sql = format_sql_insert(&test_columns(), &[], "t");
        assert_eq!(sql, "");
    }

    // ---- TSV ----

    #[test]
    fn tsv_basic() {
        let tsv = format_tsv(&test_columns(), &test_rows());
        assert_eq!(tsv, "name\tage\nAlice\t30\nBob\tNULL\n");
    }

    #[test]
    fn tsv_empty_rows() {
        let tsv = format_tsv(&test_columns(), &[]);
        assert_eq!(tsv, "name\tage\n");
    }
}
