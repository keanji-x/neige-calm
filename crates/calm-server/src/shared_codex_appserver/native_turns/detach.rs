//! Native-owner detach leases. No mutex survives an await or SQL operation.
use super::*;

/// The same issuance lease protects live idleness and explicit cold recovery.
/// Evidence is minted by the native owner; neither variant manufactures END.
enum DetachEvidence {
    ObservedEnd {
        source: Weak<CodexAppServer>,
        generation: u64,
    },
    VerifiedTerminalRead {
        fence: SettlementFence,
        turn: String,
    },
}
impl DetachEvidence {
    fn matches(&self, state: &State, thread: &str) -> bool {
        match self {
            Self::ObservedEnd { source, generation } => {
                state.source_generation == *generation
                    && state.is_current(source)
                    && state.settled_on(thread, source)
            }
            Self::VerifiedTerminalRead { fence, turn } => {
                // A current-connection read re-proved the historical END.
                // Retain its turn/activity fences without treating the old
                // observation connection as current execution authority.
                state.is_current(&fence.source)
                    && fence.pending.is_none()
                    && !fence.ambiguous
                    && !fence.unresolved
                    && state.slots.get(thread).is_some_and(|slot| {
                        Arc::ptr_eq(&slot.activity, &fence.activity)
                            && slot.pending.is_none()
                            && slot.steering.is_none()
                            && !slot.unresolved
                            && slot.current.as_ref().map(|c| &c.turn) == fence.current.as_ref()
                            && slot
                                .current
                                .as_ref()
                                .is_none_or(|c| c.turn == *turn && c.ended)
                    })
            }
        }
    }
}
/// Fences one settled thread through commit; every source change invalidates it.
pub(crate) struct IdleDetachGuard {
    owner: Arc<NativeTurns>,
    thread: String,
    identity: Arc<()>,
    evidence: DetachEvidence,
}
impl IdleDetachGuard {
    pub(crate) fn recheck(&self) -> bool {
        let state = self.owner.lock();
        self.evidence.matches(&state, &self.thread)
            && state
                .detaching
                .get(&self.thread)
                .is_some_and(|(id, _)| Arc::ptr_eq(id, &self.identity))
    }
}
impl Drop for IdleDetachGuard {
    fn drop(&mut self) {
        let mut state = self.owner.lock();
        if state
            .detaching
            .get(&self.thread)
            .is_some_and(|(id, _)| Arc::ptr_eq(id, &self.identity))
        {
            let (_, retire) = state.detaching.remove(&self.thread).expect("held park");
            if retire {
                state.slots.remove(&self.thread);
            }
        }
    }
}
impl NativeTurns {
    fn park_detach(
        self: &Arc<Self>,
        thread: &str,
        evidence: DetachEvidence,
    ) -> Option<IdleDetachGuard> {
        let mut state = self.lock();
        if state.detaching.contains_key(thread) || !evidence.matches(&state, thread) {
            return None;
        }
        let identity = Arc::new(());
        state
            .detaching
            .insert(thread.into(), (identity.clone(), false));
        Some(IdleDetachGuard {
            owner: self.clone(),
            thread: thread.into(),
            identity,
            evidence,
        })
    }
    pub(in crate::shared_codex_appserver) fn idle_for_detach(
        self: &Arc<Self>,
        thread: &str,
    ) -> Option<IdleDetachGuard> {
        let state = self.lock();
        let evidence = DetachEvidence::ObservedEnd {
            source: state.source.clone()?,
            generation: state.source_generation,
        };
        drop(state);
        self.park_detach(thread, evidence)
    }
    /// Called only after an exact failed terminal read and an idle native resume.
    pub(in crate::shared_codex_appserver) fn park_terminal_recovery(
        self: &Arc<Self>,
        fence: SettlementFence,
        turn: &str,
    ) -> Result<IdleDetachGuard> {
        let thread = fence.thread.clone();
        self.park_detach(
            &thread,
            DetachEvidence::VerifiedTerminalRead {
                fence,
                turn: turn.into(),
            },
        )
        .ok_or_else(|| CalmError::Conflict("native recovery is not quiescent".into()))
    }
}
impl State {
    fn settled_on(&self, thread: &str, source: &Weak<CodexAppServer>) -> bool {
        self.slots.get(thread).is_some_and(|slot| {
            !slot.unresolved
                && slot.pending.is_none()
                && slot.steering.is_none()
                && slot
                    .current
                    .as_ref()
                    .is_some_and(|current| current.ended && current.source.ptr_eq(source))
        })
    }
    pub(super) fn retire_source(&mut self, source: &Weak<CodexAppServer>) {
        if self.owns_source(source) {
            self.invalidate();
        }
    }
    pub(super) fn publish_source(&mut self, client: &Arc<CodexAppServer>) {
        self.invalidate();
        self.source = Some(Arc::downgrade(client));
    }
    pub(super) fn invalidate(&mut self) {
        self.source_generation = self.source_generation.wrapping_add(1);
        self.invalidate_now();
    }
    pub(super) fn retire_thread(&mut self, thread: &str) -> Option<()> {
        if let Some((_, retire)) = self.detaching.get_mut(thread) {
            *retire = true;
            None
        } else {
            self.slots.remove(thread).map(|_| ())
        }
    }
    pub(super) fn clear_threads(&mut self) {
        self.detaching
            .values_mut()
            .for_each(|(_, retire)| *retire = true);
        self.slots
            .retain(|thread, _| self.detaching.contains_key(thread));
    }
    fn invalidate_now(&mut self) {
        self.source = None;
        for slot in self.slots.values_mut() {
            slot.settled_activity = None;
            slot.unresolved_candidates.clear();
            // Retired maintenance is still evidence of an uncertain operation,
            // never current control authority. Keep it across source loss too.
            slot.retired.retain(|_, execution| match execution.input {
                NativeInput::Maintenance(_) => true,
                NativeInput::Legacy | NativeInput::Canonical(_) => false,
            });
            if let Some(current) = &mut slot.current {
                match current.input {
                    NativeInput::Legacy | NativeInput::Canonical(_) => {
                        current.input = NativeInput::Legacy
                    }
                    NativeInput::Maintenance(_) => {}
                }
            }
            if slot.steering.take().is_some() {
                slot.unresolved = true;
            }
            if let Some(pending) = slot.pending.take() {
                slot.unresolved = true;
                match &pending.ticket.input {
                    // Keep this operation's uncertainty, never a reusable grant.
                    NativeInput::Maintenance(_) => slot.pending = Some(pending),
                    NativeInput::Legacy | NativeInput::Canonical(_) => {
                        if slot.current.as_ref().is_none_or(|current| current.ended) {
                            slot.current = pending.candidate;
                        }
                    }
                }
            }
        }
    }
}
