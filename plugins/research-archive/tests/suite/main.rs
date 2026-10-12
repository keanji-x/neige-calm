//! The research-archive integration suite: one binary, driven through the
//! public `Store`, raw SQLite connections to the same file, and the binary
//! itself as a subprocess. Tests that build an SDK `OAuth` or a
//! `LongbridgeSource` in process live in the `net` binary instead.

mod cli;
#[path = "../common/cli_env.rs"]
mod cli_env;
mod clock;
mod freeze;
mod guards;
mod open;
mod reads;
mod send;
mod support;
mod writes;
