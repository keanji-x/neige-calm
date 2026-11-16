//! Per-card MCP token mint + verify. The raw token lives only in the daemon's
//! env and the in-flight `initialize` body; only `SHA-256(token)` is stored.

use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use subtle::ConstantTimeEq;

/// 64-char hex-encoded 32-byte secret; no `Clone` so accidental log emissions
/// are caught by the type system. Distinct from `PluginToken` on purpose.
pub struct CardMcpToken(String);

impl CardMcpToken {
    /// Panics only if the OS RNG itself fails.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(hex::encode(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

/// SHA-256(token) → hex. Not Display'd anywhere — even the hash in a log line is a leak.
pub fn hash_token(t: &str) -> String {
    let mut h = Sha256::new();
    h.update(t.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time compare of hex strings (same length by construction); a
/// length mismatch short-circuits, which is not a useful timing signal.
pub fn verify_token(presented: &str, stored_hash: &str) -> bool {
    let derived = hash_token(presented);
    if derived.len() != stored_hash.len() {
        return false;
    }
    derived.as_bytes().ct_eq(stored_hash.as_bytes()).into()
}

/// Server-wide MCP daemon token: establishes daemon trust during `initialize`
/// only; card identity still comes from per-call thread metadata. Persisted
/// under `<data_dir>/secrets/mcp-daemon-token` so CODEX_HOME config stays stable.
pub fn get_or_generate_daemon_token(data_dir: &Path) -> io::Result<String> {
    get_or_generate_secret_file(
        data_dir,
        "mcp-daemon-token",
        "daemon token file exists but is empty",
    )
}

/// Server-wide secret key used to deterministically derive per-session MCP tokens
/// from `(worker_session_id, mcp_token_epoch)` without persisting plaintext tokens
/// in SQLite. Persisted under `<data_dir>/secrets/mcp-session-key`.
#[derive(Clone)]
pub struct SessionMcpKey(String);

impl SessionMcpKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SessionMcpKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionMcpKey([REDACTED])")
    }
}

pub fn get_or_generate_session_mcp_key(data_dir: &Path) -> io::Result<SessionMcpKey> {
    get_or_generate_secret_file(
        data_dir,
        "mcp-session-key",
        "session MCP key file exists but is empty",
    )
    .map(SessionMcpKey)
}

/// Deterministically derive a 64-char hex MCP token for `(worker_session_id, epoch)`
/// using HMAC-SHA256 keyed by `master_key`.
pub fn derive_session_mcp_token(
    master_key: &SessionMcpKey,
    worker_session_id: &str,
    epoch: i64,
) -> String {
    const BLOCK_SIZE: usize = 64;
    let key_bytes = master_key.as_str().as_bytes();
    let mut key_block = [0u8; BLOCK_SIZE];
    if key_bytes.len() > BLOCK_SIZE {
        let digest = Sha256::digest(key_bytes);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key_bytes.len()].copy_from_slice(key_bytes);
    }

    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(b"neige-session-mcp-v1\0");
    inner.update(worker_session_id.as_bytes());
    inner.update(b"\0");
    inner.update(epoch.to_le_bytes());
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    hex::encode(outer.finalize())
}

fn get_or_generate_secret_file(
    data_dir: &Path,
    filename: &str,
    empty_msg: &str,
) -> io::Result<String> {
    let secrets_dir = data_dir.join("secrets");
    let token_path = secrets_dir.join(filename);
    fs::create_dir_all(&secrets_dir)?;
    fs::set_permissions(&secrets_dir, fs::Permissions::from_mode(0o700))?;
    match fs::read_to_string(&token_path) {
        Ok(token) if !token.trim().is_empty() => {
            fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;
            return Ok(token.trim().to_string());
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    let token = CardMcpToken::generate().into_inner();
    match write_daemon_token_file(&token_path, &token) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let token = fs::read_to_string(&token_path)?;
            fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;
            let token = token.trim();
            if token.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, empty_msg));
            }
            return Ok(token.to_string());
        }
        Err(e) => return Err(e),
    }
    Ok(token)
}

pub fn write_daemon_token_file(path: &Path, token: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn hash_token_round_trips() {
        let t = CardMcpToken::generate();
        let h = hash_token(t.as_str());
        assert!(verify_token(t.as_str(), &h));
    }

    #[test]
    fn verify_rejects_wrong_token() {
        let a = CardMcpToken::generate();
        let b = CardMcpToken::generate();
        let h = hash_token(a.as_str());
        assert!(!verify_token(b.as_str(), &h));
    }

    #[test]
    fn verify_rejects_garbage_hash() {
        let t = CardMcpToken::generate();
        assert!(!verify_token(t.as_str(), "not-a-real-sha256"));
        assert!(!verify_token(t.as_str(), ""));
    }

    #[test]
    fn generate_yields_unique_tokens() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let t = CardMcpToken::generate();
            assert_eq!(t.as_str().len(), 64, "32 bytes hex-encoded");
            assert!(seen.insert(t.into_inner()));
        }
    }

    #[test]
    fn hash_is_deterministic() {
        // Guards against a randomized hash (HMAC, salted KDF) that would break the
        // SELECT-by-hash lookup.
        let raw = "deadbeef".repeat(8);
        assert_eq!(hash_token(&raw), hash_token(&raw));
    }

    #[test]
    fn daemon_token_is_persisted_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let first = get_or_generate_daemon_token(tmp.path()).unwrap();
        let second = get_or_generate_daemon_token(tmp.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn daemon_token_file_has_0600_perms() {
        let tmp = tempfile::tempdir().unwrap();
        let _token = get_or_generate_daemon_token(tmp.path()).unwrap();

        let token_path = tmp.path().join("secrets/mcp-daemon-token");
        let mode = fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "daemon token must be 0600: got {mode:o}");

        let dir_mode = fs::metadata(tmp.path().join("secrets"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "secrets dir must be 0700: got {dir_mode:o}"
        );
    }

    #[test]
    fn daemon_token_creation_uses_o600() {
        let tmp = tempfile::tempdir().unwrap();
        let token_path = tmp.path().join("secrets/mcp-daemon-token");

        write_daemon_token_file(&token_path, "TKN-123").unwrap();

        let mode = fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "daemon token must be 0600: got {mode:o}");
        assert_eq!(
            fs::read_to_string(&token_path).unwrap(),
            "TKN-123\n",
            "daemon token writer should persist the token once"
        );
        assert_eq!(
            write_daemon_token_file(&token_path, "TKN-456")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists,
            "daemon token writer must refuse to replace an existing token"
        );
    }

    #[test]
    fn session_mcp_key_and_derivation_are_stable_and_epoch_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let k1 = get_or_generate_session_mcp_key(tmp.path()).unwrap();
        let k2 = get_or_generate_session_mcp_key(tmp.path()).unwrap();
        assert_eq!(k1.as_str(), k2.as_str());
        assert_eq!(k1.as_str().len(), 64);
        assert_eq!(format!("{k1:?}"), "SessionMcpKey([REDACTED])");

        let t0 = derive_session_mcp_token(&k1, "ws-1", 0);
        let t0_again = derive_session_mcp_token(&k1, "ws-1", 0);
        let t1 = derive_session_mcp_token(&k1, "ws-1", 1);
        let other_ws = derive_session_mcp_token(&k1, "ws-2", 0);
        assert_eq!(t0, t0_again);
        assert_eq!(t0.len(), 64);
        assert_ne!(t0, t1);
        assert_ne!(t0, other_ws);
    }
    #[test]
    fn session_mcp_key_file_permissions_and_empty_file_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let key = get_or_generate_session_mcp_key(tmp.path()).unwrap();
        let path = tmp.path().join("secrets/mcp-session-key");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), key.as_str());
        fs::write(&path, "\n").unwrap();
        assert_eq!(
            get_or_generate_session_mcp_key(tmp.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\n",
            "never replace an existing master key"
        );
    }

    #[test]
    fn session_mcp_derivation_matches_hmac_sha256_vector() {
        // Independent HMAC-SHA256 vector over the documented domain, session and LE epoch.
        assert_eq!(
            derive_session_mcp_token(&SessionMcpKey("k".repeat(64)), "ws-1", 7),
            "a7274f4b4da6b28d7e270c5f412fbc572656458a30088af91db1d1c6adcff3ab"
        );
    }
}
