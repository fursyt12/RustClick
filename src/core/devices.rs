//! Device registry: which devices exist, which are enabled, and whether an
//! event from a given device may be considered for routing.

use std::fmt;
use std::time::SystemTime;

use thiserror::Error;

use super::input::{ButtonId, DeviceId, InputEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Simulated,
    Physical,
}

impl DeviceKind {
    /// Simulated devices start enabled. Physical devices are never enabled
    /// without an explicit user decision.
    pub fn enabled_by_default(self) -> bool {
        self == Self::Simulated
    }
}

/// Provider-reported description of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    pub connected: bool,
    /// Buttons this device can produce.
    pub buttons: Vec<ButtonId>,
    /// Free-form provider details (mock profile, vendor/product ids, ...).
    pub details: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceEntry {
    pub info: DeviceInfo,
    pub enabled: bool,
    pub last_activity: Option<SystemTime>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeviceError {
    #[error("unknown device `{0}`")]
    Unknown(DeviceId),
    #[error("device name must be 1 to 64 characters")]
    InvalidName,
}

/// Why an event's source device does not allow routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRejection {
    UnknownDevice,
    Disabled,
    Disconnected,
    UnsupportedButton(ButtonId),
}

impl fmt::Display for SourceRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDevice => f.write_str("unknown device"),
            Self::Disabled => f.write_str("device is disabled"),
            Self::Disconnected => f.write_str("device is disconnected"),
            Self::UnsupportedButton(b) => write!(f, "device does not report button `{b}`"),
        }
    }
}

pub fn validate_device_name(name: &str) -> Result<String, DeviceError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(DeviceError::InvalidName);
    }
    Ok(name.to_owned())
}

#[derive(Debug, Default)]
pub struct DeviceRegistry {
    entries: Vec<DeviceEntry>,
}

impl DeviceRegistry {
    /// Adds or updates a device. `enabled_pref` is the persisted user choice,
    /// if any; otherwise the kind's default applies to new devices.
    pub fn upsert(&mut self, info: DeviceInfo, enabled_pref: Option<bool>) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.info.id == info.id) {
            entry.info = info;
            if let Some(enabled) = enabled_pref {
                entry.enabled = enabled;
            }
        } else {
            let enabled = enabled_pref.unwrap_or(info.kind.enabled_by_default());
            self.entries.push(DeviceEntry {
                info,
                enabled,
                last_activity: None,
            });
        }
    }

    pub fn remove(&mut self, id: &DeviceId) -> Option<DeviceEntry> {
        let pos = self.entries.iter().position(|e| &e.info.id == id)?;
        Some(self.entries.remove(pos))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn get(&self, id: &DeviceId) -> Option<&DeviceEntry> {
        self.entries.iter().find(|e| &e.info.id == id)
    }

    fn get_mut(&mut self, id: &DeviceId) -> Result<&mut DeviceEntry, DeviceError> {
        self.entries
            .iter_mut()
            .find(|e| &e.info.id == id)
            .ok_or_else(|| DeviceError::Unknown(id.clone()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &DeviceEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn set_enabled(&mut self, id: &DeviceId, enabled: bool) -> Result<(), DeviceError> {
        self.get_mut(id)?.enabled = enabled;
        Ok(())
    }

    pub fn rename(&mut self, id: &DeviceId, name: &str) -> Result<(), DeviceError> {
        let name = validate_device_name(name)?;
        self.get_mut(id)?.info.name = name;
        Ok(())
    }

    pub fn touch(&mut self, id: &DeviceId, at: SystemTime) {
        if let Ok(entry) = self.get_mut(id) {
            entry.last_activity = Some(at);
        }
    }

    /// Devices whose events are currently eligible for routing.
    pub fn active(&self) -> impl Iterator<Item = &DeviceEntry> {
        self.entries
            .iter()
            .filter(|e| e.enabled && e.info.connected)
    }

    /// Checks that `event` comes from a known, enabled, connected device that
    /// actually reports the event's button.
    pub fn check_source(&self, event: &InputEvent) -> Result<(), SourceRejection> {
        let entry = self
            .get(&event.device_id)
            .ok_or(SourceRejection::UnknownDevice)?;
        if !entry.enabled {
            return Err(SourceRejection::Disabled);
        }
        if !entry.info.connected {
            return Err(SourceRejection::Disconnected);
        }
        if !entry.info.buttons.contains(&event.button) {
            return Err(SourceRejection::UnsupportedButton(event.button.clone()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::core::input::{EventKind, EventOrigin};

    fn info(id: &str, kind: DeviceKind) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: id.to_uppercase(),
            kind,
            connected: true,
            buttons: vec![ButtonId::standard("forward"), ButtonId::standard("back")],
            details: String::new(),
        }
    }

    fn event(id: &str, button: &str) -> InputEvent {
        InputEvent {
            device_id: DeviceId::new(id),
            device_name: id.into(),
            sequence: 1,
            button: ButtonId::standard(button),
            kind: EventKind::Click,
            origin: EventOrigin::Simulated,
            timestamp: SystemTime::now(),
            captured_at: Instant::now(),
            metadata: vec![],
        }
    }

    #[test]
    fn physical_devices_are_never_enabled_implicitly() {
        let mut reg = DeviceRegistry::default();
        reg.upsert(info("hw", DeviceKind::Physical), None);
        reg.upsert(info("sim", DeviceKind::Simulated), None);
        assert!(!reg.get(&DeviceId::new("hw")).unwrap().enabled);
        assert!(reg.get(&DeviceId::new("sim")).unwrap().enabled);
        // A persisted preference wins.
        reg.upsert(info("hw2", DeviceKind::Physical), Some(true));
        assert!(reg.get(&DeviceId::new("hw2")).unwrap().enabled);
    }

    #[test]
    fn upsert_keeps_enablement_when_no_preference_given() {
        let mut reg = DeviceRegistry::default();
        reg.upsert(info("sim", DeviceKind::Simulated), None);
        reg.set_enabled(&DeviceId::new("sim"), false).unwrap();
        let mut updated = info("sim", DeviceKind::Simulated);
        updated.connected = false;
        reg.upsert(updated, None);
        let entry = reg.get(&DeviceId::new("sim")).unwrap();
        assert!(!entry.enabled);
        assert!(!entry.info.connected);
    }

    #[test]
    fn source_filtering() {
        let mut reg = DeviceRegistry::default();
        reg.upsert(info("a", DeviceKind::Simulated), None);
        assert_eq!(reg.check_source(&event("a", "forward")), Ok(()));
        assert_eq!(
            reg.check_source(&event("a", "top")),
            Err(SourceRejection::UnsupportedButton(ButtonId::standard(
                "top"
            )))
        );
        assert_eq!(
            reg.check_source(&event("zzz", "forward")),
            Err(SourceRejection::UnknownDevice)
        );

        reg.set_enabled(&DeviceId::new("a"), false).unwrap();
        assert_eq!(
            reg.check_source(&event("a", "forward")),
            Err(SourceRejection::Disabled)
        );
        assert_eq!(reg.active().count(), 0);

        reg.set_enabled(&DeviceId::new("a"), true).unwrap();
        let mut disconnected = info("a", DeviceKind::Simulated);
        disconnected.connected = false;
        reg.upsert(disconnected, None);
        assert_eq!(
            reg.check_source(&event("a", "forward")),
            Err(SourceRejection::Disconnected)
        );
    }

    #[test]
    fn rename_validates() {
        let mut reg = DeviceRegistry::default();
        reg.upsert(info("a", DeviceKind::Simulated), None);
        assert_eq!(
            reg.rename(&DeviceId::new("a"), "   "),
            Err(DeviceError::InvalidName)
        );
        reg.rename(&DeviceId::new("a"), "  Stage left  ").unwrap();
        assert_eq!(
            reg.get(&DeviceId::new("a")).unwrap().info.name,
            "Stage left"
        );
        assert!(matches!(
            reg.rename(&DeviceId::new("b"), "x"),
            Err(DeviceError::Unknown(_))
        ));
    }
}
