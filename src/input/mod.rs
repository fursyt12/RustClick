//! Input providers: sources of normalized input events.

pub mod mock;
pub mod native;

use crate::core::capabilities::Capability;
use crate::core::devices::DeviceInfo;
use crate::core::input::DeviceId;
use crate::provider::{ProviderError, ProviderSink};

/// Common interface of mock and native input sources.
///
/// Providers report devices and events through the [`ProviderSink`] given to
/// [`start`](InputProvider::start). Native providers run their own capture
/// threads; the sink never blocks.
pub trait InputProvider: Send {
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Vec<Capability>;

    fn start(&mut self, sink: ProviderSink) -> Result<(), ProviderError>;

    fn stop(&mut self);

    fn is_running(&self) -> bool;

    fn devices(&self) -> Vec<DeviceInfo>;

    /// Starts or stops capturing a device. For physical devices this is
    /// where an exclusive grab would be acquired or released.
    fn set_device_enabled(&mut self, id: &DeviceId, enabled: bool) -> Result<(), ProviderError>;
}
