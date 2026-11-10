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
            json!({"type":"userMessage","content":[{"type":"inputText","text":"SECRET internal system prompt"},{"type":"text","text":"SECRET legacy diff"},{"type":"localImage","path":"SECRET image"},{"type":"inputImage","imageUrl":"https://example.com/public.png"}]}),
            json!({"type":"userMessage","content":[{"type":"localImage","path":"[redacted]"},{"type":"inputImage","imageUrl":"https://example.com/public.png"}]}),
        ),
        (
            Some("user_message"),
            "item/completed",
            json!({"type":"user_message","content":[{"type":"text","text":"SECRET old envelope"}]}),
            json!({"type":"user_message","content":[]}),
        ),
        (
            None,
            "turn/completed",
            json!({"id":"turn","status":"completed","durationMs":11,"internal":"SECRET turn"}),
            json!({"id":"turn","status":"completed","durationMs":11}),
        ),
        (
            None,
            "turn/completed",
            json!({"id":"failed-turn","status":"failed","error":{"message":"SECRET provider /private/socket","codexErrorInfo":{"other":{"path":"SECRET error payload"}}}}),
            json!({"id":"failed-turn","status":"failed","error":{"codexErrorInfo":{"other":null}}}),
        ),
        (
            None,
            "turn/completed",
            json!({"id":"timeout-turn","status":"interrupted","harness_interruption_reason":"max_turn_duration","error":{"message":"SECRET timeout /private/provider"}}),
            json!({"id":"timeout-turn","status":"interrupted","harness_interruption_reason":"max_turn_duration","error":{}}),
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
            if source == "native"
                && (matches!(kind, Some("userMessage" | "user_message"))
                    || method == "turn/completed")
            {
                continue;
            }
            let params = if method == "turn/completed" {
                payload
            } else {
                json!({"completedAtMs":42,"internal":"SECRET envelope","item":payload})
            };
            let raw = params.to_string();
            let mut segments = json!([
                {"presentation":"user","text":"Public human","attachments":[{"id":"550e8400-e29b-41d4-a716-446655440000.png","size":12,"contentType":"image/png","url":"https://example.com/public.png"}],"origin":{"observation":"user_message","event_id":17,"queue_entry":{"id":"human-input","rev":1}}},
                {"presentation":"system","text":"SECRET /host/cwd dirty /host/task.md","attachments":[{"id":"550e8400-e29b-41d4-a716-446655440000.png","size":12,"contentType":"image/png","url":"https://example.com/SECRET.png"}],"origin":{"observation":"system_context","event_id":18,"queue_entry":{"id":"SECRET-queue","rev":1}}}
            ]);
            let system_labels = [
                ("system_worker_turn_finished", "Worker turn finished"),
                ("system_report_edited", "Report edited"),
                ("system_task_completed", "Task completed"),
                ("system_task_failed", "Task failed"),
                ("system_mail", "Mail"),
                ("system_planner_turn_budget", "Planner turn budget"),
                ("system_planner_context_usage", "Planner context usage"),
            ];
            for (presentation, _) in system_labels {
                let mut segment = segments[1].clone();
                segment["presentation"] = json!(presentation);
                segments.as_array_mut().unwrap().push(segment);
            }
            // Keep legacy user envelopes without segments to exercise the unproven text path.
            let stored_segments = (kind != Some("userMessage")).then(|| segments.to_string());
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
                    stored_segments.as_deref(),
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
                        assert!(
                            !item.to_string().contains("SECRET"),
                            "{source} {contract}: {item}"
                        );
                        if kind != Some("userMessage") {
                            assert_eq!(item["input_segments"][0], segments[0]);
                            assert_eq!(
                                item["input_segments"][1],
                                json!({"presentation":"system","text":"System update","attachments":[]})
                            );
                            for (index, (presentation, label)) in system_labels.iter().enumerate() {
                                assert_eq!(
                                    item["input_segments"][index + 2],
                                    json!({"presentation":presentation,"text":label,"attachments":[]})
                                );
                            }
                        }
                        let timeout = params["harness_interruption_reason"] == "max_turn_duration";
                        assert_eq!(
                            item["turn_error_text"],
                            if timeout {
                                json!(
                                    calm_types::harness::HarnessInterruptionReason::MaxTurnDuration
                                        .message()
                                )
                            } else {
                                Value::Null
                            }
                        );
                    } else {
                        // Existing Owner behavior includes local-image path redaction only.
                        let expected: Value = serde_json::from_str(
                            &calm_server::planner_attachments::redact_local_image_paths(&raw),
                        )
                        .unwrap();
                        assert_eq!(actual, expected);
                        if kind != Some("userMessage") {
                            assert_eq!(item["input_segments"], segments);
                        }
                        if let Some(message) = params.pointer("/error/message") {
                            assert_eq!(item["turn_error_text"], *message);
                        }
                    }
                }
            }
        }
    }
}
