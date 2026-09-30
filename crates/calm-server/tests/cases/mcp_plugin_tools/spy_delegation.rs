//! Real App admission: legacy identity is distinct from exact Planner delegation.

use super::*;

#[tokio::test]
async fn real_spy_execution_requires_exact_isolated_delegation() {
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
            "identity":{"account_no":"PAPER123","account_channel":"lb_papertrading"},
            "cash_usd":"10000","available_cash_usd":"10000","shares":0,"available_shares":0,
            "quote":{"price":"100","at":now.to_rfc3339(),"status":"Normal"},
            "market_open":true,"orders":[],"fills":[]
        }})
        .to_string(),
    )
    .unwrap();
    let manifest_json: Value =
        serde_json::from_str(&std::fs::read_to_string(app.join("manifest.json")).unwrap()).unwrap();
    let manifest = Manifest::parse(&manifest_json.to_string()).unwrap();
    let id = manifest.id.clone();
    fx.repo.plugin_install(NewPlugin {
        id:id.clone(),version:manifest.version.clone(),install_path:app.display().to_string(),
        manifest:manifest_json,enabled:true,user_config:json!({
            "profile":"spy_cash","account_no":"PAPER123","broker_home":home.display().to_string(),
            "owner_track_id":fx.track_id,"oauth_client_id":"fixture-client",
            "sdk_python_path":app.join("tests/allocation_fixture.py").display().to_string(),"poll_seconds":5
        }),
    }).await.unwrap();
    let guard = fx.plugin_host.try_lock_lifecycle(&id).unwrap();
    fx.plugin_host.registry_insert(&guard, manifest, Some(app));
    drop(guard);
    fx.plugin_host.spawn(&id).await.unwrap();
    let plan = format!("plugin.{id}_spy.plan");
    let execute = format!("plugin.{id}_spy.execute");
    let (token, thread) = mint_card_with_thread(
        &fx.repo,
        &fx.card_role_cache,
        fx.track_id.clone().into(),
        CardRole::Planner,
    )
    .await;
    let (mut planner_rd, mut planner_wr) = connect(&fx.socket_path).await;
    handshake(&mut planner_rd, &mut planner_wr, &token).await;
    send_frame(&mut planner_wr,tools_call_frame(50,&plan,&thread,json!({
        "decision_id":"delegate-proof","target_spy_bps":0,"rationale":"Fixture evidence supports keeping cash.",
        "source_refs":["neige://source/fixture"],"valid_until":(now+chrono::Duration::hours(1)).to_rfc3339()
    }))).await;
    let planned = recv_frame(&mut planner_rd).await;
    assert!(planned.get("error").is_none(), "{planned}");
    assert_ne!(planned["result"]["isError"], true, "{planned}");
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    let mut forged = tools_call_frame(
        51,
        &execute,
        &fx.thread_id,
        json!({"decision_id":"delegate-proof"}),
    );
    forged["params"]["_meta"]["dev.neige/caller"] = json!({"role":"worker","delegated_tool":true});
    send_frame(&mut wr, forged).await;
    let refused = recv_frame(&mut rd).await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        refused["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("delegated"),
        "{refused}"
    );
    super::isolated_grants::bind_isolated(&fx, &[&execute]).await;
    send_frame(
        &mut wr,
        tools_call_frame(
            52,
            &execute,
            &fx.thread_id,
            json!({"decision_id":"delegate-proof"}),
        ),
    )
    .await;
    let executed = recv_frame(&mut rd).await;
    assert!(executed.get("error").is_none(), "{executed}");
    assert_ne!(executed["result"]["isError"], true, "{executed}");
    assert_eq!(
        executed["result"]["structuredContent"]["decisions"][0]["state"], "noop",
        "{executed}"
    );
    fx.plugin_host.stop(&id).await.unwrap();
}
