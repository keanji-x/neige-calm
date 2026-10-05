//! #2087 B5: two connectors whose tools mint one name start concurrently; one claims the name.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::plugin_results::registry_name;
use serde_json::json;

/// `plugin_ab_c_d` from both: `ab` + `c_d` and `ab-c` + `d` (distinct `plugin_<id>_` prefixes, so
/// admission lets both start).
const PLUGINS: [(&str, &str, &str); 2] = [("ab", "c_d", "ab.fifo"), ("ab-c", "d", "abc.fifo")];

/// A `cli-query` connector whose `--version` probe is a rendezvous: it wakes the other connector's
/// probe and waits to be woken, so both bring-ups end only once both are in flight, and both spawns
/// then reach the minted-name claim together.
fn write_connector(
    plugins_dir: &Path,
    fifos: &Path,
    (id, tool, own): (&str, &str, &str),
    other: &str,
) {
    let dir = plugins_dir.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("cli.sh");
    let (own, other) = (fifos.join(own), fifos.join(other));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then\n  (printf 'go\\n' > '{}') &\n  \
             read _ < '{}'\n  wait\n  echo v1\nfi\n",
            other.display(),
            own.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": 1, "kind": "cli-query", "id": id, "version": "0.1.0",
            "min_kernel_version": "0.0.1", "display_name": id,
            "cli_query": {
                "command": script.display().to_string(), "timeout_ms": 5_000,
                "tools": [{
                    "name": tool, "description": "A tool",
                    "input_schema": {"type": "object", "properties": {}, "additionalProperties": false},
                    "args": []
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn concurrent_connectors_claim_a_minted_name_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (plugins_dir, data_dir, fifos) = (
        tmp.path().join("plugins"),
        tmp.path().join("data"),
        tmp.path().join("fifos"),
    );
    for dir in [&plugins_dir, &data_dir, &fifos] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for (_, _, fifo) in PLUGINS {
        let path = fifos.join(fifo);
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success(), "mkfifo {}", path.display());
    }
    write_connector(&plugins_dir, &fifos, PLUGINS[0], PLUGINS[1].2);
    write_connector(&plugins_dir, &fifos, PLUGINS[1], PLUGINS[0].2);
    let minted = registry_name(PLUGINS[0].0, PLUGINS[0].1);
    assert_eq!(minted, registry_name(PLUGINS[1].0, PLUGINS[1].1));

    let sqlx = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx.clone();
    for (id, _, _) in PLUGINS {
        repo.plugin_install(calm_server::model::NewPlugin {
            id: id.into(),
            version: "0.1.0".into(),
            install_path: plugins_dir.join(id).display().to_string(),
            manifest: json!({}),
            enabled: true,
            user_config: json!({}),
        })
        .await
        .unwrap();
    }
    let (registry, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo,
        plugins_dir.clone(),
        data_dir,
        Vec::new(),
        EventBus::new(),
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));

    let (first, second) = tokio::join!(host.spawn(PLUGINS[0].0), host.spawn(PLUGINS[1].0));
    let running = host.running_plugin_ids().await;
    assert_eq!(
        running.len(),
        1,
        "exactly one serves `{minted}`: {running:?}"
    );
    let winner = running.iter().next().unwrap().clone();
    let loser = PLUGINS
        .iter()
        .map(|(id, _, _)| *id)
        .find(|id| *id != winner)
        .unwrap();
    let (won, lost) = if winner == PLUGINS[0].0 {
        (first, second)
    } else {
        (second, first)
    };
    won.expect("the winner spawns");
    assert!(lost.is_err(), "the loser's spawn fails: {lost:?}");

    let served: Vec<String> = host
        .registry()
        .get(&winner)
        .unwrap()
        .exposes_tools
        .iter()
        .map(|tool| registry_name(&winner, &tool.name))
        .collect();
    assert_eq!(served, vec![minted.clone()]);
    assert!(host.registry().get(loser).unwrap().exposes_tools.is_empty());
    match host.status(loser).await.map(|status| status.status) {
        Some(PluginRuntimeStatus::Unavailable { reason }) => assert_eq!(
            reason,
            format!(
                "plugin `{loser}` mints `{minted}`, which running plugin `{winner}` already mints"
            )
        ),
        other => panic!("the loser is refused with the collision: {other:?}"),
    }
}
