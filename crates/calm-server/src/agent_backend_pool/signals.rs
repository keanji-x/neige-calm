//! Typed observations only. No subscriptions, timers, admission or lease owner in P1.

use std::time::SystemTime;

use super::BackendRef;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observed<T> {
    Unknown,
    Known(T),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendHealth {
    Ready,
    Unavailable(UnavailableReason),
    NotConfigured,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    NotRunning,
    NotAuthenticated,
    AuthenticationHeld,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuotaStatus {
    Available,
    Exhausted,
}

/// Facts supplied by protocol owners, never inferred from arbitrary error text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CooldownEvidence {
    ConcurrencyLimit,
    RateLimited,
    QuotaExhausted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cooldown {
    pub until: SystemTime,
    pub evidence: CooldownEvidence,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendSignals {
    pub health: Observed<BackendHealth>,
    pub inflight: Observed<u32>,
    pub cooldown: Observed<Option<Cooldown>>,
    pub quota: Observed<QuotaStatus>,
}

impl Default for BackendSignals {
    fn default() -> Self {
        Self {
            health: Observed::Unknown,
            inflight: Observed::Unknown,
            cooldown: Observed::Unknown,
            quota: Observed::Unknown,
        }
    }
}

/// Correlation travels with observations. P4's owner must reject stale generations;
/// merely constructing this value does not update any live state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendObservation {
    pub backend: BackendRef,
    pub connection_generation: u64,
    pub observed_at: SystemTime,
    pub signals: BackendSignals,
}
