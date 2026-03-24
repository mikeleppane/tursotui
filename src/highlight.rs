//! Lexical SQL syntax highlighter for the query editor.
//!
//! Tokenizes SQL input into classified spans (keywords, strings, numbers, etc.)
//! and maps them to theme-aware ratatui styles. Not a parser — just coloring.

#![allow(
    dead_code,
    reason = "fields and functions used incrementally as components are added"
)]

use ratatui::prelude::*;
use tursotui_sql::keywords::{SQL_FUNCTIONS, SQL_KEYWORDS, SQL_TYPES, SQL_WORD_OPERATORS};

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Operator,
    Type,
    Parameter,
    Field,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) text: String,
}

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

        // Positional parameter: ? or ?1, ?2, etc.
        if ch == '?' {
            let start = i;
            i += 1;
            while i < len && chars[i].is_ascii_digit() {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokenKind::Parameter,
                text,
            });
            continue;
        }

        // Named parameter: :name, $name, @name
        if (ch == ':' || ch == '$' || ch == '@')
            && i + 1 < len
            && (chars[i + 1].is_alphabetic() || chars[i + 1] == '_')
        {
            let start = i;
            i += 1;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokenKind::Parameter,
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

            // Dot-qualified field: word immediately preceded by '.' (e.g. u.email)
            let after_dot = start > 0 && chars[start - 1] == '.';

            let kind = if is_fn_call && SQL_FUNCTIONS.contains(&upper.as_str()) {
                TokenKind::Function
            } else if after_dot {
                TokenKind::Field
            } else if SQL_WORD_OPERATORS.contains(&upper.as_str()) {
                TokenKind::Operator
            } else if SQL_TYPES.contains(&upper.as_str()) {
                TokenKind::Type
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
                TokenKind::Type => theme.sql_type,
                TokenKind::Parameter => theme.sql_parameter,
                TokenKind::Field => theme.sql_field,
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
    fn test_data_types_highlighted() {
        let tokens = tokenize("CREATE TABLE t (id INTEGER, name TEXT, val REAL)");
        let types: Vec<&str> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Type)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(types, vec!["INTEGER", "TEXT", "REAL"]);
    }

    #[test]
    fn test_data_types_case_insensitive() {
        let tokens = tokenize("varchar boolean blob");
        assert!(
            tokens
                .iter()
                .filter(|t| !t.text.trim().is_empty())
                .all(|t| t.kind == TokenKind::Type)
        );
    }

    #[test]
    fn test_date_type_vs_function() {
        // Bare DATE → Type (column type in DDL)
        let tokens = tokenize("col DATE");
        let date_tok = tokens.iter().find(|t| t.text == "DATE").unwrap();
        assert_eq!(date_tok.kind, TokenKind::Type);

        // DATE(...) → Function
        let tokens = tokenize("DATE('now')");
        assert_eq!(tokens[0].kind, TokenKind::Function);
    }

    #[test]
    fn test_positional_parameter() {
        let tokens = tokenize("SELECT * FROM t WHERE id = ?1");
        let params: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Parameter)
            .collect();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].text, "?1");
    }

    #[test]
    fn test_bare_question_mark_parameter() {
        let tokens = tokenize("WHERE id = ?");
        let params: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Parameter)
            .collect();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].text, "?");
    }

    #[test]
    fn test_named_parameter() {
        let tokens = tokenize("WHERE name = :user_name AND age > :min_age");
        let params: Vec<&str> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Parameter)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(params, vec![":user_name", ":min_age"]);
    }

    #[test]
    fn test_dollar_parameter() {
        let tokens = tokenize("WHERE id = $user_id");
        let params: Vec<&str> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Parameter)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(params, vec!["$user_id"]);
    }

    #[test]
    fn test_at_parameter() {
        let tokens = tokenize("WHERE id = @id AND name = @name");
        let params: Vec<&str> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Parameter)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(params, vec!["@id", "@name"]);
    }

    #[test]
    fn test_colon_not_parameter_before_digit() {
        // Bare colon followed by a digit is not a named parameter
        let tokens = tokenize("a:1");
        assert!(tokens.iter().all(|t| t.kind != TokenKind::Parameter));
    }

    #[test]
    fn test_dot_qualified_field() {
        let tokens = tokenize("u.email");
        assert_eq!(tokens[0].kind, TokenKind::Default); // u
        assert_eq!(tokens[1].kind, TokenKind::Default); // .
        assert_eq!(tokens[2].kind, TokenKind::Field); // email
    }

    #[test]
    fn test_multiple_qualified_fields() {
        let tokens = tokenize("SELECT u.username, t.title FROM users u");
        let fields: Vec<&str> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Field)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(fields, vec!["username", "title"]);
    }

    #[test]
    fn test_dot_qualified_function_beats_field() {
        // is_fn_call takes priority over after_dot: t.count(...) → Function, not Field
        let tokens = tokenize("t.count(*)");
        let count_tok = tokens.iter().find(|t| t.text == "count").unwrap();
        assert_eq!(count_tok.kind, TokenKind::Function);
    }

    #[test]
    fn test_dot_qualified_type_name_becomes_field() {
        // after_dot takes priority over SQL_TYPES: t.text → Field, not Type
        let tokens = tokenize("t.text");
        let text_tok = tokens.iter().find(|t| t.text == "text").unwrap();
        assert_eq!(text_tok.kind, TokenKind::Field);
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
