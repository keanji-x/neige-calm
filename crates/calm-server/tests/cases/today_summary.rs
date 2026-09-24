//! `POST /api/today/summary` end to end, plus the launchpad conversation's opening briefing.
//! Activity is always produced through production routes here, never by inserting into `events`.

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
    track_area_cache::TrackAreaCache,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    /// This server's own create-arm counters, per instance so a sibling case cannot move them.
    create_counters: Arc<calm_server::routes::today_summary::TodaySummaryCreateCounters>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    boot_with(TempDir::new().unwrap(), repo, "workspaces").await
}

/// A server over a given database and workspace root; a second server over the same database with a
/// different root is what a workspace re-point looks like to the rows.
async fn boot_with(tmp: TempDir, repo: Arc<SqlxRepo>, root_name: &str) -> Boot {
    boot_with_rendezvous(tmp, repo, root_name, None).await
}

/// `boot_with`, plus the option to arm the create-arm rendezvous.
async fn boot_with_rendezvous(
    tmp: TempDir,
    repo: Arc<SqlxRepo>,
    root_name: &str,
    rendezvous: Option<Arc<tokio::sync::Barrier>>,
) -> Boot {
    boot_with_rendezvouses(tmp, repo, root_name, rendezvous, None).await
}

async fn boot_with_rendezvouses(
    tmp: TempDir,
    repo: Arc<SqlxRepo>,
    root_name: &str,
    create_rendezvous: Option<Arc<tokio::sync::Barrier>>,
    bootstrap_rendezvous: Option<Arc<tokio::sync::Barrier>>,
) -> Boot {
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    // Seeded, not empty: a second server over an existing database must recognise the cards already
    // there, or `ensure` tries to mint a second planner card.
    repo.seed_card_role_cache(&roles).await.unwrap();
    repo.seed_track_area_cache(&tracks).await.unwrap();
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
        WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events,
        daemon,
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ))
    .with_workspace_root(tmp.path().join(root_name));
    let state = match create_rendezvous {
        Some(barrier) => state.with_today_summary_create_rendezvous(barrier),
        None => state,
    };
    let state = match bootstrap_rendezvous {
        Some(barrier) => state.with_today_summary_bootstrap_rendezvous(barrier),
        None => state,
    };
    let create_counters = Arc::clone(&state.today_summary_create);
    let app = routes::router()
        // `POST /api/tracks/{id}/report` extracts a `Principal`, so the session layer has to be present.
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

    /// A user-visible area with one real track in it, through `POST /api/areas` and `POST /api/tracks`.
    async fn user_track(&self, title: &str) -> String {
        let (status, area) = self
            .request(
                "POST",
                "/api/areas",
                None,
                Some(json!({"name": title, "color": "#abc"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "area={area}");
        let (status, track) = self
            .request(
                "POST",
                "/api/tracks",
                None,
                Some(json!({
                    "planner_provider": "codex",
                    "area_id": area["id"],
                    "title": title,
                    "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "track={track}");
        track["id"].as_str().unwrap().to_string()
    }

    /// Real activity: a user editing a track's report through the REST route (a production emitter).
    async fn edit_report(&self, track_id: &str, summary: &str) {
        let (status, body) = self
            .request(
                "POST",
                &format!("/api/tracks/{track_id}/report"),
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

    /// `POST /api/today/launchpad/ensure` — reaches a launchpad on a day the summary endpoint would refuse.
    async fn ensure_launchpad(&self) -> String {
        let (status, body) = self
            .request("POST", "/api/today/launchpad/ensure", None, None)
            .await;
        assert!(
            status.is_success(),
            "launchpad ensure failed: {status} {body}"
        );
        body["track_id"].as_str().unwrap().to_string()
    }

    /// `POST /api/tracks/{id}/conversations` — the production create route.
    async fn create_conversation(
        &self,
        track_id: &str,
        idempotency_key: &str,
        text: &str,
    ) -> (StatusCode, Value) {
        let request = Request::post(format!("/api/tracks/{track_id}/conversations"))
            .header("content-type", "application/json")
            .header("idempotency-key", idempotency_key)
            .body(Body::from(json!({ "text": text }).to_string()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// This card's transcript in delivery order: what the app-server was handed first, then what is
    /// still queued behind it (turns before queue; within each half the source's own order).
    async fn transcript_in_order(&self, card_id: &str) -> String {
        let mut texts = self.turn_texts();
        texts.extend(self.queued_texts(card_id).await);
        texts.join("\n---\n")
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// Every `harness.user_message.enqueued` row for **this card**, oldest first, as its `char_count`.
    /// A COUNT of enqueues only: the lengths are not a discriminator; use [`Boot::delivered`] for "which message".
    async fn enqueued_char_counts(&self, card_id: &str) -> Vec<i64> {
        sqlx::query_scalar(
            "SELECT json_extract(payload, '$.char_count') FROM events \
              WHERE kind = 'harness.user_message.enqueued' AND scope_card = ?1 \
              ORDER BY id",
        )
        .bind(card_id)
        .fetch_all(self.repo.pool())
        .await
        .unwrap()
    }

    /// The distinct `events.actor` values behind one event kind, sorted.
    async fn actors_for(&self, kind: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT DISTINCT actor FROM events WHERE kind = ?1 ORDER BY actor")
            .bind(kind)
            .fetch_all(self.repo.pool())
            .await
            .unwrap()
    }

    /// The distinct actors of every event of one kind written after `mark` about `card_id`.
    /// Watermarked and keyed by kind: unrelated kernel-authored rows land in the same window.
    async fn actors_for_card_after(&self, mark: i64, card_id: &str, kind: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT DISTINCT actor FROM events \
              WHERE id > ?1 AND scope_card = ?2 AND kind = ?3 ORDER BY actor",
        )
        .bind(mark)
        .bind(card_id)
        .bind(kind)
        .fetch_all(self.repo.pool())
        .await
        .unwrap()
    }

    /// The **production** predicate statement, run against this server's database:
    /// `user_message_enqueued_on_active_runtime` is `pub(crate)`, so its SQL is executed verbatim instead.
    async fn enqueued_on_active_runtime(&self, track_id: &str, card_id: &str) -> bool {
        sqlx::query_scalar::<_, i64>(
            &calm_server::routes::conversations_shared::user_message_enqueued_on_active_runtime_sql(
            ),
        )
        .bind(card_id)
        .bind(track_id)
        .fetch_optional(self.repo.pool())
        .await
        .unwrap()
        .is_some()
    }

    /// This card's ACTIVE runtime id, through the production read rather than a
    /// hand-written state filter.
    async fn active_runtime(&self, card_id: &str) -> Option<String> {
        use calm_server::session_projection_repo::WorkerSessionProjectionRepo;
        self.repo
            .session_projection_active_for_card(&card_id.to_string())
            .await
            .unwrap()
            .map(|runtime| runtime.id)
    }

    async fn last_event_id(&self) -> i64 {
        sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events")
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// How many times each `needle` was **actually delivered**, matched by bytes over this server's turns
    /// (server-wide) plus `card_id`'s NEWEST queue. A message can briefly be in both, so the read is retried until the needles account for exactly `expected_total`.
    async fn delivered(
        &self,
        card_id: &str,
        needles: &[&str],
        expected_total: usize,
    ) -> Vec<usize> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let queued = self.queued_texts(card_id).await;
            let turns = self.turn_texts();
            let mut texts = queued.clone();
            texts.extend(turns.clone());
            let counts = count_needles(&texts, needles);
            if counts.iter().sum::<usize>() == expected_total {
                return counts;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "delivered messages never settled at {expected_total}: saw \
                     {counts:?} for {needles:?}\nqueued in the persisted \
                     snapshot ({}): {queued:#?}\nturn texts ({}): {turns:#?}",
                    queued.len(),
                    turns.len(),
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Block until `needle` has reached this server's fake app-server, and answer how many times it occurs
    /// there. Sampled at first sighting: the fake never completes a turn, so only the first turn's contents are ever delivered.
    async fn await_reached_appserver(&self, card_id: &str, needle: &str) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let texts = self.turn_texts();
            let count = count_needles(&texts, &[needle])[0];
            if count > 0 {
                return count;
            }
            if std::time::Instant::now() >= deadline {
                let queued = self.queued_texts(card_id).await;
                panic!(
                    "nothing matching {needle:?} ever reached this server's \
                     fake app-server\nturn texts ({}): {texts:#?}\nstill queued \
                     in the persisted snapshot ({}): {queued:#?}",
                    texts.len(),
                    queued.len(),
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// The user-message texts still sitting in this card's persisted harness queue, NEWEST session only:
    /// a dormant restart's harness inherits the old queue, so summing across rows counts messages twice.
    async fn queued_texts(&self, card_id: &str) -> Vec<String> {
        let states: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT handle_state_json FROM worker_sessions WHERE card_id = ?1 \
              ORDER BY created_at_ms DESC, id DESC LIMIT 1",
        )
        .bind(card_id)
        .fetch_all(self.repo.pool())
        .await
        .unwrap();
        let mut texts = Vec::new();
        for state in states.into_iter().flatten() {
            let parsed: Value = serde_json::from_str(&state).unwrap();
            for obs in parsed["pending_queue"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                if obs["type"] == json!("user_message") {
                    texts.push(obs["text"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        texts
    }

    /// Every text **this** server's fake app-server was handed, across threads.
    fn turn_texts(&self) -> Vec<String> {
        let mut texts = Vec::new();
        for (_thread, items) in self.state.shared_codex_appserver.started_turns_for_test() {
            for item in items {
                // An image item is not a turn text and is skipped.
                if let calm_server::codex_appserver::InputItem::Text { text } = item {
                    texts.push(text);
                }
            }
        }
        texts
    }

    /// Every observation still sitting in **any** of this card's persisted harness queues, paired with its
    /// `worker_sessions` row: the shape [`Boot::quiesce_and_clear_queue`] rewrites.
    async fn pending_observations(&self, card_id: &str) -> Vec<(String, Value)> {
        let states: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT id, handle_state_json FROM worker_sessions WHERE card_id = ?1")
                .bind(card_id)
                .fetch_all(self.repo.pool())
                .await
                .unwrap();
        let mut out = Vec::new();
        for (id, state) in states {
            let Some(state) = state else { continue };
            let parsed: Value = serde_json::from_str(&state).unwrap();
            for obs in parsed["pending_queue"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                out.push((id.clone(), obs));
            }
        }
        out
    }

    /// Shut this server's harnesses down and clear whatever they left in `card_id`'s persisted queue, so a
    /// server booted next over the same database inherits nothing. Waiting for a drain would hang (the fake never completes a turn); `abort()` is not a join, so the read-back polls.
    async fn quiesce_and_clear_queue(&self, card_id: &str, expected: &[&str]) {
        for harness in self.state.harness.drain_all_for_dev() {
            harness.shutdown().await.unwrap();
        }
        for (session_id, obs) in self.pending_observations(card_id).await {
            let text = obs["text"].as_str().unwrap_or_default();
            assert!(
                obs["type"] == json!("user_message")
                    && expected.iter().any(|needle| text.contains(needle)),
                "an observation this fixture did not send is queued on \
                 {card_id} (worker_sessions row {session_id}); clearing it \
                 would be deleting evidence, not staging state. \
                 Observation: {obs:#?}\nexpected a user_message containing one \
                 of: {expected:?}"
            );
        }
        let states: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT id, handle_state_json FROM worker_sessions WHERE card_id = ?1")
                .bind(card_id)
                .fetch_all(self.repo.pool())
                .await
                .unwrap();
        for (id, state) in states {
            let Some(state) = state else { continue };
            let mut parsed: Value = serde_json::from_str(&state).unwrap();
            parsed["pending_queue"] = json!([]);
            parsed["pending_envelope_ids"] = json!([]);
            // The queue is four parallel arrays, so "clear the queue" has to clear all four.
            parsed["pending_entry_meta"] = json!([]);
            parsed["pending_message_ids"] = json!([]);
            sqlx::query("UPDATE worker_sessions SET handle_state_json = ?1 WHERE id = ?2")
                .bind(parsed.to_string())
                .bind(id)
                .execute(self.repo.pool())
                .await
                .unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            let left = self.pending_observations(card_id).await;
            assert!(
                left.is_empty(),
                "the persisted queue for {card_id} holds {} observation(s) \
                 after the clear, so a write from this server's shut-down \
                 harness landed behind it: {left:#?}",
                left.len()
            );
            if std::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    async fn launchpad_track_id(&self) -> Option<String> {
        self.repo
            .track_get_launchpad()
            .await
            .unwrap()
            .map(|track| track.id.to_string())
    }
}

/// How `events.actor` spells one [`ActorId`]: `serde_json::to_string(&actor)`, computed rather than hand-typed.
fn stored(actor: ActorId) -> String {
    serde_json::to_string(&actor).unwrap()
}

/// How many times each needle occurs across `texts`. Occurrences, not messages: the harness joins
/// adjacent user messages into one turn text.
fn count_needles(texts: &[String], needles: &[&str]) -> Vec<usize> {
    needles
        .iter()
        .map(|needle| {
            texts
                .iter()
                .map(|text| text.matches(needle).count())
                .sum::<usize>()
        })
        .collect()
}

/// The summary prompt's prose ahead of its counts block — text in the summary prompt and nothing else;
/// the counts block is excluded because the opening briefing renders the same block.
fn summary_marker() -> &'static str {
    include_str!("../../prompts/today-summary/write.md")
        .split_once("{counts}")
        .expect("the summary fragment binds the counts block")
        .0
}

/// A phrase carried by the opening briefing and by nothing else, including the summary prompt.
const BRIEFING_MARKER: &str = "Context from the server before you start";

/// The briefing's empty-day sentence, which has no counts in it at all.
const EMPTY_DAY_MARKER: &str = "nothing has been recorded in this workspace today";

/// Only the user's words produce a `harness.user_message.enqueued` audit row: kernel context must not
/// masquerade as something the user typed.
#[tokio::test]
async fn a_launchpad_conversation_opens_with_todays_activity_before_the_users_message() {
    let b = boot().await;
    let track_id = b.user_track("busy").await;
    b.edit_report(&track_id, "something happened").await;
    let launchpad = b.ensure_launchpad().await;

    let (status, created) = b
        .create_conversation(&launchpad, "asking-about-today", "What happened today?")
        .await;
    assert_eq!(status, StatusCode::CREATED, "created={created}");
    let card_id = created["id"].as_str().unwrap().to_string();

    assert_eq!(
        b.enqueued_char_counts(&card_id).await,
        vec!["What happened today?".chars().count() as i64],
        "the server briefing is typed system context, not a second user message"
    );

    assert_eq!(
        b.delivered(
            &card_id,
            &[BRIEFING_MARKER, "What happened today?", summary_marker()],
            2,
        )
        .await,
        vec![1, 1, 0],
        "the briefing and the user's message, and nothing from the summary \
         endpoint — which was never called here"
    );

    let transcript = b.transcript_in_order(&card_id).await;
    assert!(
        transcript.contains("- report edits: 1"),
        "the briefing must carry the day's real counts, not a template: \
         {transcript}"
    );
    assert!(
        transcript.contains("- distinct tracks touched: 1"),
        "{transcript}"
    );
    let briefing_at = transcript.find(BRIEFING_MARKER);
    let question_at = transcript.find("What happened today?");
    assert!(
        briefing_at.is_some() && briefing_at < question_at,
        "the day's material has to reach the agent before the question does; \
         briefing at {briefing_at:?}, question at {question_at:?} in:\n\
         {transcript}"
    );
}

/// Both halves are load-bearing: a 201 alone is satisfied by an implementation that briefs nothing, the
/// empty-day sentence alone by one that also refuses.
#[tokio::test]
async fn an_empty_day_is_briefed_as_empty_and_still_opens_the_conversation() {
    let b = boot().await;
    // A track exists and no activity was produced on it: creating a track is not on the allowlist.
    let _quiet = b.user_track("quiet").await;
    let launchpad = b.ensure_launchpad().await;

    let (status, created) = b
        .create_conversation(&launchpad, "asking-on-a-quiet-day", "Anything for me?")
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an empty day must not take the conversation away: {created}"
    );
    let card_id = created["id"].as_str().unwrap().to_string();

    assert_eq!(
        b.delivered(
            &card_id,
            &[EMPTY_DAY_MARKER, "Anything for me?", "- report edits:"],
            2,
        )
        .await,
        vec![1, 1, 0],
        "an empty day is stated in words; a block of zeroes would be the \
         silent empty feed this ruling rejected"
    );
}

/// Asserted on `developer_instructions` at `thread/start`, the only place the two identities are
/// distinguishable, by equality against the production renderer rather than by a keyword.
#[tokio::test]
async fn the_launchpad_assistant_starts_under_the_launchpad_identity_and_others_do_not() {
    let b = boot().await;
    let ordinary = b.user_track("ordinary").await;
    let launchpad = b.ensure_launchpad().await;

    let (status, created) = b
        .create_conversation(&launchpad, "identity-launchpad", "hi")
        .await;
    assert_eq!(status, StatusCode::CREATED, "created={created}");
    let (status, created) = b
        .create_conversation(&ordinary, "identity-ordinary", "hi")
        .await;
    assert_eq!(status, StatusCode::CREATED, "created={created}");

    let instructions: Vec<String> = b
        .state
        .shared_codex_appserver
        .started_thread_params_for_test()
        .into_iter()
        .filter_map(|(developer_instructions, _, _)| developer_instructions)
        .collect();

    let launchpad_prompt =
        calm_server::planner_card::render_launchpad_assistant_prompt_for_test(&launchpad);
    let ordinary_prompt = calm_server::planner_card::render_assistant_prompt_for_test(&ordinary);
    assert!(
        instructions.contains(&launchpad_prompt),
        "the launchpad assistant must be started under the launchpad identity;          started threads carried: {instructions:#?}"
    );
    assert!(
        instructions.contains(&ordinary_prompt),
        "an ordinary track's assistant must keep the identity it always had;          started threads carried: {instructions:#?}"
    );
    // The launchpad identity must not leak onto the other track. The ASSISTANT start is located by its own
    // opening line: that track also has a planner thread start in this list.
    let ordinary_started = instructions
        .iter()
        .find(|text| {
            text.starts_with(&format!(
                "You are an assistant conversation on track `{ordinary}`"
            ))
        })
        .expect("the ordinary track's assistant thread start");
    assert!(
        ordinary_started.contains("you are a guest in a document"),
        "the guest framing is correct on an ordinary track and must survive:          {ordinary_started}"
    );
}

/// The workspace here has a launchpad *and* real activity, so a briefing that leaked onto other tracks
/// would have material to leak.
#[tokio::test]
async fn an_ordinary_tracks_conversation_carries_no_activity_briefing() {
    let b = boot().await;
    let track_id = b.user_track("ordinary").await;
    b.edit_report(&track_id, "something happened").await;
    b.ensure_launchpad().await;

    let (status, created) = b
        .create_conversation(&track_id, "ordinary-chat", "Hello there")
        .await;
    assert_eq!(status, StatusCode::CREATED, "created={created}");
    let card_id = created["id"].as_str().unwrap().to_string();

    assert_eq!(
        b.delivered(
            &card_id,
            &["Hello there", BRIEFING_MARKER, EMPTY_DAY_MARKER],
            1
        )
        .await,
        vec![1, 0, 0],
        "only the user's message; the day's window belongs to Today's track"
    );
}

/// Both halves in one case: the refusal assertions alone are satisfied by an endpoint that never works at all.
#[tokio::test]
async fn an_empty_activity_window_refuses_without_creating_or_sending_anything() {
    let b = boot().await;
    // A track exists, so "nothing happened" is not "nothing exists": `track.created` is not on the allowlist.
    let track_id = b.user_track("quiet").await;

    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("today_summary_no_activity"),
        "body={body}"
    );

    assert_eq!(
        b.launchpad_track_id().await,
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
        b.scalar("SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued'")
            .await,
        0,
        "no message may be enqueued after a refusal — asserted over the whole \
         table, because a refusal has no card to scope to and any enqueue at \
         all is the defect"
    );

    // --- and now the same endpoint, with activity ---
    b.edit_report(&track_id, "did a thing").await;
    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(b.launchpad_track_id().await.is_some());
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1
    );
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued'")
            .await,
        2,
        "with activity the same endpoint creates the conversation and sends \
         both messages — which is what makes the refusal assertions above mean \
         something"
    );
}

/// Turns are deliberately not counted: `maybe_issue_turn` drains the whole pending queue into a single
/// `turn_start`. The assertions are on delivered texts, not row counts, so a second bootstrap cannot pass as a summary.
#[tokio::test]
async fn the_first_trigger_sends_bootstrap_and_summary_and_each_later_one_sends_a_summary() {
    let b = boot().await;
    let track_id = b.user_track("busy").await;
    b.edit_report(&track_id, "first").await;

    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let card_id = body["card_id"].as_str().unwrap().to_string();
    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            2
        )
        .await,
        vec![1, 1],
        "the first trigger must deliver the bootstrap AND the summary — matched \
         by their bytes, because a length check cannot tell the bootstrap from \
         any other message of a similar size (a sibling case once 'proved' \
         bootstrap + summary while a foreign message stood in for the bootstrap)"
    );

    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            4
        )
        .await,
        vec![1, 3],
        "three triggers deliver 2 + 1 + 1 messages: exactly ONE bootstrap, ever, \
         and one summary per press. A second trigger delivering nothing is the \
         silent no-op this invariant exists to catch; a second bootstrap is the \
         race the per-card first-message claim exists to prevent"
    );
    assert_eq!(
        b.enqueued_char_counts(&card_id).await.len(),
        4,
        "…and the permanent audit rows agree with the delivered messages"
    );

    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1,
        "three triggers, one conversation"
    );

    // The caller's declared actor never reaches the message: the endpoint has no `Actor` extractor.
    assert_eq!(
        b.actors_for("harness.user_message.enqueued").await,
        vec![stored(ActorId::User)],
        "every message this endpoint sends is attributed to the human who \
         pressed the button"
    );
}

/// Mixing `workspace_key_digest(cwd)` into the key would derive a *second* conversation card the moment
/// the workspace moves, with both requests succeeding.
#[tokio::test]
async fn a_repointed_workspace_and_a_different_actor_reuse_the_one_summary_conversation() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let before = boot_with(TempDir::new().unwrap(), repo.clone(), "workspaces-old").await;
    let track_id = before.user_track("busy").await;
    before.edit_report(&track_id, "first").await;

    let (status, first) = before.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let launchpad = first["track_id"].as_str().unwrap().to_string();
    let card_id = first["card_id"].as_str().unwrap().to_string();
    assert_eq!(
        card_id,
        calm_server::routes::today_summary::today_summary_card_id_for_test(&launchpad),
        "the endpoint must land on the card the bare-key derivation names"
    );
    let old_path: String =
        sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE purpose='launchpad'")
            .fetch_one(repo.pool())
            .await
            .unwrap();

    // A declared AI actor must not change the attribution: the endpoint takes no `Actor`.
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
        sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE purpose='launchpad'")
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

/// Drives two production write paths and reads the answer out of the endpoint's own gate; the launchpad's
/// own report edits are in the system area and must never be what keeps the window non-empty.
#[tokio::test]
async fn a_real_report_edit_and_a_real_lifecycle_change_are_both_counted_as_activity() {
    let b = boot().await;
    let track_id = b.user_track("real").await;

    // Nothing yet — so the two writes below are the only reason the gate opens.
    let (status, _) = b.summary(None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    b.edit_report(&track_id, "a real edit").await;
    let (status, body) = b.summary(None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a real `track.report_edited` must count as activity: {body}"
    );

    // A fresh database for the lifecycle half, so the report edit above cannot
    // be what opens the gate.
    let b = boot().await;
    let track_id = b.user_track("real").await;
    let (status, _) = b.summary(None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, patched) = b
        .request(
            "PATCH",
            &format!("/api/tracks/{track_id}"),
            None,
            Some(json!({"lifecycle": "planning"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={patched}");
    let (status, body) = b.summary(None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a real `track.lifecycle_changed` must count as activity: {body}"
    );
}

/// Recovering through `/planner/reset` would erase the card's harness items (`reset_harness_items: true`),
/// which are the conversation itself, so the assertion is on the item count and not on the response.
#[tokio::test]
async fn a_dormant_harness_is_restarted_without_erasing_the_conversation() {
    let b = boot().await;
    let track_id = b.user_track("dormant").await;
    b.edit_report(&track_id, "something happened").await;

    let (status, first) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let card_id = first["card_id"].as_str().unwrap().to_string();
    let launchpad = first["track_id"].as_str().unwrap().to_string();

    // A turn's worth of transcript, kept by IDENTITY, not by count: the live harness writes a transcript row
    // per drained batch on its own schedule, and `harness_items.id` is `AUTOINCREMENT`.
    let sentinel = b
        .repo
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
            None,
        )
        .await
        .unwrap();

    // Everything after this point is the recovery's doing.
    let mark = b.last_event_id().await;

    // Dormancy, in the shape `ensure_live_planner_harness` tests for: no session row in an active state.
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
            "SELECT COUNT(*) FROM harness_items WHERE id = {sentinel} AND card_id = '{card_id}'"
        ))
        .await,
        1,
        "the recovery must not erase the transcript — that is the difference \
         between re-submitting a start and going through `/planner/reset`"
    );
    // Three messages: the mint's bootstrap, a SECOND bootstrap onto the restarted session (a new codex thread
    // holding none of the old context), and the summary; the first trigger's summary is stranded on the superseded session's queue.
    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            3
        )
        .await,
        vec![2, 1],
        "the recovery must deliver the SUMMARY the trigger was for, onto a \
         restarted session that was given the standing instruction first: the \
         mint's bootstrap, the restarted session's own bootstrap, and one \
         reachable summary — the first trigger's summary was stranded on the \
         superseded session's queue"
    );
    // The restart is the kernel's: `Actor("kernel").to_actor_id()` silently degrades to `User`, so
    // `ActorId::Kernel` is constructed directly.
    // `card.updated` specifically: the event `PlannerHarnessStartAdapter` writes under the operation payload's `actor`.
    let restart_actors = b
        .actors_for_card_after(mark, &card_id, "card.updated")
        .await;
    assert_eq!(
        restart_actors,
        vec![stored(ActorId::Kernel)],
        "the dormant recovery's `planner-harness-start` must be attributed to the \
         kernel — it is the one act here no human asked for, and \
         `Actor(\"kernel\").to_actor_id()` silently degrades to User, so it is \
         also the one place this module builds an `ActorId` by hand"
    );
    assert_eq!(
        b.actors_for("harness.user_message.enqueued").await,
        vec![stored(ActorId::User)],
        "…while the messages stay the human's: the two attributions must not \
         collapse into one"
    );
}

/// No production route is known to reach this state (the bootstrap ships inside the mint transaction); the
/// fixture stages it by hand and pins the predicate, not a reachable production sequence.
#[tokio::test]
async fn a_card_left_with_an_empty_transcript_still_receives_the_bootstrap() {
    let b = boot().await;
    let track_id = b.user_track("interrupted").await;
    b.edit_report(&track_id, "something happened").await;

    // Mint the card through the real endpoint, under the real key.
    let (status, first) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let card_id = first["card_id"].as_str().unwrap().to_string();
    assert_eq!(b.enqueued_char_counts(&card_id).await.len(), 2);

    // …then take away the evidence rows that mint wrote — BOTH of them, which
    // is what "an empty transcript" means to the predicate.
    let removed = sqlx::query(
        "DELETE FROM events WHERE kind = 'harness.user_message.enqueued' AND scope_card = ?1",
    )
    .bind(&card_id)
    .execute(b.repo.pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(removed, 2, "the fixture removes both rows the mint wrote");
    assert_eq!(
        b.enqueued_char_counts(&card_id).await,
        Vec::<i64>::new(),
        "the fixture must actually reproduce the empty transcript, or this \
         case proves nothing"
    );
    // The mint's own two messages were really delivered; only the audit rows are gone. Baseline them.
    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            2
        )
        .await,
        vec![1, 1],
        "the mint really did deliver both"
    );

    let (status, second) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={second}");
    assert_eq!(
        second["card_id"],
        json!(card_id),
        "still the one conversation"
    );

    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            4
        )
        .await,
        vec![2, 2],
        "the trigger that finds an empty transcript must deliver BOTH the \
         bootstrap and the summary — one more of each, matched by their bytes. \
         A card-only predicate delivers only the summary; a row-count assertion \
         cannot tell that from a foreign message plus a summary"
    );
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1,
        "recovering the message must not mint a second conversation — \
         re-running the create against an existing card is what `validate` \
         refuses, which is why the message is sent directly instead"
    );
}

/// Reachable in production: the mint enqueues the bootstrap in its own transaction, `thread/start` fails,
/// compensation's `delete_card` fails too, so the card survives with the bootstrap stranded on a `failed` session's queue.
#[tokio::test]
async fn a_stranded_bootstrap_on_a_failed_session_is_re_sent_by_the_next_trigger() {
    let b = boot().await;
    let track_id = b.user_track("stranded").await;
    b.edit_report(&track_id, "something happened").await;

    // The launchpad first, through its own endpoint, and **twice**: the first `ensure` runs under the `bootstrap`
    // key and the second under `reuse`, so an armed failure would otherwise be spent there instead of on the conversation mint.
    let mut launchpad = Value::Null;
    for _ in 0..2 {
        let (status, body) = b
            .request("POST", "/api/today/launchpad/ensure", None, None)
            .await;
        assert!(
            status.is_success(),
            "the launchpad must be materialized before the failure is armed: \
             status={status} body={body}"
        );
        launchpad = body;
    }
    let launchpad_track = launchpad["track_id"].as_str().unwrap().to_string();
    let card_id =
        calm_server::routes::today_summary::today_summary_card_id_for_test(&launchpad_track);

    sqlx::query(
        "CREATE TRIGGER fixture_block_assistant_card_delete \
           BEFORE DELETE ON cards WHEN OLD.role = 'assistant' \
           BEGIN SELECT RAISE(ABORT, 'fixture: delete_card must fail'); END",
    )
    .execute(b.repo.pool())
    .await
    .unwrap();
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();

    let (status, failed) = b.summary(None).await;
    assert!(
        status.is_server_error(),
        "a mint whose thread/start failed must not answer 2xx: status={status} \
         body={failed}"
    );
    sqlx::query("DROP TRIGGER fixture_block_assistant_card_delete")
        .execute(b.repo.pool())
        .await
        .unwrap();

    // Premise 1 — compensation really did fail, so the operation is stuck.
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start' \
               AND phase = 'stuck'"
        )
        .await,
        1,
        "premise: `delete_card` was blocked, so `plan_compensation` marks the \
         operation stuck and never re-drives it"
    );
    // Premise 2 — and left the card behind.
    assert_eq!(
        b.scalar(&format!(
            "SELECT COUNT(*) FROM cards WHERE id = '{card_id}'"
        ))
        .await,
        1,
        "premise: the card the retry re-derives survived the failed compensation"
    );
    // Premise 3 — with no active runtime on it.
    assert_eq!(
        b.scalar(&format!(
            "SELECT COUNT(*) FROM worker_sessions WHERE card_id = '{card_id}' \
               AND state IN ('starting','running','idle','turn_pending')"
        ))
        .await,
        0,
        "premise: the compensation marked the runtime failed, so the card is \
         dormant — this is what makes the surviving evidence row point at a \
         runtime nothing will ever drain"
    );
    // Premise 4 — and the stranded evidence row is there. This is the trap: it
    // is what the old predicate read as "already sent, decline".
    assert_eq!(
        b.enqueued_char_counts(&card_id).await.len(),
        1,
        "premise: the mint's transaction committed the bootstrap's enqueued row \
         before `thread/start` failed, and `events` is append-only"
    );

    // Press 2 — the self-heal.
    let (status, second) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={second}");
    assert_eq!(
        second["card_id"],
        json!(card_id),
        "the recovery must land on the derived card, not mint a second one"
    );
    assert_eq!(
        b.await_reached_appserver(&card_id, TODAY_SUMMARY_BOOTSTRAP_TEXT)
            .await,
        1,
        "the standing instruction must actually reach the agent — the first \
         attempt started no thread at all, so every occurrence here belongs to \
         the restarted session"
    );
    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            2
        )
        .await,
        vec![1, 1],
        "the trigger after a stranded bootstrap must deliver BOTH the bootstrap \
         and the summary onto the restarted session. The old predicate delivered \
         the summary alone, and `delivered` reads the card's NEWEST session queue \
         — so the copy stranded on the `failed` session is correctly invisible \
         here rather than being counted as reachable"
    );

    // Press 3 — and it does not keep re-sending.
    let (status, third) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={third}");
    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            3
        )
        .await,
        vec![1, 2],
        "the runtime has now been spoken to, so a third press sends the summary \
         only: re-sending on every trigger is what a constant-false predicate does"
    );
    assert_eq!(
        b.enqueued_char_counts(&card_id).await.len(),
        4,
        "…and the permanent record holds the stranded row plus the three \
         messages the two live presses sent"
    );
    assert_eq!(
        b.scalar("SELECT COUNT(*) FROM cards WHERE id LIKE 'conv-%'")
            .await,
        1,
        "healing the conversation must not mint a second one"
    );
}

/// `/planner/reset` takes the recovery lock, not the first-message claim, so it can replace the active
/// runtime under this caller; the predicate is one SQLite statement so it reads one snapshot.
#[tokio::test]
async fn evidence_bound_to_a_replaced_runtime_is_not_read_as_evidence() {
    let b = boot().await;
    let track_id = b.user_track("replaced").await;
    b.edit_report(&track_id, "something happened").await;

    let (status, first) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let card_id = first["card_id"].as_str().unwrap().to_string();
    // The track the predicate is scoped by is the launchpad's, the same one the
    // handler passes it — not the user track the activity came from.
    let launchpad = first["track_id"].as_str().unwrap().to_string();
    assert_eq!(
        b.enqueued_char_counts(&card_id).await.len(),
        2,
        "premise: the mint wrote the bootstrap's and the summary's evidence rows"
    );
    let r1 = b
        .active_runtime(&card_id)
        .await
        .expect("premise: the mint leaves an active runtime behind");
    assert!(
        b.enqueued_on_active_runtime(&launchpad, &card_id).await,
        "premise: while R1 is still active its own rows ARE evidence — without \
         this direction a statement that never matches anything would pass the \
         assertion below"
    );

    // The replacement, through the racing endpoint itself.
    let (status, reset) = b
        .request(
            "POST",
            &format!("/api/cards/{card_id}/planner/reset"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "reset={reset}");
    let r2 = b
        .active_runtime(&card_id)
        .await
        .expect("premise: the reset leaves a new active runtime");
    assert_ne!(
        r1, r2,
        "premise: the reset really replaced the runtime — if it did not, the \
         predicate below would be answering about R1 and prove nothing"
    );
    assert_eq!(
        b.enqueued_char_counts(&card_id).await.len(),
        2,
        "premise: `events` is append-only, so R1's rows survive the reset — they \
         are exactly what the two-read predicate was fooled by"
    );
    assert_eq!(
        b.scalar(&format!(
            "SELECT COUNT(*) FROM events \
               WHERE kind = 'harness.user_message.enqueued' \
                 AND scope_card = '{card_id}' \
                 AND json_extract(payload, '$.worker_session_id') = '{r2}'"
        ))
        .await,
        0,
        "premise: every surviving row names the REPLACED runtime; the new one \
         has not been spoken to yet"
    );

    assert!(
        !b.enqueued_on_active_runtime(&launchpad, &card_id).await,
        "the whole point: rows bound to a runtime that is no longer active are \
         not evidence, so the next trigger must re-send rather than skip"
    );

    // …and the end-to-end half: the bootstrap really is delivered again.
    let (status, second) = b.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={second}");
    // `await_reached_appserver` is deliberately NOT used here: R1's bootstrap is already in this server's turn
    // texts, so it would return at once and say nothing about the message this trigger sent.
    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            4
        )
        .await,
        vec![2, 2],
        "the trigger after the replacement must deliver BOTH the bootstrap and \
         the summary onto the new runtime — `delivered`'s queued half reads the \
         card's NEWEST session, so those two are R2's, not R1's leftovers (R1 \
         drained into its turns before the reset)"
    );
}

/// The window between `card_get` and the create is created here, not waited for; the interloper uses the
/// same key with **different text**, so the conflict is a permanent payload-hash 409, not an idempotent replay.
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
    let track_id = b.user_track("contended").await;
    b.edit_report(&track_id, "something happened").await;

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

    // Wait until the request has passed `card_get` and found nothing; planting earlier would take the
    // "card already exists" path.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while b.create_counters.snapshot().0 == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the trigger never entered the create arm; the rendezvous is not \
             where this case thinks it is"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let launchpad = b.launchpad_track_id().await.expect("ensure minted it");

    // The interloper, through the production route, same key, different text.
    let response = b
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api/tracks/{launchpad}/conversations"))
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

    let (attempts, conflicts, _) = b.create_counters.snapshot();
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
    // The interloper's own first message, then the summary — asserted by identity. The bootstrap is deliberately
    // absent (a user speaking first suppresses it), and the interloper also carries the day's opening briefing: three messages.
    assert_eq!(
        b.delivered(
            &card_id,
            &[
                TODAY_SUMMARY_BOOTSTRAP_TEXT,
                summary_marker(),
                "a different first message",
                BRIEFING_MARKER,
            ],
            3,
        )
        .await,
        vec![0, 1, 1, 1],
        "the interloper's briefing and message and the trigger's summary — and \
         no bootstrap, because something had already spoken to this card"
    );
}

/// The bootstrap arm is a read followed by a send, serialized only by the per-card first-message claim; the
/// window is open only in the empty-transcript state. Both requests park at a rendezvous before the claim, so they contend by construction.
#[tokio::test]
async fn two_concurrent_triggers_on_an_empty_transcript_deliver_one_bootstrap() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let staging = boot_with(TempDir::new().unwrap(), repo.clone(), "workspaces").await;
    let track_id = staging.user_track("contended-bootstrap").await;
    staging.edit_report(&track_id, "something happened").await;

    let (status, first) = staging.summary(None).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let card_id = first["card_id"].as_str().unwrap().to_string();
    // Decouple the two servers BEFORE `b` exists: `b` recovers its harness from this card's persisted
    // snapshot, so anything staging left in the queue would be inherited and re-delivered.
    staging
        .quiesce_and_clear_queue(&card_id, &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()])
        .await;
    // The empty-transcript state, staged exactly as the single-request case
    // stages it (and documented there): the card stays, its evidence rows go.
    let removed = sqlx::query(
        "DELETE FROM events WHERE kind = 'harness.user_message.enqueued' AND scope_card = ?1",
    )
    .bind(&card_id)
    .execute(repo.pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(removed, 2);

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let b = boot_with_rendezvouses(
        TempDir::new().unwrap(),
        repo.clone(),
        "workspaces",
        None,
        Some(barrier.clone()),
    )
    .await;

    let one = b.app.clone();
    let two = b.app.clone();
    let post = |app: axum::Router| async move {
        app.oneshot(
            Request::post("/api/today/summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    };
    let (left, right) = tokio::join!(post(one), post(two));
    assert_eq!(left, StatusCode::OK);
    assert_eq!(right, StatusCode::OK);

    let (_, _, arrivals) = b.create_counters.snapshot();
    assert_eq!(
        arrivals, 2,
        "both triggers reached the bootstrap block. This is a sanity check on \
         the fixture, NOT the proof that they raced: the counter increments \
         before the transcript is read, so it cannot witness two requests \
         seeing an empty transcript. What makes them race is the rendezvous \
         they both park at — and a missing partner would hang there rather than \
         reach this line"
    );
    // Scoped to THIS server. `expected_total` is the live enqueued-row count rather than a literal, so a run
    // that delivers a second bootstrap still SETTLES and then fails on the assertion below.
    // Something actually reached the agent: `delivered` counts queued and delivered together, so on its own it
    // settles on a run where `b` handed the app-server nothing. Only the bootstrap can be claimed: the fake never completes a turn.
    let bootstraps_reaching_the_agent = b
        .await_reached_appserver(&card_id, TODAY_SUMMARY_BOOTSTRAP_TEXT)
        .await;
    assert_eq!(
        bootstraps_reaching_the_agent, 1,
        "the standing instruction reached the agent exactly once as of the \
         moment it first appeared there — see `await_reached_appserver` for why \
         that is the final count here and what it does not cover. Two is the \
         race delivered rather than merely enqueued"
    );
    let enqueued = b.enqueued_char_counts(&card_id).await.len();
    assert_eq!(
        b.delivered(
            &card_id,
            &[TODAY_SUMMARY_BOOTSTRAP_TEXT, summary_marker()],
            enqueued
        )
        .await,
        vec![1, 2],
        "exactly ONE bootstrap across the two concurrent triggers, and one \
         summary each. Two bootstraps is the race: without the per-card claim \
         both requests read an empty transcript and both send, and the agent \
         gets the same standing instruction twice"
    );
}
