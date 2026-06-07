use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct HarnessConfig {
    pub debounce_min_idle: Duration,
    pub debounce_max_wait: Duration,
    pub max_turn_duration: Duration,
    pub interrupt_completion_budget: Duration,
    pub resumed_reconcile_budget: Duration,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            debounce_min_idle: Duration::from_millis(250),
            debounce_max_wait: Duration::from_secs(5),
            max_turn_duration: Duration::from_secs(30 * 60),
            interrupt_completion_budget: Duration::from_secs(30),
            resumed_reconcile_budget: Duration::from_secs(5),
        }
    }
}
