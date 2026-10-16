//! Managed agents using ACP. Protocol code lives in `provider::acp`.
#[cfg(all(test, unix))]
#[path = "../../tests/unit/acp_boot.rs"]
mod boot_tests;
mod catalog;
pub mod config;
mod process;
pub(crate) mod recovery;
pub mod session;
#[cfg(feature = "fixtures")]
pub mod test_seams;
pub mod wiring;
