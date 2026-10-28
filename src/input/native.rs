//! Native input provider.
//!
//! Not implemented yet on any platform. It reports its status explicitly
//! through [`crate::platform`] and refuses to start, so the rest of the
//! application can never mistake it for a working input source.

use crate::core::capabilities::Capability;
use crate::core::devices::DeviceInfo;
use crate::core::input::DeviceId;
use crate::platform;
use crate::provider::{ProviderError, ProviderSink};

use super::InputProvider;

#[derive(Debug, Default)]
pub struct NativeInputProvider;

impl NativeInputProvider {
    fn unsupported() -> ProviderError {
        ProviderError::Unsupported(format!(
            "Native input capture is not implemented on {}",
            platform::os_label()
        ))
    }
}

impl InputProvider for NativeInputProvider {
    fn name(&self) -> &'static str {
        "native input"
    }

    fn capabilities(&self) -> Vec<Capability> {
        platform::input_capabilities()
    }

    fn start(&mut self, _sink: ProviderSink) -> Result<(), ProviderError> {
        Err(Self::unsupported())
    }

    fn stop(&mut self) {}

    fn is_running(&self) -> bool {
        false
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        Vec::new()
    }

    fn set_device_enabled(&mut self, _id: &DeviceId, _enabled: bool) -> Result<(), ProviderError> {
        Err(Self::unsupported())
    }
}
