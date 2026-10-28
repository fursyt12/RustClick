//! Platform-independent routing decisions.
//!
//! The router only decides *whether* and *where* an event should go. How an
//! action is delivered is the job of the target provider, and delivery
//! outcomes are reported separately so that a successful decision is never
//! mistaken for a successful native delivery.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use super::devices::{DeviceRegistry, SourceRejection};
use super::input::{ButtonId, DeviceId, EventKind, EventOrigin, InputEvent};
use super::mappings::{Action, Mappings};
use super::targets::{
    ProcessKey, TargetOrigin, TargetSelection, TargetSnapshot, TargetStatus, WindowId,
};

/// Events older than this when processed are considered stale.
pub const DEFAULT_MAX_EVENT_AGE: Duration = Duration::from_secs(2);

/// The resolved destination of a routed event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTarget {
    pub window: WindowId,
    pub process: ProcessKey,
    pub app_name: String,
    pub title: String,
    pub origin: TargetOrigin,
    /// Target selection generation at decision time.
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    Duplicate { last_sequence: u64 },
    Source(SourceRejection),
    RoutingStopped,
    Stale,
    Unmapped(ButtonId),
    NoTarget,
    TargetUnavailable(String),
    TargetAmbiguous,
    TargetGone,
    SimulatedToNative,
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate { last_sequence } => {
                write!(
                    f,
                    "duplicate event (already processed up to #{last_sequence})"
                )
            }
            Self::Source(s) => write!(f, "{s}"),
            Self::RoutingStopped => f.write_str("routing is stopped"),
            Self::Stale => {
                f.write_str("stale event (captured before the current target or too old)")
            }
            Self::Unmapped(b) => write!(f, "no action is mapped to `{b}`"),
            Self::NoTarget => f.write_str("no target is selected"),
            Self::TargetUnavailable(r) => write!(f, "target unavailable: {r}"),
            Self::TargetAmbiguous => f.write_str("target is ambiguous; waiting for confirmation"),
            Self::TargetGone => f.write_str("selected window is no longer available"),
            Self::SimulatedToNative => {
                f.write_str("simulated events are never sent to native applications")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Route {
        action: Action,
        target: RouteTarget,
    },
    /// Valid event that intentionally produces no action.
    Ignore(&'static str),
    Reject(RejectReason),
}

/// Everything the router needs to decide about one event.
pub struct RouteContext<'a> {
    pub routing_enabled: bool,
    pub devices: &'a DeviceRegistry,
    pub mappings: &'a Mappings,
    pub selection: &'a TargetSelection,
    pub snapshot: &'a TargetSnapshot,
    pub now: Instant,
}

#[derive(Debug)]
pub struct Router {
    last_sequence: HashMap<DeviceId, u64>,
    max_event_age: Duration,
}

impl Default for Router {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_EVENT_AGE)
    }
}

impl Router {
    pub fn new(max_event_age: Duration) -> Self {
        Self {
            last_sequence: HashMap::new(),
            max_event_age,
        }
    }

    /// Forgets duplicate-tracking state for a removed device.
    pub fn forget_device(&mut self, id: &DeviceId) {
        self.last_sequence.remove(id);
    }

    pub fn route(&mut self, event: &InputEvent, ctx: &RouteContext<'_>) -> Decision {
        // Duplicate detection runs first and records every new sequence
        // number, so a replayed event is never routed even if the original
        // was rejected for another reason.
        if let Some(&last) = self.last_sequence.get(&event.device_id)
            && event.sequence <= last
        {
            return Decision::Reject(RejectReason::Duplicate {
                last_sequence: last,
            });
        }
        self.last_sequence
            .insert(event.device_id.clone(), event.sequence);

        if let Err(e) = ctx.devices.check_source(event) {
            return Decision::Reject(RejectReason::Source(e));
        }
        if event.kind == EventKind::Release {
            return Decision::Ignore("release events are not routed");
        }
        if !ctx.routing_enabled {
            return Decision::Reject(RejectReason::RoutingStopped);
        }
        if event.captured_at < ctx.selection.changed_at()
            || ctx.now.saturating_duration_since(event.captured_at) > self.max_event_age
        {
            return Decision::Reject(RejectReason::Stale);
        }
        let Some(action) = ctx.mappings.resolve(&event.button) else {
            return Decision::Reject(RejectReason::Unmapped(event.button.clone()));
        };
        let binding = match ctx.selection.status() {
            TargetStatus::NoSelection => return Decision::Reject(RejectReason::NoTarget),
            TargetStatus::Unavailable(r) => {
                return Decision::Reject(RejectReason::TargetUnavailable(r.clone()));
            }
            TargetStatus::AwaitingConfirmation(_) => {
                return Decision::Reject(RejectReason::TargetAmbiguous);
            }
            TargetStatus::Bound(b) => *b,
        };
        let Some(candidate) = ctx
            .snapshot
            .candidate(binding.window)
            .filter(|c| c.process == binding.process)
        else {
            return Decision::Reject(RejectReason::TargetGone);
        };
        if event.origin == EventOrigin::Simulated && ctx.snapshot.origin == TargetOrigin::Native {
            return Decision::Reject(RejectReason::SimulatedToNative);
        }
        Decision::Route {
            action: action.clone(),
            target: RouteTarget {
                window: candidate.window,
                process: candidate.process,
                app_name: candidate.app_name,
                title: candidate.title,
                origin: ctx.snapshot.origin,
                generation: ctx.selection.generation(),
            },
        }
    }
}

/// Result of handing a routed action to a target provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Delivered to a mock target. Nothing left the process.
    Simulated,
    /// Delivered to a native application.
    Delivered,
    /// The provider cannot deliver actions (e.g. not implemented).
    Unsupported(String),
    /// Delivery was attempted and failed; the target may have disappeared.
    Failed(String),
}

impl fmt::Display for DeliveryOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Simulated => f.write_str("simulated delivery to mock target"),
            Self::Delivered => f.write_str("delivered to native application"),
            Self::Unsupported(r) => write!(f, "delivery unsupported: {r}"),
            Self::Failed(r) => write!(f, "delivery failed: {r}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::core::devices::{DeviceInfo, DeviceKind};
    use crate::core::targets::{AppIdentity, ProcessInfo, SelectionMode, WindowInfo};

    struct Fixture {
        devices: DeviceRegistry,
        mappings: Mappings,
        selection: TargetSelection,
        snapshot: TargetSnapshot,
        router: Router,
    }

    const PROC: ProcessKey = ProcessKey {
        pid: 1,
        instance: 1,
    };
    const WIN: WindowId = WindowId(7);

    impl Fixture {
        fn new() -> Self {
            let mut devices = DeviceRegistry::default();
            devices.upsert(
                DeviceInfo {
                    id: DeviceId::new("d"),
                    name: "D".into(),
                    kind: DeviceKind::Simulated,
                    connected: true,
                    buttons: ["forward", "back", "aux"].map(ButtonId::standard).to_vec(),
                    details: String::new(),
                },
                None,
            );
            let snapshot = TargetSnapshot {
                origin: TargetOrigin::Simulated,
                processes: vec![ProcessInfo {
                    key: PROC,
                    identity: AppIdentity {
                        name: "App".into(),
                        process_name: "app".into(),
                        executable: Some("/app".into()),
                        bundle_id: None,
                    },
                }],
                windows: vec![WindowInfo {
                    id: WIN,
                    process: PROC,
                    title: "Deck".into(),
                    visible: true,
                }],
            };
            let mut selection = TargetSelection::new(None);
            selection
                .select_window(&snapshot, WIN, SelectionMode::Window)
                .unwrap();
            Self {
                devices,
                mappings: Mappings::default(),
                selection,
                snapshot,
                router: Router::default(),
            }
        }

        fn route(&mut self, event: &InputEvent) -> Decision {
            let ctx = RouteContext {
                routing_enabled: true,
                devices: &self.devices,
                mappings: &self.mappings,
                selection: &self.selection,
                snapshot: &self.snapshot,
                now: Instant::now(),
            };
            self.router.route(event, &ctx)
        }
    }

    fn event(seq: u64, button: &str, kind: EventKind) -> InputEvent {
        InputEvent {
            device_id: DeviceId::new("d"),
            device_name: "D".into(),
            sequence: seq,
            button: ButtonId::standard(button),
            kind,
            origin: EventOrigin::Simulated,
            timestamp: SystemTime::now(),
            captured_at: Instant::now(),
            metadata: vec![],
        }
    }

    #[test]
    fn routes_mapped_press_to_bound_target() {
        let mut f = Fixture::new();
        match f.route(&event(1, "forward", EventKind::Press)) {
            Decision::Route { action, target } => {
                assert_eq!(action, Action::Next);
                assert_eq!(target.window, WIN);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn release_is_ignored_not_routed() {
        let mut f = Fixture::new();
        assert!(matches!(
            f.route(&event(1, "forward", EventKind::Release)),
            Decision::Ignore(_)
        ));
    }

    #[test]
    fn duplicates_are_rejected() {
        let mut f = Fixture::new();
        let e = event(5, "forward", EventKind::Click);
        assert!(matches!(f.route(&e), Decision::Route { .. }));
        assert_eq!(
            f.route(&e),
            Decision::Reject(RejectReason::Duplicate { last_sequence: 5 })
        );
        assert!(matches!(
            f.route(&event(4, "forward", EventKind::Click)),
            Decision::Reject(RejectReason::Duplicate { .. })
        ));
        assert!(matches!(
            f.route(&event(6, "forward", EventKind::Click)),
            Decision::Route { .. }
        ));
    }

    #[test]
    fn unmapped_buttons_are_rejected() {
        let mut f = Fixture::new();
        assert_eq!(
            f.route(&event(1, "aux", EventKind::Click)),
            Decision::Reject(RejectReason::Unmapped(ButtonId::standard("aux")))
        );
    }

    #[test]
    fn disabled_device_is_rejected() {
        let mut f = Fixture::new();
        f.devices.set_enabled(&DeviceId::new("d"), false).unwrap();
        assert_eq!(
            f.route(&event(1, "forward", EventKind::Click)),
            Decision::Reject(RejectReason::Source(SourceRejection::Disabled))
        );
    }

    #[test]
    fn stopped_routing_rejects() {
        let mut f = Fixture::new();
        let ctx = RouteContext {
            routing_enabled: false,
            devices: &f.devices,
            mappings: &f.mappings,
            selection: &f.selection,
            snapshot: &f.snapshot,
            now: Instant::now(),
        };
        assert_eq!(
            f.router.route(&event(1, "forward", EventKind::Click), &ctx),
            Decision::Reject(RejectReason::RoutingStopped)
        );
    }

    #[test]
    fn events_captured_before_target_change_are_stale() {
        let mut f = Fixture::new();
        let early = event(1, "forward", EventKind::Click);
        std::thread::sleep(Duration::from_millis(2));
        f.selection.clear();
        f.selection
            .select_window(&f.snapshot, WIN, SelectionMode::Window)
            .unwrap();
        assert_eq!(f.route(&early), Decision::Reject(RejectReason::Stale));
    }

    #[test]
    fn old_events_are_stale() {
        let mut f = Fixture::new();
        f.router = Router::new(Duration::ZERO);
        let e = event(1, "forward", EventKind::Click);
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(f.route(&e), Decision::Reject(RejectReason::Stale));
    }

    #[test]
    fn missing_window_is_rejected_even_when_still_bound() {
        let mut f = Fixture::new();
        f.snapshot.windows.clear();
        assert_eq!(
            f.route(&event(1, "forward", EventKind::Click)),
            Decision::Reject(RejectReason::TargetGone)
        );
    }

    #[test]
    fn simulated_events_never_reach_native_targets() {
        let mut f = Fixture::new();
        f.snapshot.origin = TargetOrigin::Native;
        assert_eq!(
            f.route(&event(1, "forward", EventKind::Click)),
            Decision::Reject(RejectReason::SimulatedToNative)
        );
    }

    #[test]
    fn no_or_ambiguous_target_is_rejected() {
        let mut f = Fixture::new();
        f.selection.clear();
        assert_eq!(
            f.route(&event(1, "forward", EventKind::Click)),
            Decision::Reject(RejectReason::NoTarget)
        );
        f.selection.request_change(&f.snapshot, &[]).unwrap();
        assert_eq!(
            f.route(&event(2, "forward", EventKind::Click)),
            Decision::Reject(RejectReason::TargetAmbiguous)
        );
    }
}
