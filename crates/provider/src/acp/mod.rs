//! ACP v1 transport and pure event translation. The caller owns processes and policy.
pub mod approvals;
pub mod compaction;
pub mod configuration;
pub mod grok_compaction;
pub mod permission;
pub mod protocol;
pub mod translate;
mod transport;
pub use transport::{Client, Connection, Error, Incoming, PendingResponse, WriteFailure};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod compaction_tests;
