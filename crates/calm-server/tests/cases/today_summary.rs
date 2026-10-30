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
use calm_server::db::RepoRead;
use calm_server::{
    card_role_cache::CardRoleCache,
    db::{Repo, sqlite::SqlxRepo},
    event::EventBus,
    plugin_host::{PluginHost, PluginRegistry},
    routes,
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
    _tmp: Arc<TempDir>,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    boot_with(TempDir::new().unwrap(), repo, "workspaces").await
}

// Summary-positive generic checks need the actual recovered boot capability.
async fn boot_production() -> Boot {
    use clap::Parser;
    let tmp = Arc::new(TempDir::new().unwrap());
    let mut cfg = calm_server::config::Config::parse_from(["calm-server"]);
    cfg.db_url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("summary.db").display()
    );
    cfg.data_dir = Some(tmp.path().join("data"));
    cfg.workspace_root = Some(tmp.path().join("workspaces"));
    cfg.plugins_dir = Some(tmp.path().join("plugins"));
    cfg.plugins_data_dir = Some(tmp.path().join("plugins-data"));
    cfg.auth_username = Some("fixture-owner".into());
    cfg.auth_password = Some("fixture-password".into());
    cfg.auth_dev_autologin = false;
    cfg.codex_bin = tmp
        .path()
        .join("no-real-codex")
        .to_string_lossy()
        .into_owned();
    cfg.claude_bin = tmp
        .path()
        .join("no-real-claude")
        .to_string_lossy()
        .into_owned();
    let (state, auth) = AppState::boot(&cfg).await.unwrap().into_parts();
    let app =
        crate::support::authenticated_router::application_with_auth(state.clone(), auth).await;
    let repo = Arc::new(SqlxRepo::open(&cfg.db_url).await.unwrap());
    Boot {
        app,
        state,
        repo,
        _tmp: tmp,
    }
}

async fn boot_with(tmp: impl Into<Arc<TempDir>>, repo: Arc<SqlxRepo>, root_name: &str) -> Boot {
    let tmp = tmp.into();
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
    let app = routes::router()
        .layer(axum::middleware::from_fn_with_state(
            calm_server::auth::AuthState::new(calm_server::auth::AuthConfig {
                username: None,
                password: None,
                dev_autologin: true,
                display_name: "Owner".into(),
            }),
            calm_server::auth::require_session,
        ))
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
                if let provider::InputItem::Text { text } = item {
                    texts.push(text);
                }
            }
        }
        texts
    }

    async fn launchpad_track_id(&self) -> Option<String> {
        self.repo
            .track_get_launchpad()
            .await
            .unwrap()
            .map(|track| track.id.to_string())
    }
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

/// Only the user's words become a canonical Human message; briefing is frozen system context.
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

    let accepted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM conversation_messages WHERE card_id=?1")
            .bind(&card_id)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        accepted, 1,
        "the briefing is system context, not a second Human message"
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
        .create_conversation(
            &launchpad,
            "identity-launchpad",
            "launchpad identity question",
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "created={created}");
    let (status, created) = b
        .create_conversation(&ordinary, "identity-ordinary", "ordinary identity question")
        .await;
    assert_eq!(status, StatusCode::CREATED, "created={created}");

    b.delivered(
        created["id"].as_str().unwrap(),
        &["launchpad identity question", "ordinary identity question"],
        2,
    )
    .await;
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
    let b = boot_production().await;
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
        b.scalar("SELECT COUNT(*) FROM events WHERE kind = 'assistant.input_requested'")
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
        b.scalar("SELECT COUNT(*) FROM events WHERE kind = 'assistant.input_requested'")
            .await,
        2,
        "with activity the same endpoint creates the conversation and sends \
         both messages — which is what makes the refusal assertions above mean \
         something"
    );
}

/// Drives a production write path and reads the answer out of the endpoint's own gate; the launchpad's
/// own report edits are in the system area and must never be what keeps the window non-empty.
#[tokio::test]
async fn a_real_report_edit_is_counted_as_activity() {
    let b = boot_production().await;
    let track_id = b.user_track("real").await;

    // Nothing yet — so the write below is the only reason the gate opens.
    let (status, _) = b.summary(None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    b.edit_report(&track_id, "a real edit").await;
    let (status, body) = b.summary(None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a real `track.report_edited` must count as activity: {body}"
    );
}
