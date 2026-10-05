//! `_meta["dev.neige/track"]` carries the calling Track's creator provenance to local plugins,
//! from its row (#2104 K1). The forge route's twin is
//! `mcp_plugin_forge_action::forge_plugin_track_meta_carries_provenance`.

use calm_server::plugin_host::mcp::TRACK_META_KEY;

use super::*;

/// `tools_call`: an agent's call through the kernel's MCP server to a local plugin.
#[tokio::test]
async fn plugin_track_meta_carries_provenance() {
    let fx = boot_fixture().await;
    // The provenance `neige_track_add` stamps: both columns, which the table's CHECK requires.
    sqlx::query(
        "UPDATE tracks SET creator_track_id = 'track-portfolio', creator_key = 'invest-US-SPY-1' \
         WHERE id = ?1",
    )
    .bind(&fx.track_id)
    .execute(fx.repo.pool())
    .await
    .unwrap();
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
        tools_call_frame(9, EXPOSED_NAME, &thread, json!({})),
    )
    .await;
    let routed = recv_frame(&mut rd).await;
    assert!(routed.get("error").is_none(), "{routed:#?}");
    assert_eq!(
        routed["result"]["_meta"]["seen_call"]["meta"][TRACK_META_KEY],
        json!({
            "id": fx.track_id,
            "creator_track_id": "track-portfolio",
            "creator_key": "invest-US-SPY-1",
        })
    );
}
