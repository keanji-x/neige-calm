//! End-to-end test for the `neige.*` host-callback router: a real `PluginHost`
//! spawns `stub-plugin-caller`, which issues a fixed sequence of callbacks.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewTrack};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use serde_json::json;
use tokio::time::{Instant, sleep};

const CALLER_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-caller");

async fn boot_with_track(
    plugin_id: &str,
) -> (Arc<PluginHost>, Arc<dyn Repo>, String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(CALLER_BIN), bin_dir.join("stub")).unwrap();

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let area = repo
        .area_create(NewArea {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "demo".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Caller stub",
        "entrypoint": {
            "command": "bin/stub",
            "env": { "NEIGE_DEMO_TRACK": track.id.clone() }
        },
        "permissions": {
            "overlays_write": ["track", "card"],
            "cards_create": true,
            "cards_read_all": true,
            "events_subscribe": ["*"],
            "kv_quota_bytes": 1048576
        }, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    let events = EventBus::new();
    // Seed the plugins row so plugin_token_set's FK is satisfied at spawn time.
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        // `repo.clone()` (method call) coerces `Arc<dyn Repo>` → `Arc<dyn RouteRepo>`; `Arc::clone(&repo)` would not.
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));
    (host, repo, track.id.to_string(), tmp)
}

async fn wait_for_running(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn caller_stub_drives_neige_callbacks_end_to_end() {
    let plugin_id = "test.caller";
    let (host, repo, track_id, _tmp) = boot_with_track(plugin_id).await;

    host.spawn(plugin_id).await.expect("spawn caller stub");
    wait_for_running(&host, plugin_id).await;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let kv = repo.plugin_kv_get(plugin_id, "answer").await.unwrap();
        let cards = repo.cards_by_track(&track_id).await.unwrap();
        let demo_card = cards
            .iter()
            .find(|c| c.kind == format!("plugin:{plugin_id}:demo"));
        let terminal_card = cards.iter().find(|c| c.kind == "terminal");
        let other_card = cards.iter().find(|c| c.kind == "plugin:other.plugin:x");

        if let (Some(kv_val), Some(demo), Some(_term)) = (kv.as_ref(), demo_card, terminal_card) {
            assert_eq!(kv_val, &json!(42), "kv.set must roundtrip through router");

            assert_eq!(
                demo.payload,
                json!({ "hello": "world" }),
                "card payload must round-trip"
            );

            assert!(
                other_card.is_none(),
                "card with other plugin's prefix must be rejected; saw: {:?}",
                other_card
            );

            let overlays = repo.overlays_for("track", &track_id).await.unwrap();
            let our_overlay = overlays
                .iter()
                .find(|o| o.plugin_id == plugin_id && o.kind == "status")
                .expect("status overlay must be present");
            assert_eq!(our_overlay.payload, json!({ "state": "running" }));

            host.stop(plugin_id).await.ok();
            return;
        }

        if Instant::now() > deadline {
            let stderr_tail = host
                .stderr_tail(plugin_id, 50)
                .await
                .unwrap_or_default()
                .join("\n");
            panic!(
                "callbacks did not land within budget. kv={kv:?} cards={cards:?}\n--- stub stderr ---\n{stderr_tail}"
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}
