use super::*;
use axum::{Router, routing::post};
use std::sync::Mutex;
use tower::ServiceExt;

/// Actual authentication middleware and extractor, without a source constructor
/// or a fixture owner default. Callers must supply their real configured issuer.
pub(crate) async fn authenticated_source(
    auth: AuthState,
    headers: HeaderMap,
) -> (StatusCode, Option<HumanInputSource>) {
    async fn capture(
        State(output): State<Arc<Mutex<Option<HumanInputSource>>>>,
        source: HumanInputSource,
    ) {
        *output.lock().unwrap() = Some(source);
    }
    let output = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/input", post(capture))
        .route_layer(axum::middleware::from_fn_with_state(auth, require_session))
        .with_state(Arc::clone(&output));
    let mut request = Request::post("/input").body(Body::empty()).unwrap();
    *request.headers_mut() = headers;
    let response = app.oneshot(request).await.unwrap();
    let source = output.lock().unwrap().take();
    (response.status(), source)
}

fn config(dev: bool) -> AuthConfig {
    AuthConfig {
        username: Some("Owner".into()),
        password: Some("fixture-password".into()),
        dev_autologin: dev,
        display_name: "Owner".into(),
    }
}

#[tokio::test]
async fn actual_session_source_rechecks_revocation_and_redacts_credentials() {
    let auth = AuthState::new(config(false));
    let token = auth.sessions.create(
        SessionIdentity::owner(&auth.config),
        SessionAuthority::PasswordLogin,
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("{SESSION_COOKIE}={token}").parse().unwrap(),
    );
    let (status, source) = authenticated_source(auth.clone(), headers).await;
    assert_eq!(status, StatusCode::OK);
    let source = source.unwrap();
    assert_eq!(source.current_principal().unwrap().user_id, OWNER_USER_ID);
    assert!(!format!("{source:?}").contains(&token));
    auth.sessions.remove(&token).unwrap();
    assert!(matches!(
        source.current_principal(),
        Err(CalmError::Unauthorized)
    ));
}

#[tokio::test]
async fn configured_dev_source_comes_only_from_actual_authentication_ingress() {
    let (status, source) =
        authenticated_source(AuthState::new(config(true)), HeaderMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        source.unwrap().current_principal().unwrap().user_id,
        OWNER_USER_ID
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "calm-session=dev-autologin".parse().unwrap(),
    );
    let (status, source) = authenticated_source(AuthState::new(config(false)), headers).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(source.is_none());

    let mut parts = Request::post("/input").body(()).unwrap().into_parts().0;
    parts.extensions.insert(dev_principal(&config(true)));
    assert!(
        HumanInputSource::from_request_parts(&mut parts, &())
            .await
            .is_err()
    );
}
