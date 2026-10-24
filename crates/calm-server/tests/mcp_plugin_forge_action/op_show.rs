//! #2600: `neige_op_show` reads a forge action's recorded outcome by op_id from any card of the
//! caller's Track, in the envelope a replay of the identical call returns, and refuses an unknown,
//! foreign or non-forge op_id with one indistinguishable error.

use calm_server::operation::{OperationKey, OperationRepo};

use super::*;

const TOOL_OP_SHOW: &str = "neige_op_show";

async fn op_show_as(fx: &Fixture, raw_token: &str, thread_id: &str, id: i64, op_id: &str) -> Value {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, raw_token).await;
    send_frame(
        &mut wr,
        tools_call_frame(id, TOOL_OP_SHOW, thread_id, json!({ "op_id": op_id })),
    )
    .await;
    recv_frame(&mut rd).await
}

async fn op_show(fx: &Fixture, id: i64, op_id: &str) -> Value {
    op_show_as(fx, &fx.raw_token, &fx.thread_id, id, op_id).await
}

async fn wait_for_terminal(fx: &Fixture, op_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while fx
        ._runtime
        .operation_result(&op_id.to_owned())
        .await
        .unwrap()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "operation {op_id} did not finish"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

fn op_id_of(resp: &Value) -> String {
    resp["result"]["structuredContent"]["op_id"]
        .as_str()
        .unwrap_or_else(|| panic!("response carries an op_id: {resp:#?}"))
        .to_owned()
}

async fn env_guards() -> (
    tokio::sync::MutexGuard<'static, ()>,
    TempDir,
    EnvGuard,
    EnvGuard,
) {
    let lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results_dir = short_tempdir("fr").expect("forge results tempdir");
    let trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    (lock, results_dir, trusted, results)
}

#[tokio::test]
async fn op_show_returns_pending_then_the_replay_envelope() {
    let _env = env_guards().await;
    let fx = boot_fixture_with_role(StubMode::Parked, CardRole::Planner).await;
    let receipt = call_forge_tool(&fx, 5).await;
    assert_eq!(receipt["result"]["structuredContent"]["status"], "pending");
    let op_id = op_id_of(&receipt);

    let pending = op_show(&fx, 6, &op_id).await;
    assert!(pending.get("error").is_none(), "{pending:#?}");
    assert_eq!(
        pending["result"], receipt["result"],
        "a pending op shows the pending receipt the call returned"
    );

    wait_for_terminal(&fx, &op_id).await;
    let (ops, events) = (
        operation_count(&fx.repo).await,
        event_count(&fx.repo, "forge.scan.completed").await,
    );
    let shown = op_show(&fx, 7, &op_id).await;
    assert_eq!(
        operation_count(&fx.repo).await,
        ops,
        "op show submits nothing"
    );
    assert_eq!(
        event_count(&fx.repo, "forge.scan.completed").await,
        events,
        "op show re-emits no completion event"
    );
    let replay = call_forge_tool(&fx, 8).await;
    assert_eq!(op_id_of(&replay), op_id, "replay still finds the operation");
    assert_eq!(
        shown["result"], replay["result"],
        "op show returns the replay envelope"
    );
    let structured = &shown["result"]["structuredContent"];
    assert_eq!(structured["status"], "succeeded");
    assert_eq!(structured["parked"], false);
    assert_eq!(structured["result"]["event_kind"], "forge.scan.completed");
    assert_eq!(shown["result"]["isError"], false);
    fx.plugin_host.stop(PLUGIN_ID).await.expect("stop plugin");
}

#[tokio::test]
async fn op_show_reads_an_operation_from_another_card_of_the_same_track() {
    let _env = env_guards().await;
    let fx = boot_fixture_with_role(StubMode::Parked, CardRole::Planner).await;
    let op_id = op_id_of(&call_forge_tool(&fx, 5).await);
    wait_for_terminal(&fx, &op_id).await;

    let worker = create_card_caller(
        &fx.repo,
        &fx.card_role_cache,
        TrackId::from(fx.track_id.clone()),
        CardRole::Worker,
    )
    .await;
    let from_worker = op_show_as(&fx, &worker.raw_token, &worker.thread_id, 6, &op_id).await;
    assert!(from_worker.get("error").is_none(), "{from_worker:#?}");
    assert_eq!(
        from_worker["result"],
        op_show(&fx, 7, &op_id).await["result"],
        "a sibling card of the Track reads the same recorded outcome"
    );
    assert_eq!(
        from_worker["result"]["structuredContent"]["status"],
        "succeeded"
    );
    fx.plugin_host.stop(PLUGIN_ID).await.expect("stop plugin");
}

#[tokio::test]
async fn op_show_refuses_unknown_foreign_and_non_forge_ids_identically() {
    let _env = env_guards().await;
    let fx = boot_fixture_with_role(StubMode::Awaited, CardRole::Planner).await;
    let own = op_id_of(&call_forge_tool(&fx, 5).await);
    let own_shown = op_show(&fx, 6, &own).await;
    assert_eq!(
        own_shown["result"]["structuredContent"]["status"], "succeeded",
        "the caller reads its own Track's operation: {own_shown:#?}"
    );

    let other = create_track_caller(&fx, CardRole::Planner).await;
    let foreign = op_id_of(&call_forge_tool_for_caller(&fx, &other, 7).await);
    assert_ne!(foreign, own);
    let foreign_own_view = op_show_as(&fx, &other.raw_token, &other.thread_id, 8, &foreign).await;
    assert_eq!(
        foreign_own_view["result"]["structuredContent"]["status"], "succeeded",
        "the other Track reads its own operation: {foreign_own_view:#?}"
    );

    let non_forge = SqlxOperationRepo::new(fx.repo.db().clone())
        .insert_operation(
            "terminal-create",
            OperationKey {
                operation_key: calm_server::model::new_id(),
                idempotency_key: None,
                payload_hash: "op-show-non-forge".into(),
            },
            json!({ "track_id": fx.track_id }),
        )
        .await
        .expect("insert a non-forge operation on the caller's Track");

    let mut refusals = Vec::new();
    for (id, op_id) in [
        (9, "no-such-op"),
        (10, foreign.as_str()),
        (11, non_forge.as_str()),
    ] {
        let resp = op_show(&fx, id, op_id).await;
        assert!(
            resp.get("result").is_none(),
            "{op_id} must be refused: {resp:#?}"
        );
        let error = &resp["error"];
        assert_eq!(error["code"], -32404, "{resp:#?}");
        assert_eq!(
            error["message"],
            format!("neige_op_show: no operation {op_id} on this track")
        );
        let mut normalized = error.clone();
        normalized["message"] = json!(error["message"].as_str().unwrap().replace(op_id, "<op_id>"));
        refusals.push(normalized);
    }
    assert!(
        refusals.windows(2).all(|pair| pair[0] == pair[1]),
        "unknown, foreign and non-forge refusals must be indistinguishable: {refusals:#?}"
    );
    fx.plugin_host.stop(PLUGIN_ID).await.expect("stop plugin");
}

async fn insert_operation(fx: &Fixture, kind: &str, track_id: &str) -> String {
    SqlxOperationRepo::new(fx.repo.db().clone())
        .insert_operation(
            kind,
            OperationKey {
                operation_key: calm_server::model::new_id(),
                idempotency_key: None,
                payload_hash: "op-show-out-of-scope".into(),
            },
            json!({ "track_id": track_id }),
        )
        .await
        .expect("insert an operation")
}

/// An out-of-scope row is filtered in the lookup, never decoded: one whose stored JSON no longer
/// decodes is refused exactly like an absent id, not with an internal error.
#[tokio::test]
async fn op_show_refuses_undecodable_out_of_scope_rows_like_an_absent_id() {
    let _env = env_guards().await;
    let fx = boot_fixture_with_role(StubMode::Awaited, CardRole::Planner).await;
    let foreign = insert_operation(&fx, FORGE_ACTION_KIND, "another-track").await;
    let non_forge = insert_operation(&fx, "terminal-create", &fx.track_id).await;
    for op_id in [&foreign, &non_forge] {
        // Valid JSON, so the table's CHECKs pass, that no longer decodes as a `TxOutput`.
        sqlx::query("UPDATE operations SET tx_output_json = '\"undecodable\"' WHERE id = ?1")
            .bind(op_id)
            .execute(fx.repo.pool())
            .await
            .expect("corrupt the stored operation");
    }

    let mut refusals = Vec::new();
    for (id, op_id) in [
        (5, "no-such-op"),
        (6, foreign.as_str()),
        (7, non_forge.as_str()),
    ] {
        let resp = op_show(&fx, id, op_id).await;
        assert!(
            resp.get("result").is_none(),
            "{op_id} must be refused: {resp:#?}"
        );
        // The overflow view must apply the same scope filter before decoding, too.
        let (mut rd, mut wr) = connect(&fx.socket_path).await;
        handshake(&mut rd, &mut wr, &fx.raw_token).await;
        send_frame(
            &mut wr,
            tools_call_frame(
                id + 100,
                "neige_track_cat",
                &fx.thread_id,
                json!({"path":format!("ops/{op_id}/result")}),
            ),
        )
        .await;
        assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32404);
        let mut normalized = resp["error"].clone();
        normalized["message"] = json!(
            normalized["message"]
                .as_str()
                .unwrap_or_default()
                .replace(op_id, "<op_id>")
        );
        refusals.push(normalized);
    }
    assert_eq!(refusals[0]["code"], -32404, "{refusals:#?}");
    assert_eq!(
        refusals[0]["message"],
        "neige_op_show: no operation <op_id> on this track"
    );
    assert!(
        refusals.windows(2).all(|pair| pair[0] == pair[1]),
        "undecodable foreign and non-forge rows must be refused like an absent id: {refusals:#?}"
    );
    fx.plugin_host.stop(PLUGIN_ID).await.expect("stop plugin");
}

/// A parked `replay_any_version` call that reuses an operation stored by another program version
/// names the completion event of the STORED payload, which is the event that will be emitted, not
/// the one the replaying call declares.
#[tokio::test]
async fn parked_any_version_replay_names_its_stored_completion_event() {
    let _env = env_guards().await;
    let fx = boot_fixture_with_role(StubMode::Parked, CardRole::Planner).await;
    let release = fx._tmp.path().join("release");
    let argv = json!([
        "/bin/sh",
        "-c",
        "while [ ! -f \"$1\" ]; do sleep 0.05; done",
        "sh",
        release.display().to_string(),
    ]);
    let first = call_forge_tool_with_args(&fx, 5, json!({ "overrides": { "argv": argv } })).await;
    let receipt = &first["result"]["structuredContent"];
    assert_eq!(receipt["status"], "pending", "{first:#?}");
    assert_eq!(receipt["completion_event"], "forge.scan.completed");
    let op_id = op_id_of(&first);

    let upgraded = json!({
        "event": { "event_kind": "forge.issue.read", "fields": {} },
        "overrides": { "argv": argv },
    });
    let incompatible = call_forge_tool_with_args(&fx, 6, upgraded.clone()).await;
    assert_eq!(
        incompatible["result"]["isError"], true,
        "the upgraded payload differs from the stored one: {incompatible:#?}"
    );

    let mut any_version = upgraded;
    any_version["overrides"]["replay_any_version"] = json!(true);
    let replay = call_forge_tool_with_args(&fx, 7, any_version).await;
    assert_eq!(op_id_of(&replay), op_id, "the stored operation is reused");
    assert_eq!(
        replay["result"], first["result"],
        "the replay names the stored completion event: {replay:#?}"
    );
    assert_eq!(op_show(&fx, 8, &op_id).await["result"], first["result"]);

    std::fs::write(&release, "").expect("release the parked action");
    wait_for_terminal(&fx, &op_id).await;
    fx.plugin_host.stop(PLUGIN_ID).await.expect("stop plugin");
}

#[tokio::test]
async fn op_show_failed_keeps_the_replay_error_shape() {
    let _env = env_guards().await;
    let fx = boot_fixture_with_role(StubMode::Parked, CardRole::Planner).await;
    std::fs::remove_dir_all(fx._tmp.path().join("track-cwd"))
        .expect("delete track cwd before parked forge action");
    let failed = call_forge_tool(&fx, 5).await;
    assert_eq!(failed["result"]["isError"], true, "{failed:#?}");
    let op_id = op_id_of(&failed);

    let shown = op_show(&fx, 6, &op_id).await;
    assert!(shown.get("error").is_none(), "{shown:#?}");
    assert_eq!(
        shown["result"], failed["result"],
        "op show returns the failure envelope the call returned"
    );
    let structured = &shown["result"]["structuredContent"];
    assert_eq!(structured["outcome"], "failed");
    assert_eq!(structured["status"], "failed");
    assert_eq!(structured["parked"], false);
    assert!(
        structured["last_error"]
            .as_str()
            .unwrap_or_default()
            .contains("cwd_lease"),
        "{shown:#?}"
    );
    fx.plugin_host.stop(PLUGIN_ID).await.expect("stop plugin");
}

#[tokio::test]
async fn op_show_output_inline_and_virtual_continuation_are_track_scoped() {
    let _env = env_guards().await;
    for size in [10, 5000, 20000] {
        let fx = boot_fixture_with_role(StubMode::Awaited, CardRole::Planner).await;
        let text = if size == 5000 {
            "<".repeat(size)
        } else {
            "界".repeat(size)
        };
        let resp = call_forge_tool_with_args(
            &fx,
            5,
            json!({
                "event":{"event_kind":"forge.issue.searched","fields":{}},
                "overrides":{"argv":["/bin/sh","-c","printf '%s' \"$1\"","sh",text]}
            }),
        )
        .await;
        let id = op_id_of(&resp);
        assert_eq!(
            resp["result"]["structuredContent"]["status"], "succeeded",
            "{resp}"
        );
        let shown = op_show(&fx, 6, &id).await;
        assert_eq!(shown["result"], resp["result"]);
        if size == 10 {
            assert_eq!(
                shown["result"]["structuredContent"]["result"]["stdout"],
                text
            );
        } else {
            assert!(
                shown["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .len()
                    <= 16 * 1024
            );
            assert_eq!(
                shown["result"]["structuredContent"]["result"]["output_ref"],
                format!("ops/{id}/result")
            );
        }
        let (mut rd, mut wr) = connect(&fx.socket_path).await;
        handshake(&mut rd, &mut wr, &fx.raw_token).await;
        send_frame(
            &mut wr,
            tools_call_frame(
                7,
                "neige_track_cat",
                &fx.thread_id,
                json!({"path": if size == 10 { format!("ops/{id}/result") } else { shown["result"]["structuredContent"]["result"]["output_ref"].as_str().unwrap().to_owned() }}),
            ),
        )
        .await;
        let full = recv_frame(&mut rd).await;
        assert_eq!(
            full["result"]["structuredContent"]["content"], text,
            "{full}"
        );
        let foreign = create_track_caller(&fx, CardRole::Planner).await;
        let (mut rd, mut wr) = connect(&fx.socket_path).await;
        handshake(&mut rd, &mut wr, &foreign.raw_token).await;
        send_frame(
            &mut wr,
            tools_call_frame(
                8,
                "neige_track_cat",
                &foreign.thread_id,
                json!({"path":format!("ops/{id}/result")}),
            ),
        )
        .await;
        assert_eq!(recv_frame(&mut rd).await["error"]["code"], -32404);
        if size == 10 {
            let artifact = shown["result"]["structuredContent"]["result"]["event"]["artifact_path"]
                .as_str()
                .unwrap();
            std::fs::remove_file(artifact).unwrap();
            sqlx::query(
                "UPDATE operations SET tx_output_json = json_remove(
                    json_set(tx_output_json, '$.result.event.head_sha', ?2), '$.result.stdout')
                 WHERE id = ?1",
            )
            .bind(&id)
            .bind("界".repeat(20000))
            .execute(fx.repo.pool())
            .await
            .unwrap();
            let missing = op_show(&fx, 9, &id).await;
            assert_eq!(
                missing["result"]["structuredContent"]["result"]["output_unavailable"],
                true
            );
            assert!(
                missing["result"]["structuredContent"]["result"]
                    .get("output_ref")
                    .is_none()
            );
            assert!(missing.to_string().len() < 30000);
        }
        fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    }
}
