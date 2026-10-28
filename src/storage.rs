//! Configuration file location and atomic persistence.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::config::{Config, ConfigError};

/// Overrides the configuration directory (useful for development and tests).
pub const CONFIG_DIR_ENV: &str = "RUSTCLICK_CONFIG_DIR";

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

/// Result of loading: always a usable configuration, plus anything the user
/// should be told about.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

impl ConfigStore {
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            path: dir.into().join("config.toml"),
        }
    }

    /// The per-user configuration directory for this OS
    /// (`~/.config/rustclick` on Linux, `%APPDATA%\RustClick\config` on
    /// Windows, `~/Library/Application Support/RustClick` on macOS), unless
    /// overridden by `RUSTCLICK_CONFIG_DIR`.
    pub fn default_location() -> Option<Self> {
        if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
            return Some(Self::in_dir(dir));
        }
        directories::ProjectDirs::from("", "", "RustClick").map(|d| Self::in_dir(d.config_dir()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the configuration. A missing file yields defaults; an invalid
    /// or unsupported file is moved aside and defaults are used.
    pub fn load(&self) -> Loaded {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Loaded {
                    config: Config::default(),
                    warnings: Vec::new(),
                };
            }
            Err(e) => {
                return Loaded {
                    config: Config::default(),
                    warnings: vec![format!(
                        "Could not read {}: {e}. Using default settings.",
                        self.path.display()
                    )],
                };
            }
        };
        match Config::from_toml(&text) {
            Ok(config) => Loaded {
                config,
                warnings: Vec::new(),
            },
            Err(e) => {
                let kept = match self.move_aside() {
                    Ok(backup) => format!("the previous file was kept as {}", backup.display()),
                    Err(err) => format!("the previous file could not be backed up ({err})"),
                };
                Loaded {
                    config: Config::default(),
                    warnings: vec![format!("{e}. Using default settings; {kept}.")],
                }
            }
        }
    }

    fn move_aside(&self) -> io::Result<PathBuf> {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let backup = self.path.with_extension(format!("toml.invalid-{secs}"));
        std::fs::rename(&self.path, &backup)?;
        Ok(backup)
    }

    /// Validates and writes the configuration atomically (temp file + rename).
    pub fn save(&self, config: &Config) -> Result<(), ConfigError> {
        config.validate()?;
        let text = config.to_toml()?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
