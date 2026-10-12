//! Tracing setup. Our logs go to stderr; stdout carries only a command's own
//! output.
//!
//! **Only this crate's events are emitted.** Every other target is off,
//! `longbridge*` first among them:
//!
//! - The SDK's HTTP client logs every request URL and every full response
//!   body at `info` (`rust/crates/httpclient/src/request.rs:405-407,450` at
//!   `longbridge/openapi@v5.2.0`). Bodies are archive content, not log
//!   lines.
//! - The SDK's OAuth crate logs the browser callback's `error` query
//!   parameter at `warn` (`rust/crates/oauth/src/callback.rs:94-99`). Anyone
//!   who can reach the local callback port chooses that text.
//! - oauth2, reqwest, hyper and poem log upstream text of their own.
//!
//! Our code logs the classified outcome instead, and renders SDK errors only
//! through [`crate::sdk_error`]. The filter is fixed here and not taken from
//! `RUST_LOG`, so no environment can turn a third-party target back on.
//!
//! The SDK's quote and trade contexts also log through a per-context
//! subscriber that is a no-op unless `LONGBRIDGE_LOG_PATH` is set
//! (`rust/src/config.rs:192`), which [`crate::env_guard`] refuses with every
//! other `LONGBRIDGE_*` variable.

use tracing::{Level, Subscriber};
use tracing_subscriber::{filter::Targets, fmt::MakeWriter, layer::SubscriberExt};

/// This crate's target prefix: the library's module paths and the binary.
pub const OWN_TARGET: &str = "research_archive";

/// The level filter: `level` for this crate, everything else off.
pub fn filter(level: Level) -> Targets {
    Targets::new().with_target(OWN_TARGET, level)
}

/// A subscriber writing plain lines to `writer` with [`filter`] applied.
pub fn subscriber<W>(default: Level, writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::registry().with(filter(default)).with(
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_target(true),
    )
}

/// Install the process-wide subscriber: `info` and above to stderr.
pub fn init() {
    tracing::subscriber::set_global_default(subscriber(Level::INFO, std::io::stderr))
        .expect("the global tracing subscriber is installed once");
}
