//! Claude-specific marker metadata for the shared process cleanup primitive.
pub use crate::planner_process::{
    MarkerInstance, STOP_BOUND, SeamPolicy, stop, stop_by, sweep, sweep_by,
};
#[cfg(feature = "fixtures")]
pub use crate::planner_process::{
    clear_claude_planner_stop_failure_for_test, fail_claude_planner_stop_for_test,
    hold_claude_planner_scans_for_test, hung_claude_planner_scans_for_test,
    sigkill_verified_for_test,
};
pub const MARKER_KEY: &str = "NEIGE_CLAUDE_PLANNER";
#[cfg(test)]
pub(crate) use crate::planner_process::{Member, scan_in, scan_off_thread, signal_verified};
