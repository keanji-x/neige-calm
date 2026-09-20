//! Plugin trees the kernel itself wrote: synthesized `mcp-http` connector trees,
//! stamped with a marker so `uninstall` can tell them from operator-owned directories.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};
use utoipa::ToSchema;

use super::connector::SECRETS_FILENAME;
use super::version::KERNEL_VERSION;

/// Stamped into every kernel-written plugin tree.
pub const MARKER_FILENAME: &str = ".neige-managed.json";

/// The secrets key the synthesized manifest names in `mcp_http.api_key_secret`.
pub const API_KEY_SECRET_NAME: &str = "api_key";

/// Version stamped into a synthesized manifest.
pub const SYNTHESIZED_VERSION: &str = "0.1.0";

/// `2`, not `3`: nothing synthesized here declares a `config_schema`.
pub const SYNTHESIZED_MANIFEST_VERSION: u32 = 2;

/// Why a `local_path` install of a marked directory is refused.
pub const REJECT_MARKED_SOURCE_HINT: &str = "this directory is a kernel-managed plugin tree (it carries \
     `.neige-managed.json`) and may not be installed as a local path — \
     uninstalling would then delete it. Install it from its connector form, or \
     remove the marker if you have taken ownership of the tree";

/// The operator-supplied half of an `mcp-http` connector install.
#[derive(Clone, Deserialize, ToSchema)]
pub struct ConnectorInstall {
    /// Validated by `Manifest::parse`, not here.
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Absolute `http://` / `https://` endpoint.
    pub url: String,
    /// Private literal HTTP headers. Stored only in secrets.json; not echoed.
    #[serde(default, deserialize_with = "private_headers")]
    pub headers: BTreeMap<String, String>,
    /// The credential. Absent or empty ⇒ an unauthenticated connector. Never echoed;
    /// it reaches disk in `secrets.json` and nowhere else.
    #[serde(default)]
    pub api_key: Option<String>,
    /// `bearer` | `header:<name>`.
    #[serde(default)]
    pub api_key_in: Option<String>,
    /// Explicit all-tools mode; omitted ⇒ the strict (possibly empty) allowlist.
    #[serde(default)]
    pub tools_all: bool,
    #[serde(default)]
    pub tools_allow: Vec<String>,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub bringup_timeout_ms: Option<u64>,
}

// Serde's default map error can quote a wrongly typed credential value.
// Decode through Value and return only fixed diagnostics at this boundary.
fn private_headers<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    let value = Value::deserialize(d)?;
    let object = value
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("headers must be a string map"))?;
    object
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_string()))
                .ok_or_else(|| serde::de::Error::custom("HTTP header values must be strings"))
        })
        .collect()
}

impl std::fmt::Debug for ConnectorInstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectorInstall(<private>)")
    }
}

impl ConnectorInstall {
    /// The credential, or `None` when the operator supplied an empty one.
    pub fn credential(&self) -> Option<&str> {
        self.api_key.as_deref().filter(|k| !k.is_empty())
    }

    /// Same validation for unsaved Check and durable installation.
    pub fn prepare(&self) -> Result<(super::Manifest, BTreeMap<String, String>), String> {
        super::http_headers::HttpHeaders::parse(self.headers.clone())?;
        if let Some(key) = self.credential() {
            super::HttpCredential::parse(key)?;
        }
        let manifest =
            super::Manifest::parse(&self.manifest_json().to_string()).map_err(|e| e.to_string())?;
        let mut secrets = BTreeMap::new();
        if let Some(key) = self.credential() {
            secrets.insert(API_KEY_SECRET_NAME.to_string(), key.to_string());
        }
        for (index, value) in self.headers.values().enumerate() {
            secrets.insert(format!("http_header_{index}"), value.clone());
        }
        Ok((manifest, secrets))
    }

    /// The manifest document this connector describes; validated by `Manifest::parse` on the way back in.
    pub fn manifest_json(&self) -> Value {
        let mut mcp_http = Map::new();
        mcp_http.insert("url".into(), json!(self.url));
        if !self.headers.is_empty() {
            let refs: BTreeMap<_, _> = self
                .headers
                .keys()
                .enumerate()
                .map(|(index, name)| (name, format!("http_header_{index}")))
                .collect();
            mcp_http.insert("header_secrets".into(), json!(refs));
        }
        if self.credential().is_some() {
            mcp_http.insert("api_key_secret".into(), json!(API_KEY_SECRET_NAME));
            // Left absent rather than defaulted: the manifest validator requires it and its error names the accepted set.
            if let Some(placement) = &self.api_key_in {
                mcp_http.insert("api_key_in".into(), json!(placement));
            }
        }
        mcp_http.insert("tools_all".into(), json!(self.tools_all));
        mcp_http.insert("tools_allow".into(), json!(self.tools_allow));
        if let Some(ms) = self.request_timeout_ms {
            mcp_http.insert("request_timeout_ms".into(), json!(ms));
        }
        if let Some(ms) = self.bringup_timeout_ms {
            mcp_http.insert("bringup_timeout_ms".into(), json!(ms));
        }

        let mut doc = Map::new();
        doc.insert(
            "manifest_version".into(),
            json!(SYNTHESIZED_MANIFEST_VERSION),
        );
        doc.insert("id".into(), json!(self.id));
        doc.insert("version".into(), json!(SYNTHESIZED_VERSION));
        // An older kernel would drop `kind: "mcp-http"` from the registry on boot; stamping the running version makes that a stated refusal.
        doc.insert(
            "min_kernel_version".into(),
            json!(KERNEL_VERSION.to_string()),
        );
        doc.insert("display_name".into(), json!(self.display_name));
        if let Some(desc) = &self.description {
            doc.insert("description".into(), json!(desc));
        }
        doc.insert("kind".into(), json!("mcp-http"));
        doc.insert("mcp_http".into(), Value::Object(mcp_http));
        Value::Object(doc)
    }
}

/// Does `path` hold a tree the kernel wrote? Any uncertainty answers `false`, so `uninstall` leaves the tree on disk.
pub fn is_managed_tree(path: &Path) -> bool {
    std::fs::metadata(path.join(MARKER_FILENAME)).is_ok_and(|m| m.is_file())
}

/// Errors from writing a synthesized tree; none carries the credential.
#[derive(Debug)]
pub enum WriteError {
    /// `plugins_dir/<id>` exists and is not a kernel-written tree.
    Occupied(PathBuf),
    /// Filesystem failure, with the operation that failed.
    Io(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Occupied(p) => write!(
                f,
                "{} already exists and was not created by the kernel — refusing to overwrite it",
                p.display()
            ),
            Self::Io(msg) => f.write_str(msg),
        }
    }
}

/// Write the synthesized tree at `dir`, rebuilding a previous kernel-written tree so a
/// reinstall cannot inherit its `secrets.json`.
pub fn write_connector_tree(
    dir: &Path,
    manifest_text: &str,
    secrets: &BTreeMap<String, String>,
) -> Result<(), WriteError> {
    if dir.exists() {
        if !is_managed_tree(dir) {
            // Must not clean up: the directory is somebody else's.
            return Err(WriteError::Occupied(dir.to_path_buf()));
        }
        std::fs::remove_dir_all(dir)
            .map_err(|e| WriteError::Io(format!("removing {}: {e}", dir.display())))?;
    }
    // The marker is written last, so a half-written tree is not `is_managed_tree`; remove it here or its `secrets.json` is stranded.
    let built = build_tree(dir, manifest_text, secrets);
    if built.is_err() {
        let _ = std::fs::remove_dir_all(dir);
    }
    built
}

fn build_tree(
    dir: &Path,
    manifest_text: &str,
    secrets: &BTreeMap<String, String>,
) -> Result<(), WriteError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| WriteError::Io(format!("creating {}: {e}", dir.display())))?;
    set_mode(dir, 0o700)?;

    write_file(&dir.join("manifest.json"), manifest_text.as_bytes(), 0o644)?;

    if !secrets.is_empty() {
        let body = serde_json::to_vec_pretty(secrets)
            .map_err(|e| WriteError::Io(format!("serializing {SECRETS_FILENAME}: {e}")))?;
        write_file(&dir.join(SECRETS_FILENAME), &body, 0o600)?;
    }

    // Last, so a tree interrupted half-written is not claimed as ours.
    let marker = json!({
        "managed_by": "neige-kernel",
        "kind": "mcp-http",
        "kernel_version": KERNEL_VERSION.to_string(),
    });
    let body = serde_json::to_vec_pretty(&marker)
        .map_err(|e| WriteError::Io(format!("serializing {MARKER_FILENAME}: {e}")))?;
    write_file(&dir.join(MARKER_FILENAME), &body, 0o644)
}

/// Remove a kernel-written tree; a no-op on anything else.
pub fn remove_managed_tree(dir: &Path) -> Result<bool, String> {
    if !is_managed_tree(dir) {
        return Ok(false);
    }
    std::fs::remove_dir_all(dir)
        .map(|()| true)
        .map_err(|e| format!("removing managed plugin tree {}: {e}", dir.display()))
}

fn write_file(path: &Path, body: &[u8], mode: u32) -> Result<(), WriteError> {
    std::fs::write(path, body)
        .map_err(|e| WriteError::Io(format!("writing {}: {e}", path.display())))?;
    set_mode(path, mode)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), WriteError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| WriteError::Io(format!("chmod {:o} {}: {e}", mode, path.display())))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), WriteError> {
    // No POSIX mode to set; `secrets.json` is then only as private as the plugins directory.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connector(api_key: Option<&str>) -> ConnectorInstall {
        ConnectorInstall {
            id: "com.example.zhibao".into(),
            display_name: "Zhibao".into(),
            description: None,
            url: "https://mcp.example.test/mcp".into(),
            api_key: api_key.map(str::to_owned),
            api_key_in: Some("bearer".into()),
            headers: BTreeMap::new(),
            tools_all: false,
            tools_allow: Vec::new(),
            request_timeout_ms: None,
            bringup_timeout_ms: None,
        }
    }

    #[test]
    fn a_synthesized_manifest_never_carries_the_credential() {
        let doc = connector(Some("sk-credential")).manifest_json().to_string();
        assert!(
            !doc.contains("sk-credential"),
            "manifest must not hold the key: {doc}"
        );
        assert!(doc.contains("\"api_key_secret\":\"api_key\""), "{doc}");
        assert!(doc.contains("\"api_key_in\":\"bearer\""), "{doc}");
    }

    #[test]
    fn a_keyless_connector_claims_no_secret_and_no_placement() {
        let doc = connector(None).manifest_json();
        let block = &doc["mcp_http"];
        assert!(block.get("api_key_secret").is_none(), "{doc}");
        assert!(block.get("api_key_in").is_none(), "{doc}");
    }

    #[test]
    fn an_empty_credential_is_no_credential() {
        assert_eq!(connector(Some("")).credential(), None);
        assert_eq!(connector(Some("sk-x")).credential(), Some("sk-x"));
    }

    #[test]
    fn a_written_tree_is_recognisable_as_the_kernels_and_carries_0600_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write_connector_tree(
            &dir,
            "{}",
            &BTreeMap::from([(API_KEY_SECRET_NAME.to_string(), "sk-credential".to_string())]),
        )
        .unwrap();

        assert!(is_managed_tree(&dir));
        let secrets = dir.join(SECRETS_FILENAME);
        assert!(
            std::fs::read_to_string(&secrets)
                .unwrap()
                .contains("sk-credential")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&secrets), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
    }

    #[test]
    fn an_unmanaged_directory_is_refused_rather_than_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), "operator's manifest").unwrap();

        let err = write_connector_tree(
            &dir,
            "{}",
            &BTreeMap::from([(API_KEY_SECRET_NAME.to_string(), "sk-credential".to_string())]),
        )
        .unwrap_err();
        assert!(matches!(err, WriteError::Occupied(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join("manifest.json")).unwrap(),
            "operator's manifest"
        );
    }

    #[test]
    fn rewriting_a_managed_tree_drops_the_previous_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write_connector_tree(
            &dir,
            "{}",
            &BTreeMap::from([(API_KEY_SECRET_NAME.to_string(), "sk-credential".to_string())]),
        )
        .unwrap();
        write_connector_tree(&dir, "{}", &BTreeMap::new()).unwrap();
        assert!(!dir.join(SECRETS_FILENAME).exists());
        assert!(is_managed_tree(&dir), "and it is still ours");
    }

    #[test]
    fn removal_is_refused_for_anything_the_kernel_did_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("work.txt"), "operator's file").unwrap();

        assert_eq!(remove_managed_tree(&dir), Ok(false));
        assert!(dir.join("work.txt").is_file(), "the tree survives");
        // An absent path is not an error.
        assert_eq!(remove_managed_tree(&tmp.path().join("absent")), Ok(false));
    }

    // `is_managed_tree` follows the link; the operator's files must survive however `remove_dir_all` handles a symlink.
    #[cfg(unix)]
    #[test]
    fn removal_through_a_symlink_never_reaches_the_operators_files() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("checkout");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("work.txt"), "operator's file").unwrap();
        std::fs::write(real.join(MARKER_FILENAME), "{}").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let outcome = remove_managed_tree(&link);
        assert!(
            real.join("work.txt").is_file(),
            "the operator's directory must survive whatever the removal did: {outcome:?}"
        );
    }
}
