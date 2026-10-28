//! Versioned, validated application configuration.
//!
//! Only stable data is persisted: device ids, preferences, mappings and
//! target-matching criteria. PIDs and window handles never are.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::devices::validate_device_name;
use super::input::{DeviceId, normalize_button};
use super::mappings::{Action, Mappings};
use super::targets::{
    AppMatcher, MultipleTargetsPolicy, SelectionMode, TargetCriteria, TargetLostPolicy,
    TargetPolicy,
};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    /// Mock devices and mock targets only.
    #[default]
    Simulation,
    /// Platform input and target adapters.
    Native,
}

impl InputMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Simulation => "Simulation",
            Self::Native => "Native",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub const ALL: [Self; 5] = [
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Debug,
        Self::Trace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub selection_mode: SelectionMode,
    /// Select the only eligible target automatically.
    pub auto_select_single: bool,
    pub on_target_lost: TargetLostPolicy,
    pub on_multiple_targets: MultipleTargetsPolicy,
    /// Global shortcut for changing the target, e.g. `Ctrl+Alt+T`.
    pub change_target_shortcut: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            selection_mode: SelectionMode::Window,
            auto_select_single: true,
            on_target_lost: TargetLostPolicy::AutoRebind,
            on_multiple_targets: MultipleTargetsPolicy::KeepCurrent,
            change_target_shortcut: "Ctrl+Alt+T".into(),
        }
    }
}

impl Preferences {
    pub fn target_policy(&self) -> TargetPolicy {
        TargetPolicy {
            preferred_mode: self.selection_mode,
            auto_select_single: self.auto_select_single,
            on_target_lost: self.on_target_lost,
            on_multiple: self.on_multiple_targets,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: LogLevel,
}

/// Persisted per-device settings. Mock devices also store their profile so
/// they can be recreated on the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSettings {
    pub id: DeviceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub input_mode: InputMode,
    pub preferences: Preferences,
    pub logging: LoggingConfig,
    pub mappings: Mappings,
    pub saved_target: Option<TargetCriteria>,
    pub eligible_apps: Vec<AppMatcher>,
    pub devices: Vec<DeviceSettings>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            input_mode: InputMode::Simulation,
            preferences: Preferences::default(),
            logging: LoggingConfig::default(),
            mappings: Mappings::default(),
            saved_target: None,
            eligible_apps: AppMatcher::presentation_defaults(),
            devices: vec![DeviceSettings {
                id: DeviceId::new("mock-1"),
                name: Some("Mock remote 1".into()),
                enabled: true,
                mock_profile: Some("pointer_remote".into()),
            }],
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration could not be parsed: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("configuration version {found} is not supported (expected {CONFIG_VERSION})")]
    UnsupportedVersion { found: u32 },
    #[error("configuration is invalid: {}", .0.join("; "))]
    Invalid(Vec<String>),
    #[error("configuration could not be serialized: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("configuration file error: {0}")]
    Io(#[from] std::io::Error),
}

impl Config {
    /// Parses and validates a TOML document. Missing fields take defaults.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: self.version,
            });
        }
        let mut problems = Vec::new();

        let mut ids = HashSet::new();
        for d in &self.devices {
            if d.id.as_str().trim().is_empty() {
                problems.push("device id must not be empty".to_owned());
            }
            if !ids.insert(&d.id) {
                problems.push(format!("duplicate device id `{}`", d.id));
            }
            if let Some(name) = &d.name
                && validate_device_name(name).is_err()
            {
                problems.push(format!("device `{}` has an invalid name", d.id));
            }
        }

        for (button, action) in self.mappings.iter() {
            if normalize_button(button.as_str()).as_ref() != Some(button) {
                problems.push(format!("mapping uses non-normalized button `{button}`"));
            }
            if let Action::Custom(name) = action
                && Action::custom(name).is_err()
            {
                problems.push(format!("custom action for `{button}` has an invalid name"));
            }
        }

        if let Some(target) = &self.saved_target {
            if target.identity.name.trim().is_empty() {
                problems.push("saved target has no application name".into());
            }
            if target.mode == SelectionMode::Window && target.window_title.is_none() {
                problems.push("saved window target has no window title".into());
            }
        }

        for app in &self.eligible_apps {
            if app.name.trim().is_empty() {
                problems.push("eligible application without a name".into());
            }
            if app.executables.is_empty() && app.bundle_ids.is_empty() {
                problems.push(format!(
                    "eligible application `{}` needs an executable or bundle id",
                    app.name
                ));
            }
        }

        if let Err(e) = validate_shortcut(&self.preferences.change_target_shortcut) {
            problems.push(e);
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(problems))
        }
    }

    pub fn device(&self, id: &DeviceId) -> Option<&DeviceSettings> {
        self.devices.iter().find(|d| &d.id == id)
    }

    pub fn device_mut(&mut self, id: &DeviceId) -> Option<&mut DeviceSettings> {
        self.devices.iter_mut().find(|d| &d.id == id)
    }
}

const MODIFIERS: [&str; 6] = ["ctrl", "alt", "shift", "super", "cmd", "meta"];

/// Accepts an empty string (no shortcut) or `Modifier+...+Key` with at least
/// one modifier and exactly one non-modifier key.
pub fn validate_shortcut(shortcut: &str) -> Result<(), String> {
    let s = shortcut.trim();
    if s.is_empty() {
        return Ok(());
    }
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().expect("split yields at least one part");
    let mods_ok = !mods.is_empty()
        && mods
            .iter()
            .all(|m| MODIFIERS.contains(&m.to_ascii_lowercase().as_str()));
    let key_ok = !key.is_empty() && !MODIFIERS.contains(&key.to_ascii_lowercase().as_str());
    if mods_ok && key_ok {
        Ok(())
    } else {
        Err(format!(
            "invalid shortcut `{shortcut}` (expected e.g. Ctrl+Alt+T)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::targets::AppIdentity;

    #[test]
    fn default_config_is_valid_and_round_trips() {
        let config = Config::default();
        config.validate().unwrap();
        let text = config.to_toml().unwrap();
        assert_eq!(Config::from_toml(&text).unwrap(), config);
    }

    #[test]
    fn saved_target_round_trips_without_transient_ids() {
        let config = Config {
            saved_target: Some(TargetCriteria {
                mode: SelectionMode::Window,
                identity: AppIdentity {
                    name: "PowerPoint".into(),
                    process_name: "POWERPNT.EXE".into(),
                    executable: Some(r"C:\Office\POWERPNT.EXE".into()),
                    bundle_id: None,
                },
                window_title: Some("Deck.pptx".into()),
            }),
            ..Config::default()
        };
        let text = config.to_toml().unwrap();
        assert!(!text.contains("pid"), "{text}");
        assert!(!text.contains("window_id"), "{text}");
        assert_eq!(Config::from_toml(&text).unwrap(), config);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let config =
            Config::from_toml("version = 1\n[preferences]\nauto_select_single = false\n").unwrap();
        assert!(!config.preferences.auto_select_single);
        assert_eq!(config.preferences.change_target_shortcut, "Ctrl+Alt+T");
        assert_eq!(config.mappings, Mappings::default());
    }

    #[test]
    fn unsupported_version_is_rejected() {
        assert!(matches!(
            Config::from_toml("version = 99"),
            Err(ConfigError::UnsupportedVersion { found: 99 })
        ));
    }

    #[test]
    fn invalid_values_are_reported() {
        let text = r#"
            version = 1
            [[devices]]
            id = "a"
            enabled = true
            [[devices]]
            id = "a"
            enabled = false
            name = ""
        "#;
        match Config::from_toml(text) {
            Err(ConfigError::Invalid(problems)) => {
                assert!(problems.iter().any(|p| p.contains("duplicate")));
                assert!(problems.iter().any(|p| p.contains("invalid name")));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            Config::from_toml("version = 1\ninput_mode = \"telepathy\""),
            Err(ConfigError::Parse(_))
        ));
        assert!(matches!(
            Config::from_toml("version = 1\n[mappings]\nforward = \"explode\""),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn shortcut_validation() {
        assert!(validate_shortcut("").is_ok());
        assert!(validate_shortcut("Ctrl+Alt+T").is_ok());
        assert!(validate_shortcut("cmd+shift+F12").is_ok());
        assert!(validate_shortcut("T").is_err());
        assert!(validate_shortcut("Ctrl+Alt").is_err());
        assert!(validate_shortcut("Ctrl++").is_err());
        assert!(validate_shortcut("Hyper+T").is_err());
    }
}
