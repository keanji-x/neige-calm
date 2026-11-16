use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, new_id};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};

struct Rig {
    repo: SqlxRepo,
    store: ManagedSessionTokens,
    card: CardId,
    worker: WorkerSessionId,
}
impl Rig {
    async fn new() -> Self {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "managed tokens".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "managed tokens".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let worker = new_id();
        let w = worker.clone();
        let card = write_in_tx_typed::<_, CalmError, _>(&repo, move |tx| {
            Box::pin(async move {
                let card = card_create_with_id_tx(
                    tx,
                    new_id(),
                    NewCard {
                        track_id: track.id,
                        title: None,
                        kind: "codex".into(),
                        sort: None,
                        payload: serde_json::json!({"planner_provider":"claude"}),
                    },
                    CardRole::Planner,
                    true,
                    &crate::card_role_cache::CardRoleCache::new(),
                )
                .await?;
                session_start_runtime_tx(
                    tx,
                    WorkerSessionInit {
                        id: w,
                        card_id: card.id.to_string(),
                        kind: WorkerSessionKind::SharedPlanner,
                        agent_provider: Some(AgentProvider::Claude),
                        status: WorkerSessionState::Idle,
                        terminal_run_id: None,
                        thread_id: None,
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        spawn_op_id: None,
                        now_ms: crate::model::now_ms(),
                    },
                )
                .await?;
                Ok(card.id)
            })
        })
        .await
        .unwrap();
        Self {
            repo,
            store: Default::default(),
            card,
            worker: WorkerSessionId(worker),
        }
    }
    async fn ensure(&self) -> Result<String> {
        self.store
            .ensure(&self.repo, &self.card, &self.worker)
            .await
    }
    async fn sql(&self, sql: &str) {
        self.repo
            .db()
            .writer()
            .execute(sqlx::query(sql).bind(self.worker.as_str()))
            .await
            .unwrap();
    }
    async fn hash(&self) -> Option<String> {
        self.repo
            .session_get(&self.worker)
            .await
            .unwrap()
            .unwrap()
            .mcp_token_hash
    }
    async fn add_carrier(&self) -> (CardId, WorkerSessionId) {
        let mut row = self.repo.session_get(&self.worker).await.unwrap().unwrap();
        let card = self
            .repo
            .card_create(NewCard {
                track_id: row.track_id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({"planner_provider":"claude"}),
            })
            .await
            .unwrap()
            .id;
        let worker = WorkerSessionId(new_id());
        row.id = worker.clone();
        row.card_id = Some(card.clone());
        row.mcp_token_hash = None;
        let w = worker.clone();
        let c = card.clone();
        write_in_tx_typed::<_, CalmError, _>(&self.repo, move |tx| {
            Box::pin(async move {
                crate::db::sqlite::session_insert_tx(tx, row).await?;
                sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                    .bind(w.as_str())
                    .bind(c.as_str())
                    .execute(&mut **tx)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        (card, worker)
    }
    fn candidate(&self) -> String {
        self.store.0.lock().unwrap()[&self.worker].raw.clone()
    }
}

#[tokio::test]
async fn ensure_active_managed_planner_token_reuses_until_revoked() {
    let rig = Rig::new().await;
    let first = rig.ensure().await.unwrap();
    assert_eq!(first, rig.ensure().await.unwrap());
    let card = rig.card.clone();
    write_in_tx_typed::<_, CalmError, _>(&rig.repo, move |tx| {
        Box::pin(async move {
            crate::db::sqlite::claude_planner_revoke_tx(
                tx,
                crate::db::sqlite::ClaudePlannerScope::Card(card.as_str()),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    assert_eq!(rig.hash().await, None);
    let second = rig.ensure().await.unwrap();
    assert_ne!(first, second);
    assert_eq!(rig.hash().await, Some(hash_token(&second)));
    rig.sql("UPDATE worker_sessions SET mcp_token_hash='other-hash' WHERE id=?1")
        .await;
    let third = rig.ensure().await.unwrap();
    assert_ne!(second, third);
    assert_eq!(rig.hash().await, Some(hash_token(&third)));
    // Losing all plaintext never permits reconstructing or resurrecting an old token.
    let fresh_store = ManagedSessionTokens::default();
    let fourth = fresh_store
        .ensure(&rig.repo, &rig.card, &rig.worker)
        .await
        .unwrap();
    assert_ne!(third, fourth);
}

#[tokio::test]
async fn managed_tokens_concurrent_ensure_is_stable() {
    let rig = Rig::new().await;
    let (a, b, c) = tokio::join!(rig.ensure(), rig.ensure(), rig.ensure());
    let a = a.unwrap();
    assert_eq!(a, b.unwrap());
    assert_eq!(a, c.unwrap());
    assert_eq!(rig.hash().await, Some(hash_token(&a)));
}

#[tokio::test]
async fn managed_tokens_cached_inactive_is_refused_without_writes() {
    let rig = Rig::new().await;
    let old = rig.ensure().await.unwrap();
    rig.sql("UPDATE worker_sessions SET state='exited' WHERE id=?1")
        .await;
    assert!(matches!(rig.ensure().await, Err(CalmError::Conflict(_))));
    assert_eq!(rig.hash().await, Some(hash_token(&old)));
    assert_eq!(rig.candidate(), old);
}

#[tokio::test]
async fn managed_tokens_cached_carrier_and_row_owner_are_refused() {
    let rig = Rig::new().await;
    let old = rig.ensure().await.unwrap();
    rig.sql("UPDATE cards SET session_id=NULL WHERE session_id=?1")
        .await;
    assert!(matches!(rig.ensure().await, Err(CalmError::Conflict(_))));
    assert_eq!(rig.hash().await, Some(hash_token(&old)));
    rig.sql(
        "UPDATE cards SET session_id=?1 WHERE id=(SELECT card_id FROM worker_sessions WHERE id=?1)",
    )
    .await;
    rig.sql("UPDATE worker_sessions SET card_id=NULL WHERE id=?1")
        .await;
    assert!(matches!(rig.ensure().await, Err(CalmError::Conflict(_))));
    assert_eq!(rig.hash().await, Some(hash_token(&old)));
}

#[tokio::test]
async fn managed_tokens_rollback_does_not_publish_or_lose_valid_candidate() {
    let rig = Rig::new().await;
    let old = rig.ensure().await.unwrap();
    // Production ensure cannot participate in this revoke-then-rollback transaction.
    let card = rig.card.clone();
    let result = write_in_tx_typed::<(), CalmError, _>(&rig.repo, move |tx| {
        Box::pin(async move {
            crate::db::sqlite::claude_planner_revoke_tx(
                tx,
                crate::db::sqlite::ClaudePlannerScope::Card(card.as_str()),
            )
            .await?;
            Err(CalmError::Conflict("forced rollback".into()))
        })
    })
    .await;
    assert!(result.is_err());
    assert_eq!(rig.ensure().await.unwrap(), old);
    rig.sql("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
        .await;
    // Card write succeeds, second SQL write aborts; neither hash may escape the transaction.
    rig.sql(
        "CREATE TRIGGER reject_token BEFORE UPDATE OF mcp_token_hash ON worker_sessions
         WHEN NEW.mcp_token_hash IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'reject token'); END",
    )
    .await;
    assert!(rig.ensure().await.is_err());
    assert_eq!(rig.hash().await, None);
    rig.sql("DROP TRIGGER reject_token").await;
    let next = rig.ensure().await.unwrap();
    assert_ne!(old, next);
    assert_eq!(rig.hash().await, Some(hash_token(&next)));
}

#[tokio::test]
async fn managed_tokens_commit_failure_keeps_only_discardable_candidate() {
    let rig = Rig::new().await;
    rig.sql("CREATE TABLE token_commit_failure(worker TEXT REFERENCES worker_sessions(id) DEFERRABLE INITIALLY DEFERRED)").await;
    rig.sql(
        "CREATE TRIGGER fail_token_commit AFTER UPDATE OF mcp_token_hash ON worker_sessions
         BEGIN INSERT INTO token_commit_failure VALUES('missing-worker'); END",
    )
    .await;
    assert!(
        rig.ensure().await.is_err(),
        "commit must fail before raw is returned"
    );
    let uncommitted = rig.candidate();
    assert_eq!(rig.hash().await, None);
    let card_hash: Option<String> =
        sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens WHERE card_id=?1")
            .bind(rig.card.as_str())
            .fetch_optional(rig.repo.db().read_pool())
            .await
            .unwrap();
    assert_eq!(card_hash, None);
    rig.sql("DROP TRIGGER fail_token_commit").await;
    let next = rig.ensure().await.unwrap();
    assert_ne!(uncommitted, next);
    assert_eq!(rig.hash().await, Some(hash_token(&next)));
    assert_eq!(next, rig.ensure().await.unwrap());
}

#[tokio::test]
async fn managed_tokens_prune_keeps_dormant_and_reclaims_invalid_history() {
    let rig = Rig::new().await;
    let dormant = rig.ensure().await.unwrap();
    for n in 0..12 {
        let (card, worker) = rig.add_carrier().await;
        rig.store.ensure(&rig.repo, &card, &worker).await.unwrap();
        assert_eq!(
            rig.store.0.lock().unwrap().len(),
            2,
            "prune bounds invalid history"
        );
        assert_eq!(
            rig.ensure().await.unwrap(),
            dormant,
            "dormant token survives prune"
        );
        let sql = match n % 4 {
            0 => "UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1",
            1 => "UPDATE worker_sessions SET mcp_token_hash='foreign-' || id WHERE id=?1",
            2 => "UPDATE worker_sessions SET state='exited' WHERE id=?1",
            _ => "UPDATE cards SET session_id=NULL WHERE session_id=?1",
        };
        rig.repo
            .db()
            .writer()
            .execute(sqlx::query(sql).bind(worker.as_str()))
            .await
            .unwrap();
    }
    assert_eq!(rig.ensure().await.unwrap(), dormant);
}

#[tokio::test]
async fn managed_tokens_prune_read_error_preserves_valid_entries() {
    let rig = Rig::new().await;
    let dormant = rig.ensure().await.unwrap();
    let (card, worker) = rig.add_carrier().await;
    let other = rig.store.ensure(&rig.repo, &card, &worker).await.unwrap();
    rig.repo
        .db()
        .writer()
        .execute(
            sqlx::query(
                "UPDATE worker_sessions SET handle_state_json='malformed-json' WHERE id=?1",
            )
            .bind(worker.as_str()),
        )
        .await
        .unwrap();
    let (new_card, new_worker) = rig.add_carrier().await;
    assert!(
        rig.store
            .ensure(&rig.repo, &new_card, &new_worker)
            .await
            .is_err()
    );
    assert_eq!(rig.store.0.lock().unwrap().len(), 2);
    assert_eq!(rig.ensure().await.unwrap(), dormant);
    rig.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET handle_state_json=NULL WHERE id=?1")
                .bind(worker.as_str()),
        )
        .await
        .unwrap();
    assert_eq!(
        rig.store.ensure(&rig.repo, &card, &worker).await.unwrap(),
        other
    );
    rig.store
        .ensure(&rig.repo, &new_card, &new_worker)
        .await
        .unwrap();
    assert_eq!(rig.store.0.lock().unwrap().len(), 3);
}
