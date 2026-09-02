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
use calm_server::ids::ActorId;
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
    /// #1253 PR2 — this server's own create-arm counters. Per instance, so a
    /// sibling case in the same binary cannot move them.
    create_counters: Arc<calm_server::routes::today_summary::TodaySummaryCreateCounters>,
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
    boot_with_rendezvous(tmp, repo, root_name, None).await
}

/// `boot_with`, plus the option to arm the create-arm rendezvous. Only the
/// create-race case passes `Some`.
async fn boot_with_rendezvous(
    tmp: TempDir,
    repo: Arc<SqlxRepo>,
    root_name: &str,
    rendezvous: Option<Arc<tokio::sync::Barrier>>,
) -> Boot {
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
    let state = match rendezvous {
        Some(barrier) => state.with_today_summary_create_rendezvous(barrier),
        None => state,
    };
    let create_counters = Arc::clone(&state.today_summary_create);
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
        create_counters,
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

    /// The distinct `events.actor` values behind one event kind, sorted.
    ///
    /// The audit log's own column, read the way an auditor would. Nothing else
    /// in this file looks at attribution, and until it did, the 30 lines of
    /// reasoning on `synthetic_actor` were unguarded: the mutation that
    /// forwards the caller's declared actor went red only because a downstream
    /// authorization gate happens to reject an AI actor carrying no card
    /// context today. Relax that gate — or re-attribute `ai:codex` once a card
    /// IS in scope — and the summary would start being recorded as an agent
    /// starting itself, with nothing turning red.
    async fn actors_for(&self, kind: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT DISTINCT actor FROM events WHERE kind = ?1 ORDER BY actor")
            .bind(kind)
            .fetch_all(self.repo.pool())
            .await
            .unwrap()
    }

    /// The distinct actors of every event written after `mark` about `card_id`.
    ///
    /// Watermarked because the card already carries the first trigger's events
    /// by the time the dormancy is staged; without it "some event somewhere is
    /// the kernel's" would be true before the restart ever ran.
    async fn actors_for_card_after(&self, mark: i64, card_id: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT DISTINCT actor FROM events WHERE id > ?1 AND scope_card = ?2 ORDER BY actor",
        )
        .bind(mark)
        .bind(card_id)
        .fetch_all(self.repo.pool())
        .await
        .unwrap()
    }

    async fn last_event_id(&self) -> i64 {
        sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events")
            .fetch_one(self.repo.pool())
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

/// How `events.actor` spells one [`ActorId`].
///
/// The column holds `serde_json::to_string(&actor)`
/// (`calm-truth/src/db/sqlite/events.rs`), so the expected value is computed
/// the same way rather than written out as a literal — a hand-typed `"user"`
/// would be a guess at a representation this test does not own, and would go
/// green or red for reasons that have nothing to do with attribution.
fn stored(actor: ActorId) -> String {
    serde_json::to_string(&actor).unwrap()
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

    /*
     * The audit log says a human did this, and that is the whole point of
     * `synthetic_actor`.
     *
     * `Actor::to_actor_id` maps `"user"` and every non-`ai:codex` value to
     * `ActorId::User`, so what this pins is not "two humans agree" — it is that
     * the caller's declared actor never reaches the message. The endpoint has
     * no `Actor` extractor precisely so that no future edit can start
     * forwarding one, and this is the assertion that notices if one does.
     * Attributing a button press to an agent would make the log say the summary
     * agent started itself, which is the `identity_migration_attribution_scope`
     * failure exactly.
     */
    assert_eq!(
        b.actors_for("harness.user_message.enqueued").await,
        vec![stored(ActorId::User)],
        "every message this endpoint sends is attributed to the human who \
         pressed the button"
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

    /*
     * A declared AI actor, and what it must NOT change is the attribution.
     *
     * This used to assert the derived `card_id` was unchanged — an assertion no
     * mutation can fail, because `derive_wave_conversation_keys(wave_id, key)`
     * has no actor parameter at all. It read like a guard and was one only by
     * accident (it covered the 403 path). The real property is that the header
     * does not reach the message: the endpoint takes no `Actor`, so a caller
     * claiming `ai:codex` still has the summary recorded against the human.
     */
    let (status, same) = before.summary(Some("ai:codex")).await;
    assert_eq!(status, StatusCode::OK, "body={same}");
    assert_eq!(
        before.actors_for("harness.user_message.enqueued").await,
        vec![stored(ActorId::User)],
        "a caller declaring `ai:codex` must not get the summary attributed to \
         an agent — the endpoint does not forward the header"
    );

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

    // Everything after this point is the recovery's doing, which is what makes
    // the attribution assertions below about the restart rather than about the
    // first trigger.
    let mark = b.last_event_id().await;

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
    /*
     * The restart is the kernel's, and it is the one place in this module that
     * constructs `ActorId::Kernel` directly — it has to, because
     * `Actor("kernel").to_actor_id()` silently degrades to `User`.
     *
     * Nobody asked for this restart: the user asked for a summary and the
     * server decided a harness needed re-opening. Recording it as a human act
     * would put a session start in the log that no human performed. The
     * messages stay the human's (asserted above), so this also pins that the
     * two attributions did not collapse into one.
     */
    let restart_actors = b.actors_for_card_after(mark, &card_id).await;
    assert!(
        restart_actors.contains(&stored(ActorId::Kernel)),
        "the dormant recovery's `spec-harness-start` must be attributed to the \
         kernel — it is the one act here no human asked for, and \
         `Actor(\"kernel\").to_actor_id()` silently degrades to User, so it is \
         also the one place this module builds an `ActorId` by hand; actors \
         were {restart_actors:?}"
    );
    assert!(
        restart_actors.contains(&stored(ActorId::User)),
        "…and the messages stay the human's, so the two attributions have not \
         collapsed into one; actors were {restart_actors:?}"
    );
}

/// A derived card that exists with an **empty transcript** still gets the
/// bootstrap. Both review channels found this from opposite ends.
///
/// Two production routes reach that state, and neither is drivable in-process:
///
/// * the create operation lands `Stuck` — `plan_compensation` marks it on the
///   first compensation error and never re-drives it, leaving the card behind
///   (`deletable: false`) with no runtime and no first message;
/// * the create operation *succeeds* and `create_wave_conversation`'s own first
///   `send_spec_input` then fails — a 503 from a shared app-server that went
///   down in between. It returns `Err`, so the summary is not sent either.
///
/// So the state is staged rather than driven, and the staging is deliberately
/// minimal: the card is minted through the **production** route (the same
/// endpoint, under the same fixed key, so the row is the row production makes),
/// and then exactly one thing is removed — the evidence row the predicate
/// reads. That pair, `card_get` says yes and `user_message_already_enqueued`
/// says no, is precisely what both scenarios leave behind.
///
/// What must NOT happen is what a card-only predicate does: skip the create arm
/// and send only the summary. The first *successful* trigger would then leave
/// ONE enqueued row where INV-TODAYDOC-010 requires two, and the standing "do
/// not touch the report until instructed" bootstrap would never be delivered at
/// all — for the life of the conversation, since the card exists from then on.
#[tokio::test]
async fn a_card_left_with_an_empty_transcript_still_receives_the_bootstrap() {
    let b = boot().await;
    let wave_id = b.user_wave("interrupted").await;
    b.edit_report(&wave_id, "something happened").await;

    // Mint the card through the real endpoint, under the real key.
    let (status, first) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let card_id = first["card_id"].as_str().unwrap().to_string();
    assert_eq!(b.enqueued_char_counts().await.len(), 2);

    // …then take away the one thing both failure modes never wrote.
    sqlx::query(
        "DELETE FROM events WHERE kind = 'harness.user_message.enqueued' AND scope_card = ?1",
    )
    .bind(&card_id)
    .execute(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        b.enqueued_char_counts().await,
        Vec::<i64>::new(),
        "the fixture must actually reproduce the empty transcript, or this \
         case proves nothing"
    );

    let (status, second) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={second}");
    assert_eq!(
        second["card_id"],
        json!(card_id),
        "still the one conversation"
    );

    let counts = b.enqueued_char_counts().await;
    assert_eq!(
        counts.len(),
        2,
        "the trigger that finds an empty transcript must deliver BOTH the \
         bootstrap and the summary: {counts:?}"
    );
    assert_eq!(
        counts[0],
        bootstrap_chars(),
        "and the bootstrap must come first, so the agent cannot take a turn on \
         the summary while still unbriefed: {counts:?}"
    );
    assert_ne!(counts[1], bootstrap_chars(), "{counts:?}");
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1,
        "recovering the message must not mint a second conversation — \
         re-running the create against an existing card is what `validate` \
         refuses, which is why the message is sent directly instead"
    );
}

/// D5's create-409 fallback: **conflict ⇒ resolve the derived card ⇒ carry on
/// to the spec input.**
///
/// The window is one request wide — between this handler's `card_get` and its
/// create, a concurrent request under the same fixed key can mint the card —
/// and it is *created* here rather than waited for. `tokio::join!` does not
/// order two requests, so a case that fired two and hoped would be green on a
/// scheduler that serialised them, reporting success for a run in which the arm
/// was never entered. The counters are what prove it was: `attempts` says the
/// request found no card, `conflicts` says it took the fallback.
///
/// The interloper is the production conversation endpoint under the same key
/// with **different text**, because that is what makes the conflict permanent
/// rather than an idempotent replay: the first message is bound into the
/// operation payload as a SHA-256, so the two submissions collide on
/// `insert_operation`'s "same key, different payload hash" — the 409 that never
/// expires, since `operations` has no pruner. Failing outright there would
/// leave the button dead forever on a state the caller asked for and that now
/// exists.
#[tokio::test]
async fn a_create_that_loses_the_key_race_resolves_the_card_and_still_sends() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let b = boot_with_rendezvous(
        TempDir::new().unwrap(),
        Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap()),
        "workspaces",
        Some(barrier.clone()),
    )
    .await;
    let wave_id = b.user_wave("contended").await;
    b.edit_report(&wave_id, "something happened").await;

    let app = b.app.clone();
    let trigger = tokio::spawn(async move {
        let response = app
            .oneshot(
                Request::post("/api/today/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
        )
    });

    // Wait until the request has passed `card_get` and found nothing. Only then
    // is planting a card guaranteed to produce the conflict; acting earlier
    // would make it take the "card already exists" path and never reach the
    // create arm at all.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while b.create_counters.snapshot().0 == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the trigger never entered the create arm; the rendezvous is not \
             where this case thinks it is"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let launchpad = b.launchpad_wave_id().await.expect("ensure minted it");

    // The interloper, through the production route, same key, different text.
    let response = b
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api/waves/{launchpad}/conversations"))
                .header("content-type", "application/json")
                // The SAME fixed key the endpoint derives from — that is what
                // makes both submissions aim at one card and one operation key.
                .header(
                    "idempotency-key",
                    calm_server::routes::today_summary::TODAY_SUMMARY_CONVERSATION_KEY,
                )
                .body(Body::from(
                    json!({ "text": "a different first message" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let planted: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::CREATED, "planted={planted}");
    let card_id = planted["id"].as_str().unwrap().to_string();

    // Release the parked request into the conflict.
    barrier.wait().await;
    let (status, body) = trigger.await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "losing the key race must not fail the trigger — the card it wanted \
         now exists: {body}"
    );
    assert_eq!(body["card_id"], json!(card_id), "and it is that card");

    let (attempts, conflicts) = b.create_counters.snapshot();
    assert_eq!(attempts, 1, "one request entered the create arm");
    assert_eq!(
        conflicts, 1,
        "…and it took the 409 fallback. Without this the assertions above are \
         all satisfied by a run in which the race never happened"
    );
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1,
        "the race must not leave two conversations"
    );
    // The interloper's own first message, then the summary the trigger was for.
    // The bootstrap is not re-sent: the transcript is no longer empty, which is
    // the same predicate the empty-transcript case above turns the other way.
    let counts = b.enqueued_char_counts().await;
    assert_eq!(
        counts.len(),
        2,
        "the trigger must still deliver its summary after the fallback: \
         {counts:?}"
    );
}
