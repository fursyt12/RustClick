//! Native target provider.
//!
//! Not implemented yet on any platform. Discovery fails explicitly and
//! delivery always reports [`DeliveryOutcome::Unsupported`].

use crate::core::capabilities::Capability;
use crate::core::mappings::Action;
use crate::core::routing::{DeliveryOutcome, RouteTarget};
use crate::core::targets::{TargetOrigin, TargetSnapshot};
use crate::platform;
use crate::provider::{ProviderError, ProviderSink};

use super::TargetProvider;

#[derive(Debug, Default)]
pub struct NativeTargetProvider;

impl TargetProvider for NativeTargetProvider {
    fn name(&self) -> &'static str {
        "native targets"
    }

    fn origin(&self) -> TargetOrigin {
        TargetOrigin::Native
    }

    fn capabilities(&self) -> Vec<Capability> {
        platform::target_capabilities()
    }

    fn notifications_active(&self) -> bool {
        false
    }

    fn start(&mut self, _sink: ProviderSink) -> Result<(), ProviderError> {
        Ok(())
    }

    fn stop(&mut self) {}

    fn snapshot(&mut self) -> Result<TargetSnapshot, ProviderError> {
        Err(ProviderError::Unsupported(format!(
            "Native window discovery is not implemented on {}",
            platform::os_label()
        )))
    }

    fn deliver(&mut self, _target: &RouteTarget, _action: &Action) -> DeliveryOutcome {
        DeliveryOutcome::Unsupported(format!(
            "native delivery is not implemented on {}",
            platform::os_label()
        ))
    }
}
