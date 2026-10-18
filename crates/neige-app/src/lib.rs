//! Host application ownership. Normal service and offline maintenance share one implementation.
mod admin;
mod apply;
mod cli;
mod config;
mod execution;
mod identity;
mod installed;
mod manifest;
mod package;
mod preflight;
mod service_owner;
mod setup;
mod source;
mod supervisor;
mod tailnet;
mod upgrade;

pub use cli::run_cli;
pub(crate) use service_owner::ensure_admin_token_file;
#[cfg(test)]
pub(crate) use supervisor::calm_server_supervisor_config;
pub(crate) use supervisor::{Supervisor, SupervisorConfig};

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
