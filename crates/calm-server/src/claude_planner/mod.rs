//! Claude Code as a Planner backend (#1791). `protocol` is the wire and `translate` the pure
//! translation of a Claude turn into the Codex-shaped notifications the Planner harness consumes;
//! `session` runs one `claude -p` process per turn (`spawn` is its argv, environment and
//! instructions file, `driver` its read loop and settlement), `stop` is the marker sweep,
//! `config` the typed configuration, `auth_status` its login check (#1817) and `models` the model
//! aliases a Planner may choose.

pub mod auth_status;
pub mod config;
mod driver;
pub mod lifecycle;
pub mod models;
pub mod protocol;
pub mod session;
pub mod spawn;
pub mod stop;
pub mod translate;
pub mod wiring;

#[cfg(test)]
mod auth_status_tests;
#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod spawn_tests;
#[cfg(test)]
mod stop_tests;
#[cfg(test)]
mod tests;
