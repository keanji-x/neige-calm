use super::*;

#[tokio::test]
async fn separated_link_is_locked_without_cookie_and_never_recovers_token() {
    let fx = Fixture::new().await;
    let created = fx.create(true).await;
    let link = &created["link"];
    assert!(link.get("token").is_none());
    assert_eq!(
        link["expiresAt"].as_i64().unwrap(),
        link["delegation"]["createdAt"].as_i64().unwrap() + 86_400_000
    );
    let id = link["delegation"]["id"].as_str().unwrap();
    let (status, access) = fx
        .request(
            "GET",
            &format!("/api/share-links/{id}/access"),
            None,
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        access,
        json!({"status":"locked", "expiresAt":link["expiresAt"]})
    );
    assert_eq!(
        fx.request(
            "POST",
            &fx.path(true),
            Some(&fx.cookie),
            json!({"token":"a".repeat(64)})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

pub(super) async fn unlock(fx: &Fixture, created: &Value) -> String {
    let id = created["link"]["delegation"]["id"].as_str().unwrap();
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/share-links/{id}/unlock"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"token":created["token"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.starts_with(&format!("calm-share-session-{id}=")));
    cookie.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn scoped_unlock_isolated_from_owner_other_shares_and_dev_autologin() {
    let fx = Fixture::new().await;
    let a = fx.create(true).await;
    let b = fx.create(false).await;
    let aid = a["link"]["delegation"]["id"].as_str().unwrap();
    let bid = b["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = unlock(&fx, &a).await;
    let all = format!("{};{}", fx.cookie, cookie);
    let track_path = format!("/api/tracks/{}?share_link={aid}", fx.track);
    assert_eq!(
        fx.request("GET", &track_path, Some(&all), Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    for owner in [None, Some(fx.cookie.as_str())] {
        assert_eq!(
            fx.request("GET", &track_path, owner, Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        fx.request(
            "GET",
            &format!("/api/tracks/{}?share_link={bid}", fx.track),
            Some(&all),
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, status) = fx
        .request(
            "GET",
            &format!("/api/share-links/{aid}/access"),
            Some(&all),
            Value::Null,
        )
        .await;
    assert_eq!(
        status,
        json!({"status":"unlocked","scope":a["link"]["delegation"]["scope"],"expiresAt":a["link"]["expiresAt"]})
    );
    let (status, _) = fx
        .request(
            "POST",
            &format!("/api/auth/logout?share_link={aid}"),
            Some(&all),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fx.request("GET", "/api/auth/whoami", Some(&all), Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        fx.request("GET", &track_path, Some(&all), Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );

    let dev = AuthState::new(calm_server::auth::AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "dev".into(),
    });
    let app = app(fx.state.clone(), dev);
    for uri in [track_path, format!("/api/auth/whoami?share_link={aid}")] {
        assert_eq!(
            request(&app, "GET", &uri, Some(&fx.cookie), Value::Null)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn scope_allowlist_filters_lists_and_keeps_read_receipts_ephemeral() {
    let fx = Fixture::new().await;
    let sibling = super::fixtures::track(&fx, &fx.area).await;
    let created = fx.create(true).await;
    let id = created["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = unlock(&fx, &created).await;
    for (method, path) in [
        ("GET", format!("/api/tracks/{sibling}")),
        ("DELETE", format!("/api/tracks/{}", fx.track)),
        ("GET", fx.path(true)),
        ("POST", format!("/api/areas/{}/tracks", fx.area)),
        ("POST", "/api/areas".into()),
        ("PUT", "/api/settings".into()),
        ("GET", "/api/tracks".into()),
        ("POST", "/api/overlays".into()),
        ("DELETE", format!("/api/share-links/{id}")),
    ] {
        let response = fx
            .request(
                method,
                &format!("{path}?share_link={id}"),
                Some(&cookie),
                json!({}),
            )
            .await;
        assert_eq!(
            response.0,
            StatusCode::FORBIDDEN,
            "{method} {path}: {:?}",
            response.1
        );
    }
    let (_, areas) = fx
        .request(
            "GET",
            &format!("/api/areas?share_link={id}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(areas.as_array().unwrap().len(), 1);
    assert_eq!(areas[0]["id"], fx.area);
    let (_, tracks) = fx
        .request(
            "GET",
            &format!("/api/areas/{}/tracks?share_link={id}", fx.area),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(tracks.as_array().unwrap().len(), 1);
    assert_eq!(tracks[0]["id"], fx.track);
    let before = fx
        .request("GET", "/api/read-receipts", Some(&fx.cookie), Value::Null)
        .await
        .1;
    let (status, receipt) = fx
        .request(
            "POST",
            &format!("/api/read-receipts/track/{}/read?share_link={id}", fx.track),
            Some(&cookie),
            json!({"readThroughMs":123}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["readThroughMs"], 123);
    let (_, receipts) = fx
        .request(
            "GET",
            &format!("/api/read-receipts?share_link={id}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(receipts["receipts"], json!([]));
    assert_eq!(
        fx.request("GET", "/api/read-receipts", Some(&fx.cookie), Value::Null)
            .await
            .1,
        before
    );
    assert_eq!(
        fx.request(
            "DELETE",
            &format!("/api/share-links/{id}"),
            Some(&fx.cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        fx.request(
            "GET",
            &format!("/api/tracks/{}?share_link={id}", fx.track),
            Some(&cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fx.request(
            "GET",
            &format!("/api/share-links/{id}/access"),
            Some(&cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn unlock_checks_origin_digest_and_failed_attempt_throttle() {
    let fx = Fixture::new().await;
    let created = fx.create(true).await;
    let id = created["link"]["delegation"]["id"].as_str().unwrap();
    let path = format!("/api/share-links/{id}/unlock");
    let denied = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header("host", "calm.test")
                .header("origin", "https://foreign.test")
                .header("content-type", "application/json")
                .body(Body::from(json!({"token":created["token"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let mut throttled = false;
    for _ in 0..32 {
        let response = fx
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"token":"wrong"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            assert!(response.headers().contains_key("retry-after"));
            throttled = true;
            break;
        }
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(throttled);
    assert_eq!(
        fx.request("POST", &path, None, json!({"token":created["token"]}))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn schema_has_only_digest_and_immutable_integer_expiry() {
    let fx = Fixture::new().await;
    let created = fx.create(true).await;
    let id = created["link"]["delegation"]["id"].as_str().unwrap();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('share_links')")
            .fetch_all(db.read_pool())
            .await
            .unwrap();
    assert!(!columns.iter().any(|c| c == "token"));
    for statement in [
        "UPDATE share_links SET expires_at=expires_at+1 WHERE id=?1",
        "UPDATE share_links SET expires_at=NULL WHERE id=?1",
    ] {
        assert!(
            db.writer()
                .execute(sqlx::query(statement).bind(id))
                .await
                .is_err()
        );
    }
    for expiry in [json!(0), json!(-1), json!("bad")] {
        assert!(
            db.writer()
                .execute(
                    sqlx::query(
                        "INSERT INTO share_links
            (id,track_id,approver_user_id,purpose,resource_policy,created_at,token_hash,expires_at)
            VALUES('invalid',?1,?2,'conversation','current_deployment',0,?3,?4)"
                    )
                    .bind(&fx.track)
                    .bind(OWNER_USER_ID)
                    .bind("0".repeat(64))
                    .bind(expiry)
                )
                .await
                .is_err()
        );
    }
    db.writer()
        .execute(
            sqlx::query(
                "INSERT INTO share_links
        (id,track_id,approver_user_id,purpose,resource_policy,created_at,token_hash,expires_at)
        VALUES('expired',?1,?2,'conversation','current_deployment',0,?3,86400000)",
            )
            .bind(&fx.track)
            .bind(OWNER_USER_ID)
            .bind("0".repeat(64)),
        )
        .await
        .unwrap();
    for (method, endpoint) in [("GET", "access"), ("POST", "unlock")] {
        assert_eq!(
            fx.request(
                method,
                &format!("/api/share-links/expired/{endpoint}"),
                None,
                json!({"token":"wrong"})
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn production_shell_share_reads_and_patch_guardrails() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.request(
            "PUT",
            "/api/settings",
            Some(&fx.cookie),
            json!({"settings":{"private":"owner secret"}})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        fx.request(
            "POST",
            &format!("/api/areas/{}/folders", fx.area),
            Some(&fx.cookie),
            json!({"path":"/host-only-folder"})
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (status, recipe) = fx
        .request(
            "POST",
            "/api/track-recipes",
            Some(&fx.cookie),
            json!({"title":"Reusable recipe","body":"Shared starting point"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{recipe}");
    let created = fx.create(true).await;
    let id = created["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = unlock(&fx, &created).await;
    for (path, expected) in [
        ("/api/settings".into(), json!({"settings":{}})),
        (format!("/api/areas/{}/folders", fx.area), json!([])),
        ("/api/track-recipes".into(), json!([recipe.clone()])),
        (
            format!("/api/track-recipes/{}", recipe["id"].as_str().unwrap()),
            recipe,
        ),
    ] {
        let (status, body) = fx
            .request(
                "GET",
                &format!("{path}?share_link={id}"),
                Some(&cookie),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(body, expected);
    }
    for patch in [
        json!({"admin":true}),
        json!({"workspace":{"kind":"managed","path":"/tmp"}}),
    ] {
        assert_eq!(
            fx.request(
                "PATCH",
                &format!("/api/tracks/{}?share_link={id}", fx.track),
                Some(&cookie),
                patch
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn revocation_between_authentication_and_scope_admission_never_reads_owner_settings() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.request(
            "PUT",
            "/api/settings",
            Some(&fx.cookie),
            json!({"settings":{"private":"owner secret"}})
        )
        .await
        .0,
        StatusCode::OK
    );
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = unlock(&fx, &link).await;
    let session = cookie.split_once('=').unwrap().1;
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "share-scope-before-access",
        session,
        pause.clone(),
    );
    let app = fx.app.clone();
    let path = format!("/api/settings?share_link={id}");
    let task = tokio::spawn(async move {
        app.oneshot(
            Request::get(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        fx.request(
            "DELETE",
            &format!("/api/share-links/{id}"),
            Some(&fx.cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    pause.release.notify_one();
    assert_eq!(task.await.unwrap().status(), StatusCode::UNAUTHORIZED);
}
