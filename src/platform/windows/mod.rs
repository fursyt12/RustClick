//! Windows adapter and capability report.
//!
//! * Input: Raw Input identifies the source device of each keystroke
//!   ([`raw_input`], [`input`]). Suppressing the original keystroke is planned
//!   through Logitech HID++ button diversion ([`hidpp`]); a low-level keyboard
//!   hook cannot tell which device a key came from.
//! * Targets: `EnumWindows` for discovery, polling for lifecycle, and posted
//!   key messages for delivery ([`desktop`], [`targets`]).
//!
//! Everything is marked experimental until it has been verified on real
//! hardware with `rustclick-diag` (see `docs/windows-testing.md`).

pub mod desktop;
pub mod hidpp;
pub mod input;
pub mod keys;
pub mod raw_input;
pub mod targets;

use crate::core::capabilities::{Capability, CapabilityStatus::*};

/// NUL-terminated UTF-16 for Win32 calls.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 up to the first NUL.
pub(crate) fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

pub fn environment() -> String {
    "Windows".to_owned()
}

pub fn input_capabilities() -> Vec<Capability> {
    vec![
        Capability::new(
            "Device identification (Raw Input)",
            Experimental,
            "Logitech keyboard and consumer-control collections (vendor 046D), grouped by product id. No elevation needed.",
        ),
        Capability::new(
            "Input capture",
            Experimental,
            "WM_INPUT with RIDEV_INPUTSINK on a message-only window; works while RustClick is in the background.",
        ),
        Capability::new(
            "Input suppression",
            NotImplemented,
            "The remote's original keystroke still reaches the focused window. Planned: Logitech HID++ button diversion for R500s and Spotlight.",
        )
        .optional(),
    ]
}

pub fn target_capabilities() -> Vec<Capability> {
    vec![
        Capability::new(
            "Window discovery",
            Experimental,
            "EnumWindows, GetWindowThreadProcessId, QueryFullProcessImageNameW, GetProcessTimes (process start time).",
        ),
        Capability::new(
            "Lifecycle monitoring",
            Experimental,
            "Polls the window list once per second.",
        ),
        Capability::new(
            "Background delivery",
            Experimental,
            "Posts WM_KEYDOWN/WM_KEYUP to the target's focused child window (PowerPoint: the slide show window if one is open). Whether the application reacts is not verified; UIPI blocks elevated targets.",
        ),
    ]
}

pub fn shortcut_capability() -> Capability {
    Capability::new(
        "Global target-change shortcut",
        NotImplemented,
        "Planned: RegisterHotKey.",
    )
    .optional()
}
