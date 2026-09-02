//! #1098 slice 3 — `POST`/`GET /api/coves/{cove_id}/conversations`.
//!
//! Covers INV-CHAT-003 (nothing exists until the first message), INV-CHAT-013
//! (a failed start leaves no blank card; a retry mints no second one) and the
//! list endpoint's fail-closed marker filter + LEFT JOIN.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::Principal;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    cove_id: String,
    repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "cove-conversations".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    repo.cove_folder_create(cove.id.as_str(), "/workspace")
        .await
        .unwrap();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let waves = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&waves).await.unwrap();
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
            calm_server::state::WriteContext::new(roles.clone(), waves.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(waves),
    )
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
        cove_id: cove.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

impl Boot {
    async fn create_conversation(&self, idempotency_key: &str, text: &str) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/coves/{}/conversations", self.cove_id))
                    .header("content-type", "application/json")
                    .header("idempotency-key", idempotency_key)
                    .body(Body::from(json!({ "text": text }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn list_conversations(&self) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/coves/{}/conversations", self.cove_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn ensure_chat_wave(&self) -> String {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/coves/{}/chat-wave/ensure", self.cove_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::OK | StatusCode::CREATED
        ));
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        body["id"].as_str().unwrap().to_string()
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// Chat cards anywhere in the DB — the marker, not the wave, is the
    /// identity.
    async fn chat_card_count(&self) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM cards WHERE json_extract(payload, '$.harness_profile') = 'plain_chat'",
        )
        .await
    }

    async fn session_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM worker_sessions").await
    }

    /// Threads live in two places: on the session row and mirrored into the
    /// card payload. Neither may exist before the first message.
    async fn thread_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM worker_sessions WHERE thread_id IS NOT NULL")
            .await
            + self
                .count(
                    "SELECT COUNT(*) FROM cards WHERE json_extract(payload, '$.codex_thread_id') IS NOT NULL",
                )
                .await
    }

    async fn user_message_count(&self) -> i64 {
        self.count("SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued'")
            .await
    }

    /// How many copies of `needle` the HARNESS has been handed — the thing
    /// double-send is actually about. The audit event is only evidence of a
    /// delivery, and the gap under test is exactly evidence going missing, so
    /// counting events would be circular.
    ///
    /// Two places have to be summed, because a message the harness accepted
    /// may or may not have reached the agent yet:
    ///   * turns already started on the fake app-server (delivered), and
    ///   * observations still sitting in the run loop's `pending_queue` —
    ///     the fake's first turn never completes, so a second message waits
    ///     there, and adjacent `UserMessage`s FOLD into one concatenated
    ///     entry. Counting entries would therefore under-report; substring
    ///     occurrences do not.
    ///
    /// Polls to `want` because the run loop drains on a background task, and
    /// returns whatever it saw so a failing assertion reports the real number.
    async fn user_message_copies_in_harness(&self, needle: &str, want: usize) -> usize {
        fn occurrences(haystack: &str, needle: &str) -> usize {
            haystack.matches(needle).count()
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let started = self
                .state
                .shared_codex_appserver
                .started_turns_for_test()
                .iter()
                .map(|(_, items)| {
                    items
                        .iter()
                        .map(|item| {
                            serde_json::to_string(item)
                                .map(|s| occurrences(&s, needle))
                                .unwrap_or(0)
                        })
                        .sum::<usize>()
                })
                .sum::<usize>();
            let mut queued = 0usize;
            let runtime_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
                .fetch_all(self.repo.pool())
                .await
                .unwrap();
            for id in runtime_ids {
                if let Some(handle) = self.state.harness.get(&id) {
                    for obs in handle.pending_queue_for_test().await {
                        queued += serde_json::to_string(&obs)
                            .map(|s| occurrences(&s, needle))
                            .unwrap_or(0);
                    }
                }
            }
            let seen = started + queued;
            if seen >= want || std::time::Instant::now() >= deadline {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// `POST /api/cards/{id}/spec/input` — the *other*, public way to put a
    /// user message on a conversation card. Used to drive the known gap where
    /// this route's first-message claim mistakes a foreign message for its own.
    async fn send_spec_input(&self, card_id: &str, text: &str) -> StatusCode {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/cards/{card_id}/spec/input"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "text": text }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
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

/// INV-CHAT-003 — `+` alone mints nothing; the first message mints exactly one
/// of each.
#[tokio::test]
async fn first_message_mints_exactly_one_card_session_and_thread() {
    let b = boot().await;
    b.ensure_chat_wave().await;
    assert_eq!(
        b.chat_card_count().await,
        0,
        "no card before the first message"
    );
    assert_eq!(
        b.session_count().await,
        0,
        "no session before the first message"
    );
    assert_eq!(
        b.thread_count().await,
        0,
        "no thread before the first message"
    );

    let (status, body) = b.create_conversation("idem-first", "hello there").await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(body["kind"], "shared-chat");
    assert_eq!(body["title"], Value::Null);
    assert_eq!(b.chat_card_count().await, 1);
    assert_eq!(b.session_count().await, 1);
    assert_eq!(
        b.thread_count().await,
        2,
        "one session thread + one card mirror"
    );
    assert_eq!(b.user_message_count().await, 1);
    b.shutdown_harnesses().await;
}

/// INV-CHAT-013(b) — a retried request under one `Idempotency-Key` is one
/// conversation, one session, and one first message.
#[tokio::test]
async fn retry_under_one_idempotency_key_mints_one_conversation() {
    let b = boot().await;
    let (first_status, first) = b.create_conversation("idem-retry", "same message").await;
    assert_eq!(first_status, StatusCode::CREATED, "body={first}");
    let (second_status, second) = b.create_conversation("idem-retry", "same message").await;
    assert_eq!(second_status, StatusCode::CREATED, "body={second}");

    assert_eq!(
        first["id"], second["id"],
        "retry must answer with the same conversation"
    );
    assert_eq!(b.chat_card_count().await, 1);
    assert_eq!(b.session_count().await, 1);
    assert_eq!(
        b.user_message_count().await,
        1,
        "a retry must not make the agent act twice on the same instruction"
    );
    assert_eq!(b.list_conversations().await.1.as_array().unwrap().len(), 1);
    b.shutdown_harnesses().await;
}

/// INV-CHAT-013(a) — a failed `thread/start` leaves no blank card, no session,
/// and no first message.
#[tokio::test]
async fn failed_thread_start_leaves_no_blank_card() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = b
        .create_conversation("idem-fails", "this will not land")
        .await;
    // Pinned to the exact answer, not "anything but 2xx": a failed
    // `thread/start` carries no error class, so the operation failure maps to
    // `internal`. A regression that turned this into, say, a 400 would be a
    // contract change, and `is_server_error() || is_client_error()` would not
    // have noticed.
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    assert_eq!(body["code"], "internal", "body={body}");

    assert_eq!(
        b.chat_card_count().await,
        0,
        "compensation must delete the minted card"
    );
    let live_sessions = b
        .count("SELECT COUNT(*) FROM worker_sessions WHERE state IN ('starting','running','idle','turn_pending')")
        .await;
    assert_eq!(live_sessions, 0);
    assert_eq!(
        b.user_message_count().await,
        0,
        "the first message must not be sent"
    );
    assert_eq!(b.list_conversations().await.1.as_array().unwrap().len(), 0);
}

/// A *different* conversation started after a failed one still works — i.e. a
/// failed attempt does not poison the cove or its chat wave.
///
/// This is NOT the retry story: two different keys are two different
/// conversations by design. The same-key retry is
/// `retry_under_the_same_key_after_a_failed_start_succeeds`.
#[tokio::test]
async fn a_new_key_after_a_failed_start_ends_with_exactly_one_card() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed_status, _) = b.create_conversation("idem-attempt-1", "first try").await;
    assert!(!failed_status.is_success());
    assert_eq!(b.chat_card_count().await, 0);

    let (status, body) = b.create_conversation("idem-attempt-2", "second try").await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(b.chat_card_count().await, 1);
    assert_eq!(b.list_conversations().await.1.as_array().unwrap().len(), 1);
    b.shutdown_harnesses().await;
}

/// A failed start must not turn the user's `Idempotency-Key` into a dead end.
///
/// The failed operation was compensated away (no card), so pressing send again
/// under the SAME key has to actually retry rather than replay the recorded
/// failure forever — slice 4 binds the key to the draft the user keeps
/// pressing send on. The derived card id is unchanged across attempts, which
/// is why the retry still cannot produce a second card.
#[tokio::test]
async fn retry_under_the_same_key_after_a_failed_start_succeeds() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (failed_status, _) = b.create_conversation("idem-same", "first try").await;
    assert_eq!(failed_status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(b.chat_card_count().await, 0);

    let (status, body) = b.create_conversation("idem-same", "first try").await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the same key must retry, not replay the failure: body={body}"
    );
    assert_eq!(b.chat_card_count().await, 1);
    assert_eq!(b.session_count().await, 1);
    assert_eq!(
        b.user_message_count().await,
        1,
        "the message the first attempt never sent must land exactly once"
    );
    assert_eq!(b.list_conversations().await.1.as_array().unwrap().len(), 1);

    // A third press of send under the same key still answers with the SAME
    // conversation: the `#N` operation-key suffix the successful attempt ran
    // under never entered the derived card id.
    let (third_status, third) = b.create_conversation("idem-same", "first try").await;
    assert_eq!(third_status, StatusCode::CREATED, "body={third}");
    assert_eq!(third["id"], body["id"]);
    assert_eq!(b.chat_card_count().await, 1);
    assert_eq!(b.user_message_count().await, 1);
    b.shutdown_harnesses().await;
}

/// INV-CHAT-013(b) under concurrency — two POSTs under one
/// `Idempotency-Key` that both get past the mint operation.
///
/// Both are released together at a barrier placed exactly where the two
/// requests used to each conclude "no earlier attempt, send the message":
/// after the shared operation settled, before the first-message claim. The
/// pre-fix code sent the first message twice here; the agent would act twice
/// on one instruction.
#[tokio::test]
async fn concurrent_same_key_creates_send_the_first_message_once() {
    let b = boot().await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    routes::cove_conversations::install_first_message_barrier_for_test(&b.cove_id, barrier);

    // Bounded: a regression that stops one request short of the barrier would
    // otherwise wedge the barrier and hang CI instead of failing.
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::join!(
            b.create_conversation("idem-concurrent", "hello twice"),
            b.create_conversation("idem-concurrent", "hello twice"),
        )
    })
    .await
    .expect("both concurrent creates must reach the first-message barrier");
    routes::cove_conversations::remove_first_message_barrier_for_test(&b.cove_id);

    assert_eq!(first.0, StatusCode::CREATED, "body={}", first.1);
    assert_eq!(second.0, StatusCode::CREATED, "body={}", second.1);
    assert_eq!(
        first.1["id"], second.1["id"],
        "both answers must name the same conversation"
    );
    assert_eq!(b.chat_card_count().await, 1, "exactly one card");
    assert_eq!(b.session_count().await, 1, "exactly one session");
    assert_eq!(
        b.user_message_count().await,
        1,
        "the agent must not receive the same first message twice"
    );
    assert_eq!(b.list_conversations().await.1.as_array().unwrap().len(), 1);
    b.shutdown_harnesses().await;
}

/// KNOWN GAP, pinned as current behaviour — NOT an invariant.
///
/// `send_spec_input` does two things that are not one thing:
/// `harness.observe(UserMessage)` pushes onto an in-memory queue, then
/// `log_pure_event(HarnessUserMessageEnqueued)` writes the row this endpoint's
/// dedup reads. If the first succeeds and the second fails, the agent already
/// has the message but nothing records it — so a retry under the same
/// `Idempotency-Key` sends it AGAIN and the agent can act twice on one
/// instruction.
///
/// This test injects exactly that failure (a trigger that aborts the audit
/// insert, i.e. a DB error at the second step) and asserts the double send. It
/// exists so the gap is visible and cannot be "fixed" by accident without
/// someone reading this comment; it is deliberately written as documentation
/// of today's behaviour rather than as a guarantee.
///
/// Root cause and structural fix: minting goes through the transactional
/// operation runtime while the send does not, so a non-transactional event is
/// being used as evidence for a non-transactional step. The fix is to fold the
/// first message into the same operation (seed `Observation::UserMessage` into
/// the harness snapshot's `pending_queue` in `prepare_tx`, as
/// `initial_snapshot_with_goal` already does for a spec harness's goal).
/// Tracked on #1098; out of scope for this slice.
#[tokio::test]
async fn first_send_whose_audit_event_fails_is_re_sent_on_retry() {
    let b = boot().await;
    // Fault injection at exactly the seam: the observation reaches the harness
    // queue, the audit write that would record it raises.
    sqlx::query(
        "CREATE TRIGGER fail_user_message_audit BEFORE INSERT ON events \
         WHEN NEW.kind = 'harness.user_message.enqueued' \
         BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END",
    )
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, body) = b
        .create_conversation("idem-lost-audit", "do the thing")
        .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the failed audit write must surface as an error: body={body}"
    );
    assert_eq!(
        b.chat_card_count().await,
        1,
        "the mint operation itself succeeded, so the (empty) conversation stays"
    );
    assert_eq!(
        b.user_message_count().await,
        0,
        "nothing was recorded — which is the whole problem"
    );
    let after_first = b.user_message_copies_in_harness("do the thing", 1).await;
    assert_eq!(
        after_first, 1,
        "but the harness already has it: copies={after_first}"
    );

    // The DB recovers; the client retries under the same key.
    sqlx::query("DROP TRIGGER fail_user_message_audit")
        .execute(b.repo.pool())
        .await
        .unwrap();
    let (status, body) = b
        .create_conversation("idem-lost-audit", "do the thing")
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(b.chat_card_count().await, 1, "still exactly one card");
    assert_eq!(b.user_message_count().await, 1);

    let after_retry = b.user_message_copies_in_harness("do the thing", 2).await;
    assert_eq!(
        after_retry, 2,
        "KNOWN GAP: the harness was handed the same first message twice \
         (copies: {after_retry})"
    );
    b.shutdown_harnesses().await;
}

/// KNOWN GAP, pinned as current behaviour — NOT an invariant.
///
/// The first-message claim asks "has this CARD ever had a user message
/// enqueued?", which is not the question the route needs answered ("has THIS
/// request's first message been delivered?"). Nothing scopes the evidence to
/// the request, so any other writer of `harness.user_message.enqueued` on the
/// card satisfies it.
///
/// The chain this test walks is entirely public API:
///   1. a create whose audit write fails leaves the CARD behind (the mint
///      operation succeeded, so no compensation runs),
///   2. `GET /api/coves/{cove}/conversations` hands out that card's id,
///   3. `POST /api/cards/{id}/spec/input` — a public endpoint — writes the
///      very event kind the claim reads,
///   4. the retry under the same `Idempotency-Key` reads that FOREIGN message
///      and skips its own send.
///
/// What this test proves is step 4: the retry answers 201 without sending,
/// i.e. its decision was made by somebody else's message. Contrast
/// `first_send_whose_audit_event_fails_is_re_sent_on_retry`, which is the same
/// setup MINUS the foreign send and ends with two copies in the harness — the
/// single foreign message is what flips the outcome.
///
/// Honest scope: in *this* injection the original message did reach the
/// harness queue before the audit write failed, so nothing is lost here — one
/// copy is the correct number and the visible defect is only that the claim
/// was satisfied by the wrong evidence. The variant that genuinely drops the
/// body's first message needs a first attempt that fails BEFORE
/// `harness.observe`, and this suite has no injection point there; it is not
/// asserted rather than asserted loosely.
///
/// Root cause and fix are shared with
/// `first_send_whose_audit_event_fails_is_re_sent_on_retry`: non-transactional,
/// non-request-scoped evidence standing in for a non-transactional step. See
/// the block comment in `routes/cove_conversations.rs`. Tracked on #1098, out
/// of scope for this slice.
#[tokio::test]
async fn foreign_send_between_a_failed_first_send_and_its_retry_skips_the_first_message() {
    let b = boot().await;
    sqlx::query(
        "CREATE TRIGGER fail_user_message_audit BEFORE INSERT ON events \
         WHEN NEW.kind = 'harness.user_message.enqueued' \
         BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END",
    )
    .execute(b.repo.pool())
    .await
    .unwrap();

    let (status, body) = b
        .create_conversation("idem-foreign", "MY-FIRST-MESSAGE")
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    sqlx::query("DROP TRIGGER fail_user_message_audit")
        .execute(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        b.user_message_count().await,
        0,
        "the audit write failed, so nothing records the first message"
    );

    // Step 2: the surviving card is listed, id and all.
    let (list_status, listed) = b.list_conversations().await;
    assert_eq!(list_status, StatusCode::OK);
    let card_id = listed.as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Step 3: a foreign message through the public per-card endpoint.
    assert_eq!(
        b.send_spec_input(&card_id, "SOMEBODY-ELSES-MESSAGE").await,
        StatusCode::OK
    );
    assert_eq!(
        b.user_message_count().await,
        1,
        "the only recorded message on this card belongs to the foreign send"
    );

    // Step 4: the retry believes its own first message already landed.
    let (retry_status, retry_body) = b
        .create_conversation("idem-foreign", "MY-FIRST-MESSAGE")
        .await;
    assert_eq!(retry_status, StatusCode::CREATED, "body={retry_body}");
    assert_eq!(retry_body["id"], card_id);
    assert_eq!(
        b.user_message_count().await,
        1,
        "KNOWN GAP: the retry sent nothing — the foreign message was taken as \
         proof that this request's first message had already been delivered"
    );
    let copies = b
        .user_message_copies_in_harness("MY-FIRST-MESSAGE", 2)
        .await;
    assert_eq!(
        copies, 1,
        "only the failed attempt's copy exists; the retry skipped its send \
         (copies={copies})"
    );
    b.shutdown_harnesses().await;
}

/// Same key, DIFFERENT first message ⇒ 409, and nothing happens.
///
/// The message body travels into the operation payload as a SHA-256
/// (`SpecHarnessStartOperationPayload::first_message_sha256`), so it is part of
/// `stable_payload_hash` and `OperationRuntime::submit` rejects the mismatch
/// before it does anything else. Without that binding the payload hash could
/// not see the text at all and this call would silently answer 201 with the
/// FIRST message's conversation — the API documenting a 409 it never produced.
#[tokio::test]
async fn same_key_with_a_different_first_message_is_a_conflict() {
    let b = boot().await;
    let (status, first) = b
        .create_conversation("idem-body", "the original text")
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={first}");

    let (status, body) = b.create_conversation("idem-body", "a different text").await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], "conflict", "body={body}");

    assert_eq!(b.chat_card_count().await, 1, "no second conversation");
    assert_eq!(b.session_count().await, 1);
    assert_eq!(
        b.user_message_count().await,
        1,
        "the rejected body must not reach the agent"
    );
    // `want: 1` so the helper keeps polling for a copy that must never turn
    // up, rather than returning on its first look.
    let copies = b
        .user_message_copies_in_harness("a different text", 1)
        .await;
    assert_eq!(copies, 0, "copies={copies}");
    assert_eq!(b.list_conversations().await.1.as_array().unwrap().len(), 1);

    // The same key with the SAME body still replays the success.
    let (status, replay) = b
        .create_conversation("idem-body", "the original text")
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(b.user_message_count().await, 1);
    b.shutdown_harnesses().await;
}

/// The documented EXCEPTION to the 409 above: after a terminally failed
/// attempt, the same key with an EDITED body is accepted.
///
/// Not an accident. The failed attempt was compensated away, so the retry
/// submits under a fresh `#N` operation key that no payload hash is bound to
/// yet, and arm (b) of the key contract exists precisely so the draft the user
/// keeps pressing send on can change between attempts. This test is here
/// because the OpenAPI text now claims the exception out loud; the claim needs
/// a run behind it, not a reading of `retryable_operation_key`.
#[tokio::test]
async fn an_edited_body_after_a_failed_attempt_is_accepted_under_the_same_key() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (status, body) = b.create_conversation("idem-edit", "draft one").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    assert_eq!(b.chat_card_count().await, 0);

    let (status, body) = b
        .create_conversation("idem-edit", "draft two, edited")
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an edited draft resent after a failure must land, not 409: body={body}"
    );
    assert_eq!(b.chat_card_count().await, 1, "still exactly one card");
    assert_eq!(b.user_message_count().await, 1);
    let sent = b
        .user_message_copies_in_harness("draft two, edited", 1)
        .await;
    assert_eq!(
        sent, 1,
        "the EDITED text is what the agent got: copies={sent}"
    );
    let stale = b.user_message_copies_in_harness("draft one", 1).await;
    assert_eq!(
        stale, 0,
        "the abandoned draft never reached it: copies={stale}"
    );
    b.shutdown_harnesses().await;
}

/// An `Idempotency-Key` that used up all 64 retry slots answers 409 with its
/// OWN code, not the generic `conflict`.
///
/// The distinction is the whole point: the other 409s on this route mean
/// "already exists" or "your body disagrees with the key", and a client that
/// cannot tell them apart has no way to know that the only fix here is to mint
/// a new key. Driven through the real route 64 times rather than by inserting
/// operation rows, so the slot accounting under test is the production one.
#[tokio::test]
async fn an_exhausted_idempotency_key_answers_409_idempotency_key_exhausted() {
    let b = boot().await;
    for attempt in 1..=64 {
        b.state
            .shared_codex_appserver
            .fail_next_thread_start_for_test();
        let (status, body) = b.create_conversation("idem-exhaust", "never lands").await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "attempt {attempt} body={body}"
        );
    }
    assert_eq!(b.chat_card_count().await, 0, "every attempt compensated");

    let (status, body) = b.create_conversation("idem-exhaust", "never lands").await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], "idempotency_key_exhausted", "body={body}");
    assert_eq!(b.chat_card_count().await, 0);
    assert_eq!(b.user_message_count().await, 0);

    // A fresh key is the documented way out, and it works.
    let (status, body) = b.create_conversation("idem-exhaust-2", "lands").await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    b.shutdown_harnesses().await;
}

/// The `Idempotency-Key` header is required, and rejection happens before
/// anything is minted.
#[tokio::test]
async fn missing_idempotency_key_is_rejected_and_mints_nothing() {
    let b = boot().await;
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/coves/{}/conversations", b.cove_id))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "text": "hi" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(b.chat_card_count().await, 0);
    assert_eq!(
        b.count("SELECT COUNT(*) FROM waves").await,
        0,
        "no wave either"
    );
}

/// Blank text is rejected before the card is minted.
#[tokio::test]
async fn blank_text_is_rejected_before_minting() {
    let b = boot().await;
    let (status, _) = b.create_conversation("idem-blank", "   \n ").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(b.chat_card_count().await, 0);
    assert_eq!(b.session_count().await, 0);
}

#[tokio::test]
async fn list_on_unknown_cove_is_not_found_and_without_chat_wave_is_empty() {
    let b = boot().await;
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/coves/nope/conversations")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let (status, body) = b.list_conversations().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
    assert_eq!(
        b.count("SELECT COUNT(*) FROM waves").await,
        0,
        "listing must not ensure a chat wave"
    );
}

/// The list is fail-closed on the card marker (INV-CHAT-007): the chat wave's
/// own kernel-owned spec and report cards never appear, an unmarked codex card
/// on the chat wave never appears, and a marked codex card on an ordinary wave
/// never appears either (it is not this cove's conversation container).
#[tokio::test]
async fn list_returns_only_marked_chat_cards() {
    let b = boot().await;
    let chat_wave = b.ensure_chat_wave().await;
    let (status, created) = b.create_conversation("idem-marker", "hello").await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");

    // An unmarked codex card sharing the chat wave.
    b.repo
        .card_create(NewCard {
            wave_id: chat_wave.clone().into(),
            title: Some("plain worker".into()),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    // A marked codex card on an ordinary wave.
    let ordinary = b
        .repo
        .wave_create(NewWave {
            cove_id: b.cove_id.clone().into(),
            title: "ordinary".into(),
            sort: None,
            cwd: "/workspace".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let stray = b
        .repo
        .card_create(NewCard {
            wave_id: ordinary.id.clone(),
            title: Some("stray".into()),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    b.state
        .card_role_cache
        .insert(stray.id.clone(), CardRole::Worker, ordinary.id.clone());

    // The three conjuncts are NOT three equal defences; do not read this
    // fixture as proving three independent walls.
    //
    // TAMPERING MODEL, NOT A REACHABLE STATE: nothing in production ever puts
    // the `plain_chat` marker on a chat wave's kernel-owned spec/report card —
    // their payload comes from `create_wave_with_spec_harness` and the
    // adapter's payload rewrite only touches thread keys. This `json_set`
    // forges that row by hand, so the `role = 'worker'` conjunct it exercises
    // is defence in depth against a corrupt/hand-edited DB, not a guard with a
    // reachable counterexample.
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.harness_profile', 'plain_chat') WHERE wave_id = ?1 AND role IN ('spec','reportcard')",
    )
    .bind(&chat_wave)
    .execute(b.repo.pool())
    .await
    .unwrap();
    // PRODUCTION-REACHABLE counterexample, and the reason `kind = 'codex'` is
    // load-bearing: `POST /api/cards` has no guard against `purpose =
    // 'cove-chat'` waves, and this list hands the chat `waveId` to the client,
    // so a user really can park a marked non-codex card on the chat wave.
    let marked_terminal = b
        .repo
        .card_create(NewCard {
            wave_id: chat_wave.clone().into(),
            title: Some("marked terminal".into()),
            kind: "terminal".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    b.state.card_role_cache.insert(
        marked_terminal.id.clone(),
        CardRole::Worker,
        chat_wave.clone().into(),
    );

    let spec_and_report: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cards WHERE wave_id = ?1 AND role IN ('spec','reportcard')",
    )
    .bind(&chat_wave)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(spec_and_report, 2, "the chat wave really does carry both");

    let (status, body) = b.list_conversations().await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1, "only the marked chat card: {body}");
    assert_eq!(rows[0]["id"], created["id"]);
    assert_eq!(rows[0]["waveId"], json!(chat_wave));
    assert_eq!(rows[0]["kind"], "shared-chat");
    b.shutdown_harnesses().await;
}

/// The LEFT JOIN is the point: a chat card whose session is gone stays
/// visible, with `state: null` rather than an invented value. An INNER JOIN
/// would hide it.
#[tokio::test]
async fn card_without_a_live_session_stays_visible_with_null_state() {
    let b = boot().await;
    let (status, created) = b.create_conversation("idem-orphan", "hello").await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let card_id = created["id"].as_str().unwrap().to_string();
    b.shutdown_harnesses().await;

    let (_, before) = b.list_conversations().await;
    assert!(
        before.as_array().unwrap()[0]["state"].is_string(),
        "a live session must report its state: {before}"
    );

    sqlx::query("DELETE FROM worker_sessions WHERE card_id = ?1")
        .bind(&card_id)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, body) = b.list_conversations().await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a session-less chat card must stay visible: {body}"
    );
    assert_eq!(rows[0]["id"], json!(card_id));
    assert_eq!(rows[0]["state"], Value::Null, "state must not be invented");
    assert!(
        rows[0]["updatedAt"].as_i64().unwrap() > 0,
        "falls back to the card's own stamp"
    );
}

/// INV-CHAT-004 pairing on the freshly minted conversation: the chat card
/// accepts `/spec/input`, a PTY-backed codex card does not.
#[tokio::test]
async fn spec_input_accepts_the_chat_card_and_still_refuses_a_pty_codex_card() {
    let b = boot().await;
    let (status, created) = b.create_conversation("idem-input", "hello").await;
    assert_eq!(status, StatusCode::CREATED, "body={created}");
    let chat_card = created["id"].as_str().unwrap().to_string();

    let send = |card_id: String| {
        let app = b.app.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/cards/{card_id}/spec/input"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"text": "follow-up"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    assert_eq!(send(chat_card).await, StatusCode::OK);

    let ordinary = b
        .repo
        .wave_create(NewWave {
            cove_id: b.cove_id.clone().into(),
            title: "pty".into(),
            sort: None,
            cwd: "/workspace".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let pty = b
        .repo
        .card_create(NewCard {
            wave_id: ordinary.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    b.state
        .card_role_cache
        .insert(pty.id.clone(), CardRole::Worker, ordinary.id.clone());
    assert_eq!(send(pty.id.to_string()).await, StatusCode::FORBIDDEN);
    b.shutdown_harnesses().await;
}
