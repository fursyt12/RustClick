//! Errors and capability limits are reflected in the UI-facing view model.

mod common;

use common::*;
use rustclick::app::{Command, Severity};
use rustclick::core::capabilities::CapabilityStatus;
use rustclick::core::config::{Config, InputMode};
use rustclick::core::input::DeviceId;
use rustclick::targets::mock::MockApp;

#[test]
fn unimplemented_native_mode_is_reported_honestly() {
    let config = Config {
        input_mode: InputMode::Native,
        ..Config::default()
    };
    let s = service_with_config(config, &[]);
    let view = s.view();

    assert_eq!(view.status.0, Severity::Error);
    assert!(view.can_start.is_err());
    assert!(view.native_mode.is_err());
    assert!(view.targets_error.is_some());
    assert!(view.warnings.iter().any(|w| w.contains("not implemented")));

    let native: Vec<_> = view
        .capabilities
        .iter()
        .filter(|s| s.title.starts_with("Native"))
        .flat_map(|s| &s.items)
        .collect();
    assert!(!native.is_empty());
    assert!(native.iter().all(|c| !c.status.is_operational()));
    assert!(native.iter().any(|c| c.status == CapabilityStatus::Error));
}

#[test]
fn native_mode_cannot_be_selected_while_unimplemented() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::SetInputMode(InputMode::Native));
    let view = s.view();
    assert_eq!(view.input_mode, InputMode::Simulation);
    assert!(
        view.log[0].message.contains("not implemented"),
        "{}",
        view.log[0].message
    );
}

#[test]
fn simulation_capabilities_are_supported() {
    let s = service_with(&[]);
    let view = s.view();
    let sim = view
        .capabilities
        .iter()
        .find(|s| s.title == "Simulation")
        .unwrap();
    assert!(sim.items.iter().all(|c| c.status.is_operational()));
    assert!(view.shortcut.status != CapabilityStatus::Supported);
}

#[test]
fn rejected_commands_are_logged() {
    let mut s = service_with(&[]);
    s.execute(Command::RenameDevice(remote(), "   ".into()));
    s.execute(Command::SetShortcut("Q".into()));
    s.execute(Command::RemoveDevice(DeviceId::new("nope")));
    let warnings: Vec<_> = s
        .view()
        .log
        .iter()
        .filter(|e| e.severity == Severity::Warning)
        .map(|e| e.message.clone())
        .collect();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
}

#[test]
fn disconnected_enabled_devices_raise_a_warning() {
    let mut s = service_with(&[]);
    s.execute(Command::SetDeviceConnected(remote(), false));
    assert!(s.view().warnings.iter().any(|w| w.contains("disconnected")));
}

#[test]
fn routing_state_and_last_result_are_exposed() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::StartRouting);
    click(&mut s, "forward");
    let view = s.view();
    assert_eq!(view.status.1, "Routing active (simulated)");
    let (severity, text) = view.last_result.unwrap();
    assert_eq!(severity, Severity::Success);
    assert!(text.contains("simulated delivery"), "{text}");
    assert!(view.last_event.unwrap().contains("forward click"));
}
