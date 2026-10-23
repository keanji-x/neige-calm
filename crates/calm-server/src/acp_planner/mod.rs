//! Managed agents using ACP. Protocol code lives in `provider::acp`.
#[cfg(all(test, unix))]
mod boot_tests;
mod catalog;
pub mod config;
pub(crate) mod execution;
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
