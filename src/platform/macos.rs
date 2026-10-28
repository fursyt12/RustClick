//! macOS capability report.
//!
//! Planned adapter: IOHIDManager for device identification and capture
//! (Input Monitoring permission); a CGEventTap for suppression (Accessibility
//! permission); NSWorkspace and CGWindowList for discovery.

use crate::core::capabilities::{Capability, CapabilityStatus::*};

pub fn environment() -> String {
    "macOS".to_owned()
}

pub fn input_capabilities() -> Vec<Capability> {
    vec![
        Capability::new(
            "Device identification (IOHIDManager)",
            NotImplemented,
            "Planned: match HID vendor/product ids. Requires the Input Monitoring permission.",
        ),
        Capability::new(
            "Input capture",
            NotImplemented,
            "Planned: IOHIDManager input value callbacks. Requires Input Monitoring.",
        ),
        Capability::new(
            "Input suppression",
            NotImplemented,
            "Planned: CGEventTap. Requires the Accessibility permission; event taps cannot identify the source device on their own.",
        ),
    ]
}

pub fn target_capabilities() -> Vec<Capability> {
    vec![
        Capability::new(
            "Window discovery",
            NotImplemented,
            "Planned: NSWorkspace running applications (bundle ids) and CGWindowListCopyWindowInfo. Window titles require the Screen Recording permission.",
        ),
        Capability::new(
            "Lifecycle monitoring",
            NotImplemented,
            "Planned: NSWorkspace launch/terminate notifications and Accessibility observers.",
        ),
        Capability::new(
            "Background delivery",
            NotImplemented,
            "Planned: CGEventPostToPid or AppleScript. Requires Accessibility; behaviour is application-dependent.",
        ),
    ]
}

pub fn shortcut_capability() -> Capability {
    Capability::new(
        "Global target-change shortcut",
        NotImplemented,
        "Planned: Carbon RegisterEventHotKey.",
    )
}
