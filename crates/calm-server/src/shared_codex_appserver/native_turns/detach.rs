//! Native-owner detach leases. No mutex survives an await or SQL operation.
use super::*;

/// Fences one settled thread through commit; every source change invalidates it.
pub(crate) struct IdleDetachGuard<'a> {
    owner: &'a NativeTurns,
    thread: String,
    source: Weak<CodexAppServer>,
    source_generation: u64,
    identity: Arc<()>,
}
impl IdleDetachGuard<'_> {
    pub(crate) fn recheck(&self) -> bool {
        let state = self.owner.lock();
        state.source_generation == self.source_generation
            && state.is_current(&self.source)
            && state
                .detaching
                .get(&self.thread)
                .is_some_and(|(id, _)| Arc::ptr_eq(id, &self.identity))
            && state.settled_on(&self.thread, &self.source)
    }
}
impl Drop for IdleDetachGuard<'_> {
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
    pub(in crate::shared_codex_appserver) fn idle_for_detach(
        &self,
        thread: &str,
    ) -> Option<IdleDetachGuard<'_>> {
        let mut state = self.lock();
        let source = state.source.clone()?;
        let idle = state.is_current(&source)
            && !state.detaching.contains_key(thread)
            && state.settled_on(thread, &source);
        if !idle {
            return None;
        }
        let identity = Arc::new(());
        state
            .detaching
            .insert(thread.into(), (identity.clone(), false));
        Some(IdleDetachGuard {
            owner: self,
            thread: thread.into(),
            source,
            source_generation: state.source_generation,
            identity,
        })
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
            slot.retired.clear();
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
