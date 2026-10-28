//! Plumbing shared by input and target providers.

use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use crate::core::devices::DeviceInfo;
use crate::core::input::{DeviceId, InputEvent};

/// Messages providers push to the application service.
#[derive(Debug, Clone)]
pub enum ProviderMessage {
    DeviceAdded(DeviceInfo),
    DeviceUpdated(DeviceInfo),
    DeviceRemoved(DeviceId),
    Input(InputEvent),
    /// Target lifecycle changed; the service should take a new snapshot.
    TargetsChanged,
    Error {
        provider: &'static str,
        message: String,
    },
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum SinkError {
    #[error("event queue is full")]
    Full,
    #[error("application is shutting down")]
    Closed,
}

/// Non-blocking handle providers use to send messages. Backed by a bounded
/// queue, so `send` fails with [`SinkError::Full`] instead of accumulating
/// events without limit.
#[derive(Clone)]
pub struct ProviderSink(Arc<dyn Fn(ProviderMessage) -> Result<(), SinkError> + Send + Sync>);

impl ProviderSink {
    pub fn new(
        f: impl Fn(ProviderMessage) -> Result<(), SinkError> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(f))
    }

    pub fn send(&self, msg: ProviderMessage) -> Result<(), SinkError> {
        (self.0)(msg)
    }
}

impl fmt::Debug for ProviderSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProviderSink")
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    #[error("{0}")]
    Unsupported(String),
    #[error("provider is not running")]
    NotRunning,
    #[error("unknown device `{0}`")]
    UnknownDevice(DeviceId),
    #[error("unknown target: {0}")]
    UnknownTarget(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("event could not be queued: {0}")]
    Queue(#[from] SinkError),
}
