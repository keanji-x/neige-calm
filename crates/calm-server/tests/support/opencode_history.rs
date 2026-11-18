//! Configured host fixture for the actual compiled history backend.
#![cfg(unix)]
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
use std::sync::Arc;

pub const HISTORY: &str = "opencodehistory";
pub struct HistoryHost {
    pub host: Arc<PluginHost>,
}

pub async fn boot(
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    tmp: &Path,
    configured: bool,
) -> HistoryHost {
    let config = write_config(tmp, "http://127.0.0.1:1");
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
    HistoryHost { host }
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
