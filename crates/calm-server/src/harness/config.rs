use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct HarnessConfig {
    pub debounce_min_idle: Duration,
    pub debounce_max_wait: Duration,
    pub max_turn_duration: Duration,
    pub interrupt_completion_budget: Duration,
    pub resumed_reconcile_budget: Duration,
    /// #1505 S4 review round 2 — how long a conversation may go on retrying a
    /// TRANSIENT refusal before the reader is told anything.
    ///
    /// Pacing the retry bounds its rate, not its duration: a codex outage that
    /// lasts an hour leaves the person's sentence sitting as `queued` for an
    /// hour, politely. This bounds the SILENCE rather than the retrying — past
    /// it the conversation says it is waiting and keeps waiting, so the notice
    /// clears itself the moment codex answers.
    pub transient_silence_budget: Duration,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            debounce_min_idle: Duration::from_millis(250),
            debounce_max_wait: Duration::from_secs(5),
            max_turn_duration: Duration::from_secs(30 * 60),
            interrupt_completion_budget: Duration::from_secs(30),
            resumed_reconcile_budget: Duration::from_secs(5),
            transient_silence_budget: Duration::from_secs(30),
        }
    }
}
