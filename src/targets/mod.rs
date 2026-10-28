//! Target providers: discovery, lifecycle monitoring and action delivery.

pub mod mock;
pub mod native;

use crate::core::capabilities::Capability;
use crate::core::mappings::Action;
use crate::core::routing::{DeliveryOutcome, RouteTarget};
use crate::core::targets::{TargetOrigin, TargetSnapshot};
use crate::provider::{ProviderError, ProviderSink};

/// Common interface of mock and native target providers.
pub trait TargetProvider: Send {
    fn name(&self) -> &'static str;

    fn origin(&self) -> TargetOrigin;

    fn capabilities(&self) -> Vec<Capability>;

    /// Whether the provider currently pushes `TargetsChanged` notifications.
    /// When false, targets only update on a manual refresh.
    fn notifications_active(&self) -> bool;

    fn start(&mut self, sink: ProviderSink) -> Result<(), ProviderError>;

    fn stop(&mut self);

    fn snapshot(&mut self) -> Result<TargetSnapshot, ProviderError>;

    /// Re-validates the destination against the live state and delivers the action.
    fn deliver(&mut self, target: &RouteTarget, action: &Action) -> DeliveryOutcome;
}
