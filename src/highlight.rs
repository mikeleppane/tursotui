//! Lexical SQL syntax highlighter for the query editor.
//!
//! Tokenizes SQL input into classified spans (keywords, strings, numbers, etc.)
//! and maps them to theme-aware ratatui styles. Not a parser — just coloring.

#![allow(
    dead_code,
    reason = "fields and functions used incrementally as components are added"
)]

use ratatui::prelude::*;

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Operator,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) text: String,
}

/// SQL keywords (uppercase for matching; input is uppercased before comparison).
const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "DROP",
    "ALTER",
    "INDEX",
    "VIEW",
    "TRIGGER",
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
    "NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "DISTINCT",
    "ALL",
    "ANY",
    "ORDER",
    "BY",
    "GROUP",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "WITH",
    "RECURSIVE",
    "ASC",
    "DESC",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "AUTOINCREMENT",
    "CONSTRAINT",
    "IF",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRANSACTION",
    "PRAGMA",
    "EXPLAIN",
    "PLAN",
    "QUERY",
    "INTEGER",
    "TEXT",
    "REAL",
    "BLOB",
    "NUMERIC",
    "VARCHAR",
    "BOOLEAN",
    "WITHOUT",
    "ROWID",
    // REPLACE is intentionally in both SQL_KEYWORDS and SQL_FUNCTIONS.
    // When followed by '(' it's the REPLACE() string function → Function.
    // Bare REPLACE (as in INSERT OR REPLACE) → Keyword.
    "REPLACE",
    "ABORT",
    "FAIL",
    "IGNORE",
    "CONFLICT",
    "CASCADE",
    "RESTRICT",
    "IMMEDIATE",
    "DEFERRED",
    "EXCLUSIVE",
    "ATTACH",
    "DETACH",
    "REINDEX",
    "ANALYZE",
    "VACUUM",
    "RENAME",
    "ADD",
    "COLUMN",
    "TEMP",
    "TEMPORARY",
    "VIRTUAL",
    "USING",
    "NATURAL",
    "GLOB",
    "REGEXP",
    "MATCH",
    "ESCAPE",
    "COLLATE",
    "NOCASE",
    "CAST",
    "ISNULL",
    "NOTNULL",
];

/// Word-form operators (spec §5.2: Default fg, bold — same style as symbol operators).
const SQL_WORD_OPERATORS: &[&str] = &["AND", "OR", "NOT"];

/// Common SQL functions.
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
    "UNICODE",
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
    "ZEROBLOB",
    "QUOTE",
    "RANDOM",
    "RANDOMBLOB",
    "ROUND",
    "PRINTF",
    "FORMAT",
    "DATE",
    "TIME",
    "DATETIME",
    "JULIANDAY",
    "STRFTIME",
    "UNIXEPOCH",
    "CHANGES",
    "LAST_INSERT_ROWID",
    "TOTAL_CHANGES",
    "LIKELIHOOD",
    "LIKELY",
    "UNLIKELY",
    "JSON",
    "JSON_ARRAY",
    "JSON_OBJECT",
    "JSON_EXTRACT",
    "JSON_TYPE",
    // Turso/libsql-specific functions
    "UUID4",
    "UUID7",
    "UUID7_TIMESTAMP_MS",
    "UUID_STR",
    "UUID_BLOB",
    "VECTOR32",
    "VECTOR64",
    "VECTOR_DISTANCE_COS",
    "VECTOR_DISTANCE_L2",
    "VECTOR_EXTRACT",
    "VECTOR_CONCAT",
    "VECTOR_SLICE",
    "VECTOR_DIMENSION",
    "TIME_NOW",
    "TIME_DATE",
    "TIME_UNIX",
    "TIME_ADD",
    "TIME_SUB",
    "TIME_FMT_ISO",
    "TIME_FMT_DATETIME",
    "TIME_FMT_UNIXEPOCH",
    "REGEXP_SUBSTR",
    "REGEXP_CAPTURE",
    "REGEXP_REPLACE",
    "FTS_MATCH",
    "FTS_SCORE",
    "FTS_HIGHLIGHT",
    "MEDIAN",
    "PERCENTILE",
    "PERCENTILE_CONT",
    "PERCENTILE_DISC",
    "GENERATE_SERIES",
];

/// Tokenize a single line of SQL into classified tokens.
#[allow(clippy::too_many_lines)]
pub(crate) fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // Single-line comment: --
        // IMPORTANT: This must come before the operator check, which also matches '-'.
        if ch == '-' && i + 1 < len && chars[i + 1] == '-' {
            let text: String = chars[i..].iter().collect();
            tokens.push(Token {
                kind: TokenKind::Comment,
                text,
            });
            break; // rest of line is comment
        }

        // Block comment start: /*
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            if i + 1 < len {
                i += 2; // skip */
            } else {
                i = len; // unclosed block comment extends to end of line
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokenKind::Comment,
                text,
            });
            continue;
        }

        // Quoted identifiers: "name" or `name` — tokenized as Default to avoid
        // false keyword highlighting (e.g., SELECT "FROM" FROM t).
        if ch == '"' || ch == '`' {
            let quote = ch;
            let start = i;
            i += 1;
            while i < len && chars[i] != quote {
                i += 1;
            }
            if i < len {
                i += 1; // closing quote
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokenKind::Default,
                text,
            });
            continue;
        }

        // String literal: 'text'
        if ch == '\'' {
            let start = i;
            i += 1;
            while i < len {
                if chars[i] == '\'' {
                    if i + 1 < len && chars[i + 1] == '\'' {
                        i += 2; // escaped quote ''
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokenKind::String,
                text,
            });
            continue;
        }

        // Number: digits optionally with decimal point
        if ch.is_ascii_digit() || (ch == '.' && i + 1 < len && chars[i + 1].is_ascii_digit()) {
            let start = i;
            while i < len && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokenKind::Number,
                text,
            });
            continue;
        }

        // Word (identifier/keyword/function)
        if ch.is_alphanumeric() || ch == '_' {
            let start = i;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let upper = text.to_uppercase();

            // Check if followed by '(' → function
            let is_fn_call = i < len && chars[i] == '(';

            let kind = if is_fn_call && SQL_FUNCTIONS.contains(&upper.as_str()) {
                TokenKind::Function
            } else if SQL_WORD_OPERATORS.contains(&upper.as_str()) {
                TokenKind::Operator
            } else if SQL_KEYWORDS.contains(&upper.as_str()) {
                TokenKind::Keyword
            } else {
                TokenKind::Default
            };
            tokens.push(Token { kind, text });
            continue;
        }

        // Operators: = <> != >= <= < > || + - * / %
        if "=<>!|+-*/%".contains(ch) {
            let start = i;
            i += 1;
            // Consume second char for two-char operators
            if i < len && "<>=|".contains(chars[i]) {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokenKind::Operator,
                text,
            });
            continue;
        }

        // Everything else (whitespace, punctuation like parens, commas, semicolons)
        let start = i;
        i += 1;
        let text: String = chars[start..i].iter().collect();
        tokens.push(Token {
            kind: TokenKind::Default,
            text,
        });
    }

    tokens
}

/// Convert a line of SQL into a styled ratatui `Line` using the given theme.
pub(crate) fn highlight_line(input: &str, theme: &Theme) -> Line<'static> {
    let tokens = tokenize(input);
    let spans: Vec<Span<'static>> = tokens
        .into_iter()
        .map(|tok| {
            let style = match tok.kind {
                TokenKind::Keyword => theme.sql_keyword,
                TokenKind::String => theme.sql_string,
                TokenKind::Number => theme.sql_number,
                TokenKind::Comment => theme.sql_comment,
                TokenKind::Function => theme.sql_function,
                TokenKind::Operator => theme.sql_operator,
                TokenKind::Default => Style::default(),
            };
            Span::styled(tok.text, style)
        })
        .collect();
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_keyword() {
        let tokens = tokenize("SELECT");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Keyword);
        assert_eq!(tokens[0].text, "SELECT");
    }

    #[test]
    fn test_case_insensitive_keywords() {
        let tokens = tokenize("select from WHERE");
        let keywords: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Keyword)
            .collect();
        assert_eq!(keywords.len(), 3);
    }

    #[test]
    fn test_string_literal() {
        let tokens = tokenize("'hello world'");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::String);
        assert_eq!(tokens[0].text, "'hello world'");
    }

    #[test]
    fn test_escaped_string() {
        let tokens = tokenize("'it''s'");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::String);
    }

    #[test]
    fn test_number() {
        let tokens = tokenize("42 3.14");
        let nums: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Number)
            .collect();
        assert_eq!(nums.len(), 2);
        assert_eq!(nums[0].text, "42");
        assert_eq!(nums[1].text, "3.14");
    }

    #[test]
    fn test_single_line_comment() {
        let tokens = tokenize("SELECT -- this is a comment");
        assert_eq!(tokens[0].kind, TokenKind::Keyword); // SELECT
        let comment = tokens
            .iter()
            .find(|t| t.kind == TokenKind::Comment)
            .unwrap();
        assert!(comment.text.starts_with("--"));
    }

    #[test]
    fn test_block_comment() {
        let tokens = tokenize("SELECT /* comment */ FROM");
        assert_eq!(
            tokens
                .iter()
                .filter(|t| t.kind == TokenKind::Comment)
                .count(),
            1
        );
        assert_eq!(
            tokens
                .iter()
                .filter(|t| t.kind == TokenKind::Keyword)
                .count(),
            2
        );
    }

    #[test]
    fn test_function() {
        let tokens = tokenize("COUNT(*)");
        assert_eq!(tokens[0].kind, TokenKind::Function);
        assert_eq!(tokens[0].text, "COUNT");
    }

    #[test]
    fn test_function_not_keyword_without_parens() {
        // COUNT without parens is not in SQL_KEYWORDS → Default
        let tokens = tokenize("COUNT");
        assert_eq!(tokens[0].kind, TokenKind::Default);
    }

    #[test]
    fn test_operator() {
        let tokens = tokenize("a >= b");
        let ops: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Operator)
            .collect();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].text, ">=");
    }

    #[test]
    fn test_mixed_query() {
        let tokens = tokenize("SELECT name FROM users WHERE id = 42;");
        // Should have keywords: SELECT, FROM, WHERE
        let kw_count = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Keyword)
            .count();
        assert_eq!(kw_count, 3);
    }

    #[test]
    fn test_empty_input() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_unclosed_string_literal() {
        let tokens = tokenize("SELECT 'unterminated");
        let strings: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::String)
            .collect();
        assert_eq!(strings.len(), 1);
        assert_eq!(strings[0].text, "'unterminated");
    }

    #[test]
    fn test_double_quoted_identifier() {
        // "FROM" should be Default, not Keyword
        let tokens = tokenize(r#"SELECT "FROM" FROM t"#);
        let keywords: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Keyword)
            .collect();
        // SELECT and the bare FROM are keywords; "FROM" is Default
        assert_eq!(keywords.len(), 2);
        let quoted = tokens.iter().find(|t| t.text == r#""FROM""#).unwrap();
        assert_eq!(quoted.kind, TokenKind::Default);
    }

    #[test]
    fn test_backtick_quoted_identifier() {
        let tokens = tokenize("SELECT `table name` FROM t");
        let quoted = tokens.iter().find(|t| t.text == "`table name`").unwrap();
        assert_eq!(quoted.kind, TokenKind::Default);
    }

    #[test]
    fn test_and_or_not_are_operators() {
        let tokens = tokenize("a AND b OR NOT c");
        let ops: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Operator)
            .collect();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].text, "AND");
        assert_eq!(ops[1].text, "OR");
        assert_eq!(ops[2].text, "NOT");
    }

    #[test]
    fn test_not_equal_operator() {
        let tokens = tokenize("a != b");
        let ops: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Operator)
            .collect();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].text, "!=");
    }

    #[test]
    fn test_diamond_operator() {
        let tokens = tokenize("a <> b");
        let ops: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Operator)
            .collect();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].text, "<>");
    }

    #[test]
    fn test_replace_keyword_vs_function() {
        // Bare REPLACE → Keyword
        let tokens = tokenize("REPLACE INTO t VALUES (1)");
        assert_eq!(tokens[0].kind, TokenKind::Keyword);

        // REPLACE(...) → Function
        let tokens = tokenize("REPLACE('abc', 'a', 'x')");
        assert_eq!(tokens[0].kind, TokenKind::Function);
    }

    #[test]
    fn test_empty_block_comment() {
        let tokens = tokenize("SELECT /**/ FROM t");
        assert_eq!(
            tokens
                .iter()
                .filter(|t| t.kind == TokenKind::Comment)
                .count(),
            1
        );
    }

    #[test]
    fn test_whitespace_only() {
        let tokens = tokenize("   ");
        assert!(tokens.iter().all(|t| t.kind == TokenKind::Default));
    }

    #[test]
    fn turso_specific_functions_highlighted() {
        let tokens = tokenize("SELECT uuid4(), vector_distance_cos(a, b), time_now()");
        let fn_tokens: Vec<&str> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Function)
            .map(|t| t.text.as_str())
            .collect();
        assert!(fn_tokens.contains(&"uuid4"), "uuid4 should be Function");
        assert!(
            fn_tokens.contains(&"vector_distance_cos"),
            "vector_distance_cos should be Function"
        );
        assert!(
            fn_tokens.contains(&"time_now"),
            "time_now should be Function"
        );
    }
}
