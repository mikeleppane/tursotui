//! SQL value validation (pragma sanitization, etc.).

/// Validate and normalize a pragma value to a safe integer string.
///
/// Single source of truth for pragma value validation — used by both the UI
/// layer (`PragmaDashboard`) for fast feedback and the DB layer
/// (`run_set_pragma_inner`) for defense-in-depth.
///
/// Returns the normalized value string on success (trimmed, parsed and
/// re-formatted as a plain integer).
pub fn sanitize_pragma_value(name: &str, value: &str) -> Result<String, String> {
    match name {
        // Signed integer pragmas (negative cache_size means KB)
        "cache_size" | "busy_timeout" => {
            let n: i64 = value
                .trim()
                .parse()
                .map_err(|_| format!("{name} must be an integer"))?;
            Ok(n.to_string())
        }
        // Positive integer pragmas
        "max_page_count" => {
            let n: i64 = value
                .trim()
                .parse()
                .map_err(|_| "max_page_count must be a positive integer".to_string())?;
            if n > 0 {
                Ok(n.to_string())
            } else {
                Err("max_page_count must be a positive integer".to_string())
            }
        }
        // 0/1 boolean pragmas
        "foreign_keys" | "query_only" => match value.trim() {
            "0" | "1" => Ok(value.trim().to_string()),
            _ => Err(format!("{name} must be 0 or 1")),
        },
        // Turso only supports OFF (0) and FULL (2) — NORMAL (1) and EXTRA (3)
        // are not supported and would produce an opaque runtime error.
        "synchronous" => match value.trim() {
            "0" | "2" => Ok(value.trim().to_string()),
            _ => Err("synchronous must be 0 (OFF) or 2 (FULL) on Turso".to_string()),
        },
        "temp_store" => match value.trim() {
            "0" | "1" | "2" => Ok(value.trim().to_string()),
            _ => Err("temp_store must be 0-2".to_string()),
        },
        _ => Err(format!("{name} is not writable")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_size_valid_positive() {
        assert_eq!(sanitize_pragma_value("cache_size", "2000").unwrap(), "2000");
    }

    #[test]
    fn cache_size_valid_negative() {
        assert_eq!(
            sanitize_pragma_value("cache_size", "-4096").unwrap(),
            "-4096"
        );
    }

    #[test]
    fn cache_size_trims_whitespace() {
        assert_eq!(
            sanitize_pragma_value("cache_size", "  100  ").unwrap(),
            "100"
        );
    }

    #[test]
    fn cache_size_rejects_non_integer() {
        assert!(sanitize_pragma_value("cache_size", "abc").is_err());
    }

    #[test]
    fn busy_timeout_valid() {
        assert_eq!(
            sanitize_pragma_value("busy_timeout", "5000").unwrap(),
            "5000"
        );
    }

    #[test]
    fn busy_timeout_rejects_float() {
        assert!(sanitize_pragma_value("busy_timeout", "1.5").is_err());
    }

    #[test]
    fn max_page_count_valid() {
        assert_eq!(
            sanitize_pragma_value("max_page_count", "1073741823").unwrap(),
            "1073741823"
        );
    }

    #[test]
    fn max_page_count_rejects_zero() {
        assert!(sanitize_pragma_value("max_page_count", "0").is_err());
    }

    #[test]
    fn max_page_count_rejects_negative() {
        assert!(sanitize_pragma_value("max_page_count", "-1").is_err());
    }

    #[test]
    fn foreign_keys_accepts_zero_and_one() {
        assert_eq!(sanitize_pragma_value("foreign_keys", "0").unwrap(), "0");
        assert_eq!(sanitize_pragma_value("foreign_keys", "1").unwrap(), "1");
    }

    #[test]
    fn foreign_keys_rejects_other() {
        assert!(sanitize_pragma_value("foreign_keys", "2").is_err());
        assert!(sanitize_pragma_value("foreign_keys", "on").is_err());
    }

    #[test]
    fn query_only_accepts_zero_and_one() {
        assert_eq!(sanitize_pragma_value("query_only", "0").unwrap(), "0");
        assert_eq!(sanitize_pragma_value("query_only", "1").unwrap(), "1");
    }

    #[test]
    fn synchronous_accepts_off_and_full() {
        assert_eq!(sanitize_pragma_value("synchronous", "0").unwrap(), "0");
        assert_eq!(sanitize_pragma_value("synchronous", "2").unwrap(), "2");
    }

    #[test]
    fn synchronous_rejects_normal() {
        assert!(sanitize_pragma_value("synchronous", "1").is_err());
    }

    #[test]
    fn temp_store_accepts_valid_range() {
        assert_eq!(sanitize_pragma_value("temp_store", "0").unwrap(), "0");
        assert_eq!(sanitize_pragma_value("temp_store", "1").unwrap(), "1");
        assert_eq!(sanitize_pragma_value("temp_store", "2").unwrap(), "2");
    }

    #[test]
    fn temp_store_rejects_out_of_range() {
        assert!(sanitize_pragma_value("temp_store", "3").is_err());
    }

    #[test]
    fn unknown_pragma_rejected() {
        assert!(sanitize_pragma_value("page_size", "4096").is_err());
    }
}
