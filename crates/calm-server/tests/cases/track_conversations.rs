//! `POST`/`GET /api/tracks/{track_id}/conversations`: idempotency, derived-id refusal, and the list predicate.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, card_with_codex_create_tx};
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewArea, RequestTheme};
use calm_server::operation::OperationKey;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    area_id: String,
    repo: Arc<SqlxRepo>,
    card_role_cache: CardRoleCache,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "track-conversations".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    repo.area_folder_create(area.id.as_str(), "/workspace")
        .await
        .unwrap();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    repo.seed_track_area_cache(&tracks).await.unwrap();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(roles.clone(), tracks.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles.clone()),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"))
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo_dyn, None,
    ));
    let app = routes::router()
        .layer(Extension(Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    Boot {
        app,
        state,
        area_id: area.id.to_string(),
        repo,
        card_role_cache: roles,
        _tmp: tmp,
    }
}

impl Boot {
    /// A real, user-visible track through `POST /api/tracks`, so it carries the planner card, the report
    /// card and the workspace a production track has.
    async fn create_track(&self, title: &str) -> String {
        let (status, body) = self
            .request(
                "POST",
                "/api/tracks",
                None,
                Some(json!({
                    "planner_provider": "codex",
                    "area_id": self.area_id,
                    "title": title,
                    "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "body={body}");
        body["id"].as_str().unwrap().to_string()
    }

    async fn request(
        &self,
        method: &str,
        uri: &str,
        idempotency_key: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn create_conversation(
        &self,
        track_id: &str,
        idempotency_key: &str,
        text: &str,
    ) -> (StatusCode, Value) {
        self.request(
            "POST",
            &format!("/api/tracks/{track_id}/conversations"),
            Some(idempotency_key),
            Some(json!({ "text": text })),
        )
        .await
    }

    async fn list_conversations(&self, track_id: &str) -> (StatusCode, Value) {
        self.request(
            "GET",
            &format!("/api/tracks/{track_id}/conversations"),
            None,
            None,
        )
        .await
    }

    /// A genuine codex worker card, minted through the production transaction rather than an INSERT.
    async fn mint_codex_worker_card(&self, track_id: &str) -> String {
        let card_id = calm_server::model::new_id();
        let mut tx = self.repo.pool().begin().await.unwrap();
        card_with_codex_create_tx(
            &mut tx,
            card_id.clone(),
            &calm_server::model::new_id(),
            None,
            calm_server::ids::TrackId::from(track_id.to_string()),
            Some("worker".into()),
            None,
            "/workspace".into(),
            json!({}),
            None,
            None,
            None,
            CardRole::Worker,
            true,
            &self.card_role_cache,
            RequestTheme::default_dark(),
        )
        .await
        .expect("mint codex worker card");
        tx.commit().await.unwrap();
        card_id
    }

    /// A codex card carrying only HALF of the `(role, marker)` pair the list predicate requires — the shape
    /// a predicate that dropped one conjunct would leak.
    async fn mint_half_marked_card(&self, track_id: &str, role: CardRole, marker: &str) -> String {
        let card_id = calm_server::model::new_id();
        let mut tx = self.repo.pool().begin().await.unwrap();
        card_create_with_id_tx(
            &mut tx,
            card_id.clone(),
            calm_server::model::NewCard {
                track_id: calm_server::ids::TrackId::from(track_id.to_string()),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1, "harness_profile": marker}),
            },
            role,
            true,
            &self.card_role_cache,
        )
        .await
        .expect("mint half-marked card");
        tx.commit().await.unwrap();
        card_id
    }

    async fn scalar(&self, sql: &str, bind: &str) -> i64 {
        sqlx::query_scalar(sql)
            .bind(bind)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn card_role(&self, card_id: &str) -> String {
        sqlx::query_scalar("SELECT role FROM cards WHERE id = ?1")
            .bind(card_id)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn card_marker(&self, card_id: &str) -> Option<String> {
        sqlx::query_scalar(
            "SELECT json_extract(payload, '$.harness_profile') FROM cards WHERE id = ?1",
        )
        .bind(card_id)
        .fetch_one(self.repo.pool())
        .await
        .unwrap()
    }

    /// How many copies of `needle` the assistant harness has actually been handed, counted at the harness
    /// (turns started plus observations still queued), never at `harness.user_message.enqueued`, which only says a delivery was attempted.
    async fn copies_in_harness(&self, needle: &str, want: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut seen = self
                .state
                .shared_codex_appserver
                .started_turns_for_test()
                .iter()
                .map(|(_, items)| {
                    items
                        .iter()
                        .map(|item| {
                            serde_json::to_string(item)
                                .map(|s| s.matches(needle).count())
                                .unwrap_or(0)
                        })
                        .sum::<usize>()
                })
                .sum::<usize>();
            let worker_session_ids: Vec<String> =
                sqlx::query_scalar("SELECT id FROM worker_sessions")
                    .fetch_all(self.repo.pool())
                    .await
                    .unwrap();
            for id in worker_session_ids {
                if let Some(handle) = self.state.harness.get(&id) {
                    for obs in handle.pending_queue_for_test().await {
                        seen += serde_json::to_string(&obs)
                            .map(|s| s.matches(needle).count())
                            .unwrap_or(0);
                    }
                }
            }
            if seen >= want || std::time::Instant::now() >= deadline {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    async fn shutdown_harnesses(&self) {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
            .fetch_all(self.repo.pool())
            .await
            .unwrap();
        for id in ids {
            if let Some(handle) = self.state.harness.remove(&id) {
                let _ = handle.shutdown().await;
            }
        }
    }
}

/// Asserted on the persisted row rather than the response body: the row is what the authorization
/// gate, the list predicate and the CARDS panel all read.
#[tokio::test]
async fn the_first_message_mints_an_assistant_card_with_its_own_marker_and_mcp_token() {
    let b = boot().await;
    let track_id = b.create_track("assistant-mint").await;

    let (status, body) = b
        .create_conversation(&track_id, "idem-mint", "hello assistant")
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let card_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["kind"], "track-assistant");
    assert_eq!(body["trackId"], track_id);
    assert_eq!(body["title"], Value::Null);

    assert_eq!(
        b.card_role(&card_id).await,
        "assistant",
        "the card's role is what role_gate reads; minting a worker here would \
         silently give the conversation the plain-chat surface"
    );
    assert_eq!(
        b.card_marker(&card_id).await.as_deref(),
        Some("assistant"),
        "the persisted marker is what the list predicate and the CARDS panel read"
    );
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM card_mcp_tokens WHERE card_id = ?1",
            &card_id
        )
        .await,
        1,
        "an assistant without an MCP token cannot reach the block channel at all"
    );
    // A plain chat is `ThreadConfig::NoMcp`; the assistant must NOT be.
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 AND mcp_token_hash IS NOT NULL",
            &card_id
        )
        .await,
        1,
        "the session must mirror the card token"
    );
    // `SharedPlanner` would map to `WorkerContract::Planner` and hand the
    // assistant the track's root session pointer.
    let contract: String =
        sqlx::query_scalar("SELECT contract FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        contract, "executor",
        "an assistant session must never be a Planner: Planner sessions take \
         over tracks.root_session_id"
    );
    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    let assistant_session: String =
        sqlx::query_scalar("SELECT id FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_ne!(
        root.as_deref(),
        Some(assistant_session.as_str()),
        "the assistant displaced the planner card as the track's root session"
    );
    b.shutdown_harnesses().await;
}

/// One `Idempotency-Key`, one conversation, however many retries.
#[tokio::test]
async fn retrying_one_idempotency_key_lands_on_the_same_conversation() {
    let b = boot().await;
    let track_id = b.create_track("assistant-retry").await;

    let (first_status, first) = b
        .create_conversation(&track_id, "idem-retry", "first message")
        .await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first}");
    let (second_status, second) = b
        .create_conversation(&track_id, "idem-retry", "first message")
        .await;
    assert_eq!(second_status, StatusCode::CREATED, "body={second}");
    assert_eq!(first["id"], second["id"], "a retry minted a second card");

    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM cards WHERE track_id = ?1 AND role = 'assistant'",
            &track_id
        )
        .await,
        1
    );
    // A different key on the same track is a different conversation — the
    // derivation must not collapse distinct drafts either.
    let (status, other) = b
        .create_conversation(&track_id, "idem-other", "another message")
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={other}");
    assert_ne!(other["id"], first["id"]);
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM cards WHERE track_id = ?1 AND role = 'assistant'",
            &track_id
        )
        .await,
        2
    );
    b.shutdown_harnesses().await;
}

/// `prepare_tx` writes `harness.user_message.enqueued` in the mint transaction, `AppServerInteract` can
/// still fail afterwards and compensation deletes the card, so the audit row survives about a message that never reached an agent.
#[tokio::test]
async fn a_retry_after_a_failed_attempt_still_delivers_the_message() {
    const NEEDLE: &str = "do not lose this sentence";
    let b = boot().await;
    let track_id = b.create_track("assistant-redeliver").await;
    let card_id = calm_server::conversation_keys::derive_track_conversation_card_id_for_test(
        &track_id,
        "idem-redeliver",
    );

    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = b
        .create_conversation(&track_id, "idem-redeliver", NEEDLE)
        .await;
    assert!(
        status.is_server_error(),
        "a create whose thread/start failed must not answer 2xx: status={status} body={body}"
    );

    // Premise 1 — nothing was handed to any agent on the failed attempt.
    assert_eq!(
        b.copies_in_harness(NEEDLE, 1).await,
        0,
        "premise: no thread ever started, so no agent may hold the sentence"
    );
    // Premise 2 — the compensation really did take the card back out.
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM cards WHERE track_id = ?1 AND role = 'assistant'",
            &track_id
        )
        .await,
        0,
        "premise: `delete_card` runs on this path, so the retry re-mints rather \
         than replaying"
    );
    // Premise 3 — and the evidence row survived it. This is the trap; without
    // it the case below is green for the wrong reason.
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued' \
               AND scope_card = ?1",
            &card_id
        )
        .await,
        1,
        "premise: the failed attempt left an enqueued row behind on the very \
         card id the retry re-derives"
    );

    // The retry: same key, same text.
    let (status, retried) = b
        .create_conversation(&track_id, "idem-redeliver", NEEDLE)
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={retried}");
    assert_eq!(
        retried["id"].as_str(),
        Some(card_id.as_str()),
        "the retry must land on the card id the key derives, not on a new one"
    );
    // Waits for a *second* copy it must never see, so this catches a double
    // delivery as well as a dropped one.
    assert_eq!(
        b.copies_in_harness(NEEDLE, 2).await,
        1,
        "the retry must deliver the sentence exactly once; reading the stale \
         `harness.user_message.enqueued` row as a delivered-marker gives 0"
    );
    b.shutdown_harnesses().await;
}

/// Driven through `operation_runtime.submit`, not through the route, because the route always derives
/// a correct id.
#[tokio::test]
async fn a_planner_card_id_the_adapter_did_not_derive_is_refused() {
    use calm_server::operation::planner_harness_start_adapter::{
        HarnessProfile, LazyMintCardSeed, PlannerHarnessStartOperationPayload,
    };

    let b = boot().await;
    let track_id = b.create_track("assistant-forgery").await;
    let other_track_id = b.create_track("assistant-forgery-other").await;
    let track = b.repo.track_get(&track_id).await.unwrap().unwrap();

    let submit = |card_id: String, key: Option<String>, target_track: String| {
        let state = b.state.clone();
        let cwd = track.workspace.path.clone();
        async move {
            let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
                actor: calm_server::ids::ActorId::User,
                track_id: target_track,
                planner_card_id: card_id.into(),
                report_card_id: None,
                sort: None,
                cwd,
                goal: None,
                reset_harness_items: false,
                force_new_thread: true,
                profile: HarnessProfile::Assistant,
                create_card: Some(LazyMintCardSeed {
                    title: None,
                    sort: None,
                    idempotency_key: key,
                    model: None,
                    reasoning_effort: None,
                }),
                opening_briefing: None,
                first_message: None,
                create_request_sha256: None,
            })
            .unwrap();
            state
                .operation_runtime
                .submit(
                    "planner-harness-start",
                    OperationKey {
                        operation_key: calm_server::model::new_id(),
                        idempotency_key: None,
                        payload_hash: calm_server::model::new_id(),
                    },
                    payload,
                )
                .await
                .err()
                .map(|error| error.to_string())
        }
    };

    // 1. A conjured id under a real key.
    let conjured = submit(
        "conv-deadbeefdeadbeefdeadbeefdeadbeef".into(),
        Some("idem-forged".into()),
        track_id.clone(),
    )
    .await
    .expect("a conjured card id must be refused");
    assert!(
        conjured.contains("is not the conversation id derived for track"),
        "rejected for the wrong reason: {conjured}"
    );

    // 2. A correctly derived id, presented with a DIFFERENT key.
    let real_id = calm_server::conversation_keys::derive_track_conversation_card_id_for_test(
        &track_id,
        "idem-real",
    );
    let mismatched = submit(
        real_id.clone(),
        Some("idem-different".into()),
        track_id.clone(),
    )
    .await
    .expect("an id derived under another key must be refused");
    assert!(
        mismatched.contains("is not the conversation id derived for track"),
        "rejected for the wrong reason: {mismatched}"
    );

    // 3. An id derived for ANOTHER track, aimed at this one.
    let foreign_id = calm_server::conversation_keys::derive_track_conversation_card_id_for_test(
        &other_track_id,
        "idem-real",
    );
    let foreign = submit(foreign_id, Some("idem-real".into()), track_id.clone())
        .await
        .expect("an id derived for another track must be refused");
    assert!(
        foreign.contains("is not the conversation id derived for track"),
        "rejected for the wrong reason: {foreign}"
    );

    // 4. No key at all fails closed rather than skipping the check.
    let keyless = submit(real_id.clone(), None, track_id.clone())
        .await
        .expect("a mint with no idempotency key must be refused");
    assert!(
        keyless.contains("without the idempotency key"),
        "rejected for the wrong reason: {keyless}"
    );

    // The positive control: the id the adapter itself derives is accepted.
    assert!(
        submit(real_id, Some("idem-real".into()), track_id.clone())
            .await
            .is_none(),
        "the correctly derived id must be accepted"
    );
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM cards WHERE track_id = ?1 AND role = 'assistant'",
            &track_id
        )
        .await,
        1,
        "exactly the accepted mint reached the database"
    );
    b.shutdown_harnesses().await;
}

/// The track is deliberately crowded (planner card, report card, a real codex worker card, two
/// half-marked decoys): the predicate is a CONJUNCTION of role and marker.
#[tokio::test]
async fn the_list_returns_assistant_conversations_and_nothing_else() {
    let b = boot().await;
    let track_id = b.create_track("assistant-list").await;
    let worker_card = b.mint_codex_worker_card(&track_id).await;
    let role_only_card = b
        .mint_half_marked_card(&track_id, CardRole::Assistant, "plain_chat")
        .await;
    let marker_only_card = b
        .mint_half_marked_card(&track_id, CardRole::Worker, "assistant")
        .await;

    // The fixture is only meaningful if the decoys really are there.
    let roles: Vec<String> = sqlx::query_scalar("SELECT role FROM cards WHERE track_id = ?1")
        .bind(&track_id)
        .fetch_all(b.repo.pool())
        .await
        .unwrap();
    assert!(roles.contains(&"planner".to_string()), "roles={roles:?}");
    assert!(roles.contains(&"reportcard".to_string()), "roles={roles:?}");
    assert!(roles.contains(&"worker".to_string()), "roles={roles:?}");
    assert_eq!(
        b.card_role(&worker_card).await,
        "worker",
        "the worker decoy must be a real codex worker card"
    );
    // The half-marked decoys really are half-marked.
    assert_eq!(b.card_role(&role_only_card).await, "assistant");
    assert_eq!(
        b.card_marker(&role_only_card).await.as_deref(),
        Some("plain_chat")
    );
    assert_eq!(b.card_role(&marker_only_card).await, "worker");
    assert_eq!(
        b.card_marker(&marker_only_card).await.as_deref(),
        Some("assistant")
    );

    let (status, empty) = b.list_conversations(&track_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        empty.as_array().unwrap().len(),
        0,
        "a track with a planner card, a report card and a worker has no \
         conversations until one is created; got {empty}"
    );

    let (status, first) = b.create_conversation(&track_id, "idem-a", "first").await;
    assert_eq!(status, StatusCode::CREATED, "body={first}");
    let (status, second) = b.create_conversation(&track_id, "idem-b", "second").await;
    assert_eq!(status, StatusCode::CREATED, "body={second}");

    let (status, rows) = b.list_conversations(&track_id).await;
    assert_eq!(status, StatusCode::OK);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "rows={rows:?}");
    let mut listed: Vec<&str> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    listed.sort_unstable();
    let mut expected = vec![
        first["id"].as_str().unwrap(),
        second["id"].as_str().unwrap(),
    ];
    expected.sort_unstable();
    assert_eq!(listed, expected);
    assert!(
        rows.iter().all(|row| row["kind"] == "track-assistant"),
        "rows={rows:?}"
    );
    assert!(
        !listed.contains(&worker_card.as_str()),
        "the codex worker card leaked into the conversation list"
    );
    assert!(
        !listed.contains(&role_only_card.as_str()),
        "a card with the assistant role but the plain-chat marker leaked in — \
         the marker conjunct is not being applied"
    );
    assert!(
        !listed.contains(&marker_only_card.as_str()),
        "a card with the assistant marker but the worker role leaked in — \
         the role conjunct is not being applied"
    );
    b.shutdown_harnesses().await;
}

/// `lastTurnCompletedAt` is the newest non-interrupt `turn/completed` transcript row, `null` when none.
/// The interrupt is pinned strictly later than the completion so a subquery that forgot to exclude interrupts returns a different number.
#[tokio::test]
async fn the_list_carries_the_last_non_interrupted_turn_completion() {
    let b = boot().await;
    let track_id = b.create_track("last-turn-completed").await;

    let (status, with_turns) = b.create_conversation(&track_id, "idem-lt-a", "first").await;
    assert_eq!(status, StatusCode::CREATED, "body={with_turns}");
    let (status, without_turns) = b
        .create_conversation(&track_id, "idem-lt-b", "second")
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={without_turns}");
    let with_turns = with_turns["id"].as_str().unwrap().to_string();
    let without_turns = without_turns["id"].as_str().unwrap().to_string();

    let (status, rows) = b.list_conversations(&track_id).await;
    assert_eq!(status, StatusCode::OK);
    for row in rows.as_array().unwrap() {
        assert!(
            row["lastTurnCompletedAt"].is_null(),
            "no turn has completed yet, the field must be present and null: {row}"
        );
    }

    let session_id: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
        .bind(&with_turns)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let mut outcome_ids = Vec::new();
    for (turn_id, status) in [("turn-1", "completed"), ("turn-2", "interrupted")] {
        let params = json!({ "id": turn_id, "status": status }).to_string();
        outcome_ids.push(
            b.repo
                .harness_turn_outcome_put(
                    &session_id,
                    &with_turns,
                    &track_id,
                    "thread-lt",
                    turn_id,
                    &params,
                )
                .await
                .unwrap(),
        );
    }
    let (completed, interrupted) = (outcome_ids[0], outcome_ids[1]);
    sqlx::query(
        "UPDATE harness_items SET created_at_ms = CASE id WHEN ?1 THEN 1000 WHEN ?2 THEN 2000 END \
         WHERE id IN (?1, ?2)",
    )
    .bind(completed)
    .bind(interrupted)
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, rows) = b.list_conversations(&track_id).await;
    assert_eq!(status, StatusCode::OK);
    let rows = rows.as_array().unwrap();
    let row_of = |id: &str| {
        rows.iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from {rows:?}"))
    };
    assert_eq!(
        row_of(&with_turns)["lastTurnCompletedAt"],
        json!(1000),
        "the completed turn's instant, not the later interrupt's: {rows:?}"
    );
    assert!(
        row_of(&without_turns)["lastTurnCompletedAt"].is_null(),
        "a conversation without turns stays null: {rows:?}"
    );
    b.shutdown_harnesses().await;
}

/// `POST /api/cards/{id}/planner/reset` restarts an assistant under the ASSISTANT profile; both halves
/// of the card's identity are re-read afterwards because reset re-enters the start adapter.
#[tokio::test]
async fn resetting_an_assistant_conversation_restarts_it_under_its_own_profile() {
    let b = boot().await;
    let track_id = b.create_track("assistant-reset").await;
    let (status, created) = b
        .create_conversation(&track_id, "idem-reset", "hello assistant")
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let card_id = created["id"].as_str().unwrap().to_string();
    let thread_before: Option<String> =
        sqlx::query_scalar("SELECT thread_id FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    // Captured BEFORE the reset: a reset that clears `root_session_id` to NULL loses the planner card's root
    // just as thoroughly, so the post-condition is equality against this value.
    let root_before: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert!(
        root_before.is_some(),
        "the track must already have a planner root session before the reset, \
         otherwise the equality check below is vacuous"
    );

    let (status, body) = b
        .request(
            "POST",
            &format!("/api/cards/{card_id}/planner/reset"),
            None,
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "reset must accept an assistant conversation: body={body}"
    );

    assert_eq!(
        b.card_role(&card_id).await,
        "assistant",
        "reset must not rewrite the conversation's role"
    );
    assert_eq!(
        b.card_marker(&card_id).await.as_deref(),
        Some("assistant"),
        "reset must not rewrite the conversation's marker"
    );
    // The profile actually taken: the Planner arm writes a `SharedPlanner` session, which takes over
    // `tracks.root_session_id`.
    let contracts: Vec<String> =
        sqlx::query_scalar("SELECT contract FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert!(
        contracts.iter().all(|contract| contract == "executor"),
        "reset restarted the assistant as a planner: {contracts:?}"
    );
    let root_after: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
            .bind(&track_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        root_after, root_before,
        "reset must leave the track's root session exactly as it was: taking it \
         over for the assistant and clearing it to NULL are both losses of the \
         planner card's root"
    );
    let assistant_sessions: Vec<String> =
        sqlx::query_scalar("SELECT id FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert!(
        !assistant_sessions
            .iter()
            .any(|id| root_after.as_deref() == Some(id.as_str())),
        "the reset assistant displaced the planner card as the track's root session"
    );
    // A reset is a hard restart: a new thread, and the card still listed. Ordered explicitly: "the live
    // session" is the newest active row.
    let thread_after: Option<String> = sqlx::query_scalar(
        "SELECT thread_id FROM worker_sessions WHERE card_id = ?1 \
           AND state IN ('starting','running','idle','turn_pending') \
         ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .bind(&card_id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    assert!(thread_after.is_some());
    assert_ne!(thread_after, thread_before, "reset must mint a new thread");
    let (status, rows) = b.list_conversations(&track_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.as_array().unwrap().len(), 1, "rows={rows}");
    b.shutdown_harnesses().await;
}

/// The endpoint's own boundaries: an unknown track is 404, retired Area-chat
/// scaffolding is 403, and the header is genuinely required rather than defaulted.
#[tokio::test]
async fn the_endpoint_refuses_unknown_tracks_chat_tracks_and_a_missing_key() {
    let b = boot().await;
    let track_id = b.create_track("assistant-boundaries").await;

    let (status, _) = b.list_conversations("track-does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = b
        .create_conversation("track-does-not-exist", "idem-x", "hi")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = b
        .request(
            "POST",
            &format!("/api/tracks/{track_id}/conversations"),
            None,
            Some(json!({"text": "hi"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");

    let (status, body) = b.create_conversation(&track_id, "idem-blank", "   ").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM cards WHERE track_id = ?1 AND role = 'assistant'",
            &track_id
        )
        .await,
        0,
        "a rejected first message must leave no card behind"
    );

    // A row created by an older build, before Area conversations were retired.
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(&track_id)
        .execute(b.repo.pool())
        .await
        .unwrap();
    let (status, body) = b.create_conversation(&track_id, "idem-chat", "hi").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
}

#[tokio::test]
async fn first_conversation_message_commits_model_selection_and_binds_it_to_retry() {
    let b = boot().await;
    let track_id = b.create_track("first-message-model").await;
    let uri = format!("/api/tracks/{track_id}/conversations");
    let body =
        json!({"text": "First message", "model": "chosen-model", "reasoning_effort": "high"});
    let (status, created) = b
        .request("POST", &uri, Some("selected-first"), Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let card_id = created["id"].as_str().unwrap();
    let card = b.repo.card_get(card_id).await.unwrap().unwrap();
    assert_eq!(card.payload["model"], "chosen-model");
    assert_eq!(card.payload["reasoning_effort"], "high");
    assert_eq!(card.payload["model_ever_set"], true);
    assert_eq!(card.payload["reasoning_effort_ever_set"], true);
    let (status, retried) = b
        .request("POST", &uri, Some("selected-first"), Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{retried}");
    assert_eq!(retried["id"], created["id"]);
    let (status, rejected) = b.request("POST", &uri, Some("selected-first"),
        Some(json!({"text": "First message", "model": "different-model", "reasoning_effort": "high"}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn changing_only_the_first_message_model_cannot_reuse_its_create_key() {
    let b = boot().await;
    let track_id = b.create_track("model-request-identity").await;
    let uri = format!("/api/tracks/{track_id}/conversations");
    let (status, body) = b
        .request(
            "POST",
            &uri,
            Some("bound-model"),
            Some(json!({"text": "hello", "model": "model-a"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = b
        .request(
            "POST",
            &uri,
            Some("bound-model"),
            Some(json!({"text": "hello", "model": "model-b"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    b.shutdown_harnesses().await;
}

#[test]
fn default_conversation_seed_keeps_legacy_operation_payload_bytes() {
    use calm_server::operation::planner_harness_start_adapter::LazyMintCardSeed;
    let legacy = json!({"title":null,"sort":null,"idempotency_key":"before-upgrade"});
    let seed: LazyMintCardSeed = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(seed).unwrap(), legacy);
}

#[tokio::test]
async fn blank_first_message_model_is_rejected_before_minting() {
    let b = boot().await;
    let track_id = b.create_track("blank-model").await;
    let (status, body) = b
        .request(
            "POST",
            &format!("/api/tracks/{track_id}/conversations"),
            Some("blank-model"),
            Some(json!({"text":"hello", "model":"   "})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, rows) = b.list_conversations(&track_id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows, json!([]));
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn an_agent_cannot_choose_the_first_conversation_model() {
    let b = boot().await;
    let track_id = b.create_track("human-model-choice").await;
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track_id}/conversations"))
                .header("content-type", "application/json")
                .header("idempotency-key", "agent-model-choice")
                .header("X-Calm-Actor", "ai:some-agent")
                .body(Body::from(
                    json!({"text":"hello", "model":"chosen-by-agent"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let (_, rows) = b.list_conversations(&track_id).await;
    assert_eq!(rows, json!([]));
    b.shutdown_harnesses().await;
}
