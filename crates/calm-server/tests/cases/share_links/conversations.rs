//! Shared conversation HTTP publication contracts.
use super::reads::{base, track};
use super::*;
use calm_server::model::NewCard;

#[tokio::test]
async fn shared_canonical_conversation_hides_private_pending_draft() {
    let fx = Fixture::new().await;
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/conversations", fx.track))
                .header("cookie", &fx.cookie)
                .header("content-type", "application/json")
                .header("Idempotency-Key", "share-private-draft")
                .body(Body::from(
                    json!({"provider":"codex","text":"Private pending draft"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let card = created["id"].as_str().unwrap();
    let link = fx.create(true).await;
    let root = base(link["token"].as_str().unwrap(), &fx.track);
    let (status, owner_page) = fx
        .request(
            "GET",
            &format!("/api/cards/{card}/harness/items?contract=conversation_v1"),
            Some(&fx.cookie),
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{owner_page}");
    assert!(owner_page.to_string().contains("Private pending draft"));
    for suffix in [
        "conversations".to_string(),
        format!("conversations/{card}/items?contract=conversation_v1"),
        format!("conversations/{card}/live?contract=conversation_v1"),
        format!("conversations/{card}/run"),
    ] {
        let (status, shared) = fx
            .request("GET", &format!("{root}/{suffix}"), None, json!(null))
            .await;
        assert_eq!(status, StatusCode::OK, "{suffix}: {shared}");
        let text = shared.to_string();
        for secret in [
            "Private pending draft",
            "permission_mode",
            "cwd",
            "pending_inputs",
        ] {
            assert!(!text.contains(secret), "{suffix}: {shared}");
        }
    }
    let other = track(&fx, &fx.area).await;
    let area_link = fx.create(false).await;
    for suffix in [
        "items?contract=conversation_v1",
        "live?contract=conversation_v1",
        "run",
    ] {
        assert_eq!(
            fx.request(
                "GET",
                &format!(
                    "{}/conversations/{card}/{suffix}",
                    base(area_link["token"].as_str().unwrap(), &other)
                ),
                None,
                json!(null)
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
#[allow(deprecated)]
async fn shared_historical_items_redact_local_images() {
    let fx = Fixture::new().await;
    let card = fx.state.raw_repo().card_create(NewCard { track_id: fx.track.clone().into(),kind:"codex".into(),sort:None,title:Some("Public conversation".into()),payload:json!({"harness_profile":"assistant","assistant_provider":"codex","cwd":"/private/cwd","permission_mode":"full"}) }).await.unwrap();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer()
        .execute(
            sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1").bind(card.id.as_str()),
        )
        .await
        .unwrap();
    fx.state.write().role_cache().insert(
        card.id.clone(),
        calm_server::model::CardRole::Assistant,
        fx.track.clone().into(),
    );
    db.writer().execute(sqlx::query(concat!(
        "INSERT INTO harness_items(worker_session_id,card_id,track_id,thread_id,",
        "item_uuid,item_type,method,params,created_at_ms) ",
        "VALUES('historical',?1,?2,'thread','item','userMessage','item/completed',?3,1)"
    ))
        .bind(card.id.as_str()).bind(&fx.track).bind(json!({"item":{"type":"userMessage","content":[{"type":"localImage","path":"/private/image.png"}]}}).to_string())).await.unwrap();
    let link = fx.create(true).await;
    let root = base(link["token"].as_str().unwrap(), &fx.track);
    let (status, items) = fx
        .request(
            "GET",
            &format!("{root}/conversations/{}/items", card.id),
            None,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{items}");
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert!(!items.to_string().contains("/private/image.png"));
    let (status, run) = fx
        .request(
            "GET",
            &format!("{root}/conversations/{}/run", card.id),
            None,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(
        run,
        json!({"card_id":card.id,"phase":null,"runtime_state":null})
    );
}

pub(super) async fn canonical(fx: &Fixture) -> (String, String) {
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/conversations", fx.track))
                .header("cookie", &fx.cookie)
                .header("content-type", "application/json")
                .header("Idempotency-Key", "projection")
                .body(Body::from(
                    json!({"provider":"codex","text":"Pending human"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let card = created["id"].as_str().unwrap().to_string();
    let worker: String = sqlx::query_scalar("SELECT id FROM worker_sessions WHERE card_id=?1")
        .bind(&card)
        .fetch_one(fx.state.raw_repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    (card, worker)
}

fn samples() -> Vec<(Option<&'static str>, &'static str, Value, Value)> {
    vec![
        (
            Some("commandExecution"),
            "item/completed",
            json!({"type":"commandExecution","cwd":"SECRET cwd","command":"SECRET command","aggregatedOutput":"SECRET output","status":"completed","durationMs":7,"exitCode":0}),
            json!({"type":"commandExecution","status":"completed","durationMs":7,"exitCode":0}),
        ),
        (
            Some("fileChange"),
            "item/completed",
            json!({"type":"fileChange","status":"completed","changes":[{"path":"SECRET path","diff":"SECRET diff"},{"path":"SECRET second"}]}),
            json!({"type":"fileChange","status":"completed","changes":[{},{}]}),
        ),
        (
            Some("mcpToolCall"),
            "item/completed",
            json!({"type":"mcpToolCall","tool":"neige_task_ls","arguments":{"path":"SECRET args"},"result":{"tasks":"SECRET plan"},"status":"completed"}),
            json!({"type":"mcpToolCall","tool":"neige_task_ls","status":"completed"}),
        ),
        (
            Some("mcpToolCall"),
            "item/completed",
            json!({"type":"mcpToolCall","tool":"neige_user_ask","arguments":{"questions":[{"title":"Choose","options":["A","B"],"internal":"SECRET question"}],"internal":"SECRET args"},"result":"SECRET result","status":"completed"}),
            json!({"type":"mcpToolCall","tool":"neige_user_ask","arguments":{"questions":[{"title":"Choose","options":["A","B"]}]},"status":"completed"}),
        ),
        (
            Some("mcpToolCall"),
            "item/completed",
            json!({"type":"mcpToolCall","tool":"neige_mail_send","arguments":{"summary":"Public summary","body":"SECRET mail"},"result":"SECRET result"}),
            json!({"type":"mcpToolCall","tool":"neige_mail_send","arguments":{"summary":"Public summary"}}),
        ),
        (
            Some("dynamicToolCall"),
            "item/completed",
            json!({"type":"dynamicToolCall","tool":"example","arguments":"SECRET args","result":"SECRET result","status":"failed","error":{"message":"SECRET error"}}),
            json!({"type":"dynamicToolCall","tool":"example","status":"failed","error":true}),
        ),
        (
            Some("reasoning"),
            "item/completed",
            json!({"type":"reasoning","summary":["SECRET summary"],"content":"SECRET reasoning","durationMs":3}),
            json!({"type":"reasoning","durationMs":3}),
        ),
        (
            Some("agentMessage"),
            "item/completed",
            json!({"type":"agentMessage","text":"Public answer","internal":"SECRET agent"}),
            json!({"type":"agentMessage","text":"Public answer"}),
        ),
        (
            Some("userMessage"),
            "item/completed",
            json!({"type":"userMessage","content":[{"type":"inputText","text":"Public question","internal":"SECRET text"},{"type":"localImage","path":"SECRET image"}]}),
            json!({"type":"userMessage","content":[{"type":"inputText","text":"Public question"},{"type":"localImage","path":"[redacted]"}]}),
        ),
        (
            None,
            "turn/completed",
            json!({"id":"turn","status":"completed","durationMs":11,"internal":"SECRET turn"}),
            json!({"id":"turn","status":"completed","durationMs":11}),
        ),
        (
            Some("futureTool"),
            "item/completed",
            json!({"type":"futureTool","arguments":"SECRET future","status":"completed"}),
            json!({"type":"futureTool","status":"completed"}),
        ),
    ]
}

#[tokio::test]
#[allow(deprecated)]
async fn shared_item_projection_allowlists_all_transcript_sources_and_preserves_owner() {
    use calm_server::db::sqlite::conversation::{
        PresentationSource, allocate_presentation_tx, import_item_tx,
    };
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let fx = Fixture::with_state(with_repo(&fresh_state().await, repo.clone())).await;
    let (canonical_card, worker) = canonical(&fx).await;
    let historical_card = fx
        .state
        .raw_repo()
        .card_create(NewCard {
            track_id: fx.track.clone().into(),
            kind: "codex".into(),
            sort: None,
            title: Some("Historical".into()),
            payload: json!({"harness_profile":"assistant","assistant_provider":"codex"}),
        })
        .await
        .unwrap()
        .id
        .to_string();
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(
            sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1").bind(&historical_card),
        )
        .await
        .unwrap();
    fx.state.write().role_cache().insert(
        historical_card.clone().into(),
        calm_server::model::CardRole::Assistant,
        fx.track.clone().into(),
    );
    let link = fx.create(true).await;
    let root = base(link["token"].as_str().unwrap(), &fx.track);
    for (card, source) in [
        (&historical_card, "historical_raw"),
        (&canonical_card, "native"),
        (&canonical_card, "imported_item"),
    ] {
        for (index, (kind, method, payload, expected)) in samples().into_iter().enumerate() {
            // Canonical Human inputs have their own presentation, never a Native echo.
            if source == "native" && (kind == Some("userMessage") || method == "turn/completed") {
                continue;
            }
            let params = if method == "turn/completed" {
                payload
            } else {
                json!({"completedAtMs":42,"internal":"SECRET envelope","item":payload})
            };
            let raw = params.to_string();
            let row = repo
                .fixture_transcript_insert(
                    &worker,
                    card,
                    &fx.track,
                    source,
                    Some("turn"),
                    Some(&format!("{source}-{index}")),
                    kind,
                    method,
                    &raw,
                    None,
                )
                .await
                .unwrap();
            if source != "historical_raw" {
                let db = fx.state.raw_repo().sqlite_db().unwrap();
                db.writer().with_writer(|session| Box::pin(async move {
                    let mut tx = session.begin_immediate().await.unwrap();
                    if source == "native" {
                        allocate_presentation_tx(&mut tx,card,PresentationSource::NativeItem {row_id:row}).await.unwrap();
                    } else {
                        let boundary: i64 = sqlx::query_scalar("SELECT MAX(sequence) FROM conversation_presentations WHERE card_id=?1 AND source_kind!='imported_item'")
                            .bind(card).fetch_one(&mut **tx).await.unwrap();
                        import_item_tx(&mut tx,card,row,boundary).await.unwrap().unwrap();
                    }
                    tx.commit().await.unwrap();
                })).await.unwrap();
            }
            for contract in if source == "historical_raw" {
                vec!["", "?contract=conversation_v1"]
            } else {
                vec!["?contract=conversation_v1"]
            } {
                for shared in [false, true] {
                    let url = if shared {
                        format!("{root}/conversations/{card}/items{contract}")
                    } else {
                        format!("/api/cards/{card}/harness/items{contract}")
                    };
                    let (status, page) = fx
                        .request(
                            "GET",
                            &url,
                            if shared { None } else { Some(&fx.cookie) },
                            json!(null),
                        )
                        .await;
                    assert_eq!(status, StatusCode::OK, "{page}");
                    let rows: Vec<&Value> = if contract.is_empty() {
                        page.as_array().unwrap().iter().collect()
                    } else {
                        page["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(|entry| entry["source"].get("item"))
                            .collect()
                    };
                    let item = rows.into_iter().find(|item| item["id"] == row).unwrap();
                    let actual: Value =
                        serde_json::from_str(item["params"].as_str().unwrap()).unwrap();
                    if shared {
                        let expected = if method == "turn/completed" {
                            expected.clone()
                        } else {
                            json!({"completedAtMs":42,"item":expected})
                        };
                        assert_eq!(actual, expected, "{source} {kind:?} {contract}");
                        assert!(!actual.to_string().contains("SECRET"));
                    } else {
                        // Existing Owner behavior includes local-image path redaction only.
                        let expected: Value = serde_json::from_str(
                            &calm_server::planner_attachments::redact_local_image_paths(&raw),
                        )
                        .unwrap();
                        assert_eq!(actual, expected);
                    }
                }
            }
        }
    }
}
