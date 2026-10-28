//! Target selection, ambiguity and lifecycle through the full service.

mod common;

use common::*;
use rustclick::app::{Command, RoutingState, TargetState};
use rustclick::core::mappings::Action;
use rustclick::core::targets::{SelectionMode, TargetStatus};
use rustclick::targets::mock::MockApp;

#[test]
fn single_eligible_target_is_selected_by_policy() {
    let s = service_with(&[MockApp::PowerPoint, MockApp::TextEditor]);
    let view = s.view();
    assert_eq!(view.target.state, TargetState::Bound);
    assert!(
        view.target.summary.contains("PowerPoint"),
        "{}",
        view.target.summary
    );
    assert!(view.prompt.is_none());
    assert!(view.can_start.is_ok());
}

#[test]
fn ineligible_applications_are_never_auto_selected() {
    let s = service_with(&[MockApp::TextEditor]);
    assert_eq!(s.view().target.state, TargetState::None);
}

#[test]
fn multiple_eligible_targets_require_explicit_selection() {
    let mut s = service_with(&[MockApp::PowerPoint, MockApp::Acrobat]);
    let view = s.view();
    let prompt = view.prompt.expect("selection request");
    assert_eq!(prompt.candidates.len(), 2);
    assert!(view.can_start.is_err());

    // Routing cannot start and events are rejected until the user confirms.
    s.execute(Command::StartRouting);
    assert_eq!(s.routing_state(), RoutingState::Stopped);

    let acrobat = windows_of(&s, "Acrobat")[0];
    s.execute(Command::ConfirmTarget(acrobat));
    s.execute(Command::StartRouting);
    click(&mut s, "forward");
    assert_eq!(s.mock_targets().received(acrobat), &[Action::Next]);

    // The choice is remembered: another lifecycle change does not ask again.
    s.execute(Command::MockLaunch(MockApp::TextEditor));
    assert!(s.view().prompt.is_none());
}

#[test]
fn routing_pauses_while_the_target_is_ambiguous() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::StartRouting);
    assert_eq!(s.routing_state(), RoutingState::Active);

    s.execute(Command::MockLaunch(MockApp::Acrobat));
    s.execute(Command::RequestTargetChange);
    assert!(matches!(s.routing_state(), RoutingState::Paused(_)));
    click(&mut s, "forward");
    assert_eq!(s.mock_targets().total_received(), 0);
    assert!(last_decision(&s).contains("ambiguous"));

    // Cancelling restores the previous target and routing resumes.
    s.execute(Command::DismissPrompt {
        restore_previous: true,
        stop_routing: false,
    });
    assert_eq!(s.routing_state(), RoutingState::Active);

    // "Pause routing" in the dialog stops routing.
    s.execute(Command::RequestTargetChange);
    s.execute(Command::DismissPrompt {
        restore_previous: true,
        stop_routing: true,
    });
    assert_eq!(s.routing_state(), RoutingState::Stopped);
}

#[test]
fn closing_the_window_never_redirects_to_another_application() {
    let mut s = service_with(&[MockApp::PowerPoint, MockApp::Acrobat]);
    let ppt = windows_of(&s, "PowerPoint")[0];
    let acrobat = windows_of(&s, "Acrobat")[0];
    s.execute(Command::ConfirmTarget(ppt));
    s.execute(Command::StartRouting);

    s.execute(Command::MockCloseWindow(ppt));
    assert!(matches!(s.routing_state(), RoutingState::Paused(_)));
    assert!(matches!(
        s.selection().status(),
        TargetStatus::Unavailable(_)
    ));

    click(&mut s, "forward");
    assert_eq!(s.mock_targets().received(acrobat), &[] as &[Action]);
    assert_eq!(s.mock_targets().total_received(), 0);
}

#[test]
fn replacement_window_is_detected_after_restart() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    let old_window = windows_of(&s, "PowerPoint")[0];
    let old_process = processes_of(&s, "PowerPoint")[0];
    s.execute(Command::StartRouting);

    s.execute(Command::MockRestartProcess(old_process));
    let new_window = windows_of(&s, "PowerPoint")[0];
    assert_ne!(new_window, old_window);
    assert_eq!(
        s.routing_state(),
        RoutingState::Active,
        "unambiguous replacement resumes routing"
    );

    click(&mut s, "forward");
    assert_eq!(s.mock_targets().received(new_window), &[Action::Next]);
}

#[test]
fn ambiguous_replacement_asks_the_user() {
    let mut s = service_with(&[
        MockApp::PowerPoint,
        MockApp::PowerPoint,
        MockApp::PowerPoint,
    ]);
    let windows = windows_of(&s, "PowerPoint");
    s.execute(Command::ConfirmTarget(windows[0]));
    s.execute(Command::StartRouting);

    // Two other instances show a window with the same title.
    s.execute(Command::MockKillProcess(processes_of(&s, "PowerPoint")[0]));
    let prompt = s.view().prompt.expect("confirmation");
    assert_eq!(prompt.candidates.len(), 2);
    assert!(matches!(s.routing_state(), RoutingState::Paused(_)));

    s.execute(Command::ConfirmTarget(windows[2]));
    assert_eq!(s.routing_state(), RoutingState::Active);
}

#[test]
fn restarted_process_is_matched_by_stable_identity() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::SetSelectionMode(SelectionMode::Process));
    let process = processes_of(&s, "PowerPoint")[0];
    s.execute(Command::SelectProcess(process));
    s.execute(Command::StartRouting);

    s.execute(Command::MockKillProcess(process));
    match s.selection().status() {
        TargetStatus::Unavailable(reason) => assert!(reason.contains("not running"), "{reason}"),
        other => panic!("unexpected {other:?}"),
    }

    // A different application starting does not satisfy the criteria.
    s.execute(Command::MockLaunch(MockApp::Acrobat));
    assert!(matches!(
        s.selection().status(),
        TargetStatus::Unavailable(_)
    ));

    s.execute(Command::MockLaunch(MockApp::PowerPoint));
    let restarted = processes_of(&s, "PowerPoint")[0];
    assert_ne!(restarted.pid, process.pid);
    assert_eq!(s.selection().binding().map(|b| b.process), Some(restarted));
    assert_eq!(s.routing_state(), RoutingState::Active);
}

#[test]
fn process_mode_falls_back_to_another_window_of_the_process() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::SetSelectionMode(SelectionMode::Process));
    let process = processes_of(&s, "PowerPoint")[0];
    s.execute(Command::MockOpenWindow(process));
    let windows = windows_of(&s, "PowerPoint");

    // Selecting a process with two windows asks which one to use.
    s.execute(Command::SelectProcess(process));
    assert_eq!(s.view().prompt.expect("window choice").candidates.len(), 2);
    s.execute(Command::ConfirmTarget(windows[0]));

    s.execute(Command::MockCloseWindow(windows[0]));
    assert_eq!(s.selection().binding().map(|b| b.window), Some(windows[1]));
}

#[test]
fn delivery_revalidates_when_the_snapshot_is_stale() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::StartRouting);
    s.execute(Command::MockSetNotifications(false));
    let window = windows_of(&s, "PowerPoint")[0];

    // Without notifications the service does not know the window closed.
    s.execute(Command::MockCloseWindow(window));
    assert_eq!(s.routing_state(), RoutingState::Active);

    // The routing decision succeeds on stale data, but the provider
    // re-validates before delivery and the service resynchronizes.
    click(&mut s, "forward");
    assert_eq!(s.mock_targets().total_received(), 0);
    let view = s.view();
    assert!(
        view.log
            .iter()
            .any(|e| e.message.contains("delivery failed"))
    );
    assert!(matches!(s.routing_state(), RoutingState::Paused(_)));

    // A manual refresh also picks up changes.
    s.execute(Command::MockLaunch(MockApp::PowerPoint));
    assert!(matches!(s.routing_state(), RoutingState::Paused(_)));
    s.execute(Command::RefreshTargets);
    assert_eq!(s.routing_state(), RoutingState::Active);
}

#[test]
fn clearing_the_target_stops_automatic_selection() {
    let mut s = service_with(&[MockApp::PowerPoint]);
    s.execute(Command::ClearTarget);
    s.execute(Command::RefreshTargets);
    assert_eq!(s.view().target.state, TargetState::None);
}
