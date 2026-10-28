//! Routing behaviour through the full service, using mock providers.

mod common;

use std::time::{Instant, SystemTime};

use common::*;
use rustclick::app::{Command, Msg};
use rustclick::core::config::{Config, InputMode};
use rustclick::core::devices::{DeviceInfo, DeviceKind, DeviceRegistry};
use rustclick::core::input::{ButtonId, DeviceId, EventKind, EventOrigin, InputEvent};
use rustclick::core::mappings::{Action, Mappings};
use rustclick::core::routing::{
    Decision, DeliveryOutcome, RejectReason, RouteContext, RouteTarget, Router,
};
use rustclick::core::targets::{
    AppIdentity, ProcessInfo, ProcessKey, SelectionMode, TargetOrigin, TargetSelection,
    TargetSnapshot, WindowId, WindowInfo,
};
use rustclick::input::mock::MockProfile;
use rustclick::targets::TargetProvider;
use rustclick::targets::mock::MockApp;
use rustclick::targets::native::NativeTargetProvider;

#[test]
fn events_reach_only_the_selected_mock_window() {
    let mut s = service_with(&[MockApp::PowerPoint, MockApp::PowerPoint, MockApp::Acrobat]);
    let ppt = windows_of(&s, "PowerPoint");
    let target = ppt[1];
    s.execute(Command::SelectWindow(target));
    s.execute(Command::StartRouting);

    click(&mut s, "forward");
    click(&mut s, "back");

    assert_eq!(
        s.mock_targets().received(target),
        &[Action::Next, Action::Previous]
    );
    assert_eq!(s.mock_targets().total_received(), 2);
}

#[test]
fn disabled_devices_do_not_produce_routed_actions() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::StartRouting);
    s.execute(Command::SetDeviceEnabled(remote(), false));

    click(&mut s, "forward");

    assert_eq!(s.mock_targets().total_received(), 0);
    assert!(
        last_decision(&s).contains("disabled"),
        "{}",
        last_decision(&s)
    );

    s.execute(Command::SetDeviceEnabled(remote(), true));
    click(&mut s, "forward");
    assert_eq!(s.mock_targets().total_received(), 1);
}

#[test]
fn nothing_is_routed_while_routing_is_stopped() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    click(&mut s, "forward");
    assert_eq!(s.mock_targets().total_received(), 0);
    assert!(last_decision(&s).contains("stopped"));
}

#[test]
fn duplicate_and_release_events_are_not_routed_twice() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::StartRouting);

    send(&mut s, &remote(), "forward", EventKind::Press);
    send(&mut s, &remote(), "forward", EventKind::Release);
    assert_eq!(
        s.mock_targets().total_received(),
        1,
        "press routes, release does not"
    );

    s.execute(Command::ReplayLastEvent(remote()));
    assert_eq!(s.mock_targets().total_received(), 1);
    assert!(last_decision(&s).contains("duplicate"));
}

#[test]
fn mappings_are_applied_and_unmapped_buttons_rejected() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::StartRouting);
    let window = windows_of(&s, "PowerPoint")[0];

    click(&mut s, "aux");
    assert_eq!(s.mock_targets().total_received(), 0);
    assert!(last_decision(&s).contains("no action is mapped"));

    s.execute(Command::SetMapping {
        button: ButtonId::standard("aux"),
        action: Some(Action::custom("toggle notes").unwrap()),
    });
    s.execute(Command::SetMapping {
        button: ButtonId::standard("forward"),
        action: Some(Action::BlankScreen),
    });
    click(&mut s, "aux");
    click(&mut s, "forward");
    assert_eq!(
        s.mock_targets().received(window),
        &[Action::Custom("toggle notes".into()), Action::BlankScreen]
    );
}

#[test]
fn multiple_mock_devices_route_independently() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::AddMockDevice(MockProfile::TwoButton));
    let second = DeviceId::new("mock-2");
    assert_eq!(s.devices().len(), 2);
    s.execute(Command::StartRouting);

    click(&mut s, "forward");
    send(&mut s, &second, "back", EventKind::Click);
    assert_eq!(s.mock_targets().total_received(), 2);

    // A button the two-button profile does not have is refused at the source.
    send(&mut s, &second, "top", EventKind::Click);
    assert_eq!(s.mock_targets().total_received(), 2);

    s.execute(Command::SetDeviceConnected(second.clone(), false));
    send(&mut s, &second, "forward", EventKind::Click);
    click(&mut s, "forward");
    assert_eq!(s.mock_targets().total_received(), 3);
}

#[test]
fn event_queue_is_bounded() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::StartRouting);
    // Queue far more events than the queue holds without processing them.
    for _ in 0..rustclick::app::QUEUE_CAPACITY + 50 {
        s.handle(Msg::Command(Command::EmitMockEvent {
            device: remote(),
            button: "forward".into(),
            kind: EventKind::Click,
        }));
    }
    s.pump();
    let view = s.view();
    assert!(view.dropped_events > 0);
    assert!(s.mock_targets().total_received() <= rustclick::app::QUEUE_CAPACITY);
    assert!(view.warnings.iter().any(|w| w.contains("dropped")));
}

fn native_world() -> TargetSnapshot {
    let process = ProcessKey {
        pid: 1,
        instance: 1,
    };
    TargetSnapshot {
        origin: TargetOrigin::Native,
        processes: vec![ProcessInfo {
            key: process,
            identity: AppIdentity {
                name: "Microsoft PowerPoint".into(),
                process_name: "POWERPNT.EXE".into(),
                executable: Some(r"C:\Office\POWERPNT.EXE".into()),
                bundle_id: None,
            },
        }],
        windows: vec![WindowInfo {
            id: WindowId(1),
            process,
            title: "Deck.pptx".into(),
            visible: true,
        }],
    }
}

#[test]
fn simulated_events_never_reach_native_applications() {
    let snapshot = native_world();
    let mut devices = DeviceRegistry::default();
    devices.upsert(
        DeviceInfo {
            id: DeviceId::new("sim"),
            name: "Sim".into(),
            kind: DeviceKind::Simulated,
            connected: true,
            buttons: vec![ButtonId::standard("forward")],
            details: String::new(),
        },
        None,
    );
    let mut selection = TargetSelection::new(None);
    selection
        .select_window(&snapshot, WindowId(1), SelectionMode::Window)
        .unwrap();
    let event = InputEvent {
        device_id: DeviceId::new("sim"),
        device_name: "Sim".into(),
        sequence: 1,
        button: ButtonId::standard("forward"),
        kind: EventKind::Click,
        origin: EventOrigin::Simulated,
        timestamp: SystemTime::now(),
        captured_at: Instant::now(),
        metadata: vec![],
    };
    let decision = Router::default().route(
        &event,
        &RouteContext {
            routing_enabled: true,
            devices: &devices,
            mappings: &Mappings::default(),
            selection: &selection,
            snapshot: &snapshot,
            now: Instant::now(),
        },
    );
    assert_eq!(decision, Decision::Reject(RejectReason::SimulatedToNative));
}

#[test]
fn native_delivery_never_claims_success() {
    let target = RouteTarget {
        window: WindowId(1),
        process: ProcessKey {
            pid: 1,
            instance: 1,
        },
        app_name: "PowerPoint".into(),
        title: "Deck".into(),
        origin: TargetOrigin::Native,
        generation: 0,
    };
    let outcome = NativeTargetProvider.deliver(&target, &Action::Next);
    assert!(matches!(outcome, DeliveryOutcome::Unsupported(_)));
}

#[test]
fn native_mode_disables_simulation_controls() {
    let config = Config {
        input_mode: InputMode::Native,
        ..Config::default()
    };
    let mut s = service_with_config(config, &[MockApp::PowerPoint]);
    click(&mut s, "forward");
    s.execute(Command::MockLaunch(MockApp::Acrobat));
    s.execute(Command::StartRouting);

    let view = s.view();
    assert!(
        view.devices.is_empty(),
        "mock devices are not active in native mode"
    );
    assert!(
        view.windows.is_empty(),
        "mock windows are not offered as native targets"
    );
    assert!(view.can_start.is_err());
    assert_eq!(s.mock_targets().total_received(), 0);
}
