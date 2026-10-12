//! The research-archive integration suite: one binary, driven through the
//! public `Store`, raw SQLite connections to the same file, the real
//! `LongbridgeSource` against a local fake server, and the binary itself as a
//! subprocess.

mod cli;
mod clock;
#[path = "../common/dotenv_reach.rs"]
mod dotenv_reach;
mod fake;
mod freeze;
mod guards;
mod logs;
mod open;
mod reads;
mod secrets;
mod send;
mod source;
mod support;
mod writes;
