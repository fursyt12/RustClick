//! Windows target provider: top-level windows and posted keystrokes.
//!
//! Discovery lists visible, uncloaked, unowned top-level windows of other
//! processes. Lifecycle changes are detected by polling once per second.
//! Delivery posts WM_KEYDOWN/WM_KEYUP to the window that has focus within the
//! target's GUI thread; for PowerPoint, a running slide show window takes
//! precedence over the editor window. Posting only queues the messages, so
//! the outcome is reported as [`DeliveryOutcome::Sent`], never `Delivered`.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::core::capabilities::Capability;
use crate::core::mappings::Action;
use crate::core::routing::{DeliveryOutcome, RouteTarget};
use crate::core::targets::{
    AppIdentity, ProcessInfo, ProcessKey, TargetOrigin, TargetSnapshot, WindowId, WindowInfo,
};
use crate::provider::{ProviderError, ProviderMessage, ProviderSink};
use crate::targets::TargetProvider;

use super::desktop::{self, TopWindow};
use super::keys;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The key that performs `action` in PowerPoint and most PDF viewers.
pub fn action_key(action: &Action) -> Option<u16> {
    match action {
        Action::Next => Some(keys::VK_NEXT),
        Action::Previous => Some(keys::VK_PRIOR),
        Action::StartPresentation => Some(keys::VK_F5),
        Action::StopPresentation => Some(keys::VK_ESCAPE),
        Action::BlankScreen => Some(keys::VK_B),
        Action::Pointer | Action::Custom(_) => None,
    }
}

fn app_windows() -> Vec<TopWindow> {
    // SAFETY: returns the calling process id.
    let own = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() };
    desktop::top_level_windows()
        .into_iter()
        .filter(|w| w.pid != own && w.is_app_window())
        .collect()
}

pub fn snapshot() -> TargetSnapshot {
    let windows = app_windows();
    let mut processes: BTreeMap<u32, ProcessInfo> = BTreeMap::new();
    for w in &windows {
        processes.entry(w.pid).or_insert_with(|| {
            let details = desktop::process_details(w.pid);
            let identity = match &details.executable {
                Some(exe) => AppIdentity {
                    name: desktop::app_name(exe),
                    process_name: desktop::file_name(exe).to_owned(),
                    executable: Some(exe.clone()),
                    bundle_id: None,
                },
                None => AppIdentity {
                    name: format!("PID {}", w.pid),
                    process_name: format!("PID {}", w.pid),
                    executable: None,
                    bundle_id: None,
                },
            };
            ProcessInfo {
                key: ProcessKey {
                    pid: w.pid,
                    instance: details.created.unwrap_or_default(),
                },
                identity,
            }
        });
    }
    let windows = windows
        .iter()
        .filter_map(|w| {
            Some(WindowInfo {
                id: WindowId(w.hwnd as u64),
                process: processes.get(&w.pid)?.key,
                title: w.title.clone(),
                visible: true,
            })
        })
        .collect();
    TargetSnapshot {
        origin: TargetOrigin::Native,
        processes: processes.into_values().collect(),
        windows,
    }
}

/// What the poller compares to detect lifecycle changes.
fn fingerprint() -> Vec<(usize, u32, String)> {
    let mut f: Vec<_> = app_windows()
        .into_iter()
        .map(|w| (w.hwnd, w.pid, w.title))
        .collect();
    f.sort();
    f
}

/// Checks that the window still exists and belongs to the same process lifetime.
fn verify(hwnd: usize, process: ProcessKey) -> Result<(u32, u32), String> {
    if !desktop::is_window(hwnd) {
        return Err("the window no longer exists".into());
    }
    let (pid, thread) = desktop::window_owner(hwnd).ok_or("the window no longer exists")?;
    if pid != process.pid {
        return Err(format!("the window now belongs to PID {pid}"));
    }
    let created = desktop::process_details(pid).created.unwrap_or_default();
    if created != process.instance {
        return Err("the process was restarted".into());
    }
    Ok((pid, thread))
}

/// Picks the window that should receive the keystroke and describes the choice.
fn recipient(hwnd: usize, pid: u32, thread: u32) -> (usize, String) {
    let (top, top_desc) = match desktop::powerpoint_slideshow(pid) {
        Some(show) if show.hwnd != hwnd => {
            (show.hwnd, format!("slide show window 0x{:x}", show.hwnd))
        }
        _ => (hwnd, format!("window 0x{hwnd:x}")),
    };
    let thread = if top == hwnd {
        thread
    } else {
        desktop::window_owner(top).map_or(thread, |(_, t)| t)
    };
    match desktop::thread_focus(thread) {
        Some(focus) if focus != top && desktop::is_self_or_child(top, focus) => (
            focus,
            format!(
                "focused child 0x{focus:x} ({}) of {top_desc}",
                desktop::window_class(focus)
            ),
        ),
        _ => (top, format!("{top_desc} ({})", desktop::window_class(top))),
    }
}

struct Poller {
    stop: Sender<()>,
    join: JoinHandle<()>,
}

#[derive(Default)]
pub struct NativeTargetProvider {
    poller: Option<Poller>,
}

impl TargetProvider for NativeTargetProvider {
    fn name(&self) -> &'static str {
        "native targets"
    }

    fn origin(&self) -> TargetOrigin {
        TargetOrigin::Native
    }

    fn capabilities(&self) -> Vec<Capability> {
        super::target_capabilities()
    }

    fn notifications_active(&self) -> bool {
        self.poller.is_some()
    }

    fn start(&mut self, sink: ProviderSink) -> Result<(), ProviderError> {
        if self.poller.is_some() {
            return Ok(());
        }
        let (stop, stopped) = mpsc::channel::<()>();
        let join = std::thread::Builder::new()
            .name("rustclick-window-poller".into())
            .spawn(move || {
                let mut last = fingerprint();
                loop {
                    match stopped.recv_timeout(POLL_INTERVAL) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                    let now = fingerprint();
                    if now != last {
                        last = now;
                        // A full queue drops the notification; the next change resends.
                        let _ = sink.send(ProviderMessage::TargetsChanged);
                    }
                }
            })
            .map_err(|e| ProviderError::Unsupported(format!("window poller: {e}")))?;
        self.poller = Some(Poller { stop, join });
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(poller) = self.poller.take() {
            let _ = poller.stop.send(());
            let _ = poller.join.join();
        }
    }

    fn snapshot(&mut self) -> Result<TargetSnapshot, ProviderError> {
        Ok(snapshot())
    }

    fn deliver(&mut self, target: &RouteTarget, action: &Action) -> DeliveryOutcome {
        if target.origin != TargetOrigin::Native {
            return DeliveryOutcome::Failed("native provider received a simulated target".into());
        }
        let Some(vk) = action_key(action) else {
            return DeliveryOutcome::Unsupported(format!(
                "{action} has no keyboard equivalent yet"
            ));
        };
        let hwnd = target.window.0 as usize;
        let (pid, thread) = match verify(hwnd, target.process) {
            Ok(owner) => owner,
            Err(e) => return DeliveryOutcome::Failed(e),
        };
        let (to, description) = recipient(hwnd, pid, thread);
        let key = keys::vk_name(vk);
        tracing::info!("native delivery: posting {key} to {description}");
        match desktop::post_key(to, vk) {
            Ok(()) => DeliveryOutcome::Sent(format!("{key} posted to {description}")),
            Err(e) => DeliveryOutcome::Failed(e),
        }
    }
}

impl Drop for NativeTargetProvider {
    fn drop(&mut self) {
        self.stop();
    }
}
