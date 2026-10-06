//! Host-resolved caller identity reaches local plugins; the real paper-trading App fences on it.

use std::collections::BTreeMap;

use super::*;

#[tokio::test]
async fn a_local_plugin_receives_resolved_planner_identity() {
    let fx = boot_fixture().await;
    let (token, thread) = mint_card_with_thread(
        &fx.repo,
        &fx.card_role_cache,
        fx.track_id.clone().into(),
        CardRole::Planner,
    )
    .await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &token).await;
    send_frame(
        &mut wr,
        tools_call_frame(8, EXPOSED_NAME, &thread, json!({})),
    )
    .await;
    let routed = recv_frame(&mut rd).await;
    assert!(routed.get("error").is_none(), "{routed:#?}");
    let seen = &routed["result"]["_meta"]["seen_call"];
    assert_eq!(seen["meta"]["dev.neige/caller"]["role"], "planner");
    assert_eq!(
        seen["meta"]["dev.neige/caller"]["card_id"],
        thread.strip_prefix("thread-").unwrap()
    );
    assert_eq!(seen["meta"]["dev.neige/track"]["id"], fx.track_id);
}

fn tool_text(frame: &Value) -> &str {
    frame["result"]["content"][0]["text"].as_str().unwrap_or("")
}

/// The shipped App in its `spy_cash` profile: a Planner may only plan, a Worker may only request
/// execution, and identity claimed in the request's own `_meta` is ignored.
#[tokio::test]
async fn real_spy_app_admits_planner_plan_and_worker_execution_request() {
    let fx = boot_fixture().await;
    let app = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/paper-trading")
        .canonicalize()
        .unwrap();
    let home = fx._tmp.path().join("spy-home");
    std::fs::create_dir(&home).unwrap();
    let now = chrono::Utc::now();
    std::fs::write(
        home.join("allocation-broker.json"),
        json!({"snapshot": {
            "identity": {"account_no": "PAPER123", "account_channel": "lb_papertrading"},
            "cash_usd": "10000", "available_cash_usd": "10000", "shares": 0, "available_shares": 0,
            "quote": {
                "price": "100", "at": now.to_rfc3339(), "status": "Normal",
                "calendar_date": now.date_naive().to_string(),
                "trading_day": true, "half_day": false,
                "regular_close_at": (now + chrono::Duration::hours(2)).to_rfc3339()
            },
            "market_open": true, "orders": [], "fills": []
        }})
        .to_string(),
    )
    .unwrap();
    let manifest_json: Value =
        serde_json::from_str(&recipe_slots::plugin_file("paper-trading", "manifest.json")).unwrap();
    let manifest = Manifest::parse(&manifest_json.to_string()).unwrap();
    let id = manifest.id.clone();
    fx.repo
        .plugin_install(NewPlugin {
            id: id.clone(),
            version: manifest.version.clone(),
            install_path: app.display().to_string(),
            manifest: manifest_json,
            enabled: true,
            user_config: json!({
                "profile": "spy_cash", "account_no": "PAPER123",
                "broker_home": home.display().to_string(), "owner_track_id": fx.track_id,
                "oauth_client_id": "fixture-client",
                "sdk_python_path": app.join("tests/allocation_fixture.py").display().to_string(),
                "poll_seconds": 5
            }),
        })
        .await
        .unwrap();
    let guard = fx.plugin_host.try_lock_lifecycle(&id).unwrap();
    fx.plugin_host.registry_insert(&guard, manifest, Some(app));
    drop(guard);
    fx.plugin_host.spawn(&id).await.unwrap();
    let plan = calm_server::plugin_results::registry_name(&id, "spy.plan");
    let execute = calm_server::plugin_results::registry_name(&id, "spy.execute");
    let target = json!({
        "decision_id": "caller-proof", "target_spy_bps": 0,
        "rationale": "Fixture evidence supports keeping cash.",
        "source_refs": ["neige://source/fixture"],
        "valid_until": (now + chrono::Duration::hours(1)).to_rfc3339()
    });

    // A Worker claiming Planner identity in its own request `_meta` is refused by the App.
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    let mut forged = tools_call_frame(50, &plan, &fx.thread_id, target.clone());
    forged["params"]["_meta"]["dev.neige/caller"] =
        json!({"role": "planner", "card_id": "forged", "session_id": "forged"});
    send_frame(&mut wr, forged).await;
    let refused = recv_frame(&mut rd).await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        tool_text(&refused).contains("Planner identity"),
        "{refused}"
    );

    let (token, thread) = mint_card_with_thread(
        &fx.repo,
        &fx.card_role_cache,
        fx.track_id.clone().into(),
        CardRole::Planner,
    )
    .await;
    let (mut planner_rd, mut planner_wr) = connect(&fx.socket_path).await;
    handshake(&mut planner_rd, &mut planner_wr, &token).await;
    send_frame(
        &mut planner_wr,
        tools_call_frame(51, &plan, &thread, target),
    )
    .await;
    let planned = recv_frame(&mut planner_rd).await;
    assert!(planned.get("error").is_none(), "{planned}");
    assert_ne!(planned["result"]["isError"], true, "{planned}");

    let request = json!({"decision_id": "caller-proof"});
    send_frame(
        &mut planner_wr,
        tools_call_frame(52, &execute, &thread, request.clone()),
    )
    .await;
    let refused = recv_frame(&mut planner_rd).await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(tool_text(&refused).contains("Worker identity"), "{refused}");

    send_frame(
        &mut wr,
        tools_call_frame(53, &execute, &fx.thread_id, request),
    )
    .await;
    let requested = recv_frame(&mut rd).await;
    assert!(requested.get("error").is_none(), "{requested}");
    assert_ne!(requested["result"]["isError"], true, "{requested}");
    let content = &requested["result"]["structuredContent"];
    assert_eq!(content["decisions"][0]["state"], "requested", "{requested}");
    let audit = content["journal"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "allocation_execution_requested")
        .expect("the request is journaled");
    assert_eq!(audit["body"]["caller"]["role"], "worker", "{requested}");
    assert_eq!(
        audit["body"]["caller"]["card_id"],
        fx.thread_id.strip_prefix("thread-").unwrap()
    );

    // Every unit the App publishes must pass `validate_unit` for the slot the shipped recipe
    // gives it: the kernel's read-side check of one live slot, which degrades only that slot.
    let slots = recipe_slots::slots("paper-trading", "spy-recipe.md");
    let deadline = Instant::now() + Duration::from_secs(30);
    let published: BTreeMap<String, Value> = loop {
        let published: BTreeMap<String, Value> = fx
            .repo
            .overlays_for("track", fx.track_id.as_str())
            .await
            .unwrap()
            .into_iter()
            .filter(|o| o.plugin_id == id)
            .map(|o| (o.kind, o.payload))
            .collect();
        let current = published
            .get("spy.decision_log")
            .is_some_and(|unit| unit.to_string().contains("caller-proof"));
        if current && slots.keys().all(|kind| published.contains_key(kind)) {
            break published;
        }
        assert!(
            Instant::now() < deadline,
            "not every slot's unit was published: {:?}",
            published.keys().collect::<Vec<_>>()
        );
        sleep(Duration::from_millis(100)).await;
    };
    assert!(
        published.keys().eq(slots.keys()),
        "published kinds differ from the recipe's slots: {:?}",
        published.keys().collect::<Vec<_>>()
    );
    for (kind, expects) in &slots {
        calm_types::report_blocks::native_view::validate_unit(*expects, &published[kind])
            .unwrap_or_else(|error| panic!("{kind} is not a valid {expects:?} unit: {error}"));
    }
    fx.plugin_host.stop(&id).await.unwrap();
}
