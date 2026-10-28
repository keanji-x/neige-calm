//! Qualified late facts remain on their original input association, never a new
//! start candidate. Retired associations confer no current control authority.
use super::*;

impl ThreadSlot {
    pub(super) fn retain_observed(&mut self, mut old: NativeExecution) {
        old.retire_control_hint();
        match self.retired.entry(old.turn.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(old);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let prior = entry.get_mut();
                if matches!(prior.input, NativeInput::Legacy) {
                    prior.input = old.input;
                }
                prior.ended |= old.ended;
                if prior.terminal.is_none() {
                    prior.terminal = old.terminal;
                }
            }
        }
    }

    pub(super) fn record_retired_acceptance(
        &mut self,
        source: &Weak<CodexAppServer>,
        ticket: &Arc<StartTicket>,
        turn: &str,
    ) {
        if let Some(current) = &mut self.current
            && current.turn == turn
        {
            current.input = ticket.input.clone();
            return;
        }
        let mut old = self
            .pending
            .as_mut()
            .and_then(|pending| {
                if pending
                    .candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.turn == turn)
                {
                    pending.candidate.take()
                } else {
                    pending.other_candidates.remove(turn)
                }
            })
            .unwrap_or_else(|| NativeExecution {
                turn: turn.into(),
                source: source.clone(),
                input: ticket.input.clone(),
                ended: false,
                terminal: None,
                control_visible: false,
            });
        old.input = ticket.input.clone();
        self.retain_observed(old);
        if self.current.is_none()
            && self.pending.is_none()
            && let Some(mut known) = self.retired.remove(turn)
        {
            known.control_visible = !known.ended;
            self.current = Some(known);
        }
    }
}

impl NativeTurns {
    pub(in crate::shared_codex_appserver) fn accepted(
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
            slot.record_retired_acceptance(&Arc::downgrade(client), ticket, turn);
            return;
        }
        let mut pending = slot.pending.take().expect("matching actual pending start");
        let mut candidates = pending.other_candidates;
        if let Some(candidate) = pending.candidate.take() {
            candidates.insert(candidate.turn.clone(), candidate);
        }
        let matching = candidates.remove(turn);
        slot.unresolved_candidates = candidates
            .values()
            .filter(|candidate| !candidate.ended)
            .map(|candidate| candidate.turn.clone())
            .collect();
        slot.unresolved = !slot.unresolved_candidates.is_empty();
        for candidate in candidates.into_values() {
            slot.retain_observed(candidate);
        }
        let next = NativeExecution {
            turn: turn.into(),
            source: Arc::downgrade(client),
            input: ticket.input.clone(),
            ended: matching.as_ref().is_some_and(|candidate| candidate.ended),
            control_visible: matching
                .as_ref()
                .is_none_or(|candidate| candidate.control_visible),
            terminal: matching.and_then(|candidate| candidate.terminal),
        };
        if let Some(old) = slot.current.replace(next) {
            slot.retain_observed(old);
        }
    }

    pub(in crate::shared_codex_appserver) fn qualified_terminal(
        &self,
        target: &ExecutionEndTarget,
    ) -> Option<Value> {
        let ExecutionEndScope::NativeTurn {
            thread_id,
            native_turn_id,
        } = &target.scope
        else {
            return None;
        };
        let state = self.lock();
        if !state
            .source
            .as_ref()
            .is_some_and(|source| state.is_current(source))
        {
            return None;
        }
        let slot = state.slots.get(thread_id)?;
        // An exact ACK/END remains a fact while other observed turns are active.
        // Their activity still fences every new start and control operation.
        if (slot.unresolved && slot.unresolved_candidates.is_empty())
            || slot.pending.is_some()
            || slot.steering.is_some()
        {
            return None;
        }
        slot.current
            .as_ref()
            .filter(|current| current.turn == *native_turn_id)
            .or_else(|| slot.retired.get(native_turn_id))
            .filter(|current| current.ended && current.input.matches(target))
            .and_then(|current| current.terminal.clone())
    }
}

impl NativeTurns {
    fn recovery_matches(
        state: &State,
        expected: &SettlementFence,
        target: &ExecutionEndTarget,
    ) -> bool {
        let Some(slot) = state.slots.get(&expected.thread) else {
            return false;
        };
        let ExecutionEndScope::NativeTurn {
            thread_id,
            native_turn_id,
        } = &target.scope
        else {
            return false;
        };
        state.is_current(&expected.source)
            && thread_id == &expected.thread
            && !state.detaching.contains_key(thread_id)
            && Arc::ptr_eq(&slot.activity, &expected.activity)
            && expected.pending.is_none()
            && slot.pending.is_none()
            && slot.steering.is_none()
            && !expected.ambiguous
            && !expected.unresolved
            && !slot.unresolved
            && slot.current.as_ref().map(|c| &c.turn) == expected.current.as_ref()
            && slot
                .current
                .as_ref()
                .is_none_or(|c| c.turn == *native_turn_id && c.ended && c.input.matches(target))
            && slot
                .retired
                .get(native_turn_id)
                .is_none_or(|old| old.input.matches(target))
    }
    pub(in crate::shared_codex_appserver) fn recovery_fence_matches(
        &self,
        expected: &SettlementFence,
        target: &ExecutionEndTarget,
    ) -> bool {
        Self::recovery_matches(&self.lock(), expected, target)
    }
    /// A past completion has no reconstructed current start or control authority.
    pub(in crate::shared_codex_appserver) fn retain_canonical_recovery(
        &self,
        expected: &SettlementFence,
        target: &ExecutionEndTarget,
        terminal: &Value,
    ) -> bool {
        let mut state = self.lock();
        if !Self::recovery_matches(&state, expected, target) {
            return false;
        };
        let ExecutionEndScope::NativeTurn { native_turn_id, .. } = &target.scope else {
            return false;
        };
        let input = NativeInput::canonical(
            CodexHarnessBinding::new(&target.card_id, &target.track_id, &target.worker_session_id)
                .expect("validated recovery identity"),
            &target.logical_turn_id,
            &target.proof,
            false,
        );
        state
            .slots
            .get_mut(&expected.thread)
            .expect("checked recovery slot")
            .retain_observed(NativeExecution {
                turn: native_turn_id.clone(),
                source: expected.source.clone(),
                input,
                ended: true,
                terminal: Some(terminal_fact(native_turn_id, terminal)),
                control_visible: false,
            });
        true
    }
}
