//! Helpers shared by the integration tests. Everything runs on mock
//! providers; no window or device is ever opened.

#![allow(dead_code)]

use rustclick::app::{AppService, Command};
use rustclick::core::config::Config;
use rustclick::core::input::{DeviceId, EventKind};
use rustclick::core::targets::{ProcessKey, WindowId};
use rustclick::targets::mock::{MockApp, MockTargetProvider};

/// The mock device created by the default configuration.
pub fn remote() -> DeviceId {
    DeviceId::new("mock-1")
}

/// A service in simulation mode whose mock world contains `apps`.
pub fn service_with(apps: &[MockApp]) -> AppService {
    service_with_config(Config::default(), apps)
}

pub fn service_with_config(config: Config, apps: &[MockApp]) -> AppService {
    let mut world = MockTargetProvider::new();
    for app in apps {
        world.launch(*app);
    }
    AppService::with_mock_targets(config, None, Vec::new(), world)
}

pub fn send(service: &mut AppService, device: &DeviceId, button: &str, kind: EventKind) {
    service.execute(Command::EmitMockEvent {
        device: device.clone(),
        button: button.into(),
        kind,
    });
}

pub fn click(service: &mut AppService, button: &str) {
    send(service, &remote(), button, EventKind::Click);
}

/// Windows whose application name contains `app`, in creation order.
pub fn windows_of(service: &AppService, app: &str) -> Vec<WindowId> {
    let snap = service.snapshot();
    snap.windows
        .iter()
        .filter(|w| {
            snap.process(w.process)
                .is_some_and(|p| p.identity.name.contains(app))
        })
        .map(|w| w.id)
        .collect()
}

pub fn processes_of(service: &AppService, app: &str) -> Vec<ProcessKey> {
    service
        .snapshot()
        .processes
        .iter()
        .filter(|p| p.identity.name.contains(app))
        .map(|p| p.key)
        .collect()
}

/// The newest log message, for asserting on routing decisions.
pub fn last_decision(service: &AppService) -> String {
    service
        .view()
        .log
        .iter()
        .find(|e| e.category == rustclick::app::LogCategory::Decision)
        .map(|e| e.message.clone())
        .unwrap_or_default()
}
