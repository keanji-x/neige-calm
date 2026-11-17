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
