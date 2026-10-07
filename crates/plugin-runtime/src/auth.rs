//! Per-plugin process token: 32 bytes of `OsRng` randomness, hex-encoded, stored SHA-256-hashed in `plugin_tokens.hashed_token` and handed raw to the child via `NEIGE_PLUGIN_TOKEN`.
//! Raw tokens are not recoverable from the hash: a kernel restart re-handshakes every plugin with a fresh token.

use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// 64-char hex-encoded 32-byte secret. Never `Clone` or `Debug`, so accidental log emissions are caught by the type system.
pub struct PluginToken(String);

impl PluginToken {
    /// Mint a fresh token from OS randomness. Panics only if the OS RNG itself fails.
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

/// SHA-256(token) → hex, the `plugin_tokens.hashed_token` form. Carrying it in a log line still fingerprints the secret, so callers must avoid it.
pub fn hash_token(t: &str) -> String {
    let mut h = Sha256::new();
    h.update(t.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time `hash_token(presented) == stored_hash`; the length-mismatch fast path is only reachable with a malformed stored hash, which an attacker cannot time.
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
        // Birthday-paradox guard: 256 bits of entropy should never collide on 100 draws; if this flakes, RNG is broken.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let t = PluginToken::generate();
            assert_eq!(t.as_str().len(), 64, "32 bytes hex-encoded");
            assert!(seen.insert(t.into_inner()));
        }
    }
}
