//! `tracing` setup with a runtime-adjustable level.

use std::io::IsTerminal;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, fmt, reload};

use crate::core::config::LogLevel;

pub struct LogControl {
    handle: reload::Handle<EnvFilter, Registry>,
    /// `RUST_LOG` was set; it takes precedence over the configured level.
    env_override: bool,
}

fn filter_for(level: LogLevel) -> EnvFilter {
    EnvFilter::new(format!("warn,rustclick={level}"))
}

/// Installs the global subscriber. `RUST_LOG`, if set and valid, overrides
/// the configured level for the whole session.
pub fn init(level: LogLevel) -> LogControl {
    let env_filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| EnvFilter::try_new(s).ok());
    let env_override = env_filter.is_some();
    let (layer, handle) = reload::Layer::new(env_filter.unwrap_or_else(|| filter_for(level)));
    // Ignore the error if a subscriber is already installed (e.g. in tests).
    let _ = tracing_subscriber::registry()
        .with(layer)
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(std::io::stderr().is_terminal()),
        )
        .try_init();
    LogControl {
        handle,
        env_override,
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
}
