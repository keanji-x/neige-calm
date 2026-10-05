//! #2058: a running Track worker canceled through `neige_task_cancel` reads `canceled` in the
//! runs views, both the live `neige_track_cat` projection and the track VCS head, as `neige state`
//! does.

use super::*;

use calm_server::db::sqlite::card_create_with_id_tx;
use calm_server::db::write_with_event_typed;
use calm_server::event::EventScope;
use calm_server::mcp_server::tools::plan::TOOL_TASK_CANCEL;
use calm_server::mcp_server::tools::track_file::TOOL_TRACK_CAT;
use calm_server::track_vcs;

/// A worker card as the codex adapter mints one: its payload names the attempt and a `card.added`
/// announces it, so the track VCS projects it too.
async fn announce_worker_card(boot: &Boot, attempt_id: &str) -> String {
    let card_id = new_id();
    let scope = EventScope::Card {
        card: CardId::from(card_id.clone()),
        track: boot.track_id.clone(),
        area: boot.area_id.clone(),
    };
    let new_card = NewCard {
        track_id: boot.track_id.clone(),
        title: None,
        kind: "codex".into(),
        sort: None,
        payload: json!({ "idempotency_key": attempt_id }),
    };
    let role_cache = boot.card_role_cache.clone();
    let id = card_id.clone();
    write_with_event_typed(
        boot.repo.as_ref(),
        ActorId::KernelDispatcher,
        scope,
        None,
        &boot.events,
        &boot.write,
        move |tx| {
            Box::pin(async move {
                let card =
                    card_create_with_id_tx(tx, id, new_card, CardRole::Worker, true, &role_cache)
                        .await?;
                Ok(((), Event::CardAdded(card)))
            })
        },
    )
    .await
    .expect("announce worker card");
    card_id
}

async fn live_json(boot: &Boot, path: &str) -> Value {
    let out = call_tool(
        boot,
        TOOL_TRACK_CAT,
        planner_identity(boot),
        json!({ "path": path }),
    )
    .await
    .unwrap_or_else(|e| panic!("planner can read {path}: {e:?}"));
    serde_json::from_str(out["content"].as_str().expect("json content")).expect("json body")
}

async fn vcs_head_json(boot: &Boot, path: &str) -> Value {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let head = track_vcs::head(&pool, &boot.track_id)
        .await
        .expect("read vcs head")
        .expect("track has a vcs head");
    let blob = track_vcs::cat_at(&pool, &head, path)
        .await
        .unwrap_or_else(|e| panic!("vcs head has {path}: {e:?}"));
    serde_json::from_str(&blob.content).expect("json body")
}

fn index_entry(index: &Value, attempt_id: &str) -> Value {
    index
        .as_array()
        .expect("runs index is an array")
        .iter()
        .find(|run| run["attempt_id"] == json!(attempt_id))
        .cloned()
        .unwrap_or_else(|| panic!("runs index lists {attempt_id}: {index}"))
}

/// Every runs view of `attempt_id`: the live index entry and detail, then the VCS head's.
async fn runs_views(boot: &Boot, attempt_id: &str) -> [(&'static str, Value); 4] {
    let detail = format!("runs/{attempt_id}.json");
    [
        (
            "live runs/index.json",
            index_entry(&live_json(boot, "runs/index.json").await, attempt_id),
        ),
        ("live runs/<attempt>.json", live_json(boot, &detail).await),
        (
            "vcs runs/index.json",
            index_entry(&vcs_head_json(boot, "runs/index.json").await, attempt_id),
        ),
        (
            "vcs runs/<attempt>.json",
            vcs_head_json(boot, &detail).await,
        ),
    ]
}

#[tokio::test]
async fn canceled_running_task_reads_canceled_in_every_runs_view() {
    let boot = boot().await;
    let task = plan_task(&boot.track_id, "runs-cancel", TaskKind::Codex, &[]);
    let attempt_id = task.id.clone();
    let worker_card_id = announce_worker_card(&boot, &attempt_id).await;
    seed_projected_task(&boot, task).await;
    let (_runtime, scheduler) = build_scheduler(
        &boot,
        vec![Arc::new(CardSpawnAdapter {
            kind: "codex-worker",
            card_id: worker_card_id,
        })],
    );
    scheduler.schedule_track(boot.track_id.clone()).await;
    assert_eq!(
        task_row(&boot, "runs-cancel").await.status,
        TaskStatus::Running
    );
    for (view, run) in runs_views(&boot, &attempt_id).await {
        assert_eq!(
            run["status"],
            json!("running"),
            "{view} before cancel: {run}"
        );
    }

    call_tool(
        &boot,
        TOOL_TASK_CANCEL,
        planner_identity(&boot),
        json!({ "key": "runs-cancel", "message": "wrong direction" }),
    )
    .await
    .expect("running codex task is cancelable");

    let row = task_row(&boot, "runs-cancel").await;
    assert_eq!(row.status, TaskStatus::Canceled);
    let finished_at = row.finished_at_ms.expect("cancel stamps finished_at_ms");
    for (view, run) in runs_views(&boot, &attempt_id).await {
        assert_eq!(
            run["status"],
            json!("canceled"),
            "{view} after cancel: {run}"
        );
        assert_eq!(run["finished_at"], json!(finished_at), "{view}: {run}");
    }
    // The cancel is not a run failure: the Planner is not woken by a `task.failed`.
    assert!(event_rows(&boot, "task.failed").await.is_empty());
}
