//! Shared Codex runtime foundations; kernel authority and persistence stay in host adapters.
pub mod home;
mod liveness;
pub use liveness::liveness_facts_from_read;
mod environment;
pub use environment::{SPAWN_ENV_PASSTHROUGH, SpawnEnvironment};
mod proxy;
pub use proxy::{effective_proxy_env_from, resolved_proxy_env_pairs};
