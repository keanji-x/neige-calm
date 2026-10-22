use super::tests::task;
use super::*;

#[tokio::test]
async fn isolation_none_scheduler_refuses_upstream_worker_before_fetch_writes() {
    use crate::operation::workspace_lease::upstream_tests::{attach_origin, attached_repo};
    fn bytes(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn visit(
            at: &std::path::Path,
            root: &std::path::Path,
            out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
        ) {
            for entry in std::fs::read_dir(at).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, root, out);
                } else {
                    out.insert(
                        path.strip_prefix(root).unwrap().into(),
                        std::fs::read(&path).unwrap(),
                    );
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        visit(root, root, &mut out);
        out
    }
    let source = attached_repo();
    let origin = attach_origin(source.path());
    origin.commit("remote ahead");
    let concrete = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let repo: Arc<dyn Repo> = concrete.clone();
    let route_repo: Arc<dyn crate::db::RouteRepo> = concrete.clone();
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            crate::model::NewArea {
                name: "none".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            area_id: area.id,
            title: "none".into(),
            cwd: source.path().to_str().unwrap().into(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut dispatched = task("none", TaskStatus::Dispatched, &[], 0);
    dispatched.id = format!("{}:none", track.id);
    dispatched.track_id = track.id.to_string();
    dispatched.start = TaskStart::Upstream;
    let mut tx = crate::db::sqlite::begin_immediate_tx(concrete.db())
        .await
        .unwrap();
    crate::test_support::insert_task_tx(&mut tx, &dispatched)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let events = EventBus::new();
    let write = WriteContext::new(
        concrete.card_role_cache().clone(),
        concrete.track_area_cache().clone(),
    );
    let operation_repo = Arc::new(crate::operation::SqlxOperationRepo::new(
        concrete.db().clone(),
    ));
    let completion = crate::operation::OperationCompletionBus::new();
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        Vec::new(),
        events.clone(),
        completion.clone(),
        crate::operation::SpawnCtx::new(
            route_repo.clone(),
            operation_repo,
            Arc::new(crate::state::DaemonClient::new_stub()),
            crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(route_repo),
            events.clone(),
            completion,
        ),
    ));
    let scheduler = Scheduler::new(
        repo.clone(),
        events,
        write,
        Arc::downgrade(&runtime),
        crate::per_card_lock::new_per_card_locks(),
        Arc::new(Semaphore::new(1)),
        source.path().join("unused-gate-logs"),
        crate::scheduler::WorkerLiveness::DEFAULT,
        crate::scheduler::WorkerIdleWake::new(
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
            crate::scheduler::WORKER_IDLE_TURN_GRACE,
            crate::scheduler::WORKER_IDLE_PROBE_TIMEOUT,
        ),
    );
    let before = bytes(source.path());
    scheduler
        .drive_spawn(&dispatched, &track, false)
        .await
        .unwrap();
    assert_eq!(
        bytes(source.path()),
        before,
        "none must be refused before upstream refresh"
    );
    assert_eq!(
        repo.task_get(&dispatched.id).await.unwrap().unwrap().status,
        TaskStatus::Failed
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM operations")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
