//! The Claude Planner process and credential lifecycle's revoke-then-sweep steps (design #1791
//! §5.1 items 1 and 4): null the MCP hashes of every Claude Planner row in scope, then one marker
//! sweep over their ids. After the revocation an old token no longer completes a handshake until
//! that row's next first-turn mint.

use super::config::ClaudePlannerHost;
use super::stop::{SeamPolicy, sweep};
use crate::db::sqlite::{ClaudePlannerScope, claude_planner_revoke_tx};
use crate::db::{RepoEventWrite, write_in_tx_typed};
use crate::error::Result;

async fn revoke(
    repo: &dyn RepoEventWrite,
    scope: ClaudePlannerScope<'static>,
) -> Result<Vec<String>> {
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { Ok(claude_planner_revoke_tx(tx, scope).await?) })
    })
    .await
}

/// Boot, before the MCP listener starts: revoke every Claude Planner credential (an `Err` fails
/// the boot: the listener must not open while an old token authenticates), then sweep every
/// Claude Planner marker and empty the instructions directory; those two only log, because right
/// after boot nothing can retry them and the next spawn's own `stop` fails closed. The boot sweep
/// never consults the fixtures stop-failure seam.
pub async fn boot(repo: &dyn RepoEventWrite, host: &ClaudePlannerHost) -> Result<()> {
    let ids = revoke(repo, ClaudePlannerScope::All).await?;
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    if let Err(error) = sweep(&host.instance, &ids, SeamPolicy::Ignore).await {
        tracing::warn!(
            %error,
            "claude planner boot sweep did not confirm; each first turn stops its own id again"
        );
    }
    match std::fs::read_dir(&host.instructions_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Err(error) = std::fs::remove_file(entry.path()) {
                    tracing::warn!(
                        path = %entry.path().display(),
                        %error,
                        "claude planner boot: could not remove a leftover instructions file"
                    );
                }
            }
        }
        Err(error) => tracing::warn!(
            dir = %host.instructions_dir.display(),
            %error,
            "claude planner boot: could not list the instructions directory"
        ),
    }
    Ok(())
}

/// Before a destructive step on a track (its deletion, its area's deletion, a workspace repoint):
/// revoke then sweep every Claude Planner id of the track, in any state. `Err` aborts the step
/// before anything moves.
pub async fn sweep_track(
    repo: &dyn RepoEventWrite,
    host: &ClaudePlannerHost,
    track_id: &str,
) -> Result<()> {
    let track_id = track_id.to_string();
    let ids = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            Ok(claude_planner_revoke_tx(tx, ClaudePlannerScope::Track(&track_id)).await?)
        })
    })
    .await?;
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    sweep(&host.instance, &ids, SeamPolicy::Consult).await
}
