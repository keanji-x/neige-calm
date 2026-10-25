//! Read-only failure diagnostics. Do not use maintenance(), which binds candidates.
use super::*;
use serde_json::json;

fn input_identity(input: &NativeInput) -> Value {
    match input {
        NativeInput::Legacy => json!({"kind":"legacy"}),
        NativeInput::Canonical(_) => json!({"kind":"canonical"}),
        NativeInput::Maintenance(target) => json!({
            "kind":"maintenance", "operation_id":target.operation_id,
            "card_id":target.card_id, "track_id":target.track_id,
            "worker_session_id":target.worker_session_id,
        }),
    }
}

impl SharedCodexAppServer {
    pub(crate) fn native_diagnostics_for_test(&self, thread: &str) -> Value {
        // Failure reporting must not wait for the owner or mutate its correlation.
        let Ok(state) = self.native_turns.state.try_lock() else {
            return json!({"state":"owner_unavailable"});
        };
        let execution = |current: &NativeExecution| {
            json!({
                "turn_id":current.turn,
                "source_current":state.is_current(&current.source),
                "ended":current.ended,
                "input":input_identity(&current.input),
            })
        };
        let slot = state.slots.get(thread).map(|slot| {
            json!({
                "unresolved":slot.unresolved,
                "current":slot.current.as_ref().map(execution),
                "pending":slot.pending.as_ref().map(|pending| json!({
                    "ticket":input_identity(&pending.ticket.input),
                    "ambiguous":pending.ambiguous,
                    "candidate":pending.candidate.as_ref().map(execution),
                })),
            })
        });
        json!({
            "thread_id":thread, "source_generation":state.source_generation,
            "source_live":state.source.as_ref().is_some_and(|source| state.is_current(source)),
            "slot":slot,
        })
    }
}
