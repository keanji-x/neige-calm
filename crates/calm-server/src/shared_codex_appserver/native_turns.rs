//! One actual current-native slot, including an unresolved start candidate.
//!
//! Source publication/invalidation takes this short lock inside the supervisor
//! core lock. Observers never acquire core while holding it. No native call,
//! repository operation or asynchronous cleanup runs under either lock.
use super::*;
use crate::harness::backend::{ExecutionEndScope, ExecutionEndTarget, ExecutionInput};
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
    Canonical(Arc<CanonicalNativeInput>),
}

pub(crate) struct CanonicalNativeInput {
    binding: CodexHarnessBinding,
    input: ExecutionInput,
}

impl NativeInput {
    pub(crate) fn canonical(
        binding: CodexHarnessBinding,
        logical_turn_id: &calm_types::conversation::LogicalTurnId,
        proof: &calm_types::conversation::SubmissionDispatchProof,
    ) -> Self {
        Self::Canonical(Arc::new(CanonicalNativeInput {
            binding,
            input: ExecutionInput::canonical(logical_turn_id, proof),
        }))
    }

    fn matches(&self, target: &ExecutionEndTarget) -> bool {
        matches!(self, Self::Canonical(actual) if actual.binding.matches(target) && actual.input.matches(target))
    }
}

pub(crate) enum NativeSteer {
    Legacy,
    BudgetControl,
    CanonicalControl(ExecutionEndTarget),
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

pub(super) struct StartTicket {
    input: NativeInput,
}

struct PendingStart {
    ticket: Arc<StartTicket>,
    candidate: Option<NativeExecution>,
    ambiguous: bool,
}

struct NativeExecution {
    turn: String,
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

    fn invalidate(&mut self) {
        self.source = None;
        for slot in self.slots.values_mut() {
            if let Some(current) = &mut slot.current {
                current.input = NativeInput::Legacy;
            }
            if slot.steering.take().is_some() {
                slot.unresolved = true;
            }
            if let Some(pending) = slot.pending.take() {
                slot.unresolved = true;
                if slot.current.as_ref().is_none_or(|current| current.ended) {
                    slot.current = pending.candidate;
                }
            }
        }
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
        if state.owns_source(&self.source) {
            state.invalidate();
        }
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
        let mut state = self.lock();
        state.invalidate();
        // Installed identity alone is not eligibility: every use reads the
        // actual reader and original consumer, including pre-publication exit.
        state.source = Some(Arc::downgrade(client));
    }

    pub(super) fn begin(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        input: NativeInput,
    ) -> Result<Arc<StartTicket>> {
        let mut state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) {
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
        let ticket = Arc::new(StartTicket { input });
        slot.pending = Some(PendingStart {
            ticket: ticket.clone(),
            candidate: None,
            ambiguous: false,
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
        if !state.is_current(&Arc::downgrade(client)) {
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
                && match (input, &current.input) {
                    (NativeSteer::BudgetControl, _) => true,
                    (NativeSteer::CanonicalControl(target), _) => {
                        current.input.matches(target)
                            && matches!(&target.scope, ExecutionEndScope::NativeTurn {thread_id,native_turn_id}
                                if thread_id == thread && native_turn_id == turn)
                    }
                    (NativeSteer::Legacy, NativeInput::Legacy) => true,
                    _ => false,
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

    pub(super) fn accepted(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        ticket: &Arc<StartTicket>,
        turn: &str,
    ) {
        let mut state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) {
            return;
        }
        let Some(slot) = state.slots.get_mut(thread) else {
            return;
        };
        if !slot
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(&pending.ticket, ticket))
        {
            return;
        }
        let pending = slot.pending.take().expect("matching actual pending start");
        slot.unresolved = pending.ambiguous
            || pending
                .candidate
                .as_ref()
                .is_some_and(|candidate| candidate.turn != turn);
        let ended = pending
            .candidate
            .as_ref()
            .is_some_and(|candidate| candidate.turn == turn && candidate.ended);
        let control_visible = pending
            .candidate
            .as_ref()
            .filter(|candidate| candidate.turn == turn)
            .is_none_or(|candidate| candidate.control_visible);
        let terminal = pending
            .candidate
            .as_ref()
            .filter(|candidate| candidate.turn == turn)
            .and_then(|candidate| candidate.terminal.clone());
        slot.current = Some(NativeExecution {
            turn: turn.into(),
            input: ticket.input.clone(),
            ended,
            terminal,
            control_visible,
        });
    }

    pub(super) fn not_sent(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        ticket: &Arc<StartTicket>,
    ) {
        let mut state = self.lock();
        if !state.is_current(&Arc::downgrade(client)) {
            return;
        }
        let Some(slot) = state.slots.get_mut(thread) else {
            return;
        };
        if slot.pending.as_ref().is_some_and(|pending| {
            Arc::ptr_eq(&pending.ticket, ticket) && pending.candidate.is_none()
        }) {
            slot.pending = None;
        }
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
        if let Some(current) = &mut slot.current
            && current.turn == turn
        {
            current.ended |= ended;
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
                Some(_) => pending.ambiguous = true,
                None => {
                    pending.candidate = Some(NativeExecution {
                        turn: turn.into(),
                        input: NativeInput::Legacy,
                        ended,
                        terminal,
                        control_visible: true,
                    })
                }
            }
        } else if !ended {
            slot.current = Some(NativeExecution {
                turn: turn.into(),
                input: NativeInput::Legacy,
                ended: false,
                terminal: None,
                control_visible: true,
            });
        }
        true
    }

    pub(super) fn qualified_terminal(&self, target: &ExecutionEndTarget) -> Option<Value> {
        let ExecutionEndScope::NativeTurn {
            thread_id,
            native_turn_id,
        } = &target.scope
        else {
            return None;
        };
        let state = self.lock();
        let eligible = state
            .source
            .as_ref()
            .is_some_and(|source| state.is_current(source))
            && state.slots.get(thread_id).is_some_and(|slot| {
                !slot.unresolved
                    && slot.pending.is_none()
                    && slot.steering.is_none()
                    && slot.current.as_ref().is_some_and(|current| {
                        current.ended
                            && current.turn == *native_turn_id
                            && current.input.matches(target)
                    })
            });
        eligible
            .then(|| {
                state
                    .slots
                    .get(thread_id)
                    .and_then(|slot| slot.current.as_ref())
                    .and_then(|current| current.terminal.clone())
            })
            .flatten()
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
                "control requires its actual native turn".into(),
            ));
        };
        let state = self.lock();
        let current = state
            .slots
            .get(thread_id)
            .filter(|slot| !slot.unresolved && slot.pending.is_none() && slot.steering.is_none())
            .and_then(|slot| slot.current.as_ref());
        if !state.is_current(&Arc::downgrade(&client))
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
            input: NativeInput::Legacy,
            ended: false,
            terminal: None,
            control_visible: true,
        });
    }

    pub(super) fn remove(&self, thread: &str) -> Option<()> {
        self.lock().slots.remove(thread).map(|_| ())
    }

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
        self.lock().slots.clear();
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
