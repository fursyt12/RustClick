//! Application service.
//!
//! [`AppService`] owns the core state and all providers. It runs on a single
//! worker thread, processing [`Command`]s from the UI and messages from
//! providers in arrival order through one bounded queue, and publishes a
//! UI-independent [`ViewModel`] after each batch. Because everything that
//! touches the routing state is serialized on this thread, a target change
//! can never interleave with the routing of an event.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Instant, SystemTime};

use crate::core::capabilities::{Capability, CapabilityStatus};
use crate::core::config::{
    Config, DeviceSettings, InputMode, LogLevel, Preferences, validate_shortcut,
};
use crate::core::devices::{DeviceInfo, DeviceKind, DeviceRegistry};
use crate::core::input::{ButtonId, DeviceId, EventKind, InputEvent, STANDARD_BUTTONS};
use crate::core::mappings::Action;
use crate::core::routing::{Decision, DeliveryOutcome, RouteContext, Router};
use crate::core::targets::{
    MultipleTargetsPolicy, ProcessKey, SelectionMode, TargetLostPolicy, TargetOrigin,
    TargetSelection, TargetSnapshot, TargetStatus, WindowId,
};
use crate::input::InputProvider;
use crate::input::mock::{MockInputProvider, MockProfile};
use crate::input::native::NativeInputProvider;
use crate::platform;
use crate::provider::{ProviderMessage, ProviderSink, SinkError};
use crate::storage::ConfigStore;
use crate::targets::TargetProvider;
use crate::targets::mock::{MockApp, MockTargetProvider};
use crate::targets::native::NativeTargetProvider;

/// Capacity of the service queue shared by UI commands and provider messages.
pub const QUEUE_CAPACITY: usize = 256;
/// Number of entries kept in the event log.
pub const LOG_CAPACITY: usize = 500;

/// Requests from the UI (or tests) to the service.
#[derive(Debug, Clone)]
pub enum Command {
    StartRouting,
    StopRouting,
    AddMockDevice(MockProfile),
    RemoveDevice(DeviceId),
    RenameDevice(DeviceId, String),
    SetDeviceEnabled(DeviceId, bool),
    SetDeviceConnected(DeviceId, bool),
    EmitMockEvent {
        device: DeviceId,
        button: String,
        kind: EventKind,
    },
    ReplayLastEvent(DeviceId),
    RefreshTargets,
    /// Select a window using the preferred selection mode.
    SelectWindow(WindowId),
    SelectProcess(ProcessKey),
    ConfirmTarget(WindowId),
    DismissPrompt {
        restore_previous: bool,
        stop_routing: bool,
    },
    RequestTargetChange,
    ClearTarget,
    MockLaunch(MockApp),
    MockOpenWindow(ProcessKey),
    MockCloseWindow(WindowId),
    MockKillProcess(ProcessKey),
    MockRestartProcess(ProcessKey),
    MockSetNotifications(bool),
    SetInputMode(InputMode),
    SetSelectionMode(SelectionMode),
    SetAutoSelectSingle(bool),
    SetTargetLostPolicy(TargetLostPolicy),
    SetMultipleTargetsPolicy(MultipleTargetsPolicy),
    SetShortcut(String),
    SetLogLevel(LogLevel),
    SetMapping {
        button: ButtonId,
        action: Option<Action>,
    },
    ResetConfig,
    ClearLog,
}

#[derive(Debug)]
pub enum Msg {
    Command(Command),
    Provider(ProviderMessage),
    Shutdown,
}

/// Cloneable handle for sending commands to the service.
#[derive(Clone)]
pub struct CommandSender(SyncSender<Msg>);

impl CommandSender {
    /// Non-blocking; safe to call from the UI thread.
    pub fn send(&self, cmd: Command) -> Result<(), SinkError> {
        self.0.try_send(Msg::Command(cmd)).map_err(|e| match e {
            TrySendError::Full(_) => SinkError::Full,
            TrySendError::Disconnected(_) => SinkError::Closed,
        })
    }

    /// Asks the service to stop. Blocks until the request is queued.
    pub fn shutdown(&self) {
        let _ = self.0.send(Msg::Shutdown);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    Input,
    Decision,
    Delivery,
    Target,
    Device,
    System,
}

impl LogCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Input => "Input",
            Self::Decision => "Routing",
            Self::Delivery => "Delivery",
            Self::Target => "Target",
            Self::Device => "Device",
            Self::System => "System",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub time: SystemTime,
    pub category: LogCategory,
    pub severity: Severity,
    pub device: String,
    pub button: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingState {
    Stopped,
    Active,
    Paused(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub simulated: bool,
    pub connected: bool,
    pub enabled: bool,
    pub last_activity: Option<SystemTime>,
    pub buttons: Vec<String>,
    pub details: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    None,
    Bound,
    Unavailable,
    AwaitingConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetView {
    pub state: TargetState,
    pub summary: String,
    pub detail: String,
    pub mode: Option<SelectionMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateView {
    pub window: WindowId,
    pub app_name: String,
    pub title: String,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptView {
    pub reason: String,
    pub candidates: Vec<CandidateView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessView {
    pub key: ProcessKey,
    pub app_name: String,
    pub process_name: String,
    pub executable: String,
    pub window_count: usize,
    pub eligible: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowView {
    pub id: WindowId,
    pub process: ProcessKey,
    pub app_name: String,
    pub process_name: String,
    pub title: String,
    pub visible: bool,
    pub eligible: bool,
    pub selected: bool,
    /// Actions recorded by a mock window (simulation only).
    pub received: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingView {
    pub button: ButtonId,
    pub action: Option<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySection {
    pub title: String,
    pub items: Vec<Capability>,
}

/// Snapshot of everything the UI displays. Plain data, no UI types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewModel {
    pub input_mode: InputMode,
    /// `Err(reason)` when native mode cannot be used on this platform.
    pub native_mode: Result<(), String>,
    pub status: (Severity, String),
    pub routing: RoutingState,
    pub routing_requested: bool,
    /// `Err(reason)` when Start Routing must be disabled.
    pub can_start: Result<(), String>,
    pub devices: Vec<DeviceView>,
    pub target: TargetView,
    pub prompt: Option<PromptView>,
    pub processes: Vec<ProcessView>,
    pub windows: Vec<WindowView>,
    pub targets_error: Option<String>,
    pub notifications_active: bool,
    pub mock_notifications: bool,
    pub last_event: Option<String>,
    pub last_result: Option<(Severity, String)>,
    pub warnings: Vec<String>,
    /// Newest first.
    pub log: Vec<LogEntry>,
    pub mappings: Vec<MappingView>,
    pub preferences: Preferences,
    pub log_level: LogLevel,
    pub capabilities: Vec<CapabilitySection>,
    pub shortcut: Capability,
    pub config_path: Option<String>,
    pub log_file: Option<String>,
    pub environment: String,
    pub dropped_events: u64,
}

pub struct AppService {
    config: Config,
    store: Option<ConfigStore>,
    devices: DeviceRegistry,
    selection: TargetSelection,
    router: Router,
    mock_input: MockInputProvider,
    native_input: NativeInputProvider,
    mock_targets: MockTargetProvider,
    native_targets: NativeTargetProvider,
    snapshot: TargetSnapshot,
    input_error: Option<String>,
    targets_error: Option<String>,
    routing_requested: bool,
    log: VecDeque<LogEntry>,
    startup_warnings: Vec<String>,
    last_event: Option<String>,
    last_result: Option<(Severity, String)>,
    dropped_events: Arc<AtomicU64>,
    tx: SyncSender<Msg>,
    rx: Receiver<Msg>,
    on_log_level: Option<Box<dyn Fn(LogLevel) + Send>>,
    log_file: Option<String>,
}

impl AppService {
    /// Creates the service with the demo mock scene. `store` is `None` for
    /// in-memory operation (tests).
    pub fn new(config: Config, store: Option<ConfigStore>, warnings: Vec<String>) -> Self {
        Self::with_mock_targets(
            config,
            store,
            warnings,
            MockTargetProvider::with_demo_scene(),
        )
    }

    pub fn with_mock_targets(
        config: Config,
        store: Option<ConfigStore>,
        warnings: Vec<String>,
        mock_targets: MockTargetProvider,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let mut service = Self {
            selection: TargetSelection::new(config.saved_target.clone()),
            config,
            store,
            devices: DeviceRegistry::default(),
            router: Router::default(),
            mock_input: MockInputProvider::new(),
            native_input: NativeInputProvider::default(),
            mock_targets,
            native_targets: NativeTargetProvider::default(),
            snapshot: TargetSnapshot::empty(TargetOrigin::Simulated),
            input_error: None,
            targets_error: None,
            routing_requested: false,
            log: VecDeque::with_capacity(LOG_CAPACITY),
            startup_warnings: warnings,
            last_event: None,
            last_result: None,
            dropped_events: Arc::new(AtomicU64::new(0)),
            tx,
            rx,
            on_log_level: None,
            log_file: None,
        };
        for w in service.startup_warnings.clone() {
            service.push_log(LogCategory::System, Severity::Warning, w);
        }
        service.restore_mock_devices();
        service.start_providers();
        service.pump();
        service.push_log(
            LogCategory::System,
            Severity::Info,
            format!(
                "RustClick started in {} mode",
                service.config.input_mode.label()
            ),
        );
        service
    }

    /// Called whenever the configured log level changes.
    pub fn set_log_level_hook(&mut self, hook: Box<dyn Fn(LogLevel) + Send>) {
        self.on_log_level = Some(hook);
    }

    /// Where the log file is written, for display.
    pub fn set_log_file(&mut self, path: Option<String>) {
        self.log_file = path;
    }

    pub fn sender(&self) -> CommandSender {
        CommandSender(self.tx.clone())
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn selection(&self) -> &TargetSelection {
        &self.selection
    }

    pub fn snapshot(&self) -> &TargetSnapshot {
        &self.snapshot
    }

    pub fn devices(&self) -> &DeviceRegistry {
        &self.devices
    }

    pub fn mock_targets(&self) -> &MockTargetProvider {
        &self.mock_targets
    }

    /// Runs the service loop until [`CommandSender::shutdown`], publishing a
    /// view model initially and after each processed batch of messages.
    pub fn run(mut self, mut publish: impl FnMut(ViewModel)) {
        publish(self.view());
        while let Ok(msg) = self.rx.recv() {
            if !self.handle(msg) || !self.pump() {
                break;
            }
            publish(self.view());
        }
    }

    /// Processes all queued messages. Returns false after a shutdown request.
    pub fn pump(&mut self) -> bool {
        while let Ok(msg) = self.rx.try_recv() {
            if !self.handle(msg) {
                return false;
            }
        }
        true
    }

    /// Executes a command and everything it triggers (synchronous; for tests).
    pub fn execute(&mut self, cmd: Command) {
        self.handle(Msg::Command(cmd));
        self.pump();
    }

    /// Processes one message. Returns false on shutdown.
    pub fn handle(&mut self, msg: Msg) -> bool {
        let before = self.routing_state();
        match msg {
            Msg::Command(cmd) => {
                if let Err(e) = self.handle_command(cmd) {
                    self.push_log(LogCategory::System, Severity::Warning, e);
                }
            }
            Msg::Provider(m) => self.handle_provider(m),
            Msg::Shutdown => {
                self.shutdown();
                return false;
            }
        }
        self.sync_saved_target();
        let after = self.routing_state();
        if after != before {
            let (severity, text) = match &after {
                RoutingState::Active if before == RoutingState::Stopped => {
                    (Severity::Success, "Routing started".to_owned())
                }
                RoutingState::Active => (Severity::Success, "Routing resumed".to_owned()),
                RoutingState::Paused(r) => (Severity::Warning, format!("Routing paused: {r}")),
                RoutingState::Stopped => (Severity::Info, "Routing stopped".to_owned()),
            };
            self.push_log(LogCategory::System, severity, text);
        }
        true
    }

    fn shutdown(&mut self) {
        self.mock_input.stop();
        self.mock_targets.stop();
        self.native_input.stop();
        self.native_targets.stop();
        self.save_config();
        tracing::info!("service stopped");
    }

    // ----- providers -------------------------------------------------------

    fn make_sink(&self) -> ProviderSink {
        let tx = self.tx.clone();
        let dropped = self.dropped_events.clone();
        ProviderSink::new(move |m| match tx.try_send(Msg::Provider(m)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
                Err(SinkError::Full)
            }
            Err(TrySendError::Disconnected(_)) => Err(SinkError::Closed),
        })
    }

    fn restore_mock_devices(&mut self) {
        let mut problems = Vec::new();
        for d in self.config.devices.clone() {
            let Some(key) = &d.mock_profile else { continue };
            let Some(profile) = MockProfile::from_key(key) else {
                problems.push(format!(
                    "Unknown mock profile `{key}` for device `{}`",
                    d.id
                ));
                continue;
            };
            if let Err(e) =
                self.mock_input
                    .add_device(Some(d.id.clone()), d.name.as_deref(), profile)
            {
                problems.push(format!("Could not restore mock device `{}`: {e}", d.id));
            }
        }
        for p in problems {
            self.push_log(LogCategory::Device, Severity::Warning, p);
        }
    }

    fn start_providers(&mut self) {
        let sink = self.make_sink();
        self.input_error = None;
        let input = match self.config.input_mode {
            InputMode::Simulation => {
                self.snapshot = TargetSnapshot::empty(TargetOrigin::Simulated);
                let _ = self.mock_targets.start(sink.clone());
                self.mock_input.start(sink)
            }
            InputMode::Native => {
                self.snapshot = TargetSnapshot::empty(TargetOrigin::Native);
                let _ = self.native_targets.start(sink.clone());
                self.native_input.start(sink)
            }
        };
        if let Err(e) = input {
            self.push_log(LogCategory::System, Severity::Error, e.to_string());
            self.input_error = Some(e.to_string());
        }
        self.refresh_targets();
    }

    fn stop_providers(&mut self) {
        match self.config.input_mode {
            InputMode::Simulation => {
                self.mock_input.stop();
                self.mock_targets.stop();
            }
            InputMode::Native => {
                self.native_input.stop();
                self.native_targets.stop();
            }
        }
    }

    fn active_targets(&mut self) -> &mut dyn TargetProvider {
        match self.config.input_mode {
            InputMode::Simulation => &mut self.mock_targets,
            InputMode::Native => &mut self.native_targets,
        }
    }

    fn input_running(&self) -> bool {
        match self.config.input_mode {
            InputMode::Simulation => self.mock_input.is_running(),
            InputMode::Native => self.native_input.is_running(),
        }
    }

    fn require_simulation(&self) -> Result<(), String> {
        match self.config.input_mode {
            InputMode::Simulation => Ok(()),
            InputMode::Native => Err("Mock controls are only available in Simulation mode".into()),
        }
    }

    /// Native mode is offered only if every required native capability is
    /// operational (supported or experimental).
    fn native_mode_availability() -> Result<(), String> {
        let caps: Vec<Capability> = platform::input_capabilities()
            .into_iter()
            .chain(platform::target_capabilities())
            .collect();
        if caps
            .iter()
            .filter(|c| !c.optional)
            .all(|c| c.status.is_operational())
        {
            Ok(())
        } else {
            Err(format!(
                "Native input and target adapters are not implemented on {} yet",
                platform::os_label()
            ))
        }
    }

    fn refresh_targets(&mut self) {
        let origin = self.active_targets().origin();
        match self.active_targets().snapshot() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.targets_error = None;
            }
            Err(e) => {
                let message = e.to_string();
                if self.targets_error.as_ref() != Some(&message) {
                    self.push_log(LogCategory::Target, Severity::Error, message.clone());
                }
                self.snapshot = TargetSnapshot::empty(origin);
                self.targets_error = Some(message);
            }
        }
        let policy = self.config.preferences.target_policy();
        if let Some(note) =
            self.selection
                .reconcile(&self.snapshot, &self.config.eligible_apps, &policy)
        {
            let severity = match self.selection.status() {
                TargetStatus::Bound(_) => Severity::Success,
                _ => Severity::Warning,
            };
            self.push_log(LogCategory::Target, severity, note);
        }
    }

    // ----- persistence -----------------------------------------------------

    fn save_config(&mut self) {
        let Some(store) = &self.store else { return };
        if let Err(e) = store.save(&self.config) {
            let msg = format!("Could not save configuration: {e}");
            tracing::error!("{msg}");
            self.push_log(LogCategory::System, Severity::Error, msg);
        }
    }

    fn sync_saved_target(&mut self) {
        let criteria = self.selection.criteria().cloned();
        if self.config.saved_target != criteria {
            self.config.saved_target = criteria;
            self.save_config();
        }
    }

    fn device_settings_mut(&mut self, id: &DeviceId) -> &mut DeviceSettings {
        if let Some(pos) = self.config.devices.iter().position(|d| &d.id == id) {
            return &mut self.config.devices[pos];
        }
        self.config.devices.push(DeviceSettings {
            id: id.clone(),
            name: None,
            enabled: false,
            mock_profile: None,
        });
        self.config.devices.last_mut().expect("just pushed")
    }

    // ----- log -------------------------------------------------------------

    fn push_log(&mut self, category: LogCategory, severity: Severity, message: impl Into<String>) {
        self.push_event_log(category, severity, "", "", message.into());
    }

    fn push_event_log(
        &mut self,
        category: LogCategory,
        severity: Severity,
        device: &str,
        button: &str,
        message: String,
    ) {
        match severity {
            Severity::Error => tracing::error!(category = category.label(), "{message}"),
            Severity::Warning => tracing::warn!(category = category.label(), "{message}"),
            Severity::Info | Severity::Success => {
                tracing::debug!(category = category.label(), "{message}")
            }
        }
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            time: SystemTime::now(),
            category,
            severity,
            device: device.to_owned(),
            button: button.to_owned(),
            message,
        });
    }

    // ----- commands --------------------------------------------------------

    fn handle_command(&mut self, cmd: Command) -> Result<(), String> {
        match cmd {
            Command::StartRouting => {
                if let Err(reason) = self.start_blocker() {
                    return Err(format!("Cannot start routing: {reason}"));
                }
                self.routing_requested = true;
            }
            Command::StopRouting => self.routing_requested = false,
            Command::AddMockDevice(profile) => {
                self.require_simulation()?;
                let info = self
                    .mock_input
                    .add_device(None, None, profile)
                    .map_err(|e| e.to_string())?;
                let settings = self.device_settings_mut(&info.id);
                settings.name = Some(info.name.clone());
                settings.enabled = true;
                settings.mock_profile = Some(profile.key().to_owned());
                self.save_config();
            }
            Command::RemoveDevice(id) => {
                self.require_simulation()?;
                if self.mock_input.profile(&id).is_none() {
                    return Err("Only mock devices can be removed".into());
                }
                self.mock_input
                    .remove_device(&id)
                    .map_err(|e| e.to_string())?;
                self.config.devices.retain(|d| d.id != id);
                self.save_config();
            }
            Command::RenameDevice(id, name) => {
                if self.mock_input.profile(&id).is_some() {
                    self.mock_input
                        .rename_device(&id, &name)
                        .map_err(|e| e.to_string())?;
                } else {
                    self.devices.rename(&id, &name).map_err(|e| e.to_string())?;
                }
                let name = name.trim().to_owned();
                self.device_settings_mut(&id).name = Some(name);
                self.save_config();
            }
            Command::SetDeviceEnabled(id, enabled) => {
                let entry = self
                    .devices
                    .get(&id)
                    .ok_or_else(|| format!("Unknown device `{id}`"))?;
                let name = entry.info.name.clone();
                if entry.info.kind == DeviceKind::Simulated {
                    self.mock_input
                        .set_device_enabled(&id, enabled)
                        .map_err(|e| e.to_string())?;
                } else {
                    self.native_input
                        .set_device_enabled(&id, enabled)
                        .map_err(|e| e.to_string())?;
                }
                self.devices
                    .set_enabled(&id, enabled)
                    .map_err(|e| e.to_string())?;
                self.device_settings_mut(&id).enabled = enabled;
                self.save_config();
                let state = if enabled { "enabled" } else { "disabled" };
                self.push_log(
                    LogCategory::Device,
                    Severity::Info,
                    format!("{name} {state}"),
                );
            }
            Command::SetDeviceConnected(id, connected) => {
                self.require_simulation()?;
                self.mock_input
                    .set_connected(&id, connected)
                    .map_err(|e| e.to_string())?;
            }
            Command::EmitMockEvent {
                device,
                button,
                kind,
            } => {
                self.require_simulation()?;
                self.mock_input
                    .emit(&device, &button, kind)
                    .map_err(|e| e.to_string())?;
            }
            Command::ReplayLastEvent(device) => {
                self.require_simulation()?;
                self.mock_input
                    .replay_last(&device)
                    .map_err(|e| e.to_string())?;
            }
            Command::RefreshTargets => {
                self.refresh_targets();
                self.push_log(LogCategory::Target, Severity::Info, "Target list refreshed");
            }
            Command::SelectWindow(window) => {
                let mode = self.config.preferences.selection_mode;
                self.selection
                    .select_window(&self.snapshot, window, mode)
                    .map_err(|e| e.to_string())?;
                self.log_selection("Selected");
            }
            Command::SelectProcess(process) => {
                self.selection
                    .select_process(&self.snapshot, process)
                    .map_err(|e| e.to_string())?;
                self.log_selection("Selected process");
            }
            Command::ConfirmTarget(window) => {
                let mode = self.config.preferences.selection_mode;
                self.selection
                    .confirm(&self.snapshot, window, mode)
                    .map_err(|e| e.to_string())?;
                self.log_selection("Confirmed");
            }
            Command::DismissPrompt {
                restore_previous,
                stop_routing,
            } => {
                self.selection.dismiss(&self.snapshot, restore_previous);
                if stop_routing {
                    self.routing_requested = false;
                }
                self.log_selection("Selection dialog closed");
            }
            Command::RequestTargetChange => {
                self.refresh_targets();
                self.selection
                    .request_change(&self.snapshot, &self.config.eligible_apps)
                    .map_err(|e| e.to_string())?;
            }
            Command::ClearTarget => {
                self.selection.clear();
                self.push_log(LogCategory::Target, Severity::Info, "Target cleared");
            }
            Command::MockLaunch(app) => {
                self.require_simulation()?;
                self.mock_targets.launch(app);
            }
            Command::MockOpenWindow(process) => {
                self.require_simulation()?;
                self.mock_targets
                    .open_window(process)
                    .map_err(|e| e.to_string())?;
            }
            Command::MockCloseWindow(window) => {
                self.require_simulation()?;
                self.mock_targets
                    .close_window(window)
                    .map_err(|e| e.to_string())?;
            }
            Command::MockKillProcess(process) => {
                self.require_simulation()?;
                self.mock_targets.kill(process).map_err(|e| e.to_string())?;
            }
            Command::MockRestartProcess(process) => {
                self.require_simulation()?;
                self.mock_targets
                    .restart(process)
                    .map_err(|e| e.to_string())?;
            }
            Command::MockSetNotifications(enabled) => {
                self.require_simulation()?;
                self.mock_targets.set_notifications(enabled);
            }
            Command::SetInputMode(mode) => self.set_input_mode(mode)?,
            Command::SetSelectionMode(mode) => {
                self.config.preferences.selection_mode = mode;
                self.save_config();
            }
            Command::SetAutoSelectSingle(enabled) => {
                self.config.preferences.auto_select_single = enabled;
                self.save_config();
                self.refresh_targets();
            }
            Command::SetTargetLostPolicy(policy) => {
                self.config.preferences.on_target_lost = policy;
                self.save_config();
                self.refresh_targets();
            }
            Command::SetMultipleTargetsPolicy(policy) => {
                self.config.preferences.on_multiple_targets = policy;
                self.save_config();
            }
            Command::SetShortcut(shortcut) => {
                validate_shortcut(&shortcut)?;
                self.config.preferences.change_target_shortcut = shortcut.trim().to_owned();
                self.save_config();
            }
            Command::SetLogLevel(level) => {
                self.config.logging.level = level;
                if let Some(hook) = &self.on_log_level {
                    hook(level);
                }
                self.save_config();
            }
            Command::SetMapping { button, action } => {
                let label = action
                    .as_ref()
                    .map_or_else(|| "nothing".to_owned(), Action::label);
                self.config.mappings.set(button.clone(), action);
                self.save_config();
                self.push_log(
                    LogCategory::System,
                    Severity::Info,
                    format!("`{button}` now maps to {label}"),
                );
            }
            Command::ResetConfig => self.reset_config(),
            Command::ClearLog => self.log.clear(),
        }
        Ok(())
    }

    fn log_selection(&mut self, verb: &str) {
        let text = match self.selection.status() {
            TargetStatus::Bound(b) => match self.snapshot.candidate(b.window) {
                Some(c) => format!("{verb}: {}", c.describe()),
                None => format!("{verb}: window {}", b.window),
            },
            TargetStatus::AwaitingConfirmation(p) => p.reason.clone(),
            TargetStatus::Unavailable(r) => format!("{verb}: {r}"),
            TargetStatus::NoSelection => format!("{verb}: no target"),
        };
        self.push_log(LogCategory::Target, Severity::Info, text);
    }

    fn set_input_mode(&mut self, mode: InputMode) -> Result<(), String> {
        if mode == self.config.input_mode {
            return Ok(());
        }
        if mode == InputMode::Native {
            Self::native_mode_availability()?;
        }
        self.stop_providers();
        self.routing_requested = false;
        self.devices.clear();
        // Criteria chosen among mock targets must never bind a native
        // application (and vice versa), so the target is forgotten.
        self.selection = TargetSelection::new(None);
        self.config.input_mode = mode;
        self.config.saved_target = None;
        self.save_config();
        self.push_log(
            LogCategory::System,
            Severity::Info,
            format!("Switched to {} mode; target cleared", mode.label()),
        );
        self.start_providers();
        Ok(())
    }

    fn reset_config(&mut self) {
        self.stop_providers();
        self.config = Config::default();
        self.save_config();
        if let Some(hook) = &self.on_log_level {
            hook(self.config.logging.level);
        }
        self.mock_input = MockInputProvider::new();
        self.devices.clear();
        self.router = Router::default();
        self.selection = TargetSelection::new(None);
        self.routing_requested = false;
        self.startup_warnings.clear();
        self.restore_mock_devices();
        self.push_log(
            LogCategory::System,
            Severity::Info,
            "Configuration reset to defaults",
        );
        self.start_providers();
    }

    // ----- provider messages -----------------------------------------------

    fn handle_provider(&mut self, msg: ProviderMessage) {
        match msg {
            ProviderMessage::DeviceAdded(info) => self.on_device_added(info),
            ProviderMessage::DeviceUpdated(info) => {
                let was_connected = self.devices.get(&info.id).map(|e| e.info.connected);
                let (name, connected) = (info.name.clone(), info.connected);
                self.devices.upsert(info, None);
                if was_connected.is_some_and(|c| c != connected) {
                    let (severity, text) = if connected {
                        (Severity::Info, format!("{name} connected"))
                    } else {
                        (Severity::Warning, format!("{name} disconnected"))
                    };
                    self.push_log(LogCategory::Device, severity, text);
                }
            }
            ProviderMessage::DeviceRemoved(id) => {
                self.router.forget_device(&id);
                if let Some(entry) = self.devices.remove(&id) {
                    self.push_log(
                        LogCategory::Device,
                        Severity::Info,
                        format!("{} removed", entry.info.name),
                    );
                }
            }
            ProviderMessage::Input(event) => self.on_input(event),
            ProviderMessage::TargetsChanged => self.refresh_targets(),
            ProviderMessage::Error { provider, message } => {
                self.push_log(
                    LogCategory::System,
                    Severity::Error,
                    format!("{provider}: {message}"),
                );
            }
        }
    }

    fn on_device_added(&mut self, info: DeviceInfo) {
        let pref = self.config.device(&info.id).map(|d| d.enabled);
        // A physical device the user enabled in an earlier session is
        // enabled again in its provider, which only captures enabled devices.
        if info.kind == DeviceKind::Physical
            && pref == Some(true)
            && let Err(e) = self.native_input.set_device_enabled(&info.id, true)
        {
            self.push_log(
                LogCategory::Device,
                Severity::Warning,
                format!("Could not enable {}: {e}", info.name),
            );
        }
        let text = match (info.kind, pref) {
            (DeviceKind::Physical, None) => format!(
                "New physical device {} detected; it stays disabled until you enable it",
                info.name
            ),
            _ => format!("{} added", info.name),
        };
        self.devices.upsert(info, pref);
        self.push_log(LogCategory::Device, Severity::Info, text);
    }

    fn on_input(&mut self, event: InputEvent) {
        self.devices.touch(&event.device_id, event.timestamp);
        let button = event.button.to_string();
        let device = event.device_name.clone();
        let summary = format!(
            "{} {} (#{})",
            event.button,
            event.kind.label(),
            event.sequence
        );
        self.last_event = Some(format!("{summary} from {device}"));
        self.push_event_log(
            LogCategory::Input,
            Severity::Info,
            &device,
            &button,
            format!("{summary} from {}", event.device_id),
        );

        let decision = self.router.route(
            &event,
            &RouteContext {
                routing_enabled: self.routing_requested,
                devices: &self.devices,
                mappings: &self.config.mappings,
                selection: &self.selection,
                snapshot: &self.snapshot,
                now: Instant::now(),
            },
        );
        let (severity, result) = match decision {
            Decision::Route { action, target } => {
                self.push_event_log(
                    LogCategory::Decision,
                    Severity::Info,
                    &device,
                    &button,
                    format!(
                        "Route {action} → {} “{}” (window {}, PID {})",
                        target.app_name, target.title, target.window, target.process.pid
                    ),
                );
                let outcome = self.active_targets().deliver(&target, &action);
                let severity = match outcome {
                    DeliveryOutcome::Simulated
                    | DeliveryOutcome::Delivered
                    | DeliveryOutcome::Sent(_) => Severity::Success,
                    DeliveryOutcome::Unsupported(_) => Severity::Warning,
                    DeliveryOutcome::Failed(_) => Severity::Error,
                };
                self.push_event_log(
                    LogCategory::Delivery,
                    severity,
                    &device,
                    &button,
                    outcome.to_string(),
                );
                if matches!(outcome, DeliveryOutcome::Failed(_)) {
                    // The live state disagrees with our snapshot; resync.
                    self.refresh_targets();
                }
                (
                    severity,
                    format!("{action} → {}: {outcome}", target.app_name),
                )
            }
            Decision::Ignore(reason) => {
                let text = format!("Ignored: {reason}");
                self.push_event_log(
                    LogCategory::Decision,
                    Severity::Info,
                    &device,
                    &button,
                    text.clone(),
                );
                (Severity::Info, text)
            }
            Decision::Reject(reason) => {
                let text = format!("Rejected: {reason}");
                self.push_event_log(
                    LogCategory::Decision,
                    Severity::Warning,
                    &device,
                    &button,
                    text.clone(),
                );
                (Severity::Warning, text)
            }
        };
        self.last_result = Some((severity, result));
    }

    // ----- view ------------------------------------------------------------

    pub fn routing_state(&self) -> RoutingState {
        if !self.routing_requested {
            return RoutingState::Stopped;
        }
        match self.start_blocker() {
            Ok(()) => RoutingState::Active,
            Err(reason) => RoutingState::Paused(reason),
        }
    }

    /// Why routing cannot currently be active, if anything prevents it.
    fn start_blocker(&self) -> Result<(), String> {
        if !self.input_running() {
            return Err(self
                .input_error
                .clone()
                .unwrap_or_else(|| "the input provider is not running".into()));
        }
        match self.selection.status() {
            TargetStatus::Bound(b) if self.snapshot.is_routable(b) => Ok(()),
            TargetStatus::Bound(_) => Err("the selected window is not available".into()),
            TargetStatus::Unavailable(r) => Err(r.clone()),
            TargetStatus::AwaitingConfirmation(_) => Err("waiting for target confirmation".into()),
            TargetStatus::NoSelection => Err("no target is selected".into()),
        }
    }

    fn target_view(&self) -> TargetView {
        let mode = self.selection.criteria().map(|c| c.mode);
        match self.selection.status() {
            TargetStatus::NoSelection => TargetView {
                state: TargetState::None,
                summary: "No target selected".into(),
                detail: "Choose a window or process on the Target Application page.".into(),
                mode,
            },
            TargetStatus::Bound(b) => {
                let summary = self
                    .snapshot
                    .candidate(b.window)
                    .map_or_else(|| format!("Window {}", b.window), |c| c.describe());
                TargetView {
                    state: TargetState::Bound,
                    summary,
                    detail: format!(
                        "{} mode · PID {} · window {}",
                        mode.unwrap_or_default().label(),
                        b.process.pid,
                        b.window
                    ),
                    mode,
                }
            }
            TargetStatus::Unavailable(reason) => {
                let summary = self.selection.criteria().map_or_else(
                    || "Target unavailable".to_owned(),
                    |c| match &c.window_title {
                        Some(t) => format!("{} — “{t}”", c.identity.name),
                        None => c.identity.name.clone(),
                    },
                );
                TargetView {
                    state: TargetState::Unavailable,
                    summary,
                    detail: reason.clone(),
                    mode,
                }
            }
            TargetStatus::AwaitingConfirmation(p) => TargetView {
                state: TargetState::AwaitingConfirmation,
                summary: "Waiting for confirmation".into(),
                detail: p.reason.clone(),
                mode,
            },
        }
    }

    fn mapping_views(&self) -> Vec<MappingView> {
        let mut buttons: Vec<ButtonId> = STANDARD_BUTTONS
            .iter()
            .map(|b| ButtonId::standard(b))
            .collect();
        let extra: BTreeSet<ButtonId> = self
            .config
            .mappings
            .iter()
            .map(|(b, _)| b.clone())
            .chain(self.devices.iter().flat_map(|d| d.info.buttons.clone()))
            .filter(|b| !buttons.contains(b))
            .collect();
        buttons.extend(extra);
        buttons
            .into_iter()
            .map(|button| MappingView {
                action: self.config.mappings.resolve(&button).cloned(),
                button,
            })
            .collect()
    }

    fn capability_sections(&self) -> Vec<CapabilitySection> {
        let os = platform::os_label();
        let mut sections = vec![
            CapabilitySection {
                title: "Simulation".into(),
                items: self
                    .mock_input
                    .capabilities()
                    .into_iter()
                    .chain(self.mock_targets.capabilities())
                    .collect(),
            },
            CapabilitySection {
                title: format!("Native input ({os})"),
                items: self.native_input.capabilities(),
            },
            CapabilitySection {
                title: format!("Native targets ({os})"),
                items: self.native_targets.capabilities(),
            },
        ];
        if let Some(e) = &self.input_error {
            sections[1].items.push(Capability::new(
                "Provider start",
                CapabilityStatus::Error,
                e.clone(),
            ));
        }
        sections
    }

    /// Problems the user should act on. Routing state and pending target
    /// confirmation are shown by the UI itself and are not repeated here.
    fn warnings(&self) -> Vec<String> {
        let mut warnings = self.startup_warnings.clone();
        if self.config.input_mode == InputMode::Native {
            match Self::native_mode_availability() {
                Err(_) => warnings.push(format!(
                    "Native mode is selected, but native input and target adapters are not implemented on {}. Switch to Simulation mode to test routing.",
                    platform::os_label()
                )),
                Ok(()) => warnings.extend(
                    platform::input_capabilities()
                        .into_iter()
                        .chain(platform::target_capabilities())
                        .filter(|c| !c.status.is_operational())
                        .map(|c| format!("{}: {}. {}", c.component, c.status, c.detail)),
                ),
            }
        }
        if let Some(e) = &self.targets_error {
            warnings.push(e.clone());
        }
        if let Some(c) = self.selection.criteria()
            && !c.identity.is_stable()
        {
            warnings.push(format!(
                "{} has no stable identity (executable path or bundle id); it cannot be recognized again after it closes",
                c.identity.name
            ));
        }
        for d in self
            .devices
            .iter()
            .filter(|d| d.enabled && !d.info.connected)
        {
            warnings.push(format!("{} is disconnected", d.info.name));
        }
        let dropped = self.dropped_events.load(Ordering::Relaxed);
        if dropped > 0 {
            warnings.push(format!(
                "{dropped} provider message(s) were dropped because the event queue was full"
            ));
        }
        warnings
    }

    pub fn view(&self) -> ViewModel {
        let routing = self.routing_state();
        let can_start = self.start_blocker();
        let binding = self.selection.binding();
        let simulation = self.config.input_mode == InputMode::Simulation;
        let eligible = |key: ProcessKey| {
            self.snapshot.process(key).is_some_and(|p| {
                self.config
                    .eligible_apps
                    .iter()
                    .any(|m| m.matches(&p.identity))
            })
        };

        let status = if let Some(e) = &self.input_error {
            (Severity::Error, e.clone())
        } else {
            match &routing {
                RoutingState::Active if simulation => {
                    (Severity::Success, "Routing active (simulated)".to_owned())
                }
                RoutingState::Active => (Severity::Success, "Routing active".to_owned()),
                RoutingState::Paused(r) => (Severity::Warning, format!("Routing paused: {r}")),
                RoutingState::Stopped => match &can_start {
                    Ok(()) => (Severity::Info, "Ready to route".to_owned()),
                    Err(r) => (Severity::Info, format!("Idle: {r}")),
                },
            }
        };

        ViewModel {
            input_mode: self.config.input_mode,
            native_mode: Self::native_mode_availability(),
            status,
            routing_requested: self.routing_requested,
            can_start,
            devices: self
                .devices
                .iter()
                .map(|d| DeviceView {
                    id: d.info.id.to_string(),
                    name: d.info.name.clone(),
                    simulated: d.info.kind == DeviceKind::Simulated,
                    connected: d.info.connected,
                    enabled: d.enabled,
                    last_activity: d.last_activity,
                    buttons: d.info.buttons.iter().map(ToString::to_string).collect(),
                    details: d.info.details.clone(),
                })
                .collect(),
            target: self.target_view(),
            prompt: self.selection.prompt().map(|p| PromptView {
                reason: p.reason.clone(),
                candidates: p
                    .candidates
                    .iter()
                    .map(|c| CandidateView {
                        window: c.window,
                        app_name: c.app_name.clone(),
                        title: c.title.clone(),
                        pid: c.process.pid,
                    })
                    .collect(),
            }),
            processes: self
                .snapshot
                .processes
                .iter()
                .map(|p| ProcessView {
                    key: p.key,
                    app_name: p.identity.name.clone(),
                    process_name: p.identity.process_name.clone(),
                    executable: p.identity.executable.clone().unwrap_or_default(),
                    window_count: self
                        .snapshot
                        .windows
                        .iter()
                        .filter(|w| w.process == p.key)
                        .count(),
                    eligible: eligible(p.key),
                    selected: binding.is_some_and(|b| b.process == p.key),
                })
                .collect(),
            windows: self
                .snapshot
                .windows
                .iter()
                .map(|w| {
                    let process = self.snapshot.process(w.process);
                    WindowView {
                        id: w.id,
                        process: w.process,
                        app_name: process.map(|p| p.identity.name.clone()).unwrap_or_default(),
                        process_name: process
                            .map(|p| p.identity.process_name.clone())
                            .unwrap_or_default(),
                        title: w.title.clone(),
                        visible: w.visible,
                        eligible: eligible(w.process),
                        selected: binding.is_some_and(|b| b.window == w.id),
                        received: if simulation {
                            self.mock_targets.received(w.id).to_vec()
                        } else {
                            Vec::new()
                        },
                    }
                })
                .collect(),
            targets_error: self.targets_error.clone(),
            notifications_active: match self.config.input_mode {
                InputMode::Simulation => self.mock_targets.notifications_active(),
                InputMode::Native => self.native_targets.notifications_active(),
            },
            mock_notifications: self.mock_targets.notifications_enabled(),
            last_event: self.last_event.clone(),
            last_result: self.last_result.clone(),
            warnings: self.warnings(),
            routing,
            log: self.log.iter().rev().cloned().collect(),
            mappings: self.mapping_views(),
            preferences: self.config.preferences.clone(),
            log_level: self.config.logging.level,
            capabilities: self.capability_sections(),
            shortcut: platform::shortcut_capability(),
            config_path: self.store.as_ref().map(|s| s.path().display().to_string()),
            log_file: self.log_file.clone(),
            environment: platform::environment(),
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
        }
    }
}
