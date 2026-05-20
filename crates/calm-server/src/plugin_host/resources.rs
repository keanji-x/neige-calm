//! Kernel-side `resources/read` handler for `ui://<plugin>/<view>` resources.
//!
//! Under MCP Apps, the iframe HTML formerly served at
//! `GET /api/plugins/:id/views/:view_id` is now fetched via a JSON-RPC
//! `resources/read` call carrying a `ui://` URI. The plugin's manifest already
//! lists every view + its on-disk path, so the kernel can answer the read
//! locally without forwarding to the plugin process — this also keeps the
//! HTML cacheable + side-effect-free.
//!
//! The plumbing into the iframe transport (postMessage AppBridge) lands in
//! M5. M3 publishes `read_ui_resource` as a pure-function entry point that
//! any caller (M5's host route, today's tests) can invoke once they hold a
//! reference to the registry.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use thiserror::Error;

use super::manifest::View;
use super::mcp::{ResourceContent, ResourceContents};
use super::registry::PluginRegistry;

/// MIME type the MCP Apps spec stipulates for HTML resources backing an iframe.
/// The `profile=mcp-app` parameter is the discriminator AppBridge uses to
/// decide whether to wrap the body in a sandboxed double-iframe versus
/// rendering inline.
pub const HTML_MCP_APP_MIME: &str = "text/html;profile=mcp-app";

/// Failure modes for `read_ui_resource`. We split the URI-parse and
/// not-found cases because the caller surfaces them at different HTTP statuses
/// (400 vs 404), and we keep `Io` distinct because a stat failure on the HTML
/// asset is an operator-fix-it not a user-fix-it.
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

    /// Filesystem read failed. Almost always "expected file is missing" —
    /// surface the path so operators can spot a packaging mistake fast.
    #[error("reading view html {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Parse `ui://<plugin>/<view>` (no trailing slash, no scheme variations).
///
/// We're strict on shape: the spec leaves the URI authority component open
/// per host, and we picked `<plugin_id>` per the M3 design doc §7.6 row 1.
/// A `/` after the view_id is rejected for now — multi-asset views land in
/// a later slice if we need them.
fn parse_ui_uri(uri: &str) -> Result<(String, String), ResourceError> {
    let body = uri
        .strip_prefix("ui://")
        .ok_or_else(|| ResourceError::MalformedUri(uri.to_string()))?;
    // body = "<plugin_id>/<view_id>"
    let (plugin_id, view_id) = body
        .split_once('/')
        .ok_or_else(|| ResourceError::MalformedUri(uri.to_string()))?;
    if plugin_id.is_empty() || view_id.is_empty() || view_id.contains('/') {
        return Err(ResourceError::MalformedUri(uri.to_string()));
    }
    Ok((plugin_id.to_string(), view_id.to_string()))
}

/// Compose the `_meta.ui` object for a view. Returns `None` if neither CSP
/// nor permissions were declared (so the wire response omits `_meta`
/// entirely, matching the MCP Apps profile's "no extras" form).
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

/// Read the HTML asset backing `ui://<plugin>/<view>`.
///
/// Arguments:
///   * `registry` — the in-memory manifest cache. Must already have an
///     install_path recorded for the plugin (every installed plugin does).
///   * `uri` — the full `ui://...` request URI.
///
/// Returns one-entry `ResourceContents` with the HTML body in `text`,
/// `mimeType = "text/html;profile=mcp-app"`, and a `_meta.ui` object
/// carrying the view's CSP + permissions when set on the manifest.
///
/// Path resolution mirrors the deleted `view_html` route: HTML lives at
/// `<install_path>/views/<view_id>.html`. We deliberately don't honor the
/// manifest's `entry_html` field — that's a forward-compat slot for
/// view-specific filenames, and M3 sticks with the on-disk convention.
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

    // The registry tracks install_path per plugin. We require it here: the
    // alternative ("fall back to plugins_dir/<id>") would couple this module
    // to the PluginHost's plugins_dir, which we'd rather not. Tests seed an
    // install_path on `registry.insert(..., Some(path))`.
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::manifest::{CspBlock, Manifest, UiPermissions};
    use std::path::Path;

    /// Build a registry with one plugin installed at a tempdir, optionally
    /// writing a `views/<view_id>.html` file. Returns the temp guard so the
    /// caller can keep it alive for the test scope.
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
        // Build a minimal valid manifest carrying just this view.
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
        let registry = PluginRegistry::empty();
        registry.insert(manifest, Some(install_dir));
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
        // Manifest with CSP + permissions set; HTML file exists; round-trip
        // produces the expected ResourceContents shape.
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
        // View with neither CSP nor permissions → `_meta` is None on the
        // resource entry, so the wire response omits it entirely.
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
        // Plugin + view registered, but no HTML file on disk.
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
