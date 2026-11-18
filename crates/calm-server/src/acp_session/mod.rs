//! Role-neutral native ACP transport. Task and conversation policies supply their own
//! admission, permission handler, event sink and settlement callback.
pub(crate) mod driver;
pub(crate) mod process;
pub(crate) mod server;
pub(crate) mod setup;

pub(crate) mod failure;
pub mod host;
pub(crate) mod profile;

pub(crate) mod attachment;
