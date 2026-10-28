//! Mock applications and windows with a scriptable lifecycle.

use std::collections::HashMap;

use crate::core::capabilities::{Capability, CapabilityStatus};
use crate::core::mappings::Action;
use crate::core::routing::{DeliveryOutcome, RouteTarget};
use crate::core::targets::{
    AppIdentity, ProcessInfo, ProcessKey, TargetOrigin, TargetSnapshot, WindowId, WindowInfo,
};
use crate::provider::{ProviderError, ProviderMessage, ProviderSink};

use super::TargetProvider;

/// Keep at most this many delivered actions per mock window.
const INBOX_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockApp {
    PowerPoint,
    Acrobat,
    TextEditor,
}

impl MockApp {
    pub const ALL: [Self; 3] = [Self::PowerPoint, Self::Acrobat, Self::TextEditor];

    pub fn label(self) -> &'static str {
        match self {
            Self::PowerPoint => "PowerPoint (mock)",
            Self::Acrobat => "Acrobat (mock)",
            Self::TextEditor => "Text editor (mock)",
        }
    }

    fn identity(self) -> AppIdentity {
        let (name, process, exe) = match self {
            Self::PowerPoint => (
                "Microsoft PowerPoint",
                "POWERPNT.EXE",
                r"C:\Program Files\Microsoft Office\root\Office16\POWERPNT.EXE",
            ),
            Self::Acrobat => (
                "Adobe Acrobat",
                "Acrobat.exe",
                r"C:\Program Files\Adobe\Acrobat DC\Acrobat\Acrobat.exe",
            ),
            Self::TextEditor => ("Notepad", "notepad.exe", r"C:\Windows\System32\notepad.exe"),
        };
        AppIdentity {
            name: name.into(),
            process_name: process.into(),
            executable: Some(exe.into()),
            bundle_id: None,
        }
    }

    fn first_title(self) -> &'static str {
        match self {
            Self::PowerPoint => "Quarterly Review.pptx - PowerPoint",
            Self::Acrobat => "Handout.pdf - Adobe Acrobat",
            Self::TextEditor => "notes.txt - Notepad",
        }
    }

    fn numbered_title(self, n: u32) -> String {
        match self {
            Self::PowerPoint => format!("Presentation{n}.pptx - PowerPoint"),
            Self::Acrobat => format!("Document{n}.pdf - Adobe Acrobat"),
            Self::TextEditor => format!("Untitled {n} - Notepad"),
        }
    }
}

#[derive(Debug)]
pub struct MockTargetProvider {
    processes: Vec<(ProcessInfo, MockApp)>,
    windows: Vec<WindowInfo>,
    inbox: HashMap<WindowId, Vec<Action>>,
    next_pid: u32,
    next_instance: u64,
    next_window: u64,
    title_counter: u32,
    sink: Option<ProviderSink>,
    notifications: bool,
}

impl Default for MockTargetProvider {
    fn default() -> Self {
        Self {
            processes: Vec::new(),
            windows: Vec::new(),
            inbox: HashMap::new(),
            next_pid: 4100,
            next_instance: 0,
            next_window: 0x2a00,
            title_counter: 0,
            sink: None,
            notifications: true,
        }
    }
}

impl MockTargetProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// One PowerPoint window and one ineligible text editor.
    pub fn with_demo_scene() -> Self {
        let mut p = Self::new();
        p.launch(MockApp::PowerPoint);
        p.launch(MockApp::TextEditor);
        p
    }

    fn changed(&self) {
        if !self.notifications {
            return;
        }
        if let Some(sink) = &self.sink
            && let Err(e) = sink.send(ProviderMessage::TargetsChanged)
        {
            tracing::warn!("mock target notification dropped: {e}");
        }
    }

    fn spawn(&mut self, app: MockApp) -> ProcessKey {
        self.next_pid += 4;
        self.next_instance += 1;
        let key = ProcessKey {
            pid: self.next_pid,
            instance: self.next_instance,
        };
        self.processes.push((
            ProcessInfo {
                key,
                identity: app.identity(),
            },
            app,
        ));
        key
    }

    fn add_window(&mut self, process: ProcessKey, title: String) -> WindowId {
        self.next_window += 1;
        let id = WindowId(self.next_window);
        self.windows.push(WindowInfo {
            id,
            process,
            title,
            visible: true,
        });
        id
    }

    /// Starts a new mock process with its default window.
    pub fn launch(&mut self, app: MockApp) -> ProcessKey {
        let key = self.spawn(app);
        self.add_window(key, app.first_title().to_owned());
        self.changed();
        key
    }

    pub fn app_of(&self, process: ProcessKey) -> Option<MockApp> {
        self.processes
            .iter()
            .find(|(p, _)| p.key == process)
            .map(|(_, app)| *app)
    }

    fn require_process(&self, process: ProcessKey) -> Result<MockApp, ProviderError> {
        self.app_of(process)
            .ok_or_else(|| ProviderError::UnknownTarget(format!("process {}", process.pid)))
    }

    /// Opens another window in a running mock process.
    pub fn open_window(&mut self, process: ProcessKey) -> Result<WindowId, ProviderError> {
        let app = self.require_process(process)?;
        self.title_counter += 1;
        let id = self.add_window(process, app.numbered_title(self.title_counter));
        self.changed();
        Ok(id)
    }

    pub fn close_window(&mut self, id: WindowId) -> Result<(), ProviderError> {
        let before = self.windows.len();
        self.windows.retain(|w| w.id != id);
        if self.windows.len() == before {
            return Err(ProviderError::UnknownTarget(format!("window {id}")));
        }
        self.inbox.remove(&id);
        self.changed();
        Ok(())
    }

    /// Terminates a process and all of its windows.
    pub fn kill(&mut self, process: ProcessKey) -> Result<(), ProviderError> {
        self.require_process(process)?;
        self.remove_process(process);
        self.changed();
        Ok(())
    }

    fn remove_process(&mut self, process: ProcessKey) -> Vec<String> {
        self.processes.retain(|(p, _)| p.key != process);
        let mut titles = Vec::new();
        let inbox = &mut self.inbox;
        self.windows.retain(|w| {
            if w.process == process {
                titles.push(w.title.clone());
                inbox.remove(&w.id);
                false
            } else {
                true
            }
        });
        titles
    }

    /// Terminates a process and starts the same application again with a new
    /// PID, reopening windows with the same titles.
    pub fn restart(&mut self, process: ProcessKey) -> Result<ProcessKey, ProviderError> {
        let app = self.require_process(process)?;
        let titles = self.remove_process(process);
        let key = self.spawn(app);
        for title in titles {
            self.add_window(key, title);
        }
        self.changed();
        Ok(key)
    }

    pub fn set_notifications(&mut self, enabled: bool) {
        self.notifications = enabled;
        self.changed();
    }

    pub fn notifications_enabled(&self) -> bool {
        self.notifications
    }

    /// Actions delivered to a mock window, oldest first.
    pub fn received(&self, window: WindowId) -> &[Action] {
        self.inbox.get(&window).map_or(&[], Vec::as_slice)
    }

    pub fn total_received(&self) -> usize {
        self.inbox.values().map(Vec::len).sum()
    }
}

impl TargetProvider for MockTargetProvider {
    fn name(&self) -> &'static str {
        "mock targets"
    }

    fn origin(&self) -> TargetOrigin {
        TargetOrigin::Simulated
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::new(
                "Mock applications and windows",
                CapabilityStatus::Supported,
                "Simulated processes and windows with scriptable lifecycle",
            ),
            Capability::new(
                "Mock lifecycle notifications",
                if self.notifications {
                    CapabilityStatus::Supported
                } else {
                    CapabilityStatus::TemporarilyUnavailable
                },
                if self.notifications {
                    "Changes are pushed automatically"
                } else {
                    "Disabled for testing; use Refresh"
                },
            ),
            Capability::new(
                "Simulated delivery",
                CapabilityStatus::Supported,
                "Routed actions are recorded by the mock window; nothing is sent to real applications",
            ),
        ]
    }

    fn notifications_active(&self) -> bool {
        self.notifications && self.sink.is_some()
    }

    fn start(&mut self, sink: ProviderSink) -> Result<(), ProviderError> {
        self.sink = Some(sink);
        Ok(())
    }

    fn stop(&mut self) {
        self.sink = None;
    }

    fn snapshot(&mut self) -> Result<TargetSnapshot, ProviderError> {
        Ok(TargetSnapshot {
            origin: TargetOrigin::Simulated,
            processes: self.processes.iter().map(|(p, _)| p.clone()).collect(),
            windows: self.windows.clone(),
        })
    }

    fn deliver(&mut self, target: &RouteTarget, action: &Action) -> DeliveryOutcome {
        if target.origin != TargetOrigin::Simulated {
            return DeliveryOutcome::Failed("mock provider received a non-mock target".into());
        }
        let alive = self
            .windows
            .iter()
            .any(|w| w.id == target.window && w.process == target.process && w.visible);
        if !alive {
            return DeliveryOutcome::Failed(format!(
                "mock window {} no longer exists",
                target.window
            ));
        }
        let inbox = self.inbox.entry(target.window).or_default();
        if inbox.len() == INBOX_LIMIT {
            inbox.remove(0);
        }
        inbox.push(action.clone());
        DeliveryOutcome::Simulated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(p: &mut MockTargetProvider, window: WindowId) -> RouteTarget {
        let snap = p.snapshot().unwrap();
        let c = snap.candidate(window).unwrap();
        RouteTarget {
            window,
            process: c.process,
            app_name: c.app_name,
            title: c.title,
            origin: TargetOrigin::Simulated,
            generation: 0,
        }
    }

    #[test]
    fn restart_reopens_windows_under_a_new_pid() {
        let mut p = MockTargetProvider::new();
        let ppt = p.launch(MockApp::PowerPoint);
        p.open_window(ppt).unwrap();
        let restarted = p.restart(ppt).unwrap();
        assert_ne!(restarted.pid, ppt.pid);
        let snap = p.snapshot().unwrap();
        assert!(snap.process(ppt).is_none());
        assert_eq!(
            snap.windows
                .iter()
                .filter(|w| w.process == restarted)
                .count(),
            2
        );
        assert_eq!(
            snap.process(restarted).unwrap().identity,
            MockApp::PowerPoint.identity()
        );
    }

    #[test]
    fn delivery_is_recorded_and_validated() {
        let mut p = MockTargetProvider::new();
        let ppt = p.launch(MockApp::PowerPoint);
        let window = p.snapshot().unwrap().windows[0].id;
        let t = target(&mut p, window);
        assert_eq!(p.deliver(&t, &Action::Next), DeliveryOutcome::Simulated);
        assert_eq!(p.received(window), &[Action::Next]);

        p.kill(ppt).unwrap();
        assert!(matches!(
            p.deliver(&t, &Action::Next),
            DeliveryOutcome::Failed(_)
        ));
        assert_eq!(p.total_received(), 0);
    }

    #[test]
    fn native_targets_are_refused() {
        let mut p = MockTargetProvider::new();
        p.launch(MockApp::PowerPoint);
        let window = p.snapshot().unwrap().windows[0].id;
        let mut t = target(&mut p, window);
        t.origin = TargetOrigin::Native;
        assert!(matches!(
            p.deliver(&t, &Action::Next),
            DeliveryOutcome::Failed(_)
        ));
    }
}
