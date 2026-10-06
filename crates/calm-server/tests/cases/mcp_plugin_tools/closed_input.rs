//! #2087 C1 (§4 closed input): a native plugin tool whose manifest schema declares
//! `additionalProperties: false` refuses an unknown top-level key at the kernel's plugin dispatch,
//! with `-32602` led by its minted name and naming every key its schema accepts, before the plugin
//! is asked. A tool with an open (or absent) schema is left to its plugin.

use super::*;

#[tokio::test]
async fn a_closed_native_plugin_tool_refuses_unknown_arguments_before_the_plugin() {
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
        tools_call_frame(5, CLOSED_EXPOSED_NAME, &thread, json!({ "zz": 1 })),
    )
    .await;
    let refused = recv_frame(&mut rd).await;
    assert_eq!(
        (
            &refused["error"]["code"],
            refused["error"]["message"].as_str()
        ),
        (
            &json!(-32602),
            Some(format!("{CLOSED_EXPOSED_NAME}: unknown argument `zz`; valid: note").as_str())
        ),
        "{refused:#?}"
    );

    // The declared key reaches the plugin under its raw name.
    send_frame(
        &mut wr,
        tools_call_frame(6, CLOSED_EXPOSED_NAME, &thread, json!({ "note": "x" })),
    )
    .await;
    let routed = recv_frame(&mut rd).await;
    assert!(routed.get("error").is_none(), "{routed:#?}");
    assert_eq!(
        routed["result"]["_meta"]["requested_name"],
        CLOSED_TOOL_NAME
    );

    // An open schema keeps the plugin's own judgment of its arguments.
    send_frame(
        &mut wr,
        tools_call_frame(7, EXPOSED_NAME, &thread, json!({ "zz": 1 })),
    )
    .await;
    let open = recv_frame(&mut rd).await;
    assert!(open.get("error").is_none(), "{open:#?}");
    assert_eq!(open["result"]["_meta"]["requested_name"], TOOL_NAME);
}
