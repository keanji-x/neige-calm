use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{AuthState, SESSION_COOKIE};
use calm_server::mobile_access::funnel::FunnelConfig;
use calm_server::routes;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    _temp: TempDir,
    state_path: std::path::PathBuf,
    auth: AuthState,
    local: axum::Router,
    public: std::sync::Arc<axum::Router>,
    owner_cookie: String,
}

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Value,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", "127.0.0.1")
        .header("x-calm-actor", "user")
        .extension(axum::extract::ConnectInfo(
            "127.0.0.1:4444".parse::<std::net::SocketAddr>().unwrap(),
        ));
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

async fn fixture(options: Value) -> Fixture {
    let temp = TempDir::new().unwrap();
    let executable = temp.path().join("tailscale-fixture");
    std::fs::write(&executable, include_str!("../fixtures/mobile-funnel.py")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let state_path = temp.path().join("state.json");
    std::fs::write(&state_path, options.to_string()).unwrap();
    let state = super::auth::fresh_state().await;
    let auth = super::auth::live_auth_state("owner", "fixture-password");
    let public = std::sync::Arc::new(routes::public_mobile_router(state.clone(), auth.clone()));
    auth.mobile
        .configure(
            FunnelConfig {
                executable,
                socket: state_path.clone(),
                https_port: 10000,
            },
            public.clone(),
        )
        .await;
    let local = routes::application_router(state, auth.clone());
    let (status, headers, _) = request(
        &local,
        "POST",
        "/api/auth/login",
        None,
        json!({"username":"owner","password":"fixture-password"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let owner_cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    Fixture {
        _temp: temp,
        state_path,
        auth,
        local,
        public,
        owner_cookie,
    }
}

async fn pair(f: &Fixture) -> String {
    let (status, _, _) = request(
        &f.local,
        "POST",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, invitation) = request(
        &f.local,
        "POST",
        "/api/mobile/pairings",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        invitation["qrImage"]
            .as_str()
            .unwrap()
            .starts_with("data:image/svg+xml;base64,")
    );
    let ticket = invitation["qrPayload"]
        .as_str()
        .unwrap()
        .split("#v1.")
        .nth(1)
        .unwrap();
    let (status, _, claim) = request(
        &f.public,
        "POST",
        "/api/mobile/pairings/claim",
        None,
        json!({"ticket":ticket,"deviceName":"Fixture phone"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json!({"id":claim["id"],"secret":claim["secret"]});
    assert_eq!(
        request(
            &f.public,
            "POST",
            "/api/mobile/pairings/redeem",
            None,
            body.clone()
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let (_, _, status) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(
        status["pending"][0]["verificationCode"],
        claim["verificationCode"]
    );
    let approve = format!(
        "/api/mobile/pairings/{}/approve",
        claim["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&f.local, "POST", &approve, Some(&f.owner_cookie), json!({}))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (status, headers, _) = request(
        &f.public,
        "POST",
        "/api/mobile/pairings/redeem",
        None,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    for flag in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(cookie.contains(flag), "missing {flag}");
    }
    assert_eq!(
        request(&f.public, "POST", "/api/mobile/pairings/redeem", None, body)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    cookie.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn mobile_pairing_full_http_loop_and_explicit_public_authority_fence() {
    let f = fixture(json!({})).await;
    assert_eq!(
        request(&f.local, "POST", "/api/mobile/access", None, json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_ne!(
        request(
            &f.local,
            "POST",
            "/internal/codex/hook",
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let phone = pair(&f).await;
    assert_ne!(phone, f.owner_cookie);
    assert_eq!(
        request(
            &f.public,
            "GET",
            "/api/auth/whoami",
            Some(&phone),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    for (method, path) in [
        ("GET", "/api/mobile/access"),
        ("POST", "/api/auth/login"),
        ("POST", "/internal/codex/hook"),
    ] {
        assert_eq!(
            request(&f.public, method, path, Some(&phone), json!({}))
                .await
                .0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let record: Value =
        serde_json::from_slice(&std::fs::read(f.state_path.with_extension("record")).unwrap())
            .unwrap();
    assert_eq!(record["environmentKeys"], json!(["LANG", "PATH"]));
    assert_eq!(record["args"][0], "funnel");
    assert_eq!(record["args"][1], "--https=10000");
    assert!(
        record["target"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    f.auth.mobile.disable().await.unwrap();
    assert_eq!(
        request(
            &f.public,
            "GET",
            "/api/auth/whoami",
            Some(&phone),
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.local,
            "GET",
            "/api/auth/whoami",
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn mobile_pairing_revoke_closes_an_established_websocket() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let f = fixture(json!({})).await;
    let phone = pair(&f).await;
    let record: Value =
        serde_json::from_slice(&std::fs::read(f.state_path.with_extension("record")).unwrap())
            .unwrap();
    let target = record["target"]
        .as_str()
        .unwrap()
        .replacen("http://", "ws://", 1);
    let mut handshake = format!("{target}/api/events")
        .into_client_request()
        .unwrap();
    handshake
        .headers_mut()
        .insert(header::COOKIE, phone.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(handshake).await.unwrap();
    let (_, _, status) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    let revoke = format!(
        "/api/mobile/devices/{}",
        status["devices"][0]["id"].as_str().unwrap()
    );
    assert_eq!(
        request(
            &f.local,
            "DELETE",
            &revoke,
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(message)) = socket.next().await {
            if message.is_close() {
                break;
            }
        }
    })
    .await
    .expect("revocation must close existing WebSockets");
    assert_eq!(
        request(
            &f.public,
            "GET",
            "/api/auth/whoami",
            Some(&phone),
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    f.auth.mobile.disable().await.unwrap();
}

#[tokio::test]
async fn mobile_pairing_provider_refuses_occupied_ports_and_fails_closed_on_exit() {
    let occupied = fixture(json!({"occupied":true})).await;
    assert_eq!(
        request(
            &occupied.local,
            "POST",
            "/api/mobile/access",
            Some(&occupied.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(!occupied.state_path.with_extension("record").exists());
    let f = fixture(json!({})).await;
    let phone = pair(&f).await;
    std::fs::write(f.state_path.with_extension("exit"), "exit").unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.auth.mobile.status().await.unwrap().public_url.is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(f.auth.mobile.status().await.unwrap().public_url.is_none());
    let session = phone.strip_prefix(&format!("{SESSION_COOKIE}=")).unwrap();
    assert!(f.auth.sessions.get(session).is_none());
    f.auth.mobile.disable().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_pairing_disable_fences_redemption_and_old_claims_after_reenable() {
    let f = fixture(json!({})).await;
    assert_eq!(
        request(
            &f.local,
            "POST",
            "/api/mobile/access",
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, _, invitation) = request(
        &f.local,
        "POST",
        "/api/mobile/pairings",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    let ticket = invitation["qrPayload"]
        .as_str()
        .unwrap()
        .split("#v1.")
        .nth(1)
        .unwrap();
    let (_, _, claim) = request(
        &f.public,
        "POST",
        "/api/mobile/pairings/claim",
        None,
        json!({"ticket":ticket,"deviceName":"Race phone"}),
    )
    .await;
    let approve = format!(
        "/api/mobile/pairings/{}/approve",
        claim["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&f.local, "POST", &approve, Some(&f.owner_cookie), json!({}))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let body = json!({"id":claim["id"],"secret":claim["secret"]});
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let redeem = {
        let barrier = barrier.clone();
        let public = f.public.clone();
        let body = body.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            request(&public, "POST", "/api/mobile/pairings/redeem", None, body).await
        })
    };
    let disable = {
        let barrier = barrier.clone();
        let local = f.local.clone();
        let cookie = f.owner_cookie.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            request(
                &local,
                "DELETE",
                "/api/mobile/access",
                Some(&cookie),
                json!({}),
            )
            .await
        })
    };
    barrier.wait().await;
    let (result, stopped) = tokio::join!(redeem, disable);
    let (status, headers, _) = result.unwrap();
    assert_eq!(stopped.unwrap().0, StatusCode::OK);
    assert!(status == StatusCode::NO_CONTENT || status == StatusCode::UNAUTHORIZED);
    if let Some(cookie) = headers.get(header::SET_COOKIE) {
        let cookie = cookie.to_str().unwrap().split(';').next().unwrap();
        assert_eq!(
            request(
                &f.public,
                "GET",
                "/api/auth/whoami",
                Some(cookie),
                json!({})
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        request(
            &f.local,
            "POST",
            "/api/mobile/access",
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&f.public, "POST", "/api/mobile/pairings/redeem", None, body)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    f.auth.mobile.disable().await.unwrap();
}

#[tokio::test]
async fn mobile_pairing_dev_autologin_is_not_remote_access_authority() {
    let state = super::auth::fresh_state().await;
    let auth = AuthState::new(calm_server::auth::AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "Development owner".into(),
    });
    let app = routes::application_router(state, auth);
    for method in ["GET", "POST", "DELETE"] {
        assert_eq!(
            request(&app, method, "/api/mobile/access", None, json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
}
