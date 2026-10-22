//! Compact has no turn in its ACK. Associate the unique observed start under
//! the same pending native ticket, including END received before the RPC reply.
use super::*;
use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};

impl CodexHarnessBinding {
    pub(crate) fn matches_maintenance(&self, target: &MaintenanceTarget) -> bool {
        self.card == target.card_id
            && self.track == target.track_id
            && self.runtime == target.worker_session_id
    }
}

impl NativeTurns {
    pub(in crate::shared_codex_appserver) fn maintenance(
        &self,
        target: &MaintenanceTarget,
    ) -> Option<MaintenanceObservation> {
        let mut state = self.lock();
        let live = state
            .source
            .as_ref()
            .is_some_and(|source| state.is_current(source));
        let source = state.source.clone();
        let slot = state.slots.get_mut(&target.thread_id)?;
        if let Some(pending) = &slot.pending
            && matches!(&pending.ticket.input, NativeInput::Maintenance(actual) if actual == target)
        {
            if pending.ambiguous || slot.unresolved || !live {
                return Some(MaintenanceObservation::Unconfirmed);
            }
            pending.candidate.as_ref()?;
            let mut pending = slot.pending.take()?;
            let mut candidate = pending.candidate.take()?;
            candidate.input = pending.ticket.input.clone();
            if let Some(old) = slot.current.replace(candidate) {
                slot.retain_observed(old);
            }
        }
        let matches = |current: &NativeExecution| matches!(&current.input, NativeInput::Maintenance(actual) if actual == target);
        let Some(current) = slot.current.as_ref().filter(|current| matches(current)) else {
            return slot
                .retired
                .values()
                .any(matches)
                .then_some(MaintenanceObservation::Unconfirmed);
        };
        if !live
            || slot.unresolved
            || !source
                .as_ref()
                .is_some_and(|source| source.ptr_eq(&current.source))
        {
            return Some(MaintenanceObservation::Unconfirmed);
        }
        Some(MaintenanceObservation::Turn {
            turn_id: current.turn.clone(),
            settled: if current.ended {
                Some(current.terminal.as_ref().is_some_and(|terminal| {
                    terminal.get("status").and_then(Value::as_str) == Some("completed")
                        && !terminal.get("error").is_some_and(|error| !error.is_null())
                }))
            } else {
                None
            },
        })
    }
}
