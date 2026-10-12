//! The research-archive integration suite: one binary, driven through the
//! public `Store` and raw SQLite connections to the same file.

mod guards;
mod open;
mod reads;
mod support;
mod writes;
