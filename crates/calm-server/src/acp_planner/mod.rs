//! Managed agents using ACP. Protocol code lives in `provider::acp`.
#[cfg(all(test, unix))]
mod boot_tests;
mod catalog;
#[cfg(all(test, target_os = "linux"))]
mod catalog_lifecycle_tests;
pub mod config;
mod context_snapshot;
pub(crate) mod execution;
pub(crate) mod lifecycle;
pub(crate) use crate::acp_session::process;
pub(crate) use crate::acp_session::profile;
pub(crate) mod recovery;
pub(crate) mod resident_execution;
pub mod session;
pub(crate) mod start;
#[cfg(feature = "fixtures")]
pub mod test_seams;
#[cfg(all(test, feature = "fixtures"))]
pub(crate) mod test_transport;
pub mod wiring;
