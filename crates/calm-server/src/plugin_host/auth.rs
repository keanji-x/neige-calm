//! Slice H — per-plugin authentication primitives.
//!
//! **M5 (m3-mcp-apps) note:** the iframe-cookie half of Slice H is gone. Under
//! MCP Apps, the iframe ↔ host channel is owned by `AppBridge` (postMessage
//! over a `MessageChannel` the host minted). The host already proves authority
//! over the kernel by sitting in the same browser session that holds the
//! desktop-local server's CORS-allowed origin, and tool calls from the iframe
//! are forwarded through `POST /api/plugins/:id/tool-call` which gates on the
//! `neige.*` prefix (see migration doc §3.3 + §7.6 row 5). The second cookie
//! was belt-and-braces over a wire we no longer speak.
//!
//! What remains is the **process token**:
//!
//!   * 32 bytes of `OsRng` randomness, hex-encoded into a 64-char string. Stored
//!     hashed (SHA-256) on disk in `plugin_tokens.hashed_token`. The kernel
//!     hands the **raw** token to the child process via `NEIGE_PLUGIN_TOKEN` on
//!     spawn; the plugin echoes it back inside the `initialize` request's
//!     `params._meta["dev.neige/auth"].expected_echo` slot (M1 wire shape,
//!     post the spec-blessed-`_meta` migration), and the kernel verifies via
//!     `verify_token`. Mismatch → kill + Crashed, no respawn (see
//!     `plugin_host::mod::spawn`).
//!
//!     ⚠️ Raw tokens are **not recoverable** from the hash. That's by design:
//!     a kernel restart drops the in-memory raw, so plugins re-handshake with
//!     a fresh token on the next boot. Restart is a security boundary.
//!
//! Hash choice: SHA-256 is correct here. We're not protecting low-entropy
//! human passwords — we're proving the bearer of a 256-bit secret matches the
//! one we issued. bcrypt/argon2 would only slow us down.

use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

// ---------------------------------------------------------------------------
// Process token
// ---------------------------------------------------------------------------

/// 64-char hex-encoded 32-byte secret. Constructed via `generate`; never
/// `Clone` so accidental log emissions are caught by the type system (we
/// could derive `Debug` redacting, but skipping `Debug` entirely makes the
/// intent louder).
pub struct PluginToken(String);

impl PluginToken {
    /// Mint a fresh token from OS randomness. Panics only if the OS RNG
    /// itself fails, which would mean the system is in unrecoverable state.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(hex::encode(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume self and return the inner string. Used when we need to move
    /// the raw value into an env var without holding a stale copy.
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// SHA-256(token) → hex. Used to derive the `hashed_token` column stored in
/// `plugin_tokens`. Not Display'd anywhere; carrying it in a log line is
/// still a leak of "you fingerprinted the secret", so callers must avoid it.
pub fn hash_token(t: &str) -> String {
    let mut h = Sha256::new();
    h.update(t.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time `hash_token(presented) == stored_hash`. We compare hex
/// strings (rather than raw bytes) because the stored form is already hex —
/// hex comparison is still constant-time as long as both inputs are the same
/// length, which they will be (64 chars). If a malformed `stored_hash` slips
/// in (bad migration) the length-mismatch branch returns false fast; that's
/// not a timing leak we care about — the attacker can't time the bug.
pub fn verify_token(presented: &str, stored_hash: &str) -> bool {
    let derived = hash_token(presented);
    if derived.len() != stored_hash.len() {
        return false;
    }
    derived.as_bytes().ct_eq(stored_hash.as_bytes()).into()
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_token_round_trips() {
        let t = PluginToken::generate();
        let h = hash_token(t.as_str());
        assert!(verify_token(t.as_str(), &h));
    }

    #[test]
    fn verify_rejects_wrong_token() {
        let a = PluginToken::generate();
        let b = PluginToken::generate();
        let h = hash_token(a.as_str());
        assert!(!verify_token(b.as_str(), &h));
    }

    #[test]
    fn verify_rejects_garbage_hash() {
        let t = PluginToken::generate();
        assert!(!verify_token(t.as_str(), "not-a-real-sha256"));
        assert!(!verify_token(t.as_str(), ""));
    }

    #[test]
    fn generate_yields_unique_tokens() {
        // Pure birthday-paradox guard — 256 bits of entropy should never
        // collide on 100 draws. If this ever flakes, RNG is broken.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let t = PluginToken::generate();
            assert_eq!(t.as_str().len(), 64, "32 bytes hex-encoded");
            assert!(seen.insert(t.into_inner()));
        }
    }
}
