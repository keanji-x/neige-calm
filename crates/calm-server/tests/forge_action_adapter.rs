#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RouteRepo;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::error::Result as CalmResult;
use calm_server::event::{EventBus, FieldSource, ForgeEventSpec, ForgeMergeSubject};
use calm_server::model::{NewCove, NewWave, new_id, now_ms};
use calm_server::operation::forge_action_adapter::{
    FORGE_ACTION_KIND, ForgeActionAdapter, ForgeActionPayload,
};
use calm_server::operation::{
    OperationCompletionBus, OperationKey, OperationOutcome, OperationRuntime, SqlxOperationRepo,
};
use calm_server::routes::theme::RequestTheme;
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use serde_json::{Value, json};
use tempfile::TempDir;

struct TestBoot {
    _tmp: TempDir,
    repo: Arc<SqlxRepo>,
    runtime: OperationRuntime,
    wave_id: String,
}

impl TestBoot {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory repo"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "forge-action".into(),
                color: "#334455".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "forge-action".into(),
                sort: None,
                cwd: tmp.path().display().to_string(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");

        let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
        let events = EventBus::new();
        let completion = OperationCompletionBus::new();
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        let runtime = OperationRuntime::new_unchecked(
            operation_repo.clone(),
            vec![Arc::new(ForgeActionAdapter::new())],
            events.clone(),
            completion.clone(),
            calm_server::operation::SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events,
                completion,
            ),
        );
        Self {
            _tmp: tmp,
            repo,
            runtime,
            wave_id: wave.id.to_string(),
        }
    }

    fn temp_path(&self, name: &str) -> PathBuf {
        self._tmp.path().join(name)
    }
}

fn event_spec() -> ForgeEventSpec {
    let mut fields = BTreeMap::new();
    fields.insert(
        "merge_sha".into(),
        FieldSource::JsonField {
            path: "/oid".into(),
        },
    );
    fields.insert(
        "head_sha".into(),
        FieldSource::JsonField {
            path: "/headRefOid".into(),
        },
    );
    ForgeEventSpec {
        event_kind: "forge.pr.merged".into(),
        fields,
    }
}

fn subject() -> ForgeMergeSubject {
    ForgeMergeSubject {
        phase: "impl".into(),
        slice_id: "slice-6".into(),
        pr_number: 760,
    }
}

fn payload(boot: &TestBoot, idem_key: &str, argv: Vec<String>, result_path: PathBuf) -> Value {
    serde_json::to_value(ForgeActionPayload {
        wave_id: boot.wave_id.clone(),
        card_id: new_id(),
        subject: subject(),
        argv,
        idem_key: idem_key.into(),
        event_spec: event_spec(),
        probe: None,
        result_path,
        deadline_ms: now_ms() + 30_000,
    })
    .expect("payload serializes")
}

fn op_key(idem_key: &str) -> OperationKey {
    OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(idem_key.into()),
        payload_hash: format!("forge-action-test:{idem_key}"),
    }
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).expect("write fake action");
    let mut perms = fs::metadata(path).expect("script metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod fake action");
}

async fn cleanup_workspace_lease_dirs(repo: &SqlxRepo) {
    let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM workspace_leases")
        .fetch_all(repo.pool())
        .await
        .unwrap_or_default();
    for path in paths {
        let _ = fs::remove_dir_all(path);
    }
}

async fn phase(repo: &SqlxRepo, op_id: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_one(repo.pool())
        .await
        .expect("phase query")
}

async fn wait_for_file(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {}", path.display());
}

#[tokio::test]
async fn forge_action_idempotency_on_resubmit_collapses_to_one_operation() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("instant-action.sh");
    write_script(
        &action,
        "#!/bin/sh\nprintf '%s\\n' '{\"oid\":\"abc123\",\"headRefOid\":\"def456\"}'\n",
    );
    let idem = "forge-idem-resubmit";
    let payload = payload(
        &boot,
        idem,
        vec![action.display().to_string()],
        boot.temp_path("instant-result.json"),
    );

    let first = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), payload.clone())
        .await?;
    let result = boot.runtime.wait(&first).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));

    let second = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), payload)
        .await?;
    assert_eq!(first, second);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(FORGE_ACTION_KIND)
        .fetch_one(boot.repo.pool())
        .await?;
    assert_eq!(count, 1);
    cleanup_workspace_lease_dirs(&boot.repo).await;
    Ok(())
}

#[tokio::test]
async fn forge_action_parks_releases_post_park_and_persists_typed_event() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("blocking-action.sh");
    let started = boot.temp_path("started");
    let finish = boot.temp_path("finish");
    write_script(
        &action,
        r#"#!/bin/sh
: > "$1"
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
while [ ! -f "$2" ]; do sleep 0.02; done
"#,
    );
    let idem = "forge-typed-completion";
    let op_id = boot
        .runtime
        .submit(
            FORGE_ACTION_KIND,
            op_key(idem),
            payload(
                &boot,
                idem,
                vec![
                    action.display().to_string(),
                    started.display().to_string(),
                    finish.display().to_string(),
                ],
                boot.temp_path("blocking-result.json"),
            ),
        )
        .await?;

    assert_eq!(phase(&boot.repo, &op_id).await, "parked");
    wait_for_file(&started).await;
    assert_eq!(
        phase(&boot.repo, &op_id).await,
        "parked",
        "fake action has started but cannot complete before the test releases it"
    );
    let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind("forge.pr.merged")
        .fetch_one(boot.repo.pool())
        .await?;
    assert_eq!(event_count, 0, "event must not exist while op is parked");

    fs::write(&finish, "").expect("release fake action");
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(phase(&boot.repo, &op_id).await, "succeeded");

    let payload_text: String =
        sqlx::query_scalar("SELECT payload FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1")
            .bind("forge.pr.merged")
            .fetch_one(boot.repo.pool())
            .await?;
    let event_payload: Value = serde_json::from_str(&payload_text)?;
    assert_eq!(event_payload["merge_sha"], json!("abc123"));
    assert_eq!(event_payload["head_sha"], json!("def456"));
    assert_eq!(event_payload["wave_id"], json!(boot.wave_id));
    assert_eq!(event_payload["subject"]["phase"], json!("impl"));
    assert_eq!(event_payload["subject"]["slice_id"], json!("slice-6"));
    assert_eq!(event_payload["subject"]["pr_number"], json!(760));

    cleanup_workspace_lease_dirs(&boot.repo).await;
    Ok(())
}
