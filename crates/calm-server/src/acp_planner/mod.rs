//! Managed agents using ACP. Protocol code lives in `provider::acp`.
mod catalog;
pub mod config;
mod process;
pub(crate) mod recovery;
pub mod session;
#[cfg(feature = "fixtures")]
pub mod test_seams;
pub mod wiring;
