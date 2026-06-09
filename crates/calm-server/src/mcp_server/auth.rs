//! Per-card MCP token mint + verify.
//!
//! PR7a (#136) — when a Spec/Worker codex card is minted, the kernel
//! generates a fresh 32-byte hex token and stores `SHA-256(token)` in
//! `card_mcp_tokens.hashed_token`. The raw token is handed to the
//! codex daemon via the `NEIGE_MCP_TOKEN` env var. For worker cards
//! see `spec_card::build_codex_env_map`; for the spec card AI shell,
//! `SpecHarnessStartAdapter::app_server_interact` does per-thread
//! injection (#555 Phase B). From there, a tiny
//! `neige-mcp-stdio-shim` binary inherits the env, connects to the
//! kernel's UDS, and embeds the raw token in `initialize.params._meta`
//! so the kernel can resolve which card is on the other end of the
//! connection.
//!
//! ## Why a separate module from `plugin_host::auth`
//!
//! The shape is identical (32-byte hex, SHA-256, constant-time
//! verification), but the **identity binding** is different: a plugin
//! token authorizes a plugin id, while a card MCP token authorizes a
//! **card identity** which the kernel pulls from the `cards.role`
//! column to gate writes via `enforce_role`. Mirroring the helper
//! here (vs. reusing `PluginToken`) keeps the type system honest about
//! "this token unlocks a card, not a plugin" — a future change to
//! either token's lifecycle won't accidentally cross-wire the other.
//!
//! ## Trust model
//!
//! * Raw token lives in two places: the codex daemon's env (passed by
//!   the kernel at spawn) and the in-flight `initialize` request body.
//!   Neither persists to disk. A kernel restart drops the in-memory
//!   raw; the codex daemon must restart too to receive a fresh token.
//! * The stored hash is non-reversible. An attacker with read access to
//!   the SQLite DB still can't forge a connection without a brute-force
//!   pass against 256 bits of entropy.
//! * Constant-time comparison via `subtle::ConstantTimeEq` so the
//!   `initialize` rejection path doesn't leak per-character timing
//!   that could narrow the brute-force search.

use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use subtle::ConstantTimeEq;

/// 64-char hex-encoded 32-byte secret. Constructed via [`Self::generate`];
/// no `Clone` so accidental log emissions are caught by the type system.
///
/// PR7a uses this for the per-card MCP token. Distinct type from
/// `plugin_host::auth::PluginToken` so the two token namespaces can't be
/// mixed at any call site.
pub struct CardMcpToken(String);

impl CardMcpToken {
    /// Mint a fresh token from OS randomness. Panics only if the OS RNG
    /// itself fails (system in unrecoverable state).
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(hex::encode(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume self and return the inner string. Used when we need to
    /// move the raw value into an env var or a transient struct without
    /// holding a stale copy.
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// SHA-256(token) → hex. Used to derive the `hashed_token` column stored
/// in `card_mcp_tokens`. Not Display'd anywhere — carrying it in a log
/// line is still a leak of "you fingerprinted the secret".
pub fn hash_token(t: &str) -> String {
    let mut h = Sha256::new();
    h.update(t.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time `hash_token(presented) == stored_hash`. We compare hex
/// strings (rather than raw bytes) because the stored form is already
/// hex; hex comparison is still constant-time as long as both inputs
/// are the same length (64 chars). Length-mismatch short-circuits to
/// `false` — that's not a timing leak we care about because the
/// attacker can't time a column-length bug into a useful signal.
pub fn verify_token(presented: &str, stored_hash: &str) -> bool {
    let derived = hash_token(presented);
    if derived.len() != stored_hash.len() {
        return false;
    }
    derived.as_bytes().ct_eq(stored_hash.as_bytes()).into()
}

/// Server-wide MCP daemon token for the shared codex app-server's shim.
///
/// Unlike per-card tokens in `card_mcp_tokens`, this token only establishes
/// daemon trust during `initialize`; card identity still comes from per-call
/// thread metadata / thread mapping. The raw token is persisted under
/// `<data_dir>/secrets/mcp-daemon-token` so shared CODEX_HOME config remains
/// stable across kernel restarts.
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
        // 256-bit entropy never collides on 100 draws absent RNG breakage.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let t = CardMcpToken::generate();
            assert_eq!(t.as_str().len(), 64, "32 bytes hex-encoded");
            assert!(seen.insert(t.into_inner()));
        }
    }

    #[test]
    fn hash_is_deterministic() {
        // Same input → same hash. Belt-and-braces against accidentally
        // wiring in a randomized hash (HMAC, salted KDF) that would
        // break the SELECT-by-hash lookup the MCP server relies on.
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
