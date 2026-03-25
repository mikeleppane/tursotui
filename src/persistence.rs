use std::io;
use std::path::PathBuf;

use crate::config::app_config_dir;

/// FNV-1a hash (64-bit) — deterministic across Rust versions.
fn fnv1a_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Compute the buffer filename for a database path.
pub(crate) fn buffer_filename(db_path: &str) -> String {
    if db_path == ":memory:" {
        "_memory_.sql".to_string()
    } else {
        format!("{:016x}.sql", fnv1a_hash(db_path.as_bytes()))
    }
}

/// Compute the parameter persistence filename for a database path.
#[allow(dead_code)] // will be used when parameter persistence is wired to the editor
pub(crate) fn params_filename(db_path: &str) -> String {
    if db_path == ":memory:" {
        "_memory_.params.json".to_string()
    } else {
        format!("{:016x}.params.json", fnv1a_hash(db_path.as_bytes()))
    }
}

fn buffers_dir() -> Option<PathBuf> {
    app_config_dir().map(|p| p.join("buffers"))
}

/// Save the editor buffer to disk using atomic write (temp file + rename).
pub(crate) fn save_buffer(db_path: &str, contents: &str) -> io::Result<()> {
    let Some(dir) = buffers_dir() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No config directory available",
        ));
    };
    std::fs::create_dir_all(&dir)?;
    let filename = buffer_filename(db_path);
    let target = dir.join(&filename);
    let tmp = dir.join(format!("{filename}.tmp"));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, &target)
}

/// Load a saved editor buffer, if one exists.
pub(crate) fn load_buffer(db_path: &str) -> Option<String> {
    let dir = buffers_dir()?;
    let path = dir.join(buffer_filename(db_path));
    std::fs::read_to_string(path).ok()
}

/// Delete the saved buffer file. Treats `NotFound` as success.
pub(crate) fn delete_buffer(db_path: &str) -> io::Result<()> {
    let Some(dir) = buffers_dir() else {
        return Ok(());
    };
    let path = dir.join(buffer_filename(db_path));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Save parameter values to disk as JSON.
///
/// Serializes as a JSON object: `{"?1": "42", "?2": null}`.
/// If `params` is empty, deletes the file (no params to save).
#[allow(dead_code)] // will be used when parameter persistence is wired to the editor
pub(crate) fn save_params(db_path: &str, params: &[(String, Option<String>)]) -> io::Result<()> {
    if params.is_empty() {
        return delete_params(db_path);
    }
    let Some(dir) = buffers_dir() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No config directory available",
        ));
    };
    std::fs::create_dir_all(&dir)?;
    let map: serde_json::Map<String, serde_json::Value> = params
        .iter()
        .map(|(name, value)| {
            let v = match value {
                Some(s) => serde_json::Value::String(s.clone()),
                None => serde_json::Value::Null,
            };
            (name.clone(), v)
        })
        .collect();
    let json = serde_json::to_string_pretty(&serde_json::Value::Object(map))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let filename = params_filename(db_path);
    let target = dir.join(&filename);
    let tmp = dir.join(format!("{filename}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &target)
}

/// Load saved parameter values from disk.
///
/// Returns `None` if the file does not exist, cannot be parsed, or is empty.
#[allow(dead_code)] // will be used when parameter persistence is wired to the editor
pub(crate) fn load_params(db_path: &str) -> Option<Vec<(String, Option<String>)>> {
    let dir = buffers_dir()?;
    let path = dir.join(params_filename(db_path));
    let contents = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let obj = value.as_object()?;
    let params: Vec<(String, Option<String>)> = obj
        .iter()
        .map(|(key, val)| {
            let v = match val {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Null => None,
                other => Some(other.to_string()),
            };
            (key.clone(), v)
        })
        .collect();
    if params.is_empty() {
        None
    } else {
        Some(params)
    }
}

/// Delete the saved params file. Treats `NotFound` as success.
#[allow(dead_code)] // will be used when parameter persistence is wired to the editor
pub(crate) fn delete_params(db_path: &str) -> io::Result<()> {
    let Some(dir) = buffers_dir() else {
        return Ok(());
    };
    let path = dir.join(params_filename(db_path));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_filename_memory() {
        assert_eq!(buffer_filename(":memory:"), "_memory_.sql");
    }

    #[test]
    fn buffer_filename_deterministic() {
        let a = buffer_filename("/home/user/test.db");
        let b = buffer_filename("/home/user/test.db");
        assert_eq!(a, b);
        assert!(
            std::path::Path::new(&a)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"))
        );
        assert_eq!(a.len(), 20); // 16 hex + ".sql"
    }

    #[test]
    fn buffer_filename_different_paths() {
        let a = buffer_filename("/a/test.db");
        let b = buffer_filename("/b/test.db");
        assert_ne!(a, b);
    }

    #[test]
    fn fnv1a_stability() {
        // Pin a known hash to catch accidental algorithm changes.
        // If this fails, the hash algorithm was modified — all existing
        // buffer files will become orphaned.
        assert_eq!(
            buffer_filename("/home/user/test.db"),
            "921cb74647db693a.sql"
        );
    }

    #[test]
    fn params_filename_memory() {
        assert_eq!(params_filename(":memory:"), "_memory_.params.json");
    }

    #[test]
    fn params_filename_deterministic() {
        let a = params_filename("/home/user/test.db");
        let b = params_filename("/home/user/test.db");
        assert_eq!(a, b);
        assert!(a.ends_with(".params.json"));
    }

    #[test]
    fn params_filename_different_paths() {
        let a = params_filename("/a/test.db");
        let b = params_filename("/b/test.db");
        assert_ne!(a, b);
    }

    #[test]
    fn params_filename_uses_same_hash_as_buffer() {
        // The params file must use the same hash as the buffer file for the
        // same db path, just with a different extension.
        let buf = buffer_filename("/home/user/test.db");
        let par = params_filename("/home/user/test.db");
        let buf_hash = buf.trim_end_matches(".sql");
        let par_hash = par.trim_end_matches(".params.json");
        assert_eq!(buf_hash, par_hash);
    }
}
