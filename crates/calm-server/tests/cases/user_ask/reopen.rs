use super::*;

async fn request_reopen(boot: &Boot) -> i64 {
    let result = call_tool(
        boot,
        TOOL_USER_ASK,
        json!({
            "action": "reopen_track", "questions": [{ "title": "Continue this closed track?" }]
        }),
    )
    .await
    .expect("ask to reopen");
    result["ask_id"].as_i64().unwrap()
}

#[tokio::test]
async fn reopen_ask_click_restores_and_records_user_answer_once() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
    assert!(!track_is_open(&boot).await);
    let items = serde_json::to_value(activity_items(&boot).await).unwrap();
    assert_eq!(
        items[0]["questions"][0]["options"],
        json!(["Reopen and continue", "Keep closed"])
    );
    assert_eq!(
        answer(&boot, id, json!([{ "option": 0 }])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(track_is_open(&boot).await);
    assert!(activity_items(&boot).await.is_empty());
    assert!(
        rows(&boot, "track.updated")
            .await
            .iter()
            .all(|(actor, _)| actor == "User")
    );
    assert_eq!(rows(&boot, "track.updated").await.len(), 1);
    assert_eq!(rows(&boot, "ask.answered").await.len(), 1);
    assert_eq!(
        answer(&boot, id, json!([{ "option": 0 }])).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(rows(&boot, "track.updated").await.len(), 1);
}

#[tokio::test]
async fn reopen_ask_deny_and_typed_grant_label_never_restore() {
    for answers in [
        json!([{ "option": 1 }]),
        json!([{ "text": "Reopen and continue" }]),
    ] {
        let boot = boot().await;
        set_closed(&boot, true).await;
        let id = request_reopen(&boot).await;
        assert_eq!(answer(&boot, id, answers).await.0, StatusCode::NO_CONTENT);
        assert!(!track_is_open(&boot).await);
        assert!(rows(&boot, "track.updated").await.is_empty());
        assert_eq!(rows(&boot, "ask.answered").await.len(), 1);
    }
}

#[tokio::test]
async fn reopen_ask_rejects_agents_and_foreign_scope() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
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
        post_answer(
            &boot,
            "foreign",
            id,
            json!({"answers":[{"option":0}]}),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(!track_is_open(&boot).await);
    assert!(rows(&boot, "ask.answered").await.is_empty());
}

#[tokio::test]
async fn reopen_ask_refuses_changed_closure_without_settling_the_question() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
    set_closed(&boot, false).await;
    assert_eq!(
        answer(&boot, id, json!([{ "option": 0 }])).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(rows(&boot, "ask.answered").await.is_empty());
    assert_eq!(activity_items(&boot).await.len(), 1);
    assert_eq!(
        answer(&boot, id, json!([{ "option": 1 }])).await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn reopen_ask_survives_generic_notification_dismissal() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/activity/dismissals", boot.track_id))
                .header("content-type", "application/json")
                .body(Body::from(json!({"key":format!("ask:{id}")}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        activity_items(&boot).await.len(),
        1,
        "consent remains answerable"
    );
}

#[tokio::test]
async fn reopen_ask_normal_questions_with_matching_options_have_no_effect() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let result = ask(
        &boot,
        json!([{ "title": "Reopen?", "options": ["Reopen and continue", "Keep closed"] }]),
    )
    .await
    .unwrap();
    let id = result["ask_id"].as_i64().unwrap();
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(!track_is_open(&boot).await);
}

#[tokio::test]
async fn reopen_ask_clarification_does_not_hide_consent() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
    let event = Event::HarnessUserMessageEnqueued {
        worker_session_id: PLANNER_SESSION_ID.into(),
        card_id: boot.planner_card_id.clone(),
        track_id: boot.track_id.clone(),
        char_count: 1,
    };
    let scope = calm_server::event::EventScope::Card {
        card: boot.planner_card_id.clone(),
        track: boot.track_id.clone(),
        area: boot.area_id.clone(),
    };
    calm_server::db::write_with_actor_events_typed(
        boot.repo.as_ref(),
        None,
        &boot.events,
        &boot.ctx.write,
        move |_tx| {
            Box::pin(async move { Ok(((), vec![(calm_server::ids::ActorId::User, scope, event)])) })
        },
    )
    .await
    .unwrap();
    let items = serde_json::to_value(activity_items(&boot).await).unwrap();
    assert_eq!(items[0]["ask_id"], id);
}

#[tokio::test]
async fn reopen_ask_same_timestamp_reclosure_cannot_grant() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
    let stamp = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .closed_at
        .unwrap();
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/tracks/{}", boot.track_id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"closed":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    sqlx::query("UPDATE tracks SET closed_at = ?1 WHERE id = ?2")
        .bind(stamp)
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(
        answer(&boot, id, json!([{ "option": 0 }])).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(!track_is_open(&boot).await);
    assert!(rows(&boot, "ask.answered").await.is_empty());
}

#[tokio::test]
async fn reopen_ask_invalid_contract_and_duplicate_are_refused() {
    let boot = boot().await;
    for action in [json!(null), json!("unknown"), json!(true)] {
        assert!(
            call_tool(
                &boot,
                TOOL_USER_ASK,
                json!({ "action": action, "questions": [{ "title": "Reopen?" }] })
            )
            .await
            .is_err()
        );
    }
    assert!(
        call_tool(
            &boot,
            TOOL_USER_ASK,
            json!({ "action":"reopen_track", "questions":[{"title":"Reopen?"}] })
        )
        .await
        .is_err()
    );
    set_closed(&boot, true).await;
    assert!(call_tool(&boot, TOOL_USER_ASK, json!({ "action":"reopen_track", "questions":[{"title":"Reopen?", "options":["No", "Yes"]}] })).await.is_err());
    request_reopen(&boot).await;
    assert!(
        call_tool(
            &boot,
            TOOL_USER_ASK,
            json!({ "action":"reopen_track", "questions":[{"title":"Again?"}] })
        )
        .await
        .is_err()
    );
    assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
}

#[tokio::test]
async fn reopen_ask_rechecks_child_restriction_before_grant_without_settling() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
    let parent = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: boot.area_id.clone(),
            title: "Parent".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id = ?1 WHERE id = ?2")
        .bind(parent.id.as_str())
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,child_track_id,created_at_ms,updated_at_ms) \
        VALUES('parent:child',?1,'child','codex','g','{}','running',?2,1,1)")
        .bind(parent.id.as_str()).bind(boot.track_id.as_str()).execute(&pool).await.unwrap();
    assert_eq!(
        answer(&boot, id, json!([{ "option": 0 }])).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(!track_is_open(&boot).await);
    assert!(rows(&boot, "ask.answered").await.is_empty());
    assert!(
        call_tool(
            &boot,
            TOOL_USER_ASK,
            json!({ "action":"reopen_track", "questions":[{"title":"Again?"}] })
        )
        .await
        .is_err()
    );
    assert_eq!(
        answer(&boot, id, json!([{ "option": 1 }])).await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn reopen_ask_concurrent_grants_commit_one_transition() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    let id = request_reopen(&boot).await;
    let (first, second) = tokio::join!(
        answer(&boot, id, json!([{"option":0}])),
        answer(&boot, id, json!([{"option":0}]))
    );
    assert!(
        (first.0 == StatusCode::NO_CONTENT && second.0 == StatusCode::CONFLICT)
            || (second.0 == StatusCode::NO_CONTENT && first.0 == StatusCode::CONFLICT)
    );
    assert!(track_is_open(&boot).await);
    assert_eq!(rows(&boot, "track.updated").await.len(), 1);
    assert_eq!(rows(&boot, "ask.answered").await.len(), 1);
}
