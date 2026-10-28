//! Glue between the Slint UI and the application service.
//!
//! [`bind`] turns UI callbacks into [`Command`]s; [`apply`] copies a
//! [`ViewModel`] into the UI's properties. Both run on the UI thread. No
//! routing or target logic lives here.

use std::cell::RefCell;
use std::time::SystemTime;

use chrono::{DateTime, Local};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::app::{
    Command, CommandSender, LogCategory, LogEntry, RoutingState, Severity, TargetState, ViewModel,
};
use crate::core::capabilities::CapabilityStatus;
use crate::core::config::{InputMode, LogLevel};
use crate::core::input::{DeviceId, EventKind, normalize_button};
use crate::core::mappings::{Action, BUILTIN_ACTIONS};
use crate::core::targets::{
    MultipleTargetsPolicy, ProcessKey, SelectionMode, TargetLostPolicy, WindowId,
};
use crate::input::mock::MockProfile;
use crate::targets::mock::MockApp;
use crate::ui::{
    AppState, AppWindow, CandidateRow, CapabilityRow, DeviceRow, LogRow, MappingRow, ProcessRow,
    WindowRow,
};

/// Index of "Custom…" in the action list: "(unmapped)", built-ins, custom.
const CUSTOM_ACTION_INDEX: usize = BUILTIN_ACTIONS.len() + 1;

#[derive(Default)]
struct UiCache {
    last: Option<ViewModel>,
    log_filter: i32,
}

thread_local! {
    static CACHE: RefCell<UiCache> = RefCell::default();
}

fn window_key(id: WindowId) -> SharedString {
    id.0.to_string().into()
}

fn parse_window(key: &str) -> Option<WindowId> {
    key.parse().ok().map(WindowId)
}

fn process_key(key: ProcessKey) -> SharedString {
    format!("{}:{}", key.pid, key.instance).into()
}

fn parse_process(key: &str) -> Option<ProcessKey> {
    let (pid, instance) = key.split_once(':')?;
    Some(ProcessKey {
        pid: pid.parse().ok()?,
        instance: instance.parse().ok()?,
    })
}

fn severity_level(s: Severity) -> i32 {
    match s {
        Severity::Info => 0,
        Severity::Success => 1,
        Severity::Warning => 2,
        Severity::Error => 3,
    }
}

fn clock(t: SystemTime, millis: bool) -> SharedString {
    let t: DateTime<Local> = t.into();
    let format = if millis { "%H:%M:%S%.3f" } else { "%H:%M:%S" };
    t.format(format).to_string().into()
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn action_index(action: Option<&Action>) -> i32 {
    match action {
        None => 0,
        Some(Action::Custom(_)) => CUSTOM_ACTION_INDEX as i32,
        Some(a) => BUILTIN_ACTIONS
            .iter()
            .position(|b| b == a)
            .map_or(0, |i| i as i32 + 1),
    }
}

/// Wires every UI callback to a service command.
pub fn bind(ui: &AppWindow, sender: CommandSender) {
    let state = ui.global::<AppState>();
    let send = move |cmd: Command| {
        if let Err(e) = sender.send(cmd) {
            tracing::warn!("UI command dropped: {e}");
        }
    };
    // Registers a callback that sends the command its body evaluates to, if
    // any. Each callback gets its own clone of `send`.
    macro_rules! on {
        ($setter:ident, || $body:expr) => {
            on!($setter, | | $body)
        };
        ($setter:ident, |$($arg:ident),*| $body:expr) => {{
            let send = send.clone();
            state.$setter(move |$($arg),*| {
                if let Some(cmd) = $body {
                    send(cmd);
                }
            });
        }};
    }

    on!(on_start_routing, || Some(Command::StartRouting));
    on!(on_stop_routing, || Some(Command::StopRouting));
    on!(on_request_target_change, || Some(
        Command::RequestTargetChange
    ));
    on!(on_clear_target, || Some(Command::ClearTarget));

    on!(on_add_mock_device, |profile| MockProfile::ALL
        .get(profile as usize)
        .map(|p| Command::AddMockDevice(*p)));
    on!(on_remove_device, |id| Some(Command::RemoveDevice(
        DeviceId::new(id.as_str())
    )));
    on!(on_rename_device, |id, name| Some(Command::RenameDevice(
        DeviceId::new(id.as_str()),
        name.to_string()
    )));
    on!(on_set_device_enabled, |id, enabled| Some(
        Command::SetDeviceEnabled(DeviceId::new(id.as_str()), enabled)
    ));
    on!(on_set_device_connected, |id, connected| Some(
        Command::SetDeviceConnected(DeviceId::new(id.as_str()), connected)
    ));
    on!(on_emit_event, |id, button, kind| Some(
        Command::EmitMockEvent {
            device: DeviceId::new(id.as_str()),
            button: button.to_string(),
            kind: match kind {
                1 => EventKind::Press,
                2 => EventKind::Release,
                _ => EventKind::Click,
            },
        }
    ));
    on!(on_replay_last_event, |id| Some(Command::ReplayLastEvent(
        DeviceId::new(id.as_str())
    )));
    on!(on_clear_log, || Some(Command::ClearLog));

    on!(on_refresh_targets, || Some(Command::RefreshTargets));
    on!(on_select_window, |key| parse_window(&key)
        .map(Command::SelectWindow));
    on!(on_select_process, |key| parse_process(&key)
        .map(Command::SelectProcess));
    on!(on_set_selection_mode, |mode| Some(
        Command::SetSelectionMode(if mode == 1 {
            SelectionMode::Process
        } else {
            SelectionMode::Window
        })
    ));
    on!(on_confirm_candidate, |key| parse_window(&key)
        .map(Command::ConfirmTarget));
    on!(on_dismiss_prompt, |restore_previous, stop_routing| Some(
        Command::DismissPrompt {
            restore_previous,
            stop_routing,
        }
    ));
    on!(on_mock_launch, |app| MockApp::ALL
        .get(app as usize)
        .map(|a| Command::MockLaunch(*a)));
    on!(on_mock_open_window, |key| parse_process(&key)
        .map(Command::MockOpenWindow));
    on!(on_mock_close_window, |key| parse_window(&key)
        .map(Command::MockCloseWindow));
    on!(on_mock_kill_process, |key| parse_process(&key)
        .map(Command::MockKillProcess));
    on!(on_mock_restart_process, |key| parse_process(&key)
        .map(Command::MockRestartProcess));
    on!(on_set_mock_notifications, |enabled| Some(
        Command::MockSetNotifications(enabled)
    ));

    on!(on_set_input_mode, |mode| Some(Command::SetInputMode(
        if mode == 1 {
            InputMode::Native
        } else {
            InputMode::Simulation
        }
    )));
    on!(on_set_auto_select_single, |enabled| Some(
        Command::SetAutoSelectSingle(enabled)
    ));
    on!(on_set_target_lost_policy, |policy| Some(
        Command::SetTargetLostPolicy(if policy == 1 {
            TargetLostPolicy::Ask
        } else {
            TargetLostPolicy::AutoRebind
        })
    ));
    on!(on_set_multiple_policy, |policy| Some(
        Command::SetMultipleTargetsPolicy(if policy == 1 {
            MultipleTargetsPolicy::Ask
        } else {
            MultipleTargetsPolicy::KeepCurrent
        })
    ));
    on!(on_set_shortcut, |shortcut| Some(Command::SetShortcut(
        shortcut.to_string()
    )));
    on!(on_set_log_level, |level| LogLevel::ALL
        .get(level as usize)
        .map(|l| Command::SetLogLevel(*l)));
    on!(on_set_mapping, |button, index, custom| {
        let action = match index as usize {
            0 => Ok(None),
            i if i == CUSTOM_ACTION_INDEX => Action::custom(&custom).map(Some),
            i => Ok(BUILTIN_ACTIONS.get(i - 1).cloned()),
        };
        match (normalize_button(&button), action) {
            (Some(button), Ok(action)) => Some(Command::SetMapping { button, action }),
            (_, Err(e)) => {
                tracing::warn!("invalid custom action: {e}");
                None
            }
            (None, _) => None,
        }
    });
    on!(on_reset_config, || Some(Command::ResetConfig));

    // Opening the log folder is a local desktop action, not a service command.
    let weak = ui.as_weak();
    state.on_open_log_folder(move || {
        let Some(ui) = weak.upgrade() else { return };
        let file = ui.global::<AppState>().get_log_file();
        if let Some(dir) = std::path::Path::new(file.as_str()).parent() {
            open_folder(dir);
        }
    });

    // The log filter is a pure view concern handled locally.
    let weak = ui.as_weak();
    state.on_set_log_filter(move |filter| {
        CACHE.with(|c| c.borrow_mut().log_filter = filter);
        if let Some(ui) = weak.upgrade() {
            CACHE.with(|c| {
                if let Some(view) = &c.borrow().last {
                    ui.global::<AppState>()
                        .set_log(log_model(&view.log, filter));
                }
            });
        }
    });

    let action_names: Vec<SharedString> = std::iter::once("(unmapped)".into())
        .chain(BUILTIN_ACTIONS.iter().map(|a| a.label().into()))
        .chain(std::iter::once("Custom…".into()))
        .collect();
    state.set_action_names(model(action_names));
    state.set_version(crate::version().into());
    state.set_custom_action_index(CUSTOM_ACTION_INDEX as i32);
}

fn open_folder(dir: &std::path::Path) {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(program).arg(dir).spawn() {
        tracing::warn!("could not open {}: {e}", dir.display());
    }
}

fn log_model(log: &[LogEntry], filter: i32) -> ModelRc<LogRow> {
    let keep = |e: &&LogEntry| match filter {
        1 => e.category == LogCategory::Input,
        2 => matches!(e.category, LogCategory::Decision | LogCategory::Delivery),
        3 => matches!(
            e.category,
            LogCategory::Target | LogCategory::Device | LogCategory::System
        ),
        _ => true,
    };
    model(
        log.iter()
            .filter(keep)
            .map(|e| LogRow {
                time: clock(e.time, true),
                category: e.category.label().into(),
                severity: severity_level(e.severity),
                device: e.device.as_str().into(),
                button: e.button.as_str().into(),
                message: e.message.as_str().into(),
            })
            .collect(),
    )
}

fn received_summary(actions: &[Action]) -> String {
    match actions.last() {
        None => String::new(),
        Some(last) => format!("received {} action(s), last: {last}", actions.len()),
    }
}

/// Copies a view model into the UI. Lists are only replaced when they changed.
pub fn apply(ui: &AppWindow, view: ViewModel) {
    let state = ui.global::<AppState>();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let prev = cache.last.take();
        let changed = |differs: &dyn Fn(&ViewModel) -> bool| prev.as_ref().is_none_or(differs);

        // Scalars are always written so that UI-side edits that the service
        // rejected snap back to the real value.
        let simulation = view.input_mode == InputMode::Simulation;
        state.set_simulation(simulation);
        state.set_mode_label(
            if simulation {
                "Simulation mode"
            } else {
                "Native mode"
            }
            .into(),
        );
        state.set_status_text(view.status.1.as_str().into());
        state.set_status_level(severity_level(view.status.0));
        let (routing_label, routing_level) = match &view.routing {
            RoutingState::Stopped => ("Stopped".to_owned(), 0),
            RoutingState::Active if simulation => ("Active (simulated)".to_owned(), 1),
            RoutingState::Active => ("Active".to_owned(), 1),
            RoutingState::Paused(r) => (format!("Paused: {r}"), 2),
        };
        state.set_routing_label(routing_label.into());
        state.set_routing_level(routing_level);
        state.set_routing_requested(view.routing_requested);
        state.set_can_start(view.can_start.is_ok());
        state.set_start_blocked_reason(view.can_start.clone().err().unwrap_or_default().into());
        state.set_target_summary(view.target.summary.as_str().into());
        state.set_target_detail(view.target.detail.as_str().into());
        state.set_target_level(match view.target.state {
            TargetState::None => 0,
            TargetState::Bound => 1,
            TargetState::Unavailable | TargetState::AwaitingConfirmation => 2,
        });
        let active = view
            .devices
            .iter()
            .filter(|d| d.enabled && d.connected)
            .count();
        state.set_devices_summary(
            format!(
                "{active} of {} device(s) enabled and connected",
                view.devices.len()
            )
            .into(),
        );
        state.set_last_event(view.last_event.clone().unwrap_or_default().into());
        let (result_level, result) = view
            .last_result
            .clone()
            .map_or((0, String::new()), |(s, r)| (severity_level(s), r));
        state.set_last_result(result.into());
        state.set_last_result_level(result_level);
        state.set_targets_error(view.targets_error.clone().unwrap_or_default().into());
        state.set_notifications_active(view.notifications_active);
        state.set_mock_notifications(view.mock_notifications);
        state.set_prompt_visible(view.prompt.is_some());
        state.set_input_mode(i32::from(!simulation));
        state.set_native_available(view.native_mode.is_ok());
        state.set_native_reason(view.native_mode.clone().err().unwrap_or_default().into());
        let prefs = &view.preferences;
        state.set_selection_mode(i32::from(prefs.selection_mode == SelectionMode::Process));
        state.set_auto_select_single(prefs.auto_select_single);
        state.set_target_lost_policy(i32::from(prefs.on_target_lost == TargetLostPolicy::Ask));
        state.set_multiple_policy(i32::from(
            prefs.on_multiple_targets == MultipleTargetsPolicy::Ask,
        ));
        state.set_shortcut(prefs.change_target_shortcut.as_str().into());
        state.set_shortcut_status(
            format!("{}: {}", view.shortcut.status, view.shortcut.detail).into(),
        );
        state.set_log_level(
            LogLevel::ALL
                .iter()
                .position(|l| *l == view.log_level)
                .unwrap_or(2) as i32,
        );
        state.set_config_path(
            view.config_path
                .clone()
                .unwrap_or_else(|| "Not saved (no configuration directory)".into())
                .into(),
        );
        state.set_environment(view.environment.as_str().into());
        state.set_log_file(view.log_file.clone().unwrap_or_default().into());

        if changed(&|p| p.warnings != view.warnings) {
            state.set_warnings(model(
                view.warnings.iter().map(|w| w.as_str().into()).collect(),
            ));
        }
        if changed(&|p| p.devices != view.devices) {
            let rows = view
                .devices
                .iter()
                .map(|d| DeviceRow {
                    id: d.id.as_str().into(),
                    name: d.name.as_str().into(),
                    simulated: d.simulated,
                    connected: d.connected,
                    enabled: d.enabled,
                    last_activity: d
                        .last_activity
                        .map_or_else(|| "never".into(), |t| clock(t, false)),
                    details: d.details.as_str().into(),
                    buttons: model(d.buttons.iter().map(|b| b.as_str().into()).collect()),
                })
                .collect();
            state.set_devices(model(rows));
            let count = view.devices.len() as i32;
            if state.get_selected_device() >= count {
                state.set_selected_device((count - 1).max(0));
            }
        }
        if changed(&|p| p.log != view.log) {
            state.set_log(log_model(&view.log, cache.log_filter));
        }
        if changed(&|p| p.processes != view.processes) {
            let rows = view
                .processes
                .iter()
                .map(|p| ProcessRow {
                    key: process_key(p.key),
                    app_name: p.app_name.as_str().into(),
                    process_name: p.process_name.as_str().into(),
                    executable: p.executable.as_str().into(),
                    pid: p.key.pid as i32,
                    window_count: p.window_count as i32,
                    eligible: p.eligible,
                    selected: p.selected,
                })
                .collect();
            state.set_processes(model(rows));
        }
        if changed(&|p| p.windows != view.windows) {
            // Presentation applications first; the sort is stable otherwise.
            let mut windows: Vec<_> = view.windows.iter().collect();
            windows.sort_by_key(|w| !w.eligible);
            let rows = windows
                .into_iter()
                .map(|w| WindowRow {
                    key: window_key(w.id),
                    process_key: process_key(w.process),
                    app_name: w.app_name.as_str().into(),
                    title: w.title.as_str().into(),
                    pid: w.process.pid as i32,
                    window_id: w.id.to_string().into(),
                    available: w.visible,
                    eligible: w.eligible,
                    selected: w.selected,
                    received: received_summary(&w.received).into(),
                })
                .collect();
            state.set_windows(model(rows));
        }
        if changed(&|p| p.prompt != view.prompt) {
            let prompt = view.prompt.as_ref();
            state.set_prompt_reason(prompt.map_or_else(String::new, |p| p.reason.clone()).into());
            let rows = prompt
                .map(|p| {
                    p.candidates
                        .iter()
                        .map(|c| CandidateRow {
                            key: window_key(c.window),
                            app_name: c.app_name.as_str().into(),
                            title: c.title.as_str().into(),
                            pid: c.pid as i32,
                        })
                        .collect()
                })
                .unwrap_or_default();
            state.set_candidates(model(rows));
        }
        if changed(&|p| p.mappings != view.mappings) {
            let rows = view
                .mappings
                .iter()
                .map(|m| MappingRow {
                    button: m.button.as_str().into(),
                    action_index: action_index(m.action.as_ref()),
                    custom: match &m.action {
                        Some(Action::Custom(name)) => name.as_str().into(),
                        _ => SharedString::default(),
                    },
                })
                .collect();
            state.set_mappings(model(rows));
        }
        if changed(&|p| p.capabilities != view.capabilities || p.shortcut != view.shortcut) {
            let level = |s: CapabilityStatus| match s {
                CapabilityStatus::Supported => 1,
                CapabilityStatus::Experimental => 0,
                CapabilityStatus::Error => 3,
                CapabilityStatus::NotImplemented
                | CapabilityStatus::Unsupported
                | CapabilityStatus::PermissionRequired
                | CapabilityStatus::TemporarilyUnavailable => 2,
            };
            let mut rows: Vec<CapabilityRow> = view
                .capabilities
                .iter()
                .flat_map(|section| {
                    section
                        .items
                        .iter()
                        .map(move |c| (section.title.as_str(), c))
                })
                .chain(std::iter::once(("Shortcuts", &view.shortcut)))
                .map(|(section, c)| CapabilityRow {
                    section: section.into(),
                    component: c.component.as_str().into(),
                    status: c.status.label().into(),
                    level: level(c.status),
                    detail: c.detail.as_str().into(),
                })
                .collect();
            rows.dedup_by(|a, b| a.section == b.section && a.component == b.component);
            state.set_capabilities(model(rows));
        }

        cache.last = Some(view);
    });
}
