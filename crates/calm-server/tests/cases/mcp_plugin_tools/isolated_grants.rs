use super::*;

async fn bind_isolated(fx: &Fixture, grants: &[&str]) {
    let pool = fx.repo.sqlite_pool().unwrap();
    let card: String = sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE thread_id=?1")
        .bind(&fx.thread_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let context = json!({"neige_execution":{"version":"isolated-codex-v1","workspace":"empty","plugin_tools":grants}});
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,created_at_ms,updated_at_ms) VALUES('isolated-test',?1,'isolated-test','codex','query delegated plugin',?2,'running',?3,1,1)")
        .bind(&fx.track_id).bind(context.to_string()).bind(card).execute(&pool).await.unwrap();
    let (session, card): (String, String) =
        sqlx::query_as("SELECT id,card_id FROM worker_sessions WHERE thread_id=?1")
            .bind(&fx.thread_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let request = json!({"version":"isolated-worker-v1","actor":calm_server::ids::ActorId::KernelDispatcher,"track_id":fx.track_id,"task_id":"isolated-test","idempotency_key":"isolated-test"});
    let output = json!({"result":{},"data":{"isolated_execution":{"version":"isolated-run-v1","track_id":fx.track_id,"native_token":"fixture-private-token","admission":"open","provider":{"state":"unprepared"},
        "request":{"identity":{"run_id":"isolated-operation","attempt_id":"isolated-test","card_id":card,"session_id":session},"workspace":"/workspace","developer_instructions":"fixture"}}},"target_type":"card","target_id":card});
    sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_id,target_json,payload_json,phase,tx_output_json,created_at_ms,updated_at_ms) VALUES('isolated-operation','isolated-operation','codex-isolated-worker','isolated-test','fixture','card',?1,'{}',?2,'spawn_started',?3,1,1)")
        .bind(&card).bind(request.to_string()).bind(output.to_string()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE worker_sessions SET spawn_op_id='isolated-operation' WHERE id=?1")
        .bind(session)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn isolated_worker_plugin_grants_filter_discovery_and_calls() {
    let fx = boot_fixture().await;
    bind_isolated(&fx, &[EXPOSED_NAME]).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    let names = tool_names_from_response(&list);
    assert!(names.contains(&EXPOSED_NAME.to_string()), "{list}");
    assert!(
        !names.contains(&COLLIDING_EXPOSED_NAME.to_string()),
        "ungranted plugin leaked: {list}"
    );
    send_frame(
        &mut wr,
        tools_call_frame(
            3,
            COLLIDING_EXPOSED_NAME,
            &fx.thread_id,
            json!({"probe":true}),
        ),
    )
    .await;
    let denied = recv_frame(&mut rd).await;
    assert_eq!(denied["error"]["code"], -32601, "{denied}");
    send_frame(
        &mut wr,
        tools_call_frame(4, EXPOSED_NAME, &fx.thread_id, json!({"probe":true})),
    )
    .await;
    let allowed = recv_frame(&mut rd).await;
    assert!(allowed.get("error").is_none(), "{allowed}");
    assert_eq!(
        allowed["result"]["structuredContent"],
        json!({"echo":"through-kernel","tool":TOOL_NAME})
    );
}

#[tokio::test]
async fn isolated_worker_plugin_grants_apply_without_thread_metadata_and_revoke_live() {
    let fx = boot_fixture().await;
    bind_isolated(&fx, &[EXPOSED_NAME]).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    let list = recv_frame(&mut rd).await;
    let names = tool_names_from_response(&list);
    assert!(names.contains(&EXPOSED_NAME.to_string()), "{list}");
    assert!(
        !names.contains(&COLLIDING_EXPOSED_NAME.to_string()),
        "{list}"
    );
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}),
    )
    .await;
    assert!(
        !tool_names_from_response(&recv_frame(&mut rd).await).contains(&EXPOSED_NAME.to_string())
    );
    send_frame(&mut wr, json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":EXPOSED_NAME,"arguments":{}}})).await;
    let denied = recv_frame(&mut rd).await;
    assert_eq!(denied["error"]["code"], -32601, "{denied}");
}

#[tokio::test]
async fn isolated_worker_without_grants_cannot_call_a_known_plugin() {
    let fx = boot_fixture().await;
    bind_isolated(&fx, &[]).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(
        &mut wr,
        tools_call_frame(2, EXPOSED_NAME, &fx.thread_id, json!({})),
    )
    .await;
    let denied = recv_frame(&mut rd).await;
    assert_eq!(denied["error"]["code"], -32601, "{denied}");
    // A different session/Track cannot borrow this worker's identity.
    send_frame(
        &mut wr,
        tools_call_frame(3, &fx.trusted_exposed_name, &fx.bound_thread_id, json!({})),
    )
    .await;
    assert!(recv_frame(&mut rd).await.get("error").is_some());
}

#[tokio::test]
async fn isolated_worker_malformed_grants_fail_closed() {
    let fx = boot_fixture().await;
    bind_isolated(&fx, &[EXPOSED_NAME]).await;
    sqlx::query("UPDATE tasks SET context_json=?1 WHERE id='isolated-test'")
        .bind(r#"{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty","plugin_tools":"*"}}"#)
        .execute(&fx.repo.sqlite_pool().unwrap()).await.unwrap();
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(
        &mut wr,
        tools_call_frame(2, EXPOSED_NAME, &fx.thread_id, json!({})),
    )
    .await;
    assert!(recv_frame(&mut rd).await.get("error").is_some());
}

#[tokio::test]
async fn isolated_plugin_dispatch_freezes_grants_and_replay_survives_revocation() {
    let fx = boot_fixture().await;
    fx.repo
        .card_create(calm_server::model::NewCard {
            track_id: fx.track_id.clone().into(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        })
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
    let args = json!({"name":"Research","goal":"Look up source","acceptance":"Return source result","executor":"codex","workspace":"empty","plugin_tools":[EXPOSED_NAME]});
    send_frame(
        &mut wr,
        tools_call_frame(2, "calm.task.dispatch", &thread, args.clone()),
    )
    .await;
    let first = recv_frame(&mut rd).await;
    assert!(first.get("error").is_none(), "{first}");
    let receipt = &first["result"]["structuredContent"]["receipt"];
    assert!(receipt.is_object(), "{first}");
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&fx.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    let stopped: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&fx.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert!(stopped >= before);
    send_frame(
        &mut wr,
        tools_call_frame(3, "calm.task.dispatch", &thread, args.clone()),
    )
    .await;
    let replay = recv_frame(&mut rd).await;
    assert_eq!(
        &replay["result"]["structuredContent"]["receipt"], receipt,
        "{replay}"
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&fx.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(after, stopped, "replay must not write");
    let mut changed = args.clone();
    changed["plugin_tools"] = json!([]);
    send_frame(
        &mut wr,
        tools_call_frame(4, "calm.task.dispatch", &thread, changed),
    )
    .await;
    assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32409);
    let mut new = args;
    new["name"] = json!("New research");
    send_frame(
        &mut wr,
        tools_call_frame(5, "calm.task.dispatch", &thread, new),
    )
    .await;
    assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32403);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(&fx.repo.sqlite_pool().unwrap())
            .await
            .unwrap(),
        stopped
    );
    let task = fx
        .repo
        .tasks_by_track(&fx.track_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let context: Value = serde_json::from_str(&task.context_json).unwrap();
    assert_eq!(
        context["neige_execution"]["plugin_tools"],
        json!([EXPOSED_NAME])
    );
}

#[tokio::test]
async fn isolated_worker_plugin_grants_cannot_escape_track_scope_or_include_forge_actions() {
    let fx = boot_fixture().await;
    let forge = format!("plugin.{}_execute", fx.trusted_plugin_id);
    bind_isolated(&fx, &[EXPOSED_NAME, &forge]).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    assert!(!tool_names_from_response(&list).contains(&forge), "{list}");
    send_frame(
        &mut wr,
        tools_call_frame(3, &forge, &fx.thread_id, json!({})),
    )
    .await;
    assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32601);
    // Current scope is authoritative even after a named grant was frozen.
    sqlx::query("UPDATE tracks SET plugin_scope=?1 WHERE id=?2")
        .bind(&fx.trusted_plugin_id)
        .bind(&fx.track_id)
        .execute(&fx.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    send_frame(
        &mut wr,
        tools_call_frame(4, EXPOSED_NAME, &fx.thread_id, json!({})),
    )
    .await;
    assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32601);
}

#[tokio::test]
async fn isolated_worker_plugin_grants_end_when_attempt_finishes() {
    let fx = boot_fixture().await;
    bind_isolated(&fx, &[EXPOSED_NAME]).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    sqlx::query("UPDATE tasks SET status='done' WHERE id='isolated-test'")
        .execute(&fx.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    assert!(
        !tool_names_from_response(&list).contains(&EXPOSED_NAME.to_string()),
        "{list}"
    );
    send_frame(
        &mut wr,
        tools_call_frame(3, EXPOSED_NAME, &fx.thread_id, json!({})),
    )
    .await;
    assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32601);
}

#[tokio::test]
async fn isolated_worker_grants_filter_attributed_daemon_discovery() {
    let fx = boot_fixture().await;
    bind_isolated(&fx, &[EXPOSED_NAME]).await;
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, DAEMON_TOKEN).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    let names = tool_names_from_response(&list);
    assert!(names.contains(&EXPOSED_NAME.to_string()), "{list}");
    assert!(
        !names.contains(&COLLIDING_EXPOSED_NAME.to_string()),
        "{list}"
    );
    send_frame(
        &mut wr,
        tools_call_frame(3, COLLIDING_EXPOSED_NAME, &fx.thread_id, json!({})),
    )
    .await;
    assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32601);
}

#[tokio::test]
async fn isolated_plugin_dispatch_environment_tracks_edited_current_declaration() {
    let fx = boot_fixture().await;
    let report = fx
        .repo
        .card_create(calm_server::model::NewCard {
            track_id: fx.track_id.clone().into(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        })
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
    let args = json!({"name":"Research","goal":"Look up source","acceptance":"Return source result","executor":"codex","workspace":"empty","plugin_tools":[EXPOSED_NAME]});
    send_frame(
        &mut wr,
        tools_call_frame(2, "calm.task.dispatch", &thread, args.clone()),
    )
    .await;
    let first = recv_frame(&mut rd).await;
    assert!(first.get("error").is_none(), "{first}");
    let card = fx.repo.card_get(report.id.as_str()).await.unwrap().unwrap();
    let block = card.payload["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["kind"] == "task")
        .unwrap();
    let mut payload = block["payload"].clone();
    payload["context"]["neige_execution"]["plugin_tools"] = json!([COLLIDING_EXPOSED_NAME]);
    send_frame(
        &mut wr,
        tools_call_frame(
            3,
            "calm.report.blocks.upsert",
            &thread,
            json!({"id":block["id"],"kind":"task","payload":payload,"if_rev":block["rev"]}),
        ),
    )
    .await;
    let edit = recv_frame(&mut rd).await;
    assert!(edit.get("error").is_none(), "{edit}");
    send_frame(
        &mut wr,
        tools_call_frame(4, "calm.task.dispatch", &thread, args),
    )
    .await;
    let replay = recv_frame(&mut rd).await;
    let out = &replay["result"]["structuredContent"];
    assert_eq!(
        out["requested_executor_environment"]["plugin_tools"],
        json!([EXPOSED_NAME]),
        "{replay}"
    );
    assert_eq!(
        out["current"]["executor_environment"]["plugin_tools"],
        json!([COLLIDING_EXPOSED_NAME]),
        "{replay}"
    );
    assert_eq!(
        out["receipt"],
        first["result"]["structuredContent"]["receipt"]
    );
}

#[tokio::test]
async fn isolated_worker_plugin_binding_corruption_never_becomes_legacy_access() {
    for corruption in ["missing-op", "wrong-session", "wrong-task-track"] {
        let fx = boot_fixture().await;
        bind_isolated(&fx, &[EXPOSED_NAME]).await;
        let pool = fx.repo.sqlite_pool().unwrap();
        match corruption {
            "missing-op" => {
                sqlx::query("UPDATE worker_sessions SET spawn_op_id=NULL WHERE thread_id=?1")
                    .bind(&fx.thread_id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "wrong-session" => {
                sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.isolated_execution.request.identity.session_id','different-session') WHERE id='isolated-operation'").execute(&pool).await.unwrap();
            }
            "wrong-task-track" => {
                sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms) VALUES('foreign-test',?1,'foreign-test','codex','foreign',?2,'running',1,1)")
                    .bind(&fx.bound_track_id)
                    .bind(json!({"neige_execution":{"version":"isolated-codex-v1","workspace":"empty","plugin_tools":[EXPOSED_NAME]}}).to_string())
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("UPDATE operations SET payload_json=json_set(payload_json,'$.task_id','foreign-test','$.idempotency_key','foreign-test'),tx_output_json=json_set(tx_output_json,'$.data.isolated_execution.request.identity.attempt_id','foreign-test') WHERE id='isolated-operation'")
                    .execute(&pool).await.unwrap();
            }
            _ => unreachable!(),
        }
        let (mut rd, mut wr) = connect(&fx.socket_path).await;
        handshake(&mut rd, &mut wr, &fx.raw_token).await;
        send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
        let list = recv_frame(&mut rd).await;
        assert!(list.get("error").is_some(), "{corruption}: {list}");
        send_frame(
            &mut wr,
            tools_call_frame(3, EXPOSED_NAME, &fx.thread_id, json!({})),
        )
        .await;
        let called = recv_frame(&mut rd).await;
        assert!(called.get("error").is_some(), "{corruption}: {called}");
    }
}

#[tokio::test]
async fn isolated_worker_plugin_discovery_before_startup_ack_uses_operation_binding() {
    let fx = boot_fixture().await;
    bind_isolated(&fx, &[EXPOSED_NAME]).await;
    sqlx::query(
        "UPDATE tasks SET status='dispatched',worker_card_id=NULL WHERE id='isolated-test'",
    )
    .execute(&fx.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    let list = recv_frame(&mut rd).await;
    let names = tool_names_from_response(&list);
    assert!(names.contains(&EXPOSED_NAME.to_string()), "{list}");
    assert!(
        !names.contains(&COLLIDING_EXPOSED_NAME.to_string()),
        "{list}"
    );
}
