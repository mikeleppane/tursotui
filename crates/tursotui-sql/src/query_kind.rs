//! SQL statement classification by leading keyword.

/// Detected query type — used for status bar messaging and execution routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Select,
    Explain,
    Insert,
    Update,
    Delete,
    Ddl,
    Pragma,
    Batch {
        statement_count: usize,
        has_trailing_select: bool,
    },
    Other,
}

/// Detect the query kind from the first non-whitespace, non-comment token.
pub fn detect_query_kind(sql: &str) -> QueryKind {
    let sql = skip_leading_whitespace_and_comments(sql);
    let first_word = sql
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("");
    match first_word.to_uppercase().as_str() {
        "SELECT" => QueryKind::Select,
        "EXPLAIN" => QueryKind::Explain,
        "INSERT" => QueryKind::Insert,
        "UPDATE" => QueryKind::Update,
        "DELETE" => QueryKind::Delete,
        "CREATE" | "ALTER" | "DROP" => QueryKind::Ddl,
        "PRAGMA" => QueryKind::Pragma,
        _ => QueryKind::Other,
    }
}

/// Returns true if `sql` begins a transaction-control statement
/// (BEGIN/COMMIT/ROLLBACK/END), after stripping leading whitespace and comments.
pub fn is_transaction_control(sql: &str) -> bool {
    let upper = skip_leading_whitespace_and_comments(sql).to_uppercase();
    upper.starts_with("BEGIN")
        || upper.starts_with("COMMIT")
        || upper.starts_with("ROLLBACK")
        || upper.starts_with("END")
}

/// Skip leading whitespace and SQL comments (line and block) to find the first real token.
pub(crate) fn skip_leading_whitespace_and_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            // Skip to end of line
            s = s.find('\n').map_or("", |i| &s[i + 1..]).trim_start();
        } else if s.starts_with("/*") {
            // Skip to end of block comment
            s = match s[2..].find("*/") {
                Some(i) => &s[2 + i + 2..],
                None => "",
            };
            s = s.trim_start();
        } else {
            break;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_query_kind_select() {
        assert!(matches!(detect_query_kind("SELECT 1"), QueryKind::Select));
    }

    #[test]
    fn detect_query_kind_case_insensitive() {
        assert!(matches!(detect_query_kind("sElEcT 1"), QueryKind::Select));
    }

    #[test]
    fn detect_query_kind_leading_whitespace() {
        assert!(matches!(
            detect_query_kind("  \n  INSERT INTO t VALUES (1)"),
            QueryKind::Insert
        ));
    }

    #[test]
    fn detect_query_kind_leading_line_comment() {
        assert!(matches!(
            detect_query_kind("-- comment\nSELECT 1"),
            QueryKind::Select
        ));
    }

    #[test]
    fn detect_query_kind_leading_block_comment() {
        assert!(matches!(
            detect_query_kind("/* block */  DELETE FROM t"),
            QueryKind::Delete
        ));
    }

    #[test]
    fn detect_query_kind_explain() {
        assert!(matches!(
            detect_query_kind("EXPLAIN SELECT 1"),
            QueryKind::Explain
        ));
    }

    #[test]
    fn detect_query_kind_ddl() {
        assert!(matches!(
            detect_query_kind("CREATE TABLE t (id INT)"),
            QueryKind::Ddl
        ));
        assert!(matches!(
            detect_query_kind("ALTER TABLE t ADD col INT"),
            QueryKind::Ddl
        ));
        assert!(matches!(detect_query_kind("DROP TABLE t"), QueryKind::Ddl));
    }

    #[test]
    fn detect_query_kind_pragma() {
        assert!(matches!(
            detect_query_kind("PRAGMA table_info(t)"),
            QueryKind::Pragma
        ));
    }

    #[test]
    fn detect_query_kind_update() {
        assert!(matches!(
            detect_query_kind("UPDATE t SET x = 1"),
            QueryKind::Update
        ));
    }

    #[test]
    fn detect_query_kind_unknown() {
        assert!(matches!(detect_query_kind("VACUUM"), QueryKind::Other));
        assert!(matches!(detect_query_kind(""), QueryKind::Other));
    }

    #[test]
    fn detect_query_kind_begin_commit_rollback() {
        assert!(matches!(detect_query_kind("BEGIN"), QueryKind::Other));
        assert!(matches!(detect_query_kind("COMMIT"), QueryKind::Other));
        assert!(matches!(detect_query_kind("ROLLBACK"), QueryKind::Other));
        assert!(matches!(detect_query_kind("END"), QueryKind::Other));
    }

    #[test]
    fn detect_query_kind_stacked_comments() {
        assert!(matches!(
            detect_query_kind("-- first\n-- second\nSELECT 1"),
            QueryKind::Select
        ));
        assert!(matches!(
            detect_query_kind("/* a */ /* b */ INSERT INTO t VALUES (1)"),
            QueryKind::Insert
        ));
    }

    #[test]
    fn is_transaction_control_positive() {
        assert!(is_transaction_control("BEGIN"));
        assert!(is_transaction_control("COMMIT"));
        assert!(is_transaction_control("ROLLBACK"));
        assert!(is_transaction_control("END"));
        assert!(is_transaction_control("  BEGIN TRANSACTION"));
        assert!(is_transaction_control("-- comment\nBEGIN"));
    }

    #[test]
    fn is_transaction_control_negative() {
        assert!(!is_transaction_control("SELECT 1"));
        assert!(!is_transaction_control("INSERT INTO t VALUES (1)"));
        assert!(!is_transaction_control(""));
    }
}
