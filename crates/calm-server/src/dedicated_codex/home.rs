use super::{Error, Result, policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table};

#[path = "home_io.rs"]
mod io;
#[cfg(test)]
#[path = "home/tests.rs"]
mod tests;

const MAX_SEED_BYTES: u64 = 1_048_576;

/// Only already-configured provider/model values are imported. This value holds
/// provider secrets and intentionally has neither Serialize nor derived Debug.
#[derive(Clone)]
pub struct ProviderSettings {
    document: DocumentMut,
}

#[derive(Clone)]
pub struct HomeSeed {
    settings: ProviderSettings,
    authentication: Vec<u8>,
}

impl std::fmt::Debug for HomeSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HomeSeed([REDACTED])")
    }
}

pub struct NativeMcp {
    pub socket: PathBuf,
    pub card_token: String,
}
impl std::fmt::Debug for NativeMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeMcp")
            .field("socket", &self.socket)
            .field("card_token", &"[REDACTED]")
            .finish()
    }
}

impl HomeSeed {
    /// No directory walk, source rewrite or implicit process-environment import.
    pub fn read(configuration: &Path, authentication: &Path) -> Result<Self> {
        for source in [configuration, authentication] {
            if let Some(parent) = source.parent()
                && parent.join("managed_config.toml").try_exists()?
            {
                return Err(Error::Unsupported(
                    "legacy home-managed configuration needs an explicit requirements import"
                        .into(),
                ));
            }
        }
        let text = String::from_utf8(read_bounded(configuration)?)
            .map_err(|_| Error::Configuration("provider config must be UTF-8".into()))?;
        let source: DocumentMut = text
            .parse()
            .map_err(|_| Error::Configuration("provider config is not valid TOML".into()))?;
        let mut selected = DocumentMut::new();
        for name in [
            "model",
            "model_provider",
            "model_reasoning_effort",
            "model_reasoning_summary",
            "model_verbosity",
            "service_tier",
        ] {
            if let Some(item) = source.get(name) {
                if !item.is_str() {
                    return Err(Error::Configuration(format!("{name} must be a string")));
                }
                selected[name] = toml_edit::value(item.as_str().expect("validated string"));
            }
        }
        if let Some(provider) = source.get("model_provider").and_then(Item::as_str)
            && let Some(table) = source
                .get("model_providers")
                .and_then(|item| item.get(provider))
        {
            selected["model_providers"] = Item::Table(Table::new());
            for name in [
                "name",
                "base_url",
                "wire_api",
                "env_key",
                "requires_openai_auth",
                "http_headers",
                "env_http_headers",
                "request_max_retries",
                "stream_max_retries",
                "stream_idle_timeout_ms",
            ] {
                if let Some(item) = table.get(name) {
                    selected["model_providers"][provider][name] = item.clone();
                }
            }
        }
        let authentication = read_bounded(authentication)?;
        let value: serde_json::Value = serde_json::from_slice(&authentication)
            .map_err(|_| Error::Configuration("authentication is not valid JSON".into()))?;
        if !value.is_object() {
            return Err(Error::Configuration(
                "authentication must be a JSON object".into(),
            ));
        }
        Ok(Self {
            settings: ProviderSettings { document: selected },
            authentication,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeReceipt {
    pub version: u32,
    pub run_id: String,
    pub request_digest: String,
    pub root: PathBuf,
    pub home: PathBuf,
    pub control: PathBuf,
    pub socket: PathBuf,
    pub mcp_source_socket: PathBuf,
    pub mcp_device: u64,
    pub mcp_inode: u64,
    pub policy_digest: String,
    pub authentication_digest: String,
}

pub struct PrivateHome {
    root: PathBuf,
}
impl PrivateHome {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        let root = root.canonicalize()?;
        let metadata = root.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(Error::Configuration(
                "private provider root must be caller-owned mode0700".into(),
            ));
        }
        Self::sync_ancestry(&root)?;
        Ok(Self { root })
    }

    pub fn prepare(
        &self,
        run_id: &str,
        request_digest: &str,
        seed: &HomeSeed,
        native: &NativeMcp,
    ) -> Result<HomeReceipt> {
        valid_segment(run_id)?;
        if request_digest.is_empty() {
            return Err(Error::Configuration(
                "frozen request digest required".into(),
            ));
        }
        let socket = native.socket.canonicalize()?;
        let socket_metadata = socket.metadata()?;
        if !socket_metadata.file_type().is_socket() {
            return Err(Error::Configuration(
                "native MCP endpoint must be a Unix socket".into(),
            ));
        }
        let socket_name = socket
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::Configuration("invalid MCP socket path".into()))?;
        let mut document = seed.settings.document.clone();
        policy::apply(&mut document, &native.card_token, socket_name)?;
        let bytes = document.to_string().into_bytes();
        let root = self.root.join(run_id);
        let receipt = HomeReceipt {
            version: 1,
            run_id: run_id.into(),
            request_digest: request_digest.into(),
            home: root.join("home"),
            control: root.join("control"),
            socket: root.join("control/app-server.sock"),
            root: root.clone(),
            mcp_source_socket: socket,
            mcp_device: socket_metadata.dev(),
            mcp_inode: socket_metadata.ino(),
            policy_digest: digest(&bytes),
            authentication_digest: digest(&seed.authentication),
        };
        if root.try_exists()? {
            let old: HomeReceipt =
                serde_json::from_slice(&read_bounded(&root.join("receipt.json"))?)
                    .map_err(|_| Error::Unknown("private home receipt unreadable".into()))?;
            if old != receipt {
                return Err(Error::Conflict(
                    "run ID already has different provider configuration".into(),
                ));
            }
            Self::verify(&old)?;
            self.publication_barrier(&old)?;
            return Ok(old);
        }
        let staging = self.root.join(format!(".prepare-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
        let result = (|| -> Result<()> {
            for directory in ["home", "control"] {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(staging.join(directory))?;
            }
            write_private(&staging.join("home/auth.json"), &seed.authentication)?;
            write_private(&staging.join("home/config.toml"), &bytes)?;
            write_private(
                &staging.join("receipt.json"),
                &serde_json::to_vec(&receipt)
                    .map_err(|_| Error::Configuration("cannot encode home receipt".into()))?,
            )?;
            io::sync_directory(&staging.join("home"))?;
            io::sync_directory(&staging)?;
            // renameat2 publishes once; a concurrent prepare cannot replace a home.
            use std::os::unix::ffi::OsStrExt;
            let from = std::ffi::CString::new(staging.as_os_str().as_bytes())
                .map_err(|_| Error::Configuration("invalid staging path".into()))?;
            let to = std::ffi::CString::new(root.as_os_str().as_bytes())
                .map_err(|_| Error::Configuration("invalid home path".into()))?;
            #[cfg(test)]
            io::faults::before_publish();
            if unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    from.as_ptr(),
                    libc::AT_FDCWD,
                    to.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.publication_barrier(&receipt)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        if let Err(error) = result {
            if matches!(&error, Error::Io(io) if io.kind() == std::io::ErrorKind::AlreadyExists) {
                let old: HomeReceipt =
                    serde_json::from_slice(&read_bounded(&root.join("receipt.json"))?)
                        .map_err(|_| Error::Unknown("concurrent home receipt unreadable".into()))?;
                if old != receipt {
                    return Err(Error::Conflict(
                        "concurrent provider configuration differs".into(),
                    ));
                }
                Self::verify(&old)?;
                self.publication_barrier(&old)?;
                return Ok(old);
            }
            return Err(error);
        }
        Ok(receipt)
    }

    fn sync_ancestry(root: &Path) -> Result<()> {
        // Reopen must finish a previous creator's failed parent-entry barrier.
        for directory in root.ancestors() {
            io::sync_directory(directory)?;
        }
        Ok(())
    }

    fn publication_barrier(&self, receipt: &HomeReceipt) -> Result<()> {
        for directory in [&receipt.home, &receipt.control, &receipt.root] {
            io::sync_directory(directory)?;
        }
        Self::sync_ancestry(&self.root)
    }

    pub fn verify(receipt: &HomeReceipt) -> Result<()> {
        if receipt.version != 1
            || receipt.home != receipt.root.join("home")
            || receipt.control != receipt.root.join("control")
            || receipt.socket != receipt.control.join("app-server.sock")
            || digest(&read_bounded(&receipt.home.join("config.toml"))?) != receipt.policy_digest
        {
            return Err(Error::Unknown("private provider policy changed".into()));
        }
        let recorded: HomeReceipt =
            serde_json::from_slice(&read_bounded(&receipt.root.join("receipt.json"))?)
                .map_err(|_| Error::Unknown("private home receipt unreadable".into()))?;
        if &recorded != receipt {
            return Err(Error::Unknown(
                "private home receipt identity changed".into(),
            ));
        }
        let socket = receipt.mcp_source_socket.metadata()?;
        if socket.dev() != receipt.mcp_device
            || socket.ino() != receipt.mcp_inode
            || !socket.file_type().is_socket()
        {
            return Err(Error::Unknown(
                "native MCP socket was replaced; endpoint requires reconciliation".into(),
            ));
        }
        // Authentication may legitimately refresh; it is never imported again on
        // reconnect or silently compared to today's host credentials.
        Ok(())
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn valid_segment(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(Error::Configuration(
            "invalid dedicated run identifier".into(),
        ));
    }
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Configuration(
            "provider seed must be a regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SEED_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SEED_BYTES {
        return Err(Error::Configuration("provider file exceeds limit".into()));
    }
    Ok(bytes)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Only provider transport settings; no incidental host environment or MCP keys.
pub(crate) fn provider_environment(
    values: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::from([
        ("CODEX_HOME".into(), policy::PROVIDER_HOME.into()),
        ("HOME".into(), "/provider".into()),
        ("PATH".into(), policy::EXECUTOR_PATH.into()),
        ("LANG".into(), "C.UTF-8".into()),
    ]);
    for (name, value) in values {
        if !matches!(
            name.as_str(),
            "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "ALL_PROXY"
                | "NO_PROXY"
                | "SSL_CERT_FILE"
                | "SSL_CERT_DIR"
        ) {
            return Err(Error::Configuration(
                "provider environment key is not allowed".into(),
            ));
        }
        result.insert(name.clone(), value.clone());
    }
    Ok(result)
}
