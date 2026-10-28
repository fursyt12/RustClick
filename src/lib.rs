//! RustClick: routes presentation-remote input to a chosen application.
//!
//! Layers, from the inside out:
//!
//! * [`core`]: platform-independent business logic (no OS, GUI or device access).
//! * [`input`], [`targets`], [`platform`]: mock and native providers and
//!   capability reporting.
//! * [`app`]: the [`app::AppService`] that owns the state and providers and
//!   publishes UI-independent [`app::ViewModel`]s.
//! * [`controller`] and [`ui`]: the Slint UI and the glue that maps view
//!   models to Slint properties and UI callbacks to [`app::Command`]s.

pub mod app;
pub mod controller;
pub mod core;
pub mod input;
pub mod logging;
pub mod platform;
pub mod provider;
pub mod storage;
pub mod targets;

/// Types generated from `ui/*.slint` by `build.rs`.
pub mod ui {
    slint::include_modules!();
}
