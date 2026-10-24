//! Managed agents using ACP. Protocol code lives in `provider::acp`.
#[cfg(all(test, unix))]
mod boot_tests;
mod catalog;
#[cfg(all(test, target_os = "linux"))]
mod catalog_lifecycle_tests;
pub mod config;
pub(crate) mod execution;
pub(crate) mod lifecycle;
mod process;
mod profile;
pub(crate) mod recovery;
pub mod session;
mod start;
#[cfg(feature = "fixtures")]
pub mod test_seams;
#[cfg(all(test, feature = "fixtures"))]
pub(crate) mod test_transport;
pub mod wiring;
