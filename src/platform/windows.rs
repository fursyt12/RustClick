//! Windows capability report.
//!
//! Planned adapter: Raw Input for device identification; a low-level keyboard
//! hook for suppression (which cannot itself identify the source device, so
//! it must be correlated with Raw Input); EnumWindows / WinEvent hooks for
//! discovery and lifecycle.

use crate::core::capabilities::{Capability, CapabilityStatus::*};

pub fn environment() -> String {
    "Windows".to_owned()
}

pub fn input_capabilities() -> Vec<Capability> {
    vec![
        Capability::new(
            "Device identification (Raw Input)",
            NotImplemented,
            "Planned: RegisterRawInputDevices with RIDEV_INPUTSINK and GetRawInputDeviceInfo for HID vendor/product ids.",
        ),
        Capability::new(
            "Input capture",
            NotImplemented,
            "Planned: WM_INPUT on a message-only window.",
        ),
        Capability::new(
            "Input suppression",
            NotImplemented,
            "Raw Input cannot block keystrokes. A WH_KEYBOARD_LL hook can, but cannot tell which device a key came from; correlating both is timing-sensitive and must be validated on hardware.",
        ),
    ]
}

pub fn target_capabilities() -> Vec<Capability> {
    vec![
        Capability::new(
            "Window discovery",
            NotImplemented,
            "Planned: EnumWindows, GetWindowThreadProcessId, QueryFullProcessImageNameW.",
        ),
        Capability::new(
            "Lifecycle monitoring",
            NotImplemented,
            "Planned: SetWinEventHook for EVENT_OBJECT_CREATE / EVENT_OBJECT_DESTROY.",
        ),
        Capability::new(
            "Background delivery",
            NotImplemented,
            "PostMessage(WM_KEYDOWN) works only for some applications; PowerPoint COM automation is an alternative. Behaviour must be verified per application.",
        ),
    ]
}

pub fn shortcut_capability() -> Capability {
    Capability::new(
        "Global target-change shortcut",
        NotImplemented,
        "Planned: RegisterHotKey.",
    )
}
