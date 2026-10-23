//! Fixtures interpose only waits and I/O faults; production admission still runs.
use super::*;

pub(super) async fn prepare_writer(shared: &Shared, slot: &Arc<TurnSlot>) {
    let (flush, wait) = {
        let hooks = shared.hooks.lock().expect("hooks");
        (
            hooks.user_flush_failure.clone(),
            hooks.user_writer_wait.clone(),
        )
    };
    if let Some(pause) = flush {
        slot.stdin
            .lock()
            .await
            .as_mut()
            .expect("open stdin")
            .flush_failure = Some(Box::pin(async move {
            pause.entered.notify_one();
            pause.release.notified().await;
            Err(std::io::Error::other(
                "fixture flush failure after consumed line",
            ))
        }));
    }
    if let Some(pause) = wait {
        let slot = slot.clone();
        let (entered, blocked) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _writer = slot.stdin.lock().await;
            let _ = entered.send(());
            pause.entered.notify_one();
            pause.release.notified().await;
        });
        blocked.await.expect("writer blocker acquired mutex");
    }
}

#[cfg(test)]
impl ClaudePlannerSession {
    pub(crate) fn fail_user_flush_for_test(&self, pause: SettlePause) {
        self.shared.hooks.lock().expect("hooks").user_flush_failure = Some(pause);
    }
    pub(crate) fn hold_user_writer_for_test(&self, pause: SettlePause) {
        self.shared.hooks.lock().expect("hooks").user_writer_wait = Some(pause);
    }
}

pub(super) async fn after_install(shared: &Shared) {
    let pause = shared.hooks.lock().expect("hooks").dispatch_result.clone();
    if let Some(pause) = pause {
        pause.entered.notify_one();
        pause.release.notified().await;
    }
}

#[cfg(test)]
impl ClaudePlannerSession {
    pub(crate) fn hold_dispatch_result_for_test(&self, pause: SettlePause) {
        self.shared.hooks.lock().expect("hooks").dispatch_result = Some(pause);
    }
}

#[cfg(test)]
impl ClaudePlannerSession {
    pub(crate) fn control_state_for_test(&self) -> (bool, bool) {
        let state = self.shared.state();
        let slot = &state.active.as_ref().expect("running execution").slot;
        (slot.cause().is_some(), slot.stop_armed())
    }

    pub(crate) async fn block_control_flush_for_test(&self, pause: SettlePause) {
        let slot = self.shared.state().active.as_ref().unwrap().slot.clone();
        slot.stdin.lock().await.as_mut().unwrap().flush_failure = Some(Box::pin(async move {
            pause.entered.notify_one();
            pause.release.notified().await;
            Err(std::io::Error::other("control flush failed"))
        }));
    }
}

#[cfg(test)]
impl ClaudePlannerSession {
    pub(crate) fn set_instructions_fault_for_test(&self, fault: Option<spawn::InstructionsFault>) {
        self.shared.hooks.lock().expect("hooks").instructions_fault = fault;
    }
}

impl ClaudePlannerSession {
    pub fn finish_old_turn_for_test(&self, turn: &str) {
        let slot = self.shared.state().active.as_ref().map(|a| a.slot.clone());
        if let Some(slot) = slot {
            self.shared.finish_turn(turn, &slot, []);
        }
    }
}

impl ClaudePlannerSession {
    pub async fn write_control_response_for_test(&self) -> Result<()> {
        let slot = self
            .shared
            .state()
            .active
            .as_ref()
            .map(|active| active.slot.clone())
            .expect("active turn");
        super::super::approvals::write_response(
            &slot,
            &provider::claude::protocol::ControlResponseOut::new(
                provider::claude::protocol::ControlResponseOutBody::Error {
                    request_id: "late-control".into(),
                    error: "unsupported control".into(),
                },
            ),
        )
        .await
    }
}
