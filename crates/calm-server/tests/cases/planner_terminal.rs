//! Production MCP/operation/renderer integration; the driver shares setup only.
use crate::terminal_support::{Harness, assert_text_observation};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::card_with_codex_create_tx;
use calm_server::model::{CardRole, NewTrack, new_id};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn planner_opens_visible_terminal_and_receives_png_and_confirmed_input() {
    let h = Harness::start().await;
    let opened = h
        .call(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"open-1","title":"Planner terminal","format":"image"}),
        )
        .await;
    assert!(opened.get("error").is_none(), "{opened}");
    let meta = &opened["result"]["structuredContent"];
    let terminal = meta["terminal_id"].as_str().unwrap().to_owned();
    assert!(
        opened["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "image" && part["mimeType"] == "image/png")
    );
    let card = h
        .state
        .repo
        .card_get(meta["card_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card.track_id.as_str(), h.track);
    assert_eq!(card.kind, "terminal");
    let repeated = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"open-1","title":"Planner terminal","format":"image"}),
        )
        .await;
    assert_eq!(repeated["terminal_id"], terminal);
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let view = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    assert_eq!(
        h.input(
            &terminal,
            &view,
            "text-1",
            json!({"type":"text","text":"printf '%s%s\\n' PLANNER_ TERMINAL_OK; printf x >> terminal-input-count"})
        )
        .await["outcome"],
        "written"
    );
    let typed = h.observe_text(&terminal, "printf").await;
    let first = h
        .input(
            &terminal,
            &typed,
            "enter-1",
            json!({"type":"key","key":"Enter"}),
        )
        .await;
    assert_eq!(first["outcome"], "written");
    let repeat = h
        .input(
            &terminal,
            &typed,
            "enter-1",
            json!({"type":"key","key":"Enter"}),
        )
        .await;
    assert_eq!(
        first, repeat,
        "same request must replay its receipt without writing again"
    );
    let result = h.observe_text(&terminal, "PLANNER_TERMINAL_OK").await;
    assert!(
        result["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str() == Some("PLANNER_TERMINAL_OK")),
        "must observe standalone application result, not command echo"
    );
    assert_eq!(
        std::fs::read(h.root.path().join("terminal-input-count")).unwrap(),
        b"x"
    );
    assert_eq!(result["terminal_session_id"], view["terminal_session_id"]);
    h.stop(&terminal).await;
}

#[tokio::test]
async fn planner_terminal_refuses_unowned_and_cross_track_input() {
    let mut h = Harness::start().await;
    let open = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"no-owner"}),
        )
        .await;
    let terminal = open["terminal_id"].as_str().unwrap().to_owned();
    let denied=h.call("calm.terminal.input",json!({"terminal_id":terminal,"observation_id":open["observation_id"],"request_id":"denied","action":{"type":"text","text":"bad"}})).await;
    assert!(denied.get("error").is_some());
    let own_track = h.state.repo.track_get(&h.track).await.unwrap().unwrap();
    let foreign_track = h
        .sql
        .track_create(NewTrack {
            template_input: None,
            area_id: own_track.area_id,
            title: "foreign".into(),
            sort: None,
            cwd: h.root.path().to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut tx = h.sql.pool().begin().await.unwrap();
    let (_, _, foreign_token) = card_with_codex_create_tx(
        &mut tx,
        new_id(),
        &new_id(),
        None,
        foreign_track.id,
        None,
        None,
        h.root.path().to_str().unwrap().into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Planner,
        false,
        &CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Same database and live renderer, different authenticated Planner Track.
    h.token = foreign_token.expect("second Planner token");
    let foreign = h
        .call("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert!(foreign.get("error").is_some(), "{foreign}");
    h.stop(&terminal).await;
}

#[tokio::test]
async fn human_takeover_revokes_the_planners_saved_observation() {
    use calm_server::terminal_renderer::{ClientInputScope, ClientPumpContext, run_client_pump};
    use calm_session::{
        ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
        RenderEncoding,
    };
    let h = Harness::start().await;
    let open = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"handoff"}),
        )
        .await;
    let terminal = open["terminal_id"].as_str().unwrap().to_owned();
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let saved = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let (incoming, rx) = tokio::sync::mpsc::channel(8);
    let (tx, mut outgoing) = tokio::sync::mpsc::channel(32);
    let user = uuid::Uuid::new_v4();
    let pump = tokio::spawn(run_client_pump(
        rx,
        tx,
        ClientPumpContext {
            input_barrier: entry.handle.input_barrier.clone(),
            input_scope: ClientInputScope::InteractiveUser,
            event_rx: entry.subscribe(),
            event_tx: entry.handle.event_tx.clone(),
            render_plane: entry.handle.render_plane.clone(),
            exit: entry.exit.clone(),
            supervisor_tx: entry.handle.supervisor_tx.clone(),
            owner_registry: entry.handle.owner_registry.clone(),
            session_id: entry.handle.session_id,
            terminal_id: terminal.clone(),
        },
    ));
    incoming
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal.clone(),
            client_id: user,
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .unwrap();
    assert!(matches!(
        outgoing.recv().await,
        Some(DaemonMsg::ServerHello { .. })
    ));
    incoming.send(ClientMsg::OwnerClaim).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {if matches!(outgoing.recv().await,Some(DaemonMsg::OwnerChanged{owner_client_id:Some(id)}) if id==user){break;}}
    }).await.unwrap();
    let refused = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":saved["observation_id"],
        "request_id":"stale-owner","action":{"type":"text","text":"printf BAD"}}),
        )
        .await;
    assert!(
        refused.get("error").is_some()
            || refused["result"]["structuredContent"]["outcome"] == "refused",
        "{refused}"
    );
    let view = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert!(
        !view["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("printf BAD"))
    );
    pump.abort();
    let _ = pump.await;
    h.stop(&terminal).await;
}

#[tokio::test]
async fn repeated_input_request_never_reaches_the_terminal_twice() {
    let h = Harness::start().await;
    let open = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"input-receipts"}),
        )
        .await;
    let terminal = open["terminal_id"].as_str().unwrap().to_owned();
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let view = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    // Keep an application reading each submitted line. Unlike an idle shell,
    // it counts an empty Enter too, so an accidental duplicate is observable.
    let command = "i=0; printf '%s%s\\n' COUNTER_ READY; while IFS= read -r line; do i=$((i+1)); printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done";
    h.input(
        &terminal,
        &view,
        "program",
        json!({"type":"text","text":command}),
    )
    .await;
    let typed = h.observe_text(&terminal, "while IFS=").await;
    h.input(
        &terminal,
        &typed,
        "start",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    let ready = h.observe_text(&terminal, "COUNTER_READY").await;
    assert!(
        ready["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str() == Some("COUNTER_READY"))
    );
    let first = h
        .input(
            &terminal,
            &ready,
            "one-enter",
            json!({"type":"key","key":"Enter"}),
        )
        .await;
    assert_eq!(first["outcome"], "written");
    h.observe_text(&terminal, "COUNT:1:").await;
    assert_eq!(
        h.input(
            &terminal,
            &ready,
            "one-enter",
            json!({"type":"key","key":"Enter"})
        )
        .await,
        first
    );
    let after = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    h.input(
        &terminal,
        &after,
        "probe-text",
        json!({"type":"text","text":"PROBE"}),
    )
    .await;
    let typed = h.observe_text(&terminal, "PROBE").await;
    h.input(
        &terminal,
        &typed,
        "probe-enter",
        json!({"type":"key","key":"Enter"}),
    )
    .await;
    let result = h.observe_text(&terminal, ":PROBE").await;
    let lines = result["text"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line.as_str() == Some("COUNT:2:PROBE")),
        "duplicate Enter changed the physical input count: {result}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.as_str() == Some("COUNT:3:PROBE"))
    );
    h.stop(&terminal).await;
}

#[tokio::test]
async fn detach_releases_receipts_and_old_observations_cannot_authorize_a_new_connection() {
    let h = Harness::start().await;
    let open = h
        .ok(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"detach"}),
        )
        .await;
    let terminal = open["terminal_id"].as_str().unwrap().to_owned();
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let old = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    h.input(&terminal, &old, "one", json!({"type":"key","key":"Ctrl+U"}))
        .await;
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"detach"}),
    )
    .await;
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let fresh = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    assert_ne!(old["connection_id"], fresh["connection_id"]);
    let stale = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":old["observation_id"],
        "request_id":"one","action":{"type":"key","key":"Ctrl+U"}}),
        )
        .await;
    assert!(stale.get("error").is_some());
    let written = h
        .input(
            &terminal,
            &fresh,
            "one",
            json!({"type":"key","key":"Ctrl+U"}),
        )
        .await;
    assert_eq!(written["outcome"], "written");
    h.stop(&terminal).await;
}

#[tokio::test]
async fn default_terminal_observations_authorize_shell_input_without_images() {
    let h = Harness::start().await;
    let opened = h
        .call(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"text-default"}),
        )
        .await;
    let terminal = assert_text_observation(&opened)["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let observed = h
        .call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":100}),
        )
        .await;
    let before = assert_text_observation(&observed);
    assert_eq!(before["role"], "owner");
    assert_eq!(before["controllable"], true);
    assert!(before["control_id"].is_string());
    assert_eq!(
        h.input(
            &terminal,
            before,
            "type",
            json!({"type":"text","text":"printf '%s%s\\n' TEXT_ OBSERVATION_OK"})
        )
        .await["outcome"],
        "written"
    );
    let mut needle = "printf";
    for (request, action) in [
        ("submit", Some(json!({"type":"key","key":"Enter"}))),
        ("result", None),
    ] {
        let start = std::time::Instant::now();
        loop {
            let response = h
                .call(
                    "calm.terminal.observe",
                    json!({"terminal_id":terminal,"wait_ms":30}),
                )
                .await;
            let view = assert_text_observation(&response);
            let visible = view["text"].as_array().unwrap().iter().any(|line| {
                let line = line.as_str().unwrap();
                if action.is_some() {
                    line.contains(needle)
                } else {
                    line == needle
                }
            });
            if visible {
                assert_eq!(view["terminal_session_id"], before["terminal_session_id"]);
                if let Some(action) = action {
                    assert_eq!(
                        h.input(&terminal, view, request, action).await["outcome"],
                        "written"
                    );
                }
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "missing {needle}: {view}"
            );
        }
        needle = "TEXT_OBSERVATION_OK";
    }
    h.stop(&terminal).await;
}

#[tokio::test]
async fn terminal_formats_are_explicit_and_do_not_change_open_identity() {
    let h = Harness::start().await;
    for invalid in [json!("png"), json!("TEXT"), json!(null), json!(1)] {
        for tool in ["calm.terminal.open", "calm.terminal.observe"] {
            let args = if tool.ends_with("open") {
                json!({"request_id":"invalid-format","format":invalid})
            } else {
                json!({"terminal_id":"not-created","format":invalid})
            };
            let rejected = h.call(tool, args).await;
            assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
        }
    }
    assert_eq!(
        h.sql.cards_by_track(&h.track).await.unwrap().len(),
        1,
        "invalid formats must not create a card"
    );
    let opened = h
        .call(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"same-open","format":"text"}),
        )
        .await;
    let first = assert_text_observation(&opened);
    let terminal = first["terminal_id"].as_str().unwrap().to_owned();
    let image = h
        .call(
            "calm.terminal.open",
            json!({"program":"exec /bin/sh","request_id":"same-open","format":"image"}),
        )
        .await;
    assert!(image.get("error").is_none(), "{image}");
    let image_meta = &image["result"]["structuredContent"];
    assert_eq!(image_meta["terminal_id"], terminal);
    assert_eq!(image_meta["operation_id"], first["operation_id"]);
    assert_eq!(
        image_meta["terminal_session_id"],
        first["terminal_session_id"]
    );
    assert_eq!(image_meta["image_source"], "rmux_client_projection");
    assert!(
        image["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "image" && part["mimeType"] == "image/png")
    );
    let text = h
        .call(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"format":"text"}),
        )
        .await;
    assert_eq!(
        assert_text_observation(&text)["terminal_session_id"],
        first["terminal_session_id"]
    );
    assert_eq!(
        h.sql.cards_by_track(&h.track).await.unwrap().len(),
        2,
        "changing presentation must not create another terminal"
    );
    h.stop(&terminal).await;
}
