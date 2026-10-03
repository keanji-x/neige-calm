//! Claude Code as a Planner backend (#1791). `protocol` is the wire and `translate` the pure
//! translation of a Claude turn into the `PlannerEvent`s the Planner harness consumes;
//! `session` runs one `claude -p` process per turn (`spawn` is its argv, environment and
//! instructions file, `driver` its read loop and settlement), `stop` is the marker sweep,
//! `config` the typed configuration. `availability` is the readiness check (#1817): `auth_status`
//! is its login step, `catalog_fetch` its model-list step (#1822), and `readiness_command` the
//! bounded run they and the `--version` check share. `models` is the CLI's model list that check
//! caches: `GET /api/models` answers it and a write advises a selection against it.

pub mod auth_status;
pub mod availability;
pub mod catalog_fetch;
pub mod config;
mod driver;
pub mod lifecycle;
pub mod models;
pub mod protocol;
pub mod readiness_command;
pub(crate) mod rewind;
pub mod session;
pub mod spawn;
pub mod stop;
pub mod translate;
pub mod wiring;

#[cfg(test)]
mod auth_status_tests;
#[cfg(test)]
mod catalog_fetch_tests;
#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod rewind_tests;
#[cfg(test)]
mod spawn_tests;
#[cfg(test)]
mod stop_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod tests;
