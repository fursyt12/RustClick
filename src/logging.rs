//! `tracing` setup with a runtime-adjustable level and an optional log file.

use std::fs::{File, OpenOptions};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, fmt, reload};

use crate::core::config::LogLevel;

/// The log file is moved aside when it grows beyond this size.
const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;

pub struct LogControl {
    handle: reload::Handle<EnvFilter, Registry>,
    /// `RUST_LOG` was set; it takes precedence over the configured level.
    env_override: bool,
    file: Result<Option<PathBuf>, String>,
}

fn filter_for(level: LogLevel) -> EnvFilter {
    EnvFilter::new(format!("warn,rustclick={level}"))
}

/// Opens the log file for appending, keeping one previous file as
/// `<name>.old.log` once it gets large.
fn open_log(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_SIZE) {
        std::fs::rename(path, path.with_extension("old.log"))?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// Installs the global subscriber. `RUST_LOG`, if set and valid, overrides
/// the configured level for the whole session. The console follows the
/// configured level; the log file, if any, always records debug details.
pub fn init(level: LogLevel, file: Option<&Path>) -> LogControl {
    let env_filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| EnvFilter::try_new(s).ok());
    let env_override = env_filter.is_some();
    let (filter, handle) = reload::Layer::new(env_filter.unwrap_or_else(|| filter_for(level)));
    let console = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_filter(filter);

    let (file_layer, file) = match file.map(|p| (p, open_log(p))) {
        None => (None, Ok(None)),
        Some((path, Ok(f))) => (
            Some(
                fmt::layer()
                    .with_ansi(false)
                    .with_writer(Mutex::new(f))
                    .with_filter(EnvFilter::new("warn,rustclick=debug")),
            ),
            Ok(Some(path.to_owned())),
        ),
        Some((path, Err(e))) => (
            None,
            Err(format!("Could not open log file {}: {e}", path.display())),
        ),
    };
    // Ignore the error if a subscriber is already installed (e.g. in tests).
    let _ = tracing_subscriber::registry()
        .with(console)
        .with(file_layer)
        .try_init();
    LogControl {
        handle,
        env_override,
        file,
    }
}

impl LogControl {
    pub fn set_level(&self, level: LogLevel) {
        if self.env_override {
            return;
        }
        if let Err(e) = self.handle.reload(filter_for(level)) {
            tracing::warn!("could not change log level: {e}");
        }
    }

    /// The log file in use, or why it could not be opened.
    pub fn file(&self) -> Result<Option<&Path>, &str> {
        match &self.file {
            Ok(path) => Ok(path.as_deref()),
            Err(e) => Err(e),
        }
    }
}
