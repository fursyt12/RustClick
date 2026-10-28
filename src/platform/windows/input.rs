//! Windows input provider: Logitech remotes through Raw Input.
//!
//! Every Raw Input keyboard or consumer-control collection with Logitech's
//! vendor id is grouped by product id into one device. Keystrokes from
//! enabled devices become button events; the laptop keyboard and other
//! vendors' devices are never listed. Original keystrokes are not
//! suppressed yet (see `hidpp.rs` for the planned approach).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use crate::core::capabilities::Capability;
use crate::core::devices::{DeviceInfo, DeviceKind};
use crate::core::input::{
    ButtonId, DeviceId, EventKind, EventOrigin, InputEvent, normalize_button,
};
use crate::input::InputProvider;
use crate::provider::{ProviderError, ProviderMessage, ProviderSink};

use super::hidpp::LOGITECH_VID;
use super::keys;
use super::raw_input::{
    self, Capture, CaptureEvent, CaptureOptions, RawDevice, RawKind, USAGE_PAGE_CONSUMER,
};

struct Remote {
    info: DeviceInfo,
    /// Raw Input handles of the remote's collections currently present.
    handles: BTreeSet<usize>,
    enabled: bool,
    sequence: u64,
    /// Virtual keys currently held, to drop auto-repeat.
    pressed: BTreeSet<u16>,
}

#[derive(Default)]
struct Shared {
    sink: Option<ProviderSink>,
    remotes: BTreeMap<DeviceId, Remote>,
    handles: HashMap<usize, DeviceId>,
}

impl Shared {
    fn send(&self, msg: ProviderMessage) {
        if let Some(sink) = &self.sink
            && let Err(e) = sink.send(msg)
        {
            tracing::warn!("native input: message dropped: {e}");
        }
    }

    /// Registers a newly seen collection; announces new or reconnected remotes.
    fn add_collection(&mut self, dev: &RawDevice, names: &HashMap<u16, String>) {
        let Some(id) = remote_id(dev) else { return };
        self.handles.insert(dev.handle, id.clone());
        let pid = dev.pid.unwrap_or_default();
        let remote = self.remotes.entry(id.clone()).or_insert_with(|| Remote {
            info: DeviceInfo {
                id: id.clone(),
                name: names
                    .get(&pid)
                    .cloned()
                    .unwrap_or_else(|| format!("Logitech device {pid:04X}")),
                kind: DeviceKind::Physical,
                connected: false,
                buttons: ["forward", "back"].map(ButtonId::standard).to_vec(),
                details: String::new(),
            },
            handles: BTreeSet::new(),
            enabled: false,
            sequence: 0,
            pressed: BTreeSet::new(),
        });
        let is_new = remote.handles.is_empty() && remote.info.details.is_empty();
        let was_connected = remote.info.connected;
        remote.handles.insert(dev.handle);
        remote.info.connected = true;
        remote.info.details = format!(
            "Raw Input · VID {LOGITECH_VID:04X} PID {pid:04X} · {} collection(s)",
            remote.handles.len()
        );
        tracing::info!(
            device = %id,
            kind = dev.kind.label(),
            path = %dev.name,
            "native input: collection added"
        );
        let info = remote.info.clone();
        if is_new {
            self.send(ProviderMessage::DeviceAdded(info));
        } else if !was_connected {
            self.send(ProviderMessage::DeviceUpdated(info));
        }
    }

    fn remove_collection(&mut self, handle: usize) {
        let Some(id) = self.handles.remove(&handle) else {
            return;
        };
        let Some(remote) = self.remotes.get_mut(&id) else {
            return;
        };
        remote.handles.remove(&handle);
        tracing::info!(device = %id, "native input: collection removed");
        if remote.handles.is_empty() {
            remote.info.connected = false;
            remote.pressed.clear();
            let info = remote.info.clone();
            self.send(ProviderMessage::DeviceUpdated(info));
        }
    }

    fn on_key(&mut self, device: usize, vkey: u16, make_code: u16, flags: u16) {
        let Some(id) = self.handles.get(&device).cloned() else {
            return;
        };
        let Some(remote) = self.remotes.get_mut(&id) else {
            return;
        };
        // A disabled device may be a Logitech keyboard: its keys are neither
        // used nor logged.
        if !remote.enabled {
            return;
        }
        let up = CaptureEvent::is_key_up(flags);
        tracing::debug!(
            device = %id,
            "native input: {} {} (vk 0x{vkey:02x}, scan 0x{make_code:02x}, flags 0x{flags:x})",
            keys::vk_name(vkey),
            if up { "up" } else { "down" },
        );
        if vkey == 0xFF || keys::is_modifier(vkey) {
            return;
        }
        let kind = if up {
            if !remote.pressed.remove(&vkey) {
                return;
            }
            EventKind::Release
        } else {
            if !remote.pressed.insert(vkey) {
                return; // auto-repeat
            }
            EventKind::Press
        };
        let Some(button) = normalize_button(&keys::button_for_vk(vkey)) else {
            return;
        };
        let mut updated = None;
        if !remote.info.buttons.contains(&button) {
            remote.info.buttons.push(button.clone());
            updated = Some(remote.info.clone());
        }
        remote.sequence += 1;
        let event = InputEvent {
            device_id: id,
            device_name: remote.info.name.clone(),
            sequence: remote.sequence,
            button,
            kind,
            origin: EventOrigin::Native,
            timestamp: SystemTime::now(),
            captured_at: std::time::Instant::now(),
            metadata: vec![
                ("vk".into(), format!("0x{vkey:02x}")),
                ("key".into(), keys::vk_name(vkey)),
                ("scan".into(), format!("0x{make_code:02x}")),
            ],
        };
        // The device must list the button before the event is routed.
        if let Some(info) = updated {
            self.send(ProviderMessage::DeviceUpdated(info));
        }
        self.send(ProviderMessage::Input(event));
    }
}

/// Collections that belong to a remote: Logitech keyboard or consumer pages.
fn remote_id(dev: &RawDevice) -> Option<DeviceId> {
    let relevant = match dev.kind {
        RawKind::Keyboard => true,
        RawKind::Hid => dev.usage_page == USAGE_PAGE_CONSUMER,
        RawKind::Mouse => false,
    };
    (relevant && dev.vid == Some(LOGITECH_VID))
        .then(|| DeviceId::new(format!("logi-{:04x}", dev.pid.unwrap_or_default())))
}

/// Product strings by product id, from the HID API.
fn product_names() -> HashMap<u16, String> {
    let mut names = HashMap::new();
    match hidapi::HidApi::new() {
        Ok(api) => {
            for d in api.device_list() {
                if d.vendor_id() == LOGITECH_VID
                    && let Some(name) = d.product_string().filter(|s| !s.trim().is_empty())
                {
                    names
                        .entry(d.product_id())
                        .or_insert_with(|| format!("Logitech {}", name.trim()));
                }
            }
        }
        Err(e) => tracing::warn!("native input: HID enumeration failed: {e}"),
    }
    names
}

#[derive(Default)]
pub struct NativeInputProvider {
    shared: Arc<Mutex<Shared>>,
    capture: Option<Capture>,
}

impl NativeInputProvider {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

impl InputProvider for NativeInputProvider {
    fn name(&self) -> &'static str {
        "native input"
    }

    fn capabilities(&self) -> Vec<Capability> {
        super::input_capabilities()
    }

    fn start(&mut self, sink: ProviderSink) -> Result<(), ProviderError> {
        if self.capture.is_some() {
            return Ok(());
        }
        let names = product_names();
        {
            let mut shared = self.lock();
            shared.sink = Some(sink);
            for dev in raw_input::list_devices() {
                shared.add_collection(&dev, &names);
            }
        }
        let shared = self.shared.clone();
        let handler = Box::new(move |event: &CaptureEvent| {
            let mut s = lock(&shared);
            match *event {
                CaptureEvent::Key {
                    device,
                    vkey,
                    make_code,
                    flags,
                    ..
                } => s.on_key(device, vkey, make_code, flags),
                CaptureEvent::Hid {
                    device, ref report, ..
                } => {
                    if let Some(id) = s.handles.get(&device)
                        && s.remotes.get(id).is_some_and(|r| r.enabled)
                    {
                        tracing::debug!(device = %id, "native input: HID report {report:02x?}");
                    }
                }
                CaptureEvent::DeviceArrived(handle) => {
                    if !s.handles.contains_key(&handle)
                        && let Some(dev) = raw_input::device(handle)
                    {
                        s.add_collection(&dev, &names);
                    }
                }
                CaptureEvent::DeviceRemoved(handle) => s.remove_collection(handle),
                _ => {}
            }
            false
        });
        let options = CaptureOptions {
            keyboard: true,
            consumer: true,
            mouse: false,
            keyboard_hook: false,
        };
        match Capture::start(options, handler) {
            Ok(capture) => {
                self.capture = Some(capture);
                tracing::info!("native input: Raw Input capture started");
                Ok(())
            }
            Err(e) => {
                self.lock().sink = None;
                Err(ProviderError::Unsupported(format!(
                    "Raw Input capture could not start: {e}"
                )))
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            capture.stop();
            tracing::info!("native input: Raw Input capture stopped");
        }
        let mut shared = self.lock();
        shared.sink = None;
        shared.remotes.clear();
        shared.handles.clear();
    }

    fn is_running(&self) -> bool {
        self.capture.is_some()
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        self.lock()
            .remotes
            .values()
            .map(|r| r.info.clone())
            .collect()
    }

    fn set_device_enabled(&mut self, id: &DeviceId, enabled: bool) -> Result<(), ProviderError> {
        let mut shared = self.lock();
        let remote = shared
            .remotes
            .get_mut(id)
            .ok_or_else(|| ProviderError::UnknownDevice(id.clone()))?;
        remote.enabled = enabled;
        remote.pressed.clear();
        tracing::info!(device = %id, enabled, "native input: device enablement changed");
        Ok(())
    }
}
