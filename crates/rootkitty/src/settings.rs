//! Configuration and settings management

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use crate::ui::SortMode;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub ui: UiSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSettings {
    #[serde(default = "default_file_tree_sort")]
    pub file_tree_sort: SortMode,
    #[serde(default = "default_scan_list_sort")]
    pub scan_list_sort: SortMode,
    #[serde(default = "default_auto_fold_depth")]
    pub auto_fold_depth: u32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            file_tree_sort: default_file_tree_sort(),
            scan_list_sort: default_scan_list_sort(),
            auto_fold_depth: default_auto_fold_depth(),
        }
    }
}

fn default_file_tree_sort() -> SortMode {
    SortMode::ByPath
}

fn default_scan_list_sort() -> SortMode {
    SortMode::BySize
}

fn default_auto_fold_depth() -> u32 {
    1
}

impl Settings {
    /// Load settings from a file, or return defaults if file doesn't exist
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            // Return defaults if file doesn't exist
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read settings from {}", path.display()))?;

        let settings: Settings = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse settings from {}", path.display()))?;

        Ok(settings)
    }

    /// Save settings to a file
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        let contents = toml::to_string_pretty(self).context("Failed to serialize settings")?;

        std::fs::write(path, contents)
            .with_context(|| format!("Failed to write settings to {}", path.display()))?;

        Ok(())
    }

    /// Get the default settings file path (next to database)
    pub fn default_path() -> PathBuf {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rootkitty");

        config_dir.join("settings.toml")
    }
}

// We need dirs crate for cross-platform config directory
// For now, implement a simple fallback
mod dirs {
    use std::path::PathBuf;

    pub fn config_dir() -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
        }
        #[cfg(target_os = "linux")]
        {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })
        }
        #[cfg(target_os = "windows")]
        {
            std::env::var_os("APPDATA").map(PathBuf::from)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_settings() {
        let settings = Settings::default();
        assert_eq!(settings.ui.file_tree_sort, SortMode::ByPath);
        assert_eq!(settings.ui.scan_list_sort, SortMode::BySize);
        assert_eq!(settings.ui.auto_fold_depth, 1);
    }

    #[test]
    fn test_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.toml");

        // Create custom settings
        let mut settings = Settings::default();
        settings.ui.file_tree_sort = SortMode::BySize;
        settings.ui.scan_list_sort = SortMode::ByPath;

        // Save
        settings.save(&settings_path).unwrap();

        // Load
        let loaded = Settings::load(&settings_path).unwrap();
        assert_eq!(loaded.ui.file_tree_sort, SortMode::BySize);
        assert_eq!(loaded.ui.scan_list_sort, SortMode::ByPath);
    }

    #[test]
    fn test_load_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("nonexistent.toml");

        // Should return defaults without error
        let settings = Settings::load(&settings_path).unwrap();
        assert_eq!(settings.ui.file_tree_sort, SortMode::ByPath);
        assert_eq!(settings.ui.scan_list_sort, SortMode::BySize);
    }

    #[test]
    fn test_sort_mode_toggle() {
        // Test ByPath toggles to BySize
        let mode = SortMode::ByPath;
        assert_eq!(mode.toggle(), SortMode::BySize);

        // Test BySize toggles to ByPath
        let mode = SortMode::BySize;
        assert_eq!(mode.toggle(), SortMode::ByPath);

        // Test double toggle returns to original
        let mode = SortMode::ByPath;
        assert_eq!(mode.toggle().toggle(), SortMode::ByPath);
    }

    #[test]
    fn test_multiple_saves_overwrite() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.toml");

        // Save initial settings
        let mut settings = Settings::default();
        settings.ui.file_tree_sort = SortMode::BySize;
        settings.save(&settings_path).unwrap();

        // Verify first save
        let loaded = Settings::load(&settings_path).unwrap();
        assert_eq!(loaded.ui.file_tree_sort, SortMode::BySize);

        // Toggle and save again
        settings.ui.file_tree_sort = settings.ui.file_tree_sort.toggle();
        settings.save(&settings_path).unwrap();

        // Verify second save overwrote the first
        let loaded = Settings::load(&settings_path).unwrap();
        assert_eq!(loaded.ui.file_tree_sort, SortMode::ByPath);
    }

    #[test]
    fn test_settings_file_format() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir.path().join("settings.toml");

        // Create settings with known values
        let mut settings = Settings::default();
        settings.ui.file_tree_sort = SortMode::BySize;
        settings.ui.scan_list_sort = SortMode::ByPath;
        settings.ui.auto_fold_depth = 3;

        // Save
        settings.save(&settings_path).unwrap();

        // Read the raw file content
        let content = std::fs::read_to_string(&settings_path).unwrap();

        // Verify TOML format contains expected keys
        assert!(content.contains("file_tree_sort"));
        assert!(content.contains("scan_list_sort"));
        assert!(content.contains("auto_fold_depth"));
        assert!(content.contains("by_size"));
        assert!(content.contains("by_path"));
    }

    #[test]
    fn test_sort_mode_display_names() {
        assert_eq!(SortMode::BySize.display_name(), "By Size (Descending)");
        assert_eq!(SortMode::ByPath.display_name(), "Alphabetical (by path)");
    }

    #[test]
    fn test_settings_creates_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir
            .path()
            .join("subdir")
            .join("nested")
            .join("settings.toml");

        // Parent directories don't exist yet
        assert!(!nested_path.parent().unwrap().exists());

        // Save should create parent directories
        let settings = Settings::default();
        settings.save(&nested_path).unwrap();

        // Verify file was created
        assert!(nested_path.exists());

        // Verify it can be loaded
        let loaded = Settings::load(&nested_path).unwrap();
        assert_eq!(loaded.ui.file_tree_sort, SortMode::ByPath);
    }
}
