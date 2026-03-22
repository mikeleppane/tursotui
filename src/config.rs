use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};

/// Returns the application config directory: `{config_dir}/tursotui/`
pub(crate) fn app_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("tursotui"))
}

/// Loads `AppConfig` from `{config_dir}/tursotui/config.toml`.
///
/// Returns `(defaults, None)` if the file does not exist.
/// Returns `(defaults, Some(error_message))` on parse/read failure.
pub(crate) fn load_config() -> (AppConfig, Option<String>) {
    let Some(dir) = app_config_dir() else {
        return (AppConfig::default(), None);
    };
    let path = dir.join("config.toml");
    match fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(config) => (config, None),
            Err(e) => (
                AppConfig::default(),
                Some(format!("Failed to parse config: {e}")),
            ),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => (AppConfig::default(), None),
        Err(e) => (
            AppConfig::default(),
            Some(format!("Failed to read config: {e}")),
        ),
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
    fs::create_dir_all(&dir)?;
    let contents = toml::to_string_pretty(config).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to serialize config: {e}"),
        )
    })?;
    fs::write(dir.join("config.toml"), contents)
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn invalid_theme_mode_rejected() {
        let bad = "[theme]\nmode = \"midnight\"\n";
        assert!(toml::from_str::<AppConfig>(bad).is_err());
    }

    #[test]
    fn empty_config_uses_all_defaults() {
        let config: AppConfig = toml::from_str("").unwrap();
        assert_eq!(config.theme.mode, ThemeMode::Dark);
    }
}
