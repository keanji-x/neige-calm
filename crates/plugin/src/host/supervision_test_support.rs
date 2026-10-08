//! One-shot supervisor handshakes, scoped to one host and one run instance.
use super::*;
use tokio::sync::oneshot;

/// A controlled pause with an arrival notification. Dropping `resume` also releases it.
pub struct SupervisorPause {
    pub reached: oneshot::Receiver<()>,
    pub resume: oneshot::Sender<()>,
}

pub(super) struct Pause {
    reached: oneshot::Sender<()>,
    resume: oneshot::Receiver<()>,
}

impl Pause {
    fn pair() -> (Self, SupervisorPause) {
        let (reached_tx, reached) = oneshot::channel();
        let (resume, resume_rx) = oneshot::channel();
        (
            Self {
                reached: reached_tx,
                resume: resume_rx,
            },
            SupervisorPause { reached, resume },
        )
    }

    pub(super) async fn wait(self) {
        let _ = self.reached.send(());
        let _ = self.resume.await;
    }
}

/// Notifications for the selected crash only; later crashes cannot reuse these gates.
pub struct SupervisorHandshake {
    pub crashed_under_guard: SupervisorPause,
    pub guard_released: SupervisorPause,
    pub before_respawn_lock: oneshot::Receiver<()>,
}

pub(super) struct ArmedHandshake {
    id: String,
    run_epoch: u64,
    pub(super) crashed_under_guard: Option<Pause>,
    pub(super) guard_released: Pause,
    pub(super) before_respawn_lock: oneshot::Sender<()>,
}

impl<E: ErrorFactory> PluginHost<E> {
    /// Arm before spawning on an otherwise idle test host. The next allocated run
    /// epoch and `id` must both match; another plugin/run cannot consume the gates.
    pub fn arm_next_run_supervisor(&self, id: &str) -> SupervisorHandshake {
        let (crashed, crashed_under_guard) = Pause::pair();
        let (released, guard_released) = Pause::pair();
        let (before_respawn_lock, receiver) = oneshot::channel();
        let mut slot = self.supervisor_handshake.lock().unwrap();
        assert!(slot.is_none(), "only one supervisor handshake may be armed");
        *slot = Some(ArmedHandshake {
            id: id.to_owned(),
            run_epoch: self.run_epoch_seq.load(std::sync::atomic::Ordering::SeqCst),
            crashed_under_guard: Some(crashed),
            guard_released: released,
            before_respawn_lock,
        });
        SupervisorHandshake {
            crashed_under_guard,
            guard_released,
            before_respawn_lock: receiver,
        }
    }

    pub(super) fn take_supervisor_handshake(
        &self,
        id: &str,
        run_epoch: u64,
    ) -> Option<ArmedHandshake> {
        let mut slot = self.supervisor_handshake.lock().unwrap();
        if slot
            .as_ref()
            .is_some_and(|h| h.id == id && h.run_epoch == run_epoch)
        {
            slot.take()
        } else {
            None
        }
    }
}
