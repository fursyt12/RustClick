//! Explicit capability reporting for providers and platform adapters.

use std::fmt;

/// Implementation state of one capability of a provider or adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    Supported,
    Unsupported,
    PermissionRequired,
    NotImplemented,
    /// Implemented, but not yet verified on real hardware and applications.
    Experimental,
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
            Self::Experimental => "Experimental",
            Self::TemporarilyUnavailable => "Temporarily unavailable",
            Self::Error => "Error",
        }
    }

    /// Whether the capability can be used (experimental ones included).
    pub fn is_operational(self) -> bool {
        matches!(self, Self::Supported | Self::Experimental)
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
    /// Native mode can be used without this capability (e.g. suppression).
    pub optional: bool,
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
            optional: false,
        }
    }

    /// Marks the capability as not required for native mode.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}
