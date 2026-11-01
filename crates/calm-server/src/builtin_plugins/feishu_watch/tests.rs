use super::adapter::Adapter;
use super::*;
use crate::{
    card_role_cache::CardRoleCache,
    db::{prelude::*, sqlite::SqlxRepo},
    event::EventBus,
    mcp_server::registry::{AppContext, ToolCallIdentity},
    model::{CardRole, NewArea, NewCard, NewTrack},
    plugin_host::{PluginHost, PluginRegistry},
    session_projection_repo::{AgentProvider, WorkerSessionKind},
    state::WriteContext,
    track_area_cache::TrackAreaCache,
};
use calm_truth::db::{RepoOutOfDomain, RepoRead};
use plugin::builtin::feishu_watch::{
    Storage,
    model::*,
    reader::{ReadError, Reader},
};
use serde_json::json;
struct Fixture {
    ctx: Arc<AppContext>,
    repo: Arc<SqlxRepo>,
    host: Arc<PluginHost>,
    dir: tempfile::TempDir,
    roles: CardRoleCache,
    areas: TrackAreaCache,
}
impl Fixture {
    async fn configured() -> (Self, ToolCallIdentity) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let events = EventBus::new();
        let roles = CardRoleCache::new();
        let areas = TrackAreaCache::new();
        let write = WriteContext::new(roles.clone(), areas.clone());
        let host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty().with_builtins()),
            repo.clone(),
            dir.path().join("plugins"),
            dir.path().join("data"),
            vec![],
            events.clone(),
            write.clone(),
        ));
        host.reconcile_builtins().await.unwrap();
        let cell = Arc::new(tokio::sync::OnceCell::new());
        assert!(cell.set(host.clone()).is_ok());
        let ctx = AppContext::new(
            repo.clone(),
            events,
            write,
            None,
            cell,
            Arc::new(tokio::sync::OnceCell::new()),
            dir.path().join("gates"),
        );
        let fx = Self {
            ctx,
            repo,
            host,
            dir,
            roles,
            areas,
        };
        let owner = fx.identity().await;
        let file = fx.dir.path().join("legacy.json");
        std::fs::write(&file,json!({"schema_version":1,"profile":"fixture","self_open_id":"ou_fixture","identity":"user","enabled":true,
            "interval_minutes":30,"overlap_minutes":5,"monitor_sources":["group"],"group":{"chat_id":"oc_fixture"},
            "source_watermarks":{"group":"2026-11-01T00:00:00+08:00"},"tracked_threads":[],
            "notified_message_ids":[],"handled_message_ids":[],"acknowledged_in_chat_message_ids":[],"pending_items":[],
            "rules":"Only Ask on actionable information; do not send external messages."}).to_string()).unwrap();
        let cfg = json!({"track_id":owner.track_id,"cli_path":"/usr/bin/true","home":fx.dir.path(),"profile":"fixture","self_open_id":"ou_fixture","import_state_path":file});
        fx.repo
            .plugin_update_user_config(PLUGIN_ID, cfg)
            .await
            .unwrap();
        fx.host.enable(PLUGIN_ID).await.unwrap();
        (fx, owner)
    }
    async fn identity(&self) -> ToolCallIdentity {
        let area = self
            .repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "inspection".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = self
            .repo
            .track_create(NewTrack {
                area_id: area.id.clone(),
                title: "inspection".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                template_input: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::model::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = self
            .repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: json!({"planner_provider":"codex"}),
                title: None,
            })
            .await
            .unwrap();
        (self.repo.db())
            .writer()
            .execute(
                sqlx::query("UPDATE cards SET role='planner' WHERE id=?").bind(card.id.as_str()),
            )
            .await
            .unwrap();
        self.repo.seed_card_role_cache(&self.roles).await.unwrap();
        self.repo.seed_track_area_cache(&self.areas).await.unwrap();
        self.repo
            .seed_card_role_cache(self.repo.card_role_cache())
            .await
            .unwrap();
        let session = crate::model::new_id();
        {
            let write_db = (self.repo.db()).clone();
            write_db
                .writer()
                .with_writer(|write_session| {
                    Box::pin(async {
                        let mut tx = write_session.begin_immediate().await.unwrap();
                        crate::db::sqlite::session_start_runtime_tx(
                            &mut tx,
                            crate::session_projection_repo::WorkerSessionInit {
                                id: session.clone(),
                                card_id: card.id.to_string(),
                                kind: WorkerSessionKind::SharedPlanner,
                                agent_provider: Some(AgentProvider::Codex),
                                status: crate::session_projection_repo::WorkerSessionState::Running,
                                terminal_run_id: None,
                                thread_id: Some(session.clone()),
                                session_id: None,
                                active_turn_id: None,
                                handle_state_json: None,
                                spawn_op_id: None,
                                now_ms: crate::model::now_ms(),
                            },
                        )
                        .await
                        .unwrap();
                        tx.commit().await.unwrap();
                    })
                })
                .await
                .expect("independent write operation")
        };
        ToolCallIdentity {
            card_id: card.id.to_string(),
            role: CardRole::Planner,
            provider: AgentProvider::Codex,
            session_id: session.clone(),
            track_id: Some(track.id.to_string()),
            area_id: area.id.to_string(),
            thread_id: session,
        }
    }
    async fn wakes(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='track.wake_requested' AND json_extract(payload,'$.source')=?").bind(PLUGIN_ID).fetch_one(self.repo.pool()).await.unwrap()
    }
    async fn initialize(&self) -> Adapter {
        let a = Adapter::capture(&self.ctx, None).await.unwrap();
        feature::service::poll(&a, &Empty, &a.config, 1_793_500_000_000)
            .await
            .unwrap();
        a
    }
}
struct Empty;
#[async_trait::async_trait]
impl Reader for Empty {
    async fn verify_identity(&self, _: &Config) -> Result<(), ReadError> {
        Ok(())
    }
    async fn read(
        &self,
        _: &Config,
        _: &Source,
        _: i64,
        _: i64,
    ) -> Result<Vec<Message>, ReadError> {
        Ok(vec![])
    }
}
#[tokio::test]
async fn inspection_quiet_repeated_scans_preserve_one_handoff_wake_and_restart_state() {
    let (fx, _) = Fixture::configured().await;
    let a = fx.initialize().await;
    assert_eq!(fx.wakes().await, 1);
    for i in 1..=10 {
        feature::service::poll(&a, &Empty, &a.config, 1_793_500_000_000 + i * 1_800_000)
            .await
            .unwrap();
    }
    assert_eq!(
        fx.wakes().await,
        1,
        "quiet scans must never manufacture model input"
    );
    let before = a.load().await.unwrap().unwrap();
    let after = Adapter::capture(&fx.ctx, None)
        .await
        .unwrap()
        .load()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.revision, before.revision);
    assert!(after.inbox.contains_key("import"));
}
#[tokio::test]
async fn inspection_event_failure_rolls_back_inbox_and_checkpoint() {
    let (fx, _) = Fixture::configured().await;
    let a = Adapter::capture(&fx.ctx, None).await.unwrap();
    (fx.repo.db())
        .writer()
        .execute(sqlx::query(
            "CREATE TRIGGER reject_inspection_wake BEFORE INSERT ON events \
        WHEN NEW.kind='track.wake_requested' \
        BEGIN SELECT RAISE(ABORT,'fixture wake failure'); END",
        ))
        .await
        .unwrap();
    assert!(
        feature::service::poll(&a, &Empty, &a.config, 1_793_500_000_000)
            .await
            .is_err()
    );
    assert!(a.load().await.unwrap().is_none());
    assert_eq!(fx.wakes().await, 0);
    (fx.repo.db())
        .writer()
        .execute(sqlx::query("DROP TRIGGER reject_inspection_wake"))
        .await
        .unwrap();
    feature::service::poll(&a, &Empty, &a.config, 1_793_500_000_000)
        .await
        .unwrap();
    assert_eq!(fx.wakes().await, 1);
}
#[tokio::test]
async fn inspection_stale_cas_rejects_entire_publication() {
    let (fx, _) = Fixture::configured().await;
    let a = fx.initialize().await;
    let mut next = a.load().await.unwrap().unwrap();
    let revision = next.revision;
    next.revision += 1;
    a.commit(Some(revision), next.clone(), None).await.unwrap();
    assert!(
        a.commit(
            Some(revision),
            next,
            Some(Wake {
                key: "stale".into(),
                text: "must not wake".into()
            })
        )
        .await
        .is_err()
    );
    assert_eq!(fx.wakes().await, 1);
}
#[tokio::test]
async fn inspection_disabled_reenabled_runtime_cannot_publish_stale_scan() {
    let (fx, _) = Fixture::configured().await;
    let a = fx.initialize().await;
    let mut next = a.load().await.unwrap().unwrap();
    let revision = next.revision;
    next.revision += 1;
    fx.host.disable(PLUGIN_ID).await.unwrap();
    fx.host.enable(PLUGIN_ID).await.unwrap();
    assert!(
        a.commit(Some(revision), next, None).await.is_err(),
        "same-config reenable must reject old runtime generation"
    );
    assert_eq!(fx.wakes().await, 1);
}
#[tokio::test]
async fn inspection_closed_track_and_changed_config_cannot_advance() {
    let (fx, owner) = Fixture::configured().await;
    let a = fx.initialize().await;
    let original = a.load().await.unwrap().unwrap();
    let mut next = original.clone();
    next.revision += 1;
    (fx.repo.db())
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?")
                .bind(owner.track_id.as_ref().unwrap()),
        )
        .await
        .unwrap();
    assert!(
        a.commit(Some(original.revision), next.clone(), None)
            .await
            .is_err()
    );
    (fx.repo.db())
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET closed_at=NULL WHERE id=?")
                .bind(owner.track_id.as_ref().unwrap()),
        )
        .await
        .unwrap();
    let mut config = serde_json::to_value(a.config.clone()).unwrap();
    config["profile"] = json!("changed");
    fx.repo
        .plugin_update_user_config(PLUGIN_ID, config)
        .await
        .unwrap();
    assert!(a.commit(Some(original.revision), next, None).await.is_err());
    assert_eq!(fx.wakes().await, 1);
}
#[tokio::test]
async fn inspection_native_tools_fence_foreign_and_stale_planners() {
    let (fx, owner) = Fixture::configured().await;
    fx.initialize().await;
    let registry = crate::mcp_server::build_default_registry();
    let status = registry.lookup("plugin_feishuwatch_status").unwrap();
    status(fx.ctx.clone(), owner.clone(), json!({}))
        .await
        .unwrap();
    let other = fx.identity().await;
    assert!(status(fx.ctx.clone(), other, json!({})).await.is_err());
    let mut stale = owner.clone();
    stale.session_id = "retired".into();
    assert!(status(fx.ctx.clone(), stale, json!({})).await.is_err());
    let settle = registry.lookup("plugin_feishuwatch_settle").unwrap();
    let mut stale = owner;
    stale.session_id = "retired".into();
    assert!(settle(fx.ctx.clone(),stale,json!({"batch_id":"import","classification":{},"deadlines":[],"cancel_deadline_keys":[]})).await.is_err());
    assert!(
        Adapter::capture(&fx.ctx, None)
            .await
            .unwrap()
            .load()
            .await
            .unwrap()
            .unwrap()
            .inbox["import"]
            .settlement
            .is_none()
    );
}
#[tokio::test]
async fn inspection_capacity_diagnostic_is_bounded_once_and_preserves_checkpoint() {
    let (fx, _) = Fixture::configured().await;
    let a = fx.initialize().await;
    let before = a.load().await.unwrap().unwrap();
    let mut huge = before.clone();
    huge.revision += 1;
    huge.legacy = json!({"large":"x".repeat(1024*1024)});
    assert!(a.commit(Some(before.revision), huge, None).await.is_err());
    a.diagnostic(true).await.unwrap();
    a.diagnostic(true).await.unwrap();
    assert_eq!(fx.wakes().await, 2);
    assert_eq!(a.load().await.unwrap().unwrap().revision, before.revision);
    a.diagnostic(false).await.unwrap();
    a.diagnostic(false).await.unwrap();
    assert_eq!(fx.wakes().await, 3);
}

#[tokio::test]
async fn inspection_revoked_runtime_refuses_new_external_reads() {
    let (fx, _) = Fixture::configured().await;
    let a = fx.initialize().await;
    let reader = GuardedReader {
        adapter: &a,
        reader: Empty,
    };
    assert!(reader.verify_identity(&a.config).await.is_ok());
    fx.host.disable(PLUGIN_ID).await.unwrap();
    fx.host.enable(PLUGIN_ID).await.unwrap();
    assert!(reader.verify_identity(&a.config).await.is_err());
    assert!(
        reader
            .read(&a.config, &Source::IncomingP2p, 0, 1)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn inspection_registered_reminder_changes_need_no_new_batch_and_reject_stale_owner() {
    let (fx, owner) = Fixture::configured().await;
    let a = fx.initialize().await;
    let registry = crate::mcp_server::build_default_registry();
    let update = registry.lookup("plugin_feishuwatch_reminder_set").unwrap();
    let revision = a.load().await.unwrap().unwrap().revision;
    let request = json!({"idempotency_key":"first","expected_version":revision,"key":"review","at_ms":crate::model::now_ms()+60_000,"note":"Ask reference"});
    update(fx.ctx.clone(), owner.clone(), request.clone())
        .await
        .unwrap();
    update(fx.ctx.clone(), owner.clone(), request)
        .await
        .unwrap();
    let state = a.load().await.unwrap().unwrap();
    assert!(state.due.contains_key("review"));
    assert_eq!(state.inbox.len(), 1);
    let cancel =
        json!({"idempotency_key":"answered","expected_version":state.revision,"key":"review"});
    let remove = registry.lookup("plugin_feishuwatch_reminder_rm").unwrap();
    let mut stale = owner.clone();
    stale.session_id = "retired".into();
    assert!(remove(fx.ctx.clone(), stale, cancel.clone()).await.is_err());
    assert!(a.load().await.unwrap().unwrap().due.contains_key("review"));
    remove(fx.ctx.clone(), owner, cancel).await.unwrap();
    assert!(a.load().await.unwrap().unwrap().due.is_empty());
}
