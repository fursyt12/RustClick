//! Explicit capability reporting for providers and platform adapters.

use std::fmt;

/// Implementation state of one capability of a provider or adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    Supported,
    Unsupported,
    PermissionRequired,
    NotImplemented,
    TemporarilyUnavailable,
    Error,
}

impl CapabilityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Supported => "Supported",
            Self::Unsupported => "Unsupported",
            Self::PermissionRequired => "Permission required",
            Self::NotImplemented => "Not implemented",
            Self::TemporarilyUnavailable => "Temporarily unavailable",
            Self::Error => "Error",
        }
    }

    pub fn is_operational(self) -> bool {
        self == Self::Supported
    }
}

impl fmt::Display for CapabilityStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One line of a capability report, e.g. "Input suppression: Not implemented".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub component: String,
    pub status: CapabilityStatus,
    pub detail: String,
}

impl Capability {
    pub fn new(
        component: impl Into<String>,
        status: CapabilityStatus,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            component: component.into(),
            status,
            detail: detail.into(),
        }
    }
}
