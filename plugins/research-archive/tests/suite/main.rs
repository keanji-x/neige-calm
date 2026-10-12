//! The research-archive integration suite: one binary, driven through the
//! public `Store`, raw SQLite connections to the same file, and the binary
//! itself as a subprocess. Tests that build an SDK client, or the archive's
//! source, in process live in the `net` binary instead.

mod capture_cli;
mod capture_proc;
mod capture_secrets;
mod capture_support;
mod cli;
#[path = "../common/cli_env.rs"]
mod cli_env;
mod clock;
mod freeze;
mod guards;
mod open;
mod reads;
mod runner;
mod send;
mod support;
mod units;
mod writes;
