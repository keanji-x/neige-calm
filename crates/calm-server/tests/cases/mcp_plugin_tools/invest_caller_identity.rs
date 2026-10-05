//! The real invest App behind the plugin host: host-resolved caller identity fences its portfolio
//! tools, and every unit it publishes is valid for the slot its portfolio recipe gives it.

use std::collections::BTreeMap;

use super::*;

const PLUGIN_DIR: &str = "invest";
const RECIPE: &str = "portfolio-recipe.md";

/// The App's refusal: a JSON-RPC error with its code, the message naming the served tool.
fn refusal(frame: &Value, code: i64, tool: &str) -> String {
    assert_eq!(frame["error"]["code"], code, "{frame}");
    let message = frame["error"]["message"].as_str().unwrap_or("");
    assert!(message.starts_with(&format!("{tool}: ")), "{frame}");
    message.to_string()
}

/// The port of `real_spy_app_admits_planner_plan_and_worker_execution_request` to invest: on the
/// portfolio Track a Planner may only add a decision, a Worker may only request its execution, and
/// identity claimed in the request's own `_meta` is ignored.
#[tokio::test]
async fn real_invest_app_admits_planner_decision_and_worker_execution_request() {
    let fx = boot_fixture().await;
    let app = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(PLUGIN_DIR)
        .canonicalize()
        .unwrap();
    let home = fx._tmp.path().join("invest-home");
    std::fs::create_dir(&home).unwrap();
    let now = chrono::Utc::now();
    std::fs::write(
        home.join("invest-broker.json"),
        json!({"snapshot": {
            "identity": {"account_no": "PAPER123", "account_channel": "lb_papertrading"},
            "cash_usd": "10000", "available_cash_usd": "10000", "positions": {}, "quotes": {},
            "market_open": true, "orders": [], "fills": []
        }})
        .to_string(),
    )
    .unwrap();
    let manifest_json: Value =
        serde_json::from_str(&recipe_slots::plugin_file(PLUGIN_DIR, "manifest.json")).unwrap();
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
                "account_no": "PAPER123", "broker_home": home.display().to_string(),
                "portfolio_track_id": fx.track_id, "instrument_recipe_id": "recipe-instrument",
                "oauth_client_id": "fixture-client",
                "sdk_python_path": app.join("tests/broker_fixture.py").display().to_string(),
                "max_held": 4, "max_watched": 4, "max_weight_bps": 10000, "poll_seconds": 5
            }),
        })
        .await
        .unwrap();
    let guard = fx.plugin_host.try_lock_lifecycle(&id).unwrap();
    fx.plugin_host.registry_insert(&guard, manifest, Some(app));
    drop(guard);
    fx.plugin_host.spawn(&id).await.unwrap();
    let add = calm_server::plugin_results::registry_name(&id, "decision_add");
    let execute = calm_server::plugin_results::registry_name(&id, "execution_add");
    let decision = json!({
        "decision_id": "caller-proof", "weights": [],
        "message": "Fixture evidence supports holding cash.",
        "source_refs": ["neige://source/fixture"],
        "valid_until": (now + chrono::Duration::hours(1)).to_rfc3339()
    });

    // A Worker claiming Planner identity in its own request `_meta` is refused by the App.
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    let mut forged = tools_call_frame(60, &add, &fx.thread_id, decision.clone());
    forged["params"]["_meta"]["dev.neige/caller"] =
        json!({"role": "planner", "card_id": "forged", "session_id": "forged"});
    send_frame(&mut wr, forged).await;
    let refused = recv_frame(&mut rd).await;
    assert!(refusal(&refused, -32403, &add).contains("Planner identity"));

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
        tools_call_frame(61, &add, &thread, decision),
    )
    .await;
    let added = recv_frame(&mut planner_rd).await;
    assert!(added.get("error").is_none(), "{added}");
    assert_ne!(added["result"]["isError"], true, "{added}");

    let request = json!({"decision_id": "caller-proof"});
    send_frame(
        &mut planner_wr,
        tools_call_frame(62, &execute, &thread, request.clone()),
    )
    .await;
    let refused = recv_frame(&mut planner_rd).await;
    assert!(refusal(&refused, -32403, &execute).contains("Worker identity"));

    send_frame(
        &mut wr,
        tools_call_frame(63, &execute, &fx.thread_id, request),
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
        .find(|e| e["kind"] == "execution_requested")
        .expect("the request is journaled");
    assert_eq!(audit["body"]["caller"]["role"], "worker", "{requested}");
    assert_eq!(
        audit["body"]["caller"]["card_id"],
        fx.thread_id.strip_prefix("thread-").unwrap()
    );

    // Every unit the App publishes must pass `validate_unit` for the slot the shipped recipe gives
    // it: the kernel's read-side check of one live slot, which degrades only that slot.
    let slots = recipe_slots::slots(PLUGIN_DIR, RECIPE);
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
            .get("portfolio.decision_log")
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
