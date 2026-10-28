//! Platform capability reporting.
//!
//! These functions only describe what the native adapters can do on the
//! current operating system. They never open devices or change system state.
//! Each OS module documents the APIs a future adapter is expected to use.

use crate::core::capabilities::Capability;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as imp;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod imp {
    use crate::core::capabilities::{Capability, CapabilityStatus};

    pub fn environment() -> String {
        std::env::consts::OS.to_owned()
    }

    fn unsupported(component: &str) -> Capability {
        Capability::new(
            component,
            CapabilityStatus::Unsupported,
            "This operating system is not a RustClick target",
        )
    }

    pub fn input_capabilities() -> Vec<Capability> {
        vec![unsupported("Native input")]
    }

    pub fn target_capabilities() -> Vec<Capability> {
        vec![unsupported("Native targets")]
    }

    pub fn shortcut_capability() -> Capability {
        unsupported("Global target-change shortcut")
    }
}

/// Human-readable operating system name.
pub fn os_label() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    }
}

/// OS plus session details, e.g. "Linux — Wayland session (KDE)".
pub fn environment() -> String {
    imp::environment()
}

/// Device identification, capture and suppression.
pub fn input_capabilities() -> Vec<Capability> {
    imp::input_capabilities()
}

/// Window discovery, lifecycle monitoring and background delivery.
pub fn target_capabilities() -> Vec<Capability> {
    imp::target_capabilities()
}

pub fn shortcut_capability() -> Capability {
    imp::shortcut_capability()
}
