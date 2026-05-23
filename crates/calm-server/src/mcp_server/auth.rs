//! Per-card MCP token mint + verify.
//!
//! PR7a (#136) — when a Spec/Worker codex card is minted, the kernel
//! generates a fresh 32-byte hex token and stores `SHA-256(token)` in
//! `card_mcp_tokens.hashed_token`. The raw token is handed to the
//! codex daemon via the `NEIGE_MCP_TOKEN` env var (see
//! `spec_card::build_codex_env_map`); from there, a tiny
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
