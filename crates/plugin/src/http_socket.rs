//! Manifest version 6 `http_socket`: the file name of a Unix socket an `app` plugin serves HTTP on,
//! inside its working directory. The kernel proxies session-gated WebSocket upgrades to it; the
//! plugin never listens on TCP.

use std::path::{Path, PathBuf};

/// The first `manifest_version` that may declare `http_socket`.
pub const MIN_MANIFEST_VERSION: u32 = 6;

/// A plain file name: not empty, not `.` or `..`, no separator, no NUL. Anything else could name a
/// file outside the plugin's working directory.
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(
            "must be a plain file name inside the plugin's working directory \
             (not empty, not `.` or `..`, no `/`, `\\` or NUL)",
        );
    }
    Ok(())
}

/// Where the socket named `name` lives for a plugin whose working directory is `work_dir`.
pub fn path(work_dir: &Path, name: &str) -> PathBuf {
    work_dir.join(name)
}

#[cfg(test)]
mod tests {
    use crate::manifest::{Manifest, ManifestError};
    use serde_json::{Value, json};

    fn manifest(version: u32, http_socket: Value) -> Result<Manifest, ManifestError> {
        let v = json!({
            "manifest_version": version,
            "id": "sockets",
            "version": "0.1.0",
            "min_kernel_version": "0.1.0",
            "display_name": "Sockets",
            "entrypoint": { "command": "bin/x" },
            "http_socket": http_socket,
        });
        Manifest::parse(&v.to_string())
    }

    fn field(err: ManifestError) -> String {
        match err {
            ManifestError::Invalid { field, .. } => field,
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn http_socket_is_accepted_from_version_6() {
        let m = manifest(6, json!("http.sock")).unwrap();
        assert_eq!(m.http_socket.as_deref(), Some("http.sock"));
        assert_eq!(
            field(manifest(5, json!("http.sock")).unwrap_err()),
            "manifest_version"
        );
        // Absent stays legal at every version.
        assert!(manifest(5, Value::Null).unwrap().http_socket.is_none());
    }

    #[test]
    fn http_socket_must_be_a_plain_file_name() {
        for bad in [
            "",
            ".",
            "..",
            "/run/http.sock",
            "a/b",
            "../http.sock",
            "a\\b",
            "a\0b",
        ] {
            let err = manifest(6, json!(bad)).unwrap_err();
            assert_eq!(field(err), "http_socket", "{bad:?}");
        }
    }

    #[test]
    fn http_socket_is_an_app_only_surface() {
        let v = json!({
            "manifest_version": 6,
            "id": "sockets",
            "version": "0.1.0",
            "min_kernel_version": "0.1.0",
            "display_name": "Sockets",
            "kind": "mcp-http",
            "mcp_http": { "url": "https://mcp.example.com/mcp", "tools_allow": ["list_reports"] },
            "http_socket": "http.sock",
        });
        let err = Manifest::parse(&v.to_string()).unwrap_err();
        assert_eq!(field(err), "http_socket");
    }

    #[test]
    fn the_socket_lives_in_the_working_directory() {
        let dir = std::path::Path::new("/data/plugins/sockets");
        assert_eq!(super::path(dir, "http.sock"), dir.join("http.sock"));
    }
}
