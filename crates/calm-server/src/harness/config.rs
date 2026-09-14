use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct HarnessConfig {
    pub debounce_min_idle: Duration,
    pub debounce_max_wait: Duration,
    /// #1667 D2 — the idle / max-wait pair used INSTEAD of the two above
    /// while the pending queue holds nothing but `ReportEdited`
    /// observations and no hard-fire is armed. One edit is many saves;
    /// the planner should wake once per edit, after the editor has gone
    /// quiet, not once per keystroke-sized save. A hard-fire arriving in
    /// the window (a user message, a task receipt) still issues at once
    /// and takes the queued edits with it.
    pub report_edit_min_idle: Duration,
    pub report_edit_max_wait: Duration,
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
            report_edit_min_idle: Duration::from_secs(20),
            report_edit_max_wait: Duration::from_secs(120),
            max_turn_duration: Duration::from_secs(30 * 60),
            interrupt_completion_budget: Duration::from_secs(30),
            resumed_reconcile_budget: Duration::from_secs(5),
            transient_silence_budget: Duration::from_secs(30),
        }
    }
}
