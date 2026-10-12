//! Point-in-time archive of Longbridge fundamentals (#2575).
//!
//! S1-1 is the persistence boundary: the compiled endpoint [`catalog`] and
//! the append-only [`store`] of `archive.sqlite3`. See the README for the
//! schema, the read contract and how to import the file read-only.
//!
//! S1-2a adds what the capture job stands on:
//!
//! - [`config`]: the typed TOML config;
//! - [`env_guard`]: the access region and the refusal of ambient SDK
//!   configuration;
//! - [`lock`]: the data-dir `flock`;
//! - [`auth`]: the data-dir token storage, token import, the browser login
//!   and the `OAuth` client that never starts a browser flow;
//! - [`source`]: one catalog request as the Store's `Response`;
//! - [`logging`]: the tracing setup that keeps the SDK's body logging off;
//! - [`exit`]: the process exit codes;
//! - [`cli`]: the `research-archive` command line.
//!
//! S1-2b adds the capture job itself:
//!
//! - [`capture`]: one start of the job, its run phases and the recorder;
//! - [`calendar`]: the US trading calendar cache and the capture window;
//! - [`clock`]: the injected clock every decision and timestamp uses;
//! - [`state`]: `last_exit.json`, how the last start ended;
//! - [`notify`]: the deduplicated `OnFailure=` alert.

pub mod auth;
pub mod calendar;
pub mod capture;
pub mod catalog;
pub mod cli;
pub mod clock;
pub mod config;
pub mod env_guard;
pub mod exit;
pub mod files;
pub mod lock;
pub mod logging;
pub mod membership;
pub mod notify;
pub mod sdk_error;
pub mod source;
pub mod state;
pub mod store;
pub mod types;

pub use catalog::{Catalog, Endpoint, EndpointId, KeyKind};
pub use store::{OpenError, Store, StoreError};
