use super::*;

async fn restart(f: &mut Fixture) {
    f.auth.mobile.shutdown().await.unwrap();
    f._private_ingress.take();
    let address = f._temp.path().join("ingress.sock");
    tokio::time::timeout(Duration::from_secs(3), async {
        while tokio::net::UnixStream::connect(&address).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let state = super::super::auth::fresh_state().await;
    let auth = super::super::auth::live_auth_state("owner", "fixture-password");
    let public = std::sync::Arc::new(routes::public_mobile_router(state.clone(), auth.clone()));
    let config =
        calm_server::mobile_access::private_tailnet::PrivateTailnetConfig::load(&f.state_path)
            .unwrap();
    let ingress = auth
        .mobile
        .configure_private(config, public.clone())
        .await
        .unwrap();
    f.local = routes::application_router(state, auth.clone());
    f.public = public;
    f.auth = auth;
    f._private_ingress = Some(ingress);
}

#[tokio::test]
async fn private_tailnet_mobile_auth_survives_server_restart() {
    let (mut f, _, control) = private_tailnet_fixture().await;
    let phone = pair(&f).await;
    let (_, _, before) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(before["devices"].as_array().unwrap().len(), 1);
    restart(&mut f).await;
    let (status, _, identity) = request(
        &f.public,
        "GET",
        "/api/auth/whoami",
        Some(&phone),
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "paired phone must survive a new server process: {identity}"
    );
    assert_eq!(identity["userId"], "local-owner");
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
        StatusCode::UNAUTHORIZED,
        "password sessions remain process-local"
    );
    assert_eq!(
        request(
            &f.local,
            "GET",
            "/api/mobile/access",
            Some(&phone),
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "restoration must preserve paired authority"
    );
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}

async fn owner_login(f: &mut Fixture) {
    let (status, headers, _) = request(
        &f.local,
        "POST",
        "/api/auth/login",
        None,
        json!({"username":"owner","password":"fixture-password"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    f.owner_cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into();
}

#[tokio::test]
async fn private_tailnet_revocations_survive_server_restart() {
    for action in ["logout", "revoke", "disable", "tailnet_logout"] {
        let (mut f, _, control) = private_tailnet_fixture().await;
        let phone = pair(&f).await;
        let second = pair(&f).await;
        let (_, _, before) = request(
            &f.local,
            "GET",
            "/api/mobile/access",
            Some(&f.owner_cookie),
            json!({}),
        )
        .await;
        let device = before["devices"].as_array().unwrap()[0]["id"]
            .as_str()
            .unwrap();
        // Revoke by stable device identity: determine which credential the chosen row owns.
        let revoked_cookie = if action == "revoke" {
            let status = request(
                &f.local,
                "DELETE",
                &format!("/api/mobile/devices/{device}"),
                Some(&f.owner_cookie),
                json!({}),
            )
            .await
            .0;
            assert_eq!(status, StatusCode::NO_CONTENT);
            if request(
                &f.public,
                "GET",
                "/api/auth/whoami",
                Some(&phone),
                json!({}),
            )
            .await
            .0 == StatusCode::UNAUTHORIZED
            {
                phone.clone()
            } else {
                second.clone()
            }
        } else {
            let (app, path, method, cookie) = match action {
                "logout" => (&*f.public, "/api/auth/logout", "POST", &phone),
                "disable" => (&f.local, "/api/mobile/access", "DELETE", &f.owner_cookie),
                _ => (
                    &f.local,
                    "/api/mobile/tailnet/logout",
                    "POST",
                    &f.owner_cookie,
                ),
            };
            assert_eq!(
                request(app, method, path, Some(cookie), json!({})).await.0,
                StatusCode::OK,
                "{action}"
            );
            phone.clone()
        };
        restart(&mut f).await;
        owner_login(&mut f).await;
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
            request(
                &f.public,
                "GET",
                "/api/auth/whoami",
                Some(&revoked_cookie),
                json!({})
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED,
            "{action} must remain revoked after restart and enable"
        );
        let remaining = if revoked_cookie == phone {
            &second
        } else {
            &phone
        };
        let survives = action == "logout" || action == "revoke";
        assert_eq!(
            request(
                &f.public,
                "GET",
                "/api/auth/whoami",
                Some(remaining),
                json!({})
            )
            .await
            .0,
            if survives {
                StatusCode::OK
            } else {
                StatusCode::UNAUTHORIZED
            },
            "other device after {action}"
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
            status["devices"].as_array().unwrap().len(),
            usize::from(survives),
            "{action}"
        );
        f.auth.mobile.shutdown().await.unwrap();
        control.abort();
    }
}

#[tokio::test]
async fn private_tailnet_scan_auth_survives_restart_but_invitation_does_not() {
    let (mut f, _, control) = private_tailnet_fixture().await;
    let (created, qr) = super::enrollment::scan(&f).await;
    let enrollment = created["enrollmentId"].as_str().unwrap();
    let claim = json!({"enrollmentId":enrollment,"ticket":qr["pairTicket"],"deviceName":"Scan phone","attemptId":"restart-test","attemptSecret":"b".repeat(64)});
    assert_eq!(
        request(
            &f.public,
            "POST",
            "/api/mobile/enrollments/claim",
            None,
            claim.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let redeem = json!({"enrollmentId":enrollment,"attemptId":"restart-test","attemptSecret":"b".repeat(64)});
    let (status, headers, _) = request(
        &f.public,
        "POST",
        "/api/mobile/enrollments/redeem",
        None,
        redeem.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    for flag in [
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=",
        "Expires=",
    ] {
        assert!(cookie.contains(flag), "{cookie}");
    }
    let phone = cookie.split(';').next().unwrap().to_owned();
    let (_, pending_qr) = super::enrollment::scan(&f).await;
    restart(&mut f).await;
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
    assert_eq!(
        request(
            &f.public,
            "POST",
            "/api/mobile/enrollments/redeem",
            None,
            redeem
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED,
        "completed QR redeem is process-local"
    );
    assert_eq!(request(&f.public, "POST", "/api/mobile/enrollments/claim", None,
        json!({"enrollmentId":pending_qr["enrollmentId"],"ticket":pending_qr["pairTicket"],"deviceName":"Late phone","attemptId":"late","attemptSecret":"c".repeat(64)})).await.0,
        StatusCode::UNAUTHORIZED, "pending invitation must not survive restart");
    owner_login(&mut f).await;
    let (_, _, status) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(status["devices"].as_array().unwrap().len(), 1);
    assert_eq!(status["devices"][0]["deviceName"], "Scan phone");
    assert_eq!(
        request(
            &f.local,
            "GET",
            "/api/mobile/access",
            Some(&phone),
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}

async fn ingress_status(f: &Fixture, method: &str, path: &str, cookie: &str) -> u16 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut stream = tokio::net::UnixStream::connect(f._temp.path().join("ingress.sock"))
        .await
        .unwrap();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: fixture\r\nCookie: {cookie}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn private_tailnet_logout_storage_failure_recovers_at_same_origin() {
    let (mut f, _, control) = private_tailnet_fixture().await;
    let phone = pair(&f).await;
    let other = pair(&f).await;
    let path = f._temp.path().join("mobile-grants.json");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        ingress_status(&f, "POST", "/api/auth/logout", &phone).await,
        500
    );
    assert_eq!(
        ingress_status(&f, "GET", "/api/auth/whoami", &other).await,
        503
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    f.auth.mobile.status().await.unwrap();
    assert_eq!(
        ingress_status(&f, "POST", "/api/auth/logout", &phone).await,
        200,
        "verified same-origin recovery must allow logout retry"
    );
    assert_eq!(
        ingress_status(&f, "GET", "/api/auth/whoami", &other).await,
        200,
        "remaining devices must recover without server restart"
    );
    restart(&mut f).await;
    assert_eq!(
        ingress_status(&f, "GET", "/api/auth/whoami", &phone).await,
        401
    );
    assert_eq!(
        ingress_status(&f, "GET", "/api/auth/whoami", &other).await,
        200
    );
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}

#[tokio::test]
async fn private_tailnet_failed_ingress_startup_preserves_primary_grants() {
    let (f, _, control) = private_tailnet_fixture().await;
    let phone = pair(&f).await;
    let path = f._temp.path().join("mobile-grants.json");
    let before = std::fs::read(&path).unwrap();
    let state = super::super::auth::fresh_state().await;
    let auth = super::super::auth::live_auth_state("owner", "fixture-password");
    let router = std::sync::Arc::new(routes::public_mobile_router(state, auth.clone()));
    let config =
        calm_server::mobile_access::private_tailnet::PrivateTailnetConfig::load(&f.state_path)
            .unwrap();
    assert!(
        auth.mobile.configure_private(config, router).await.is_err(),
        "another server cannot bind the active private ingress"
    );
    auth.mobile.shutdown().await.unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "failed startup/shutdown must not erase another server's grant file"
    );
    assert_eq!(
        ingress_status(&f, "GET", "/api/auth/whoami", &phone).await,
        200
    );
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}

#[tokio::test]
async fn private_tailnet_restart_control_outage_is_retryable_not_logout() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (mut f, address, control) = private_tailnet_fixture().await;
    let phone = pair(&f).await;
    control.abort();
    let _ = control.await;
    restart(&mut f).await;
    let mut stream = tokio::net::UnixStream::connect(&address).await.unwrap();
    stream.write_all(format!("GET /api/auth/whoami HTTP/1.1\r\nHost: fixture\r\nCookie: {phone}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    assert_eq!(line.split_whitespace().nth(1).unwrap(), "503", "{line}");
    let stored: Value =
        serde_json::from_slice(&std::fs::read(f._temp.path().join("mobile-grants.json")).unwrap())
            .unwrap();
    assert_eq!(
        stored["devices"].as_object().unwrap().len(),
        1,
        "control outage must not revoke the saved grant"
    );
    assert_eq!(
        ingress_status(&f, "POST", "/api/auth/logout", &phone).await,
        200,
        "logout must remain possible while origin verification is unavailable"
    );
    let revoked: Value =
        serde_json::from_slice(&std::fs::read(f._temp.path().join("mobile-grants.json")).unwrap())
            .unwrap();
    assert!(revoked["devices"].as_object().unwrap().is_empty());
    f.auth.mobile.shutdown().await.unwrap();
}
