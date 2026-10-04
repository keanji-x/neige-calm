use std::path::Path;

use calm_types::runtime::TimestampMs;
use calm_types::worker::{ExitEvidence, ExitSource, Liveness};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SupervisorProbe {
    Running,
    Exited,
    Unknown,
}

pub(crate) async fn probe_terminal_liveness(
    supervisor_sock: &Path,
    terminal_run_id: Option<&str>,
    now_ms: TimestampMs,
) -> Liveness {
    let Some(terminal_run_id) = terminal_run_id else {
        return Liveness::Unknown { since_ms: now_ms };
    };
    liveness_from_supervisor_probe(
        probe_supervisor_proc(supervisor_sock, &format!("term:{terminal_run_id}")).await,
        now_ms,
    )
}

pub(crate) fn liveness_from_supervisor_probe(
    probe: SupervisorProbe,
    now_ms: TimestampMs,
) -> Liveness {
    match probe {
        SupervisorProbe::Running => Liveness::Alive {
            active_turn_id: None,
        },
        SupervisorProbe::Exited => Liveness::Exited {
            evidence: ExitEvidence {
                exit_code: Some(-1),
                signal_killed: false,
                observed_at_ms: now_ms,
                source: ExitSource::Probe,
            },
        },
        SupervisorProbe::Unknown => Liveness::Unknown { since_ms: now_ms },
    }
}

#[cfg(unix)]
async fn probe_supervisor_proc(supervisor_sock: &Path, proc_id: &str) -> SupervisorProbe {
    use calm_session::control::{ControlMsg, ControlReply, ProbeRequest};
    use calm_session::{read_frame, write_frame};
    use tokio::net::UnixStream;

    let mut stream = match UnixStream::connect(supervisor_sock).await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::debug!(
                supervisor_sock = %supervisor_sock.display(),
                error = %e,
                "proc-supervisor probe connect failed"
            );
            return SupervisorProbe::Unknown;
        }
    };
    if let Err(e) = write_frame(
        &mut stream,
        &ControlMsg::Probe(ProbeRequest {
            proc_id: proc_id.to_string(),
        }),
    )
    .await
    {
        tracing::debug!(
            supervisor_sock = %supervisor_sock.display(),
            proc_id,
            error = %e,
            "proc-supervisor probe write failed"
        );
        return SupervisorProbe::Unknown;
    }

    match read_frame::<ControlReply, _>(&mut stream).await {
        Ok(ControlReply::ProbeOk { proc_running, .. }) => {
            if proc_running {
                SupervisorProbe::Running
            } else {
                SupervisorProbe::Exited
            }
        }
        Ok(ControlReply::Error { message, .. }) => {
            tracing::debug!(
                supervisor_sock = %supervisor_sock.display(),
                proc_id,
                message,
                "proc-supervisor probe returned error"
            );
            SupervisorProbe::Unknown
        }
        Ok(other) => {
            tracing::debug!(
                supervisor_sock = %supervisor_sock.display(),
                proc_id,
                reply = ?other,
                "proc-supervisor probe returned unexpected reply"
            );
            SupervisorProbe::Unknown
        }
        Err(e) => {
            tracing::debug!(
                supervisor_sock = %supervisor_sock.display(),
                proc_id,
                error = %e,
                "proc-supervisor probe read failed"
            );
            SupervisorProbe::Unknown
        }
    }
}

#[cfg(not(unix))]
async fn probe_supervisor_proc(_supervisor_sock: &Path, _proc_id: &str) -> SupervisorProbe {
    SupervisorProbe::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_probe_running_maps_alive() {
        assert_eq!(
            liveness_from_supervisor_probe(SupervisorProbe::Running, 7),
            Liveness::Alive {
                active_turn_id: None
            }
        );
    }

    #[test]
    fn supervisor_probe_exited_maps_probe_evidence() {
        assert_eq!(
            liveness_from_supervisor_probe(SupervisorProbe::Exited, 7),
            Liveness::Exited {
                evidence: ExitEvidence {
                    exit_code: Some(-1),
                    signal_killed: false,
                    observed_at_ms: 7,
                    source: ExitSource::Probe,
                }
            }
        );
    }

    #[test]
    fn supervisor_probe_unknown_maps_unknown_since_now() {
        assert_eq!(
            liveness_from_supervisor_probe(SupervisorProbe::Unknown, 7),
            Liveness::Unknown { since_ms: 7 }
        );
    }
}
