//! Real socket -> identity -> plugin host regressions for explicit Assistant access.
use super::*;

async fn opted_in_fixture() -> Fixture {
    boot_fixture_with_options(FixtureOptions {
        assistant_access: true,
        duplicate_tool_name: false,
        market_sina_endpoint: None,
    })
    .await
}

#[tokio::test]
async fn duplicate_tool_declarations_cannot_diverge_discovery_and_dispatch() {
    let options = FixtureOptions {
        assistant_access: false,
        duplicate_tool_name: true,
        market_sina_endpoint: None,
    };
    // Rejecting the ambiguous manifest is a valid boundary fix. If it is
    // admitted, exercise the real socket rather than guessing which duplicate
    // discovery and dispatch will choose.
    if let Err(error) = Manifest::parse(&echo_manifest(&options).to_string()) {
        assert!(error.to_string().contains("duplicate"), "{error}");
        assert!(error.to_string().contains("exposes_tools"), "{error}");
        return;
    }
    let fx = boot_fixture_with_options(options).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.assistant_raw_token).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.assistant_thread_id)).await;
    let names = tool_names_from_response(&recv_frame(&mut rd).await);
    assert!(names.contains(&EXPOSED_NAME.to_string()));
    send_frame(
        &mut wr,
        tools_call_frame(3, EXPOSED_NAME, &fx.assistant_thread_id, json!({})),
    )
    .await;
    let response = recv_frame(&mut rd).await;
    assert!(
        response.get("error").is_none(),
        "an advertised tool must not resolve a different declaration: {response:#?}"
    );
}

#[tokio::test]
async fn assistant_opt_in_discovers_and_dispatches_with_injected_track() {
    let fx = opted_in_fixture().await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.assistant_raw_token).await;
    send_frame(&mut wr, tools_call_frame(3, EXPOSED_NAME, &fx.assistant_thread_id,
        json!({"track_id": fx.bound_track_id, "_meta": {"dev.neige/track": {"id": fx.bound_track_id}}}))).await;
    let response = recv_frame(&mut rd).await;
    assert!(response.get("error").is_none(), "{response:#?}");
    assert_eq!(
        response["result"]["_meta"]["seen_call"]["meta"]["dev.neige/track"]["id"],
        fx.track_id
    );
    send_frame(&mut wr, tools_list_frame(2, &fx.assistant_thread_id)).await;
    let names = tool_names_from_response(&recv_frame(&mut rd).await);
    assert!(
        names.contains(&EXPOSED_NAME.to_string()),
        "opted-in tool missing: {names:?}"
    );
    assert!(
        !names.contains(&COLLIDING_EXPOSED_NAME.to_string()),
        "unmarked tool leaked"
    );
    // Card-bound discovery and dispatch also work before a threadId is sent.
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}),
    )
    .await;
    assert!(
        tool_names_from_response(&recv_frame(&mut rd).await).contains(&EXPOSED_NAME.to_string())
    );
    send_frame(&mut wr, json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":EXPOSED_NAME,"arguments":{}}})).await;
    let response = recv_frame(&mut rd).await;
    assert_eq!(
        response["result"]["_meta"]["seen_call"]["meta"]["dev.neige/track"]["id"], fx.track_id,
        "{response:#?}"
    );
}

#[tokio::test]
async fn assistant_opt_in_still_denies_cross_session_and_wrong_scope() {
    let fx = opted_in_fixture().await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.assistant_raw_token).await;
    let cross =
        call_expect_error(&mut rd, &mut wr, 2, EXPOSED_NAME, Some(&fx.bound_thread_id)).await;
    assert_eq!(cross["code"], -32602);
    send_frame(&mut wr, tools_list_frame(3, &fx.bound_thread_id)).await;
    assert!(tool_names_from_response(&recv_frame(&mut rd).await).is_empty());
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.bound_assistant_raw_token).await;
    send_frame(&mut wr, tools_list_frame(4, &fx.bound_assistant_thread_id)).await;
    assert!(
        !tool_names_from_response(&recv_frame(&mut rd).await).contains(&EXPOSED_NAME.to_string())
    );
    let denied = call_expect_error(
        &mut rd,
        &mut wr,
        5,
        EXPOSED_NAME,
        Some(&fx.bound_assistant_thread_id),
    )
    .await;
    assert_eq!(denied["code"], -32601);
}

#[tokio::test]
async fn assistant_opt_in_still_requires_identity_and_a_running_plugin() {
    let fx = opted_in_fixture().await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, DAEMON_TOKEN).await;
    let missing = call_expect_error(&mut rd, &mut wr, 2, EXPOSED_NAME, None).await;
    let unknown = call_expect_error(&mut rd, &mut wr, 3, "plugin.unknown_tool", None).await;
    assert_eq!(
        missing, unknown,
        "identity failures must not reveal tool existence"
    );
    send_frame(&mut wr, tools_list_frame(4, &fx.assistant_thread_id)).await;
    assert!(
        tool_names_from_response(&recv_frame(&mut rd).await).contains(&EXPOSED_NAME.to_string())
    );
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    send_frame(&mut wr, tools_list_frame(5, &fx.assistant_thread_id)).await;
    assert!(
        !tool_names_from_response(&recv_frame(&mut rd).await).contains(&EXPOSED_NAME.to_string())
    );
    let stopped = call_expect_error(
        &mut rd,
        &mut wr,
        6,
        EXPOSED_NAME,
        Some(&fx.assistant_thread_id),
    )
    .await;
    assert_eq!(stopped["code"], -32601);
}

#[tokio::test]
async fn assistant_market_sets_lists_and_quotes_only_its_own_track() {
    let fx = boot_fixture_with_options(FixtureOptions {
        assistant_access: false,
        duplicate_tool_name: false,
        market_sina_endpoint: Some(crate::market_plugin_process::sina_server()),
    })
    .await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.assistant_raw_token).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.assistant_thread_id)).await;
    let names = tool_names_from_response(&recv_frame(&mut rd).await);
    for name in [
        "market.quote",
        "market.holdings.list",
        "market.holdings.set",
    ] {
        assert!(
            names.contains(&format!("plugin.dev-neige-market_{name}")),
            "{names:?}"
        );
    }
    send_frame(
        &mut wr,
        tools_call_frame(
            3,
            "plugin.dev-neige-market_market.holdings.set",
            &fx.assistant_thread_id,
            json!({"asset":"SH:600519","quantity":7,"track_id":fx.bound_track_id,
          "_meta":{"dev.neige/track":{"id":fx.bound_track_id}}}),
        ),
    )
    .await;
    let set = recv_frame(&mut rd).await;
    assert!(
        set.get("error").is_none() && set["result"]["isError"] != true,
        "{set:#?}"
    );
    send_frame(
        &mut wr,
        tools_call_frame(
            4,
            "plugin.dev-neige-market_market.holdings.list",
            &fx.assistant_thread_id,
            json!({}),
        ),
    )
    .await;
    let listed = recv_frame(&mut rd).await;
    assert!(
        listed.get("error").is_none() && listed["result"]["isError"] != true,
        "{listed:#?}"
    );
    assert_eq!(
        listed["result"]["structuredContent"]["holdings"][0]["asset"], "600519",
        "{listed:#?}"
    );
    assert_eq!(
        listed["result"]["structuredContent"]["holdings"][0]["qty"], 7.0,
        "{listed:#?}"
    );
    send_frame(
        &mut wr,
        tools_call_frame(
            5,
            "plugin.dev-neige-market_market.quote",
            &fx.assistant_thread_id,
            json!({"asset":"SH:600519"}),
        ),
    )
    .await;
    let quote = recv_frame(&mut rd).await;
    assert!(
        quote.get("error").is_none() && quote["result"]["isError"] != true,
        "{quote:#?}"
    );
    assert_eq!(quote["result"]["structuredContent"]["price"], 1316.94);
    assert_eq!(quote["result"]["structuredContent"]["currency"], "CNY");
    let repo = &fx.repo;
    let own = repo
        .plugin_kv_get("dev-neige-market", &format!("holdings/{}", fx.track_id))
        .await
        .unwrap();
    assert_eq!(own, Some(json!([{"asset":"SH:600519","quantity":7.0}])));
    let other = repo
        .plugin_kv_get(
            "dev-neige-market",
            &format!("holdings/{}", fx.bound_track_id),
        )
        .await
        .unwrap();
    assert!(
        other.is_none(),
        "forged argument must not change another Track"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let overlays = repo.overlays_for("track", &fx.track_id).await.unwrap();
        if let Some(holdings) = overlays.iter().find(|overlay| {
            overlay.plugin_id == "dev-neige-market" && overlay.kind == "portfolio.holdings"
        }) {
            assert_eq!(holdings.payload["rows"][0]["asset"], "600519");
            assert_eq!(holdings.payload["rows"][0]["qty"], 7.0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "market must update the caller's overlay"
        );
        sleep(Duration::from_millis(25)).await;
    }
    assert!(
        repo.overlays_for("track", &fx.bound_track_id)
            .await
            .unwrap()
            .is_empty()
    );
}
