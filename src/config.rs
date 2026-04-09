use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};

/// Returns the application config directory: `{config_dir}/tursotui/`
pub(crate) fn app_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("tursotui"))
}

/// Result of loading the application configuration.
pub(crate) struct ConfigLoadResult {
    pub(crate) config: AppConfig,
    pub(crate) error: Option<String>,
    /// `true` only when a new config file was successfully written (first launch).
    pub(crate) was_created: bool,
}

/// Loads `AppConfig` from `{config_dir}/tursotui/config.toml`.
///
/// On first launch (file does not exist), writes a default config file and
/// reports `was_created = true`. On parse/read/write failure, returns
/// defaults with an error message.
pub(crate) fn load_config() -> ConfigLoadResult {
    let Some(dir) = app_config_dir() else {
        return ConfigLoadResult {
            config: AppConfig::default(),
            error: None,
            was_created: false,
        };
    };
    load_config_from(&dir.join("config.toml"))
}

/// Path-based variant of [`load_config`]. Reads (or creates) a config file at
/// the given absolute path. Extracted to make the load/create branches
/// testable against a real filesystem in unit tests.
fn load_config_from(path: &Path) -> ConfigLoadResult {
    match fs::read_to_string(path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(config) => ConfigLoadResult {
                config,
                error: None,
                was_created: false,
            },
            Err(e) => ConfigLoadResult {
                config: AppConfig::default(),
                error: Some(format!("Failed to parse config: {e}")),
                was_created: false,
            },
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // First launch: write default config
            let config = AppConfig::default();
            match save_config_to(path, &config) {
                Ok(()) => ConfigLoadResult {
                    config,
                    error: None,
                    was_created: true,
                },
                Err(write_err) => ConfigLoadResult {
                    config,
                    error: Some(format!("Failed to create config: {write_err}")),
                    was_created: false,
                },
            }
        }
        Err(e) => ConfigLoadResult {
            config: AppConfig::default(),
            error: Some(format!("Failed to read config: {e}")),
            was_created: false,
        },
    }
}

/// Saves `AppConfig` to `{config_dir}/tursotui/config.toml`, creating the
/// directory on first write.
pub(crate) fn save_config(config: &AppConfig) -> io::Result<()> {
    let dir = app_config_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Could not determine config directory",
        )
    })?;
    save_config_to(&dir.join("config.toml"), config)
}

/// Path-based variant of [`save_config`]. Writes the serialized config to
/// the given absolute path, creating the parent directory if needed.
fn save_config_to(path: &Path, config: &AppConfig) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to serialize config: {e}"),
        )
    })?;
    fs::write(path, contents)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemeMode {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct ThemeConfig {
    #[serde(default)]
    pub(crate) mode: ThemeMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EditorConfig {
    #[serde(default = "default_tab_size")]
    pub(crate) tab_size: usize,
    /// Parsed from config but not yet wired to editor rendering (planned for M6).
    #[serde(default = "default_show_line_numbers")]
    pub(crate) show_line_numbers: bool,
    #[serde(default = "default_autocomplete")]
    pub(crate) autocomplete: bool,
    #[serde(default = "default_autocomplete_min_chars")]
    pub(crate) autocomplete_min_chars: usize,
}

fn default_tab_size() -> usize {
    4
}

fn default_show_line_numbers() -> bool {
    true
}

fn default_autocomplete() -> bool {
    true
}

fn default_autocomplete_min_chars() -> usize {
    1
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_size: default_tab_size(),
            show_line_numbers: default_show_line_numbers(),
            autocomplete: default_autocomplete(),
            autocomplete_min_chars: default_autocomplete_min_chars(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResultsConfig {
    #[serde(default = "default_max_column_width")]
    pub(crate) max_column_width: u16,
    #[serde(default = "default_null_display")]
    pub(crate) null_display: String,
}

fn default_max_column_width() -> u16 {
    40
}

fn default_null_display() -> String {
    String::from("NULL")
}

impl Default for ResultsConfig {
    fn default() -> Self {
        Self {
            max_column_width: default_max_column_width(),
            null_display: default_null_display(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HistoryConfig {
    #[serde(default = "default_max_entries")]
    pub(crate) max_entries: usize,
}

fn default_max_entries() -> usize {
    5000
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_entries: default_max_entries(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PerformanceConfig {
    #[serde(default = "default_slow_query_ms")]
    pub(crate) slow_query_ms: u64,
}

fn default_slow_query_ms() -> u64 {
    500
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            slow_query_ms: default_slow_query_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProfileConfig {
    #[serde(default = "default_sample_threshold")]
    pub(crate) sample_threshold: u64,
}

fn default_sample_threshold() -> u64 {
    10_000
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            sample_threshold: default_sample_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MouseConfig {
    #[serde(default = "default_mouse_mode")]
    pub(crate) mouse_mode: bool,
}

fn default_mouse_mode() -> bool {
    true
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            mouse_mode: default_mouse_mode(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct AppConfig {
    #[serde(default)]
    pub(crate) theme: ThemeConfig,
    #[serde(default)]
    pub(crate) editor: EditorConfig,
    #[serde(default)]
    pub(crate) results: ResultsConfig,
    #[serde(default)]
    pub(crate) history: HistoryConfig,
    #[serde(default)]
    pub(crate) performance: PerformanceConfig,
    #[serde(default)]
    pub(crate) profile: ProfileConfig,
    #[serde(default)]
    pub(crate) mouse: MouseConfig,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// Returns a unique path under the system temp directory for the current
    /// test. Combines pid + an atomic counter so concurrent `cargo test`
    /// invocations and parallel tests within a process never collide.
    fn unique_temp_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("tursotui_cfg_test_{pid}_{n}_{label}"))
    }

    /// Drop guard that recursively removes a directory on scope exit. Best-effort
    /// cleanup — failures (e.g., file already gone) are silently ignored.
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn default_config_round_trips() {
        let config = AppConfig::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: AppConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.theme.mode, ThemeMode::Dark);
        assert_eq!(deserialized.editor.tab_size, 4);
        assert!(deserialized.editor.autocomplete);
        assert_eq!(deserialized.editor.autocomplete_min_chars, 1);
        assert_eq!(deserialized.results.max_column_width, 40);
        assert_eq!(deserialized.history.max_entries, 5000);
    }

    #[test]
    fn partial_config_uses_defaults() {
        let partial = "[theme]\nmode = \"light\"\n";
        let config: AppConfig = toml::from_str(partial).unwrap();
        assert_eq!(config.theme.mode, ThemeMode::Light);
        assert_eq!(config.editor.tab_size, 4);
    }

    #[test]
    fn partial_config_autocomplete_defaults() {
        let partial = "[editor]\ntab_size = 2\n";
        let config: AppConfig = toml::from_str(partial).unwrap();
        assert_eq!(config.editor.tab_size, 2);
        assert!(config.editor.autocomplete);
        assert_eq!(config.editor.autocomplete_min_chars, 1);
    }

    #[test]
    fn config_without_performance_section_uses_defaults() {
        let config: AppConfig = toml::from_str("[theme]\nmode = \"dark\"\n").unwrap();
        assert_eq!(config.performance.slow_query_ms, 500);
    }

    #[test]
    fn invalid_theme_mode_rejected() {
        let bad = "[theme]\nmode = \"midnight\"\n";
        assert!(toml::from_str::<AppConfig>(bad).is_err());
    }

    #[test]
    fn empty_config_uses_all_defaults() {
        let config: AppConfig = toml::from_str("").unwrap();
        assert_eq!(config.theme.mode, ThemeMode::Dark);
    }

    #[test]
    fn load_config_from_creates_file_when_missing() {
        let dir = unique_temp_path("create");
        let _guard = TempDirGuard(dir.clone());
        let path = dir.join("config.toml");
        // Sanity: parent dir does not exist yet — load_config_from must create it.
        assert!(!dir.exists());

        let result = load_config_from(&path);

        assert!(result.was_created, "expected was_created = true");
        assert!(result.error.is_none(), "expected no error on first launch");
        assert!(path.exists(), "expected config file to be written");
        assert!(dir.exists(), "expected parent directory to be created");

        // The file on disk must be a valid serialized default config.
        let contents = fs::read_to_string(&path).expect("written file is readable");
        let parsed: AppConfig = toml::from_str(&contents).expect("written file is valid TOML");
        assert_eq!(parsed.theme.mode, ThemeMode::Dark);
        assert_eq!(parsed.editor.tab_size, 4);
    }

    #[test]
    fn load_config_from_reads_existing_file() {
        let dir = unique_temp_path("existing");
        let _guard = TempDirGuard(dir.clone());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "[theme]\nmode = \"light\"\n[editor]\ntab_size = 2\n").unwrap();

        let result = load_config_from(&path);

        assert!(
            !result.was_created,
            "must not report was_created when file already exists"
        );
        assert!(result.error.is_none());
        assert_eq!(result.config.theme.mode, ThemeMode::Light);
        assert_eq!(result.config.editor.tab_size, 2);
    }

    #[test]
    fn load_config_from_returns_error_on_parse_failure() {
        let dir = unique_temp_path("parse_err");
        let _guard = TempDirGuard(dir.clone());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "this is not [valid toml").unwrap();

        let result = load_config_from(&path);

        assert!(!result.was_created);
        let err = result.error.expect("expected parse error message");
        assert!(
            err.contains("Failed to parse config"),
            "error should mention parse failure: {err}"
        );
        // Should still return a usable default config so the app can run.
        assert_eq!(result.config.theme.mode, ThemeMode::Dark);
    }

    /// Exercises the write-failure branch in `load_config_from` by stripping
    /// write permission from the target directory. Unix-only because Windows
    /// permission semantics differ; transparently no-ops when running with
    /// permission bypass (e.g., root in CI containers) since we cannot force
    /// a write failure in that environment.
    #[cfg(unix)]
    #[test]
    fn load_config_from_returns_error_when_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_path("write_err");
        let _guard = TempDirGuard(dir.clone());
        fs::create_dir_all(&dir).unwrap();
        // Strip write permission from the directory; reads still work so
        // load_config_from sees NotFound for the file and tries to create it.
        let mut perms = fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&dir, perms).unwrap();
        let path = dir.join("config.toml");

        let result = load_config_from(&path);

        // Always restore writable permissions so TempDirGuard can clean up.
        if let Ok(meta) = fs::metadata(&dir) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&dir, perms);
        }

        // If the write somehow succeeded (root bypassing DAC, CAP_DAC_OVERRIDE,
        // unusual filesystem semantics), the failure-path assertions don't apply.
        // Skip silently rather than failing on environments where the test
        // precondition cannot be established.
        if result.was_created {
            return;
        }

        let err = result
            .error
            .expect("expected error message on write failure");
        assert!(
            err.contains("Failed to create config"),
            "error should mention create failure: {err}"
        );
        // Defaults must still be returned so the app can boot in a degraded mode.
        assert_eq!(result.config.theme.mode, ThemeMode::Dark);
    }

    #[test]
    fn save_config_to_writes_file_and_creates_parent_dir() {
        let dir = unique_temp_path("save");
        let _guard = TempDirGuard(dir.clone());
        let path = dir.join("nested").join("config.toml");
        assert!(!dir.exists());

        let mut config = AppConfig::default();
        config.theme.mode = ThemeMode::Light;
        save_config_to(&path, &config).expect("save_config_to should succeed");

        assert!(path.exists());
        let contents = fs::read_to_string(&path).unwrap();
        let parsed: AppConfig = toml::from_str(&contents).unwrap();
        assert_eq!(parsed.theme.mode, ThemeMode::Light);
    }
}
