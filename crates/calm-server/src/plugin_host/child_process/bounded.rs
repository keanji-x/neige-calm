//! One bounded run of a short local command under an absolute deadline: its own process group
//! (so the group sweep on timeout reaches anything it forked), output drained under a cap, then
//! the leader reaped and the group swept. Callers build the command (argv, cwd, environment) and
//! word the failures; the verification gate's sampling commands and the task-replace carry use it.

use std::process::Stdio;

use super::{
    ChildFinishError, SpawnTimedOut, finish_within, read_capped, set_process_group_leader,
    spawn_within,
};

/// Why a bounded run produced no output.
#[derive(Debug)]
pub(crate) enum BoundedRunError {
    /// The command could not be spawned.
    Spawn(std::io::Error),
    /// The deadline passed (before the spawn finished, while draining, or while reaping).
    TimedOut,
    /// The child had no stdout/stderr pipe.
    PipesMissing,
    /// Reading its output failed.
    Drain(std::io::Error),
    /// Waiting for the leader failed.
    Reap(std::io::Error),
    /// stdout or stderr exceeded the cap.
    Oversized,
}

/// Run `command` to completion before `deadline`, capturing at most `cap` bytes of each stream.
/// Past the deadline the child is dropped (`kill_on_drop` + the group sweep).
pub(crate) async fn run_bounded(
    mut command: tokio::process::Command,
    deadline: tokio::time::Instant,
    cap: usize,
) -> Result<std::process::Output, BoundedRunError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    set_process_group_leader(&mut command);
    let mut child = match spawn_within(command, deadline).await {
        Ok(Ok(child)) => child,
        Ok(Err(error)) => return Err(BoundedRunError::Spawn(error)),
        Err(SpawnTimedOut) => return Err(BoundedRunError::TimedOut),
    };
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout(), child.stderr()) else {
        return Err(BoundedRunError::PipesMissing);
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let finished = finish_within(
        deadline,
        async {
            let (o, e) = tokio::join!(
                read_capped(&mut stdout, cap, &mut out),
                read_capped(&mut stderr, cap, &mut err),
            );
            o?;
            e?;
            Ok::<(), std::io::Error>(())
        },
        child.wait_and_release_group(),
    )
    .await;
    let (status, released) = match finished {
        Ok(value) => value,
        Err(ChildFinishError::Drain(error)) => return Err(BoundedRunError::Drain(error)),
        Err(ChildFinishError::TimedOut) => return Err(BoundedRunError::TimedOut),
    };
    released.sweep();
    let status = status.map_err(BoundedRunError::Reap)?;
    if out.len() > cap || err.len() > cap {
        return Err(BoundedRunError::Oversized);
    }
    Ok(std::process::Output {
        status,
        stdout: out,
        stderr: err,
    })
}
