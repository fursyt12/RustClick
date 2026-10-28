//! Linux capability report.
//!
//! Planned adapter: evdev for device identification, capture and exclusive
//! grabs (works under X11 and Wayland because it bypasses the display
//! server); EWMH/XRes for window discovery under X11. Wayland offers no
//! portable way to enumerate or address other clients' windows.

use crate::core::capabilities::{Capability, CapabilityStatus::*};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    X11,
    Wayland,
    Unknown,
}

fn session() -> Session {
    let kind = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    if kind == "wayland" || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Session::Wayland
    } else if kind == "x11" || std::env::var_os("DISPLAY").is_some() {
        Session::X11
    } else {
        Session::Unknown
    }
}

pub fn environment() -> String {
    let session = match session() {
        Session::X11 => "X11 session",
        Session::Wayland => "Wayland session",
        Session::Unknown => "no graphical session detected",
    };
    match std::env::var("XDG_CURRENT_DESKTOP") {
        Ok(desktop) if !desktop.is_empty() => format!("Linux — {session} ({desktop})"),
        _ => format!("Linux — {session}"),
    }
}

/// Group id of `input` from /etc/group.
fn input_gid() -> Option<u32> {
    let groups = std::fs::read_to_string("/etc/group").ok()?;
    groups.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == "input").then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

/// Supplementary groups of this process from /proc/self/status.
fn current_groups() -> Option<Vec<u32>> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Groups:"))?;
    Some(
        line["Groups:".len()..]
            .split_whitespace()
            .filter_map(|g| g.parse().ok())
            .collect(),
    )
}

fn input_permission_note() -> &'static str {
    match (input_gid(), current_groups()) {
        (Some(gid), Some(groups)) if groups.contains(&gid) => {
            "This user is in the `input` group, so /dev/input/event* should be readable."
        }
        (Some(_), Some(_)) => {
            "This user is not in the `input` group; reading /dev/input/event* will need that group or a udev rule for the remote."
        }
        _ => "Could not determine access to /dev/input/event*.",
    }
}

pub fn input_capabilities() -> Vec<Capability> {
    vec![
        Capability::new(
            "Device identification (evdev/udev)",
            NotImplemented,
            format!(
                "Planned: enumerate /dev/input/event* and match USB/Bluetooth vendor and product ids. {}",
                input_permission_note()
            ),
        ),
        Capability::new(
            "Input capture",
            NotImplemented,
            "Planned: read evdev events per device. Independent of X11/Wayland.",
        ),
        Capability::new(
            "Input suppression",
            NotImplemented,
            "Planned: EVIOCGRAB exclusive grab of the remote's event node so its keystrokes do not reach the focused application.",
        ),
    ]
}

pub fn target_capabilities() -> Vec<Capability> {
    match session() {
        Session::X11 => vec![
            Capability::new(
                "Window discovery",
                NotImplemented,
                "Planned: _NET_CLIENT_LIST, _NET_WM_PID and /proc/<pid>/exe.",
            ),
            Capability::new(
                "Lifecycle monitoring",
                NotImplemented,
                "Planned: PropertyNotify on the root window's _NET_CLIENT_LIST.",
            ),
            Capability::new(
                "Background delivery",
                NotImplemented,
                "XSendEvent is ignored by many toolkits and XTEST only reaches the focused window; per-application verification is required.",
            ),
        ],
        Session::Wayland => vec![
            Capability::new(
                "Window discovery",
                Unsupported,
                "Wayland does not let regular clients enumerate other applications' windows. Only compositor-specific protocols or extensions can provide this.",
            ),
            Capability::new(
                "Lifecycle monitoring",
                Unsupported,
                "Not available to regular Wayland clients.",
            ),
            Capability::new(
                "Background delivery",
                Unsupported,
                "Wayland has no standard protocol to send input to an unfocused window; uinput injection reaches only the focused window.",
            ),
        ],
        Session::Unknown => vec![Capability::new(
            "Window discovery",
            TemporarilyUnavailable,
            "No X11 or Wayland session detected.",
        )],
    }
}

pub fn shortcut_capability() -> Capability {
    let detail = match session() {
        Session::X11 => "Planned: XGrabKey on the root window.",
        Session::Wayland => {
            "Requires the xdg-desktop-portal GlobalShortcuts interface, which depends on the desktop environment."
        }
        Session::Unknown => "No graphical session detected.",
    };
    Capability::new("Global target-change shortcut", NotImplemented, detail)
}
