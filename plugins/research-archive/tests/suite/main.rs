//! The research-archive integration suite: one binary, driven through the
//! public `Store`, raw SQLite connections to the same file, and the binary
//! itself as a subprocess. Tests that build an SDK client, or the archive's
//! source, in process live in the `net` binary instead.

#[path = "../common/archive.rs"]
#[allow(dead_code)]
mod archive;
mod capture_cli;
mod capture_proc;
mod capture_secrets;
mod cli;
#[path = "../common/cli_env.rs"]
mod cli_env;
mod clock;
#[path = "../net/fake.rs"]
#[allow(dead_code)]
mod fake;
mod freeze;
mod guards;
mod open;
mod plugin;
mod read_only;
mod reads;
mod send;
mod status;
mod support;
mod units;
mod writes;
