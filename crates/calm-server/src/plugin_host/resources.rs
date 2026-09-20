//! Kernel-side `resources/read` handler for `ui://<plugin>/<view>` resources, answered locally from the manifest without forwarding to the plugin.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use thiserror::Error;

use super::manifest::View;
use super::mcp::{ResourceContent, ResourceContents};
use super::registry::PluginRegistry;

/// MIME type the MCP Apps specification stipulates for HTML resources backing an iframe; `profile=mcp-app` is what AppBridge keys its sandboxing on.
pub const HTML_MCP_APP_MIME: &str = "text/html;profile=mcp-app";

/// URI-parse and not-found are split because the caller maps them to 400 vs 404.
#[derive(Debug, Error)]
pub enum ResourceError {
    /// URI didn't match `ui://<plugin>/<view>`.
    #[error("malformed ui:// uri: {0}")]
    MalformedUri(String),

    /// No plugin with this id in the registry.
    #[error("plugin `{0}` not installed")]
    PluginNotFound(String),

    /// Plugin exists but the manifest doesn't list this view_id.
    #[error("view `{view_id}` not found on plugin `{plugin_id}`")]
    ViewNotFound { plugin_id: String, view_id: String },

    /// Filesystem read failed; carries the path so a packaging mistake is visible.
    #[error("reading view html {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Parse `ui://<plugin>/<view>`; a `/` after the view_id is rejected.
fn parse_ui_uri(uri: &str) -> Result<(String, String), ResourceError> {
    let body = uri
        .strip_prefix("ui://")
        .ok_or_else(|| ResourceError::MalformedUri(uri.to_string()))?;
    let (plugin_id, view_id) = body
        .split_once('/')
        .ok_or_else(|| ResourceError::MalformedUri(uri.to_string()))?;
    if plugin_id.is_empty() || view_id.is_empty() || view_id.contains('/') {
        return Err(ResourceError::MalformedUri(uri.to_string()));
    }
    Ok((plugin_id.to_string(), view_id.to_string()))
}

/// Compose `_meta.ui` for a view; `None` when neither CSP nor permissions were declared, so the wire response omits `_meta`.
fn build_meta_ui(view: &View) -> Option<Value> {
    let csp_val = view
        .csp
        .as_ref()
        .map(|c| serde_json::to_value(c).expect("CspBlock serializes"));
    let perms_val = view
        .permissions
        .as_ref()
        .map(|p| serde_json::to_value(p).expect("UiPermissions serializes"));
    if csp_val.is_none() && perms_val.is_none() {
        return None;
    }
    let mut ui = Map::new();
    if let Some(c) = csp_val {
        ui.insert("csp".into(), c);
    }
    if let Some(p) = perms_val {
        ui.insert("permissions".into(), p);
    }
    Some(json!({ "ui": Value::Object(ui) }))
}

/// Read the HTML asset backing `ui://<plugin>/<view>` from `<install_path>/views/<view_id>.html`; the manifest's `entry_html` is deliberately not honored.
pub fn read_ui_resource(
    registry: &PluginRegistry,
    uri: &str,
) -> Result<ResourceContents, ResourceError> {
    let (plugin_id, view_id) = parse_ui_uri(uri)?;
    let manifest = registry
        .get(&plugin_id)
        .ok_or_else(|| ResourceError::PluginNotFound(plugin_id.clone()))?;
    let view = manifest
        .views
        .iter()
        .find(|v| v.view_id == view_id)
        .ok_or_else(|| ResourceError::ViewNotFound {
            plugin_id: plugin_id.clone(),
            view_id: view_id.clone(),
        })?;

    // An install_path is required; falling back to `plugins_dir/<id>` would couple this module to the host.
    let install_path: PathBuf = registry
        .install_path(&plugin_id)
        .ok_or_else(|| ResourceError::PluginNotFound(plugin_id.clone()))?;
    let html_path = install_path.join("views").join(format!("{view_id}.html"));
    let text = std::fs::read_to_string(&html_path).map_err(|e| ResourceError::Io {
        path: html_path.display().to_string(),
        source: e,
    })?;

    let meta = build_meta_ui(view);
    Ok(ResourceContents {
        contents: vec![ResourceContent {
            uri: uri.to_string(),
            mime_type: Some(HTML_MCP_APP_MIME.to_string()),
            text: Some(text),
            blob: None,
            meta,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::manifest::{CspBlock, Manifest, UiPermissions};
    use std::path::Path;

    /// One plugin installed at a tempdir, optionally with a `views/<view_id>.html`; the guard keeps the tempdir alive.
    fn seed_plugin(
        plugin_id: &str,
        view: View,
        html_body: Option<&str>,
    ) -> (PluginRegistry, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path().join(plugin_id);
        std::fs::create_dir_all(install_dir.join("bin")).unwrap();
        if let Some(body) = html_body {
            let views_dir = install_dir.join("views");
            std::fs::create_dir_all(&views_dir).unwrap();
            std::fs::write(views_dir.join(format!("{}.html", view.view_id)), body).unwrap();
        }
        let manifest_json = serde_json::json!({
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Stub",
            "entrypoint": { "command": "bin/stub" },
            "views": []
        });
        let mut manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");
        manifest.views.push(view);
        let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir))]);
        (registry, tmp)
    }

    fn base_view() -> View {
        View {
            view_id: "status".to_string(),
            title: "Status".to_string(),
            icon: None,
            scope: "card".to_string(),
            default_size: None,
            entry_html: None,
            csp: None,
            permissions: None,
        }
    }

    #[test]
    fn read_ui_resource_returns_html_with_meta_ui() {
        let mut view = base_view();
        view.csp = Some(CspBlock {
            default_src: Some(vec!["'self'".into()]),
            script_src: Some(vec!["'self'".into(), "'unsafe-inline'".into()]),
            connect_src: Some(vec!["'self'".into()]),
            ..Default::default()
        });
        view.permissions = Some(UiPermissions {
            tools: vec!["neige.overlay.set".into()],
        });
        let (reg, _tmp) = seed_plugin(
            "dev.neige.demo",
            view,
            Some("<!doctype html><html><body>hi</body></html>"),
        );

        let contents = read_ui_resource(&reg, "ui://dev.neige.demo/status").expect("ok");
        assert_eq!(contents.contents.len(), 1);
        let entry = &contents.contents[0];
        assert_eq!(entry.uri, "ui://dev.neige.demo/status");
        assert_eq!(entry.mime_type.as_deref(), Some(HTML_MCP_APP_MIME));
        assert!(entry.text.as_ref().unwrap().contains("<body>hi"));
        let meta = entry.meta.as_ref().expect("meta set");
        assert_eq!(
            meta.pointer("/ui/csp/default_src/0")
                .and_then(|v| v.as_str()),
            Some("'self'")
        );
        assert_eq!(
            meta.pointer("/ui/permissions/tools/0")
                .and_then(|v| v.as_str()),
            Some("neige.overlay.set")
        );
    }

    #[test]
    fn read_ui_resource_404_when_plugin_unknown() {
        let reg = PluginRegistry::empty();
        let err = read_ui_resource(&reg, "ui://nope.never.installed/status").unwrap_err();
        assert!(
            matches!(err, ResourceError::PluginNotFound(ref p) if p == "nope.never.installed"),
            "got {err:?}",
        );
    }

    #[test]
    fn read_ui_resource_404_when_view_id_unknown() {
        let (reg, _tmp) = seed_plugin("dev.neige.demo", base_view(), Some("<html></html>"));
        let err = read_ui_resource(&reg, "ui://dev.neige.demo/no-such-view").unwrap_err();
        assert!(
            matches!(
                err,
                ResourceError::ViewNotFound { ref plugin_id, ref view_id }
                if plugin_id == "dev.neige.demo" && view_id == "no-such-view"
            ),
            "got {err:?}",
        );
    }

    #[test]
    fn read_ui_resource_omits_meta_ui_when_view_has_no_csp_permissions() {
        let (reg, _tmp) = seed_plugin(
            "dev.neige.demo",
            base_view(),
            Some("<html><body>plain</body></html>"),
        );
        let contents = read_ui_resource(&reg, "ui://dev.neige.demo/status").expect("ok");
        let entry = &contents.contents[0];
        assert!(
            entry.meta.is_none(),
            "no csp+permissions → _meta omitted; got {:?}",
            entry.meta
        );
    }

    #[test]
    fn malformed_uri_rejected() {
        let reg = PluginRegistry::empty();
        for bad in [
            "ui://",
            "ui://only-one-segment",
            "ui:///empty-plugin/view",
            "ui://plugin/",
            "http://plugin/view",
            "",
        ] {
            let err = read_ui_resource(&reg, bad).unwrap_err();
            assert!(
                matches!(err, ResourceError::MalformedUri(_)),
                "{bad:?} should be MalformedUri, got {err:?}"
            );
        }
    }

    #[test]
    fn io_error_surfaces_path() {
        let (reg, _tmp) = seed_plugin("dev.neige.demo", base_view(), None);
        let err = read_ui_resource(&reg, "ui://dev.neige.demo/status").unwrap_err();
        match err {
            ResourceError::Io { path, .. } => {
                assert!(
                    path.ends_with("views/status.html"),
                    "io path should point at the missing html: {path}"
                );
            }
            other => panic!("expected Io, got {other:?}"),
        }
        // suppress unused-Path warning when no `views` dir was created
        let _ = Path::new(".");
    }
}
