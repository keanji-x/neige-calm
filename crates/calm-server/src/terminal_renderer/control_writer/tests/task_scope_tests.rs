//! Hold actual pump work, change persisted authority, then run the real writer.
use super::*;
use crate::card_role_cache::CardRoleCache;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, card_with_codex_create_tx, card_with_terminal_create_tx};
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::{CardRole, NewArea, NewTrack, new_id, now_ms};
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use crate::terminal_interaction::{Binding, TaskBinding, TerminalInteraction};
use serde_json::json;

async fn queued_task_change(replace_session: bool) {
    let sql = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = sql
        .area_create(NewArea {
            name: "queued".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let theme = crate::routes::theme::RequestTheme::default_dark();
    let track = sql
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "queued".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme,
        })
        .await
        .unwrap();
    let planner = new_id();
    let planner_session = new_id();
    let card = new_id();
    let session = new_id();
    let key = new_id();
    let task = format!("{}:{key}", track.id);
    let op = SqlxOperationRepo::new(sql.pool().clone())
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.clone()),
                payload_hash: key.clone(),
            },
            json!({"track_id":track.id,"cmd":"true"}),
        )
        .await
        .unwrap();
    let roles = CardRoleCache::new();
    let mut tx = sql.pool().begin().await.unwrap();
    card_with_codex_create_tx(
        &mut tx,
        planner.clone(),
        &planner_session,
        None,
        track.id.clone(),
        None,
        None,
        "/tmp".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Planner,
        false,
        &roles,
        theme,
    )
    .await
    .unwrap();
    let (_, terminal) = card_with_terminal_create_tx(
        &mut tx,
        card.clone(),
        &session,
        Some(&op),
        track.id.clone(),
        None,
        None,
        "/bin/sh".into(),
        "/tmp".into(),
        json!({}),
        CardRole::Worker,
        true,
        &roles,
        theme,
        false,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sql.session_projection_set_status_for_card(&card, WorkerSessionState::Running)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,declared_by,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,'terminal','test','[]','running',?4,'user',?5,?5)")
        .bind(&task).bind(track.id.as_str()).bind(&key).bind(&card).bind(now_ms()).execute(sql.pool()).await.unwrap();
    let identity = ToolCallIdentity {
        card_id: planner,
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: planner_session,
        track_id: Some(track.id.to_string()),
        area_id: area.id.to_string(),
        thread_id: "card-bound".into(),
    };
    let binding = Binding {
        terminal_id: terminal.id.clone(),
        card_id: card.clone(),
        worker_session_id: session.clone(),
        task: Some(TaskBinding {
            task_id: task.clone(),
            task_key: key,
        }),
    };
    // This is the same factory used by real Planner clients, not a test policy.
    let scope = TerminalInteraction::bound_scope(sql.clone(), &identity, &binding);
    assert!(scope.allowed().await);
    assert!(scope.control_allowed().await);
    let barrier = Arc::new(crate::terminal_renderer::InputBarrier::default());
    let registry = Arc::new(Mutex::new(OwnerRegistry::new()));
    let (events, _) = broadcast::channel(32);
    let (control, mut queue) = mpsc::unbounded_channel();
    let mut client = scoped_client(barrier, registry, control.clone(), events, scope.clone()).await;
    client
        .input
        .send(ClientMsg::Input {
            data: b"revoked".to_vec(),
            input_seq: 1,
        })
        .await
        .unwrap();
    let queued = tokio::time::timeout(Duration::from_secs(2), queue.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&queued, SupervisorControl::Write(_)));
    if replace_session {
        let mut tx = sql.pool().begin().await.unwrap();
        crate::db::sqlite::session_supersede_and_start_tx(
            &mut tx,
            &session,
            WorkerSessionInit {
                id: new_id(),
                card_id: card,
                kind: WorkerSessionKind::Terminal,
                agent_provider: None,
                status: WorkerSessionState::Running,
                terminal_run_id: Some(terminal.id),
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: Some(op),
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    } else {
        sqlx::query("UPDATE tasks SET status='done',finished_at_ms=?2 WHERE id=?1")
            .bind(&task)
            .bind(now_ms())
            .execute(sql.pool())
            .await
            .unwrap();
        // Completion retains read access; using observe scope at write time is wrong.
        assert!(scope.allowed().await);
    }
    assert!(!scope.control_allowed().await);
    control.send(queued).unwrap();
    let (writer, mut peer) = UnixStream::pair().unwrap();
    let writer_task = spawn_supervisor_control_writer(writer, "term:queued-task".into(), queue);
    let result = tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            tokio::select! {
                sent = read_frame::<ControlMsg,_>(&mut peer) => return Err(format!("revoked task input reached supervisor: {sent:?}")),
                message = client.output.recv() => if matches!(message,Some(DaemonMsg::ProtocolError {code:calm_session::ProtocolErrorCode::NotOwner,..})) { return Ok(()); },
            }
        }
    }).await;
    writer_task.abort();
    let _ = writer_task.await;
    assert!(matches!(result, Ok(Ok(()))), "{result:?}");
}

#[tokio::test]
async fn task_completion_revokes_already_queued_input() {
    queued_task_change(false).await;
}
#[tokio::test]
async fn worker_session_replacement_revokes_already_queued_input() {
    queued_task_change(true).await;
}
