//! Point-in-time archive of Longbridge fundamentals (#2575).
//!
//! S1-1 is the persistence boundary: the compiled endpoint [`catalog`] and
//! the append-only [`store`] of `archive.sqlite3`. See the README for the
//! schema, the read contract and how to import the file read-only.

pub mod catalog;
pub mod store;
pub mod types;

pub use catalog::{Catalog, Endpoint, EndpointId, KeyKind};
pub use store::{OpenError, Store, StoreError};
