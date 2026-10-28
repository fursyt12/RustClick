//! Target model and the target-selection lifecycle state machine.
//!
//! A [`TargetSnapshot`] is what a target provider currently sees. A
//! [`TargetSelection`] holds what the user chose (stable [`TargetCriteria`])
//! and what that choice is currently bound to (a transient [`Binding`]).
//! [`TargetSelection::reconcile`] re-evaluates the binding whenever the
//! snapshot changes: it rebinds only to unambiguous matches of the saved
//! criteria and asks the user otherwise. It never picks an arbitrary window.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifies one process lifetime. `instance` distinguishes a restarted or
/// PID-reusing process (platform start time; a counter in the mock provider).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessKey {
    pub pid: u32,
    pub instance: u64,
}

/// Transient window handle. Never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:x}", self.0)
    }
}

/// Application identity as far as the platform can establish it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AppIdentity {
    pub name: String,
    pub process_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
}

impl AppIdentity {
    /// Whether this identity can be matched across restarts.
    pub fn is_stable(&self) -> bool {
        self.executable.is_some() || self.bundle_id.is_some()
    }

    /// Whether two identities denote the same application, using only stable
    /// identifiers (bundle id, then executable path). Names and PIDs are
    /// never sufficient.
    pub fn same_app(&self, other: &Self) -> bool {
        if let (Some(a), Some(b)) = (&self.bundle_id, &other.bundle_id) {
            return a == b;
        }
        matches!((&self.executable, &other.executable), (Some(a), Some(b)) if a == b)
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Describes an application that is eligible for automatic selection and
/// ambiguity prompts (e.g. PowerPoint, Acrobat).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppMatcher {
    pub name: String,
    /// Executable file names, compared case-insensitively.
    #[serde(default)]
    pub executables: Vec<String>,
    #[serde(default)]
    pub bundle_ids: Vec<String>,
}

impl AppMatcher {
    pub fn matches(&self, id: &AppIdentity) -> bool {
        if let Some(bundle) = &id.bundle_id
            && self
                .bundle_ids
                .iter()
                .any(|b| b.eq_ignore_ascii_case(bundle))
        {
            return true;
        }
        let exe = id.executable.as_deref().map(file_name);
        self.executables.iter().any(|e| {
            exe.is_some_and(|x| x.eq_ignore_ascii_case(e))
                || e.eq_ignore_ascii_case(&id.process_name)
        })
    }

    pub fn presentation_defaults() -> Vec<Self> {
        vec![
            Self {
                name: "Microsoft PowerPoint".into(),
                executables: vec!["POWERPNT.EXE".into(), "Microsoft PowerPoint".into()],
                bundle_ids: vec!["com.microsoft.Powerpoint".into()],
            },
            Self {
                name: "Adobe Acrobat".into(),
                executables: vec!["Acrobat.exe".into(), "AcroRd32.exe".into()],
                bundle_ids: vec!["com.adobe.Acrobat.Pro".into(), "com.adobe.Reader".into()],
            },
        ]
    }
}

/// Whether targets are mock objects or real applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOrigin {
    Simulated,
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub key: ProcessKey,
    pub identity: AppIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WindowId,
    pub process: ProcessKey,
    pub title: String,
    /// Whether the window can currently receive routed actions.
    pub visible: bool,
}

/// A routable window, as offered to the user or bound by the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub window: WindowId,
    pub process: ProcessKey,
    pub app_name: String,
    pub process_name: String,
    pub title: String,
}

impl Candidate {
    pub fn binding(&self) -> Binding {
        Binding {
            process: self.process,
            window: self.window,
        }
    }

    pub fn describe(&self) -> String {
        format!("{} — “{}”", self.app_name, self.title)
    }
}

/// Everything a target provider currently sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSnapshot {
    pub origin: TargetOrigin,
    pub processes: Vec<ProcessInfo>,
    pub windows: Vec<WindowInfo>,
}

impl TargetSnapshot {
    pub fn empty(origin: TargetOrigin) -> Self {
        Self {
            origin,
            processes: Vec::new(),
            windows: Vec::new(),
        }
    }

    pub fn process(&self, key: ProcessKey) -> Option<&ProcessInfo> {
        self.processes.iter().find(|p| p.key == key)
    }

    pub fn window(&self, id: WindowId) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn is_routable(&self, binding: &Binding) -> bool {
        self.candidate(binding.window)
            .is_some_and(|c| c.process == binding.process)
    }

    /// The window as a candidate, if it exists, is visible, and its process is alive.
    pub fn candidate(&self, id: WindowId) -> Option<Candidate> {
        let w = self.window(id).filter(|w| w.visible)?;
        let p = self.process(w.process)?;
        Some(Candidate {
            window: w.id,
            process: p.key,
            app_name: p.identity.name.clone(),
            process_name: p.identity.process_name.clone(),
            title: w.title.clone(),
        })
    }

    pub fn candidates_where(
        &self,
        pred: impl Fn(&ProcessInfo, &WindowInfo) -> bool,
    ) -> Vec<Candidate> {
        self.windows
            .iter()
            .filter(|w| self.process(w.process).is_some_and(|p| pred(p, w)))
            .filter_map(|w| self.candidate(w.id))
            .collect()
    }

    pub fn eligible_candidates(&self, eligible: &[AppMatcher]) -> Vec<Candidate> {
        self.candidates_where(|p, _| eligible.iter().any(|m| m.matches(&p.identity)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub process: ProcessKey,
    pub window: WindowId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    /// One specific window.
    #[default]
    Window,
    /// A process; any of its windows may serve as the destination.
    Process,
}

impl SelectionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Window => "Window",
            Self::Process => "Process",
        }
    }
}

/// Persistable description of the user's choice. Contains no PIDs or handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetCriteria {
    pub mode: SelectionMode,
    pub identity: AppIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetLostPolicy {
    /// Rebind automatically when exactly one replacement matches the criteria.
    #[default]
    AutoRebind,
    /// Always ask before binding a replacement.
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultipleTargetsPolicy {
    /// Keep the current target when other eligible applications appear.
    #[default]
    KeepCurrent,
    /// Ask again whenever a new eligible window appears alongside others.
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TargetPolicy {
    pub preferred_mode: SelectionMode,
    pub auto_select_single: bool,
    pub on_target_lost: TargetLostPolicy,
    pub on_multiple: MultipleTargetsPolicy,
}

/// Why a confirmation prompt was opened; determines how its candidates are
/// recomputed when targets change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSource {
    /// Several eligible applications are open.
    Eligible,
    /// The user asked to change the target.
    Explicit,
    /// The selected target disappeared and the replacement is not unambiguous.
    Replacement,
    /// The user selected a process that has several windows.
    ProcessWindows(ProcessKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub reason: String,
    pub candidates: Vec<Candidate>,
    pub source: PromptSource,
    /// What was bound before the prompt opened, restored if the prompt is cancelled.
    previous: Option<(Binding, TargetCriteria)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetStatus {
    NoSelection,
    Bound(Binding),
    /// Criteria are kept; waiting for a matching window.
    Unavailable(String),
    /// Routing must not happen until the user picks one of the candidates.
    AwaitingConfirmation(Prompt),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SelectionError {
    #[error("window {0} is not available")]
    WindowUnavailable(WindowId),
    #[error("process {0} is not running")]
    ProcessNotRunning(u32),
    #[error("no target confirmation is pending")]
    NoPrompt,
    #[error("there are no windows to choose from")]
    NothingToSelect,
}

#[derive(Debug, Clone)]
pub struct TargetSelection {
    criteria: Option<TargetCriteria>,
    status: TargetStatus,
    /// Last process bound in process mode, used to prefer windows of the same
    /// process over a restarted one.
    last_process: Option<ProcessKey>,
    generation: u64,
    changed_at: Instant,
    /// Set when the user dismissed a prompt or cleared the target; suppresses
    /// automatic prompts and auto-selection until the situation changes.
    prompt_suppressed: bool,
    known_eligible: BTreeSet<WindowId>,
}

impl TargetSelection {
    pub fn new(saved: Option<TargetCriteria>) -> Self {
        let status = match saved {
            Some(_) => TargetStatus::Unavailable("Waiting for the saved target".into()),
            None => TargetStatus::NoSelection,
        };
        Self {
            criteria: saved,
            status,
            last_process: None,
            generation: 0,
            changed_at: Instant::now(),
            prompt_suppressed: false,
            known_eligible: BTreeSet::new(),
        }
    }

    pub fn criteria(&self) -> Option<&TargetCriteria> {
        self.criteria.as_ref()
    }

    pub fn status(&self) -> &TargetStatus {
        &self.status
    }

    pub fn binding(&self) -> Option<Binding> {
        match self.status {
            TargetStatus::Bound(b) => Some(b),
            _ => None,
        }
    }

    pub fn prompt(&self) -> Option<&Prompt> {
        match &self.status {
            TargetStatus::AwaitingConfirmation(p) => Some(p),
            _ => None,
        }
    }

    /// Incremented on every status change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// When the status last changed. Events captured earlier are stale.
    pub fn changed_at(&self) -> Instant {
        self.changed_at
    }

    fn set_status(&mut self, status: TargetStatus) {
        if self.status != status {
            self.status = status;
            self.generation += 1;
            self.changed_at = Instant::now();
        }
    }

    fn bind(&mut self, snap: &TargetSnapshot, c: &Candidate, mode: SelectionMode) {
        let identity = snap
            .process(c.process)
            .map(|p| p.identity.clone())
            .unwrap_or_default();
        self.criteria = Some(TargetCriteria {
            mode,
            identity,
            window_title: Some(c.title.clone()),
        });
        self.last_process = Some(c.process);
        self.prompt_suppressed = false;
        self.set_status(TargetStatus::Bound(c.binding()));
    }

    fn open_prompt(&mut self, candidates: Vec<Candidate>, reason: &str, source: PromptSource) {
        let previous = match &self.status {
            TargetStatus::Bound(b) => self.criteria.clone().map(|c| (*b, c)),
            TargetStatus::AwaitingConfirmation(p) => p.previous.clone(),
            _ => None,
        };
        self.set_status(TargetStatus::AwaitingConfirmation(Prompt {
            reason: reason.to_owned(),
            candidates,
            source,
            previous,
        }));
    }

    /// Explicit user choice of one window.
    pub fn select_window(
        &mut self,
        snap: &TargetSnapshot,
        window: WindowId,
        mode: SelectionMode,
    ) -> Result<(), SelectionError> {
        let c = snap
            .candidate(window)
            .ok_or(SelectionError::WindowUnavailable(window))?;
        self.bind(snap, &c, mode);
        Ok(())
    }

    /// Explicit user choice of a process. Binds directly if it has exactly
    /// one window, otherwise asks which window to use.
    pub fn select_process(
        &mut self,
        snap: &TargetSnapshot,
        process: ProcessKey,
    ) -> Result<(), SelectionError> {
        let info = snap
            .process(process)
            .ok_or(SelectionError::ProcessNotRunning(process.pid))?;
        let windows = snap.candidates_where(|p, _| p.key == process);
        match windows.as_slice() {
            [only] => self.bind(snap, only, SelectionMode::Process),
            [] => {
                self.criteria = Some(TargetCriteria {
                    mode: SelectionMode::Process,
                    identity: info.identity.clone(),
                    window_title: None,
                });
                self.last_process = Some(process);
                self.prompt_suppressed = false;
                let reason = format!("{} has no open windows", info.identity.name);
                self.set_status(TargetStatus::Unavailable(reason));
            }
            _ => {
                self.prompt_suppressed = false;
                self.open_prompt(
                    windows,
                    "This process has several windows. Choose the one that should receive input.",
                    PromptSource::ProcessWindows(process),
                );
            }
        }
        Ok(())
    }

    /// The user wants to pick a different target.
    pub fn request_change(
        &mut self,
        snap: &TargetSnapshot,
        eligible: &[AppMatcher],
    ) -> Result<(), SelectionError> {
        let candidates = explicit_candidates(snap, eligible);
        if candidates.is_empty() {
            return Err(SelectionError::NothingToSelect);
        }
        self.prompt_suppressed = false;
        self.open_prompt(
            candidates,
            "Choose the application that should receive input.",
            PromptSource::Explicit,
        );
        Ok(())
    }

    /// Resolves a pending prompt with one of its candidates (or any other
    /// currently available window).
    pub fn confirm(
        &mut self,
        snap: &TargetSnapshot,
        window: WindowId,
        preferred_mode: SelectionMode,
    ) -> Result<(), SelectionError> {
        let prompt = self.prompt().ok_or(SelectionError::NoPrompt)?;
        let mode = match prompt.source {
            PromptSource::ProcessWindows(_) => SelectionMode::Process,
            PromptSource::Replacement => self.criteria.as_ref().map_or(preferred_mode, |c| c.mode),
            PromptSource::Eligible | PromptSource::Explicit => preferred_mode,
        };
        self.select_window(snap, window, mode)
    }

    /// Closes a pending prompt without choosing. With `restore_previous`, the
    /// target bound before the prompt is restored if it is still routable.
    pub fn dismiss(&mut self, snap: &TargetSnapshot, restore_previous: bool) {
        let Some(prompt) = self.prompt().cloned() else {
            return;
        };
        self.prompt_suppressed = true;
        if restore_previous
            && let Some((binding, criteria)) = prompt.previous
            && snap.is_routable(&binding)
        {
            self.criteria = Some(criteria);
            self.set_status(TargetStatus::Bound(binding));
            return;
        }
        let status = match self.criteria {
            Some(_) => TargetStatus::Unavailable("Target selection was cancelled".into()),
            None => TargetStatus::NoSelection,
        };
        self.set_status(status);
    }

    /// Forgets the target entirely and suppresses auto-selection.
    pub fn clear(&mut self) {
        self.criteria = None;
        self.last_process = None;
        self.prompt_suppressed = true;
        self.set_status(TargetStatus::NoSelection);
    }

    /// Re-evaluates the selection against a new snapshot. Returns a
    /// human-readable note when something noteworthy happened.
    pub fn reconcile(
        &mut self,
        snap: &TargetSnapshot,
        eligible: &[AppMatcher],
        policy: &TargetPolicy,
    ) -> Option<String> {
        let eligible_now = snap.eligible_candidates(eligible);
        let ids: BTreeSet<WindowId> = eligible_now.iter().map(|c| c.window).collect();
        let new_eligible = ids.difference(&self.known_eligible).next().is_some();
        self.known_eligible = ids;
        if new_eligible {
            self.prompt_suppressed = false;
        }

        match self.status.clone() {
            TargetStatus::NoSelection => {
                if self.prompt_suppressed {
                    return None;
                }
                match eligible_now.as_slice() {
                    [only] if policy.auto_select_single => {
                        self.bind(snap, only, policy.preferred_mode);
                        Some(format!(
                            "Selected {} (the only eligible target)",
                            only.describe()
                        ))
                    }
                    [] | [_] => None,
                    _ => {
                        self.open_prompt(
                            eligible_now,
                            "More than one eligible application is open. Choose where input should go.",
                            PromptSource::Eligible,
                        );
                        Some("Several eligible targets are open; confirmation required".into())
                    }
                }
            }
            TargetStatus::Bound(b) => {
                if let Some(w) = snap.window(b.window).filter(|_| snap.is_routable(&b)) {
                    if let Some(c) = self.criteria.as_mut() {
                        c.window_title = Some(w.title.clone());
                    }
                    if policy.on_multiple == MultipleTargetsPolicy::Ask
                        && new_eligible
                        && eligible_now.len() > 1
                    {
                        self.open_prompt(
                            eligible_now,
                            "Another eligible application appeared. Confirm where input should go.",
                            PromptSource::Eligible,
                        );
                        return Some(
                            "Another eligible target appeared; confirmation required".into(),
                        );
                    }
                    return None;
                }
                self.recover(snap, policy)
            }
            TargetStatus::Unavailable(_) => self.recover(snap, policy),
            TargetStatus::AwaitingConfirmation(prompt) => {
                self.refresh_prompt(snap, eligible, policy, prompt)
            }
        }
    }

    /// Finds a replacement for a lost target.
    fn recover(&mut self, snap: &TargetSnapshot, policy: &TargetPolicy) -> Option<String> {
        let criteria = self.criteria.clone()?;
        let candidates = self.replacement_candidates(snap, &criteria);
        match candidates.as_slice() {
            [] => {
                let reason = self.unavailable_reason(snap, &criteria);
                self.set_unavailable(reason)
            }
            [only] if policy.on_target_lost == TargetLostPolicy::AutoRebind => {
                self.bind(snap, only, criteria.mode);
                Some(format!("Rebound to {}", only.describe()))
            }
            _ if self.prompt_suppressed => self.set_unavailable(
                "A replacement target is available; use Change target to confirm it".into(),
            ),
            _ => {
                let reason = if candidates.len() == 1 {
                    "The selected target disappeared. Confirm the replacement."
                } else {
                    "The selected target disappeared and several windows match it. Choose one."
                };
                self.open_prompt(candidates, reason, PromptSource::Replacement);
                Some("Replacement target needs confirmation".into())
            }
        }
    }

    fn set_unavailable(&mut self, reason: String) -> Option<String> {
        let changed = !matches!(&self.status, TargetStatus::Unavailable(r) if *r == reason);
        self.set_status(TargetStatus::Unavailable(reason.clone()));
        changed.then_some(reason)
    }

    fn refresh_prompt(
        &mut self,
        snap: &TargetSnapshot,
        eligible: &[AppMatcher],
        policy: &TargetPolicy,
        prompt: Prompt,
    ) -> Option<String> {
        let candidates = match prompt.source {
            PromptSource::Eligible => snap.eligible_candidates(eligible),
            PromptSource::Explicit => explicit_candidates(snap, eligible),
            PromptSource::ProcessWindows(key) => snap.candidates_where(|p, _| p.key == key),
            PromptSource::Replacement => match self.criteria.clone() {
                Some(c) => self.replacement_candidates(snap, &c),
                None => Vec::new(),
            },
        };
        if candidates == prompt.candidates {
            return None;
        }
        match (prompt.source, candidates.as_slice()) {
            (_, []) => {
                if let Some(criteria) = self.criteria.clone() {
                    let reason = self.unavailable_reason(snap, &criteria);
                    self.set_unavailable(reason)
                } else {
                    self.set_status(TargetStatus::NoSelection);
                    Some("No candidate targets remain".into())
                }
            }
            (PromptSource::Replacement, [only])
                if policy.on_target_lost == TargetLostPolicy::AutoRebind =>
            {
                let mode = self
                    .criteria
                    .as_ref()
                    .map_or(policy.preferred_mode, |c| c.mode);
                self.bind(snap, only, mode);
                Some(format!("Rebound to {}", only.describe()))
            }
            (PromptSource::Eligible, [only]) if policy.auto_select_single => {
                self.bind(snap, only, policy.preferred_mode);
                Some(format!(
                    "Selected {} (the only eligible target)",
                    only.describe()
                ))
            }
            _ => {
                self.set_status(TargetStatus::AwaitingConfirmation(Prompt {
                    candidates,
                    ..prompt
                }));
                None
            }
        }
    }

    fn replacement_candidates(
        &self,
        snap: &TargetSnapshot,
        criteria: &TargetCriteria,
    ) -> Vec<Candidate> {
        match criteria.mode {
            SelectionMode::Window => snap.candidates_where(|p, w| {
                p.identity.same_app(&criteria.identity)
                    && criteria.window_title.as_deref() == Some(w.title.as_str())
            }),
            SelectionMode::Process => {
                match self.last_process.filter(|k| snap.process(*k).is_some()) {
                    Some(alive) => snap.candidates_where(|p, _| p.key == alive),
                    None => snap.candidates_where(|p, _| p.identity.same_app(&criteria.identity)),
                }
            }
        }
    }

    fn unavailable_reason(&self, snap: &TargetSnapshot, criteria: &TargetCriteria) -> String {
        let app = &criteria.identity.name;
        match criteria.mode {
            SelectionMode::Window => format!(
                "Window “{}” of {app} is closed; waiting for it to reappear",
                criteria.window_title.as_deref().unwrap_or("?")
            ),
            SelectionMode::Process
                if self.last_process.is_some_and(|k| snap.process(k).is_some()) =>
            {
                format!("{app} has no open windows")
            }
            SelectionMode::Process => format!("{app} is not running; waiting for it to restart"),
        }
    }
}

/// Eligible windows if there are any, otherwise every routable window.
fn explicit_candidates(snap: &TargetSnapshot, eligible: &[AppMatcher]) -> Vec<Candidate> {
    let candidates = snap.eligible_candidates(eligible);
    if candidates.is_empty() {
        snap.candidates_where(|_, _| true)
    } else {
        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PPT: &str = r"C:\Office\POWERPNT.EXE";
    const ACRO: &str = r"C:\Adobe\Acrobat.exe";
    const NOTEPAD: &str = r"C:\Windows\notepad.exe";

    fn identity(exe: &str) -> AppIdentity {
        AppIdentity {
            name: file_name(exe)
                .trim_end_matches(".EXE")
                .trim_end_matches(".exe")
                .into(),
            process_name: file_name(exe).into(),
            executable: Some(exe.into()),
            bundle_id: None,
        }
    }

    /// Small mutable world for building snapshots.
    #[derive(Default)]
    struct World {
        processes: Vec<ProcessInfo>,
        windows: Vec<WindowInfo>,
        next_instance: u64,
        next_window: u64,
    }

    impl World {
        fn spawn(&mut self, pid: u32, exe: &str) -> ProcessKey {
            self.next_instance += 1;
            let key = ProcessKey {
                pid,
                instance: self.next_instance,
            };
            self.processes.push(ProcessInfo {
                key,
                identity: identity(exe),
            });
            key
        }

        fn open(&mut self, process: ProcessKey, title: &str) -> WindowId {
            self.next_window += 1;
            let id = WindowId(self.next_window);
            self.windows.push(WindowInfo {
                id,
                process,
                title: title.into(),
                visible: true,
            });
            id
        }

        fn close(&mut self, id: WindowId) {
            self.windows.retain(|w| w.id != id);
        }

        fn kill(&mut self, key: ProcessKey) {
            self.processes.retain(|p| p.key != key);
            self.windows.retain(|w| w.process != key);
        }

        fn snap(&self) -> TargetSnapshot {
            TargetSnapshot {
                origin: TargetOrigin::Simulated,
                processes: self.processes.clone(),
                windows: self.windows.clone(),
            }
        }
    }

    fn policy() -> TargetPolicy {
        TargetPolicy {
            auto_select_single: true,
            ..TargetPolicy::default()
        }
    }

    fn eligible() -> Vec<AppMatcher> {
        AppMatcher::presentation_defaults()
    }

    fn reconcile(
        sel: &mut TargetSelection,
        world: &World,
        policy: &TargetPolicy,
    ) -> Option<String> {
        sel.reconcile(&world.snap(), &eligible(), policy)
    }

    #[test]
    fn matcher_uses_executable_file_name_or_bundle() {
        let m = &eligible()[0];
        assert!(m.matches(&identity(PPT)));
        assert!(!m.matches(&identity(NOTEPAD)));
        let mac = AppIdentity {
            name: "PowerPoint".into(),
            process_name: "Microsoft PowerPoint".into(),
            executable: None,
            bundle_id: Some("com.microsoft.Powerpoint".into()),
        };
        assert!(m.matches(&mac));
    }

    #[test]
    fn same_app_requires_stable_identifiers() {
        let a = identity(PPT);
        assert!(a.same_app(&identity(PPT)));
        assert!(!a.same_app(&identity(ACRO)));
        let unstable = AppIdentity {
            name: "X".into(),
            process_name: "x".into(),
            executable: None,
            bundle_id: None,
        };
        assert!(!unstable.same_app(&unstable.clone()));
    }

    #[test]
    fn single_eligible_target_is_auto_selected() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let win = w.open(ppt, "Deck.pptx");
        let notepad = w.spawn(11, NOTEPAD);
        w.open(notepad, "notes.txt");
        let mut sel = TargetSelection::new(None);
        assert!(reconcile(&mut sel, &w, &policy()).is_some());
        assert_eq!(sel.binding().map(|b| b.window), Some(win));
    }

    #[test]
    fn auto_select_can_be_disabled() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        w.open(ppt, "Deck.pptx");
        let mut sel = TargetSelection::new(None);
        let p = TargetPolicy::default();
        assert!(reconcile(&mut sel, &w, &p).is_none());
        assert_eq!(sel.status(), &TargetStatus::NoSelection);
    }

    #[test]
    fn multiple_eligible_targets_require_confirmation() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let acro = w.spawn(20, ACRO);
        w.open(acro, "Handout.pdf");
        let mut sel = TargetSelection::new(None);
        reconcile(&mut sel, &w, &policy());
        let prompt = sel.prompt().expect("prompt");
        assert_eq!(prompt.candidates.len(), 2);
        assert_eq!(sel.binding(), None);

        sel.confirm(&w.snap(), deck, SelectionMode::Window).unwrap();
        assert_eq!(sel.binding().map(|b| b.window), Some(deck));
        // The choice is remembered: further reconciles do not ask again.
        assert!(reconcile(&mut sel, &w, &policy()).is_none());
        assert!(sel.prompt().is_none());
    }

    #[test]
    fn window_closure_never_redirects_to_unrelated_app() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let acro = w.spawn(20, ACRO);
        w.open(acro, "Handout.pdf");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();

        w.close(deck);
        reconcile(&mut sel, &w, &policy());
        assert!(matches!(sel.status(), TargetStatus::Unavailable(_)));
        assert_eq!(sel.criteria().unwrap().identity, identity(PPT));
    }

    #[test]
    fn window_mode_rebinds_to_reopened_window_with_same_title() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        w.open(ppt, "Other.pptx");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();

        w.close(deck);
        reconcile(&mut sel, &w, &policy());
        assert!(
            matches!(sel.status(), TargetStatus::Unavailable(_)),
            "a window with a different title is not a replacement"
        );

        let reopened = w.open(ppt, "Deck.pptx");
        let note = reconcile(&mut sel, &w, &policy());
        assert!(note.unwrap().starts_with("Rebound"));
        assert_eq!(sel.binding().map(|b| b.window), Some(reopened));
    }

    #[test]
    fn ambiguous_replacement_requires_confirmation() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();

        w.close(deck);
        w.open(ppt, "Deck.pptx");
        let second = w.spawn(11, PPT);
        w.open(second, "Deck.pptx");
        reconcile(&mut sel, &w, &policy());
        let prompt = sel.prompt().expect("prompt");
        assert_eq!(prompt.source, PromptSource::Replacement);
        assert_eq!(prompt.candidates.len(), 2);
    }

    #[test]
    fn ask_policy_confirms_even_unambiguous_replacements() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();
        let p = TargetPolicy {
            on_target_lost: TargetLostPolicy::Ask,
            ..policy()
        };
        w.close(deck);
        let reopened = w.open(ppt, "Deck.pptx");
        reconcile(&mut sel, &w, &p);
        assert_eq!(sel.prompt().unwrap().candidates.len(), 1);
        sel.confirm(&w.snap(), reopened, SelectionMode::Process)
            .unwrap();
        // A replacement keeps the original selection mode.
        assert_eq!(sel.criteria().unwrap().mode, SelectionMode::Window);
    }

    #[test]
    fn process_mode_moves_to_another_window_of_the_same_process() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let other = w.open(ppt, "Other.pptx");
        let second = w.spawn(11, PPT);
        w.open(second, "Second instance.pptx");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Process)
            .unwrap();

        w.close(deck);
        reconcile(&mut sel, &w, &policy());
        assert_eq!(sel.binding().map(|b| b.window), Some(other));
    }

    #[test]
    fn process_mode_rebinds_after_restart_using_stable_identity() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Process)
            .unwrap();

        w.kill(ppt);
        reconcile(&mut sel, &w, &policy());
        match sel.status() {
            TargetStatus::Unavailable(r) => assert!(r.contains("not running"), "{r}"),
            other => panic!("unexpected {other:?}"),
        }

        // An unrelated app reusing the same PID is not a match.
        let impostor = w.spawn(10, NOTEPAD);
        w.open(impostor, "Deck.pptx");
        reconcile(&mut sel, &w, &policy());
        assert!(matches!(sel.status(), TargetStatus::Unavailable(_)));

        let restarted = w.spawn(42, PPT);
        let new_win = w.open(restarted, "Deck.pptx");
        reconcile(&mut sel, &w, &policy());
        assert_eq!(
            sel.binding(),
            Some(Binding {
                process: restarted,
                window: new_win
            })
        );
    }

    #[test]
    fn same_pid_new_instance_is_a_different_process() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();
        let generation = sel.generation();

        // Same PID and window title, but a new process lifetime.
        w.kill(ppt);
        let again = w.spawn(10, PPT);
        let win = w.open(again, "Deck.pptx");
        reconcile(&mut sel, &w, &policy());
        assert_eq!(sel.binding().map(|b| b.process), Some(again));
        assert_eq!(sel.binding().map(|b| b.window), Some(win));
        assert!(sel.generation() > generation);
    }

    #[test]
    fn unstable_identity_is_never_rebound() {
        let mut w = World::default();
        let key = ProcessKey {
            pid: 5,
            instance: 1,
        };
        let mystery = ProcessInfo {
            key,
            identity: AppIdentity {
                name: "Mystery".into(),
                process_name: "mystery".into(),
                executable: None,
                bundle_id: None,
            },
        };
        w.processes.push(mystery.clone());
        let win = w.open(key, "Slides");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), win, SelectionMode::Window)
            .unwrap();
        w.close(win);
        let other = ProcessKey {
            pid: 6,
            instance: 2,
        };
        w.processes.push(ProcessInfo {
            key: other,
            ..mystery
        });
        w.open(other, "Slides");
        reconcile(&mut sel, &w, &policy());
        assert!(matches!(sel.status(), TargetStatus::Unavailable(_)));
    }

    #[test]
    fn selecting_a_process_with_several_windows_asks_for_one() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        w.open(ppt, "A.pptx");
        let b = w.open(ppt, "B.pptx");
        let mut sel = TargetSelection::new(None);
        sel.select_process(&w.snap(), ppt).unwrap();
        assert_eq!(sel.prompt().unwrap().candidates.len(), 2);
        sel.confirm(&w.snap(), b, SelectionMode::Window).unwrap();
        assert_eq!(sel.criteria().unwrap().mode, SelectionMode::Process);
        assert_eq!(sel.binding().map(|x| x.window), Some(b));
    }

    #[test]
    fn dismiss_restores_previous_target() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let acro = w.spawn(20, ACRO);
        w.open(acro, "Handout.pdf");
        let mut sel = TargetSelection::new(None);
        sel.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();
        sel.request_change(&w.snap(), &eligible()).unwrap();
        assert!(sel.binding().is_none());
        sel.dismiss(&w.snap(), true);
        assert_eq!(sel.binding().map(|b| b.window), Some(deck));

        sel.request_change(&w.snap(), &eligible()).unwrap();
        sel.dismiss(&w.snap(), false);
        assert!(matches!(sel.status(), TargetStatus::Unavailable(_)));
        // A dismissed prompt is not reopened automatically for the same situation.
        reconcile(&mut sel, &w, &policy());
        assert!(sel.prompt().is_none());
    }

    #[test]
    fn multiple_targets_policy() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let mut keep = TargetSelection::new(None);
        let mut ask = TargetSelection::new(None);
        keep.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();
        ask.select_window(&w.snap(), deck, SelectionMode::Window)
            .unwrap();
        let ask_policy = TargetPolicy {
            on_multiple: MultipleTargetsPolicy::Ask,
            ..policy()
        };
        reconcile(&mut keep, &w, &policy());
        reconcile(&mut ask, &w, &ask_policy);

        let acro = w.spawn(20, ACRO);
        w.open(acro, "Handout.pdf");
        reconcile(&mut keep, &w, &policy());
        reconcile(&mut ask, &w, &ask_policy);
        assert_eq!(keep.binding().map(|b| b.window), Some(deck));
        assert!(ask.prompt().is_some());
    }

    #[test]
    fn clear_suppresses_auto_selection_until_something_new_appears() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        w.open(ppt, "Deck.pptx");
        let mut sel = TargetSelection::new(None);
        reconcile(&mut sel, &w, &policy());
        assert!(sel.binding().is_some());
        sel.clear();
        reconcile(&mut sel, &w, &policy());
        assert_eq!(sel.status(), &TargetStatus::NoSelection);

        let acro = w.spawn(20, ACRO);
        w.open(acro, "Handout.pdf");
        reconcile(&mut sel, &w, &policy());
        assert!(sel.prompt().is_some());
    }

    #[test]
    fn saved_criteria_bind_on_startup() {
        let mut w = World::default();
        let ppt = w.spawn(77, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let acro = w.spawn(20, ACRO);
        w.open(acro, "Handout.pdf");
        let mut sel = TargetSelection::new(Some(TargetCriteria {
            mode: SelectionMode::Window,
            identity: identity(PPT),
            window_title: Some("Deck.pptx".into()),
        }));
        reconcile(&mut sel, &w, &policy());
        assert_eq!(sel.binding().map(|b| b.window), Some(deck));
    }

    #[test]
    fn prompt_candidates_follow_lifecycle_changes() {
        let mut w = World::default();
        let ppt = w.spawn(10, PPT);
        let deck = w.open(ppt, "Deck.pptx");
        let acro = w.spawn(20, ACRO);
        let pdf = w.open(acro, "Handout.pdf");
        let mut sel = TargetSelection::new(None);
        reconcile(&mut sel, &w, &policy());
        assert_eq!(sel.prompt().unwrap().candidates.len(), 2);

        w.close(pdf);
        reconcile(&mut sel, &w, &policy());
        assert_eq!(sel.binding().map(|b| b.window), Some(deck));
    }
}
