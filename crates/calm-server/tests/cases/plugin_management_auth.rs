//! #1413: owner authorization at the production plugin management boundary.
//! Real routes, SQL persistence and echo child; no real Codex processes.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{AuthConfig, AuthState};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes::application_router;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const ID: &str = "test.management-auth";

struct Fixture {
    app: Router,
    host: Arc<PluginHost>,
    repo: Arc<dyn Repo>,
    root: TempDir,
    source: PathBuf,
    cookie: String,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("bin")).unwrap();
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_plugin-host-stub-echo"),
            source.join("bin/stub"),
        )
        .unwrap();
        std::fs::write(
            source.join("manifest.json"),
            json!({
                "manifest_version": 1, "id": ID, "version": "0.1.0",
                "min_kernel_version": "0.0.1", "display_name": "Auth fixture",
                "entrypoint": { "command": "bin/stub" }
            })
            .to_string(),
        )
        .unwrap();
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let events = EventBus::new();
        let host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            root.path().join("plugins"),
            root.path().join("data"),
            Vec::new(),
            events.clone(),
            WriteContext::new(Default::default(), Default::default()),
        ));
        let state = AppState::from_parts(
            repo.clone(),
            events,
            Arc::new(DaemonClient::new_stub()),
            host.clone(),
            Arc::new(CodexClient::new_stub()),
            None,
            None,
        );
        let auth = AuthState::new(AuthConfig {
            username: Some("owner".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let app = application_router(state, auth);
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"username": "owner", "password": "fixture-password"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        Self {
            app,
            host,
            repo,
            root,
            source,
            cookie,
        }
    }

    fn install_body(&self) -> Value {
        json!({"source": {"kind": "local_path", "path": self.source}})
    }

    async fn post(&self, path: &str, body: Value, cookie: Option<&str>) -> (StatusCode, Value) {
        let mut request = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn install(&self) {
        let (status, body) = self
            .post(
                "/api/plugins/install",
                self.install_body(),
                Some(&self.cookie),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(
            !self
                .repo
                .plugin_get_by_id(ID)
                .await
                .unwrap()
                .unwrap()
                .enabled
        );
        assert_eq!(
            std::fs::read_link(self.root.path().join("plugins").join(ID)).unwrap(),
            self.source
        );
    }

    async fn enable(&self) {
        let (status, body) = self
            .post(
                &format!("/api/plugins/{ID}/enable"),
                json!({}),
                Some(&self.cookie),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let running = self.host.status(ID).await.unwrap();
        assert_eq!(running.status, PluginRuntimeStatus::Running);
        assert!(running.pid.is_some());
        assert!(
            self.repo
                .plugin_get_by_id(ID)
                .await
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(self.repo.plugin_token_get(ID).await.unwrap().is_some());
    }

    async fn snapshot(&self) -> Value {
        let runtime = self
            .host
            .status(ID)
            .await
            .map(|s| (format!("{:?}", s.status), s.pid));
        json!({
            "row": self.repo.plugin_get_by_id(ID).await.unwrap(),
            "token": self.repo.plugin_token_get(ID).await.unwrap(),
            "manifest": self.host.registry().get(ID).map(|m| m.to_json()),
            "install_path": self.host.registry().install_path(ID),
            "runtime": runtime,
            "tree": tree_snapshot(self.root.path()),
        })
    }

    async fn assert_rejected(&self, path: &str, body: Value) {
        for cookie in [None, Some("calm-session=invalid-session")] {
            let before = self.snapshot().await;
            let (status, body) = self.post(path, body.clone(), cookie).await;
            let after = self.snapshot().await;
            // A deliberately broken fence can start/restart the echo child.
            // Clean it up before failing, including during mutation verification.
            if status != StatusCode::UNAUTHORIZED || before != after {
                let _ = self.host.stop(ID).await;
            }
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{path}, cookie={cookie:?}: {body}"
            );
            assert_eq!(body["code"], "unauthorized");
            assert_eq!(
                before, after,
                "{path} changed state before owner authorization"
            );
        }
    }
}

/// Snapshot the temp execution/data tree without following executable symlinks.
fn tree_snapshot(path: &std::path::Path) -> Value {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    if metadata.file_type().is_symlink() {
        return json!({"link": std::fs::read_link(path).unwrap()});
    }
    if metadata.is_file() {
        return json!({"bytes": std::fs::read(path).unwrap()});
    }
    let mut entries = serde_json::Map::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        entries.insert(
            entry.file_name().into_string().unwrap(),
            tree_snapshot(&entry.path()),
        );
    }
    Value::Object(entries)
}

#[tokio::test]
async fn install_requires_owner_without_side_effects() {
    let fx = Fixture::new().await;
    fx.assert_rejected("/api/plugins/install", fx.install_body())
        .await;
    assert!(fx.repo.plugin_get_by_id(ID).await.unwrap().is_none());
    fx.install().await;
}

#[tokio::test]
async fn enable_requires_owner_without_side_effects() {
    let fx = Fixture::new().await;
    fx.install().await;
    fx.assert_rejected(&format!("/api/plugins/{ID}/enable"), json!({}))
        .await;
    fx.enable().await;
    fx.host.stop(ID).await.unwrap();
}

#[tokio::test]
async fn reload_requires_owner_without_side_effects() {
    let fx = Fixture::new().await;
    fx.install().await;
    fx.enable().await;
    // A valid pending disk change makes an accidental reload observable in
    // both DB/registry and process/token state; an unchanged fixture would not.
    let path = fx.source.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["display_name"] = json!("Reloaded fixture");
    std::fs::write(path, manifest.to_string()).unwrap();
    fx.assert_rejected(&format!("/api/plugins/{ID}/reload"), json!({}))
        .await;
    let old_token = fx.repo.plugin_token_get(ID).await.unwrap();
    let (status, body) = fx
        .post(
            &format!("/api/plugins/{ID}/reload"),
            json!({}),
            Some(&fx.cookie),
        )
        .await;
    let runtime = fx.host.status(ID).await.unwrap();
    fx.host.stop(ID).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(runtime.status, PluginRuntimeStatus::Running);
    assert!(runtime.pid.is_some());
    assert_eq!(
        fx.repo
            .plugin_get_by_id(ID)
            .await
            .unwrap()
            .unwrap()
            .manifest["display_name"],
        "Reloaded fixture"
    );
    assert_eq!(
        fx.host.registry().get(ID).unwrap().display_name,
        "Reloaded fixture"
    );
    assert_ne!(fx.repo.plugin_token_get(ID).await.unwrap(), old_token);
}

/// #1480 — the connector install source is behind the same fence, and the
/// unauthorized arm has one side effect the local-path arm cannot have: this
/// route *writes a credential to disk*. So the assertion is not only "no row"
/// but the tree snapshot `assert_rejected` takes, which would show a
/// `secrets.json` an anonymous request had planted.
#[tokio::test]
async fn connector_install_requires_owner_and_writes_no_credential() {
    let fx = Fixture::new().await;
    let body = json!({"source": {
        "kind": "mcp_http",
        "id": "test.connector-auth",
        "display_name": "Connector fixture",
        "url": "https://mcp.example.test/mcp",
        "api_key": "sk-must-never-reach-disk",
        "api_key_in": "bearer",
    }});
    fx.assert_rejected("/api/plugins/install", body.clone())
        .await;
    assert!(
        fx.repo
            .plugin_get_by_id("test.connector-auth")
            .await
            .unwrap()
            .is_none()
    );
    // Stated over the whole tree rather than one expected path: the refusal
    // has to hold wherever the writer would have put the file.
    let planted = tree_snapshot(fx.root.path()).to_string();
    assert!(
        !planted.contains("secrets") && !planted.contains("neige-managed"),
        "an unauthorized install left a synthesized tree behind: {planted}"
    );

    // The authenticated control, so the refusal above is a fence and not a
    // route that never worked.
    let (status, created) = fx
        .post("/api/plugins/install", body, Some(&fx.cookie))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(
        fx.root
            .path()
            .join("plugins/test.connector-auth/secrets.json")
            .is_file()
    );
}
