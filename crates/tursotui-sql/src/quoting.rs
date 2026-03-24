//! SQL identifier and literal quoting/escaping.

/// Wrap `name` in double-quotes, doubling any internal `"`.
///
/// ```
/// assert_eq!(tursotui_sql::quoting::quote_identifier("col"), r#""col""#);
/// assert_eq!(tursotui_sql::quoting::quote_identifier(r#"a"b"#), r#""a""b""#);
/// ```
pub fn quote_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Wrap `value` in single-quotes, doubling any internal `'`.
///
/// ```
/// assert_eq!(tursotui_sql::quoting::quote_literal("hello"), "'hello'");
/// assert_eq!(tursotui_sql::quoting::quote_literal("it's"), "'it''s'");
/// ```
pub fn quote_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Format an `Option<&str>` as a SQL literal: `None` → `"NULL"`, `Some(v)` →
/// `quote_literal(v)`.
pub fn format_value(opt: Option<&str>) -> String {
    match opt {
        None => "NULL".to_owned(),
        Some(v) => quote_literal(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_identifier_simple() {
        assert_eq!(quote_identifier("col"), r#""col""#);
    }

    #[test]
    fn quote_identifier_with_quotes() {
        assert_eq!(quote_identifier(r#"a"b"#), r#""a""b""#);
    }

    #[test]
    fn quote_literal_simple() {
        assert_eq!(quote_literal("hello"), "'hello'");
    }

    #[test]
    fn quote_literal_with_quotes() {
        assert_eq!(quote_literal("it's"), "'it''s'");
    }

    #[test]
    fn format_value_none() {
        assert_eq!(format_value(None), "NULL");
    }

    #[test]
    fn format_value_some() {
        assert_eq!(format_value(Some("hello")), "'hello'");
    }

    #[test]
    fn format_value_some_with_quotes() {
        assert_eq!(format_value(Some("it's")), "'it''s'");
    }
}
