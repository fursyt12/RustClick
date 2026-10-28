//! End-to-end check of the Slint UI bindings using Slint's headless testing
//! backend: UI callbacks reach the service, and view models reach the UI.
//! Kept to a single test because the backend is process-global.

mod common;

use common::*;
use rustclick::app::RoutingState;
use rustclick::controller;
use rustclick::targets::mock::MockApp;
use rustclick::ui::{AppState, AppWindow};
use slint::{ComponentHandle, Model};

fn rows<T: Clone>(model: slint::ModelRc<T>) -> Vec<T> {
    (0..model.row_count())
        .filter_map(|i| model.row_data(i))
        .collect()
}

#[test]
fn ui_callbacks_drive_the_service_and_views_update_the_ui() {
    i_slint_backend_testing::init_no_event_loop();
    let mut service = service_with(&[MockApp::PowerPoint, MockApp::Acrobat]);
    let ui = AppWindow::new().unwrap();
    controller::bind(&ui, service.sender());
    controller::apply(&ui, service.view());
    let state = ui.global::<AppState>();

    assert!(state.get_simulation());
    assert_eq!(state.get_devices().row_count(), 1);
    assert!(!state.get_can_start());
    assert!(
        state.get_prompt_visible(),
        "two eligible targets must trigger the dialog"
    );
    let acrobat = rows(state.get_candidates())
        .into_iter()
        .find(|c| c.app_name.contains("Acrobat"))
        .unwrap();

    state.invoke_confirm_candidate(acrobat.key);
    service.pump();
    controller::apply(&ui, service.view());
    assert!(!state.get_prompt_visible());
    assert!(state.get_can_start());

    state.invoke_start_routing();
    state.invoke_emit_event("mock-1".into(), "forward".into(), 0);
    service.pump();
    assert_eq!(service.routing_state(), RoutingState::Active);
    controller::apply(&ui, service.view());
    assert!(
        state.get_last_result().contains("Adobe Acrobat"),
        "{}",
        state.get_last_result()
    );
    let target_row = rows(state.get_windows())
        .into_iter()
        .find(|w| w.selected)
        .unwrap();
    assert!(
        target_row.received.contains("Next"),
        "{}",
        target_row.received
    );

    // The log filter is applied locally by the controller.
    state.invoke_set_log_filter(1);
    let log = rows(state.get_log());
    assert!(!log.is_empty());
    assert!(log.iter().all(|r| r.category == "Input"));

    // Mapping edits round-trip through the service.
    state.invoke_set_mapping(
        "aux".into(),
        state.get_custom_action_index(),
        "notes".into(),
    );
    service.pump();
    controller::apply(&ui, service.view());
    let aux = rows(state.get_mappings())
        .into_iter()
        .find(|m| m.button == "aux")
        .unwrap();
    assert_eq!(aux.action_index, state.get_custom_action_index());
    assert_eq!(aux.custom, "notes");

    // A UI-side value the service rejects snaps back on the next update.
    // (Native mode exists on Windows, so it cannot serve as the example there.)
    #[cfg(not(target_os = "windows"))]
    {
        state.set_input_mode(1);
        state.invoke_set_input_mode(1);
        service.pump();
        controller::apply(&ui, service.view());
        assert_eq!(state.get_input_mode(), 0);
        assert!(!state.get_native_available());
    }
    #[cfg(target_os = "windows")]
    assert!(state.get_native_available());
}
