use super::*;

#[tokio::test]
async fn rebuild_template_add_replays_once() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("rebuild").await;
    let mut args = boot.args("template-rebuild");
    args.as_object_mut().unwrap().remove("recipe_id");
    args["template_id"] = json!("small-change");
    let first = boot
        .add(&planner, args.clone())
        .await
        .expect("template create");
    let id = first["track_id"].as_str().unwrap();
    let track = boot.repo.track_get(id).await.unwrap().unwrap();
    assert_eq!(track.template_id.as_deref(), Some("small-change"));
    assert!(track.admin_granted_at.is_none());
    assert_eq!(boot.add(&planner, args).await.unwrap(), first);
    assert_eq!(boot.deliveries(id).await, 1);
}

async fn catalog(boot: &Boot, caller: &Caller, name: &str, args: Value) -> Result<Value, RpcError> {
    boot.registry.lookup(name).unwrap()(boot.ctx.clone(), caller.identity.clone(), args)
        .await
        .map(calm_server::mcp_server::result::ToolResult::into_structured)
}

#[tokio::test]
async fn rebuild_catalog_matches_rest_and_refuses_worker() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("catalog").await;
    let worker = boot.worker_on(&creator).await;
    for kind in ["template", "recipe"] {
        let tool = "neige_catalog_ls";
        let page = catalog(&boot, &planner, tool, json!({"kind":kind,"limit":50}))
            .await
            .unwrap();
        let (status, rest) = send(
            boot.app.clone(),
            "GET",
            &format!("/api/track-{kind}s"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        for item in page["items"].as_array().unwrap() {
            let expected = rest
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == item["id"])
                .unwrap();
            for (key, value) in item.as_object().unwrap() {
                assert_eq!(&expected[key], value);
            }
            let detail = catalog(
                &boot,
                &planner,
                "neige_catalog_cat",
                json!({"kind":kind,"source_id":item["id"]}),
            )
            .await
            .unwrap();
            let (_, rest_detail) = send(
                boot.app.clone(),
                "GET",
                &format!("/api/track-{kind}s/{}", item["id"].as_str().unwrap()),
                None,
            )
            .await;
            for (key, value) in rest_detail.as_object().unwrap() {
                assert_eq!(&detail["source"][key], value);
            }
        }
        assert_forbidden(
            catalog(&boot, &worker, tool, json!({"kind":kind,"limit":1})).await,
            "Worker discovery",
        );
        assert_forbidden(
            catalog(
                &boot,
                &worker,
                "neige_catalog_cat",
                json!({"kind":kind,"source_id":"x"}),
            )
            .await,
            "Worker detail",
        );
        for bad in [
            json!({"kind":kind,"limit":0}),
            json!({"kind":kind,"limit":51}),
            json!({"kind":kind,"limit":1,"cursor":null}),
            json!({"kind":kind,"limit":1,"cursor":"bad"}),
        ] {
            assert_eq!(
                catalog(&boot, &planner, tool, bad).await.unwrap_err().code,
                -32602
            );
        }
    }
    let first = catalog(
        &boot,
        &planner,
        "neige_catalog_ls",
        json!({"kind":"template","limit":1}),
    )
    .await
    .unwrap();
    let second = catalog(
        &boot,
        &planner,
        "neige_catalog_ls",
        json!({"kind":"template","limit":1,"cursor":first["next_cursor"]}),
    )
    .await
    .unwrap();
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    assert_eq!(first["revision"], second["revision"]);
}

#[tokio::test]
async fn rebuild_catalog_cursor_refuses_changed_revision_and_bounds_detail() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("catalog-change").await;
    for title in ["second", "third"] {
        assert_eq!(
            send(
                boot.app.clone(),
                "POST",
                "/api/track-recipes",
                Some(json!({"title":title,"body":"# Recipe\n"}))
            )
            .await
            .0,
            StatusCode::CREATED
        );
    }
    let first = catalog(
        &boot,
        &planner,
        "neige_catalog_ls",
        json!({"kind":"recipe","limit":1}),
    )
    .await
    .unwrap();
    assert!(first["next_cursor"].is_string());
    let (_, recipe) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{}", boot.recipe_id),
        None,
    )
    .await;
    assert_eq!(send(boot.app.clone(),"PUT",&format!("/api/track-recipes/{}",boot.recipe_id),Some(json!({"title":"changed","body":"x".repeat(70_000),"if_revision":recipe["revision"]}))).await.0,StatusCode::OK);
    assert_eq!(
        catalog(
            &boot,
            &planner,
            "neige_catalog_ls",
            json!({"kind":"recipe","limit":1,"cursor":first["next_cursor"]})
        )
        .await
        .unwrap_err()
        .code,
        -32409
    );
    assert_eq!(
        catalog(
            &boot,
            &planner,
            "neige_catalog_cat",
            json!({"kind":"recipe","source_id":boot.recipe_id})
        )
        .await
        .unwrap_err()
        .code,
        -32602
    );
}

#[tokio::test]
async fn rebuild_new_fields_are_closed_nonnull_and_require_valid_combinations() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("invalid").await;
    let before = boot.track_count().await;
    let original = boot.args("invalid");
    for field in [
        "recipe_id",
        "template_id",
        "template_input",
        "cwd",
        "attach_folder",
        "allow_cross_area_cwd",
    ] {
        let mut args = original.clone();
        args[field] = Value::Null;
        assert_eq!(
            boot.add(&planner, args).await.unwrap_err().code,
            -32602,
            "{field}"
        );
    }
    for (field, value) in [
        ("template_id", json!("small-change")),
        ("template_input", json!({})),
        ("cwd", json!("relative")),
        ("attach_folder", json!(false)),
        (
            "allow_cross_area_cwd",
            json!({"folder_id":1,"area_id":"other"}),
        ),
        ("area_id", json!("other")),
        ("provider", json!("codex")),
    ] {
        let mut args = original.clone();
        args[field] = value;
        assert_eq!(
            boot.add(&planner, args).await.unwrap_err().code,
            -32602,
            "{field}"
        );
    }
    let mut args = original;
    args["cwd"] = json!("/tmp");
    args["allow_cross_area_cwd"] = json!({"folder_id":1,"area_id":"other"});
    assert!(
        boot.add(&planner, args)
            .await
            .unwrap_err()
            .message
            .contains("human-confirmation")
    );
    assert_eq!(boot.track_count().await, before);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM track_create_idempotency")
            .await,
        0
    );
}

#[tokio::test]
async fn rebuild_legacy_fingerprint_and_extended_presence_are_bound() {
    use calm_server::mcp_server::tools::track_add::{TrackAddArgs, TrackAddRequest};
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("hash").await;
    let mut legacy = boot.args("hash");
    legacy["message"] = json!("  audit note \n");
    let out = boot.add(&planner, legacy.clone()).await.unwrap();
    legacy["message"] = json!("audit note");
    let expected = calm_server::routes::idempotency_key::stable_payload_hash(&legacy).unwrap();
    let actual: String = boot
        .scalar(
            "SELECT create_request_sha256 FROM track_create_idempotency WHERE track_id=?1",
            out["track_id"].as_str().unwrap(),
        )
        .await;
    assert_eq!(
        actual, expected,
        "old five-field object must remain byte compatible"
    );
    let args: TrackAddArgs = serde_json::from_value(legacy.clone()).unwrap();
    let base = TrackAddRequest {
        creator_track_id: "x".into(),
        area_id: boot.area_id.clone(),
        create_actor: calm_server::ids::ActorId::User,
        start_actor: calm_server::ids::ActorId::User,
        planner_provider: AgentProvider::Codex,
        args,
    };
    let hash = base.fingerprint().unwrap();
    for (field, value) in [
        ("template_id", json!("small-change")),
        ("template_input", json!({"issue_url":"u"})),
        ("cwd", json!("/tmp")),
        ("attach_folder", json!(false)),
        (
            "allow_cross_area_cwd",
            json!({"folder_id":1,"area_id":"other"}),
        ),
    ] {
        let mut changed = legacy.clone();
        changed[field] = value;
        let mut request = base.clone();
        request.args = serde_json::from_value(changed).unwrap();
        assert_ne!(
            request.fingerprint().unwrap(),
            hash,
            "{field} must be bound including false presence"
        );
    }
    let mut extended = legacy.clone();
    extended["cwd"] = json!("/tmp");
    let mut request = base;
    request.args = serde_json::from_value(extended.clone()).unwrap();
    let before = request.fingerprint().unwrap();
    for (field, value) in [
        ("recipe_id", json!("other")),
        ("title", json!("other")),
        ("text", json!("other")),
        ("message", json!("other")),
        ("idempotency_key", json!("other")),
        ("attach_folder", json!(false)),
    ] {
        let mut changed = extended.clone();
        changed[field] = value;
        request.args = serde_json::from_value(changed).unwrap();
        assert_ne!(request.fingerprint().unwrap(), before, "extended {field}");
    }
    // An extra supplied false cannot replay a legacy request, even before mutable cwd validation.
    let mut changed = legacy;
    changed["cwd"] = json!("/tmp");
    changed["attach_folder"] = json!(false);
    assert_eq!(boot.add(&planner, changed).await.unwrap_err().code, -32409);
}
