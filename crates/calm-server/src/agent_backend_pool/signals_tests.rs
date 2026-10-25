use super::signals::{BackendSignals, Observed};

#[test]
fn agent_backend_pool_unobserved_signals_are_unknown_not_zero_or_ready() {
    let signals = BackendSignals::default();
    assert_eq!(signals.health, Observed::Unknown);
    assert_eq!(signals.inflight, Observed::Unknown);
    assert_eq!(signals.cooldown, Observed::Unknown);
    assert_eq!(signals.quota, Observed::Unknown);
}
