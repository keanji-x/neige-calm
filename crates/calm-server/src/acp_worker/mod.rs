//! Task role policy over the common ACP host. No conversation receipts or Planner permissions.
mod adapter;
pub(crate) mod admission;
mod attachment;
mod flow;
pub(crate) mod lifecycle;
mod permission;
mod runtime;
mod session;
use crate::acp_session::driver::CancelRequest;
pub use adapter::AcpWorkerAdapter;
pub use lifecycle::WorkerEnd;
pub(crate) use lifecycle::pending_finalize_tx;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
#[derive(Default)]
pub(crate) struct Workers(Mutex<HashMap<String, Arc<Control>>>);
pub(crate) struct Control {
    turn: String,
    cancel: watch::Sender<CancelRequest>,
    pub(crate) settled: watch::Receiver<bool>,
    authorized: Arc<std::sync::atomic::AtomicBool>,
    admission: admission::Admission,
    ctx: crate::operation::SpawnCtx,
    native: String,
    cleanup: crate::acp_session::server::CleanupWitness,
    round_ended: Arc<std::sync::atomic::AtomicBool>,
}
impl Workers {
    pub(crate) fn get(&self, session: &str) -> Option<Arc<Control>> {
        self.0.lock().expect("ACP workers").get(session).cloned()
    }
    pub(crate) fn active_turn(&self, session: &str) -> Option<Option<String>> {
        self.get(session).map(|control| {
            if *control.settled.borrow() {
                None
            } else {
                Some(control.turn.clone())
            }
        })
    }
}

impl std::fmt::Debug for Workers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpWorkers")
            .field("sessions", &self.0.lock().expect("ACP workers").len())
            .finish()
    }
}
