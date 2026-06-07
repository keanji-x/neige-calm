use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::harness::{
    HarnessPhaseTag, HarnessSnapshot, Observation, rehydrate_spec_push_queue,
};
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use serde_json::json;

#[tokio::test]
async fn queued_rows_move_into_handle_state_and_are_deleted() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "queue".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "queue".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let stale_obs = Observation::TaskCompleted {
        idempotency_key: "task-stale".into(),
        result: json!({"stale": true}),
    };
    let mut snapshot = HarnessSnapshot::initial(7, vec![stale_obs.clone()]);
    snapshot.pending_envelope_ids = vec![Some(7)];
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("thread-q".into());
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-q".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    repo.spec_card_enqueue_observation(
        card.id.as_str(),
        7,
        &serde_json::to_string(&stale_obs).unwrap(),
    )
    .await
    .unwrap();
    repo.spec_card_enqueue_observation(
        card.id.as_str(),
        8,
        &serde_json::to_string(&Observation::TaskCompleted {
            idempotency_key: "task-8".into(),
            result: json!({"ok": true}),
        })
        .unwrap(),
    )
    .await
    .unwrap();
    repo.spec_card_enqueue_observation(card.id.as_str(), 9, "legacy rendered text")
        .await
        .unwrap();

    rehydrate_spec_push_queue(repo.clone(), card.id.as_str(), &mut snapshot)
        .await
        .unwrap();
    assert_eq!(
        repo.spec_card_queued_observations(card.id.as_str())
            .await
            .unwrap(),
        Vec::<(i64, i64, String)>::new()
    );
    let runtime = repo.runtime_get_by_id(&runtime_id).await.unwrap().unwrap();
    let stored: HarnessSnapshot =
        serde_json::from_value(runtime.handle_state_json.unwrap()).unwrap();
    assert_eq!(stored.push_watermark, 9);
    assert_eq!(stored.pending_queue.len(), 3);
    assert_eq!(stored.pending_envelope_ids, vec![Some(7), Some(8), Some(9)]);
    assert!(matches!(
        &stored.pending_queue[0],
        Observation::TaskCompleted {
            idempotency_key, ..
        } if idempotency_key == "task-stale"
    ));
    assert!(matches!(
        &stored.pending_queue[1],
        Observation::TaskCompleted {
            idempotency_key, ..
        } if idempotency_key == "task-8"
    ));
    assert!(matches!(
        &stored.pending_queue[2],
        Observation::WaveGoal { text } if text == "legacy rendered text"
    ));
}
