//! `<claude_binary> auth status --json` (#1817): whether the dedicated `CLAUDE_CONFIG_DIR` is
//! logged in. Only the boolean `loggedIn` is read; every other field the CLI prints (auth method,
//! account email, organization) is skipped by the decoder and never logged or returned.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncReadExt;

/// How long `auth status` may take; it answers from the local config dir.
pub const AUTH_STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// More than the CLI's whole status object; a longer answer is refused, not read.
const STDOUT_LIMIT: u64 = 64 * 1024;

/// The one field neige reads. Unknown fields are skipped without being stored.
#[derive(Deserialize)]
struct AuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
}

/// Run `auth status --json` with `env` (the spawn's own allowlisted environment, readiness marker
/// included) and answer `loggedIn`. `Err` is neige's own sentence for why the answer is unknown.
/// On timeout, or an answer longer than [`STDOUT_LIMIT`], the child is killed and reaped before
/// this returns.
pub async fn logged_in(
    binary: &Path,
    env: &[(String, OsString)],
    timeout: Duration,
) -> Result<bool, String> {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(["auth", "status", "--json"])
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A cancelled caller still kills the child; the paths below also reap it.
        .kill_on_drop(true);
    let command_name = format!("{} auth status --json", binary.display());
    let mut child = command
        .spawn()
        .map_err(|error| format!("{command_name} could not start: {error}"))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let read = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(STDOUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| format!("{command_name}: reading its output failed: {error}"))?;
        if bytes.len() as u64 > STDOUT_LIMIT {
            return Err(format!(
                "{command_name} printed more than {STDOUT_LIMIT} bytes"
            ));
        }
        child
            .wait()
            .await
            .map_err(|error| format!("{command_name}: waiting for it failed: {error}"))?;
        Ok(bytes)
    };
    let outcome = match tokio::time::timeout(timeout, read).await {
        Ok(outcome) => outcome,
        Err(_) => Err(format!(
            "{command_name} did not answer within {} s",
            timeout.as_secs_f64()
        )),
    };
    let bytes = match outcome {
        Ok(bytes) => bytes,
        Err(reason) => {
            // Kill and reap: a hung or flooding child must not outlive the check.
            let _ = child.kill().await;
            return Err(reason);
        }
    };
    // The exit status is not consulted: `loggedIn` is the answer either way.
    serde_json::from_slice::<AuthStatus>(&bytes)
        .map(|status| status.logged_in)
        .map_err(|_| format!("{command_name} printed no boolean `loggedIn`"))
}
