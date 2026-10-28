//! Simulated input devices, fully functional on every platform.

use std::time::{Instant, SystemTime};

use crate::core::capabilities::{Capability, CapabilityStatus};
use crate::core::devices::{DeviceInfo, DeviceKind, validate_device_name};
use crate::core::input::{
    ButtonId, DeviceId, EventKind, EventOrigin, InputEvent, normalize_button,
};
use crate::provider::{ProviderError, ProviderMessage, ProviderSink};

use super::InputProvider;

/// Button layouts for mock devices. These are generic layouts, not
/// emulations of specific hardware models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockProfile {
    TwoButton,
    ClickerWithHold,
    PointerRemote,
}

impl MockProfile {
    pub const ALL: [Self; 3] = [Self::TwoButton, Self::ClickerWithHold, Self::PointerRemote];

    pub fn key(self) -> &'static str {
        match self {
            Self::TwoButton => "two_button",
            Self::ClickerWithHold => "clicker_with_hold",
            Self::PointerRemote => "pointer_remote",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TwoButton => "Two-button clicker",
            Self::ClickerWithHold => "Clicker with hold actions",
            Self::PointerRemote => "Pointer remote",
        }
    }

    pub fn buttons(self) -> Vec<ButtonId> {
        let names: &[&str] = match self {
            Self::TwoButton => &["forward", "back"],
            Self::ClickerWithHold => &["forward", "back", "forward_hold", "back_hold"],
            Self::PointerRemote => &[
                "forward",
                "back",
                "forward_hold",
                "back_hold",
                "top",
                "top_hold",
                "aux",
            ],
        };
        names.iter().map(|n| ButtonId::standard(n)).collect()
    }
}

#[derive(Debug)]
struct MockDevice {
    info: DeviceInfo,
    profile: MockProfile,
    sequence: u64,
    last_event: Option<InputEvent>,
}

#[derive(Debug, Default)]
pub struct MockInputProvider {
    devices: Vec<MockDevice>,
    sink: Option<ProviderSink>,
    next_number: u32,
}

impl MockInputProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn profile(&self, id: &DeviceId) -> Option<MockProfile> {
        self.devices
            .iter()
            .find(|d| &d.info.id == id)
            .map(|d| d.profile)
    }

    fn device_mut(&mut self, id: &DeviceId) -> Result<&mut MockDevice, ProviderError> {
        self.devices
            .iter_mut()
            .find(|d| &d.info.id == id)
            .ok_or_else(|| ProviderError::UnknownDevice(id.clone()))
    }

    fn announce(&self, msg: ProviderMessage) -> Result<(), ProviderError> {
        match &self.sink {
            Some(sink) => Ok(sink.send(msg)?),
            None => Ok(()),
        }
    }

    /// Creates a mock device. Without an `id`, a fresh `mock-N` id is chosen.
    pub fn add_device(
        &mut self,
        id: Option<DeviceId>,
        name: Option<&str>,
        profile: MockProfile,
    ) -> Result<DeviceInfo, ProviderError> {
        let id = match id {
            Some(id) if self.devices.iter().any(|d| d.info.id == id) => {
                return Err(ProviderError::InvalidRequest(format!(
                    "device `{id}` already exists"
                )));
            }
            Some(id) => id,
            None => loop {
                self.next_number += 1;
                let candidate = DeviceId::new(format!("mock-{}", self.next_number));
                if !self.devices.iter().any(|d| d.info.id == candidate) {
                    break candidate;
                }
            },
        };
        let name = match name {
            Some(n) => {
                validate_device_name(n).map_err(|e| ProviderError::InvalidRequest(e.to_string()))?
            }
            None => format!("Mock remote {}", self.devices.len() + 1),
        };
        let info = DeviceInfo {
            id,
            name,
            kind: DeviceKind::Simulated,
            connected: true,
            buttons: profile.buttons(),
            details: profile.label().to_owned(),
        };
        self.devices.push(MockDevice {
            info: info.clone(),
            profile,
            sequence: 0,
            last_event: None,
        });
        self.announce(ProviderMessage::DeviceAdded(info.clone()))?;
        Ok(info)
    }

    pub fn remove_device(&mut self, id: &DeviceId) -> Result<(), ProviderError> {
        let pos = self
            .devices
            .iter()
            .position(|d| &d.info.id == id)
            .ok_or_else(|| ProviderError::UnknownDevice(id.clone()))?;
        self.devices.remove(pos);
        self.announce(ProviderMessage::DeviceRemoved(id.clone()))
    }

    pub fn rename_device(
        &mut self,
        id: &DeviceId,
        name: &str,
    ) -> Result<DeviceInfo, ProviderError> {
        let name =
            validate_device_name(name).map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;
        let device = self.device_mut(id)?;
        device.info.name = name;
        let info = device.info.clone();
        self.announce(ProviderMessage::DeviceUpdated(info.clone()))?;
        Ok(info)
    }

    /// Simulates plugging or unplugging the device.
    pub fn set_connected(
        &mut self,
        id: &DeviceId,
        connected: bool,
    ) -> Result<DeviceInfo, ProviderError> {
        let device = self.device_mut(id)?;
        device.info.connected = connected;
        let info = device.info.clone();
        self.announce(ProviderMessage::DeviceUpdated(info.clone()))?;
        Ok(info)
    }

    /// Generates a button event as if the physical button had been used.
    pub fn emit(
        &mut self,
        id: &DeviceId,
        raw_button: &str,
        kind: EventKind,
    ) -> Result<(), ProviderError> {
        let sink = self.sink.clone().ok_or(ProviderError::NotRunning)?;
        let device = self.device_mut(id)?;
        if !device.info.connected {
            return Err(ProviderError::InvalidRequest(format!(
                "{} is disconnected",
                device.info.name
            )));
        }
        let button = normalize_button(raw_button)
            .filter(|b| device.info.buttons.contains(b))
            .ok_or_else(|| {
                ProviderError::InvalidRequest(format!(
                    "{} has no button `{raw_button}`",
                    device.info.name
                ))
            })?;
        device.sequence += 1;
        let event = InputEvent {
            device_id: device.info.id.clone(),
            device_name: device.info.name.clone(),
            sequence: device.sequence,
            button,
            kind,
            origin: EventOrigin::Simulated,
            timestamp: SystemTime::now(),
            captured_at: Instant::now(),
            metadata: vec![("profile".into(), device.profile.key().into())],
        };
        device.last_event = Some(event.clone());
        Ok(sink.send(ProviderMessage::Input(event))?)
    }

    /// Re-sends the previous event with its original sequence number,
    /// simulating a driver that reports the same event twice.
    pub fn replay_last(&mut self, id: &DeviceId) -> Result<(), ProviderError> {
        let sink = self.sink.clone().ok_or(ProviderError::NotRunning)?;
        let device = self.device_mut(id)?;
        let mut event = device.last_event.clone().ok_or_else(|| {
            ProviderError::InvalidRequest(format!(
                "{} has not sent any event yet",
                device.info.name
            ))
        })?;
        event.timestamp = SystemTime::now();
        event.captured_at = Instant::now();
        Ok(sink.send(ProviderMessage::Input(event))?)
    }
}

impl InputProvider for MockInputProvider {
    fn name(&self) -> &'static str {
        "mock input"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new(
            "Mock input devices",
            CapabilityStatus::Supported,
            "Simulated remotes with configurable button layouts",
        )]
    }

    fn start(&mut self, sink: ProviderSink) -> Result<(), ProviderError> {
        self.sink = Some(sink);
        for d in &self.devices {
            self.announce(ProviderMessage::DeviceAdded(d.info.clone()))?;
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.sink = None;
    }

    fn is_running(&self) -> bool {
        self.sink.is_some()
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        self.devices.iter().map(|d| d.info.clone()).collect()
    }

    fn set_device_enabled(&mut self, id: &DeviceId, _enabled: bool) -> Result<(), ProviderError> {
        // Mock devices have nothing to grab; enablement is enforced by the router.
        self.device_mut(id).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn running() -> (MockInputProvider, Arc<Mutex<Vec<ProviderMessage>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let mut p = MockInputProvider::new();
        p.start(ProviderSink::new(move |m| {
            sink_seen.lock().unwrap().push(m);
            Ok(())
        }))
        .unwrap();
        (p, seen)
    }

    #[test]
    fn emits_normalized_events_with_increasing_sequence() {
        let (mut p, seen) = running();
        let info = p.add_device(None, None, MockProfile::TwoButton).unwrap();
        p.emit(&info.id, "Next", EventKind::Press).unwrap();
        p.emit(&info.id, "back", EventKind::Click).unwrap();
        let events: Vec<InputEvent> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                ProviderMessage::Input(e) => Some(e.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].button.as_str(), "forward");
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[1].sequence, 2);
        assert_eq!(events[0].origin, EventOrigin::Simulated);
    }

    #[test]
    fn rejects_buttons_the_profile_does_not_have() {
        let (mut p, _) = running();
        let info = p.add_device(None, None, MockProfile::TwoButton).unwrap();
        assert!(matches!(
            p.emit(&info.id, "top", EventKind::Click),
            Err(ProviderError::InvalidRequest(_))
        ));
    }

    #[test]
    fn disconnected_devices_cannot_emit() {
        let (mut p, _) = running();
        let info = p.add_device(None, None, MockProfile::TwoButton).unwrap();
        p.set_connected(&info.id, false).unwrap();
        assert!(p.emit(&info.id, "forward", EventKind::Click).is_err());
    }

    #[test]
    fn generated_ids_skip_restored_ones() {
        let (mut p, _) = running();
        p.add_device(
            Some(DeviceId::new("mock-1")),
            Some("Restored"),
            MockProfile::TwoButton,
        )
        .unwrap();
        let fresh = p.add_device(None, None, MockProfile::TwoButton).unwrap();
        assert_eq!(fresh.id.as_str(), "mock-2");
        assert!(
            p.add_device(Some(DeviceId::new("mock-1")), None, MockProfile::TwoButton)
                .is_err()
        );
    }

    #[test]
    fn replay_repeats_the_sequence_number() {
        let (mut p, seen) = running();
        let info = p.add_device(None, None, MockProfile::TwoButton).unwrap();
        p.emit(&info.id, "forward", EventKind::Click).unwrap();
        p.replay_last(&info.id).unwrap();
        let seqs: Vec<u64> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                ProviderMessage::Input(e) => Some(e.sequence),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, vec![1, 1]);
    }

    #[test]
    fn emitting_requires_a_running_provider() {
        let mut p = MockInputProvider::new();
        let info = p.add_device(None, None, MockProfile::TwoButton).unwrap();
        assert_eq!(
            p.emit(&info.id, "forward", EventKind::Click),
            Err(ProviderError::NotRunning)
        );
    }
}
