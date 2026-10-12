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

pub mod auth;
pub mod catalog;
pub mod cli;
pub mod config;
pub mod env_guard;
pub mod exit;
pub mod files;
pub mod lock;
pub mod logging;
pub mod membership;
pub mod sdk_error;
pub mod source;
pub mod store;
pub mod types;

pub use catalog::{Catalog, Endpoint, EndpointId, KeyKind};
pub use store::{OpenError, Store, StoreError};
