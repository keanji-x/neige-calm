use std::path::Path;

use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::{
    card_mcp_token_set_tx, session_mcp_token_set_if_active_tx, session_mcp_token_set_tx,
};
use crate::error::Result;
use crate::mcp_server::auth::{CardMcpToken, hash_token};
use crate::model::CardRole;

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

/// codex does NOT inherit the daemon process env into exec-shells; the per-thread `shell_environment_policy.set` field is the
/// ONLY channel that reaches the `neige` CLI an agent must run. `path` is the kernel-led PATH: `set` is re-applied after codex
/// restores its login-shell snapshot, so `neige` stays the running kernel's CLI (#1784). The card role additionally selects the
/// Planner Terminal approval policy.
pub(crate) fn card_mcp_thread_start_config(
    socket_path: &Path,
    raw_token: &str,
    role: CardRole,
    path: &str,
) -> serde_json::Value {
    let mut set = serde_json::Map::new();
    for (key, value) in card_mcp_env(socket_path, raw_token) {
        set.insert(key.to_string(), serde_json::Value::String(value));
    }
    // A PATH in `set` stops codex re-adding its own PATH entries after the snapshot
    // (codex-rs core/src/tools/runtimes/mod.rs:140): the per-process arg0 tempdir
    // (apply_patch/applypatch/codex-linux-sandbox/codex-execve-wrapper aliases) and, in
    // snapshot mode, the bundled-rg package dir. Acceptable: codex intercepts apply_patch
    // in-process and runs the sandbox and execve wrapper by absolute path. Lost: shell forms
    // the apply_patch parser does not recognise (e.g. `cat p | apply_patch`) and bundled rg
    // on a host without rg. `set` values are literal and the arg0 dir is a random tempdir,
    // so no prepend-style carrier exists.
    set.insert("PATH".into(), serde_json::Value::String(path.into()));
    let mut config = serde_json::json!({
        "shell_environment_policy": {
            "set": set,
        },
    });
    // Delegated only for a Planner thread; the kernel remains the live role/Track/session/control authority.
    if role == CardRole::Planner {
        config["mcp_servers"] = serde_json::json!({"calm":{"tools":{
            "calm.terminal.open":{"approval_mode":"approve"},
            "calm.terminal.control":{"approval_mode":"approve"},
            "calm.terminal.input":{"approval_mode":"approve"}
        }}});
    }
    config
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

/// A Claude Planner harness's first-turn mint (#1791 §5.1): the card row and the session hash in
/// the caller's one transaction, the session write through the guarded writer that refuses a row
/// that is no longer active. The plaintext is returned to the harness only.
pub async fn mint_and_persist_claude_planner_token(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
) -> Result<String> {
    let (raw, hashed) = set_card_mcp_token(tx, card_id).await?;
    session_mcp_token_set_if_active_tx(tx, worker_session_id, &hashed).await?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx};
    use crate::model::{CardRole, NewArea, NewCard, NewTrack, new_id};

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

    /// The single channel-3 producer used by ALL spawn paths emits the exact `shell_environment_policy.set.{NEIGE_MCP_SOCKET,NEIGE_MCP_TOKEN}` shape.
    #[test]
    fn card_mcp_thread_start_config_emits_channel_3_shape() {
        let cfg = card_mcp_thread_start_config(
            Path::new("/tmp/kernel.sock"),
            "raw-token",
            CardRole::Worker,
            "/k/bin:/usr/bin",
        );
        assert_eq!(
            cfg,
            serde_json::json!({
                "shell_environment_policy": {
                    "set": {
                        "NEIGE_MCP_SOCKET": "/tmp/kernel.sock",
                        "NEIGE_MCP_TOKEN": "raw-token",
                        "PATH": "/k/bin:/usr/bin",
                    }
                }
            })
        );
    }

    #[test]
    fn planner_terminal_policy_never_grants_other_roles() {
        for role in [
            CardRole::Planner,
            CardRole::Assistant,
            CardRole::Worker,
            CardRole::ReportCard,
        ] {
            let cfg = card_mcp_thread_start_config(
                Path::new("/tmp/kernel.sock"),
                "raw-token",
                role,
                "/k/bin:/usr/bin",
            );
            if role == CardRole::Planner {
                assert_eq!(
                    cfg["mcp_servers"],
                    serde_json::json!({"calm":{"tools":{
                        "calm.terminal.open":{"approval_mode":"approve"},
                        "calm.terminal.control":{"approval_mode":"approve"},
                        "calm.terminal.input":{"approval_mode":"approve"}
                    }}})
                );
            } else {
                assert!(
                    cfg.get("mcp_servers").is_none(),
                    "unexpected provider delegation for {role:?}"
                );
            }
            assert!(cfg.get("approval_policy").is_none());
            assert!(cfg.get("sandbox_mode").is_none());
        }
    }

    #[test]
    fn terminal_policy_keeps_truthful_annotations_and_exact_write_inventory() {
        let cfg = card_mcp_thread_start_config(
            Path::new("/tmp/kernel.sock"),
            "raw-token",
            CardRole::Planner,
            "/k/bin:/usr/bin",
        );
        let granted: std::collections::BTreeSet<_> = cfg["mcp_servers"]["calm"]["tools"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let descriptors = crate::mcp_server::build_default_registry().descriptors();
        let mut writes = std::collections::BTreeSet::new();
        for descriptor in descriptors
            .iter()
            .filter(|d| d.name.starts_with("calm.terminal."))
        {
            assert_eq!(descriptor.visible_to_roles, &[CardRole::Planner]);
            let read = matches!(
                descriptor.name.as_str(),
                "calm.terminal.resolve" | "calm.terminal.observe"
            );
            assert_eq!(
                descriptor.annotations,
                Some(serde_json::json!({
                    "readOnlyHint":read,"destructiveHint":!read,"openWorldHint":true
                }))
            );
            if !read {
                writes.insert(descriptor.name.clone());
            }
        }
        assert_eq!(
            granted, writes,
            "new write capabilities require an explicit policy decision"
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
        let area = repo
            .area_create(NewArea {
                name: "wiring-mcp-token".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "wiring-mcp-token".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
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
                track_id: track.id,
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({"planner_provider": "codex"}),
            },
            CardRole::Planner,
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
