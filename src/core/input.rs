//! Normalized input events and button identifiers.
//!
//! Native adapters translate platform key codes and HID usages into these
//! types; the rest of the application never sees platform-specific input.

use std::fmt;
use std::time::{Instant, SystemTime};

use serde::{Deserialize, Serialize};

/// Stable identifier of an input device (e.g. `mock-1`, or a HID path hash).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Logical, model-independent button identifier.
///
/// Always holds a normalized identifier: lowercase ASCII letters, digits and
/// underscores. Construct through [`normalize_button`] or [`ButtonId::standard`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ButtonId(String);

/// Buttons common to presentation remotes. Devices report which subset they have.
pub const STANDARD_BUTTONS: [&str; 7] = [
    "forward",
    "back",
    "forward_hold",
    "back_hold",
    "top",
    "top_hold",
    "aux",
];

impl ButtonId {
    /// A button from [`STANDARD_BUTTONS`].
    ///
    /// # Panics
    /// Panics if `name` is not a standard button identifier.
    pub fn standard(name: &str) -> Self {
        assert!(
            STANDARD_BUTTONS.contains(&name),
            "not a standard button: {name}"
        );
        Self(name.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ButtonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ButtonId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        normalize_button(&value).ok_or_else(|| format!("invalid button identifier `{value}`"))
    }
}

impl From<ButtonId> for String {
    fn from(value: ButtonId) -> Self {
        value.0
    }
}

/// Normalizes a raw button name (from a mock device, a config file, or a
/// native key mapping) into a [`ButtonId`].
///
/// Common aliases map to the standard set (`next`/`page_down` → `forward`).
/// Other well-formed identifiers are accepted as custom buttons; anything
/// else is rejected.
pub fn normalize_button(raw: &str) -> Option<ButtonId> {
    let key = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let canonical = match key.as_str() {
        "forward" | "next" | "page_down" | "pagedown" | "right" => "forward",
        "back" | "previous" | "prev" | "page_up" | "pageup" | "left" => "back",
        "forward_hold" | "forward_long" | "next_hold" => "forward_hold",
        "back_hold" | "back_long" | "previous_hold" => "back_hold",
        "top" | "top_button" => "top",
        "top_hold" | "top_long" => "top_hold",
        other => {
            let valid = !other.is_empty()
                && other.len() <= 32
                && other
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            return valid.then(|| ButtonId(other.to_owned()));
        }
    };
    Some(ButtonId(canonical.to_owned()))
}

/// What happened to the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Press,
    Release,
    /// A discrete action with no separate press/release (e.g. a decoded gesture).
    Click,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Press => "press",
            Self::Release => "release",
            Self::Click => "click",
        }
    }
}

/// Where an event came from. Simulated events must never reach native applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOrigin {
    Simulated,
    Native,
}

/// A normalized input event as produced by any input provider.
#[derive(Debug, Clone)]
pub struct InputEvent {
    pub device_id: DeviceId,
    pub device_name: String,
    /// Per-device sequence number, strictly increasing. Used for duplicate detection.
    pub sequence: u64,
    pub button: ButtonId,
    pub kind: EventKind,
    pub origin: EventOrigin,
    /// Wall-clock time for display.
    pub timestamp: SystemTime,
    /// Monotonic capture time for staleness checks.
    pub captured_at: Instant,
    /// Optional platform-specific details (key code, HID usage, ...).
    pub metadata: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_normalize_to_standard_buttons() {
        assert_eq!(normalize_button("Next").unwrap().as_str(), "forward");
        assert_eq!(normalize_button(" page-down ").unwrap().as_str(), "forward");
        assert_eq!(normalize_button("PageUp").unwrap().as_str(), "back");
        assert_eq!(
            normalize_button("forward long").unwrap().as_str(),
            "forward_hold"
        );
        assert_eq!(normalize_button("top").unwrap().as_str(), "top");
    }

    #[test]
    fn custom_identifiers_are_accepted_when_well_formed() {
        assert_eq!(normalize_button("Volume_Up").unwrap().as_str(), "volume_up");
        assert!(normalize_button("").is_none());
        assert!(normalize_button("bad/name").is_none());
        assert!(normalize_button(&"x".repeat(33)).is_none());
    }

    #[test]
    fn button_ids_deserialize_through_normalization() {
        #[derive(Deserialize)]
        struct W {
            #[allow(dead_code)]
            b: ButtonId,
        }
        let w: W = toml::from_str("b = \"Next\"").unwrap();
        assert_eq!(w.b.as_str(), "forward");
        assert!(toml::from_str::<W>("b = \"no way\"").is_ok());
        assert!(toml::from_str::<W>("b = \"a.b\"").is_err());
    }
}
