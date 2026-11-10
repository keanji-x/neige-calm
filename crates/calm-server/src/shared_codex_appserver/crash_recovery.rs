//! Watcher death evidence permits in-memory revocation, never process or durable effects.
use super::*;

impl SharedCodexAppServer {
    /// Only the crash watcher calls this, after a successful Child wait or a positive
    /// external exit observation. Generation equality in an ordinary replace call
    /// carries no such evidence. Refusal preserves the durable row and endpoint.
    pub(super) async fn fail_current_crashed_incarnation(
        self: &Arc<Self>,
        generation: u64,
        error: &CalmError,
    ) {
        let _serial = self.transition_serial.lock().await;
        let still_installed = {
            let core = self.core.lock().await;
            ReplacePrecondition::GenerationIs(generation).allows(&core)
        };
        if still_installed {
            self.fail_in_memory_and_arm_heal(error.to_string(), FailureClass::Persistent)
                .await;
        }
    }
}

/// Observe only the watched incarnation, installed during this supervisor's boot.
/// A missing identity read is unknown; it cannot be treated as an exit. This
/// proves no absence of group members and authorizes no cleanup or row write.
pub(super) fn observed_pid_exited(pid: i32, expected_start: u64) -> bool {
    match observe_process(pid) {
        ProcessObservation::Absent => true,
        ProcessObservation::Present {
            start_time,
            is_zombie,
        } => is_zombie || start_time != expected_start,
        ProcessObservation::Unknown => false,
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::observed_pid_exited;
    use crate::proc_identity::read_proc_start_time;

    struct OwnedChild(std::process::Child);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            self.0.wait().expect("reap owned watcher test child");
        }
    }

    #[test]
    fn watched_live_incarnation_is_retained_and_reaped_incarnation_has_exited() {
        let mut child = OwnedChild(
            std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("spawn owned watcher test child"),
        );
        let pid = child.0.id() as i32;
        let start = read_proc_start_time(pid).expect("native process birth");
        assert!(!observed_pid_exited(pid, start));
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(observed_pid_exited(pid, start));
    }
}
