use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct HarnessConfig {
    pub debounce_min_idle: Duration,
    pub debounce_max_wait: Duration,
    /// Idle / max-wait pair used INSTEAD of the two above while the pending queue holds nothing
    /// but `ReportEdited` observations and no hard-fire is armed (one edit is many saves).
    pub report_edit_min_idle: Duration,
    pub report_edit_max_wait: Duration,
    pub max_turn_duration: Duration,
    pub interrupt_completion_budget: Duration,
    pub resumed_reconcile_budget: Duration,
    /// How long a conversation may go on retrying a TRANSIENT refusal before the reader is told;
    /// bounds the silence, not the retrying.
    pub transient_silence_budget: Duration,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            debounce_min_idle: Duration::from_millis(250),
            debounce_max_wait: Duration::from_secs(5),
            report_edit_min_idle: Duration::from_secs(20),
            report_edit_max_wait: Duration::from_secs(120),
            max_turn_duration: Duration::from_secs(30 * 60),
            interrupt_completion_budget: Duration::from_secs(30),
            resumed_reconcile_budget: Duration::from_secs(5),
            transient_silence_budget: Duration::from_secs(30),
        }
    }
}
