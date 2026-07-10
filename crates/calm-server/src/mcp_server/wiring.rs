use std::path::Path;

use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::{card_mcp_token_set_tx, session_mcp_token_set_tx};
use crate::error::Result;
use crate::mcp_server::auth::{CardMcpToken, hash_token};

/// Pure per-card MCP environment assembler.
pub fn card_mcp_env(socket_path: &Path, raw_token: &str) -> [(&'static str, String); 2] {
    [
        (
            "NEIGE_MCP_SOCKET",
            socket_path.to_string_lossy().into_owned(),
        ),
        ("NEIGE_MCP_TOKEN", raw_token.to_string()),
    ]
}

/// Builds the per-card `thread/start` `config` that injects the MCP env into
/// the daemon's AI exec-shells. codex does NOT inherit the daemon process env
/// into exec-shells; the per-thread `shell_environment_policy.set` field is the
/// ONLY channel that reaches the `neige` CLI an agent must run to report its
/// task. Both the spec harness (`spec_harness_start_adapter`) and the codex
/// worker spawn (`codex_adapter`) emit this same shape.
pub(crate) fn card_mcp_thread_start_config(
    socket_path: &Path,
    raw_token: &str,
) -> serde_json::Value {
    let mut set = serde_json::Map::new();
    for (key, value) in card_mcp_env(socket_path, raw_token) {
        set.insert(key.to_string(), serde_json::Value::String(value));
    }
    serde_json::json!({
        "shell_environment_policy": {
            "set": set,
        },
    })
}

/// Daemon-shim MCP environment assembler.
pub fn daemon_shim_env(socket_path: &Path, daemon_token: &str) -> [(&'static str, String); 2] {
    [
        (
            "NEIGE_MCP_SOCKET",
            socket_path.to_string_lossy().into_owned(),
        ),
        ("NEIGE_MCP_DAEMON_TOKEN", daemon_token.to_string()),
    ]
}

pub(crate) fn mint_card_mcp_token_pair() -> (String, String) {
    let raw = CardMcpToken::generate().into_inner();
    let hashed = hash_token(&raw);
    (raw, hashed)
}

pub(crate) async fn persist_card_mcp_token_hash(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    hash: &str,
) -> Result<()> {
    card_mcp_token_set_tx(tx, card_id, hash).await?;
    Ok(())
}

pub async fn set_card_mcp_token(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<(String, String)> {
    let (raw, hashed) = mint_card_mcp_token_pair();
    persist_card_mcp_token_hash(tx, card_id, &hashed).await?;
    Ok((raw, hashed))
}

pub async fn mirror_session_mcp_token(
    tx: &mut Transaction<'_, Sqlite>,
    runtime_id: &str,
    hash: &str,
) -> Result<()> {
    session_mcp_token_set_tx(tx, runtime_id, hash).await?;
    Ok(())
}

pub async fn mint_and_persist_card_token(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    runtime_id: &str,
) -> Result<String> {
    let (raw, hashed) = set_card_mcp_token(tx, card_id).await?;
    mirror_session_mcp_token(tx, runtime_id, &hashed).await?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::Value;

    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx};
    use crate::model::{CardRole, NewCard, NewCove, NewWave, new_id};

    #[test]
    fn card_mcp_env_emits_per_card_keys_in_order() {
        assert_eq!(
            card_mcp_env(Path::new("/tmp/kernel.sock"), "raw-token"),
            [
                ("NEIGE_MCP_SOCKET", "/tmp/kernel.sock".to_string()),
                ("NEIGE_MCP_TOKEN", "raw-token".to_string()),
            ]
        );
    }

    /// #838 (lean Move 1): the single channel-3 producer used by ALL spawn
    /// paths (spec, worker, cold-respawn) emits the exact
    /// `shell_environment_policy.set.{NEIGE_MCP_SOCKET,NEIGE_MCP_TOKEN}` shape.
    /// Pinning the byte shape here locks the contract every producer now goes
    /// through after the spec path's parallel `SpecThread*` structs were
    /// deleted — i.e. the unification holds at the function level.
    #[test]
    fn card_mcp_thread_start_config_emits_channel_3_shape() {
        let cfg = card_mcp_thread_start_config(Path::new("/tmp/kernel.sock"), "raw-token");
        assert_eq!(
            cfg,
            serde_json::json!({
                "shell_environment_policy": {
                    "set": {
                        "NEIGE_MCP_SOCKET": "/tmp/kernel.sock",
                        "NEIGE_MCP_TOKEN": "raw-token",
                    }
                }
            })
        );
    }

    #[test]
    fn daemon_shim_env_emits_daemon_token_key_only() {
        let env = daemon_shim_env(Path::new("/tmp/kernel.sock"), "daemon-token");
        assert_eq!(
            env,
            [
                ("NEIGE_MCP_SOCKET", "/tmp/kernel.sock".to_string()),
                ("NEIGE_MCP_DAEMON_TOKEN", "daemon-token".to_string()),
            ]
        );
        assert!(!env.iter().any(|(key, _)| *key == "NEIGE_MCP_TOKEN"));
    }

    #[tokio::test]
    async fn set_card_mcp_token_rotates_and_returns_matching_hashes() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let cove = repo
            .cove_create(NewCove {
                name: "wiring-mcp-token".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: cove.id,
                title: "wiring-mcp-token".into(),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card_id = new_id();
        let role_cache = CardRoleCache::new();
        let mut tx = repo.pool().begin().await.unwrap();
        card_create_with_id_tx(
            &mut tx,
            card_id.clone(),
            NewCard {
                wave_id: wave.id,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            },
            CardRole::Spec,
            true,
            &role_cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        let (raw_a, hash_a) = set_card_mcp_token(&mut tx, &card_id).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        let (raw_b, hash_b) = set_card_mcp_token(&mut tx, &card_id).await.unwrap();
        tx.commit().await.unwrap();

        assert_ne!(raw_a, raw_b);
        assert_eq!(hash_token(&raw_a), hash_a);
        assert_eq!(hash_token(&raw_b), hash_b);
        assert!(
            repo.card_mcp_token_lookup_by_hash(&hash_a)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repo.card_mcp_token_lookup_by_hash(&hash_b).await.unwrap(),
            Some((card_id, hash_b))
        );
    }
}
