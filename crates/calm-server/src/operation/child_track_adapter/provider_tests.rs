//! #1791: a child track's Planner inherits its parent Planner's `planner_provider`.

use super::tests::{
    create_child_from_task, operation, payload, seed_parent, seed_task, test_workspace_root,
};
use super::*;
use crate::db::sqlite::SqlxRepo;
use crate::db::sqlite::card_create_with_id_tx;
use crate::model::{CardRole, NewCard};
use crate::track_creation::planner_harness_card_payload;

/// Every production track has a Planner card, and a child inherits its `planner_provider`.
pub(super) async fn ensure_planner_card(repo: &SqlxRepo, track_id: &str) {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards WHERE track_id=?1 AND role='planner')",
    )
    .bind(track_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    if exists {
        return;
    }
    let mut tx = repo.pool().begin().await.unwrap();
    card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            title: None,
            track_id: track_id.into(),
            kind: "codex".into(),
            sort: None,
            payload: planner_harness_card_payload(
                &crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
                None,
                crate::session_projection_repo::AgentProvider::Codex,
            ),
        },
        CardRole::Planner,
        false,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn planner_provider_of(repo: &SqlxRepo, track_id: &str) -> Value {
    let payload: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    serde_json::from_str::<Value>(&payload).unwrap()
        [crate::validation::PLANNER_PROVIDER_PAYLOAD_KEY]
        .clone()
}

#[tokio::test]
async fn a_child_planner_inherits_its_parent_planners_provider() {
    for provider in ["claude", "codex"] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        sqlx::query(
            "UPDATE cards SET payload=json_set(payload,'$.planner_provider',?1) \
             WHERE track_id=?2 AND role='planner'",
        )
        .bind(provider)
        .bind(&parent)
        .execute(repo.pool())
        .await
        .unwrap();
        let task = seed_task(&repo, &parent, false).await;
        let child = create_child_from_task(&repo, &parent, &task.id).await;
        assert_eq!(planner_provider_of(&repo, &child).await, provider);
    }
}

#[tokio::test]
async fn a_parent_planner_without_a_provider_refuses_the_child() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let parent = seed_parent(&repo, false).await;
    sqlx::query(
        "UPDATE cards SET payload=json_remove(payload,'$.planner_provider') \
         WHERE track_id=?1 AND role='planner'",
    )
    .bind(&parent)
    .execute(repo.pool())
    .await
    .unwrap();
    let task = seed_task(&repo, &parent, false).await;
    let input = serde_json::to_value(payload(&task)).unwrap();
    let adapter = ChildTrackAdapter::new(
        std::sync::Arc::new(
            crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
        ),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        test_workspace_root(),
        std::path::PathBuf::from("/unused-objective-checkouts"),
        workspace_runtime::sweep::TeardownTargets::default(),
    );
    let mut tx = repo.pool().begin().await.unwrap();
    let error = adapter
        .prepare_tx(&mut tx, &input, &operation(input.clone()))
        .await
        .expect_err("no provider to inherit");
    assert!(
        matches!(&error, CalmError::Conflict(message) if message.contains("planner_provider")),
        "{error:?}"
    );
}

#[tokio::test]
async fn child_track_config_inherits_permission_and_selection() {
    for mode in ["ask", "full"] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let parent = seed_parent(&repo, false).await;
        sqlx::query(
            "UPDATE cards SET payload=json_set(payload, \
             '$.permission_mode',?1,'$.model','parent-model','$.reasoning_effort','high') \
             WHERE track_id=?2 AND role='planner'",
        )
        .bind(mode)
        .bind(&parent)
        .execute(repo.pool())
        .await
        .unwrap();
        let task = seed_task(&repo, &parent, false).await;
        let child = create_child_from_task(&repo, &parent, &task.id).await;
        let stored: String =
            sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND role='planner'")
                .bind(&child)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        let stored: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["permission_mode"], mode);
        assert_eq!(stored["model"], "parent-model");
        assert_eq!(stored["reasoning_effort"], "high");
    }
}

#[tokio::test]
async fn child_track_config_attached_child_has_own_worktree() {
    use crate::operation::{OperationKey, OperationOutcome};
    let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    git(
        dir.path(),
        &["remote", "add", "origin", dir.path().to_str().unwrap()],
    );
    git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(dir.path(), &["branch", "--set-upstream-to=origin/main"]);
    git(
        dir.path(),
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "parent ahead of upstream",
        ],
    );
    let frozen_head = git(dir.path(), &["rev-parse", "HEAD"]);
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let parent = parent_with_plan(&repo, &nested, TrackWorkspacePlan::AttachedFromCwd)
        .await
        .id
        .to_string();
    let task = seed_task(&repo, &parent, false).await;
    let input = serde_json::to_value(payload(&task)).unwrap();
    let runtime = child_runtime(&repo, false).await;
    let op = runtime
        .submit(
            CHILD_TRACK_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id),
                payload_hash: crate::routes::idempotency_key::stable_payload_hash(&input).unwrap(),
            },
            input,
        )
        .await
        .unwrap();
    let outcome = runtime.wait(&op).await.unwrap();
    let OperationOutcome::Succeeded { result } = outcome.outcome else {
        panic!("{outcome:?}")
    };
    let child = result["child_track_id"].as_str().unwrap();
    let worktree: Option<String> =
        sqlx::query_scalar("SELECT workspace_worktree_path FROM tracks WHERE id=?1")
            .bind(child)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let worktree = worktree.expect("attached child needs its own worktree");
    assert_eq!(
        std::path::Path::new(&worktree),
        workspace::worktree::track_worktree_path_for(dir.path(), child)
    );
    assert_eq!(result["cwd"], worktree);
    assert_eq!(
        git(
            std::path::Path::new(&worktree),
            &["rev-parse", "--abbrev-ref", "@{upstream}"]
        ),
        "origin/main"
    );
    assert_eq!(
        git(std::path::Path::new(&worktree), &["rev-parse", "HEAD"]),
        frozen_head
    );
    assert_ne!(frozen_head, git(dir.path(), &["rev-parse", "origin/main"]));

    assert_eq!(
        git(std::path::Path::new(&worktree), &["rev-parse", "HEAD"]),
        git(dir.path(), &["rev-parse", "HEAD"])
    );
}

#[tokio::test]
async fn child_attached_admission_refuses_live_core_worktree_redirect_without_child_row() {
    use crate::operation::{OperationKey, OperationOutcome};
    let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let source = tempfile::tempdir().unwrap();
    let owned_repo = tempfile::tempdir().unwrap();
    let owned = owned_repo.path().join("live");
    init_repo(source.path());
    init_repo(owned_repo.path());
    git(
        owned_repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            owned.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert!(!owned.as_path().starts_with(test_workspace_root()));
    let parent = parent_with_plan(&repo, source.path(), TrackWorkspacePlan::AttachedFromCwd).await;
    let owner = parent_with_plan(&repo, owned.as_path(), TrackWorkspacePlan::AttachedFromCwd).await;
    sqlx::query("UPDATE tracks SET workspace_worktree_path=?1 WHERE id=?2")
        .bind(owned.as_path().to_str().unwrap())
        .bind(owner.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    git(
        source.path(),
        &["config", "core.worktree", owned.as_path().to_str().unwrap()],
    );
    assert_eq!(
        git(source.path(), &["rev-parse", "--show-toplevel"]),
        owned.as_path().to_str().unwrap()
    );
    let task = seed_task(&repo, parent.id.as_str(), false).await;
    let input = serde_json::to_value(payload(&task)).unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let runtime = child_runtime(&repo, false).await;
    let op = runtime
        .submit(
            CHILD_TRACK_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id.clone()),
                payload_hash: crate::routes::idempotency_key::stable_payload_hash(&input).unwrap(),
            },
            input,
        )
        .await
        .unwrap();
    let outcome = runtime.wait(&op).await.unwrap();
    assert!(
        matches!(&outcome.outcome, OperationOutcome::Failed { last_error, .. }
            if last_error.contains("overlaps Neige-owned location")),
        "{outcome:?}"
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        after, before,
        "admission must refuse before minting a child row"
    );
    let child: Option<String> = sqlx::query_scalar("SELECT child_track_id FROM tasks WHERE id=?1")
        .bind(&task.id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(child, None);
}

fn git(path: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn init_repo(path: &std::path::Path) {
    git(path, &["init", "-b", "main"]);
    git(
        path,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
}

async fn parent_with_plan(
    repo: &SqlxRepo,
    cwd: &std::path::Path,
    plan: TrackWorkspacePlan,
) -> crate::model::Track {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = crate::db::sqlite::area_create_tx(
        &mut tx,
        calm_types::organization::LEGACY_OWNER_USER_ID,
        crate::model::NewArea {
            name: "parent".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let parent = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "parent".into(),
            sort: None,
            cwd: cwd.to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &plan,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    ensure_planner_card(repo, parent.id.as_str()).await;
    parent
}

/// Only inject a filesystem obstacle at the declared destination. Every lifecycle method
/// delegates to the real child adapter, including the actual worktree creation and recovery.
struct BlockChildWorktree(ChildTrackAdapter);

#[async_trait]
impl ProviderAdapter for BlockChildWorktree {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn phases(&self) -> &'static [PhaseTag] {
        self.0.phases()
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        self.0.validate(input).await
    }
    async fn preparation_track(&self, input: &Value, ctx: &SpawnCtx) -> Result<Option<String>> {
        self.0.preparation_track(input, ctx).await
    }
    async fn prepare_external(
        &self,
        input: &Value,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn crate::operation::PreparedTx>>> {
        self.0.prepare_external(input, op, ctx).await
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        self.0.prepare_tx(tx, input, op).await
    }
    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<crate::operation::AppServerInteractKind> {
        self.0.app_server_interact_kind(output, op)
    }
    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let track: crate::model::Track =
            serde_json::from_value(output.data["workspace_track"].clone()).unwrap();
        if let Some(path) = track.workspace.worktree {
            let path = std::path::Path::new(&path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "blocks git worktree add").unwrap();
        }
        self.0.app_server_interact(output, op, ctx).await
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        self.0.spawn_side_effect(output, op, ctx).await
    }
    async fn plan_compensation(
        &self,
        phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        self.0.plan_compensation(phase, reason, output, op).await
    }
    async fn compensate_step(
        &self,
        step: &crate::operation::CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        self.0.compensate_step(step, output, op, ctx).await
    }
}

async fn child_runtime(
    repo: &std::sync::Arc<SqlxRepo>,
    block_worktree: bool,
) -> crate::operation::OperationRuntime {
    use crate::operation::{OperationCompletionBus, OperationRuntime, SqlxOperationRepo};
    use std::sync::Arc;
    let operations = Arc::new(SqlxOperationRepo::new(repo.db().clone()));
    let adapter = ChildTrackAdapter::new(
        std::sync::Arc::new(
            crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
        ),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        test_workspace_root(),
        std::path::PathBuf::from("/unused-objective-checkouts"),
        workspace_runtime::sweep::TeardownTargets::default(),
    );
    let adapter = adapter.with_selection_context(selection_context(repo).await);
    let adapter: Arc<dyn ProviderAdapter> = if block_worktree {
        Arc::new(BlockChildWorktree(adapter))
    } else {
        Arc::new(adapter)
    };
    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    let events = crate::event::EventBus::new();
    let completion = OperationCompletionBus::new();
    let renderer =
        crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(route_repo.clone());
    OperationRuntime::new_unchecked(
        operations.clone(),
        vec![adapter],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operations,
            Arc::new(crate::state::DaemonClient::new_stub()),
            renderer,
            events,
            completion,
        ),
    )
}

async fn stored_planner(repo: &SqlxRepo, child: &str) -> Value {
    let raw: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(child)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[tokio::test]
async fn child_track_config_retry_keeps_frozen_parent_head_and_selection() {
    use crate::operation::{OperationKey, OperationOutcome, OperationReceipt};
    for (parent_worktree, declared) in [(false, false), (true, false), (true, true)] {
        let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let plan = if parent_worktree {
            TrackWorkspacePlan::AttachedWithTrackWorktree(dir.path().to_path_buf())
        } else {
            TrackWorkspacePlan::AttachedFromCwd
        };
        let parent = parent_with_plan(&repo, dir.path(), plan).await;
        if parent_worktree {
            workspace_runtime::track_worktree::ensure_track_worktree(
                &parent,
                crate::workspace_service::ports::ServerWorkspacePorts,
                crate::workspace_service::ports::observe,
            )
            .await
            .unwrap();
            git(
                std::path::Path::new(parent.workspace.agent_cwd()),
                &[
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "parent-only",
                ],
            );
        }
        let parent_cwd = std::path::PathBuf::from(parent.workspace.agent_cwd());
        let frozen_head = git(&parent_cwd, &["rev-parse", "HEAD"]);
        let parent = parent.id.to_string();
        sqlx::query(
            "UPDATE cards SET payload=json_set(payload, \
            '$.permission_mode','full','$.planner_provider','claude','$.model',null, \
            '$.model_ever_set',json('true'),'$.reasoning_effort','high', \
            '$.reasoning_effort_ever_set',json('true')) WHERE track_id=?1 AND role='planner'",
        )
        .bind(&parent)
        .execute(repo.pool())
        .await
        .unwrap();
        let mut expected = stored_planner(&repo, &parent).await;
        if declared {
            expected["model"] = json!("sonnet");
            expected["reasoning_effort"] = json!("low");
        }
        let task = seed_task(&repo, &parent, false).await;
        let mut input = serde_json::to_value(payload(&task)).unwrap();
        if declared {
            input["child_track"] = json!({"model":"sonnet", "reasoning_effort":"low"});
        }
        let key = OperationKey {
            operation_key: new_id(),
            idempotency_key: Some(task.id.clone()),
            payload_hash: crate::routes::idempotency_key::stable_payload_hash(&input).unwrap(),
        };
        let runtime = child_runtime(&repo, true).await;
        let op = runtime
            .submit(CHILD_TRACK_KIND, key.clone(), input.clone())
            .await
            .unwrap();
        let OperationReceipt::AwaitingRetry {
            output, attempt, ..
        } = runtime.observe_operation(&op).await.unwrap()
        else {
            panic!("worktree failure must retain a retryable child")
        };
        let child = output.data["child_track_id"].as_str().unwrap().to_owned();
        assert_eq!(output.data["parent_head"], frozen_head);
        assert!(output.data["workspace_error"].is_string());
        let worktree = output.data["cwd"].as_str().unwrap().to_owned();
        assert_ne!(worktree, parent_cwd.to_str().unwrap());
        // Change every parent choice, and its committed HEAD, while the child waits.
        sqlx::query(
            "UPDATE cards SET payload=json_set(payload,'$.permission_mode','never', \
            '$.planner_provider','codex','$.model','changed','$.reasoning_effort','low', \
            '$.model_ever_set',json('false'),'$.reasoning_effort_ever_set',json('false')) \
            WHERE track_id=?1 AND role='planner'",
        )
        .bind(&parent)
        .execute(repo.pool())
        .await
        .unwrap();
        git(
            &parent_cwd,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "later-parent",
            ],
        );
        std::fs::remove_file(&worktree).unwrap();
        drop(runtime);
        let runtime = child_runtime(&repo, false).await;
        runtime
            .retry_interaction(CHILD_TRACK_KIND, &key, attempt)
            .await
            .unwrap();
        let result = runtime.wait(&op).await.unwrap();
        let OperationOutcome::Succeeded { result } = result.outcome else {
            panic!("{result:?}")
        };
        assert_eq!(result["child_track_id"], child);
        assert_eq!(result["cwd"], worktree);
        assert_eq!(
            git(std::path::Path::new(&worktree), &["rev-parse", "HEAD"]),
            frozen_head
        );
        let stored = stored_planner(&repo, &child).await;
        for field in [
            "permission_mode",
            "planner_provider",
            "model",
            "reasoning_effort",
            "model_ever_set",
            "reasoning_effort_ever_set",
        ] {
            assert_eq!(stored[field], expected[field], "{field}");
        }
        assert_eq!(
            runtime.submit(CHILD_TRACK_KIND, key, input).await.unwrap(),
            op
        );
        let children: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tracks WHERE parent_track_id=?1")
                .bind(&parent)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(children, 1);
        // The actual worker lease preparation reads the child's stored workspace and task.
        let worker = seed_task(&repo, &child, false).await;
        let mut tx = repo.pool().begin().await.unwrap();
        let plan = crate::operation::workspace_lease::worker::prepare_worker_lease_tx(
            &mut tx,
            &child,
            &worker.id,
            &test_workspace_root(),
        )
        .await
        .unwrap();
        assert_eq!(plan.path.to_str().unwrap(), worktree);
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn child_track_config_managed_parent_retains_own_managed_workspace() {
    use crate::operation::{OperationKey, OperationOutcome};
    let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let parent = parent_with_plan(
        &repo,
        std::path::Path::new(""),
        TrackWorkspacePlan::ManagedUnder(test_workspace_root()),
    )
    .await;
    let parent_path = parent.workspace.path;
    let parent = parent.id.to_string();
    let task = seed_task(&repo, &parent, false).await;
    let input = serde_json::to_value(payload(&task)).unwrap();
    let runtime = child_runtime(&repo, false).await;
    let op = runtime
        .submit(
            CHILD_TRACK_KIND,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.id),
                payload_hash: crate::routes::idempotency_key::stable_payload_hash(&input).unwrap(),
            },
            input,
        )
        .await
        .unwrap();
    let outcome = runtime.wait(&op).await.unwrap();
    let OperationOutcome::Succeeded { result } = outcome.outcome else {
        panic!("{outcome:?}")
    };
    let child = result["child_track_id"].as_str().unwrap();
    let (kind, path, worktree, frozen): (String, String, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT workspace_kind,workspace_path,workspace_worktree_path,workspace_frozen_at FROM tracks WHERE id=?1")
        .bind(child).fetch_one(repo.pool()).await.unwrap();
    assert_eq!(kind, "managed");
    assert!(worktree.is_none());
    assert!(frozen.is_some());
    assert_eq!(result["cwd"], path);
    assert_ne!(path, parent_path);
    git(std::path::Path::new(&path), &["rev-parse", "HEAD"]);
}

async fn selection_context(
    repo: &std::sync::Arc<SqlxRepo>,
) -> crate::planner_model::child_selection::ChildSelectionContext {
    use std::sync::Arc;
    let availability = Arc::new(crate::agent_providers::ProviderAvailabilityCache::default());
    let model = provider::claude::discovery::ClaudeModel {
        value: "sonnet".into(),
        resolved_model: "sonnet".into(),
        display_name: "Sonnet".into(),
        description: String::new(),
        effort_levels: vec!["low".into(), "high".into()],
    };
    let parent_model = provider::claude::discovery::ClaudeModel {
        value: "parent-model".into(),
        ..model.clone()
    };
    availability
        .seed_claude_catalog(crate::claude_planner::models::ClaudeCatalog {
            default: model.clone(),
            models: vec![model, parent_model],
            fetched_at_ms: now_ms(),
        })
        .await;
    crate::planner_model::child_selection::ChildSelectionContext {
        availability,
        claude: Arc::new(
            crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch().unwrap(),
        ),
        acp: Arc::new(crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap()),
        codex: crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
    }
}

#[cfg(test)]
mod selection_tests;
