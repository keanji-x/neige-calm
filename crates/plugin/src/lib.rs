//! Plugin protocol and execution runtime. The server supplies lifecycle, authorization and storage.
pub mod auth;
pub mod child_process;
pub mod cli_query;
pub mod config;
pub mod connector;
pub mod error;
pub mod forge_caller;
pub mod glob;
pub mod http_headers;
pub mod http_mcp;
pub mod manifest;
pub mod mcp;
pub mod perms;
pub mod process;
pub mod results;
pub mod template_input;
pub mod version;
pub use http_mcp::HttpCredential;

pub mod proc_identity;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub mod builtin;
pub mod ports;

pub mod host;
