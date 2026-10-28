//! Platform-independent business logic.
//!
//! Nothing in this module calls an operating-system API, touches a GUI
//! toolkit, or opens a physical device. Everything here is driven by plain
//! data so it can be unit tested in isolation.

pub mod capabilities;
pub mod config;
pub mod devices;
pub mod input;
pub mod mappings;
pub mod routing;
pub mod targets;
