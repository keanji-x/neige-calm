use super::*;
use std::sync::Arc;

fn private_state() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = temp.path().join("mobile-grants.json");
    (temp, path)
}
fn open(path: &Path) -> GrantStore {
    let store = GrantStore::new(crate::auth::SessionIdentity {
        user_id: crate::auth::OWNER_USER_ID.into(),
        display_name: "Owner".into(),
        role: crate::auth::OWNER_ROLE.into(),
    });
    store.load(path.into()).unwrap();
    store
}
const ORIGIN: &str = "https://phone.example.ts.net";

#[tokio::test]
async fn paired_owner_keeps_host_file_reads_and_revocation() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (_private, path) = private_state();
    let store = std::sync::Arc::new(open(&path));
    let token = store
        .mint(ORIGIN, "Phone".into(), DeviceGrant::Scan)
        .unwrap();
    let auth = crate::auth::AuthState::new(crate::auth::AuthConfig {
        username: Some("owner".into()),
        password: Some("fixture-password".into()),
        dev_autologin: false,
        display_name: "Owner".into(),
    });
    auth.sessions.set_backend(store.clone());
    let (_state_files, app) = crate::routes::fs::host_access_tests::app(auth).await;
    let file = path.with_extension("png");
    std::fs::write(&file, b"paired owner image").unwrap();
    let request = || {
        Request::get(format!("/api/fs/readfile-raw?path={}", file.display()))
            .header(
                header::COOKIE,
                format!("{}={token}", crate::auth::SESSION_COOKIE),
            )
            .body(Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        "sandbox"
    );
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        b"paired owner image"
    );
    store.revoke(&store.list()[0].id).unwrap();
    assert_eq!(
        app.oneshot(request()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn mobile_grants_input_source_uses_actual_backend_and_observes_device_revocation() {
    let (_temp, path) = private_state();
    let store = Arc::new(open(&path));
    let token = store
        .mint(ORIGIN, "Input phone".into(), DeviceGrant::Scan)
        .unwrap();
    let auth = crate::auth::AuthState::new(crate::auth::AuthConfig {
        username: Some("Owner".into()),
        password: Some("fixture-password".into()),
        dev_autologin: false,
        display_name: "Owner".into(),
    });
    auth.sessions.set_backend(store.clone());
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("{}={token}", crate::auth::SESSION_COOKIE)
            .parse()
            .unwrap(),
    );
    let (status, source) = crate::auth::authenticated_source(auth, headers).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let source = source.unwrap();
    assert_eq!(
        source.current_principal().unwrap().user_id,
        crate::auth::OWNER_USER_ID
    );
    assert!(!format!("{source:?}").contains(&token));
    store.revoke(&store.list()[0].id).unwrap();
    assert!(matches!(
        source.current_principal(),
        Err(CalmError::Unauthorized)
    ));
}

#[test]
fn mobile_grants_restore_only_hashes_at_verified_origin_and_keep_authority() {
    let (_temp, path) = private_state();
    let store = open(&path);
    let token = store
        .mint(ORIGIN, "Phone".into(), DeviceGrant::Scan)
        .unwrap();
    assert_eq!(
        store.get(&token).unwrap().authority,
        SessionAuthority::PairedDevice
    );
    let bytes = std::fs::read_to_string(&path).unwrap();
    assert!(!bytes.contains(&token));
    assert!(
        !bytes.contains("expires"),
        "permanent server grants have no expiry field"
    );
    assert!(bytes.contains(&hash(&token)));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    store.suspend();
    assert!(store.get(&token).is_none());
    let restored = open(&path);
    assert!(
        restored.get(&token).is_none(),
        "boot must await verified enabled origin"
    );
    restored.activate(ORIGIN).unwrap();
    assert_eq!(
        restored.get(&token).unwrap().authority,
        SessionAuthority::PairedDevice
    );
    let session = restored.get(&token).unwrap();
    assert_eq!(session.session_id, token);
    assert_eq!(session.identity.user_id, crate::auth::OWNER_USER_ID);
    assert_eq!(session.identity.display_name, "Owner");
    assert_eq!(session.identity.role, crate::auth::OWNER_ROLE);
    assert!(
        restored.get(&hash(&token)).is_none(),
        "a digest is not a bearer token"
    );
    restored
        .activate("https://different.example.ts.net")
        .unwrap();
    assert!(restored.get(&token).is_none());
    let again = open(&path);
    again.activate(ORIGIN).unwrap();
    assert!(
        again.get(&token).is_none(),
        "origin mismatch revocation must persist"
    );
}

#[test]
fn mobile_grants_logout_revoke_and_disable_persist_without_reviving_other_devices() {
    for action in ["logout", "revoke", "disable"] {
        let (_temp, path) = private_state();
        let store = open(&path);
        let first = store
            .mint(ORIGIN, "First".into(), DeviceGrant::Scan)
            .unwrap();
        let second = store
            .mint(ORIGIN, "Second".into(), DeviceGrant::Scan)
            .unwrap();
        match action {
            "logout" => store.remove(&first).unwrap(),
            "revoke" => store
                .revoke(
                    &store
                        .list()
                        .into_iter()
                        .find(|d| d.device_name == "First")
                        .unwrap()
                        .id,
                )
                .unwrap(),
            _ => store.disable().unwrap(),
        }
        let restored = open(&path);
        restored.activate(ORIGIN).unwrap();
        assert!(restored.get(&first).is_none(), "{action}");
        assert_eq!(
            restored.get(&second).is_some(),
            action != "disable",
            "{action}"
        );
        assert_eq!(restored.list().len(), usize::from(action != "disable"));
    }
}

#[test]
fn mobile_grants_refuse_corruption_permissions_symlinks_and_oversized_state() {
    for mode in [
        "corrupt",
        "permissions",
        "symlink",
        "directory",
        "oversize",
        "parent",
    ] {
        let (temp, path) = private_state();
        match mode {
            "corrupt" => {
                std::fs::write(&path, b"{bad json").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            "permissions" => {
                std::fs::write(&path, b"{}").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            "symlink" => {
                let target = temp.path().join("other");
                std::fs::write(&target, b"{}").unwrap();
                std::os::unix::fs::symlink(&target, &path).unwrap();
            }
            "directory" => std::fs::create_dir(&path).unwrap(),
            "oversize" => {
                std::fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            _ => std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755))
                .unwrap(),
        }
        let store = GrantStore::new(crate::auth::SessionIdentity {
            user_id: crate::auth::OWNER_USER_ID.into(),
            display_name: "Owner".into(),
            role: crate::auth::OWNER_ROLE.into(),
        });
        assert!(store.load(path).is_err(), "{mode}");
        assert!(store.list().is_empty(), "{mode}");
    }
}

#[test]
fn mobile_grants_storage_failure_refuses_success_and_suspends_authentication() {
    let (_temp, path) = private_state();
    let store = open(&path);
    let token = store
        .mint(ORIGIN, "Phone".into(), DeviceGrant::Scan)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.remove(&token).is_err());
    assert!(store.get(&token).is_none());
    assert!(
        store
            .mint(ORIGIN, "Another".into(), DeviceGrant::Scan)
            .is_err()
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    store.remove(&token).unwrap();
    let restored = open(&path);
    restored.activate(ORIGIN).unwrap();
    assert!(restored.get(&token).is_none());
}
