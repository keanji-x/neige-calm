use super::*;
use calm_types::forge_git::GIT_COMMIT_PROBE_SCRIPT;
use std::process::Command;
use std::sync::Arc;

use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::model::{NewCove, NewWave, new_id, now_ms};
use crate::operation::{
    OperationKey, OperationOutcome, OperationRepo, OperationResult, OperationRuntime,
    ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;

#[test]
fn frozen_forge_six_shape_defaults_new_optional_fields() {
    let frozen: FrozenForge = serde_json::from_value(json!({
        "wave_id": "wave-01",
        "cove_id": "cove-01",
        "card_id": "card-01",
        "subject": {
            "phase": "impl",
            "slice_id": "6",
            "pr_number": 760
        },
        "argv": ["/bin/true"],
        "idem_key": "forge-merge",
        "event_spec": {
            "event_kind": "forge.pr.merged",
            "fields": {
                "head_sha": { "json_field": { "path": "/headRefOid" } },
                "merge_sha": { "json_field": { "path": "/oid" } }
            }
        },
        "probe": null,
        "cwd_lease": "/tmp/lease",
        "result_path": "/tmp/result.json",
        "deadline_ms": 1
    }))
    .expect("slice 6 frozen forge shape remains readable");

    assert!(frozen.context.is_empty());
    assert!(frozen.subject.is_some());
    assert!(frozen.event_spec.is_some());
}

#[test]
fn operation_key_match_accepts_raw_or_scoped_payload_idem() {
    assert!(operation_key_matches_payload_idem(
        Some("idem"),
        "wave",
        "card",
        "idem"
    ));
    assert!(operation_key_matches_payload_idem(
        Some("plugin:wave:card:idem"),
        "wave",
        "card",
        "idem"
    ));
    assert!(operation_key_matches_payload_idem(
        Some("plugin:wave:card:idem:with:colons"),
        "wave",
        "card",
        "idem:with:colons"
    ));
    assert!(!operation_key_matches_payload_idem(
        Some("not-idem"),
        "wave",
        "card",
        "idem"
    ));
    assert!(!operation_key_matches_payload_idem(
        Some("prefixidem"),
        "wave",
        "card",
        "idem"
    ));
    assert!(!operation_key_matches_payload_idem(
        Some("plugin:other-wave:card:idem"),
        "wave",
        "card",
        "idem"
    ));
    assert!(!operation_key_matches_payload_idem(
        Some("plugin:wave:other-card:idem"),
        "wave",
        "card",
        "idem"
    ));
    assert!(!operation_key_matches_payload_idem(
        None, "wave", "card", "idem"
    ));
}

#[test]
fn forge_artifact_tmp_path_is_unique_per_attempt() {
    let result_path = Path::new("/tmp/forge-result.patch");
    let first = forge_artifact_tmp_path(result_path);
    let second = forge_artifact_tmp_path(result_path);
    let prefix = format!("{}.tmp.{}.", result_path.display(), std::process::id());

    assert_ne!(first, second);
    assert!(first.display().to_string().starts_with(&prefix));
    assert!(second.display().to_string().starts_with(&prefix));
}

#[tokio::test]
async fn forge_action_live_nonzero_merge_probe_landed_succeeds_once() {
    let fx = forge_runtime_fixture().await;
    let payload = merge_payload(
        &fx,
        vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
        ProbeSpec {
            probe_argv: shell_probe("exit 0"),
            output_probe_argv: Some(shell_probe(
                "printf '%s\n' '{\"headRefOid\":\"1111111111111111111111111111111111111111\",\"mergeCommit\":{\"oid\":\"2222222222222222222222222222222222222222\"}}'",
            )),
        },
    );

    let result = submit_and_wait(&fx, payload, "merge-landed-hash").await;

    match result.outcome {
        OperationOutcome::Succeeded { result } => {
            assert_eq!(result["event_kind"], "forge.pr.merged");
        }
        other => panic!("merge should succeed after landed probe: {other:?}"),
    }
    assert_eq!(event_count(&fx.repo, "forge.pr.merged").await, 1);
    assert_eq!(
        event_count(&fx.repo, "forge.issue.closed").await,
        0,
        "unrelated forge events should not be emitted"
    );
}

#[tokio::test]
async fn forge_action_nonzero_merge_probe_not_landed_fails_without_merge_event() {
    let fx = forge_runtime_fixture().await;
    let payload = merge_payload(
        &fx,
        vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
        ProbeSpec {
            probe_argv: shell_probe("exit 1"),
            output_probe_argv: Some(shell_probe(
                "printf '%s\n' '{\"headRefOid\":\"1111111111111111111111111111111111111111\",\"mergeCommit\":null}'",
            )),
        },
    );

    let result = submit_and_wait(&fx, payload, "merge-open-hash").await;

    match result.outcome {
        OperationOutcome::Failed {
            last_error_class, ..
        } => {
            assert_eq!(last_error_class.as_deref(), Some("action-not-landed"));
        }
        other => panic!("merge should fail when probe reports open: {other:?}"),
    }
    assert_eq!(event_count(&fx.repo, "forge.pr.merged").await, 0);
}

#[tokio::test]
async fn landed_probe_completion_tx_error_leaves_operation_parked() {
    let fx = forge_runtime_fixture().await;
    let payload = merge_payload(
        &fx,
        vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
        ProbeSpec {
            probe_argv: shell_probe("exit 0"),
            output_probe_argv: Some(shell_probe(
                "printf '%s\n' '{\"headRefOid\":\"1111111111111111111111111111111111111111\",\"mergeCommit\":{\"oid\":\"2222222222222222222222222222222222222222\"}}'",
            )),
        },
    );
    let operation_repo = SqlxOperationRepo::new(fx.repo.pool().clone());
    let op_id = operation_repo
        .insert_operation(
            FORGE_ACTION_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(payload.idem_key.clone()),
                payload_hash: "landed-probe-tx-error".into(),
            },
            serde_json::to_value(&payload).expect("payload json"),
        )
        .await
        .expect("insert operation");
    let frozen = FrozenForge {
        wave_id: payload.wave_id,
        cove_id: "cove-1".into(),
        card_id: payload.card_id,
        subject: payload.subject,
        argv: payload.argv,
        idem_key: payload.idem_key,
        event_spec: payload.event_spec,
        context: payload.context,
        probe: payload.probe.clone(),
        cwd_lease: payload.cwd_lease,
        result_path: payload.result_path,
        deadline_ms: payload.deadline_ms,
    };
    let mut output = TxOutput::new("wave", Some(frozen.wave_id.clone()), json!({}));
    output.data = serde_json::to_value(&frozen).expect("frozen json");
    let artifacts = SpawnArtifacts {
        pid: 1,
        pgid: 1,
        start_time: 1,
        boot_id: "boot-test".into(),
        log_path: None,
        extra: json!({}),
    };
    sqlx::query(
        r#"UPDATE operations
               SET tx_output_json = ?1,
                   phase = 'parked',
                   parked_at_ms = ?2,
                   parked_deadline_ms = ?3,
                   spawn_artifacts_json = ?4
               WHERE id = ?5"#,
    )
    .bind(serde_json::to_string(&output).expect("tx output json"))
    .bind(now_ms())
    .bind(frozen.deadline_ms)
    .bind(serde_json::to_string(&artifacts).expect("spawn artifacts json"))
    .bind(&op_id)
    .execute(fx.repo.pool())
    .await
    .expect("park operation");
    sqlx::query(
        r#"CREATE TRIGGER fail_forge_success_update
               BEFORE UPDATE OF phase ON operations
               WHEN NEW.phase = 'succeeded'
               BEGIN
                 SELECT RAISE(ABORT, 'injected success tx failure');
               END"#,
    )
    .execute(fx.repo.pool())
    .await
    .expect("install trigger");

    let result = resolve_post_release_via_probe(
        fx.repo.pool(),
        &OperationCompletionBus::new(),
        &EventBus::new(),
        fx.repo.as_ref(),
        &op_id,
        &frozen,
        "test ambiguous outcome",
    )
    .await;
    assert!(
        result.is_err(),
        "landed probe completion tx failure should propagate"
    );
    let recover_repo = Arc::new(SqlxOperationRepo::new(fx.repo.pool().clone()));
    let route_repo: Arc<dyn RouteRepo> = fx.repo.clone();
    let recover_events = EventBus::new();
    let recover_completion = OperationCompletionBus::new();
    let recover_ctx = SpawnCtx::new(
        route_repo.clone(),
        recover_repo.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(route_repo),
        recover_events,
        recover_completion,
    );
    let op = recover_repo
        .get_operation(&op_id)
        .await
        .expect("get parked operation")
        .expect("parked operation exists");
    let recover_result = ForgeActionAdapter::new()
        .recover_parked(
            &op,
            &artifacts,
            false,
            RecoveryMode::PastDeadline,
            &recover_ctx,
        )
        .await;
    assert!(
        recover_result.is_err(),
        "past-deadline landed probe completion tx failure should propagate"
    );

    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(fx.repo.pool())
        .await
        .expect("read phase");
    assert_eq!(phase, "parked");
    assert_eq!(event_count(&fx.repo, "forge.pr.merged").await, 0);
}

#[tokio::test]
async fn forge_action_nonzero_git_commit_clean_worktree_probe_succeeds() {
    let fx = forge_runtime_fixture().await;
    init_clean_git_repo(fx.cwd.path());
    let payload = ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: None,
        argv: vec!["git".into(), "commit".into(), "-m".into(), "nothing".into()],
        idem_key: "git.commit:clean-index".into(),
        event_spec: None,
        context: Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: shell_probe(GIT_COMMIT_PROBE_SCRIPT),
            output_probe_argv: None,
        }),
        cwd_lease: fx.cwd.path().to_path_buf(),
        result_path: fx.result_path("commit-clean"),
        deadline_ms: now_ms() + 60_000,
    };

    let result = submit_and_wait(&fx, payload, "commit-clean-hash").await;

    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "clean-worktree commit should succeed via probe: {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn forge_action_clean_worktree_probe_extracts_worktree_committed_event() {
    let fx = forge_runtime_fixture().await;
    init_clean_git_repo(fx.cwd.path());
    let head = git_stdout(fx.cwd.path(), ["rev-parse", "HEAD"]);
    let payload = ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: None,
        argv: vec!["git".into(), "commit".into(), "-m".into(), "nothing".into()],
        idem_key: "git.commit:clean-index-event".into(),
        event_spec: Some(ForgeEventSpec {
            event_kind: "worktree.committed".into(),
            fields: std::collections::BTreeMap::from([
                (
                    "branch".into(),
                    FieldSource::JsonField {
                        path: "/branch".into(),
                    },
                ),
                (
                    "commit_sha".into(),
                    FieldSource::JsonField {
                        path: "/commit".into(),
                    },
                ),
            ]),
        }),
        context: Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: shell_probe(GIT_COMMIT_PROBE_SCRIPT),
            output_probe_argv: Some(shell_probe(
                "git log -1 --format='{\"commit\":\"%H\",\"branch\":\"neige/wave-1/card-1\"}'",
            )),
        }),
        cwd_lease: fx.cwd.path().to_path_buf(),
        result_path: fx.result_path("commit-clean-event"),
        deadline_ms: now_ms() + 60_000,
    };

    let result = submit_and_wait(&fx, payload, "commit-clean-event-hash").await;

    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "clean-worktree commit should succeed via JSON output probe: {:?}",
        result.outcome
    );
    assert_eq!(
        git_stdout(fx.cwd.path(), ["rev-list", "--count", "HEAD"]),
        "1"
    );
    let payloads = event_payloads(&fx.repo, "worktree.committed").await;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["wave_id"], fx.wave_id);
    assert_eq!(payloads[0]["card_id"], "card-1");
    assert_eq!(payloads[0]["commit_sha"], head);
    assert_eq!(payloads[0]["branch"], "neige/wave-1/card-1");
}

#[tokio::test]
#[cfg(unix)]
async fn forge_action_git_commit_dirty_index_failure_does_not_emit_worktree_committed_event() {
    use std::os::unix::fs::PermissionsExt;

    let fx = forge_runtime_fixture().await;
    init_clean_git_repo(fx.cwd.path());
    let hooks_dir = fx.cwd.path().join(".git").join("hooks");
    let hook_path = hooks_dir.join("pre-commit");
    std::fs::write(&hook_path, "#!/bin/sh\nexit 1\n").expect("write failing hook");
    let mut permissions = std::fs::metadata(&hook_path)
        .expect("hook metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook_path, permissions).expect("chmod hook");
    std::fs::write(fx.cwd.path().join("change.txt"), "change\n").expect("write change");

    let commit_script = "git add -A || exit 1; if git diff --cached --quiet; then :; else git commit -m \"$1\" || exit 1; fi; git log -1 --format='{\"commit\":\"%H\",\"branch\":\"'\"$2\"'\"}'";
    let payload = ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: None,
        argv: vec![
            "sh".into(),
            "-c".into(),
            commit_script.into(),
            "sh".into(),
            "should fail".into(),
            "neige/wave-1/card-1".into(),
        ],
        idem_key: "git.commit:dirty-index-failure".into(),
        event_spec: Some(ForgeEventSpec {
            event_kind: "worktree.committed".into(),
            fields: std::collections::BTreeMap::from([
                (
                    "branch".into(),
                    FieldSource::JsonField {
                        path: "/branch".into(),
                    },
                ),
                (
                    "commit_sha".into(),
                    FieldSource::JsonField {
                        path: "/commit".into(),
                    },
                ),
            ]),
        }),
        context: Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: shell_probe(GIT_COMMIT_PROBE_SCRIPT),
            output_probe_argv: Some(shell_probe(
                "git log -1 --format='{\"commit\":\"%H\",\"branch\":\"neige/wave-1/card-1\"}'",
            )),
        }),
        cwd_lease: fx.cwd.path().to_path_buf(),
        result_path: fx.result_path("commit-dirty-failure"),
        deadline_ms: now_ms() + 60_000,
    };

    let result = submit_and_wait(&fx, payload, "commit-dirty-failure-hash").await;

    assert!(
        matches!(result.outcome, OperationOutcome::Failed { .. }),
        "dirty-index commit failure should fail the op: {:?}",
        result.outcome
    );
    assert_eq!(
        git_stdout(fx.cwd.path(), ["rev-list", "--count", "HEAD"]),
        "1"
    );
    assert_eq!(event_count(&fx.repo, "worktree.committed").await, 0);
}

#[tokio::test]
#[cfg(unix)]
async fn git_status_probe_infra_failure_does_not_emit_worktree_committed() {
    use std::os::unix::fs::PermissionsExt;

    let fx = forge_runtime_fixture().await;
    init_clean_git_repo(fx.cwd.path());
    let fake_git_dir = tempfile::tempdir().expect("fake git dir");
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    assert!(
        real_git.status.success(),
        "locate git failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&real_git.stdout),
        String::from_utf8_lossy(&real_git.stderr)
    );
    let real_git = String::from_utf8_lossy(&real_git.stdout).trim().to_string();
    let fake_git = fake_git_dir.path().join("git");
    std::fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\nif [ \"$1\" = status ] && [ \"$2\" = --porcelain ]; then exit 42; fi\nexec {} \"$@\"\n",
            shell_quote(&real_git)
        ),
    )
    .expect("write fake git");
    let mut permissions = std::fs::metadata(&fake_git)
        .expect("fake git metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_git, permissions).expect("chmod fake git");
    let probe_script = format!(
        "PATH={}:$PATH; {}",
        shell_quote(&fake_git_dir.path().display().to_string()),
        GIT_COMMIT_PROBE_SCRIPT
    );
    let payload = ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: None,
        argv: vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
        idem_key: "git.commit:status-probe-infra-failure".into(),
        event_spec: Some(ForgeEventSpec {
            event_kind: "worktree.committed".into(),
            fields: std::collections::BTreeMap::from([
                (
                    "branch".into(),
                    FieldSource::JsonField {
                        path: "/branch".into(),
                    },
                ),
                (
                    "commit_sha".into(),
                    FieldSource::JsonField {
                        path: "/commit".into(),
                    },
                ),
            ]),
        }),
        context: Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: shell_probe(&probe_script),
            output_probe_argv: Some(shell_probe(
                "git log -1 --format='{\"commit\":\"%H\",\"branch\":\"neige/wave-1/card-1\"}'",
            )),
        }),
        cwd_lease: fx.cwd.path().to_path_buf(),
        result_path: fx.result_path("commit-status-probe-infra-failure"),
        deadline_ms: now_ms() + 60_000,
    };

    let result = submit_and_wait(&fx, payload, "commit-status-probe-infra-failure-hash").await;

    match result.outcome {
        OperationOutcome::Failed {
            last_error_class, ..
        } => {
            assert_eq!(last_error_class.as_deref(), Some("gate-infra"));
        }
        other => panic!("git status infra failure should fail op: {other:?}"),
    }
    assert_eq!(event_count(&fx.repo, "worktree.committed").await, 0);
}

#[tokio::test]
async fn git_add_failure_dirty_worktree_clean_index_does_not_emit_worktree_committed() {
    let fx = forge_runtime_fixture().await;
    init_clean_git_repo(fx.cwd.path());

    let commit_script = r#"git() { if [ "$1" = add ]; then return 1; fi; command git "$@"; }; printf '%s\n' dirty > worker-output.txt; git add -A || exit 1; if git diff --cached --quiet; then :; else git commit -m "$1" || exit 1; fi; git log -1 --format='{"commit":"%H","branch":"'"$2"'"}'"#;
    let payload = ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: None,
        argv: vec![
            "sh".into(),
            "-c".into(),
            commit_script.into(),
            "sh".into(),
            "should fail before staging".into(),
            "neige/wave-1/card-1".into(),
        ],
        idem_key: "git.commit:add-failure-dirty-worktree".into(),
        event_spec: Some(ForgeEventSpec {
            event_kind: "worktree.committed".into(),
            fields: std::collections::BTreeMap::from([
                (
                    "branch".into(),
                    FieldSource::JsonField {
                        path: "/branch".into(),
                    },
                ),
                (
                    "commit_sha".into(),
                    FieldSource::JsonField {
                        path: "/commit".into(),
                    },
                ),
            ]),
        }),
        context: Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: shell_probe(GIT_COMMIT_PROBE_SCRIPT),
            output_probe_argv: Some(shell_probe(
                "git log -1 --format='{\"commit\":\"%H\",\"branch\":\"neige/wave-1/card-1\"}'",
            )),
        }),
        cwd_lease: fx.cwd.path().to_path_buf(),
        result_path: fx.result_path("commit-add-failure-dirty-worktree"),
        deadline_ms: now_ms() + 60_000,
    };

    let result = submit_and_wait(&fx, payload, "commit-add-failure-dirty-worktree-hash").await;

    assert!(
        matches!(result.outcome, OperationOutcome::Failed { .. }),
        "git add failure with dirty worktree should fail the op: {:?}",
        result.outcome
    );
    assert_eq!(
        git_stdout(fx.cwd.path(), ["rev-list", "--count", "HEAD"]),
        "1"
    );
    assert_eq!(
        git_stdout(fx.cwd.path(), ["diff", "--cached", "--name-only"]),
        ""
    );
    assert_eq!(
        git_stdout(fx.cwd.path(), ["status", "--porcelain"]),
        "?? worker-output.txt"
    );
    assert_eq!(event_count(&fx.repo, "worktree.committed").await, 0);
}

#[tokio::test]
async fn forge_action_idempotency_retry_with_different_argv_collapses_to_one_operation() {
    let fx = forge_runtime_fixture().await;
    let mut first = no_event_payload(
        &fx,
        "gh.pr.merge:owner/repo:42",
        vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
        "idem-retry",
    );
    let mut second = no_event_payload(
        &fx,
        "gh.pr.merge:owner/repo:42",
        vec!["/bin/sh".into(), "-c".into(), "exit 99".into()],
        "idem-retry",
    );
    first.probe = None;
    second.probe = None;

    let first_op = submit_forge(&fx, first, "same-semantic-hash")
        .await
        .expect("first submit");
    let first_result = fx.runtime.wait(&first_op).await.expect("first result");
    assert!(
        matches!(first_result.outcome, OperationOutcome::Succeeded { .. }),
        "first op should succeed: {:?}",
        first_result.outcome
    );

    let second_op = submit_forge(&fx, second, "same-semantic-hash")
        .await
        .expect("second submit should dedupe, not conflict");
    assert_eq!(second_op, first_op);
    assert_eq!(
        operation_count_for_idem(&fx.repo, "gh.pr.merge:owner/repo:42").await,
        1
    );
}

struct ForgeRuntimeFixture {
    repo: Arc<SqlxRepo>,
    runtime: Arc<OperationRuntime>,
    wave_id: String,
    cwd: tempfile::TempDir,
    results: tempfile::TempDir,
}

async fn forge_runtime_fixture() -> ForgeRuntimeFixture {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let results = tempfile::tempdir().expect("results tempdir");
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "forge-action-adapter-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "forge-action-adapter-test".into(),
            sort: None,
            cwd: cwd.path().display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    let events = EventBus::new();
    let operation_repo = Arc::new(SqlxOperationRepo::new(sqlx_repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn RouteRepo> = sqlx_repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let runtime = Arc::new(
        OperationRuntime::new(
            operation_repo.clone(),
            vec![Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>],
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events,
                completion,
            ),
        )
        .await
        .expect("operation runtime"),
    );

    ForgeRuntimeFixture {
        repo: sqlx_repo,
        runtime,
        wave_id: wave.id.to_string(),
        cwd,
        results,
    }
}

impl ForgeRuntimeFixture {
    fn result_path(&self, label: &str) -> PathBuf {
        self.results.path().join(format!("{label}.result"))
    }
}

fn merge_payload(
    fx: &ForgeRuntimeFixture,
    argv: Vec<String>,
    probe: ProbeSpec,
) -> ForgeActionPayload {
    ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: Some(
            serde_json::from_value(json!({
                "phase": "impl",
                "slice_id": "815",
                "pr_number": 42
            }))
            .expect("merge subject"),
        ),
        argv,
        idem_key: "gh.pr.merge:owner/repo:42".into(),
        event_spec: Some(
            serde_json::from_value(json!({
                "event_kind": "forge.pr.merged",
                "fields": {
                    "head_sha": { "json_field": { "path": "/headRefOid" } },
                    "merge_sha": { "json_field": { "path": "/mergeCommit/oid" } }
                }
            }))
            .expect("merge event spec"),
        ),
        context: Map::new(),
        probe: Some(probe),
        cwd_lease: fx.cwd.path().to_path_buf(),
        result_path: fx.result_path("merge"),
        deadline_ms: now_ms() + 60_000,
    }
}

fn no_event_payload(
    fx: &ForgeRuntimeFixture,
    idem_key: &str,
    argv: Vec<String>,
    result_label: &str,
) -> ForgeActionPayload {
    ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: None,
        argv,
        idem_key: idem_key.into(),
        event_spec: None,
        context: Map::new(),
        probe: None,
        cwd_lease: fx.cwd.path().to_path_buf(),
        result_path: fx.result_path(result_label),
        deadline_ms: now_ms() + 60_000,
    }
}

fn shell_probe(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn submit_and_wait(
    fx: &ForgeRuntimeFixture,
    payload: ForgeActionPayload,
    payload_hash: &str,
) -> OperationResult {
    let op_id = submit_forge(fx, payload, payload_hash)
        .await
        .expect("submit forge op");
    fx.runtime.wait(&op_id).await.expect("forge op result")
}

async fn submit_forge(
    fx: &ForgeRuntimeFixture,
    payload: ForgeActionPayload,
    payload_hash: &str,
) -> Result<String> {
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(payload.idem_key.clone()),
        payload_hash: payload_hash.into(),
    };
    let operation_payload = serde_json::to_value(payload)?;
    fx.runtime
        .submit(FORGE_ACTION_KIND, key, operation_payload)
        .await
}

async fn event_count(repo: &SqlxRepo, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .expect("event count")
}

async fn event_payloads(repo: &SqlxRepo, kind: &str) -> Vec<Value> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT payload FROM events WHERE kind = ?1 ORDER BY id ASC")
            .bind(kind)
            .fetch_all(repo.pool())
            .await
            .expect("event payload rows");
    rows.into_iter()
        .map(|(payload,)| serde_json::from_str(&payload).expect("event payload json"))
        .collect()
}

async fn operation_count_for_idem(repo: &SqlxRepo, idem_key: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE idempotency_key = ?1")
        .bind(idem_key)
        .fetch_one(repo.pool())
        .await
        .expect("operation count")
}

fn init_clean_git_repo(path: &Path) {
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "forge-action@example.test"]);
    run_git(path, ["config", "user.name", "Forge Action Test"]);
    std::fs::write(path.join("README.md"), "initial\n").expect("write README");
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn git_stdout<const N: usize>(path: &Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run_git<const N: usize>(path: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
