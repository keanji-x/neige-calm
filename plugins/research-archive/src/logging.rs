//! Tracing setup. Our logs go to stderr; stdout carries only a command's own
//! output.
//!
//! The SDK's HTTP client logs every request URL and every full response body
//! at `info` under its module targets `longbridge_httpcli::request` and
//! `longbridge_httpcli::client` (`rust/crates/httpclient/src/request.rs:405-407,450`,
//! `client.rs:102` at `longbridge/openapi@v5.2.0`). Bodies are archive
//! content, not log lines, so that crate is capped at `warn`. The cap is fixed
//! here and not taken from `RUST_LOG`, so no environment can turn the body
//! logging back on.
//!
//! The SDK's quote and trade contexts log through a per-context subscriber
//! that is a no-op unless `LONGBRIDGE_LOG_PATH` is set (`rust/src/config.rs:192`),
//! which [`crate::env_guard`] refuses with every other `LONGBRIDGE_*` variable.

use tracing::{Level, Subscriber};
use tracing_subscriber::{filter::Targets, fmt::MakeWriter, layer::SubscriberExt};

/// The SDK HTTP client's crate target prefix.
pub const SDK_HTTP_TARGET: &str = "longbridge_httpcli";

/// The level filter: `default` for everything, `warn` for the SDK HTTP client.
pub fn filter(default: Level) -> Targets {
    Targets::new()
        .with_default(default)
        .with_target(SDK_HTTP_TARGET, Level::WARN)
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
