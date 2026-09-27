//! One bounded run of `<claude_binary> <args>` for the readiness checks (`--version`,
//! `auth status --json`, #1817; the `initialize` catalog exchange, #1822): the given allowlisted
//! environment only, stdin either empty or one written input then closed, stderr dropped, stdout
//! capped, and on a timeout or an over-long answer the child is killed and reaped before this
//! returns.

use std::ffi::OsString;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// More than any readiness answer; a longer one is refused, not read. The largest is the
/// `initialize` answer of the catalog exchange, about 4 KiB as the catalog runs it (#1822).
pub const STDOUT_LIMIT: u64 = 64 * 1024;

/// Why a run produced no answer; `Display` is the detail after the command's name.
#[derive(Debug)]
pub enum RunFailure {
    Spawn(std::io::Error),
    Write(std::io::Error),
    Read(std::io::Error),
    Wait(std::io::Error),
    TooLong,
    TimedOut(Duration),
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "could not start: {error}"),
            Self::Write(error) => write!(f, "writing its input failed: {error}"),
            Self::Read(error) => write!(f, "reading its output failed: {error}"),
            Self::Wait(error) => write!(f, "waiting for it failed: {error}"),
            Self::TooLong => write!(f, "printed more than {STDOUT_LIMIT} bytes"),
            Self::TimedOut(timeout) => {
                write!(f, "did not answer within {} s", timeout.as_secs_f64())
            }
        }
    }
}

/// What a run writes to the child's stdin, and where it runs.
pub struct Input<'a> {
    /// Written whole, then stdin is closed.
    pub stdin: &'a [u8],
    pub cwd: &'a Path,
}

/// The exit status and stdout of `binary args` with an empty stdin, or why there is none.
pub async fn run(
    binary: &Path,
    args: &[&str],
    env: &[(String, OsString)],
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>), RunFailure> {
    run_inner(binary, args, None, env, timeout).await
}

/// [`run`], with `input` written to stdin (then closed) and the child started in `input.cwd`.
pub async fn run_with_input(
    binary: &Path,
    args: &[&str],
    input: Input<'_>,
    env: &[(String, OsString)],
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>), RunFailure> {
    run_inner(binary, args, Some(input), env, timeout).await
}

async fn run_inner(
    binary: &Path,
    args: &[&str],
    input: Option<Input<'_>>,
    env: &[(String, OsString)],
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>), RunFailure> {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A cancelled caller still kills the child; the paths below also reap it.
        .kill_on_drop(true);
    if let Some(input) = &input {
        command.current_dir(input.cwd);
    }
    let mut child = command.spawn().map_err(RunFailure::Spawn)?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let stdin = child.stdin.take();
    let read = async {
        if let (Some(mut stdin), Some(input)) = (stdin, &input) {
            // Dropping the handle closes stdin, which tells the CLI no further input follows. A
            // child that exits unread is judged by its exit status and output, not this write.
            match stdin.write_all(input.stdin).await {
                Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => {
                    return Err(RunFailure::Write(error));
                }
                _ => {}
            }
        }
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
