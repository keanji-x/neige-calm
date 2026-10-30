//! One actual current-native slot, including an unresolved start candidate.
//!
//! Source publication/invalidation takes this short lock inside the supervisor
//! core lock. Observers never acquire core while holding it. No native call,
//! repository operation or asynchronous cleanup runs under either lock.
//! Detach leases fence one thread and fail recheck on source-generation changes.
use super::*;
mod detach;
#[cfg(test)]
mod diagnostics;
mod history;
mod maintenance;
mod settlement;
use crate::harness::backend::{ExecutionEndScope, ExecutionEndTarget, ExecutionInput};
pub(crate) use detach::IdleDetachGuard;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, Weak};

/// Closed protocol facts, not an arbitrary native diagnostic/body copy. END
/// identity is already validated by this owner; unknown extras are not stored.
fn terminal_fact(id: &str, native: &Value) -> Value {
    let mut fact = serde_json::json!({"id":id});
    if let Some(status @ ("completed" | "interrupted" | "failed")) =
        native.get("status").and_then(Value::as_str)
    {
        fact["status"] = Value::String(status.into());
    }
    if native.get("error").is_some_and(|error| !error.is_null()) {
        fact["error"] = Value::Bool(true);
        crate::harness::turn_outcome::canonical_diagnostic(&mut fact);
    }
    fact
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CodexHarnessBinding {
    card: String,
    track: String,
    runtime: String,
}

impl CodexHarnessBinding {
    pub(crate) fn new(card: &str, track: &str, runtime: &str) -> Result<Self> {
        if [card, track, runtime].iter().any(|id| id.is_empty()) {
            return Err(CalmError::Internal(
                "Codex harness requires its real row identity".into(),
            ));
        }
        Ok(Self {
            card: card.into(),
            track: track.into(),
            runtime: runtime.into(),
        })
    }

    pub(crate) fn matches(&self, target: &ExecutionEndTarget) -> bool {
        target.matches_owner(&self.card, &self.track, &self.runtime)
    }
}

#[derive(Clone)]
pub(crate) enum NativeInput {
    Legacy,
    Maintenance(crate::harness::backend::MaintenanceTarget),
    Canonical(Arc<CanonicalNativeInput>),
}

pub(crate) struct CanonicalNativeInput {
    binding: CodexHarnessBinding,
    input: ExecutionInput,
    require_settled: bool,
}

impl NativeInput {
    pub(crate) fn canonical(
        binding: CodexHarnessBinding,
        logical_turn_id: &calm_types::conversation::LogicalTurnId,
        proof: &calm_types::conversation::SubmissionDispatchProof,
        require_settled: bool,
    ) -> Self {
        Self::Canonical(Arc::new(CanonicalNativeInput {
            binding,
            input: ExecutionInput::canonical(logical_turn_id, proof),
            require_settled,
        }))
    }

    fn matches(&self, target: &ExecutionEndTarget) -> bool {
        matches!(self, Self::Canonical(actual) if actual.binding.matches(target) && actual.input.matches(target))
    }
}

pub(crate) enum NativeSteer {
    Legacy,
    BudgetControl,
    CanonicalControl(Box<ExecutionEndTarget>),
    CanonicalSupplemental {
        target: Box<ExecutionEndTarget>,
        proof: calm_types::conversation::SubmissionDispatchProof,
    },
}

/// Consumed by one interrupt RPC on the actual client qualified under this owner.
pub(super) struct ControlTicket {
    pub(super) client: Arc<CodexAppServer>,
    pub(super) thread: String,
    pub(super) turn: String,
}

pub(super) struct SteerTicket {
    turn: String,
}

pub(super) struct SettlementFence {
    source: Weak<CodexAppServer>,
    thread: String,
    pending: Option<Arc<StartTicket>>,
    current: Option<String>,
    candidate: Option<String>,
    ambiguous: bool,
    unresolved: bool,
    activity: Arc<()>,
}

pub(super) struct StartTicket {
    input: NativeInput,
    activity: Arc<()>,
}

struct PendingStart {
    ticket: Arc<StartTicket>,
    candidate: Option<NativeExecution>,
    ambiguous: bool,
    other_candidates: HashMap<String, NativeExecution>,
}

struct NativeExecution {
    turn: String,
    source: Weak<CodexAppServer>,
    input: NativeInput,
    ended: bool,
    terminal: Option<Value>,
    // Only the existing control projection. Acknowledgement is not END and
    // cannot retire the Original Initial association held by this execution.
    control_visible: bool,
}

impl NativeExecution {
    fn needs_control(&self) -> bool {
        !self.ended && self.control_visible
    }

    fn retire_control_hint(&mut self) {
        self.control_visible = false;
    }
}

#[derive(Default)]
struct ThreadSlot {
    current: Option<NativeExecution>,
    pending: Option<PendingStart>,
    unresolved: bool,
    steering: Option<Arc<SteerTicket>>,
    activity: Arc<()>,
    settled_activity: Option<Arc<()>>,
    retired: HashMap<String, NativeExecution>,
    unresolved_candidates: std::collections::HashSet<String>,
}

impl ThreadSlot {
    fn retire_control_hint(&mut self, turn: &str) {
        if let Some(current) = self.current.as_mut().filter(|current| current.turn == turn) {
            current.retire_control_hint();
        }
        if let Some(candidate) = self
            .pending
            .as_mut()
            .and_then(|pending| pending.candidate.as_mut())
            .filter(|candidate| candidate.turn == turn)
        {
            candidate.retire_control_hint();
        }
    }
}

#[derive(Default)]
struct State {
    source: Option<Weak<CodexAppServer>>,
    slots: HashMap<String, ThreadSlot>,
    // The bool records a thread removal deferred until this lease drops.
    detaching: HashMap<String, (Arc<()>, bool)>,
    source_generation: u64,
}

impl State {
    fn owns_source(&self, source: &Weak<CodexAppServer>) -> bool {
        self.source
            .as_ref()
            .is_some_and(|current| current.ptr_eq(source))
    }

    fn is_current(&self, source: &Weak<CodexAppServer>) -> bool {
        self.owns_source(source)
            && source.upgrade().is_some_and(|client| {
                !client.reader_finished() && !client.notification_stream_closed()
            })
    }
}

/// Bound to the actual notification future, including cancellation before its
/// first poll. It neither owns the client nor waits for any native/SQL cleanup.
pub(super) struct SourceRetirement {
    owner: Arc<NativeTurns>,
    source: Weak<CodexAppServer>,
}

impl SourceRetirement {
    pub(super) fn new(owner: Arc<NativeTurns>, source: Weak<CodexAppServer>) -> Self {
        Self { owner, source }
    }
}

impl Drop for SourceRetirement {
    fn drop(&mut self) {
        let mut state = self.owner.lock();
        // A dead source must still be able to retire itself; liveness is not
        // part of this identity comparison, and an old task cannot clear a new one.
        state.retire_source(&self.source);
    }
}

pub(super) struct InterruptSnapshot {
    pub(super) source: Option<Arc<CodexAppServer>>,
    pub(super) turns: Vec<String>,
    pub(super) unresolved: bool,
}

#[derive(Default)]
pub(super) struct NativeTurns {
    state: StdMutex<State>,
}

impl NativeTurns {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("Codex native owner mutex poisoned")
    }

    pub(super) fn is_current_source(&self, source: &Weak<CodexAppServer>) -> bool {
        self.lock().is_current(source)
    }

    /// Final synchronous publication after prerequisite awaits. Source changes
    /// use this same owner; no callback may await or acquire Core/native/SQL here.
    pub(super) fn publish_from_current_source(
        &self,
        source: &Weak<CodexAppServer>,
        publish: impl FnOnce(),
    ) {
        let state = self.lock();
        if state.is_current(source) {
            publish();
        }
    }

    pub(super) fn acknowledge_control_from_source(
        &self,
        source: &Weak<CodexAppServer>,
        thread: &str,
        turn: &str,
    ) {
        let mut state = self.lock();
        if state.is_current(source)
            && let Some(slot) = state.slots.get_mut(thread)
        {
            slot.retire_control_hint(turn);
        }
    }

    #[cfg(feature = "fixtures")]
    pub(super) fn acknowledge_control_for_fixture(&self, thread: &str, turn: &str) {
        if let Some(slot) = self.lock().slots.get_mut(thread) {
            slot.retire_control_hint(turn);
        }
    }

    /// A confirmed history replacement retires correlation, unlike a control ACK.
    pub(super) fn retire_from_source(
        &self,
        source: &Weak<CodexAppServer>,
        thread: &str,
        turn: &str,
    ) {
        let mut state = self.lock();
        if !state.is_current(source) {
            return;
        }
        if state.detaching.contains_key(thread) {
            if state
                .slots
                .get(thread)
                .and_then(|slot| slot.current.as_ref())
                .is_some_and(|current| current.turn == turn)
            {
                state.retire_thread(thread);
            }
            return;
        }
        let Some(slot) = state.slots.get_mut(thread) else {
            return;
        };
        if slot
            .current
            .as_ref()
            .is_some_and(|current| current.turn == turn)
        {
            slot.current = None;
            slot.steering = None;
        }
        if slot.pending.as_ref().is_some_and(|pending| {
            pending
                .candidate
                .as_ref()
                .is_some_and(|candidate| candidate.turn == turn)
        }) {
            slot.unresolved |= slot
                .pending
                .as_ref()
                .is_some_and(|pending| pending.ambiguous);
            slot.pending = None;
        }
    }

    pub(super) fn invalidate(&self) {
        self.lock().invalidate();
    }

    pub(super) fn publish(&self, client: &Arc<CodexAppServer>) {
        self.lock().publish_source(client);
    }

    pub(super) fn prepare_start(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        input: NativeInput,
    ) -> Result<Arc<StartTicket>> {
        let mut state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) || state.detaching.contains_key(thread) {
            return Err(CalmError::Conflict(
                "Codex source changed before native issue".into(),
            ));
        }
        let slot = state.slots.entry(thread.into()).or_default();
        if slot.pending.is_some() || slot.steering.is_some() || slot.unresolved {
            return Err(CalmError::Conflict(
                "Codex has an unresolved native start".into(),
            ));
        }
        if matches!(&input, NativeInput::Canonical(input) if input.require_settled)
            && !slot
                .settled_activity
                .as_ref()
                .is_some_and(|activity| Arc::ptr_eq(activity, &slot.activity))
        {
            return Err(CalmError::Conflict(
                "exact-thread idle observation expired before native issue".into(),
            ));
        }
        if matches!(
            &input,
            NativeInput::Canonical(_) | NativeInput::Maintenance(_)
        ) && slot
            .current
            .as_ref()
            .is_some_and(NativeExecution::needs_control)
        {
            return Err(CalmError::Conflict(
                "Codex has a different live native execution".into(),
            ));
        }
        let ticket = Arc::new(StartTicket {
            input,
            activity: slot.activity.clone(),
        });
        slot.pending = Some(PendingStart {
            ticket: ticket.clone(),
            candidate: None,
            ambiguous: false,
            other_candidates: HashMap::new(),
        });
        Ok(ticket)
    }

    pub(super) fn begin_steer(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        turn: &str,
        input: &NativeSteer,
    ) -> Result<Arc<SteerTicket>> {
        let mut state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) || state.detaching.contains_key(thread) {
            return Err(CalmError::Conflict(
                "Codex source changed before supplemental issue".into(),
            ));
        }
        let slot = state
            .slots
            .get_mut(thread)
            .ok_or_else(|| CalmError::Conflict("Codex native turn is not current".into()))?;
        let matched = slot.current.as_ref().is_some_and(|current| {
            current.turn == turn
                && current.needs_control()
                && match input {
                    NativeSteer::BudgetControl => true,
                    NativeSteer::CanonicalControl(target) => {
                        current.input.matches(target)
                            && matches!(&target.scope, ExecutionEndScope::NativeTurn {thread_id,native_turn_id}
                                if thread_id == thread && native_turn_id == turn)
                    }
                    NativeSteer::CanonicalSupplemental { target, proof } => {
                        current.input.matches(target) && proof.attempt() != target.proof.attempt()
                            && matches!(&target.scope, ExecutionEndScope::NativeTurn {thread_id,native_turn_id}
                                if thread_id == thread && native_turn_id == turn)
                    }
                    NativeSteer::Legacy => matches!(&current.input, NativeInput::Legacy),
                }
        });
        if !matched || slot.pending.is_some() || slot.steering.is_some() || slot.unresolved {
            return Err(CalmError::Conflict(
                "Codex supplemental input does not match the current execution".into(),
            ));
        }
        let ticket = Arc::new(SteerTicket { turn: turn.into() });
        slot.steering = Some(ticket.clone());
        Ok(ticket)
    }

    pub(super) fn finish_steer(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        ticket: &Arc<SteerTicket>,
    ) {
        let mut state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) {
            return;
        }
        let Some(slot) = state.slots.get_mut(thread) else {
            return;
        };
        if slot
            .steering
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, ticket))
            && slot
                .current
                .as_ref()
                .is_some_and(|current| current.turn == ticket.turn)
        {
            slot.steering = None;
        }
    }

    /// Release only this unstarted request. A notification candidate contradicts
    /// a refusal and keeps the outcome uncertain; stale responses cannot clear
    /// a replacement source's slot.
    pub(super) fn not_started(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        ticket: &Arc<StartTicket>,
    ) -> bool {
        let mut state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) {
            return true;
        }
        let Some(slot) = state.slots.get_mut(thread) else {
            return true;
        };
        if let Some(pending) = &mut slot.pending
            && Arc::ptr_eq(&pending.ticket, ticket)
        {
            if pending.candidate.is_some() {
                if matches!(&pending.ticket.input, NativeInput::Maintenance(_)) {
                    // A turn preceding a proven unwritten request is foreign activity.
                    pending.ticket = Arc::new(StartTicket {
                        input: NativeInput::Legacy,
                        activity: pending.ticket.activity.clone(),
                    });
                }
                return false;
            }
            slot.pending = None;
        }
        true
    }

    pub(super) fn observe(
        &self,
        source: &Weak<CodexAppServer>,
        notification: &Notification,
    ) -> bool {
        let mut state = self.lock();
        if !state.is_current(source) {
            return false;
        }
        if let Notification::Other { method, params } = notification
            && method == "turn/aborted"
            && let (Some(thread), Some(turn)) = (other_thread_id(params), other_turn_id(params))
            && let Some(slot) = state.slots.get_mut(thread)
        {
            // Compatibility status only. Preserve the same original correlation
            // for a later formal END, including a not-yet-acknowledged candidate.
            slot.retire_control_hint(turn);
        }
        let (thread, native, ended) = match notification {
            Notification::TurnStarted { thread_id, turn } => (thread_id, turn, false),
            Notification::TurnCompleted { thread_id, turn } => (thread_id, turn, true),
            _ => return true,
        };
        let Some(turn) = turn_id(native).filter(|turn| !turn.is_empty()) else {
            return true;
        };
        let terminal = ended.then(|| terminal_fact(turn, native));
        let slot = state.slots.entry(thread.clone()).or_default();
        if let Some(old) = slot.retired.get_mut(turn) {
            old.ended |= ended;
            if ended && old.terminal.is_none() {
                old.terminal = terminal;
            }
            if ended && slot.unresolved_candidates.remove(turn) {
                slot.activity = Arc::new(());
                if slot.unresolved_candidates.is_empty() {
                    slot.unresolved = false;
                }
            }
            return true;
        }
        slot.activity = Arc::new(());
        if let Some(current) = &mut slot.current
            && current.turn == turn
        {
            if ended {
                current.source = source.clone();
            }
            current.ended |= ended;
            if !ended {
                current.control_visible = true;
            }
            if ended && current.terminal.is_none() {
                current.terminal = terminal;
            }
            return true;
        }
        if let Some(pending) = &mut slot.pending {
            match &mut pending.candidate {
                Some(candidate) if candidate.turn == turn => {
                    candidate.ended |= ended;
                    if ended && candidate.terminal.is_none() {
                        candidate.terminal = terminal;
                    }
                }
                Some(_) => {
                    pending.ambiguous = true;
                    let other = pending
                        .other_candidates
                        .entry(turn.into())
                        .or_insert_with(|| NativeExecution {
                            turn: turn.into(),
                            source: source.clone(),
                            input: NativeInput::Legacy,
                            ended: false,
                            terminal: None,
                            control_visible: true,
                        });
                    other.ended |= ended;
                    if ended && other.terminal.is_none() {
                        other.terminal = terminal;
                    }
                }
                None => {
                    pending.candidate = Some(NativeExecution {
                        turn: turn.into(),
                        source: source.clone(),
                        input: NativeInput::Legacy,
                        ended,
                        terminal,
                        control_visible: true,
                    })
                }
            }
        } else if !ended {
            let next = NativeExecution {
                turn: turn.into(),
                source: source.clone(),
                input: NativeInput::Legacy,
                ended: false,
                terminal: None,
                control_visible: true,
            };
            if let Some(previous) = slot.current.replace(next) {
                slot.retain_observed(previous);
            }
        }
        true
    }

    pub(super) fn begin_control(
        &self,
        client: Arc<CodexAppServer>,
        target: &ExecutionEndTarget,
    ) -> Result<ControlTicket> {
        let ExecutionEndScope::NativeTurn {
            thread_id,
            native_turn_id,
        } = &target.scope
        else {
            return Err(CalmError::Conflict(
                "Codex control requires a native turn".into(),
            ));
        };
        let state = self.lock();
        let current = state
            .slots
            .get(thread_id)
            .filter(|slot| !slot.unresolved && slot.pending.is_none() && slot.steering.is_none())
            .and_then(|slot| slot.current.as_ref());
        if !state.is_current(&Arc::downgrade(&client))
            || state.detaching.contains_key(thread_id)
            || !current.is_some_and(|current| {
                current.needs_control()
                    && current.turn == *native_turn_id
                    && current.input.matches(target)
            })
        {
            return Err(CalmError::Conflict(
                "control no longer owns the original native execution".into(),
            ));
        }
        Ok(ControlTicket {
            client,
            thread: thread_id.clone(),
            turn: native_turn_id.clone(),
        })
    }

    pub(super) fn active(&self, thread: &str) -> Option<String> {
        let state = self.lock();
        let slot = state.slots.get(thread)?;
        slot.current
            .as_ref()
            .filter(|current| current.needs_control())
            .or_else(|| {
                slot.pending
                    .as_ref()
                    .and_then(|p| p.candidate.as_ref())
                    .filter(|candidate| candidate.needs_control())
            })
            .map(|current| current.turn.clone())
    }

    #[cfg(feature = "fixtures")]
    pub(super) fn interrupt_snapshot(&self, thread: &str) -> InterruptSnapshot {
        let state = self.lock();
        Self::snapshot(
            &state,
            thread,
            state.source.as_ref().and_then(Weak::upgrade),
        )
    }

    pub(super) fn interrupt_snapshot_for(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
    ) -> Result<InterruptSnapshot> {
        let state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) {
            return Err(CalmError::Conflict(
                "Codex native source is not current".into(),
            ));
        }
        Ok(Self::snapshot(&state, thread, Some(client.clone())))
    }

    fn snapshot(
        state: &State,
        thread: &str,
        source: Option<Arc<CodexAppServer>>,
    ) -> InterruptSnapshot {
        let mut snapshot = InterruptSnapshot {
            source,
            turns: Vec::with_capacity(2),
            unresolved: false,
        };
        if let Some(slot) = state.slots.get(thread) {
            if let Some(current) = slot
                .current
                .as_ref()
                .filter(|current| current.needs_control())
            {
                snapshot.turns.push(current.turn.clone());
            }
            if let Some(candidate) = slot
                .pending
                .as_ref()
                .and_then(|pending| pending.candidate.as_ref())
                .filter(|candidate| candidate.needs_control())
                && !snapshot.turns.contains(&candidate.turn)
            {
                snapshot.turns.push(candidate.turn.clone());
            }
            // A retained start ticket correlates the later response; it is not
            // itself an outstanding control after its exact observed candidate
            // was acknowledged. No-ID/ambiguous starts remain unresolved.
            snapshot.unresolved = slot.unresolved
                || slot.pending.as_ref().is_some_and(|pending| {
                    pending.ambiguous
                        || pending
                            .candidate
                            .as_ref()
                            .is_none_or(|candidate| candidate.needs_control())
                })
                || slot.steering.is_some();
        }
        snapshot
    }

    #[cfg(feature = "fixtures")]
    pub(super) fn insert(&self, thread: String, turn: String) {
        let mut state = self.lock();
        let slot = state.slots.entry(thread).or_default();
        slot.current = Some(NativeExecution {
            turn,
            source: Weak::new(),
            input: NativeInput::Legacy,
            ended: false,
            terminal: None,
            control_visible: true,
        });
    }

    pub(super) fn remove(&self, thread: &str) -> Option<()> {
        self.lock().retire_thread(thread)
    }

    #[cfg(feature = "fixtures")]
    pub(super) fn retire_if(&self, thread: &str, predicate: impl FnOnce(&str, &String) -> bool) {
        let mut state = self.lock();
        let Some(slot) = state.slots.get_mut(thread) else {
            return;
        };
        if slot
            .current
            .as_ref()
            .is_some_and(|current| predicate(thread, &current.turn))
        {
            // History replacement retires correlation without fabricating END.
            slot.current = None;
        }
    }

    pub(super) fn clear(&self) {
        self.lock().clear_threads();
    }
}

impl SharedCodexAppServer {
    pub(crate) fn qualified_execution_terminal(
        &self,
        target: &ExecutionEndTarget,
    ) -> Option<Value> {
        self.native_turns.qualified_terminal(target)
    }
}
