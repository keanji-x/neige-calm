//! What a fresh start would lose, on the real schema (#2184).

use super::session_conversation::card_conversation;
use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{SqlxRepo, area_create_tx, card_create_tx, track_create_tx};
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
use crate::session_projection_repo::CardConversation;
use serde_json::json;

pub(super) async fn card(repo: &SqlxRepo) -> (String, String) {
    let mut tx = repo.db().writer_pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        calm_types::organization::LEGACY_OWNER_USER_ID,
        NewArea {
            name: "history".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "history".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let card = card_create_tx(
        &mut tx,
        NewCard {
            track_id: track.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
            title: None,
        },
        &CardRoleCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), track.id.to_string())
}

async fn conversation(repo: &SqlxRepo, card_id: &str) -> CardConversation {
    card_conversation(repo.pool(), card_id).await.unwrap()
}

#[tokio::test]
async fn only_completed_failed_starts_or_nothing_at_all_leave_no_conversation() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card_id, _) = card(&repo).await;
    let (other_card, _) = card(&repo).await;
    let (transcript_card, transcript_track) = card(&repo).await;
    sqlx::query(
        "INSERT INTO harness_items (worker_session_id, card_id, track_id, thread_id, method, \
           params, created_at_ms) VALUES ('gone', ?1, ?2, 't', 'item/completed', '{}', 0)",
    )
    .bind(&transcript_card)
    .bind(&transcript_track)
    .execute(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        conversation(&repo, &transcript_card).await,
        CardConversation::ThreadToPreserve,
        "a transcript without any row is a conversation"
    );
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::NeverStarted
    );

    sqlx::query(
        "INSERT INTO operations (id, operation_key, kind, payload_hash, target_type, \
           target_json, payload_json, phase, created_at_ms, updated_at_ms) \
         VALUES ('op', 'op', 'planner-harness-start', 'h', 'track', '{}', ?1, 'pending', 0, 0)",
    )
    .bind(json!({ "spec_card_id": card_id }).to_string())
    .execute(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::StartInFlight
    );
    assert_eq!(
        conversation(&repo, &other_card).await,
        CardConversation::NeverStarted,
        "another card's start is not this card's"
    );
    for finished in ["succeeded", "failed", "stuck"] {
        sqlx::query("UPDATE operations SET phase = ?1 WHERE id = 'op'")
            .bind(finished)
            .execute(repo.pool())
            .await
            .unwrap();
        assert_eq!(
            conversation(&repo, &card_id).await,
            CardConversation::NeverStarted,
            "a {finished} start is not in flight"
        );
    }
    sqlx::query("UPDATE operations SET phase = 'pending' WHERE id = 'op'")
        .execute(repo.pool())
        .await
        .unwrap();

    sqlx::query("UPDATE operations SET phase = 'failed' WHERE id = 'op'")
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit::shared_planner(
            "row".into(),
            card_id.clone(),
            crate::session_projection_repo::AgentProvider::Codex,
            calm_types::worker::WorkerSessionState::Idle,
            None,
            json!({ "last_thread_id": null }),
            0,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let shape = |sql: &'static str| {
        let pool = repo.pool().clone();
        async move { sqlx::query(sql).execute(&pool).await.unwrap() }
    };
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::ThreadToPreserve,
        "a live row is a conversation even before its thread exists"
    );
    // What a failed start's compensation leaves behind.
    shape("UPDATE worker_sessions SET state = 'failed', completed_at_ms = 1 WHERE id = 'row'")
        .await;
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::OnlyFailedStarts,
        "a failed start that never got a thread preserves nothing"
    );
    // A failed start may have bound a thread before it failed (a Claude Planner mints its id with
    // no RPC); with no transcript, nothing ran on it (#2212).
    for (threaded, why) in [
        (
            "UPDATE worker_sessions SET thread_id = 'thread-1', handle_state_json = NULL \
             WHERE id = 'row'",
            "a thread on the row",
        ),
        (
            "UPDATE worker_sessions SET thread_id = NULL, \
               handle_state_json = '{\"last_thread_id\":\"thread-1\"}' WHERE id = 'row'",
            "a thread only in the snapshot",
        ),
        (
            "UPDATE worker_sessions SET thread_id = NULL, handle_state_json = 'not json' \
             WHERE id = 'row'",
            "a snapshot that cannot be read",
        ),
    ] {
        shape(threaded).await;
        assert_eq!(
            conversation(&repo, &card_id).await,
            CardConversation::OnlyFailedStarts,
            "a completed failed start with {why} and no transcript preserves nothing"
        );
    }
    // The same completed failed row on the card that has a transcript is a conversation.
    let mut tx = repo.pool().begin().await.unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit::shared_planner(
            "transcript-row".into(),
            transcript_card.clone(),
            crate::session_projection_repo::AgentProvider::Codex,
            calm_types::worker::WorkerSessionState::Failed,
            Some("t".into()),
            json!({}),
            0,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    shape("UPDATE worker_sessions SET completed_at_ms = 1 WHERE id = 'transcript-row'").await;
    assert_eq!(
        conversation(&repo, &transcript_card).await,
        CardConversation::ThreadToPreserve,
        "a completed failed row with a thread and a transcript is a conversation"
    );
    shape("UPDATE worker_sessions SET thread_id = NULL, handle_state_json = '{}' WHERE id = 'row'")
        .await;
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::OnlyFailedStarts
    );
    for (holding, restore, why) in [
        (
            "UPDATE worker_sessions SET completed_at_ms = NULL WHERE id = 'row'",
            "UPDATE worker_sessions SET completed_at_ms = 1 WHERE id = 'row'",
            "a failed row not completed (recoverable)",
        ),
        (
            "UPDATE worker_sessions SET state = 'exited' WHERE id = 'row'",
            "UPDATE worker_sessions SET state = 'failed' WHERE id = 'row'",
            "a retired row",
        ),
    ] {
        shape(holding).await;
        assert_eq!(
            conversation(&repo, &card_id).await,
            CardConversation::ThreadToPreserve,
            "{why} is a conversation"
        );
        shape(restore).await;
        assert_eq!(
            conversation(&repo, &card_id).await,
            CardConversation::OnlyFailedStarts,
            "restored after: {why}"
        );
    }
}

#[tokio::test]
async fn assistant_provider_persistence_managed_identity_never_inherits_token_or_repoints_root() {
    use crate::db::sqlite::{
        card_mcp_token_set_tx, session_start_runtime_tx, session_supersede_and_start_tx,
    };
    use crate::session_projection_repo::{AgentProvider, WorkerSessionInit};
    use calm_types::worker::{SessionMode, WorkerContract, WorkerSessionId, WorkerSessionState};
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card_id, track_id) = card(&repo).await;
    let planner = "planner-card".to_owned();
    for provider in [
        AgentProvider::Codex,
        AgentProvider::Claude,
        AgentProvider::OpenCode,
    ] {
        let mut tx = repo.pool().begin().await.unwrap();
        sqlx::query("UPDATE cards SET role='assistant',payload=?2 WHERE id=?1")
            .bind(&card_id).bind(json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":provider}).to_string())
            .execute(&mut *tx).await.unwrap();
        crate::db::sqlite::card_create_with_id_tx(&mut tx, planner.clone(), NewCard {
            track_id:track_id.clone().into(),title:None,kind:"codex".into(),sort:None,
            payload:json!({"schemaVersion":1,"planner_harness":true,"planner_provider":"codex"}),
        },crate::model::CardRole::Planner,false,repo.card_role_cache()).await.unwrap();
        // Seed the Track's Planner root before the Assistant start under test.
        let root = format!("root-{}", provider.wire_name());
        crate::db::sqlite::session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::shared_planner(
                root.clone(),
                planner.clone(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                None,
                json!({}),
                10,
            ),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE tracks SET root_session_id=?2 WHERE id=?1")
            .bind(&track_id)
            .bind(&root)
            .execute(&mut *tx)
            .await
            .unwrap();
        card_mcp_token_set_tx(&mut tx, &card_id, "predecessor-hash")
            .await
            .unwrap();
        let id = format!("assistant-{}", provider.wire_name());
        let init = WorkerSessionInit::managed_assistant(
            id.clone(),
            card_id.clone(),
            provider.clone(),
            WorkerSessionState::Starting,
            None,
            json!({"pending_queue":[]}),
            20,
        );
        if let Some(old) =
            crate::db::sqlite::session_projection_active_for_card_tx(&mut tx, &card_id)
                .await
                .unwrap()
        {
            session_supersede_and_start_tx(&mut tx, &old.id, init)
                .await
                .unwrap();
        } else {
            session_start_runtime_tx(&mut tx, init).await.unwrap();
        }
        let live = crate::db::sqlite::session_get_tx(&mut tx, &WorkerSessionId(id.clone()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.contract, WorkerContract::Assistant);
        assert_eq!(live.mode, SessionMode::Resumable);
        assert_eq!(live.provider.as_db_str(), provider.wire_name());
        assert!(
            live.mcp_token_hash.is_none(),
            "a managed Assistant must not inherit card credentials"
        );
        let current_root: Option<String> =
            sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
                .bind(&track_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(
            current_root.as_deref(),
            Some(root.as_str()),
            "Assistant must never become root"
        );
        let projected = crate::db::sqlite::session_projection_by_id_tx(&mut tx, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            projected.kind,
            crate::session_projection_repo::WorkerSessionKind::ManagedAssistant
        );
        assert_eq!(projected.agent_provider, Some(provider.clone()));
        crate::db::sqlite::session_mcp_token_set_if_active_tx(&mut tx, &id, "current-hash")
            .await
            .unwrap();
        let successor = format!("{id}-successor");
        session_supersede_and_start_tx(
            &mut tx,
            &id,
            WorkerSessionInit::managed_assistant(
                successor.clone(),
                card_id.clone(),
                provider,
                WorkerSessionState::Starting,
                None,
                json!({}),
                30,
            ),
        )
        .await
        .unwrap();
        let old = crate::db::sqlite::session_get_tx(&mut tx, &WorkerSessionId(id))
            .await
            .unwrap()
            .unwrap();
        assert!(
            old.mcp_token_hash.is_none(),
            "supersede revokes the old managed hash"
        );
        let next = crate::db::sqlite::session_get_tx(&mut tx, &WorkerSessionId(successor.clone()))
            .await
            .unwrap()
            .unwrap();
        assert!(next.mcp_token_hash.is_none(), "replacement never inherits");
        crate::db::sqlite::session_mcp_token_set_if_active_tx(
            &mut tx,
            &successor,
            "successor-hash",
        )
        .await
        .unwrap();
        crate::db::sqlite::session_fail_if_active_runtime_tx(&mut tx, &successor)
            .await
            .unwrap();
        let failed = crate::db::sqlite::session_get_tx(&mut tx, &WorkerSessionId(successor))
            .await
            .unwrap()
            .unwrap();
        assert!(
            failed.mcp_token_hash.is_none(),
            "terminal failure revokes the managed hash"
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn assistant_provider_managed_actor_uses_live_binding_and_has_no_planner_authority() {
    use crate::db::sqlite::{append_decision_event_in_tx, session_start_runtime_tx};
    use crate::event::{EditAuthor, Event, EventScope};
    use crate::ids::ActorId;
    use crate::session_projection_repo::{AgentProvider, WorkerSessionInit};
    use calm_types::worker::WorkerSessionState;
    for provider in [
        AgentProvider::Codex,
        AgentProvider::Claude,
        AgentProvider::OpenCode,
    ] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let (assistant, track) = card(&repo).await;
        let area: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
            .bind(&track)
            .fetch_one(repo.pool())
            .await
            .unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        sqlx::query("UPDATE cards SET role='assistant',payload=?2 WHERE id=?1").bind(&assistant)
        .bind(json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":provider}).to_string()).execute(&mut *tx).await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::managed_assistant(
                "acp-assistant".into(),
                assistant.clone(),
                provider.clone(),
                WorkerSessionState::Idle,
                None,
                json!({}),
                1,
            ),
        )
        .await
        .unwrap();
        let report = "report";
        crate::db::sqlite::card_create_with_id_tx(
            &mut tx,
            report.into(),
            NewCard {
                track_id: track.clone().into(),
                title: None,
                kind: "track-report".into(),
                sort: None,
                payload: serde_json::to_value(crate::track_report::TrackReportPayload::initial())
                    .unwrap(),
            },
            crate::model::CardRole::ReportCard,
            false,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        let scope = EventScope::Card {
            card: report.into(),
            track: track.clone().into(),
            area: area.into(),
        };
        let event = Event::TrackReportEdited {
            track_id: track.clone().into(),
            card_id: report.into(),
            author: EditAuthor::Assistant,
            author_plugin_id: None,
            edit_id: "edit".into(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: String::new(),
            body_after: "prose".into(),
            agent_message: None,
        };
        let actor = ActorId::AiManagedSession("acp-assistant".into());
        append_decision_event_in_tx(&mut tx, &actor, &scope, None, &event)
            .await
            .unwrap();
        assert!(
            append_decision_event_in_tx(
                &mut tx,
                &ActorId::AiPlannerSession("acp-assistant".into()),
                &scope,
                None,
                &event
            )
            .await
            .is_err()
        );
        for borrowed in [
            ActorId::AiCodexSession("acp-assistant".into()),
            ActorId::AiClaudeSession("acp-assistant".into()),
        ] {
            assert!(
                append_decision_event_in_tx(&mut tx, &borrowed, &scope, None, &event)
                    .await
                    .is_err(),
                "{borrowed:?}"
            );
        }
        let other_provider = if provider == AgentProvider::Claude {
            "codex"
        } else {
            "claude"
        };
        let mismatch = format!(
            "UPDATE cards SET payload=json_set(payload,'$.assistant_provider','{other_provider}') WHERE id=?1"
        );
        let cross_track = EventScope::Card {
            card: report.into(),
            track: "another-track".into(),
            area: "another-area".into(),
        };
        assert!(
            append_decision_event_in_tx(&mut tx, &actor, &cross_track, None, &event)
                .await
                .is_err()
        );
        for damage in [
            "UPDATE cards SET session_id=NULL WHERE id=?1",
            mismatch.as_str(),
            "UPDATE worker_sessions SET card_id=NULL WHERE id=(SELECT session_id FROM cards WHERE id=?1)",
            "UPDATE worker_sessions SET contract='planner' WHERE id=(SELECT session_id FROM cards WHERE id=?1)",
            "UPDATE cards SET role='planner' WHERE id=?1",
        ] {
            sqlx::query("SAVEPOINT invalid_identity")
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query(damage)
                .bind(&assistant)
                .execute(&mut *tx)
                .await
                .unwrap();
            assert!(
                append_decision_event_in_tx(&mut tx, &actor, &scope, None, &event)
                    .await
                    .is_err(),
                "{damage}"
            );
            sqlx::query("ROLLBACK TO invalid_identity")
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("RELEASE invalid_identity")
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id='acp-assistant'")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(
            append_decision_event_in_tx(&mut tx, &actor, &scope, None, &event)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn assistant_provider_projection_preserves_shared_codex_without_mislabeling_native_backends()
{
    use crate::db::RepoRead;
    use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    for provider in [
        AgentProvider::Codex,
        AgentProvider::Claude,
        AgentProvider::OpenCode,
    ] {
        let (_, track) = card(&repo).await;
        let id = format!("assistant-{}", provider.wire_name());
        let thread = format!("thread-{}", provider.wire_name());
        let mut tx = repo.pool().begin().await.unwrap();
        crate::db::sqlite::card_create_with_id_tx(&mut tx,id.clone(),NewCard {
            track_id:track.into(),title:None,kind:"codex".into(),sort:None,
            payload:json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":provider}),
        },crate::model::CardRole::Assistant,false,repo.card_role_cache()).await.unwrap();
        crate::db::sqlite::session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::managed_assistant(
                format!("session-{id}"),
                id.clone(),
                provider.clone(),
                WorkerSessionState::Running,
                Some(thread.clone()),
                json!({}),
                1,
            ),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let mut card = repo.card_get(&id).await.unwrap().unwrap();
        crate::session_projection_lookup::project_runtime_into_card_payload(&repo, &mut card)
            .await
            .unwrap();
        let projection=crate::session_projection_repo::WorkerSessionProjectionRepo::session_projection_active_for_card(&repo,&id).await.unwrap().unwrap();
        assert_eq!(
            card.runtime.as_ref().unwrap().provider,
            Some(provider.clone())
        );
        let codex = provider == AgentProvider::Codex;
        assert_eq!(
            crate::session_projection_lookup::card_is_shared_planner(&card, Some(&projection)),
            codex
        );
        assert_eq!(
            card.payload
                .get("codex_thread_id")
                .and_then(serde_json::Value::as_str),
            codex.then_some(thread.as_str())
        );
    }
}

#[tokio::test]
async fn assistant_provider_storage_rejects_terminal_and_nonresumable_managed_identities() {
    use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
    use calm_types::worker::{SessionMode, WorkerProviderKind, WorkerSessionId};
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (_, track) = card(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let card = "storage-assistant".to_owned();
    crate::db::sqlite::card_create_with_id_tx(&mut tx,card.clone(),NewCard {
        track_id:track.into(),title:None,kind:"codex".into(),sort:None,
        payload:json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":"codex"}),
    },crate::model::CardRole::Assistant,false,repo.card_role_cache()).await.unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit::managed_assistant(
            "valid".into(),
            card,
            AgentProvider::Codex,
            WorkerSessionState::Idle,
            None,
            json!({}),
            1,
        ),
    )
    .await
    .unwrap();
    let valid = crate::db::sqlite::session_get_tx(&mut tx, &WorkerSessionId("valid".into()))
        .await
        .unwrap()
        .unwrap();
    for (provider, mode) in [
        (WorkerProviderKind::Terminal, SessionMode::Resumable),
        (WorkerProviderKind::Codex, SessionMode::Ephemeral),
        (WorkerProviderKind::Claude, SessionMode::Ephemeral),
        (WorkerProviderKind::OpenCode, SessionMode::Ephemeral),
    ] {
        let mut invalid = valid.clone();
        invalid.id = format!("invalid-{provider:?}").into();
        invalid.state = WorkerSessionState::Superseded;
        invalid.provider = provider;
        invalid.mode = mode;
        assert!(
            crate::db::sqlite::session_insert_tx(&mut tx, invalid)
                .await
                .is_err(),
            "{provider:?} {mode:?}"
        );
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn assistant_provider_schema_is_extensible_but_unknown_typed_identity_is_rejected() {
    use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
    use calm_types::worker::WorkerSessionId;
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (id, _) = card(&repo).await;
    let mut tx = repo.pool().begin().await.unwrap();
    sqlx::query("UPDATE cards SET role='assistant',payload=?2 WHERE id=?1")
        .bind(&id)
        .bind(
            json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":"codex"})
                .to_string(),
        )
        .execute(&mut *tx)
        .await
        .unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit::managed_assistant(
            "future-session".into(),
            id,
            AgentProvider::Codex,
            WorkerSessionState::Idle,
            None,
            json!({}),
            1,
        ),
    )
    .await
    .unwrap();
    // A later owner enum/decoder can introduce a provider without rebuilding the generic
    // Assistant schema. Until then typed reads fail closed, never guessing Codex.
    sqlx::query("UPDATE worker_sessions SET provider='future-agent' WHERE id='future-session'")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        crate::db::sqlite::session_get_tx(&mut tx, &WorkerSessionId("future-session".into()))
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}
