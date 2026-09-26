//! One bounded run of `<claude_binary> <args>` for the readiness checks (`--version`,
//! `auth status --json`, #1817): the given allowlisted environment only, no stdin, stderr dropped,
//! stdout capped, and on a timeout or an over-long answer the child is killed and reaped before
//! this returns.

use std::ffi::OsString;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;

/// More than any readiness answer; a longer one is refused, not read.
pub const STDOUT_LIMIT: u64 = 64 * 1024;

/// Why a run produced no answer; `Display` is the detail after the command's name.
#[derive(Debug)]
pub enum RunFailure {
    Spawn(std::io::Error),
    Read(std::io::Error),
    Wait(std::io::Error),
    TooLong,
    TimedOut(Duration),
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "could not start: {error}"),
            Self::Read(error) => write!(f, "reading its output failed: {error}"),
            Self::Wait(error) => write!(f, "waiting for it failed: {error}"),
            Self::TooLong => write!(f, "printed more than {STDOUT_LIMIT} bytes"),
            Self::TimedOut(timeout) => {
                write!(f, "did not answer within {} s", timeout.as_secs_f64())
            }
        }
    }
}

/// The exit status and stdout of `binary args`, or why there is none.
pub async fn run(
    binary: &Path,
    args: &[&str],
    env: &[(String, OsString)],
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>), RunFailure> {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A cancelled caller still kills the child; the paths below also reap it.
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(RunFailure::Spawn)?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let read = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(STDOUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(RunFailure::Read)?;
        if bytes.len() as u64 > STDOUT_LIMIT {
            return Err(RunFailure::TooLong);
        }
        let status = child.wait().await.map_err(RunFailure::Wait)?;
        Ok((status, bytes))
    };
    let outcome = match tokio::time::timeout(timeout, read).await {
        Ok(outcome) => outcome,
        Err(_) => Err(RunFailure::TimedOut(timeout)),
    };
    if outcome.is_err() {
        // Kill and reap: a hung or flooding child must not outlive the check.
        let _ = child.kill().await;
    }
    outcome
}
