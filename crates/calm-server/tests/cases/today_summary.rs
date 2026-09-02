//! #1253 PR2 — `POST /api/today/summary` end to end.
//!
//! Owns three of the design's invariants:
//!
//! * **INV-TODAYDOC-007** — this endpoint refuses an empty activity window,
//!   creating no conversation and enqueuing no message.
//! * **INV-TODAYDOC-010** — the first successful trigger leaves **two**
//!   `harness.user_message.enqueued` rows (bootstrap + summary) and every later
//!   one leaves a third, fourth, … Merged turns are expected and are never
//!   counted.
//! * **INV-TODAYDOC-011** — the derived conversation card is invariant under
//!   actor, workspace re-point and request count.
//!
//! Plus the one thing the projection's own unit tests cannot claim: that the
//! rows a *real* emitter writes are the rows it counts.
//!
//! Activity is always produced through production routes here — never by
//! inserting into `events` — because "does the projection see what the kernel
//! writes?" is precisely what an insert would assume rather than test. The unit
//! tests in `activity_window` take the opposite side of that split: they need
//! millisecond control over `at`, which no route offers, and they say so.

#![cfg(unix)]

use std::{path::PathBuf, sync::Arc};

use axum::{
    Extension,
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::auth::Principal;
use calm_server::db::{RepoOutOfDomain, RepoRead};
use calm_server::{
    card_role_cache::CardRoleCache,
    db::{Repo, sqlite::SqlxRepo},
    event::EventBus,
    plugin_host::{PluginHost, PluginRegistry},
    routes,
    routes::today_summary::TODAY_SUMMARY_BOOTSTRAP_TEXT,
    shared_codex_appserver::SharedCodexAppServer,
    state::{AppState, CodexClient, DaemonClient, WriteContext},
    wave_cove_cache::WaveCoveCache,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    boot_with(TempDir::new().unwrap(), repo, "workspaces").await
}

/// A server over a given database and workspace root.
///
/// The root is a parameter for the same reason `today_launchpad`'s is: booting
/// a *second* server over the *same* database with a *different* root is what a
/// workspace re-point looks like from the rows' point of view, and that is the
/// fixture INV-TODAYDOC-011 needs.
async fn boot_with(tmp: TempDir, repo: Arc<SqlxRepo>, root_name: &str) -> Boot {
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let roles = CardRoleCache::new();
    let waves = WaveCoveCache::new();
    // Seeded, not empty: a second server over an existing database must
    // recognise the cards already there, or `ensure` tries to mint a second
    // spec card and the re-point fixture stops being a re-point.
    repo.seed_card_role_cache(&roles).await.unwrap();
    repo.seed_wave_cove_cache(&waves).await.unwrap();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("data"),
        proc_supervisor_sock: None,
    });
    std::fs::create_dir_all(&daemon.data_dir).unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo_dyn.clone(),
        PathBuf::new(),
        tmp.path().join("plugins-data"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), waves.clone()),
    ));
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(waves),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ))
    .with_workspace_root(tmp.path().join(root_name));
    let app = routes::router()
        // `POST /api/waves/{id}/report` — the route this file produces its
        // activity with — extracts a `Principal`, so the session layer has to
        // be present exactly as `main.rs` assembles it.
        .layer(Extension(Principal {
            user_id: "owner".into(),
            display_name: "owner".into(),
            role: "owner".into(),
            session_id: "test".into(),
        }))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    Boot {
        app,
        repo,
        _tmp: tmp,
    }
}

impl Boot {
    async fn request(
        &self,
        method: &str,
        uri: &str,
        actor: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(actor) = actor {
            builder = builder.header("x-calm-actor", actor);
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

    /// A user-visible cove with one real wave in it. Through `POST /api/coves`
    /// and `POST /api/waves`, so the cove is `kind = 'user'` and the wave has
    /// the cards and workspace a production wave has.
    async fn user_wave(&self, title: &str) -> String {
        let (status, cove) = self
            .request(
                "POST",
                "/api/coves",
                None,
                Some(json!({"name": title, "color": "#abc"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "cove={cove}");
        let (status, wave) = self
            .request(
                "POST",
                "/api/waves",
                None,
                Some(json!({
                    "cove_id": cove["id"],
                    "title": title,
                    "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "wave={wave}");
        wave["id"].as_str().unwrap().to_string()
    }

    /// Real activity: a user editing a wave's report through the REST route.
    ///
    /// This is a production emitter — `persist_report` writes one `CardUpdated`
    /// and one `WaveReportEdited` at `EventScope::Wave` — so it is the row shape
    /// the projection has to be able to see, not a hand-built approximation.
    async fn edit_report(&self, wave_id: &str, summary: &str) {
        let (status, body) = self
            .request(
                "POST",
                &format!("/api/waves/{wave_id}/report"),
                None,
                Some(json!({"ifDocRev": 0, "summary": summary, "body": format!("# {summary}\n")})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "report edit failed: {body}");
    }

    async fn summary(&self, actor: Option<&str>) -> (StatusCode, Value) {
        self.request("POST", "/api/today/summary", actor, None)
            .await
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// Every `harness.user_message.enqueued` row's `char_count`, oldest first.
    ///
    /// The counts are how a bootstrap message is told from a summary message:
    /// the two texts differ in length, so the sequence says *which* messages
    /// were sent and in what order — which is the whole content of
    /// INV-TODAYDOC-010. A bare `COUNT(*)` would be satisfied by three
    /// bootstraps.
    async fn enqueued_char_counts(&self) -> Vec<i64> {
        sqlx::query_scalar(
            "SELECT json_extract(payload, '$.char_count') FROM events \
              WHERE kind = 'harness.user_message.enqueued' ORDER BY id",
        )
        .fetch_all(self.repo.pool())
        .await
        .unwrap()
    }

    async fn launchpad_wave_id(&self) -> Option<String> {
        self.repo
            .wave_get_launchpad()
            .await
            .unwrap()
            .map(|wave| wave.id.to_string())
    }
}

fn bootstrap_chars() -> i64 {
    TODAY_SUMMARY_BOOTSTRAP_TEXT.chars().count() as i64
}

/// INV-TODAYDOC-007 — the endpoint refuses an empty window and leaves nothing
/// behind.
///
/// Both halves are in one case on purpose. The refusal assertions are all
/// satisfied by an endpoint that never works at all, so the second half drives
/// the same endpoint against a workspace that *has* activity and shows every
/// one of them flipping. Without it this would be green on a handler that
/// returned 409 unconditionally.
///
/// The statement is narrow by design: it is about **this endpoint**.
/// `POST /api/waves/{id}/conversations` and `POST /api/cards/{id}/spec/input`
/// remain reachable and are deliberately out of scope — a user typing to an
/// agent by hand is not what is being prevented.
#[tokio::test]
async fn an_empty_activity_window_refuses_without_creating_or_sending_anything() {
    let b = boot().await;
    // A wave exists, so "nothing happened" is not "nothing exists": creating a
    // wave is not activity under the allowlist (`wave.created` is not on it),
    // and that is the state a user opening Today on a quiet morning is in.
    let wave_id = b.user_wave("quiet").await;

    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("today_summary_no_activity"),
        "body={body}"
    );

    assert_eq!(
        b.launchpad_wave_id().await,
        None,
        "a refusal must not even bootstrap the launchpad: `ensure` materializes \
         a workspace and waits on a harness start, and the gate is placed \
         before it precisely so an empty day costs neither"
    );
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        0,
        "no conversation card may exist after a refusal"
    );
    assert_eq!(
        b.enqueued_char_counts().await,
        Vec::<i64>::new(),
        "no message may be enqueued after a refusal"
    );

    // --- and now the same endpoint, with activity ---
    b.edit_report(&wave_id, "did a thing").await;
    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(b.launchpad_wave_id().await.is_some());
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1
    );
    assert_eq!(
        b.enqueued_char_counts().await.len(),
        2,
        "with activity the same endpoint creates the conversation and sends \
         both messages — which is what makes the refusal assertions above mean \
         something"
    );
}

/// INV-TODAYDOC-010 — the first trigger enqueues two messages, every later one
/// enqueues one more.
///
/// **Turns are deliberately not counted.** `run_loop::maybe_issue_turn` drains
/// the whole pending queue into a single `turn_start`, so "three presses, three
/// turns" is false by design and a case asserting it could only pass on
/// timing. `harness.user_message.enqueued` is a permanent kind written once per
/// enqueue, which is the layer that can actually be proved.
///
/// The regression it exists for is the silent no-op: a second press that sends
/// nothing at all, because `create_wave_conversation` skips its send once the
/// card has ever had a message. That is why the summary is sent *outside* the
/// create branch, and it is what the third and fourth rows below assert.
///
/// The `char_count` sequence — rather than a count — also pins the first
/// trigger's *second* row as a summary rather than a second bootstrap, which is
/// the other defect this shape had: a design revision that gave the summary
/// only to the re-run branch produced a first summary with no material in it.
#[tokio::test]
async fn the_first_trigger_sends_bootstrap_and_summary_and_each_later_one_sends_a_summary() {
    let b = boot().await;
    let wave_id = b.user_wave("busy").await;
    b.edit_report(&wave_id, "first").await;

    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let after_first = b.enqueued_char_counts().await;
    assert_eq!(
        after_first.len(),
        2,
        "the first trigger must leave TWO rows — the static bootstrap and the \
         summary: {after_first:?}"
    );
    assert_eq!(
        after_first[0],
        bootstrap_chars(),
        "the first message is the static bootstrap: {after_first:?}"
    );
    let summary_chars = after_first[1];
    assert_ne!(
        summary_chars,
        bootstrap_chars(),
        "the second message must be the summary, not a second bootstrap — a \
         first use that carries no activity is the defect this shape exists to \
         avoid: {after_first:?}"
    );

    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert_eq!(
        b.enqueued_char_counts().await,
        vec![
            bootstrap_chars(),
            summary_chars,
            summary_chars,
            summary_chars
        ],
        "three triggers must leave 2 + 1 + 1 = 4 rows; a second trigger that \
         adds none is the silent no-op this invariant exists to catch"
    );
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1,
        "three triggers, one conversation"
    );
}

/// INV-TODAYDOC-011 — the summary conversation is the same card whatever the
/// actor, and across a workspace re-point.
///
/// The re-point is the decisive half, and it is why this is an end-to-end case
/// rather than only the module's golden. `derive_wave_conversation_keys` feeds
/// one digest to the card id **and** the operation key, so mixing
/// `workspace_key_digest(cwd)` into the key — the shape `today.rs` uses for a
/// key that carries no conversation identity — derives a *second* conversation
/// card the moment the workspace moves. Nothing about that failure looks wrong:
/// both requests succeed, and the user simply finds two conversations, one of
/// which has the history.
///
/// The second server over the same database with a different workspace root is
/// exactly what a `CALM_WORKSPACE_ROOT` change (or a pre-S2 upgrade) looks like
/// to the rows.
#[tokio::test]
async fn a_repointed_workspace_and_a_different_actor_reuse_the_one_summary_conversation() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let before = boot_with(TempDir::new().unwrap(), repo.clone(), "workspaces-old").await;
    let wave_id = before.user_wave("busy").await;
    before.edit_report(&wave_id, "first").await;

    let (status, first) = before.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let launchpad = first["wave_id"].as_str().unwrap().to_string();
    let card_id = first["card_id"].as_str().unwrap().to_string();
    assert_eq!(
        card_id,
        calm_server::routes::today_summary::today_summary_card_id_for_test(&launchpad),
        "the endpoint must land on the card the bare-key derivation names"
    );
    let old_path: String =
        sqlx::query_scalar("SELECT workspace_path FROM waves WHERE purpose='launchpad'")
            .fetch_one(repo.pool())
            .await
            .unwrap();

    // A declared AI actor. The endpoint has no `Actor` extractor, so this must
    // change nothing — including the derived card, which would move if the
    // actor ever reached the key.
    let (status, same) = before.summary(Some("ai:codex")).await;
    assert_eq!(status, StatusCode::OK, "body={same}");
    assert_eq!(same["card_id"], json!(card_id));

    // --- the re-point ---
    let after = boot_with(TempDir::new().unwrap(), repo.clone(), "workspaces-new").await;
    let (status, repointed) = after.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={repointed}");
    let new_path: String =
        sqlx::query_scalar("SELECT workspace_path FROM waves WHERE purpose='launchpad'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_ne!(
        new_path, old_path,
        "the fixture must actually re-point the workspace, or the case proves \
         nothing about a cwd-keyed derivation"
    );
    assert_eq!(
        repointed["card_id"],
        json!(card_id),
        "a re-pointed workspace must reuse the one summary conversation"
    );

    let cards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        cards, 1,
        "three requests across two roots, one conversation"
    );
    // …and it is still the same conversation, still being talked to: 2 + 1 + 1.
    let enqueued: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(enqueued, 4);
}

/// The projection counts what the kernel actually writes.
///
/// `activity_window`'s own cases build their rows with an INSERT, because they
/// need to choose `at` to the millisecond. That leaves one thing they cannot
/// claim: that a real emitter's row has the `kind` and the `scope_wave` the
/// query joins on. This drives two production write paths and reads the answer
/// out of the endpoint's own gate — if either emitter's shape stopped matching,
/// the endpoint would refuse a day on which two things demonstrably happened.
///
/// It also pins the visibility filter from the other side: the launchpad's own
/// report edits, which every successful summary produces, are in the system
/// cove and must never be what keeps the window non-empty.
#[tokio::test]
async fn a_real_report_edit_and_a_real_lifecycle_change_are_both_counted_as_activity() {
    let b = boot().await;
    let wave_id = b.user_wave("real").await;

    // Nothing yet — so the two writes below are the only reason the gate opens.
    let (status, _) = b.summary(None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    b.edit_report(&wave_id, "a real edit").await;
    let (status, body) = b.summary(None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a real `wave.report_edited` must count as activity: {body}"
    );

    // A fresh database for the lifecycle half, so the report edit above cannot
    // be what opens the gate.
    let b = boot().await;
    let wave_id = b.user_wave("real").await;
    let (status, _) = b.summary(None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, patched) = b
        .request(
            "PATCH",
            &format!("/api/waves/{wave_id}"),
            None,
            Some(json!({"lifecycle": "planning"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={patched}");
    let (status, body) = b.summary(None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a real `wave.lifecycle_changed` must count as activity: {body}"
    );
}

/// A dormant harness is recovered by re-submitting `spec-harness-start`, and
/// the conversation's transcript survives it.
///
/// D5 makes this mandatory, and the reason is that with one long-lived
/// conversation a single dormant session would kill the button for good: there
/// is no other route back to a live harness, so the button would answer 409
/// forever until a human pressed Reset.
///
/// **What must NOT happen is the easy fix.** `reset_spec_harness_card` — the
/// `/spec/reset` path — hard-codes `reset_harness_items: true`, which erases
/// the card's harness items. Those items *are* the conversation the user asked
/// for, so recovering through reset would answer 200 while deleting the thing
/// the feature exists to produce. Nothing about that shows up in a status code,
/// which is why the assertion below is on the item count and not on the
/// response.
#[tokio::test]
async fn a_dormant_harness_is_restarted_without_erasing_the_conversation() {
    let b = boot().await;
    let wave_id = b.user_wave("dormant").await;
    b.edit_report(&wave_id, "something happened").await;

    let (status, first) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let card_id = first["card_id"].as_str().unwrap().to_string();
    let launchpad = first["wave_id"].as_str().unwrap().to_string();

    // A turn's worth of transcript, so "did the recovery keep it?" has an
    // answer. Written through the repo the harness itself writes with.
    b.repo
        .harness_item_insert(
            "runtime-x",
            &card_id,
            &launchpad,
            "thread-x",
            Some("turn"),
            Some("item"),
            Some("agent_message"),
            "item/completed",
            "{}",
        )
        .await
        .unwrap();
    let items_before = b
        .scalar(&format!(
            "SELECT COUNT(*) FROM harness_items WHERE card_id = '{card_id}'"
        ))
        .await;
    assert_eq!(items_before, 1);

    // Dormancy, in the shape `ensure_live_spec_harness` actually tests for: no
    // session row in an active state for this card. That is trigger B of #649 —
    // the state a failed start or a crashed session leaves behind.
    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE card_id = ?1")
        .bind(&card_id)
        .execute(b.repo.pool())
        .await
        .unwrap();

    let (status, second) = b.summary(None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a dormant harness must be restarted rather than surfaced: {second}"
    );
    assert_eq!(second["card_id"], json!(card_id), "and on the same card");
    assert_eq!(
        b.scalar(&format!(
            "SELECT COUNT(*) FROM harness_items WHERE card_id = '{card_id}'"
        ))
        .await,
        items_before,
        "the recovery must not erase the transcript — that is the difference \
         between re-submitting a start and going through `/spec/reset`"
    );
    assert_eq!(
        b.enqueued_char_counts().await.len(),
        3,
        "and the summary the trigger was for must actually have been sent"
    );
}
