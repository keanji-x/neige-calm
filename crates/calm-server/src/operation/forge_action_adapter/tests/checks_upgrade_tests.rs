use super::*;
use crate::builtin_plugins::dev::git_actions::lower;
use crate::mcp_server::transport::{PluginForgePayload, submit_forge_action_with_key};

fn historical_payload(fx: &ForgeRuntimeFixture) -> PluginForgePayload {
    // Frozen semantic descriptor from 162aff1d, before #2129. Only volatile argv
    // is replaced to make completion deterministic without a network subprocess.
    let mut payload: PluginForgePayload =
        serde_json::from_str(include_str!("../checks-pre-2129.json")).unwrap();
    payload.argv = vec!["sh".into(), "-c".into(),
        "while [ ! -f \"$1\" ]; do [ -d \"${1%/*}\" ] || exit 1; sleep 0.05; done; printf '%s\\n' '{\"conclusion\":\"success\",\"mergeable\":\"mergeable\",\"head_sha\":\"historical-head\"}'".into(),
        "sh".into(), fx.cwd.path().join("release").display().to_string()];
    payload
}

fn pre2363_payload(fx: &ForgeRuntimeFixture) -> PluginForgePayload {
    let mut payload: PluginForgePayload = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../plugin/src/builtin/gitforge/git_actions/checks-pre-2363.json"
    )))
    .unwrap();
    payload.argv = historical_payload(fx).argv;
    payload.argv[2] = payload.argv[2].replace(
        "\"head_sha\":\"historical-head\"}",
        "\"head_sha\":\"historical-head\",\"snapshot\":{\"head_sha\":\"historical-head\",\"mergeable\":\"mergeable\"},\"failed_checks\":[]}",
    );
    payload
}

fn current_payload() -> PluginForgePayload {
    serde_json::from_value(
        lower(
            "gh_pr_checks",
            &json!({
                "repo": "owner/repo", "pr": 42, "attempt": "upgrade"
            }),
        )
        .unwrap(),
    )
    .unwrap()
}

async fn submit(fx: &ForgeRuntimeFixture, payload: PluginForgePayload) -> String {
    submit_forge_action_with_key(
        &fx.runtime,
        fx.results.path(),
        "dev",
        fx.track_id.clone(),
        "caller".into(),
        fx.cwd.path().to_path_buf(),
        payload,
        new_id(),
    )
    .await
    .unwrap()
    .expect("same checks call must retrieve frozen operation")
    .op_id
}

async fn upgraded_checks(completed: bool, pre2363: bool) {
    let fx = forge_runtime_fixture().await;
    let old = if pre2363 {
        pre2363_payload(&fx)
    } else {
        historical_payload(&fx)
    };
    let op_id = submit(&fx, old).await;
    let before = fx
        .runtime
        .find_by_kind_and_idempotency(
            FORGE_ACTION_KIND,
            &format!(
                "dev:{}:caller:gh.pr.checks:owner/repo:42:upgrade",
                fx.track_id
            ),
        )
        .await
        .unwrap()
        .unwrap();
    let receipt = crate::mcp_server::forge_receipt::pending(&op_id, Some("forge.pr.checks"));
    let old_result = if completed {
        std::fs::write(fx.cwd.path().join("release"), "").unwrap();
        Some(fx.runtime.wait(&op_id).await.unwrap())
    } else {
        None
    };
    let repeated = submit(&fx, current_payload()).await;
    assert_eq!(repeated, op_id);
    let after = fx
        .runtime
        .find_by_kind_and_idempotency(
            FORGE_ACTION_KIND,
            &format!(
                "dev:{}:caller:gh.pr.checks:owner/repo:42:upgrade",
                fx.track_id
            ),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.payload, before.payload,
        "frozen action must not be updated"
    );
    assert_eq!(after.payload_hash, before.payload_hash);
    if let Some(result) = old_result {
        let repeated_result = fx
            .runtime
            .operation_result(&repeated)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repeated_result.op_id, result.op_id);
        match (repeated_result.outcome, result.outcome) {
            (
                OperationOutcome::Succeeded { result: repeated },
                OperationOutcome::Succeeded { result: original },
            ) => {
                assert_eq!(repeated, original);
                assert_eq!(original["event"].get("snapshot").is_some(), pre2363);
                assert!(
                    original["event"]["snapshot"]
                        .get("all_checks_completed")
                        .is_none()
                );
            }
            other => panic!("historical result must remain successful: {other:?}"),
        }
    } else {
        assert!(
            fx.runtime
                .operation_result(&repeated)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::mcp_server::forge_receipt::pending(&repeated, Some("forge.pr.checks")),
            receipt
        );
        std::fs::write(fx.cwd.path().join("release"), "").unwrap();
        fx.runtime.wait(&op_id).await.unwrap();
    }
    let events = event_payloads(&fx.repo, "forge.pr.checks").await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].get("snapshot").is_some_and(|v| !v.is_null()),
        pre2363
    );
    assert_eq!(
        operation_count_for_idem(
            &fx.repo,
            &format!(
                "dev:{}:caller:gh.pr.checks:owner/repo:42:upgrade",
                fx.track_id
            )
        )
        .await,
        1
    );
}

#[tokio::test]
async fn checks_upgrade_pending_reuses_frozen_receipt() {
    upgraded_checks(false, false).await;
}

#[tokio::test]
async fn checks_upgrade_completed_reuses_frozen_result() {
    upgraded_checks(true, false).await;
}

#[tokio::test]
async fn checks_upgrade_pre2363_pending_reuses_frozen_receipt() {
    upgraded_checks(false, true).await;
}

#[tokio::test]
async fn checks_upgrade_pre2363_completed_reuses_frozen_result() {
    upgraded_checks(true, true).await;
}

#[tokio::test]
async fn checks_upgrade_all_mode_cannot_reuse_frozen_default_result() {
    let fx = forge_runtime_fixture().await;
    let old = submit(&fx, pre2363_payload(&fx)).await;
    std::fs::write(fx.cwd.path().join("release"), "").unwrap();
    fx.runtime.wait(&old).await.unwrap();
    let mut all: PluginForgePayload = serde_json::from_value(
        lower(
            "gh_pr_checks",
            &json!({
                "repo":"owner/repo", "pr":42, "attempt":"upgrade", "wait_for_all":true
            }),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(all.compatible_payload_hashes.is_empty());
    all.argv = vec!["/bin/false".into()];
    let new = submit(&fx, all).await;
    assert_ne!(new, old);
    fx.runtime.wait(&new).await.unwrap();
}

#[tokio::test]
async fn checks_upgrade_rejects_different_parameters_and_scopes() {
    let fx = forge_runtime_fixture().await;
    let op_id = submit(&fx, historical_payload(&fx)).await;
    std::fs::write(fx.cwd.path().join("release"), "").unwrap();
    fx.runtime.wait(&op_id).await.unwrap();

    // Delimiter collision: identical textual key with different actual args.
    let mut numeric_attempt = historical_payload(&fx);
    numeric_attempt.idem_key = "gh.pr.checks:owner/repo:42:43".into();
    let numeric_op = submit(&fx, numeric_attempt).await;
    fx.runtime.wait(&numeric_op).await.unwrap();
    let collision = lower(
        "gh_pr_checks",
        &json!({
            "repo": "owner/repo:42", "pr": 43
        }),
    )
    .unwrap();
    let changed: PluginForgePayload = serde_json::from_value(collision).unwrap();
    assert_eq!(changed.idem_key, "gh.pr.checks:owner/repo:42:43");
    let error = submit_forge_action_with_key(
        &fx.runtime,
        fx.results.path(),
        "dev",
        fx.track_id.clone(),
        "caller".into(),
        fx.cwd.path().to_path_buf(),
        changed,
        new_id(),
    )
    .await
    .unwrap()
    .err()
    .expect("different parameters must conflict");
    assert!(error.contains("different payload"), "{error}");

    for (plugin, track, card) in [
        ("other-plugin", fx.track_id.as_str(), "caller"),
        ("dev", fx.track_id.as_str(), "other-caller"),
        ("dev", "other-track", "caller"),
    ] {
        let mut payload = current_payload();
        payload.argv = vec!["/bin/false".into()];
        let submitted = submit_forge_action_with_key(
            &fx.runtime,
            fx.results.path(),
            plugin,
            track.into(),
            card.into(),
            fx.cwd.path().to_path_buf(),
            payload,
            new_id(),
        )
        .await
        .unwrap();
        if let Ok(submitted) = submitted {
            assert_ne!(submitted.op_id, op_id, "different scope exposed old result");
            fx.runtime.wait(&submitted.op_id).await.unwrap();
        }
    }
    for args in [
        json!({"repo":"other/repo", "pr":42, "attempt":"upgrade"}),
        json!({"repo":"owner/repo", "pr":43, "attempt":"upgrade"}),
        json!({"repo":"owner/repo", "pr":42, "attempt":"new-attempt"}),
    ] {
        let mut payload: PluginForgePayload =
            serde_json::from_value(lower("gh_pr_checks", &args).unwrap()).unwrap();
        payload.argv = vec!["/bin/false".into()];
        let new_op = submit(&fx, payload).await;
        assert_ne!(new_op, op_id);
        fx.runtime.wait(&new_op).await.unwrap();
    }
}
