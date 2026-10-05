//! #2087 B5: an issue comment submitted before the built-in rename replays after it.
use super::*;
use crate::builtin_plugins::dev::git_actions::lower_for_caller;
use crate::mcp_server::transport::{PluginForgePayload, submit_forge_action_with_key};
use crate::plugin_host::forge_caller::ForgeCallerScope;
use sha2::{Digest, Sha256};

const OLD_PLUGIN_ID: &str = "dev.neige.git-forge"; // retired-name: rejection input
const CARD: &str = "card-a";

fn comment_args() -> Value {
    json!({"repo":"owner/repo", "issue":42, "body":"A progress update", "idem":"update-1"})
}

/// Today's lowering of the comment for the renamed caller.
fn current_lowering(track_id: &str) -> Value {
    let caller = ForgeCallerScope {
        plugin_id: crate::builtin_plugins::dev::PLUGIN_ID.into(),
        track_id: track_id.into(),
        card_id: CARD.into(),
    };
    lower_for_caller("gh_issue_comment", &comment_args(), &caller).unwrap()
}

/// argv is outside the semantic hash; a deterministic one lets the action complete offline.
fn runnable(lowered: Value) -> PluginForgePayload {
    let mut payload: PluginForgePayload = serde_json::from_value(lowered).unwrap();
    payload.argv = vec!["/bin/true".into()];
    payload
}

/// The payload origin/main lowered before the rename: the same descriptor, its marker the
/// released `sha256([caller, repo, issue, idem, body])` over the caller's pre-rename id. Built by
/// substituting that marker, so the oracle does not depend on today's marker code.
fn pre_rename_lowering(track_id: &str) -> Value {
    let current = current_lowering(track_id).to_string();
    let tag = "neige:issue-comment:";
    let start = current.find(tag).unwrap() + tag.len();
    let posted = current[start..start + 64].to_string();
    let released = json!([
        {"plugin_id": OLD_PLUGIN_ID, "track_id": track_id, "card_id": CARD},
        "owner/repo", 42, "update-1", "A progress update"
    ]);
    let released = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&released).unwrap())
    );
    serde_json::from_str(&current.replace(&posted, &released)).unwrap()
}

async fn submit(
    fx: &ForgeRuntimeFixture,
    plugin_id: &str,
    payload: PluginForgePayload,
) -> Result<String, String> {
    submit_forge_action_with_key(
        &fx.runtime,
        fx.results.path(),
        plugin_id,
        fx.track_id.clone(),
        CARD.into(),
        fx.cwd.path().to_path_buf(),
        payload,
        new_id(),
    )
    .await
    .unwrap()
    .map(|submitted| submitted.op_id)
}

#[tokio::test]
async fn a_comment_from_before_the_rename_replays_after_migration_0148() {
    let fx = forge_runtime_fixture().await;
    let op_id = submit(
        &fx,
        OLD_PLUGIN_ID,
        runnable(pre_rename_lowering(&fx.track_id)),
    )
    .await
    .expect("pre-rename submit");
    fx.runtime.wait(&op_id).await.unwrap();
    let migration = calm_truth::MIGRATOR
        .iter()
        .find(|m| m.description == "plugin names")
        .expect("the #2087 B5 migration is embedded");
    sqlx::raw_sql(&migration.sql)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let moved = format!(
        "gitforge:{}:{CARD}:gh.issue.comment:[\"owner/repo\",42,\"update-1\"]",
        fx.track_id
    );
    assert_eq!(operation_count_for_idem(&fx.repo, &moved).await, 1);

    let replayed = submit(
        &fx,
        crate::builtin_plugins::dev::PLUGIN_ID,
        runnable(current_lowering(&fx.track_id)),
    )
    .await
    .expect("the same comment after the rename replays, not idempotency_key_reused");
    assert_eq!(replayed, op_id);
    assert_eq!(operation_count_for_idem(&fx.repo, &moved).await, 1);
}
