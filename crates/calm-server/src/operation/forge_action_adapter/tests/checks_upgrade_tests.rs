use super::*;
use crate::builtin_plugins::dev::git_actions::lower;
use crate::mcp_server::transport::{ForgeSubmit, PluginForgePayload, submit_forge_action_with_key};
use calm_types::forge_action::semantic_payload_hash;

/// Waits for `release`, then prints `output`: completion is deterministic without a network
/// subprocess. Only volatile argv, which the semantic hash excludes, is replaced.
fn release_argv(fx: &ForgeRuntimeFixture, output: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "while [ ! -f \"$1\" ]; do [ -d \"${{1%/*}}\" ] || exit 1; sleep 0.05; done; printf '%s\\n' '{output}'"
        ),
        "sh".into(),
        fx.cwd.path().join("release").display().to_string(),
    ]
}

const CURRENT_OUTPUT: &str = "{\"conclusion\":\"success\",\"mergeable\":\"mergeable\",\"head_sha\":\"historical-head\",\"snapshot\":{\"head_sha\":\"historical-head\",\"mergeable\":\"mergeable\"},\"failed_checks\":[]}";

fn lowered(args: Value) -> PluginForgePayload {
    serde_json::from_value(lower("gh_pr_checks", &args).unwrap()).unwrap()
}

fn current_payload() -> PluginForgePayload {
    lowered(json!({"repo": "owner/repo", "pr": 42, "attempt": "upgrade"}))
}

/// A program version other than the current one: the same call with an edited read script.
fn edited_program(mut payload: PluginForgePayload) -> PluginForgePayload {
    let current = semantic_payload_hash(&payload).unwrap();
    let read = payload.probe.as_mut().unwrap();
    read.output_probe_argv.as_mut().unwrap()[2].push_str("\n# an earlier program version\n");
    assert_ne!(semantic_payload_hash(&payload).unwrap(), current);
    payload
}

/// A row of the current key written by another program version of the same call.
fn stored_payload(fx: &ForgeRuntimeFixture) -> PluginForgePayload {
    let mut payload = edited_program(current_payload());
    payload.argv = release_argv(fx, CURRENT_OUTPUT);
    payload
}

/// A row a released version stored under its own key form, with the call that must not replay it.
fn released_rows(fx: &ForgeRuntimeFixture) -> Vec<(PluginForgePayload, Value)> {
    // Frozen semantic descriptor from 162aff1d, before #2129: no snapshot field.
    let mut pre2129: PluginForgePayload =
        serde_json::from_str(include_str!("../checks-pre-2129.json")).unwrap();
    pre2129.argv = release_argv(
        fx,
        "{\"conclusion\":\"success\",\"mergeable\":\"mergeable\",\"head_sha\":\"historical-head\"}",
    );
    // The released text key of {repo:"owner/repo:42", pr:43} also spells the colon-free call
    // {repo:"owner/repo", pr:42, attempt:43}.
    let mut colon_repo = edited_program(lowered(json!({"repo": "owner/repo:42", "pr": 43})));
    colon_repo.idem_key = "gh.pr.checks:owner/repo:42:43".into();
    colon_repo.argv = release_argv(fx, CURRENT_OUTPUT);
    let all_args =
        json!({"repo": "owner/repo", "pr": 42, "attempt": "upgrade", "wait_for_all": true});
    let mut released_all = edited_program(lowered(all_args.clone()));
    released_all.idem_key = r#"gh_pr_checks_all:["owner/repo",42,"upgrade"]"#.into();
    released_all.argv = release_argv(fx, CURRENT_OUTPUT);
    vec![
        (
            colon_repo,
            json!({"repo": "owner/repo", "pr": 42, "attempt": 43}),
        ),
        (
            pre2129,
            json!({"repo": "owner/repo", "pr": 42, "attempt": "upgrade"}),
        ),
        (released_all, all_args),
    ]
}

async fn submit(fx: &ForgeRuntimeFixture, payload: PluginForgePayload) -> String {
    submit_as(fx, "dev", &fx.track_id, "caller", payload)
        .await
        .expect("same checks call must retrieve the stored operation")
}

async fn submit_as(
    fx: &ForgeRuntimeFixture,
    plugin: &str,
    track: &str,
    card: &str,
    payload: PluginForgePayload,
) -> Result<String, String> {
    submit_forge_action_with_key(
        &fx.runtime,
        fx.results.path(),
        None,
        plugin,
        track.into(),
        card.into(),
        fx.cwd.path().to_path_buf(),
        payload,
        new_id(),
    )
    .await
    .unwrap()
    .and_then(ForgeSubmit::submitted)
    .map(|submitted| submitted.op_id)
}

async fn stored_row(fx: &ForgeRuntimeFixture, idem_key: &str) -> crate::operation::Operation {
    fx.runtime
        .find_by_kind_and_idempotency(
            FORGE_ACTION_KIND,
            &format!("dev:{}:caller:{idem_key}", fx.track_id),
        )
        .await
        .unwrap()
        .unwrap()
}

/// Submit `stored`, optionally complete it, then repeat the call with `repeated`: the stored
/// operation, receipt and result are reused, never rewritten, and the action runs once.
async fn replays_stored_operation(
    fx: &ForgeRuntimeFixture,
    stored: PluginForgePayload,
    repeated: PluginForgePayload,
    completed: bool,
) -> Option<Value> {
    let idem_key = stored.idem_key.clone();
    assert_eq!(repeated.idem_key, idem_key);
    assert!(repeated.replay_any_version);
    let op_id = submit(fx, stored).await;
    let before = stored_row(fx, &idem_key).await;
    let receipt = crate::mcp_server::forge_receipt::pending(&op_id, Some("forge.pr.checks"));
    let old_result = if completed {
        std::fs::write(fx.cwd.path().join("release"), "").unwrap();
        Some(fx.runtime.wait(&op_id).await.unwrap())
    } else {
        None
    };
    let replayed = submit(fx, repeated).await;
    assert_eq!(replayed, op_id);
    let after = stored_row(fx, &idem_key).await;
    assert_eq!(
        after.payload, before.payload,
        "stored action must not be updated"
    );
    assert_eq!(after.payload_hash, before.payload_hash);
    let event = if let Some(result) = old_result {
        let replayed_result = fx
            .runtime
            .operation_result(&replayed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replayed_result.op_id, result.op_id);
        match (replayed_result.outcome, result.outcome) {
            (
                OperationOutcome::Succeeded { result: replayed },
                OperationOutcome::Succeeded { result: original },
            ) => {
                assert_eq!(replayed, original);
                Some(original["event"].clone())
            }
            other => panic!("stored result must remain successful: {other:?}"),
        }
    } else {
        assert!(
            fx.runtime
                .operation_result(&replayed)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::mcp_server::forge_receipt::pending(&replayed, Some("forge.pr.checks")),
            receipt
        );
        std::fs::write(fx.cwd.path().join("release"), "").unwrap();
        fx.runtime.wait(&op_id).await.unwrap();
        None
    };
    assert_eq!(event_payloads(&fx.repo, "forge.pr.checks").await.len(), 1);
    assert_eq!(
        operation_count_for_idem(&fx.repo, &format!("dev:{}:caller:{idem_key}", fx.track_id)).await,
        1
    );
    event
}

async fn upgraded_checks(completed: bool) {
    let fx = forge_runtime_fixture().await;
    let stored = stored_payload(&fx);
    let event = replays_stored_operation(&fx, stored, current_payload(), completed).await;
    let events = event_payloads(&fx.repo, "forge.pr.checks").await;
    assert!(events[0].get("snapshot").is_some_and(|v| !v.is_null()));
    if let Some(event) = event {
        assert!(event.get("snapshot").is_some());
        assert!(event["snapshot"].get("all_checks_completed").is_none());
    }
}

/// Reverse upgrade: a row stored under a key a released version wrote is never replayed, even
/// when the current call's arguments would spell that released key. The current call starts a
/// new operation and the stored row is left as it was.
async fn released_rows_start_a_new_operation(completed: bool) {
    for index in 0..3 {
        let fx = forge_runtime_fixture().await;
        let mut rows = released_rows(&fx);
        assert_eq!(rows.len(), 3);
        let (stored, args) = rows.swap_remove(index);
        let released_key = stored.idem_key.clone();
        let stored_op = submit(&fx, stored).await;
        if completed {
            std::fs::write(fx.cwd.path().join("release"), "").unwrap();
            fx.runtime.wait(&stored_op).await.unwrap();
        }
        let before = stored_row(&fx, &released_key).await;
        let mut current = lowered(args);
        assert!(current.replay_any_version);
        current.argv = vec!["/bin/false".into()];
        let current_key = current.idem_key.clone();
        let current_op = submit(&fx, current).await;
        assert_ne!(
            current_op, stored_op,
            "{released_key}: a released row was replayed"
        );
        fx.runtime.wait(&current_op).await.unwrap();
        assert_ne!(current_key, released_key);
        let after = stored_row(&fx, &released_key).await;
        assert_eq!(after.id, stored_op);
        assert_eq!(after.payload, before.payload);
        assert_eq!(after.payload_hash, before.payload_hash);
        for key in [&released_key, &current_key] {
            assert_eq!(
                operation_count_for_idem(&fx.repo, &format!("dev:{}:caller:{key}", fx.track_id))
                    .await,
                1,
                "{key}"
            );
        }
        if !completed {
            std::fs::write(fx.cwd.path().join("release"), "").unwrap();
            fx.runtime.wait(&stored_op).await.unwrap();
        }
    }
}

#[tokio::test]
async fn checks_upgrade_pending_released_row_is_not_replayed() {
    released_rows_start_a_new_operation(false).await;
}

#[tokio::test]
async fn checks_upgrade_completed_released_row_is_not_replayed() {
    released_rows_start_a_new_operation(true).await;
}

#[tokio::test]
async fn checks_upgrade_pending_reuses_any_version_receipt() {
    upgraded_checks(false).await;
}

#[tokio::test]
async fn checks_upgrade_completed_reuses_any_version_result() {
    upgraded_checks(true).await;
}

#[tokio::test]
async fn checks_upgrade_all_mode_retry_after_program_change_replays() {
    let fx = forge_runtime_fixture().await;
    let all = || {
        lowered(json!({
            "repo": "owner/repo", "pr": 42, "attempt": "upgrade", "wait_for_all": true
        }))
    };
    let mut stored = edited_program(all());
    stored.argv = release_argv(&fx, CURRENT_OUTPUT);
    let mut repeated = all();
    repeated.argv = vec!["/bin/false".into()];
    replays_stored_operation(&fx, stored, repeated, true).await;
}

#[tokio::test]
async fn checks_upgrade_rejects_different_parameters_and_scopes() {
    let fx = forge_runtime_fixture().await;
    let op_id = submit(&fx, stored_payload(&fx)).await;
    std::fs::write(fx.cwd.path().join("release"), "").unwrap();
    fx.runtime.wait(&op_id).await.unwrap();

    // Without the plugin's replay declaration a different payload under a held key conflicts.
    let mut undeclared = current_payload();
    undeclared.replay_any_version = false;
    let error = submit_as(&fx, "dev", &fx.track_id, "caller", undeclared)
        .await
        .expect_err("an undeclared different payload must conflict");
    assert!(error.contains("different payload"), "{error}");

    // Delimiter collision: calls whose arguments join to the same text get distinct keys.
    let mut numeric_attempt = lowered(json!({"repo": "owner/repo", "pr": 42, "attempt": 43}));
    numeric_attempt.argv = release_argv(&fx, CURRENT_OUTPUT);
    let numeric_key = numeric_attempt.idem_key.clone();
    let numeric_op = submit(&fx, numeric_attempt).await;
    fx.runtime.wait(&numeric_op).await.unwrap();
    let mut colliding = lowered(json!({"repo": "owner/repo:42", "pr": 43}));
    assert_ne!(colliding.idem_key, numeric_key);
    colliding.argv = vec!["/bin/false".into()];
    let colliding_op = submit(&fx, colliding).await;
    assert_ne!(colliding_op, numeric_op, "different parameters replayed");
    fx.runtime.wait(&colliding_op).await.unwrap();

    for (plugin, track, card) in [
        ("other-plugin", fx.track_id.as_str(), "caller"),
        ("dev", fx.track_id.as_str(), "other-caller"),
        ("dev", "other-track", "caller"),
    ] {
        let mut payload = current_payload();
        payload.argv = vec!["/bin/false".into()];
        if let Ok(submitted) = submit_as(&fx, plugin, track, card, payload).await {
            assert_ne!(submitted, op_id, "different scope exposed old result");
            fx.runtime.wait(&submitted).await.unwrap();
        }
    }
    for args in [
        json!({"repo":"other/repo", "pr":42, "attempt":"upgrade"}),
        json!({"repo":"owner/repo", "pr":43, "attempt":"upgrade"}),
        json!({"repo":"owner/repo", "pr":42, "attempt":"new-attempt"}),
        json!({"repo":"owner/repo", "pr":42}),
        json!({"repo":"owner/repo", "pr":42, "attempt":"upgrade", "wait_for_all":true}),
    ] {
        let mut payload = lowered(args);
        payload.argv = vec!["/bin/false".into()];
        let new_op = submit(&fx, payload).await;
        assert_ne!(new_op, op_id);
        fx.runtime.wait(&new_op).await.unwrap();
    }
}
