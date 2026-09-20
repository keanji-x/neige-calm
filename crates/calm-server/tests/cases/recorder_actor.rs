//! RECORD_SESSION recorder × `BroadcastEnvelope.actor`: recorded lines carry the producing actor.

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, area_create_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::NewArea;
use calm_server::replay::spawn_session_recorder;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

async fn boot() -> (
    Arc<dyn Repo>,
    EventBus,
    CardRoleCache,
    calm_server::track_area_cache::TrackAreaCache,
    NamedTempFile,
) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory repo"),
    );
    let bus = EventBus::new();
    let cache = CardRoleCache::new();
    let wcc = calm_server::track_area_cache::TrackAreaCache::new();
    let tmp = NamedTempFile::new().expect("tempfile");
    spawn_session_recorder(&bus, tmp.path().to_path_buf());
    // The recorder subscribes inside `tokio::spawn`; give it a tick to land its subscription.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    (repo, bus, cache, wcc, tmp)
}

async fn create_area_as(
    repo: &dyn Repo,
    bus: &EventBus,
    cache: &CardRoleCache,
    wcc: &calm_server::track_area_cache::TrackAreaCache,
    actor: ActorId,
    name: &str,
) -> i64 {
    let p = NewArea {
        name: name.to_string(),
        color: "#000".into(),
        sort: None,
    };
    let (_area, event_id) = write_with_event_typed(
        repo,
        actor,
        EventScope::System,
        None,
        bus,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        move |tx| {
            Box::pin(async move {
                let c = area_create_tx(tx, p).await?;
                Ok((c.clone(), Event::AreaUpdated(c)))
            })
        },
    )
    .await
    .expect("write_with_event ok");
    event_id
}

fn read_recorded(tmp: &NamedTempFile) -> Vec<Value> {
    let file = std::fs::File::open(tmp.path()).expect("reopen session file");
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(&l).expect("recorded line is JSON"))
        .collect()
}

#[tokio::test]
async fn recorder_captures_real_actor_per_envelope() {
    let (repo, bus, cache, wcc, tmp) = boot().await;
    let _id_user = create_area_as(&*repo, &bus, &cache, &wcc, ActorId::User, "u").await;
    let _id_plugin = create_area_as(
        &*repo,
        &bus,
        &cache,
        &wcc,
        ActorId::Plugin("plugin-7".into()),
        "a",
    )
    .await;

    let _kernel_id = repo
        .log_pure_event(
            ActorId::Kernel,
            EventScope::System,
            None,
            &bus,
            &cache,
            &wcc,
            Event::PluginState {
                id: "todo".into(),
                state: "Running".into(),
                last_error: None,
            },
        )
        .await
        .expect("log_pure_event ok");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let lines = read_recorded(&tmp);
    assert_eq!(
        lines.len(),
        3,
        "expected three recorded lines, got {lines:?}"
    );

    let actors: Vec<&Value> = lines.iter().map(|l| &l["actor"]).collect();
    assert_eq!(actors[0], &json!({"kind": "User"}));
    assert_eq!(actors[1], &json!({"kind": "Plugin", "id": "plugin-7"}));
    assert_eq!(actors[2], &json!({"kind": "Kernel"}));

    for l in &lines {
        assert!(
            l["actor"].is_object(),
            "actor must be the typed ActorId JSON object: {l}"
        );
        assert!(l["kind"].is_string(), "kind missing: {l}");
        assert!(!l["payload"].is_null(), "payload missing: {l}");
    }
}

#[tokio::test]
async fn envelope_carries_actor_alongside_event() {
    let (repo, bus, cache, wcc, _tmp) = boot().await;
    let mut sub = bus.subscribe();
    let event_id = create_area_as(
        &*repo,
        &bus,
        &cache,
        &wcc,
        ActorId::Plugin("plugin-1".into()),
        "c",
    )
    .await;
    let env = sub.recv().await.expect("envelope delivered");
    assert_eq!(env.id, event_id);
    assert_eq!(env.actor, ActorId::Plugin("plugin-1".into()));
    match env.event {
        Event::AreaUpdated(_) => {}
        other => panic!("expected AreaUpdated, got {other:?}"),
    }
}
