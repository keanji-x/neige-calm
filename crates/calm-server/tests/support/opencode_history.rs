//! Synthetic HTTP fixture for the actual compiled history backend.
#![cfg(unix)]
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, Uri},
    response::IntoResponse,
    routing::get,
};
use calm_server::{
    db::prelude::*,
    event::EventBus,
    model::NewPlugin,
    plugin_host::{Manifest, PluginHost, PluginRegistry},
    state::WriteContext,
};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub const HISTORY: &str = "opencodehistory";
pub const EXCERPT: &str = "Migrate only the ledger; preserve audit IDs.";

#[derive(Clone, Default)]
pub struct HttpState {
    pub requests: Arc<Mutex<Vec<(String, String, String)>>>,
    pub mixed: Arc<AtomicBool>,
}

#[allow(dead_code)] // Shared across the host-only and end-to-end suites.
pub struct HistoryHost {
    pub host: Arc<PluginHost>,
    pub http: HttpState,
    pub config: Value,
    _http_task: tokio::task::JoinHandle<()>,
}
impl Drop for HistoryHost {
    fn drop(&mut self) {
        self._http_task.abort();
    }
}

async fn response(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
) -> axum::response::Response {
    state.requests.lock().unwrap().push((
        uri.path().into(),
        uri.query().unwrap_or_default().into(),
        headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .into(),
    ));
    let project = json!({"id":"project-a","worktree":"/work/allowed","sandboxes":["/work/second"],"time":{"created":1,"updated":1}});
    let session = |directory: &str, id: &str| json!({"id":id,"slug":"allowed","projectID":"project-a","directory":directory,"title":"Allowed history","version":"1.19.3","time":{"created":1,"updated":2}});
    let message = json!({"info":{"id":"msg_one","sessionID":"ses_allowed","role":"user","time":{"created":1},"agent":"build","model":{"providerID":"synthetic","modelID":"synthetic"}},"parts":[{"id":"prt_one","sessionID":"ses_allowed","messageID":"msg_one","type":"text","text":EXCERPT}]});
    let mut reply = axum::Json(match uri.path() {
        "/global/health" => json!({"healthy":true,"version":"1.19.3"}),
        "/doc" => serde_json::from_str(include_str!(
            "../../../plugin/src/builtin/opencode_history/contract.json"
        ))
        .unwrap(),
        "/project" => json!([project]),
        "/project/current" => project,
        "/session" if state.mixed.load(Ordering::SeqCst) => json!([
            session("/work/allowed", "ses_allowed"),
            session("/work/denied", "ses_SECRET")
        ]),
        "/session" => json!([session("/work/allowed", "ses_allowed")]),
        "/session/ses_allowed" => session("/work/allowed", "ses_allowed"),
        "/session/ses_allowed/message"
            if uri
                .query()
                .is_some_and(|q| q.contains("before=opaque-page-2")) =>
        {
            let mut older = message.clone();
            older["info"]["id"] = json!("msg_older");
            older["info"]["time"]["created"] = json!(0);
            older["parts"][0]["messageID"] = json!("msg_older");
            older["parts"][0]["id"] = json!("prt_older");
            older["parts"][0]["text"] = json!("Earlier context: keep migration reversible.");
            json!([older])
        }
        "/session/ses_allowed/message" => json!([message]),
        "/session/ses_allowed/message/msg_one" => message,
        _ => panic!("unexpected HTTP route {uri}"),
    })
    .into_response();
    if uri.path() == "/session/ses_allowed/message"
        && !uri.query().is_some_and(|q| q.contains("before="))
    {
        reply
            .headers_mut()
            .insert("X-Next-Cursor", "opaque-page-2".parse().unwrap());
    }
    reply
}

pub async fn boot(
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    tmp: &Path,
    configured: bool,
) -> HistoryHost {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let http = HttpState::default();
    let app = Router::new()
        .fallback(get(response))
        .with_state(http.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = write_config(tmp, &format!("http://{addr}"));
    let manifest_text = plugin::builtin::opencode_history::MANIFEST;
    let manifest = Manifest::parse(manifest_text).expect("actual manifest parses");
    let mut with_extra = config.clone();
    with_extra["discard_me"] = json!("not declared");
    assert_eq!(
        Value::Object(calm_server::plugin_host::effective_config(
            &manifest,
            &with_extra
        )),
        config
    );
    repo.plugin_install(NewPlugin {
        id: HISTORY.into(),
        version: manifest.version.clone(),
        install_path: format!("builtin:{HISTORY}"),
        manifest: serde_json::from_str(manifest_text).unwrap(),
        enabled: true,
        user_config: if configured { with_extra } else { json!({}) },
    })
    .await
    .unwrap();
    let registry = PluginRegistry::empty().with_builtins();
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo,
        tmp.join("plugins"),
        tmp.join("plugins-data"),
        Vec::new(),
        events,
        write,
    ));
    HistoryHost {
        host,
        http,
        config,
        _http_task: task,
    }
}

/// Synthetic configuration loaded and validated by the production builtin start path.
pub fn write_config(tmp: &Path, base_url: &str) -> Value {
    let password = tmp.join("password");
    std::fs::write(&password, "synthetic-password").unwrap();
    std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config_path = tmp.join("history.json");
    std::fs::write(&config_path, json!({
        "sources":[{"source_id":"synthetic","base_url":base_url,"credential_ref":"local","directories":["/work/allowed","/work/second"]}],
        "credentials":{"local":{"username":"opencode","password_file":password}},
        "limits":{"page_messages":50,"max_response_bytes":2097152,"max_result_bytes":131072,"max_parts":128,"max_part_bytes":16384,"request_timeout_ms":1000}
    }).to_string()).unwrap();
    json!({"history_config_path":config_path})
}
