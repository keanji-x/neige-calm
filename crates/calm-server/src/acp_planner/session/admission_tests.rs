use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, new_id};
use crate::session_projection_repo::{WorkerSessionInit, WorkerSessionKind, WorkerSessionState};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use provider::send_boundary::{PreSendGate, SendRefusal};
use tokio::io::AsyncReadExt;

pub(super) async fn fixture() -> (AcpPlannerSession, Arc<SqlxRepo>, String) {
    fixture_for(false).await
}

async fn fixture_for(task_worker: bool) -> (AcpPlannerSession, Arc<SqlxRepo>, String) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "ACP boot".into(),
                color: "#111111".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "ACP boot".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let write_db = (repo.db()).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin_immediate().await.unwrap();
        let card = card_create_with_id_tx(&mut tx, new_id(), NewCard {
            track_id: track.id.clone(), title: None, kind: "codex".into(), sort: None,
            payload: if task_worker { json!({"schemaVersion":1,"harness_profile":"task_worker","worker_provider":"opencode"}) } else { json!({"schemaVersion":1,"planner_harness":true,"planner_provider":"opencode","permission_mode":"never"}) },
        }, if task_worker { CardRole::Worker } else { CardRole::Planner }, false, &crate::card_role_cache::CardRoleCache::new()).await.unwrap();
        let worker = new_id();
        let thread = new_id();
        if task_worker {
            // Step 2 tests live admission against real persisted identities. The
            // legacy runtime-creation adapter remains intentionally disabled until step 3.
            sqlx::query(
                "INSERT INTO worker_sessions
                 (id,track_id,provider,mode,contract,state,thread_id,card_id,created_at_ms,updated_at_ms)
                 VALUES (?1,?2,'opencode','resumable','executor','idle',?3,?4,?5,?5)"
            )
                .bind(&worker).bind(track.id.as_str()).bind(&thread).bind(card.id.as_str())
                .bind(crate::model::now_ms()).execute(&mut **tx).await.unwrap();
            sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                .bind(&worker).bind(card.id.as_str()).execute(&mut **tx).await.unwrap();
        } else {
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: worker.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::OpenCode),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some(thread.clone()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: crate::model::now_ms(),
            },
        )
        .await
        .unwrap();
        }
        tx.commit().await.unwrap();
        let host = Arc::new(AcpPlannerHost::unconfigured_scratch().unwrap());
        let session = AcpPlannerSession::open(SessionParams {
            engine: host.execution.engine().cloned(),
            host,
            provider: AgentProvider::OpenCode,
            repo: repo.clone(),
            seals: Arc::new(ThreadSeals::new()),
            worker_session_id: worker,
            card_id: card.id.to_string(),
            track_id: track.id.to_string(),
            cwd: "/tmp".into(),
            instructions: String::new(),
        });
        (session, repo, thread)

    })).await.expect("independent write operation")
}

#[tokio::test]
async fn acp_final_live_fence_refuses_setup_prompt_and_cancel_after_writer_wait() {
    final_live_fence(false).await;
    final_live_fence(true).await;
}

async fn final_live_fence(parked: bool) {
    for method in [
        "session/new",
        "session/load",
        "session/resume",
        "session/set_config_option",
        "session/prompt",
        "session/cancel",
    ] {
        let (session, _repo, thread) = fixture().await;
        let admission =
            session
                .shared
                .admission(&thread, Some(PlannerPermissionMode::Never), None, None);
        admission.check_local().await.unwrap();
        let (input, _input_peer) = tokio::io::duplex(4096);
        let (output, mut peer) = tokio::io::duplex(4096);
        let connection = provider::acp::Connection::new(input, output);
        let entered = tokio::sync::Notify::new();
        let release = tokio::sync::Notify::new();
        let blocked = PreSendGate::new(|| {
            Box::pin(async {
                entered.notify_one();
                release.notified().await;
                Err(SendRefusal("blocker finished".into()))
            })
        });
        let blocker = connection
            .client
            .notify_gated("blocker", json!({}), &blocked);
        tokio::pin!(blocker);
        tokio::select! { _=&mut blocker=>panic!("blocker must wait"), _=entered.notified()=>{} }
        let send = async {
            if method == "session/cancel" {
                protocol::cancel(&connection.client, "native", admission.gate()).await
            } else {
                connection
                    .client
                    .submit_gated(method, json!({}), admission.gate())
                    .await
                    .map(drop)
            }
        };
        tokio::pin!(send);
        assert!(futures_util::poll!(&mut send).is_pending());
        let _park = if parked {
            Some(session.carrier_idle_for_detach().unwrap())
        } else {
            session.shared.params.seals.seal_for_deletion(&thread);
            None
        };
        release.notify_one();
        assert!(matches!(
            blocker.await,
            Err(provider::acp::WriteFailure::BeforeWrite(_))
        ));
        assert!(
            matches!(send.await, Err(provider::acp::WriteFailure::BeforeWrite(_))),
            "{method}"
        );
        assert!(
            admission.take_refusal().is_some(),
            "server owns the original refusal"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.read_u8())
                .await
                .is_err(),
            "{method} wrote bytes"
        );
    }
}

#[tokio::test]
async fn task_worker_acp_full_admission() {
    let (session, _repo, thread) = fixture_for(true).await;
    let row = session
        .shared
        .current_owner(&thread, Some(PlannerPermissionMode::Full))
        .await
        .unwrap();
    assert_eq!(row.contract, WorkerContract::Executor);
}

#[tokio::test]
async fn task_worker_acp_rejects_non_full_admission() {
    let (session, _repo, thread) = fixture_for(true).await;
    for permission in [
        None,
        Some(PlannerPermissionMode::Never),
        Some(PlannerPermissionMode::Ask),
    ] {
        let error = session
            .shared
            .current_owner(&thread, permission)
            .await
            .expect_err("non Full must refuse");
        assert!(matches!(CalmError::from(error), CalmError::Conflict(_)));
    }
}

#[tokio::test]
async fn task_worker_acp_cancel_uses_exact_admitted_turn_permission() {
    let (session, repo, thread) = fixture_for(true).await;
    let worker = session.shared.params.worker_session_id.clone();
    repo.db()
        .writer()
        .with_writer(|writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                sqlx::query("UPDATE worker_sessions SET agent_session_id='native' WHERE id=?1")
                    .bind(worker)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    let (cancel, _receiver) = watch::channel(CancelRequest::Running);
    session.shared.state.lock().unwrap().active = Some(Active {
        permission: PlannerPermissionMode::Full,
        thread: thread.clone(),
        turn: "turn".into(),
        cancel,
        approvals: std::sync::Weak::new(),
        native_session_id: "native".into(),
        execution_id: None,
    });
    session
        .shared
        .admission(&thread, None, Some("native"), Some("turn"))
        .check_local()
        .await
        .unwrap();
    assert!(
        session
            .shared
            .admission(&thread, None, Some("native"), Some("other-turn"))
            .check_local()
            .await
            .is_err()
    );
}
