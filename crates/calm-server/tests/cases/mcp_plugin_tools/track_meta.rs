//! `_meta["dev.neige/track"]` carries the calling Track's creator provenance to local plugins,
//! from its row, on both plugin call shapes (#2104 K1).

use calm_server::plugin_host::forge_caller::{FORGE_CALLER_META_KEY, ForgeCallerScope};
use calm_server::plugin_host::mcp::{
    InitializeMeta, KERNEL_PROTOCOL_VERSION, McpClient, TRACK_META_KEY, TrackMeta,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::*;

/// What a local forge lowerer receives under [`TRACK_META_KEY`] for `track`, read off one
/// `tools/call` frame by a duplex stub.
async fn forge_call_track_meta(track: &TrackMeta) -> Value {
    let (kernel, plugin) = tokio::io::duplex(8192);
    let (kernel_read, kernel_write) = tokio::io::split(kernel);
    let (plugin_read, mut plugin_write) = tokio::io::split(plugin);
    let stub = tokio::spawn(async move {
        let mut lines = BufReader::new(plugin_read).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let frame: Value = serde_json::from_str(&line).unwrap();
            let Some(id) = frame.get("id").cloned() else {
                continue;
            };
            let seen = (frame["method"] == "tools/call")
                .then(|| frame["params"]["_meta"][TRACK_META_KEY].clone());
            let result = match seen {
                None => json!({
                    "protocolVersion": KERNEL_PROTOCOL_VERSION,
                    "serverInfo": {"name": "stub", "version": "0"},
                    "capabilities": {}
                }),
                Some(_) => json!({"content": [], "isError": false}),
            };
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
            plugin_write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
            plugin_write.flush().await.unwrap();
            if let Some(seen) = seen {
                return seen;
            }
        }
        panic!("no tools/call frame arrived");
    });
    let client = McpClient::connect_with_auth(
        kernel_read,
        kernel_write,
        InitializeMeta {
            expected_echo: None,
            config: None,
        },
    )
    .await
    .unwrap();
    let caller = ForgeCallerScope {
        plugin_id: "plugin-a".into(),
        track_id: track.id.clone(),
        card_id: "card-a".into(),
    };
    client
        .forge_tools_call(
            "tool-a",
            json!({ FORGE_CALLER_META_KEY: {} }),
            track,
            &caller,
        )
        .await
        .unwrap();
    stub.await.unwrap()
}

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
    let expected = json!({
        "id": fx.track_id,
        "creator_track_id": "track-portfolio",
        "creator_key": "invest-US-SPY-1",
    });

    // `tools_call`: an agent's call through the kernel's MCP server to a local plugin.
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
        expected
    );

    // `forge_tools_call`: a forge lowerer's call carries the same value for the same row.
    let track = fx.repo.track_get(&fx.track_id).await.unwrap().unwrap();
    assert_eq!(
        forge_call_track_meta(&TrackMeta::from_track(&track)).await,
        expected
    );
}
