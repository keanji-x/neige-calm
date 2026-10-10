//! #2492: every Assistant of a Track reaches that Track's task workers through the terminal tools,
//! under the Planner's checks: same Track only, no typed keys into a codex task worker, a finished
//! task not writable, and no terminal card of its own. Unlike the Planner it reaches task workers
//! only: a manual Terminal card or a task-less agent card is refused. Driven over the production
//! MCP socket.
use super::task_terminal::{ECHO_WORKER, Worker, spawn_viewer, stop, worker_running};
use super::terminal_support::Harness;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    card_mcp_token_set_tx, card_with_claude_create_tx, card_with_codex_create_tx,
    card_with_terminal_create_tx, session_mcp_token_set_tx,
};
use calm_server::model::{CardRole, NewTrack, new_id, now_ms};
use serde_json::{Value, json};

/// A codex card of `role` in `track` with its own MCP token, as a conversation is minted.
pub(crate) async fn agent_token(
    h: &Harness,
    track: &str,
    role: CardRole,
) -> (String, String, String) {
    let (card, session) = (new_id(), new_id());
    let mut tx = h.sql.pool().begin().await.unwrap();
    let (_, _, token) = card_with_codex_create_tx(
        &mut tx,
        card.clone(),
        &session,
        None,
        track.to_owned().into(),
        None,
        None,
        h.root.path().to_str().unwrap().into(),
        json!({}),
        None,
        None,
        None,
        role,
        true,
        &CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    // An Assistant's token is minted by its harness start, as `planner-harness-start` persists it.
    let token = token.unwrap_or_else(|| {
        let token = calm_server::mcp_server::auth::CardMcpToken::generate();
        token.into_inner()
    });
    let hash = calm_server::mcp_server::auth::hash_token(&token);
    card_mcp_token_set_tx(&mut tx, &card, &hash).await.unwrap();
    session_mcp_token_set_tx(&mut tx, &session, &hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (card, session, token)
}

/// A terminal card in `track` that no task owns; returns its terminal id.
async fn terminal_card(h: &Harness, track: &str) -> String {
    let mut tx = h.sql.pool().begin().await.unwrap();
    let (_, terminal) = card_with_terminal_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        track.to_owned().into(),
        None,
        None,
        "/bin/sh".into(),
        h.root.path().to_str().unwrap().to_owned(),
        json!({}),
        CardRole::Worker,
        true,
        &CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
        false,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    terminal.id
}

pub(crate) async fn foreign_track(h: &Harness) -> String {
    let own = h.sql.track_get(&h.track).await.unwrap().unwrap();
    h.sql
        .track_create(NewTrack {
            template_input: None,
            area_id: own.area_id,
            title: "foreign".into(),
            sort: None,
            cwd: h.root.path().to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string()
}

async fn ok_as(h: &Harness, token: &str, name: &str, args: Value) -> Value {
    let reply = h.call_with_token(token, name, args).await;
    assert!(reply.get("error").is_none(), "{reply}");
    reply["result"]["structuredContent"].clone()
}

async fn input_as(h: &Harness, token: &str, w: &Worker, key: &str, action: Value) -> Value {
    let before = ok_as(
        h,
        token,
        "neige_terminal_read",
        json!({"attempt_id":w.task,"wait_ms":50}),
    )
    .await;
    h.call_with_token(
        token,
        "neige_terminal_input",
        json!({"attempt_id":w.task,"observation_id":before["observation_id"],
            "idempotency_key":key,"claim":true,"action":action}),
    )
    .await
}

#[tokio::test]
async fn an_assistant_reads_and_drives_a_same_track_claude_worker() {
    let h = Harness::start().await;
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    let w = worker_running(&h, "claude", &h.track, Some(ECHO_WORKER)).await;
    let shown = ok_as(
        &h,
        &assistant,
        "neige_terminal_show",
        json!({"attempt_id":w.task}),
    )
    .await;
    assert_eq!(shown["terminal_id"], w.terminal);
    assert_eq!(shown["controllable"], true);
    let typed = input_as(
        &h,
        &assistant,
        &w,
        "text",
        json!({"type":"text","text":"hello"}),
    )
    .await;
    assert!(typed.get("error").is_none(), "{typed}");
    assert_eq!(typed["result"]["structuredContent"]["outcome"], "written");
    let entered = input_as(
        &h,
        &assistant,
        &w,
        "enter",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    assert_eq!(entered["result"]["structuredContent"]["outcome"], "written");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let view = ok_as(
            &h,
            &assistant,
            "neige_terminal_read",
            json!({"attempt_id":w.task,"wait_ms":30}),
        )
        .await;
        if view["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("WORKER_REPLY:hello"))
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{view}");
    }
    ok_as(
        &h,
        &assistant,
        "neige_terminal_control",
        json!({"attempt_id":w.task,"action":"release"}),
    )
    .await;
    stop(&h, &w).await;
}

#[tokio::test]
async fn an_assistant_is_refused_across_tracks() {
    let h = Harness::start().await;
    let foreign = foreign_track(&h).await;
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    let (_, _, foreign_assistant) = agent_token(&h, &foreign, CardRole::Assistant).await;
    let own = worker_running(&h, "claude", &h.track, Some(ECHO_WORKER)).await;
    let theirs = worker_running(&h, "claude", &foreign, Some(ECHO_WORKER)).await;
    // A terminal card with no task: only the card's own Track scopes it.
    let own_card = terminal_card(&h, &h.track).await;
    let their_card = terminal_card(&h, &foreign).await;
    for (token, w, card) in [
        (&assistant, &theirs, &their_card),
        (&foreign_assistant, &own, &own_card),
    ] {
        for target in [
            json!({"attempt_id":w.task}),
            json!({"terminal_id":w.terminal}),
            json!({"terminal_id":card}),
        ] {
            for tool in ["neige_terminal_show", "neige_terminal_read"] {
                let refused = h.call_with_token(token, tool, target.clone()).await;
                assert_eq!(refused["error"]["code"], -32403, "{refused}");
                assert!(
                    refused["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("outside the caller's Track"),
                    "{refused}"
                );
            }
        }
    }
    stop(&h, &own).await;
    stop(&h, &theirs).await;
}

#[tokio::test]
async fn an_assistant_reads_but_never_types_into_a_codex_task_worker_or_a_finished_task() {
    let h = Harness::start().await;
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    let codex = worker_running(&h, "codex", &h.track, Some(ECHO_WORKER)).await;
    let shown = ok_as(
        &h,
        &assistant,
        "neige_terminal_show",
        json!({"attempt_id":codex.task}),
    )
    .await;
    assert_eq!(shown["controllable"], false);
    let refused = input_as(
        &h,
        &assistant,
        &codex,
        "codex",
        json!({"type":"text","text":"x"}),
    )
    .await;
    assert_eq!(refused["error"]["code"], -32403, "{refused}");
    assert_eq!(
        refused["error"]["data"]["refusal"], "worker_keys_refused",
        "{refused}"
    );
    // `message` is the Planner's: its header names the Planner (#2493).
    let message = h
        .call_with_token(
            &assistant,
            "neige_terminal_input",
            json!({"attempt_id":codex.task,"idempotency_key":"message",
                "action":{"type":"message","text":"x"}}),
        )
        .await;
    assert_eq!(message["error"]["code"], -32403, "{message}");
    assert!(
        message["error"]["message"]
            .as_str()
            .unwrap()
            .contains("action \"message\" is the Planner's"),
        "{message}"
    );

    let finished = worker_running(&h, "claude", &h.track, Some(ECHO_WORKER)).await;
    let before = ok_as(
        &h,
        &assistant,
        "neige_terminal_read",
        json!({"attempt_id":finished.task,"wait_ms":50}),
    )
    .await;
    sqlx::query("UPDATE tasks SET status='done',finished_at_ms=?2 WHERE id=?1")
        .bind(&finished.task)
        .bind(now_ms())
        .execute(h.sql.pool())
        .await
        .unwrap();
    let late = h
        .call_with_token(
            &assistant,
            "neige_terminal_input",
            json!({"attempt_id":finished.task,"observation_id":before["observation_id"],
                "idempotency_key":"late","claim":true,"action":{"type":"text","text":"x"}}),
        )
        .await;
    assert_eq!(late["error"]["code"], -32403, "{late}");
    stop(&h, &codex).await;
    stop(&h, &finished).await;
}

#[tokio::test]
async fn an_assistant_opens_no_terminal_card() {
    let h = Harness::start().await;
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    let cards = h.sql.cards_by_track(&h.track).await.unwrap().len();
    let open = h
        .call_with_token(
            &assistant,
            "neige_terminal_open",
            json!({"idempotency_key":"assistant-open"}),
        )
        .await;
    assert_eq!(open["error"]["code"], -32403, "{open}");
    assert!(
        open["error"]["message"]
            .as_str()
            .unwrap()
            .contains("got=Assistant"),
        "{open}"
    );
    assert_eq!(h.sql.cards_by_track(&h.track).await.unwrap().len(), cards);
}

/// A same-Track claude Worker card that no task owns, as the owner opens one; returns its terminal.
async fn manual_claude_card(h: &Harness) -> String {
    let mut tx = h.sql.pool().begin().await.unwrap();
    let (_, terminal) = card_with_claude_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        h.track.clone().into(),
        None,
        None,
        "/bin/sh".into(),
        h.root.path().to_str().unwrap().to_owned(),
        json!({}),
        None,
        None,
        None,
        "unused-settings".into(),
        new_id(),
        CardRole::Worker,
        true,
        &CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    terminal.id
}

/// The Planner drives a manual Terminal card or a task-less claude Worker card
/// (`manual_codex_and_claude_workers_are_controllable_without_a_task`); an Assistant reaches
/// neither, even with a live view: they run outside its sandbox.
#[tokio::test]
async fn an_assistant_reaches_no_terminal_without_a_current_task() {
    let h = Harness::start().await;
    let (_, _, assistant) = agent_token(&h, &h.track, CardRole::Assistant).await;
    for terminal in [
        terminal_card(&h, &h.track).await,
        manual_claude_card(&h).await,
    ] {
        spawn_viewer(&h, &terminal).await;
        let shown = h
            .ok("neige_terminal_show", json!({"terminal_id":terminal}))
            .await;
        assert_eq!(shown["task"], Value::Null);
        assert_eq!(shown["available"], true, "the Planner control: {shown}");
        assert_eq!(shown["controllable"], true, "the Planner control: {shown}");
        let planner_view = h.observe_text(&terminal, "WORKER_READY").await;
        for (tool, args) in [
            ("neige_terminal_show", json!({"terminal_id":terminal})),
            (
                "neige_terminal_read",
                json!({"terminal_id":terminal,"wait_ms":30}),
            ),
            (
                "neige_terminal_control",
                json!({"terminal_id":terminal,"action":"claim"}),
            ),
            (
                "neige_terminal_input",
                json!({"terminal_id":terminal,"observation_id":planner_view["observation_id"],
                    "idempotency_key":"manual","claim":true,
                    "action":{"type":"text","text":"x"}}),
            ),
        ] {
            let refused = h.call_with_token(&assistant, tool, args).await;
            assert_eq!(refused["error"]["code"], -32403, "{tool}: {refused}");
            assert!(
                refused["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("an Assistant reaches only task workers"),
                "{tool}: {refused}"
            );
        }
        h.state.terminal_renderer.drop_entry(&terminal).await;
    }
}
