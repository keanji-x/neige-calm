//! `<claude_binary> auth status --json` (#1817): whether the dedicated `CLAUDE_CONFIG_DIR` is
//! logged in. Only the boolean `loggedIn` is read; every other field the CLI prints (auth method,
//! account email, organization) is skipped by the decoder and never logged or returned.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::readiness_command;

/// How long `auth status` may take; it answers from the local config dir.
pub const AUTH_STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// The one field neige reads. Unknown fields are skipped without being stored.
#[derive(Deserialize)]
struct AuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
}

/// Run `auth status --json` with `env` (the spawn's own allowlisted environment, readiness marker
/// included) through [`readiness_command::run`] and answer `loggedIn`. `Err` is neige's own
/// sentence for why the answer is unknown.
pub async fn logged_in(
    binary: &Path,
    env: &[(String, OsString)],
    timeout: Duration,
) -> Result<bool, String> {
    let command_name = format!("{} auth status --json", binary.display());
    let (_status, bytes) =
        readiness_command::run(binary, &["auth", "status", "--json"], env, timeout)
            .await
            .map_err(|failure| format!("{command_name} {failure}"))?;
    // The exit status is not consulted: `loggedIn` is the answer either way.
    serde_json::from_slice::<AuthStatus>(&bytes)
        .map(|status| status.logged_in)
        .map_err(|_| format!("{command_name} printed no boolean `loggedIn`"))
}
