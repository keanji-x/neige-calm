use super::*;
use calm_truth::objective_host;

const TOOL: &str = "neige_objective_ask";
pub(super) fn definition() -> Value {
    json!({"key":"kr", "title":"交付 KR", "mode":"converge",
        "checks":[{"id":"accepted","role":"goal","config":{"kind":"fact","params":{"fact":"task_accepted","subject":"finish"}}}],
        "constraints":[],"budget":{"tokens":10000,"turns":1000,"wall_secs":10000},"compact":"none"})
}
pub(super) async fn define(boot: &Boot, id: Option<Value>, payload: Value) -> Value {
    let mut op = json!({"kind":"objective","payload":payload});
    if let Some(id) = id {
        op["id"] = id;
    }
    crate::support::report_writes::upsert_block(
        &boot.ctx,
        &boot.registry,
        planner_identity(boot),
        op,
    )
    .await
    .unwrap()["id"]
        .clone()
}
pub(super) async fn setup() -> (Boot, Value) {
    setup_on(boot().await).await
}
pub(super) async fn setup_on(boot: Boot) -> (Boot, Value) {
    let db = boot.repo.sqlite_db().unwrap();
    let write_db = (db).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                calm_server::db::sqlite::track_capabilities::stamp_tx(
                    &mut tx,
                    &boot.track_id,
                    &[calm_types::track_capability::TrackCapability::Objectives],
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                let report = boot
                    .repo
                    .card_create(NewCard {
                        track_id: boot.track_id.clone(),
                        title: None,
                        kind: "track-report".into(),
                        sort: None,
                        payload: serde_json::to_value(
                            calm_server::track_report::TrackReportPayload::initial(),
                        )
                        .unwrap(),
                    })
                    .await
                    .unwrap();
                boot.repo
                    .seed_card_role_cache(&boot.card_role_cache)
                    .await
                    .unwrap();
                crate::support::report_writes::announce_report(
                    boot.repo.as_ref(),
                    &boot.events,
                    &boot.ctx.write,
                    report,
                )
                .await;
                let block = define(&boot, None, definition()).await;
                (boot, block)
            })
        })
        .await
        .expect("independent write operation")
}
async fn approved(boot: &Boot) -> bool {
    objective_host::load_state(&boot.repo.sqlite_db().unwrap(), &boot.track_id)
        .await
        .unwrap()
        .1
        .objectives[&objective::ids::ObjectiveKey::new("kr")]
        .is_approved()
}
pub(super) async fn request(boot: &Boot) -> Value {
    call_tool(boot, TOOL, json!({"key":"kr"})).await.unwrap()
}

#[tokio::test]
async fn objective_approval_rest_click_tool_reuse_and_projection() {
    let (boot, _) = setup().await;
    let first = request(&boot).await;
    let id = first["ask_id"].as_i64().unwrap();
    assert_eq!(request(&boot).await, first);
    assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
    let items = serde_json::to_value(activity_items(&boot).await).unwrap();
    assert_eq!(
        items[0]["questions"][0]["options"],
        json!(["批准", "不批准"])
    );
    assert_eq!(
        post_answer(
            &boot,
            boot.track_id.as_str(),
            id,
            json!({"answers":[{"option":0}]}),
            Some("ai:planner")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(approved(&boot).await);
    assert_eq!(request(&boot).await["status"], "approved");
    assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::CONFLICT
    );
    let projector = calm_server::track_planner_context::TrackPlannerContextProjector::new(
        boot.repo.clone(),
        boot.events.clone(),
        boot.ctx.write.clone(),
    )
    .unwrap();
    projector
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap();
    let overlays = boot
        .repo
        .overlays_for("track", boot.track_id.as_str())
        .await
        .unwrap();
    let kr = overlays.iter().find(|o| o.kind == "objective.kr").unwrap();
    assert_eq!(
        kr.payload["cell"]["table"]["rows"][0],
        json!({"key":"kr","title":"交付 KR","approval":"已批准","current":"—","target":"accepted: 任务 finish 已被接受","verdict":"—","evaluation":"—","baseline":"—","ledger":"0"})
    );
    // Resolve the shipped template's slot through the production Report tools too.
    let source = include_str!("../../../templates/builtin/long-term-goal.md");
    let start = source.find("```neige-block view\n").unwrap() + "```neige-block view\n".len();
    let end = source[start..].find("\n```").unwrap() + start;
    let view = crate::support::report_writes::upsert_block(
        &boot.ctx,
        &boot.registry,
        planner_identity(&boot),
        json!({
            "kind":"view", "payload":serde_json::from_str::<Value>(&source[start..end]).unwrap()
        }),
    )
    .await
    .unwrap();
    let report = call_tool(
        &boot,
        "neige_report_read",
        json!({"resolve":{view["id"].as_str().unwrap():"full"}}),
    )
    .await
    .unwrap();
    let block = report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == view["id"])
        .unwrap();
    let cell = block["resolved"]["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["source"] == "neige://plugin/kernel/objective.kr")
        .unwrap();
    assert_eq!(cell["status"], "ok");
    assert_eq!(
        cell["data"]["cell"]["table"]["rows"][0]["approval"],
        "已批准"
    );
}

#[tokio::test]
async fn objective_approval_decline_or_text_does_not_authorize() {
    for answers in [json!([{"option":1}]), json!([{"text":"批准"}])] {
        let (boot, _) = setup().await;
        let id = request(&boot).await["ask_id"].as_i64().unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM objective_events")
            .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
        assert_eq!(answer(&boot, id, answers).await.0, StatusCode::NO_CONTENT);
        assert!(!approved(&boot).await);
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM objective_events")
            .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
        assert_eq!(n, before);
        assert_ne!(request(&boot).await["ask_id"], id);
    }
}

#[tokio::test]
async fn objective_approval_stale_standard_is_ignored() {
    let (boot, block) = setup().await;
    let id = request(&boot).await["ask_id"].as_i64().unwrap();
    let mut changed = definition();
    changed["checks"][0]["config"]["params"]["subject"] = json!("different");
    define(&boot, Some(block), changed).await;
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(!approved(&boot).await);
    let pending = objective_host::unsettled_intents(&boot.repo.sqlite_db().unwrap())
        .await
        .unwrap();
    assert!(pending.iter().any(|i| matches!(
        i.intent,
        objective::Intent::Notify {
            notice: objective::engine::Notice::EventIgnored {
                reason: objective::engine::IgnoreReason::StaleApproval,
                ..
            }
        }
    )));
}

#[tokio::test]
async fn objective_approval_refuses_non_planner_and_foreign_question_contract() {
    let (boot, _) = setup().await;
    let action: calm_server::event::AskAction = serde_json::from_value(json!({
        "kind":"objective_approval","objective":"kr","standard_hash":"hash"
    }))
    .unwrap();
    let (track, area, actor) = (
        boot.track_id.clone(),
        boot.area_id.clone(),
        calm_server::ids::ActorId::AiPlanner(boot.planner_card_id.clone()),
    );
    let (_, ids) = calm_server::db::write_with_actor_events_typed::<(), _>(
        boot.repo.as_ref(),
        None,
        &boot.events,
        &boot.ctx.write,
        move |_tx| {
            Box::pin(async move {
                Ok((
                    (),
                    vec![(
                        actor,
                        calm_server::event::EventScope::Track {
                            track: track.clone(),
                            area,
                        },
                        Event::AskRequested {
                            track_id: track,
                            questions: vec![AskQuestion {
                                detail: None,
                                title: "批准？".into(),
                                options: vec!["是".into(), "否".into()],
                            }],
                            delivery: AskDelivery::Wake,
                            action: Some(action),
                            source_item_id: None,
                        },
                    )],
                ))
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(
        answer(&boot, ids[0], json!([{"option":0}])).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(rows(&boot, "ask.answered").await.is_empty());
    crate::support::mcp::set_persisted_card_role(
        boot.repo.as_ref(),
        boot.planner_card_id.as_str(),
        CardRole::Worker,
    )
    .await;
    assert!(call_tool(&boot, TOOL, json!({"key":"kr"})).await.is_err());
    assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
}

#[tokio::test]
async fn objective_approval_answer_transaction_rolls_back_with_event_write() {
    let (boot, _) = setup().await;
    let id = request(&boot).await["ask_id"].as_i64().unwrap();
    let db = boot.repo.sqlite_db().unwrap();
    (db).writer()
        .execute(sqlx::query(
            "CREATE TRIGGER reject_objective_answer BEFORE INSERT ON events \
         WHEN NEW.kind='ask.answered' \
         BEGIN SELECT RAISE(ABORT,'answer failure'); END",
        ))
        .await
        .unwrap();
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(!approved(&boot).await);
    assert!(rows(&boot, "ask.answered").await.is_empty());
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM objective_events WHERE kind='approval_granted'")
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    assert_eq!(n, 0);
    (db).writer()
        .execute(sqlx::query("DROP TRIGGER reject_objective_answer"))
        .await
        .unwrap();
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(approved(&boot).await);
}

#[tokio::test]
async fn objective_approval_oversized_definition_requests_and_reuses_real_tool_ask() {
    let (boot, block) = setup().await;
    let mut large = definition();
    large["constraints"] = json!([
        "criterion ".repeat(150),
        format!("{} LAST CRITERION", "criterion ".repeat(150))
    ]);
    define(&boot, Some(block.clone()), large.clone()).await;
    let serialized = serde_json::to_string_pretty(&large).unwrap();
    assert!(serialized.chars().count() > calm_server::ask::MAX_TEXT_CHARS);
    let first = request(&boot).await;
    assert_eq!(first["status"], "awaiting");
    assert!(first["ask_id"].as_i64().is_some());
    assert_eq!(request(&boot).await, first);
    assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
}

#[tokio::test]
async fn objective_approval_snapshot_reuse_edits_and_stale_answers() {
    let (boot, block) = setup().await;
    let mut value = definition();
    value["title"] = json!("x".repeat(2048));
    value["constraints"] = json!([
        "criterion ".repeat(140),
        format!("{} LAST CRITERION", "criterion ".repeat(140))
    ]);
    define(&boot, Some(block.clone()), value.clone()).await;
    let first = request(&boot).await;
    let first_id = first["ask_id"].as_i64().unwrap();
    let events = rows(&boot, "ask.requested").await;
    let Event::AskRequested {
        questions, action, ..
    } = &events[0].1
    else {
        panic!("ask event")
    };
    assert_eq!(
        questions[0].title,
        include_str!("../../../prompts/objective_approval_title.md").trim_end()
    );
    let detail = questions[0].detail.as_deref().unwrap();
    assert!(detail.contains("LAST CRITERION"));
    assert_eq!(serde_json::from_str::<Value>(detail).unwrap(), value);
    let definition: objective::definition::ObjectiveDefinition =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        action,
        &Some(calm_server::event::AskAction::ObjectiveApprovalV2 {
            objective: "kr".into(),
            standard_hash: objective::standard::standard_hash(&definition)
                .as_str()
                .into(),
        })
    );
    let activity = serde_json::to_value(activity_items(&boot).await).unwrap();
    assert_eq!(activity[0]["questions"][0]["detail"], detail);
    assert_eq!(request(&boot).await, first);
    value["title"] = json!("Metadata-only rename");
    value["budget"]["tokens"] = json!(500);
    value["compact"] = json!("on_close");
    define(&boot, Some(block.clone()), value.clone()).await;
    assert_eq!(request(&boot).await, first);
    // The stored presentation remains frozen even after non-standard metadata changes.
    let stored = rows(&boot, "ask.requested").await;
    let Event::AskRequested { questions, .. } = &stored[0].1 else {
        panic!("ask event")
    };
    assert_eq!(questions[0].detail.as_deref(), Some(detail));
    value["constraints"][0] = json!("different standard");
    define(&boot, Some(block.clone()), value.clone()).await;
    let second = request(&boot).await;
    assert_ne!(second, first);
    assert_eq!(request(&boot).await, second);
    assert_eq!(rows(&boot, "ask.requested").await.len(), 2);
    assert_eq!(
        answer(&boot, first_id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(!approved(&boot).await);
    assert_eq!(
        answer(
            &boot,
            second["ask_id"].as_i64().unwrap(),
            json!([{"option":0}])
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(approved(&boot).await);
    value["title"] = json!("Approved standard renamed");
    define(&boot, Some(block.clone()), value.clone()).await;
    assert!(approved(&boot).await);
    assert_eq!(request(&boot).await["status"], "approved");
    value["constraints"][0] = json!("edited after approval");
    define(&boot, Some(block), value).await;
    assert!(!approved(&boot).await);
    let third = request(&boot).await;
    assert_eq!(third["status"], "awaiting");
    assert_ne!(third, second);
    assert_eq!(request(&boot).await, third);
    assert_eq!(rows(&boot, "ask.requested").await.len(), 3);
}

#[tokio::test]
async fn objective_approval_legacy_inline_snapshot_reuses_and_authorizes() {
    let (boot, block) = setup().await;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../fixtures/objective_approval_legacy.json"
    ))
    .unwrap();
    define(&boot, Some(block), fixture["definition"].clone()).await;
    let id = request(&boot).await["ask_id"].as_i64().unwrap();
    let old_title = fixture["title"].as_str().unwrap();
    // Replay the exact historical event shape through the production reader and answer route.
    (boot.repo.sqlite_db().unwrap())
        .writer()
        .execute(
            sqlx::query(concat!(
                "UPDATE events SET payload=json_remove(json_set(payload,",
                "'$.action.kind','objective_approval','$.questions[0].title',?), ",
                "'$.questions[0].detail') WHERE id=?"
            ))
            .bind(old_title)
            .bind(id),
        )
        .await
        .unwrap();
    assert_eq!(request(&boot).await["ask_id"], id);
    assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(approved(&boot).await);
}

#[tokio::test]
async fn objective_approval_missing_or_mismatched_snapshot_cannot_authorize_or_reuse() {
    for mode in [
        "missing",
        "invalid",
        "wrong_key",
        "wrong_standard",
        "downgraded",
    ] {
        let (boot, _) = setup().await;
        let id = request(&boot).await["ask_id"].as_i64().unwrap();
        let mut snapshot = definition();
        if mode == "wrong_key" {
            snapshot["key"] = json!("other");
        }
        if mode == "wrong_standard" {
            snapshot["constraints"] = json!(["changed"]);
        }
        let detail = if mode == "invalid" {
            "not JSON".into()
        } else {
            serde_json::to_string_pretty(&snapshot).unwrap()
        };
        let db = boot.repo.sqlite_db().unwrap();
        if mode == "downgraded" {
            (db).writer()
                .execute(
                    sqlx::query(concat!(
                        "UPDATE events SET payload=json_remove(json_set(payload,",
                        "'$.action.kind','objective_approval'),'$.questions[0].detail') WHERE id=?"
                    ))
                    .bind(id),
                )
                .await
                .unwrap();
        } else if mode == "missing" {
            (db).writer().execute(sqlx::query(
                "UPDATE events SET payload=json_remove(payload,'$.questions[0].detail') WHERE id=?",
            )
            .bind(id))
            .await
            .unwrap();
        } else {
            (db).writer().execute(sqlx::query(
                "UPDATE events SET payload=json_set(payload,'$.questions[0].detail',?) WHERE id=?",
            )
            .bind(detail)
            .bind(id))
            .await
            .unwrap();
        }
        assert!(
            call_tool(&boot, TOOL, json!({"key":"kr"})).await.is_err(),
            "{mode}"
        );
        assert_eq!(
            answer(&boot, id, json!([{"option":0}])).await.0,
            StatusCode::BAD_REQUEST,
            "{mode}"
        );
        assert!(!approved(&boot).await, "{mode}");
        assert!(rows(&boot, "ask.answered").await.is_empty(), "{mode}");
        assert_eq!(rows(&boot, "ask.requested").await.len(), 1, "{mode}");
    }
}
