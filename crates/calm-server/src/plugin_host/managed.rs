//! #1480 — plugin trees the **kernel itself wrote**.
//!
//! Every other install source hands the kernel a directory somebody else owns:
//! a checked-out repo, an unpacked release. `PluginHost::install` symlinks that
//! directory into `plugins_dir/<id>` and `uninstall` deliberately leaves it
//! alone, because deleting an operator's working copy on uninstall would
//! destroy work the kernel never created.
//!
//! An `mcp-http` connector added from the UI has no such directory. The
//! operator supplies a URL and an API key; the tree — `manifest.json`, and
//! `secrets.json` holding the credential — is *synthesized here*, lives under
//! `plugins_dir/<id>` as a real directory, and belongs to nobody else. For that
//! tree the "leave it on disk" contract inverts: what is left behind is a
//! credential the operator asked us to forget, and a `secrets.json` that a
//! later reinstall of the same id would silently inherit.
//!
//! So the two families need to be **decidable apart on disk**, and that is this
//! module's whole job:
//!
//! * [`write_connector_tree`] writes the tree and stamps [`MARKER_FILENAME`]
//!   into it;
//! * [`is_managed_tree`] answers "did the kernel write this?" by reading that
//!   stamp — used by `uninstall` to decide whether the tree may be removed;
//! * a `local_path` install whose source already carries the stamp is
//!   **refused** by the route ([`REJECT_MARKED_SOURCE_HINT`]). That refusal is
//!   what keeps the stamp a reliable answer rather than a hint: without it an
//!   operator could point `local_path` at a directory containing a copied
//!   marker and have `uninstall` delete their own tree.
//!
//! The marker is not a security boundary — anyone who can call the install API
//! can already install anything. It is an *ownership record*, and the refusal
//! above is what makes it a truthful one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use utoipa::ToSchema;

use super::connector::SECRETS_FILENAME;
use super::version::KERNEL_VERSION;

/// Stamped into every kernel-written plugin tree. Dot-prefixed so it cannot
/// collide with a manifest key or a plugin's own file.
pub const MARKER_FILENAME: &str = ".neige-managed.json";

/// The secrets key the synthesized manifest names in `mcp_http.api_key_secret`.
/// One connector, one credential — the form offers no way to add a second, so
/// the name is a constant rather than an operator-supplied string.
pub const API_KEY_SECRET_NAME: &str = "api_key";

/// Version stamped into a synthesized manifest. Connectors have no code of
/// their own, so there is nothing for this to track; it exists because
/// `Manifest::version` is required and must parse as semver.
pub const SYNTHESIZED_VERSION: &str = "0.1.0";

/// `manifest_version` for a synthesized connector manifest.
///
/// `2`, not `3`: nothing synthesized here declares a `config_schema`, and `3`
/// would make a pre-#1284 kernel refuse the file by version for no gain. See
/// `Manifest::manifest_version` for what each level buys.
pub const SYNTHESIZED_MANIFEST_VERSION: u32 = 2;

/// Why a `local_path` install of a marked directory is refused.
pub const REJECT_MARKED_SOURCE_HINT: &str = "this directory is a kernel-managed plugin tree (it carries \
     `.neige-managed.json`) and may not be installed as a local path — \
     uninstalling would then delete it. Install it from its connector form, or \
     remove the marker if you have taken ownership of the tree";

/// The operator-supplied half of an `mcp-http` connector install.
///
/// Mirrors the request body of `POST /api/plugins/install` with
/// `source.kind = "mcp_http"`. Everything the *kernel* decides — the secrets
/// key name, the manifest version, `min_kernel_version` — is a constant above,
/// not a field here.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ConnectorInstall {
    /// Plugin id. Validated by `Manifest::parse`, not here — the manifest is
    /// the single source of truth for what a legal id is.
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Absolute `http://` / `https://` endpoint. Shape-checked by
    /// `McpHttpBlock::validate` once the manifest is parsed.
    pub url: String,
    /// The credential. Absent or empty ⇒ an unauthenticated connector, and the
    /// synthesized manifest then names no `api_key_secret` and no
    /// `secrets.json` is written.
    ///
    /// **Never echoed.** It reaches disk in `secrets.json` and nowhere else:
    /// not the manifest, not the row, not any response body.
    #[serde(default)]
    pub api_key: Option<String>,
    /// `bearer` | `header:<name>`. Closed set enforced by
    /// `McpHttpBlock::validate`; `query:<name>` is refused there (#1194).
    #[serde(default)]
    pub api_key_in: Option<String>,
    #[serde(default)]
    pub tools_allow: Vec<String>,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub bringup_timeout_ms: Option<u64>,
}

impl ConnectorInstall {
    /// The credential, or `None` when the operator supplied an empty one.
    ///
    /// An empty string is treated as "no credential" rather than as a
    /// credential that is empty: `HttpCredential::parse` refuses it anyway, and
    /// a form field left blank must mean the unauthenticated connector the
    /// operator plainly asked for, not a 400.
    pub fn credential(&self) -> Option<&str> {
        self.api_key.as_deref().filter(|k| !k.is_empty())
    }

    /// The manifest document this connector describes.
    ///
    /// **Validation is not done here on purpose.** Every field this writes is
    /// checked by `Manifest::parse` + `McpHttpBlock::validate` on the way back
    /// in — the same code that validates a hand-written manifest — so a
    /// connector added from the UI and one installed from a directory cannot
    /// disagree about what is legal. Re-stating those rules here is how the two
    /// would drift apart.
    pub fn manifest_json(&self) -> Value {
        let mut mcp_http = Map::new();
        mcp_http.insert("url".into(), json!(self.url));
        if self.credential().is_some() {
            mcp_http.insert("api_key_secret".into(), json!(API_KEY_SECRET_NAME));
            // Absent `api_key_in` is left absent rather than defaulted: with a
            // credential present the manifest validator requires it, and its
            // error names the accepted set better than a guess would.
            if let Some(placement) = &self.api_key_in {
                mcp_http.insert("api_key_in".into(), json!(placement));
            }
        }
        if !self.tools_allow.is_empty() {
            mcp_http.insert("tools_allow".into(), json!(self.tools_allow));
        }
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
        // The kernel that wrote it is the kernel it needs: `kind: "mcp-http"`
        // is an unknown variant to a pre-#1164 kernel, which would drop the
        // plugin from the registry with a `warn!` on boot. Stamping the running
        // version makes that rollback a stated refusal instead.
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

/// Does `path` hold a tree the kernel wrote?
///
/// A pure predicate over the filesystem: the marker file exists and is a
/// regular file. Nothing else about the tree is inspected — the question this
/// answers is "who owns this directory", not "is this plugin healthy".
///
/// A path that does not exist, is not a directory, or cannot be read answers
/// `false`. That is the fail-safe direction for its one consumer: `uninstall`
/// deletes only on `true`, so every uncertainty leaves the tree on disk.
pub fn is_managed_tree(path: &Path) -> bool {
    std::fs::metadata(path.join(MARKER_FILENAME)).is_ok_and(|m| m.is_file())
}

/// Errors from writing a synthesized tree. Every variant is the operator's to
/// act on, and none carries the credential.
#[derive(Debug)]
pub enum WriteError {
    /// `plugins_dir/<id>` exists and is *not* a kernel-written tree. Refused
    /// rather than overwritten: it is somebody else's directory.
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

/// Write the synthesized tree at `dir`, replacing a previous **kernel-written**
/// tree at the same path.
///
/// Ordering is deliberate: the directory is emptied and rebuilt rather than
/// written over, so a reinstall cannot inherit the previous install's
/// `secrets.json` when the new connector carries no credential.
///
/// `secrets.json` is written `0600` and the directory `0700`; the manifest and
/// the marker are `0644`. On a failure part-way through, the caller is expected
/// to call [`remove_managed_tree`] — this function does not clean up after
/// itself, because the caller holds the lifecycle guard that makes removal
/// safe.
pub fn write_connector_tree(
    dir: &Path,
    manifest_text: &str,
    credential: Option<&str>,
) -> Result<(), WriteError> {
    if dir.exists() {
        if !is_managed_tree(dir) {
            // The one failure that must NOT clean up: the directory is
            // somebody else's, and this call has not written a byte.
            return Err(WriteError::Occupied(dir.to_path_buf()));
        }
        std::fs::remove_dir_all(dir)
            .map_err(|e| WriteError::Io(format!("removing {}: {e}", dir.display())))?;
    }
    // Past the ownership gate, `dir` is ours: either it did not exist or it was
    // a kernel-written tree we have just removed. So a failure from here on may
    // — and must — take the partial tree with it. The marker is written last,
    // so a half-written tree is not `is_managed_tree`, and leaving one behind
    // would strand a `secrets.json` that no later call is willing to delete.
    let built = build_tree(dir, manifest_text, credential);
    if built.is_err() {
        let _ = std::fs::remove_dir_all(dir);
    }
    built
}

fn build_tree(dir: &Path, manifest_text: &str, credential: Option<&str>) -> Result<(), WriteError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| WriteError::Io(format!("creating {}: {e}", dir.display())))?;
    set_mode(dir, 0o700)?;

    write_file(&dir.join("manifest.json"), manifest_text.as_bytes(), 0o644)?;

    if let Some(key) = credential {
        let mut secrets = BTreeMap::new();
        secrets.insert(API_KEY_SECRET_NAME.to_string(), key.to_string());
        let body = serde_json::to_vec_pretty(&secrets)
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

/// Remove a kernel-written tree. A no-op on anything else — the check is
/// [`is_managed_tree`], so an operator's directory can never be deleted here
/// however the path was reached.
///
/// Errors are returned rather than swallowed; callers that are already
/// committed (uninstall's DB row is gone by then) log and continue.
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
    // No POSIX mode to set. `secrets.json` is then only as private as the
    // plugins directory itself, which matches every other file the kernel
    // writes on that platform.
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
            tools_allow: Vec::new(),
            request_timeout_ms: None,
            bringup_timeout_ms: None,
        }
    }

    /// The manifest names the secrets **key**, never the secret. Asserted on
    /// the serialized document, because that is the artifact that goes to disk
    /// and into the plugins row.
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

    /// A keyless connector claims no secret. The opposite — a manifest naming
    /// `api_key_secret` with no `secrets.json` beside it — is a bring-up that
    /// fails at the first request with a confusing reason.
    #[test]
    fn a_keyless_connector_claims_no_secret_and_no_placement() {
        let doc = connector(None).manifest_json();
        let block = &doc["mcp_http"];
        assert!(block.get("api_key_secret").is_none(), "{doc}");
        assert!(block.get("api_key_in").is_none(), "{doc}");
    }

    /// An empty box is the unauthenticated connector the operator asked for,
    /// not a credential that happens to be empty.
    #[test]
    fn an_empty_credential_is_no_credential() {
        assert_eq!(connector(Some("")).credential(), None);
        assert_eq!(connector(Some("sk-x")).credential(), Some("sk-x"));
    }

    #[test]
    fn a_written_tree_is_recognisable_as_the_kernels_and_carries_0600_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write_connector_tree(&dir, "{}", Some("sk-credential")).unwrap();

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

    /// The ownership gate. A directory the kernel did not write is refused with
    /// its contents untouched — this is the operator's plugin checkout.
    #[test]
    fn an_unmanaged_directory_is_refused_rather_than_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), "operator's manifest").unwrap();

        let err = write_connector_tree(&dir, "{}", Some("sk-credential")).unwrap_err();
        assert!(matches!(err, WriteError::Occupied(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join("manifest.json")).unwrap(),
            "operator's manifest"
        );
    }

    /// A rewrite is a rebuild: the previous install's `secrets.json` must not
    /// survive into a keyless one.
    #[test]
    fn rewriting_a_managed_tree_drops_the_previous_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plug");
        write_connector_tree(&dir, "{}", Some("sk-credential")).unwrap();
        write_connector_tree(&dir, "{}", None).unwrap();
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
        // And a path that is not there at all is not an error: uninstall runs
        // this after the row is already gone.
        assert_eq!(remove_managed_tree(&tmp.path().join("absent")), Ok(false));
    }

    /// **The symlink case, stated as behaviour rather than as an assumption.**
    ///
    /// `install` materializes a `local_path` plugin as a symlink at
    /// `plugins_dir/<id>`, so an uninstall can be handed a symlink whose target
    /// is an operator's own directory. If somebody plants the marker in that
    /// target, `is_managed_tree` — which follows links to read the file — says
    /// yes. What must still hold is that the operator's files survive, and this
    /// pins whichever way `remove_dir_all` decides: it refuses a symlink rather
    /// than deleting through it.
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
