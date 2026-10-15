use super::*;

#[tokio::test]
async fn admin_patch_refuses_ai_actor_headers() {
    let (state, id, repo) = boot().await;
    let before = repo.track_get(&id).await.unwrap().unwrap();
    for actor in ["ai:codex", "ai:claude", "ai:planner", "ai:custom"] {
        for admin in [true, false] {
            assert_eq!(
                patch(
                    state.clone(),
                    &id,
                    Some(actor),
                    json!({"admin":admin,"title":"forbidden"})
                )
                .await
                .status(),
                StatusCode::FORBIDDEN
            );
        }
    }
    assert_eq!(
        repo.track_get(&id).await.unwrap().unwrap().title,
        before.title
    );
    assert!(
        repo.track_get(&id)
            .await
            .unwrap()
            .unwrap()
            .admin_granted_at
            .is_none()
    );
    assert!(
        repo.events_for_track(&id, &["track.updated"], None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn admin_patch_refuses_an_ineligible_track() {
    let (state, id, repo) = boot().await;
    sqlx::query("UPDATE tracks SET plugin_scope='bound' WHERE id=?1")
        .bind(&id)
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(
        patch(state.clone(), &id, None, json!({"admin":true}))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        repo.track_get(&id)
            .await
            .unwrap()
            .unwrap()
            .admin_granted_at
            .is_none()
    );
    assert!(
        repo.events_for_track(&id, &["track.updated"], None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn admin_revoke_is_allowed_when_ineligible() {
    let (state, id, repo) = boot().await;
    assert_eq!(
        patch(state.clone(), &id, None, json!({"admin":true}))
            .await
            .status(),
        StatusCode::OK
    );
    let stamp = repo.track_get(&id).await.unwrap().unwrap().admin_granted_at;
    assert!(stamp.is_some());
    assert_eq!(
        patch(state.clone(), &id, None, json!({"admin":true}))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        repo.events_for_track(&id, &["track.updated"], None)
            .await
            .unwrap()
            .len(),
        1
    );
    sqlx::query("UPDATE tracks SET plugin_scope='bound' WHERE id=?1")
        .bind(&id)
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(
        patch(state, &id, None, json!({"admin":false}))
            .await
            .status(),
        StatusCode::OK
    );
    assert!(
        repo.track_get(&id)
            .await
            .unwrap()
            .unwrap()
            .admin_granted_at
            .is_none()
    );
}

#[tokio::test]
async fn admin_eligibility_and_detail_use_storage_facts() {
    use http_body_util::BodyExt;
    for shape in [
        "plugin_scope='missing-plugin'",
        "purpose='area-chat'",
        "creator_track_id='historical-creator',creator_key='key'",
        "parent_track_id='historical-parent'",
        "managed",
        "system",
    ] {
        let (state, id, repo) = boot().await;
        let pool = repo.sqlite_pool().unwrap();
        if shape == "managed" {
            sqlx::query("INSERT INTO managed_track_identities VALUES('owner','identity',?1,'area','UTC','standard',0)")
                .bind(&id).execute(&pool).await.unwrap();
        } else if shape == "system" {
            sqlx::query(
                "UPDATE areas SET kind='system' WHERE id=(SELECT area_id FROM tracks WHERE id=?1)",
            )
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        } else {
            if shape.starts_with("parent_track_id") {
                let area = repo.track_get(&id).await.unwrap().unwrap().area_id;
                sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('historical-parent',?1,'parent',0,1,1)")
                    .bind(area.as_str()).execute(&pool).await.unwrap();
            }
            sqlx::query(&format!("UPDATE tracks SET {shape} WHERE id=?1"))
                .bind(&id)
                .execute(&pool)
                .await
                .unwrap();
        }
        assert_eq!(
            patch(state.clone(), &id, None, json!({"admin":true}))
                .await
                .status(),
            StatusCode::FORBIDDEN,
            "{shape}"
        );
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/tracks/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{shape}");
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["can_admin"], json!(false), "{shape}: {value}");
    }
}
