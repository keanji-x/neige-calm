//! Where a task execution runs.

/// #1830 S2 D5: whether a task runs in its track's checkout — a codex or claude task that is not
/// on the child-track route. Such tasks share the checkout, so they run one at a time.
pub fn runs_in_track_checkout(kind: &str, spawn: &str) -> bool {
    matches!(kind, "codex" | "claude") && spawn != crate::task_recovery::TASK_CHILD_TRACK_ROUTE
}
