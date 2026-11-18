//! Atomic publication of the terminal-free task executor and its managed identity.
use super::{card_create_with_id_tx, session_start_runtime_tx};
use crate::{
    card_role_cache::CardRoleCache,
    error::Result,
    model::{Card, CardRole, NewCard},
    session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    },
};
use sqlx::{Sqlite, Transaction};

/// A task's durable card binding survives malformed or historical harness markers.
/// Both the task row and the scheduler's creation operation are authoritative evidence.
pub async fn card_has_task_binding_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<bool> {
    let bound: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tasks WHERE worker_card_id=?1)")
            .bind(card_id)
            .fetch_one(&mut **tx)
            .await?;
    Ok(bound || super::card_is_worker_spawn_target_tx(tx, card_id).await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn task_worker_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    worker: String,
    op: String,
    card: NewCard,
    provider: AgentProvider,
    acp_registration: Option<String>,
    cache: &CardRoleCache,
) -> Result<Card> {
    use calm_types::runtime::AgentProtocol;
    if card.kind != "codex"
        || card.payload["harness_profile"] != "task_worker"
        || card.payload["worker_provider"] != serde_json::to_value(&provider)?
        || card.payload["idempotency_key"]
            .as_str()
            .is_none_or(|id| id.is_empty())
        || provider == AgentProvider::Claude
        || (provider.protocol() == AgentProtocol::Acp) != acp_registration.is_some()
    {
        return Err(crate::error::CalmError::BadRequest(
            "invalid managed task worker identity",
        ));
    }
    let card =
        card_create_with_id_tx(tx, card_id.clone(), card, CardRole::Worker, false, cache).await?;
    let mut init = WorkerSessionInit::shared_planner(
        worker.clone(),
        card_id,
        provider,
        WorkerSessionState::Starting,
        None,
        serde_json::json!({}),
        crate::model::now_ms(),
    );
    init.kind = WorkerSessionKind::ManagedWorker;
    init.spawn_op_id = Some(op);
    session_start_runtime_tx(tx, init).await?;
    if let Some(digest) = acp_registration {
        sqlx::query(
            "INSERT INTO acp_managed_sessions(worker_session_id,registration_digest) VALUES(?1,?2)",
        )
        .bind(&worker)
        .bind(digest)
        .execute(&mut **tx)
        .await?;
    }
    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::RepoRead;
    use crate::db::sqlite::session_conversation_tests::card;
    use crate::db::sqlite::{SqlxRepo, session_get_tx, session_projection_by_id_tx};
    use serde_json::json;

    #[tokio::test]
    async fn task_worker_acp_session_actor_reports_only_on_its_own_card() {
        use crate::event::{Event, EventScope};
        use crate::ids::ActorId;
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let (_, track_id) = card(&repo).await;
        let track = repo.track_get(&track_id).await.unwrap().unwrap();
        let db = repo.db().clone();
        db.writer().with_writer(|writer| Box::pin(async move {
            let mut tx = writer.begin_immediate().await.unwrap();
            for provider in [AgentProvider::OpenCode, AgentProvider::Antigravity, AgentProvider::Grok] {
                let card_id = format!("worker-{}", provider.wire_name());
                let worker = format!("session-{card_id}");
                let op = format!("op-{card_id}");
                sqlx::query(concat!("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,",
                    "target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) ",
                    "VALUES(?1,?1,'agent-worker',?1,'fixture','card','{}','{}','pending',1,1)"))
                    .bind(&op).execute(&mut **tx).await.unwrap();
                task_worker_create_tx(&mut tx, card_id.clone(), worker.clone(), op.clone(), NewCard {
                    track_id: track.id.clone(), kind: "codex".into(), title: None, sort: None,
                    payload: json!({"schemaVersion":1,"harness_profile":"task_worker",
                        "worker_provider":provider,"idempotency_key":op}),
                }, provider.clone(), Some("registration".into()), repo.card_role_cache()).await.unwrap();
                super::super::session_set_status_tx(&mut tx, &worker, WorkerSessionState::Running).await.unwrap();
                let actor = ActorId::for_agent_session(CardRole::Worker, &provider, worker.into());
                let event = Event::TaskCompleted { notification: calm_types::event::PlannerNotification::Notify,
                    idempotency_key: op, result: json!(null), artifacts: vec![], agent_message: None };
                let scope = EventScope::Card { card: card_id.clone().into(), track: track.id.clone(), area: track.area_id.clone() };
                crate::decision_gate::enforce_role_resolving_session_from_tx(&mut *tx, &actor, &event, &scope)
                    .await.expect("ACP worker may report its own outcome");
                let foreign = EventScope::Card { card: "foreign".into(), track: track.id.clone(), area: track.area_id.clone() };
                assert!(crate::decision_gate::enforce_role_resolving_session_from_tx(&mut *tx, &actor, &event, &foreign).await.is_err());
                sqlx::query("UPDATE cards SET payload=json_set(payload,'$.harness_profile','plain_chat') WHERE id=?1")
                    .bind(&card_id).execute(&mut **tx).await.unwrap();
                assert!(crate::decision_gate::enforce_role_resolving_session_from_tx(&mut *tx, &actor, &event, &scope).await.is_err());
            }
            tx.rollback().await.unwrap();
        })).await.unwrap();
    }

    #[tokio::test]
    async fn task_worker_storage_creates_terminal_free_managed_executor_for_each_provider() {
        use crate::session_projection_repo::{AgentProvider, WorkerSessionKind};
        use calm_types::worker::{SessionMode, WorkerContract};
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let (_, track) = card(&repo).await;
        let db = repo.db().clone();
        db.writer().with_writer(|session|Box::pin(async move {
        let mut tx=session.begin_immediate().await.unwrap();
        for provider in [AgentProvider::Codex,AgentProvider::Antigravity,AgentProvider::OpenCode,AgentProvider::Grok] {
            let card=format!("task-worker-{}",provider.wire_name());
            let worker=format!("session-{card}");
            let op=format!("spawn-{}",provider.wire_name());
            sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,
                target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms)
                VALUES(?1,?1,'agent-worker',?1,'fixture','card','{}','{}','pending',1,1)")
                .bind(&op).execute(&mut **tx).await.unwrap();
            let digest=(provider!=AgentProvider::Codex).then(||"registration".to_owned());
            task_worker_create_tx(&mut tx,card.clone(),worker.clone(),op.clone(),NewCard {
                track_id:track.clone().into(),kind:"codex".into(),title:None,sort:None,
                payload:json!({"schemaVersion":1,"harness_profile":"task_worker","worker_provider":provider,"idempotency_key":op}),
            },provider.clone(),digest.clone(),repo.card_role_cache()).await.unwrap();
            let runtime=session_projection_by_id_tx(&mut tx,&worker).await.unwrap().unwrap();
            assert_eq!(runtime.kind,WorkerSessionKind::ManagedWorker);
            assert_eq!(runtime.agent_provider,Some(provider.clone()));
            assert!(runtime.terminal_run_id.is_none());
            let stored=session_get_tx(&mut tx,&worker.clone().into()).await.unwrap().unwrap();
            assert_eq!(stored.mode,SessionMode::Resumable);
            assert_eq!(stored.contract,WorkerContract::Executor);
            assert_eq!(stored.spawn_op_id.as_deref(),Some(op.as_str()));
            let terminals:i64=sqlx::query_scalar("SELECT count(*) FROM terminals WHERE card_id=?1").bind(&card).fetch_one(&mut **tx).await.unwrap();
            assert_eq!(terminals,0);
            let registration:Option<String>=sqlx::query_scalar("SELECT registration_digest FROM acp_managed_sessions WHERE worker_session_id=?1")
                .bind(worker).fetch_optional(&mut **tx).await.unwrap();
            assert_eq!(registration,digest);
        }
        tx.rollback().await.unwrap();
    })).await.unwrap();
    }
    #[tokio::test]
    async fn task_worker_native_conversation_view_reads_every_page() {
        let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let (_, track) = card(&repo).await;
        let db = repo.db().clone();
        let writer_repo = repo.clone();
        let writer_track = track.clone();
        db.writer().with_writer(move|session|Box::pin(async move {
            let mut tx=session.begin_immediate().await.unwrap();
            sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,
                target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms)
                VALUES('native-op','native-op','agent-worker','native-attempt','fixture','card','{}','{}','pending',1,1)").execute(&mut **tx).await.unwrap();
            task_worker_create_tx(&mut tx,"native-card".into(),"native-session".into(),"native-op".into(),NewCard {
                track_id:writer_track.clone().into(),kind:"codex".into(),title:None,sort:None,
                payload:json!({"schemaVersion":1,"harness_profile":"task_worker","worker_provider":"codex","idempotency_key":"native-attempt"}),
            },AgentProvider::Codex,None,writer_repo.card_role_cache()).await.unwrap();
            {
                let mut carrier=crate::db::sqlite::conversation::current_native_fact_tx(&mut tx,"native-card","native-session",true).await.unwrap();
                let started=json!({"threadId":"thread","turnId":"turn","item":{"id":"tool","type":"commandExecution","command":"cargo check","status":"inProgress"}});
                crate::db::sqlite::record_native_item_tx(&mut carrier,"thread",Some("turn"),Some("tool"),Some("commandExecution"),"item/started",&started.to_string(),None).await.unwrap();
                for n in 0..502 {
                    let id=format!("reply-{n}");
                    let params=json!({"threadId":"thread","turnId":"turn","item":{"id":id,"type":"agentMessage","text":format!("body[{n}]")}});
                    crate::db::sqlite::record_native_item_tx(&mut carrier,"thread",Some("turn"),Some(&id),Some("agentMessage"),"item/completed",&params.to_string(),None).await.unwrap();
                }
                let completed=json!({"threadId":"thread","turnId":"turn","item":{"id":"tool","type":"commandExecution","command":"cargo check","status":"completed","aggregatedOutput":"ok"}});
                crate::db::sqlite::record_native_item_tx(&mut carrier,"thread",Some("turn"),Some("tool"),Some("commandExecution"),"item/completed",&completed.to_string(),None).await.unwrap();
            }
            tx.commit().await.unwrap();
        })).await.unwrap();
        let track = repo.track_get(&track).await.unwrap().unwrap();
        let write = crate::state::WriteContext::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
        );
        let view = crate::track_fs_view::TrackFsView::new(repo.as_ref(), &write);
        let content = view
            .cat(&track, "cards/native-card/conversation.md")
            .await
            .unwrap();
        for n in 0..502 {
            assert!(
                content.content.contains(&format!("body[{n}]")),
                "missing native message {n}"
            );
        }
        assert_eq!(content.content.matches("cargo check").count(), 1);
        assert!(!content.content.contains("inProgress"));
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::db::sqlite::{AcpSubmission, AcpSubmissionState, SqlxRepo};
    use serde_json::json;

    async fn worker(provider: AgentProvider) -> (SqlxRepo, String, String) {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let (_, track) = super::super::session_conversation_tests::card(&repo).await;
        let db = repo.db().clone();
        let roles = repo.card_role_cache().clone();
        db.writer().with_writer(|writer| Box::pin(async move {
            let mut tx = writer.begin_immediate().await.unwrap();
            sqlx::query(concat!("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,",
                "target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) ",
                "VALUES('op','op','agent-worker','attempt','fixture','card','{}','{}','pending',1,1)"))
                .execute(&mut **tx).await.unwrap();
            task_worker_create_tx(&mut tx, "worker-card".into(), "worker-session".into(), "op".into(),
                NewCard { track_id: track.into(), kind: "codex".into(), title: None, sort: None,
                    payload: json!({"schemaVersion":1,"harness_profile":"task_worker",
                        "worker_provider":provider,"idempotency_key":"attempt"}) },
                provider.clone(), (provider != AgentProvider::Codex).then(|| "registration".into()),
                &roles).await.unwrap();
            super::super::session_bind_attribution_tx(&mut tx, &"worker-session".into(),
                crate::session_projection_repo::ThreadAttribution {
                    worker_session_id: "worker-session".into(), provider,
                    thread_id: Some("thread".into()), session_id: Some("native".into()), active_turn_id: None,
                }).await.unwrap();
            tx.commit().await.unwrap();
        })).await.unwrap();
        (repo, "worker-card".into(), "worker-session".into())
    }

    #[tokio::test]
    async fn task_worker_acp_receipt_admits_executor() {
        for provider in [
            AgentProvider::OpenCode,
            AgentProvider::Antigravity,
            AgentProvider::Grok,
        ] {
            let (repo, _, worker) = worker(provider).await;
            let receipt = AcpSubmission {
                worker_session_id: worker.clone(),
                client_id: "client".into(),
                turn_id: "turn".into(),
                thread_id: "thread".into(),
                native_session_id: "native".into(),
                input_json: json!({"input":null,"prompt":[],"queue":[]}).to_string(),
                state: AcpSubmissionState::Sending,
                outcome_json: None,
                kept_cleanup: None,
            };
            super::super::acp_submission_prepare(&repo.db().clone(), &receipt, 1, None)
                .await
                .unwrap();
            assert!(
                super::super::acp_submission_get(repo.pool(), &worker, "client")
                    .await
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[tokio::test]
    async fn task_worker_harness_identity_is_server_owned_and_sticky() {
        let (repo, card, _) = worker(AgentProvider::Codex).await;
        let rejected = crate::validation::reject_client_supplied_server_owned_keys(
            &json!({"schemaVersion":1,"harness_profile":"plain_chat"}),
        );
        assert!(matches!(
            rejected,
            Err(crate::TruthError::Core(
                calm_types::error::CoreError::BadRequest(_)
            ))
        ));
        repo.db()
            .writer()
            .with_writer(|writer| {
                Box::pin(async move {
                    let mut tx = writer.begin_immediate().await.unwrap();
                    let updated = super::super::card_update_tx(
                        &mut tx,
                        &card,
                        crate::model::CardPatch {
                            payload: Some(json!({"schemaVersion":1})),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                    assert_eq!(updated.payload["harness_profile"], "task_worker");
                    tx.rollback().await.unwrap();
                })
            })
            .await
            .unwrap();
    }
}
