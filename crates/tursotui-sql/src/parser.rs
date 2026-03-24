//! SQL parsing utilities: statement splitting, FK extraction, source table detection.

/// Foreign key relationship from one column to another table/column.
#[derive(Debug, Clone)]
pub struct ForeignKeyInfo {
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
}

/// Split a SQL string into individual statements, respecting quoted strings and comments.
///
/// Semicolons inside single-quoted strings, double-quoted identifiers,
/// line comments (`--`), and block comments (`/* */`) are **not** treated as
/// statement separators.
pub fn detect_statements(sql: &str) -> Vec<&str> {
    let mut statements = Vec::new();
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut stmt_start = 0;

    while i < len {
        match bytes[i] {
            b'\'' => {
                // Single-quoted string — skip until closing quote.
                // SQLite escapes quotes by doubling: 'it''s'
                i += 1;
                while i < len {
                    if bytes[i] == b'\'' {
                        i += 1;
                        if i < len && bytes[i] == b'\'' {
                            i += 1; // escaped quote, continue
                        } else {
                            break; // end of string
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => {
                // Double-quoted identifier — skip until closing quote.
                // SQLite allows doubled quotes as escape: "col""name"
                i += 1;
                while i < len {
                    if bytes[i] == b'"' {
                        i += 1;
                        if i < len && bytes[i] == b'"' {
                            i += 1; // escaped quote, continue
                        } else {
                            break; // end of identifier
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'-' if i + 1 < len && bytes[i + 1] == b'-' => {
                // Line comment — skip to end of line
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < len {
                    i += 1; // skip newline
                }
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                // Block comment — skip to */
                i += 2;
                while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 < len {
                    i += 2; // skip */
                }
            }
            b';' => {
                let stmt = sql[stmt_start..i].trim();
                if !stmt.is_empty() {
                    statements.push(stmt);
                }
                i += 1;
                stmt_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Last statement (no trailing semicolon)
    let last = sql[stmt_start..].trim();
    if !last.is_empty() {
        statements.push(last);
    }

    statements
}

/// Parse foreign key relationships from a `CREATE TABLE` SQL statement.
///
/// Handles both table-level (`FOREIGN KEY (col) REFERENCES ...`) and
/// inline column-level (`col TYPE REFERENCES ...`) syntax.
pub fn parse_foreign_keys(create_sql: &str) -> Vec<ForeignKeyInfo> {
    let upper = create_sql.to_uppercase();
    let mut fks = Vec::new();

    // Find all "FOREIGN KEY (col) REFERENCES table (col)" patterns
    let mut search_from = 0;
    while let Some(fk_pos) = upper[search_from..].find("FOREIGN KEY") {
        let abs_pos = search_from + fk_pos;
        search_from = abs_pos + 11;

        // Extract from_column: text between first ( and )
        let Some(open) = create_sql[search_from..].find('(') else {
            continue;
        };
        let paren_start = search_from + open + 1;
        let Some(close) = create_sql[paren_start..].find(')') else {
            continue;
        };
        let from_col = create_sql[paren_start..paren_start + close].trim();

        // Find REFERENCES keyword after the closing paren
        let after_paren = paren_start + close + 1;
        let rest_upper = &upper[after_paren..];
        let Some(ref_pos) = rest_upper.find("REFERENCES") else {
            continue;
        };
        let after_ref = after_paren + ref_pos + 10; // len("REFERENCES")

        // Extract target table name (may be quoted)
        let target_start = create_sql[after_ref..].trim_start();
        let offset = create_sql.len() - target_start.len();
        let (to_table, rest) = extract_identifier(target_start);
        if to_table.is_empty() {
            continue;
        }

        // Extract target column: text between ( and )
        let rest_trimmed = rest.trim_start();
        if !rest_trimmed.starts_with('(') {
            continue;
        }
        let inner = &rest_trimmed[1..];
        let Some(end) = inner.find(')') else {
            continue;
        };
        let to_col = inner[..end].trim();

        let table_len = to_table.len();
        fks.push(ForeignKeyInfo {
            from_column: unquote(from_col),
            to_table,
            to_column: unquote(to_col),
        });

        search_from = offset + table_len;
    }

    // Also parse inline column-level FK syntax:
    //   column_name TYPE [NOT NULL] REFERENCES table_name (column_name)
    // Scan for REFERENCES not preceded by FOREIGN KEY.
    let mut search_from = 0;
    while let Some(ref_pos) = upper[search_from..].find("REFERENCES") {
        let abs_pos = search_from + ref_pos;
        search_from = abs_pos + 10;

        // Skip if this REFERENCES is part of a FOREIGN KEY ... REFERENCES
        // (already handled above). Look for "FOREIGN KEY" OR a closing paren
        // between the last clause separator and REFERENCES — both indicate
        // a table-level constraint, not an inline column FK.
        let before = &upper[..abs_pos];
        let last_separator = before.rfind([',', '(']).unwrap_or(0);
        let clause_before = &before[last_separator..];
        if clause_before.contains("FOREIGN KEY") || clause_before.contains(')') {
            continue;
        }

        // Walk backwards from REFERENCES to find the column name.
        // The text before REFERENCES looks like: "col_name TYPE [NOT NULL] [qualifiers] "
        let before_ref = create_sql[..abs_pos].trim_end();
        // Split the clause (from last comma or open paren) into tokens
        let clause_start = before_ref.rfind([',', '(']).map_or(0, |p| p + 1);
        let clause = before_ref[clause_start..].trim();
        // First token in the clause is the column name
        let from_col = clause.split_whitespace().next().unwrap_or("");
        if from_col.is_empty() {
            continue;
        }

        // Extract target table name after REFERENCES
        let after_ref = &create_sql[abs_pos + 10..].trim_start();
        let offset_after = create_sql.len() - after_ref.len();
        let (to_table, rest) = extract_identifier(after_ref);
        if to_table.is_empty() {
            continue;
        }

        // Extract target column from (col)
        let rest_trimmed = rest.trim_start();
        if !rest_trimmed.starts_with('(') {
            continue;
        }
        let inner = &rest_trimmed[1..];
        let Some(end) = inner.find(')') else {
            continue;
        };
        let to_col = inner[..end].trim();

        let tbl_len = to_table.len();
        fks.push(ForeignKeyInfo {
            from_column: unquote(from_col),
            to_table,
            to_column: unquote(to_col),
        });

        search_from = offset_after + tbl_len;
    }

    fks
}

/// Extract an identifier (possibly quoted with `"` or `` ` ``) from the start of `s`.
/// Returns (identifier, `rest_of_string`).
pub fn extract_identifier(s: &str) -> (String, &str) {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        // Double-quoted identifier
        if let Some(end) = rest.find('"') {
            return (s[1..=end].to_string(), &s[2 + end..]);
        }
    } else if let Some(rest) = s.strip_prefix('`')
        && let Some(end) = rest.find('`')
    {
        return (s[1..=end].to_string(), &s[2 + end..]);
    }
    // Unquoted: read until non-identifier char
    let end = s
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(s.len());
    (s[..end].to_string(), &s[end..])
}

/// Remove surrounding quotes from a column name if present.
pub fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('`') && s.ends_with('`')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Remove SQL comments (both line `--` and block `/* */`) from a SQL string.
///
/// **Known limitation:** Does not track string literal context. Comment-like
/// sequences inside single-quoted strings (e.g., `'-- not a comment'`) will
/// be incorrectly stripped. This is acceptable for its current use case
/// (`detect_source_table` editability detection on user-entered SQL) but
/// should not be used for SQL transformation before execution.
pub fn strip_comments(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Block comment
        if i + 1 < len && chars[i] == '/' && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            // skip the closing */
            if i + 1 < len {
                i += 2;
            }
        // Line comment
        } else if i + 1 < len && chars[i] == '-' && chars[i + 1] == '-' {
            i += 2;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Detect whether a SQL query targets a single table and return that table name.
///
/// Returns `Some(table_name)` if the query is a simple single-table SELECT,
/// or `None` if the query is too complex to edit safely.
pub fn detect_source_table(sql: &str) -> Option<String> {
    let stripped = strip_comments(sql);
    let trimmed = stripped.trim();

    if trimmed.is_empty() {
        return None;
    }

    let upper = trimmed.to_uppercase();

    // Must start with SELECT (case-insensitive)
    if !upper.starts_with("SELECT") {
        return None;
    }

    // Reject queries with complexity keywords (simple string containment)
    let reject_keywords = ["JOIN", "UNION", "INTERSECT", "EXCEPT", "GROUP BY", "WITH"];
    for kw in &reject_keywords {
        if upper.contains(kw) {
            return None;
        }
    }

    // Find FROM keyword position
    // We search for FROM as a word boundary approximately — find " FROM " or similar
    let from_pos = find_from_keyword(trimmed)?;

    let after_from = trimmed[from_pos..].trim();

    // Reject subquery in FROM: FROM (
    if after_from.starts_with('(') {
        return None;
    }

    // Extract the table name (possibly quoted)
    Some(extract_table_name(after_from))
}

/// Find the position of the table name (the text after "FROM ") in `sql`.
/// Returns the byte offset into `sql` of the text immediately after FROM and whitespace.
pub fn find_from_keyword(sql: &str) -> Option<usize> {
    let upper = sql.to_uppercase();
    let bytes = upper.as_bytes();
    let len = bytes.len();
    let from_bytes = b"FROM";

    let mut i = 0;
    while i + 4 <= len {
        if &bytes[i..i + 4] == from_bytes {
            // Check that FROM is preceded by whitespace or start
            let preceded_ok = i == 0 || bytes[i - 1].is_ascii_whitespace();
            // Check that FROM is followed by whitespace or end
            let followed_ok = i + 4 == len || bytes[i + 4].is_ascii_whitespace();

            if preceded_ok && followed_ok {
                // Skip "FROM" and leading whitespace
                let mut pos = i + 4;
                while pos < len && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                return Some(pos);
            }
        }
        i += 1;
    }
    None
}

/// Extract a single table name from the beginning of `text`.
/// Handles double-quoted and backtick-quoted names.
/// Stops at whitespace, `;`, or end of string.
pub fn extract_table_name(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    if first == '"' || first == '`' {
        let close = first;
        let mut name = String::new();
        for c in chars {
            if c == close {
                break;
            }
            name.push(c);
        }
        name
    } else {
        let mut name = String::new();
        name.push(first);
        for c in chars {
            if c.is_ascii_whitespace() || c == ';' {
                break;
            }
            name.push(c);
        }
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── detect_statements tests ──────────────────────────────────────

    #[test]
    fn detect_statements_single() {
        let stmts = detect_statements("SELECT 1");
        assert_eq!(stmts, vec!["SELECT 1"]);
    }

    #[test]
    fn detect_statements_multiple() {
        let stmts = detect_statements("INSERT INTO t VALUES (1); SELECT 1");
        assert_eq!(stmts, vec!["INSERT INTO t VALUES (1)", "SELECT 1"]);
    }

    #[test]
    fn detect_statements_trailing_semicolons() {
        let stmts = detect_statements("SELECT 1;;;");
        assert_eq!(stmts, vec!["SELECT 1"]);
    }

    #[test]
    fn detect_statements_semicolon_in_string() {
        let stmts = detect_statements("SELECT 'a;b'");
        assert_eq!(stmts, vec!["SELECT 'a;b'"]);
    }

    #[test]
    fn detect_statements_semicolon_in_double_quoted() {
        let stmts = detect_statements(r#"SELECT "col;name" FROM t"#);
        assert_eq!(stmts, vec![r#"SELECT "col;name" FROM t"#]);
    }

    #[test]
    fn detect_statements_semicolon_in_line_comment() {
        let stmts = detect_statements("SELECT 1 -- ; comment\n; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 1 -- ; comment", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_semicolon_in_block_comment() {
        let stmts = detect_statements("SELECT /* ; */ 1; SELECT 2");
        assert_eq!(stmts, vec!["SELECT /* ; */ 1", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_escaped_single_quote() {
        let stmts = detect_statements("SELECT 'it''s'; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 'it''s'", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_empty_input() {
        let stmts = detect_statements("");
        assert!(stmts.is_empty());
    }

    #[test]
    fn detect_statements_whitespace_only() {
        let stmts = detect_statements("  \n  ");
        assert!(stmts.is_empty());
    }

    #[test]
    fn detect_statements_doubled_double_quotes() {
        let stmts = detect_statements(r#"SELECT "col""name" FROM t; SELECT 2"#);
        assert_eq!(stmts, vec![r#"SELECT "col""name" FROM t"#, "SELECT 2"]);
    }

    #[test]
    fn detect_statements_consecutive_semicolons_mid_input() {
        let stmts = detect_statements("SELECT 1;; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn detect_statements_unclosed_string() {
        // Graceful degradation — treats rest as one statement
        let stmts = detect_statements("SELECT 'unclosed; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 'unclosed; SELECT 2"]);
    }

    #[test]
    fn detect_statements_unclosed_block_comment() {
        let stmts = detect_statements("SELECT /* unclosed; SELECT 2");
        assert_eq!(stmts, vec!["SELECT /* unclosed; SELECT 2"]);
    }

    // ── strip_comments tests ──────────────────────────────────────────

    #[test]
    fn strip_comments_known_limitation_string_literals() {
        // Known limitation: comment-like sequences inside string literals ARE stripped.
        // The `--` is treated as a line comment, consuming everything to the newline.
        // This is acceptable because strip_comments is only used for editability
        // detection (detect_source_table), not for SQL execution.
        let result = strip_comments("SELECT '-- not a comment' FROM t");
        // Documents actual behavior: the line-comment eats the rest of the line
        assert_eq!(result, "SELECT '");
    }

    // ── parse_foreign_keys tests ──────────────────────────────────────

    #[test]
    fn parse_fk_single_constraint() {
        let sql = r"CREATE TABLE employees (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            department_id INTEGER NOT NULL,
            FOREIGN KEY (department_id) REFERENCES departments (id)
        )";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "department_id");
        assert_eq!(fks[0].to_table, "departments");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_multiple_constraints() {
        let sql = r"CREATE TABLE project_assignments (
            id INTEGER PRIMARY KEY,
            employee_id INTEGER NOT NULL,
            project_id INTEGER NOT NULL,
            FOREIGN KEY (employee_id) REFERENCES employees (id),
            FOREIGN KEY (project_id) REFERENCES projects (id)
        )";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].from_column, "employee_id");
        assert_eq!(fks[0].to_table, "employees");
        assert_eq!(fks[0].to_column, "id");
        assert_eq!(fks[1].from_column, "project_id");
        assert_eq!(fks[1].to_table, "projects");
        assert_eq!(fks[1].to_column, "id");
    }

    #[test]
    fn parse_fk_no_foreign_keys() {
        let sql = "CREATE TABLE simple (id INTEGER PRIMARY KEY, name TEXT)";
        let fks = parse_foreign_keys(sql);
        assert!(fks.is_empty());
    }

    #[test]
    fn parse_fk_quoted_identifiers() {
        let sql = r#"CREATE TABLE "my table" (
            id INTEGER PRIMARY KEY,
            "ref_id" INTEGER,
            FOREIGN KEY ("ref_id") REFERENCES "other table" ("pk_col")
        )"#;
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "ref_id");
        assert_eq!(fks[0].to_table, "other table");
        assert_eq!(fks[0].to_column, "pk_col");
    }

    #[test]
    fn parse_fk_inline_column_constraint() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, ref_id INTEGER REFERENCES other(id))";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "ref_id");
        assert_eq!(fks[0].to_table, "other");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_with_on_delete_cascade() {
        let sql = r"CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
        )";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "user_id");
        assert_eq!(fks[0].to_table, "users");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_case_insensitive() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, ref_id INTEGER, foreign key (ref_id) references other (id))";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "ref_id");
        assert_eq!(fks[0].to_table, "other");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_inline_with_not_null() {
        let sql = r"CREATE TABLE albums (
            id INTEGER PRIMARY KEY,
            artist_id INTEGER NOT NULL REFERENCES artists (id),
            title TEXT NOT NULL
        )";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "artist_id");
        assert_eq!(fks[0].to_table, "artists");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_inline_multiple() {
        // Multiple inline FKs in the same table (like demo.db tracks table)
        let sql = r"CREATE TABLE tracks (
            id INTEGER PRIMARY KEY,
            album_id INTEGER NOT NULL REFERENCES albums (id),
            featured_artist_id INTEGER REFERENCES artists (id)
        )";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].from_column, "album_id");
        assert_eq!(fks[0].to_table, "albums");
        assert_eq!(fks[0].to_column, "id");
        assert_eq!(fks[1].from_column, "featured_artist_id");
        assert_eq!(fks[1].to_table, "artists");
        assert_eq!(fks[1].to_column, "id");
    }

    #[test]
    fn parse_fk_inline_quoted_table() {
        let sql =
            r#"CREATE TABLE t (id INTEGER PRIMARY KEY, ref_id INTEGER REFERENCES "My Table" (id))"#;
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "ref_id");
        assert_eq!(fks[0].to_table, "My Table");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_inline_case_insensitive() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, ref_id INTEGER references other(id))";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "ref_id");
        assert_eq!(fks[0].to_table, "other");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_mixed_inline_and_table_level() {
        // Both inline and table-level FKs in the same CREATE TABLE — no duplicates
        let sql = r"CREATE TABLE t (
            id INTEGER PRIMARY KEY,
            a_id INTEGER REFERENCES a (id),
            b_id INTEGER NOT NULL,
            FOREIGN KEY (b_id) REFERENCES b (id)
        )";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 2);
        // Table-level FK comes first (parsed in first pass)
        assert_eq!(fks[0].from_column, "b_id");
        assert_eq!(fks[0].to_table, "b");
        // Inline FK comes second (parsed in second pass)
        assert_eq!(fks[1].from_column, "a_id");
        assert_eq!(fks[1].to_table, "a");
    }

    #[test]
    fn parse_fk_inline_composite_pk_table() {
        // Like demo.db playlist_tracks — composite PK with inline FKs
        let sql = r"CREATE TABLE playlist_tracks (
            playlist_id INTEGER NOT NULL REFERENCES playlists (id),
            track_id INTEGER NOT NULL REFERENCES tracks (id),
            position INTEGER NOT NULL,
            PRIMARY KEY (playlist_id, track_id)
        )";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].from_column, "playlist_id");
        assert_eq!(fks[0].to_table, "playlists");
        assert_eq!(fks[1].from_column, "track_id");
        assert_eq!(fks[1].to_table, "tracks");
    }

    #[test]
    fn parse_fk_real_testdata_employees() {
        // Exact SQL from testdb/testdata.db
        let sql = "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL UNIQUE, department_id INTEGER NOT NULL, salary REAL NOT NULL, hire_date TEXT NOT NULL, title TEXT NOT NULL, FOREIGN KEY (department_id) REFERENCES departments (id))";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].from_column, "department_id");
        assert_eq!(fks[0].to_table, "departments");
        assert_eq!(fks[0].to_column, "id");
    }

    #[test]
    fn parse_fk_real_testdata_project_assignments() {
        // Exact SQL from testdb/testdata.db
        let sql = "CREATE TABLE project_assignments (id INTEGER PRIMARY KEY, employee_id INTEGER NOT NULL, project_id INTEGER NOT NULL, role TEXT NOT NULL, hours_allocated REAL NOT NULL DEFAULT 40.0, FOREIGN KEY (employee_id) REFERENCES employees (id), FOREIGN KEY (project_id) REFERENCES projects (id))";
        let fks = parse_foreign_keys(sql);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].from_column, "employee_id");
        assert_eq!(fks[0].to_table, "employees");
        assert_eq!(fks[1].from_column, "project_id");
        assert_eq!(fks[1].to_table, "projects");
    }

    // ── detect_source_table tests ────────────────────────────────────

    #[test]
    fn test_simple_select_is_editable() {
        assert_eq!(
            detect_source_table("SELECT * FROM users"),
            Some("users".to_string())
        );
    }

    #[test]
    fn test_select_with_where_is_editable() {
        assert_eq!(
            detect_source_table("SELECT * FROM users WHERE id = 1"),
            Some("users".to_string())
        );
    }

    #[test]
    fn test_select_with_limit() {
        assert_eq!(
            detect_source_table("SELECT * FROM \"users\" LIMIT 100;"),
            Some("users".to_string())
        );
    }

    #[test]
    fn test_join_is_not_editable() {
        assert_eq!(detect_source_table("SELECT * FROM users JOIN orders"), None);
    }

    #[test]
    fn test_union_is_not_editable() {
        assert_eq!(
            detect_source_table("SELECT * FROM users UNION SELECT * FROM admins"),
            None
        );
    }

    #[test]
    fn test_group_by_is_not_editable() {
        assert_eq!(
            detect_source_table("SELECT count(*) FROM users GROUP BY role"),
            None
        );
    }

    #[test]
    fn test_cte_is_not_editable() {
        assert_eq!(
            detect_source_table("WITH cte AS (SELECT * FROM users) SELECT * FROM cte"),
            None
        );
    }

    #[test]
    fn test_subquery_in_from_is_not_editable() {
        assert_eq!(
            detect_source_table("SELECT * FROM (SELECT * FROM users)"),
            None
        );
    }

    #[test]
    fn test_non_select_is_not_editable() {
        assert_eq!(detect_source_table("INSERT INTO users VALUES (1)"), None);
    }

    #[test]
    fn test_select_with_comments_is_editable() {
        assert_eq!(
            detect_source_table("-- comment\nSELECT * FROM users"),
            Some("users".to_string())
        );
    }

    #[test]
    fn test_block_comment() {
        assert_eq!(
            detect_source_table("/* comment */ SELECT * FROM users"),
            Some("users".to_string())
        );
    }

    #[test]
    fn test_quoted_table_name() {
        assert_eq!(
            detect_source_table("SELECT * FROM \"my table\""),
            Some("my table".to_string())
        );
    }

    #[test]
    fn test_backtick_quoted_table_name() {
        assert_eq!(
            detect_source_table("SELECT * FROM `my table`"),
            Some("my table".to_string())
        );
    }

    #[test]
    fn test_case_insensitive_keywords() {
        assert_eq!(
            detect_source_table("select * from Users"),
            Some("Users".to_string())
        );
    }

    /// Per spec: keyword rejection is simple string containment (space-separated "GROUP BY").
    /// The identifier `my_group_by_stats` uses underscores, so "GROUP BY" (with a space)
    /// does NOT appear in the query — this table is correctly treated as editable.
    /// This test documents the boundary: underscore-separated names are not false-negatives.
    #[test]
    fn test_keyword_in_identifier_false_negative() {
        // "GROUP BY" (with space) is NOT in "my_group_by_stats" (underscores) → Some
        assert_eq!(
            detect_source_table("SELECT * FROM my_group_by_stats"),
            Some("my_group_by_stats".to_string())
        );
    }

    #[test]
    fn test_intersect_rejected() {
        assert_eq!(
            detect_source_table("SELECT * FROM a INTERSECT SELECT * FROM b"),
            None
        );
    }

    #[test]
    fn test_except_rejected() {
        assert_eq!(
            detect_source_table("SELECT * FROM a EXCEPT SELECT * FROM b"),
            None
        );
    }

    #[test]
    fn test_with_clause_rejected() {
        assert_eq!(
            detect_source_table("WITH t AS (SELECT 1) SELECT * FROM t"),
            None
        );
    }

    #[test]
    fn test_empty_query() {
        assert_eq!(detect_source_table(""), None);
    }

    #[test]
    fn test_whitespace_only() {
        assert_eq!(detect_source_table("   "), None);
    }
}
