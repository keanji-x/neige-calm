//! The Claude CLI's model list (#1822): one `initialize` control request is written to
//! `<claude_binary> -p`, stdin is closed, and the CLI answers with a `control_response` whose
//! `response.models` is its own `/model` list, then exits. No user message is sent, so no model is
//! called. Only `models` is decoded: every other key of the answer (the account among them) is
//! skipped by the decoder. A refusal quotes at most a value from inside the `models` list (a
//! repeated `value` or effort level), never anything outside it, so never the account; nothing
//! outside the list reaches a reason, a log line or the cache.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use super::models::ClaudeCatalog;
use super::readiness_command;
use provider::claude::discovery::{catalog_initialize_line, decode_catalog};

/// How long the exchange may take; the pinned CLI answers in 2.6–2.8 s. With `--version` and
/// `auth status` (10 s each) the whole check stays under the web client's 30 s request budget.
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(8);

/// The argv of the exchange. `--setting-sources project` and `settings` are the turn spawn's own,
/// so a settings-level model restriction the turn would meet is in the list. The rest keep the
/// run to the answer: no MCP server, no slash commands, and no session written.
pub(crate) fn argv(settings: &str) -> Vec<&str> {
    vec![
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "project",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--settings",
        settings,
    ]
}

/// Run the exchange with `env` (the spawn's own allowlisted environment, without a marker or MCP token) in `cwd` through [`readiness_command::run_with_input`], and parse the list strictly.
/// `Err` is neige's own sentence for why there is no list.
pub async fn fetch(
    binary: &Path,
    env: &[(String, OsString)],
    cwd: &Path,
    timeout: Duration,
) -> Result<ClaudeCatalog, String> {
    let command_name = format!("{} -p (initialize)", binary.display());
    let settings = super::spawn::settings_json();
    let line = catalog_initialize_line();
    let (status, stdout) = readiness_command::run_with_input(
        binary,
        &argv(&settings),
        readiness_command::Input {
            stdin: line.as_bytes(),
            cwd,
        },
        env,
        timeout,
    )
    .await
    .map_err(|failure| format!("{command_name} {failure}"))?;
    if !status.success() {
        return Err(format!("{command_name} exited with {status}"));
    }
    let catalog = decode_catalog(&stdout)
        .map_err(|why| format!("{command_name} printed no usable model list: {why}"))?;
    Ok(ClaudeCatalog {
        default: catalog.default,
        models: catalog.models,
        fetched_at_ms: crate::model::now_ms(),
    })
}
