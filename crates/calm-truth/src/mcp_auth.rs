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
    let secrets_dir = data_dir.join("secrets");
    let token_path = secrets_dir.join("mcp-daemon-token");
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
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "daemon token file exists but is empty",
                ));
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
}
