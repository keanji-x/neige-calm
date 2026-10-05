//! Both shipped invest recipes against the real invest App behind the plugin host (#2104 P2): a
//! research Track the portfolio Track added under its symbol's current key is attested through the
//! kernel's provenance `_meta`, and every live slot of each recipe resolves, on the Track whose
//! report places it, to a unit that passes `validate_unit`.

use std::collections::BTreeMap;

use super::*;

const PLUGIN_DIR: &str = "invest";
const RECIPES: [&str; 2] = ["portfolio-recipe.md", "instrument-recipe.md"];

/// `kind -> payload` of every unit the invest App set on `track`.
async fn published(fx: &Fixture, plugin: &str, track: &str) -> BTreeMap<String, Value> {
    fx.repo
        .overlays_for("track", track)
        .await
        .unwrap()
        .into_iter()
        .filter(|o| o.plugin_id == plugin)
        .map(|o| (o.kind, o.payload))
        .collect()
}

#[tokio::test]
async fn invest_recipe_slots_resolve() {
    let fx = boot_fixture().await;
    let app = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(PLUGIN_DIR)
        .canonicalize()
        .unwrap();
    // The portfolio Track is the fixture's second Track; the research Track is the unbound one,
    // whose Planner calls the App. US:SPY is an opening position: live under key 1 once reconciled.
    let portfolio = fx.bound_track_id.clone();
    let research = fx.track_id.clone();
    let home = fx._tmp.path().join("invest-home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("invest-broker.json"),
        json!({"snapshot": {
            "identity": {"account_no": "PAPER123", "account_channel": "lb_papertrading"},
            "cash_usd": "10000", "available_cash_usd": "10000",
            "positions": {"SPY.US": {"shares": 10, "available_shares": 10}},
            "quotes": {"SPY.US": {"price": "500", "at": chrono::Utc::now().to_rfc3339(), "status": "Normal"}},
            "market_open": true, "orders": [], "fills": []
        }})
        .to_string(),
    )
    .unwrap();
    let manifest_json: Value =
        serde_json::from_str(&recipe_slots::plugin_file(PLUGIN_DIR, "manifest.json")).unwrap();
    let manifest = Manifest::parse(&manifest_json.to_string()).unwrap();
    assert!(manifest.planner_instructions.is_some());
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
                "portfolio_track_id": portfolio, "instrument_recipe_id": "recipe-instrument",
                "oauth_client_id": "fixture-client",
                "sdk_python_path": app.join("tests/broker_fixture.py").display().to_string(),
                "max_held": 4, "max_watched": 4, "max_weight_bps": 10000, "poll_seconds": 5,
                "opening_positions": "[{\"symbol\": \"US:SPY\", \"shares\": 10}]"
            }),
        })
        .await
        .unwrap();
    // The provenance `neige_track_add` stamps on the Track it adds.
    sqlx::query(
        "UPDATE tracks SET creator_track_id = ?1, creator_key = 'invest-US-SPY-1' WHERE id = ?2",
    )
    .bind(&portfolio)
    .bind(&research)
    .execute(fx.repo.pool())
    .await
    .unwrap();
    let guard = fx.plugin_host.try_lock_lifecycle(&id).unwrap();
    fx.plugin_host.registry_insert(&guard, manifest, Some(app));
    drop(guard);
    fx.plugin_host.spawn(&id).await.unwrap();

    let (token, thread) = mint_card_with_thread(
        &fx.repo,
        &fx.card_role_cache,
        research.clone().into(),
        CardRole::Planner,
    )
    .await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &token).await;
    let status = format!("plugin_{id}_instrument_status");
    // Until the first reconciliation verifies US:SPY no key is issued, so the App refuses the
    // Track as forbidden; the kernel passes the App's code through.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut call = 100;
    let view = loop {
        call += 1;
        send_frame(&mut wr, tools_call_frame(call, &status, &thread, json!({}))).await;
        let frame = recv_frame(&mut rd).await;
        if let Some(error) = frame.get("error") {
            assert_eq!(error["code"], -32403, "{frame}");
            assert!(Instant::now() < deadline, "US:SPY never went live: {frame}");
            sleep(Duration::from_millis(100)).await;
            continue;
        }
        assert_ne!(frame["result"]["isError"], true, "{frame}");
        break frame["result"]["structuredContent"].clone();
    };
    assert_eq!(view["symbol"], "US:SPY", "{view}");
    assert_eq!(view["key"], "invest-US-SPY-1", "{view}");

    for (recipe, track) in RECIPES.iter().zip([&portfolio, &research]) {
        let body = recipe_slots::plugin_file(PLUGIN_DIR, recipe);
        calm_types::report_contract::check_document(&body)
            .unwrap_or_else(|error| panic!("{recipe}: {error:?}"))
            .unwrap_or_else(|| panic!("{recipe} has no contract header"));
        let slots = recipe_slots::slots(PLUGIN_DIR, recipe);
        let deadline = Instant::now() + Duration::from_secs(30);
        let units = loop {
            let units = published(&fx, &id, track).await;
            if slots.keys().all(|kind| units.contains_key(kind)) {
                break units;
            }
            assert!(
                Instant::now() < deadline,
                "{recipe}: not every slot's unit was set on its Track: {:?}",
                units.keys().collect::<Vec<_>>()
            );
            sleep(Duration::from_millis(100)).await;
        };
        assert!(
            units.keys().eq(slots.keys()),
            "{recipe}: the units on its Track differ from its slots: {:?}",
            units.keys().collect::<Vec<_>>()
        );
        for (kind, expects) in &slots {
            calm_types::report_blocks::native_view::validate_unit(*expects, &units[kind])
                .unwrap_or_else(|error| {
                    panic!("{recipe}: {kind} is not a valid {expects:?} unit: {error}")
                });
        }
    }
    fx.plugin_host.stop(&id).await.unwrap();
}
