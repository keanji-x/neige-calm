//! Extra Track authority uses real tool handlers, discovery and the authenticated ask answer route.
use super::*;
use crate::mcp_server::registry::{CardIdentity, ConnectionIdentity};
use crate::mcp_server::transport::tool_descriptors_for_connection;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn owned(fx: &Fixture) -> ToolCallIdentity {
    fx.host
        .enable(crate::builtin_plugins::dev::PLUGIN_ID)
        .await
        .unwrap();
    let who = fx.identity(CardRole::Planner).await;
    fx.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET plugin_scope=? WHERE id=?")
                .bind(crate::builtin_plugins::dev::PLUGIN_ID)
                .bind(who.track_id.as_ref().unwrap()),
        )
        .await
        .unwrap();
    who
}
async fn invoke(
    fx: &Fixture,
    who: &ToolCallIdentity,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, crate::mcp_server::framing::RpcError> {
    let r = crate::mcp_server::build_default_registry();
    r.lookup(name).unwrap()(fx.ctx.clone(), who.clone(), args)
        .await
        .map(|v| serde_json::to_value(v).unwrap()["structuredContent"].clone())
}
async fn access(fx: &Fixture, who: &ToolCallIdentity, enabled: bool) -> serde_json::Value {
    invoke(
        fx,
        who,
        "neige_access_ask",
        json!({"action":"request","plugin_id":"calendar","enabled":enabled}),
    )
    .await
    .unwrap()
}
async fn answer(
    fx: &Fixture,
    track: &str,
    ask: i64,
    answers: serde_json::Value,
    actor: &str,
) -> u16 {
    let response = fx
        .http_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/asks/{ask}/answer"))
                .header("content-type", "application/json")
                .header("x-calm-actor", actor)
                .body(Body::from(json!({"answers":answers}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    if status != 204 {
        eprintln!("answer {status}: {}", String::from_utf8_lossy(&body));
    }
    status
}
async fn visible(fx: &Fixture, who: &ToolCallIdentity) -> Vec<String> {
    let bound = ConnectionIdentity::CardBound(CardIdentity {
        mcp_token_hash: None,
        card_id: who.card_id.clone().into(),
        role: who.role,
        provider: who.provider.clone(),
        session_id: who.session_id.clone(),
        track_id: who.track_id.clone(),
        area_id: who.area_id.clone(),
    });
    tool_descriptors_for_connection(
        &fx.ctx,
        &crate::mcp_server::build_default_registry(),
        &bound,
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|d| d.name)
    .collect()
}
async fn status(fx: &Fixture, who: &ToolCallIdentity) -> serde_json::Value {
    invoke(fx, who, "neige_access_ask", json!({"action":"status"}))
        .await
        .unwrap()["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["plugin_id"] == "calendar")
        .unwrap()
        .clone()
}
async fn calendar(
    fx: &Fixture,
    who: &ToolCallIdentity,
) -> Result<serde_json::Value, crate::mcp_server::framing::RpcError> {
    invoke(fx, who, "plugin_calendar_add", json!(request())).await
}

#[tokio::test]
async fn track_plugin_access_roundtrip_is_durable_isolated_and_revocable() {
    let fx = Fixture::new().await;
    let who = owned(&fx).await;
    let other = owned(&fx).await;
    let track = who.track_id.as_deref().unwrap();
    fx.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET parent_track_id=? WHERE id=?")
                .bind(track)
                .bind(other.track_id.as_ref().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(status(&fx, &who).await["authorization"], "not_authorized");
    assert_eq!(calendar(&fx, &who).await.unwrap_err().code, -32601);
    assert!(
        !visible(&fx, &who)
            .await
            .iter()
            .any(|n| n == "plugin_calendar_add")
    );
    let ask = access(&fx, &who, true).await["ask_id"].as_i64().unwrap();
    assert_eq!(
        access(&fx, &who, true).await["ask_id"],
        ask,
        "pending request is deduplicated"
    );
    assert_eq!(
        calendar(&fx, &who).await.unwrap_err().code,
        -32601,
        "asking alone grants nothing"
    );
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":0}]), "user").await,
        204
    );
    assert_eq!(status(&fx, &who).await["authorization"], "extra");
    assert!(
        visible(&fx, &who)
            .await
            .iter()
            .any(|n| n == "plugin_calendar_add")
    );
    assert!(
        visible(&fx, &who)
            .await
            .iter()
            .any(|n| n.starts_with("plugin_gitforge_")),
        "owner tools survive"
    );
    assert_eq!(calendar(&fx, &who).await.unwrap()["source_track_id"], track);
    assert_eq!(
        calendar(&fx, &other).await.unwrap_err().code,
        -32601,
        "another Track gets no grant"
    );
    let again = access(&fx, &who, true).await;
    assert_eq!(again["state"], "already_set");
    assert!(again["ask_id"].is_null());
    let reopened = SqlxRepo::open(&format!(
        "sqlite://{}",
        fx._dir.path().join("calendar.db").display()
    ))
    .await
    .unwrap();
    let reopened_track = reopened.track_get(track).await.unwrap().unwrap();
    assert!(
        crate::track_plugin_access::current_grants(Some(reopened.db()), &reopened_track, &fx.host)
            .await
            .unwrap()
            .contains("calendar")
    );
    let revoke = access(&fx, &who, false).await["ask_id"].as_i64().unwrap();
    assert_eq!(
        answer(&fx, track, revoke, json!([{"option":0}]), "user").await,
        204
    );
    assert_eq!(calendar(&fx, &who).await.unwrap_err().code, -32601);
    assert!(
        !visible(&fx, &who)
            .await
            .iter()
            .any(|n| n == "plugin_calendar_add")
    );
    assert!(
        visible(&fx, &who)
            .await
            .iter()
            .any(|n| n.starts_with("plugin_gitforge_"))
    );
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":0}]), "user").await,
        409,
        "old approval cannot restore a revoked grant"
    );
}

#[tokio::test]
async fn track_plugin_access_decline_can_be_asked_again_and_only_user_option_grants() {
    let fx = Fixture::new().await;
    let who = owned(&fx).await;
    let other = owned(&fx).await;
    let track = who.track_id.as_deref().unwrap();
    let ask = access(&fx, &who, true).await["ask_id"].as_i64().unwrap();
    assert_eq!(
        answer(&fx, track, ask, json!([{"text":"yes"}]), "user").await,
        400
    );
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":0}]), "ai:codex").await,
        403
    );
    assert_eq!(
        answer(
            &fx,
            other.track_id.as_deref().unwrap(),
            ask,
            json!([{"option":0}]),
            "user"
        )
        .await,
        404
    );
    assert_eq!(calendar(&fx, &who).await.unwrap_err().code, -32601);
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":1}]), "user").await,
        204
    );
    let next = access(&fx, &who, true).await["ask_id"].as_i64().unwrap();
    assert_ne!(next, ask);
    assert_eq!(
        answer(&fx, track, next, json!([{"option":0}]), "user").await,
        204
    );
    let mut worker = who.clone();
    worker.role = CardRole::Worker;
    assert_eq!(calendar(&fx, &worker).await.unwrap_err().code, -32403);
    assert_eq!(
        invoke(
            &fx,
            &worker,
            "neige_access_ask",
            json!({"action":"request","plugin_id":"calendar","enabled":true})
        )
        .await
        .unwrap_err()
        .code,
        -32403
    );
}

#[tokio::test]
async fn track_plugin_access_stale_manifest_and_global_lifecycle_fail_closed() {
    let fx = Fixture::new().await;
    let who = owned(&fx).await;
    let track = who.track_id.as_deref().unwrap();
    let ask = access(&fx, &who, true).await["ask_id"].as_i64().unwrap();
    let raw: String = sqlx::query_scalar("SELECT manifest FROM plugins WHERE id='calendar'")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    let mut changed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    changed["description"] = json!("Changed authority");
    fx.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE plugins SET manifest=? WHERE id='calendar'")
                .bind(changed.to_string()),
        )
        .await
        .unwrap();
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":0}]), "user").await,
        400
    );
    assert_eq!(calendar(&fx, &who).await.unwrap_err().code, -32601);
    fx.repo
        .db()
        .writer()
        .execute(sqlx::query("UPDATE plugins SET manifest=? WHERE id='calendar'").bind(raw))
        .await
        .unwrap();
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":0}]), "user").await,
        204
    );
    fx.host.stop("calendar").await.unwrap();
    assert!(
        !visible(&fx, &who)
            .await
            .iter()
            .any(|n| n == "plugin_calendar_add")
    );
    assert!(calendar(&fx, &who).await.is_err());
    assert_eq!(status(&fx, &who).await["running"], false);
    fx.host.enable("calendar").await.unwrap();
    assert!(calendar(&fx, &who).await.is_ok());
    fx.host
        .stop(crate::builtin_plugins::dev::PLUGIN_ID)
        .await
        .unwrap();
    assert_eq!(
        calendar(&fx, &who).await.unwrap_err().code,
        -32601,
        "unavailable owner remains closed"
    );
}

#[tokio::test]
async fn track_plugin_access_rejects_forged_target_options_and_owner_only_plugins() {
    let fx = Fixture::new().await;
    let who = owned(&fx).await;
    for extra in [
        json!({"track_id":"elsewhere"}),
        json!({"questions":[]}),
        json!({"options":["Grant"]}),
    ] {
        let mut args = json!({"action":"request","plugin_id":"calendar","enabled":true});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            invoke(&fx, &who, "neige_access_ask", args)
                .await
                .unwrap_err()
                .code,
            -32602
        );
    }
    assert!(invoke(&fx,&who,"neige_access_ask",json!({"action":"request","plugin_id":crate::builtin_plugins::dev::PLUGIN_ID,"enabled":true})).await.is_err());
    assert_eq!(calendar(&fx, &who).await.unwrap_err().code, -32601);
}

async fn install_grant_probe(fx: &Fixture) -> crate::plugin_host::Manifest {
    let raw=json!({"manifest_version":2,"id":"grantprobe","version":"0.1.0","min_kernel_version":"0.1.0",
        "kind":"cli-query","display_name":"Grant probe","exposes_tools":[],
        "cli_query":{"command":"/bin/echo","tools":[{"name":"ping","input_schema":{"type":"object","properties":{},"additionalProperties":false},"args":["{\"ok\":true}"]}]}}).to_string();
    let src = fx._dir.path().join("grantprobe-source");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("manifest.json"), &raw).unwrap();
    fx.host
        .install(crate::plugin_host::Manifest::parse(&raw).unwrap(), &src)
        .await
        .unwrap();
    fx.host.enable("grantprobe").await.unwrap();
    crate::plugin_host::Manifest::parse(&raw).unwrap()
}

#[tokio::test]
async fn track_plugin_access_materialized_cli_connector_uses_declared_identity() {
    let fx = Fixture::new().await;
    let who = owned(&fx).await;
    let track = who.track_id.as_deref().unwrap();
    install_grant_probe(&fx).await;
    let stored = fx
        .repo
        .plugin_get_by_id("grantprobe")
        .await
        .unwrap()
        .unwrap();
    assert!(
        stored.manifest["exposes_tools"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !fx.host
            .registry()
            .get("grantprobe")
            .unwrap()
            .exposes_tools
            .is_empty(),
        "real connector materializes live tools"
    );
    let bound = ConnectionIdentity::CardBound(CardIdentity {
        mcp_token_hash: None,
        card_id: who.card_id.clone().into(),
        role: who.role,
        provider: who.provider.clone(),
        session_id: who.session_id.clone(),
        track_id: who.track_id.clone(),
        area_id: who.area_id.clone(),
    });
    let registry = Arc::new(crate::mcp_server::build_default_registry());
    let call = || {
        crate::mcp_server::transport::dispatch_tools_call(
            &fx.ctx,
            None,
            json!({"name":"plugin_grantprobe_ping","arguments":{}}),
            &bound,
            &registry,
        )
    };
    assert_eq!(call().await.unwrap_err().code, -32601);
    let requested = invoke(
        &fx,
        &who,
        "neige_access_ask",
        json!({"action":"request","plugin_id":"grantprobe","enabled":true}),
    )
    .await
    .unwrap();
    let ask = requested["ask_id"].as_i64().unwrap();
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":0}]), "user").await,
        204
    );
    assert!(
        visible(&fx, &who)
            .await
            .iter()
            .any(|n| n == "plugin_grantprobe_ping")
    );
    let result = call().await.unwrap();
    assert_eq!(result["isError"], false, "{result}");
    fx.host.disable("grantprobe").await.unwrap();
    assert_eq!(
        call().await.unwrap_err().code,
        -32503,
        "known authorized disabled plugin is explained"
    );
    let revoke = invoke(
        &fx,
        &who,
        "neige_access_ask",
        json!({"action":"request","plugin_id":"grantprobe","enabled":false}),
    )
    .await
    .unwrap();
    assert_eq!(
        answer(
            &fx,
            track,
            revoke["ask_id"].as_i64().unwrap(),
            json!([{"option":0}]),
            "user"
        )
        .await,
        204
    );
    fx.host.enable("grantprobe").await.unwrap();
    assert_eq!(
        call().await.unwrap_err().code,
        -32601,
        "global re-enable does not undo revocation"
    );
    fx.host.disable("grantprobe").await.unwrap();
}

#[tokio::test]
async fn track_plugin_access_pending_decision_cannot_overwrite_a_newer_revision() {
    let fx = Fixture::new().await;
    let who = owned(&fx).await;
    let track = who.track_id.as_deref().unwrap();
    let ordinary = invoke(
        &fx,
        &who,
        "neige_user_ask",
        json!({"questions":[{
            "title":"Allow Calendar?", "options":["Allow", "Later"]
        }]}),
    )
    .await
    .unwrap();
    assert_eq!(
        answer(
            &fx,
            track,
            ordinary["ask_id"].as_i64().unwrap(),
            json!([{"option":0}]),
            "user"
        )
        .await,
        204
    );
    assert_eq!(
        calendar(&fx, &who).await.unwrap_err().code,
        -32601,
        "ordinary question is never an authorization"
    );
    let ask = access(&fx, &who, true).await["ask_id"].as_i64().unwrap();
    // Simulate a separate committed decision while this exact question is still open.
    fx.repo.db().writer().execute(sqlx::query(concat!(
        "INSERT INTO track_plugin_access(track_id,plugin_id,granted,manifest_sha256,revision) ",
        "VALUES(?,'calendar',0,'earlier-decision',1)"
    )).bind(track)).await.unwrap();
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":0}]), "user").await,
        400
    );
    assert_eq!(calendar(&fx, &who).await.unwrap_err().code, -32601);
    assert_eq!(
        answer(&fx, track, ask, json!([{"option":1}]), "user").await,
        204
    );
}

#[tokio::test]
async fn track_plugin_access_unbound_track_reads_persisted_cumulative_grants() {
    let fx = Fixture::new().await;
    fx.host.enable("gitforge").await.unwrap();
    let who = fx.identity(CardRole::Planner).await;
    let track = fx
        .repo
        .track_get(who.track_id.as_ref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(track.plugin_scope.is_none());
    assert!(
        crate::track_plugin_access::current_grants(Some(fx.repo.db()), &track, &fx.host)
            .await
            .unwrap()
            .is_empty()
    );
    let id = track.id.clone();
    let manifests: Vec<_> = ["calendar", "gitforge"]
        .iter()
        .map(|id| fx.host.registry().get(id).unwrap())
        .collect();
    crate::db::write_in_tx_typed(fx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            for manifest in manifests {
                crate::track_plugin_access::grant_template_plugin_tx(tx, &id, &manifest).await?;
            }
            Ok(())
        })
    })
    .await
    .unwrap();
    let grants = crate::track_plugin_access::current_grants(Some(fx.repo.db()), &track, &fx.host)
        .await
        .unwrap();
    assert_eq!(
        grants,
        ["calendar".to_owned(), "gitforge".to_owned()]
            .into_iter()
            .collect()
    );
    assert!(
        visible(&fx, &who)
            .await
            .iter()
            .any(|name| name == "plugin_calendar_add")
    );
}

#[tokio::test]
async fn track_plugin_access_unbound_track_requires_sqlite() {
    let fx = Fixture::installed().await;
    let who = fx.identity(CardRole::Planner).await;
    let track = fx
        .repo
        .track_get(who.track_id.as_ref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(track.plugin_scope.is_none());
    assert!(matches!(
        crate::track_plugin_access::current_grants(None, &track, &fx.host).await,
        Err(crate::error::CalmError::Internal(_))
    ));
}

#[tokio::test]
async fn template_plugin_grant_materialized_cli_connector_preserves_declared_identity_when_disabled()
 {
    let fx = Fixture::new().await;
    let who = owned(&fx).await;
    let track: crate::ids::TrackId = who.track_id.clone().unwrap().into();
    let declared = install_grant_probe(&fx).await;
    let live = fx.host.registry().get("grantprobe").unwrap();
    assert_ne!(
        crate::track_plugin_access::digest(&declared).unwrap(),
        crate::track_plugin_access::digest(&live).unwrap()
    );
    crate::db::write_in_tx_typed(fx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::track_plugin_access::grant_template_plugin_tx(tx, &track, &live).await
        })
    })
    .await
    .unwrap();
    let approved: String = sqlx::query_scalar(
        "SELECT manifest_sha256 FROM track_plugin_access WHERE track_id=? AND plugin_id='grantprobe'",
    )
    .bind(who.track_id.as_ref().unwrap())
    .fetch_one(fx.repo.db().read_pool())
    .await
    .unwrap();
    assert_eq!(
        approved,
        crate::track_plugin_access::digest(&declared).unwrap()
    );
    assert!(
        visible(&fx, &who)
            .await
            .iter()
            .any(|n| n == "plugin_grantprobe_ping")
    );
    let bound = ConnectionIdentity::CardBound(CardIdentity {
        mcp_token_hash: None,
        card_id: who.card_id.clone().into(),
        role: who.role,
        provider: who.provider.clone(),
        session_id: who.session_id.clone(),
        track_id: who.track_id.clone(),
        area_id: who.area_id.clone(),
    });
    let registry = Arc::new(crate::mcp_server::build_default_registry());
    let call = || {
        crate::mcp_server::transport::dispatch_tools_call(
            &fx.ctx,
            None,
            json!({"name":"plugin_grantprobe_ping","arguments":{}}),
            &bound,
            &registry,
        )
    };
    assert_eq!(call().await.unwrap()["isError"], false);
    fx.host.disable("grantprobe").await.unwrap();
    assert_eq!(call().await.unwrap_err().code, -32503);
    let status = invoke(&fx, &who, "neige_access_ask", json!({"action":"status"}))
        .await
        .unwrap();
    assert_eq!(
        status["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["plugin_id"] == "grantprobe")
            .unwrap()["authorization"],
        "extra"
    );
}
