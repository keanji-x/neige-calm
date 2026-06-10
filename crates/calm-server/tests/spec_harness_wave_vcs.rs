use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::{InputItem, Notification};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, runtime_start_tx};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, Observation, SpecHarness,
    SpecHarnessParams,
};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{Card, CardPatch, CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::routes::theme::RequestTheme;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::WriteContext;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_server::wave_report::WaveReportPayload;
use calm_server::wave_vcs;
use serde_json::{Value, json};

struct Boot {
    repo: Arc<SqlxRepo>,
    harness: SpecHarness,
    daemon: Arc<SharedCodexAppServer>,
    events: EventBus,
    roles: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    write: WriteContext,
    runtime_id: String,
    wave_id: WaveId,
    cove_id: CoveId,
    spec_card_id: CardId,
    thread_id: String,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let roles = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    let write = WriteContext::new(roles.clone(), wave_cove_cache.clone());
    let cove = repo
        .cove_create(NewCove {
            name: "wave-vcs-pr2".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "wave-vcs-pr2".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    wave_cove_cache.insert(wave.id.clone(), cove.id.clone());
    let spec_card = add_card_with_event(
        &repo,
        &events,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "codex",
        CardRole::Spec,
        json!({"schemaVersion": 1, "spec_harness": true}),
    )
    .await;
    let runtime_id = new_id();
    let thread_id = "thread-wave-vcs-pr2".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: runtime_id.clone(),
            card_id: spec_card.id.to_string(),
            kind: RuntimeKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
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

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id: runtime_id.clone(),
        wave_id: wave.id.clone(),
        card_id: spec_card.id.clone(),
        thread_id: Some(thread_id.clone()),
        repo: repo_dyn,
        events: events.clone(),
        card_role_cache: roles.clone(),
        wave_cove_cache: wave_cove_cache.clone(),
        daemon: daemon.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_millis(10),
            debounce_max_wait: Duration::from_millis(200),
            ..HarnessConfig::default()
        },
        snapshot,
    });

    Boot {
        repo,
        harness,
        daemon,
        events,
        roles,
        wave_cove_cache,
        write,
        runtime_id,
        wave_id: wave.id,
        cove_id: cove.id,
        spec_card_id: spec_card.id,
        thread_id,
    }
}

#[allow(clippy::too_many_arguments)]
async fn add_card_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    roles: &CardRoleCache,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
    kind: &str,
    role: CardRole,
    payload: Value,
) -> Card {
    let card_id = CardId::from(new_id());
    let lookup_card_id = card_id.clone();
    let new_card = NewCard {
        wave_id: wave_id.clone(),
        kind: kind.into(),
        sort: None,
        payload,
    };
    let roles = roles.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    repo.write_with_event(
        ActorId::Kernel,
        scope,
        None,
        bus,
        write,
        Box::new(move |tx| {
            let roles = roles.clone();
            let card_id = card_id.clone();
            let new_card = new_card.clone();
            Box::pin(async move {
                let card = card_create_with_id_tx(
                    tx,
                    card_id.to_string(),
                    new_card,
                    role,
                    !matches!(role, CardRole::ReportCard | CardRole::Spec),
                    &roles,
                )
                .await?;
                Ok(Event::CardAdded(card))
            })
        }),
    )
    .await
    .unwrap();
    repo.card_get(lookup_card_id.as_str())
        .await
        .unwrap()
        .unwrap()
}

async fn add_report_card_event(boot: &Boot) -> Card {
    add_card_with_event(
        &boot.repo,
        &boot.events,
        &boot.roles,
        &boot.write,
        &boot.wave_id,
        &boot.cove_id,
        "wave-report",
        CardRole::ReportCard,
        serde_json::to_value(WaveReportPayload::initial()).unwrap(),
    )
    .await
}

async fn add_worker_card_event(boot: &Boot, label: &str) -> Card {
    add_card_with_event(
        &boot.repo,
        &boot.events,
        &boot.roles,
        &boot.write,
        &boot.wave_id,
        &boot.cove_id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "label": label}),
    )
    .await
}

async fn issue_observation(boot: &Boot, obs: Observation, expected_turns: usize) -> String {
    boot.harness.observe(obs).unwrap();
    wait_for_turn_count(&boot.daemon, expected_turns).await;
    turn_text(&boot.daemon, expected_turns - 1)
}

async fn complete_latest_turn(boot: &Boot) {
    let turn_id = boot
        .daemon
        .active_turn_for_test(&boot.thread_id)
        .expect("active turn");
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: boot.thread_id.clone(),
            turn: json!({ "id": turn_id, "status": "completed" }),
        });
    wait_for_state(&boot.harness, |s| {
        matches!(
            s,
            HarnessState::TurnCompleted { last_turn_id } if last_turn_id == &turn_id
        )
    })
    .await;
}

async fn complete_first_turn_and_stamp(boot: &Boot) -> String {
    let text = issue_observation(
        boot,
        Observation::WaveGoal {
            text: "first turn".into(),
        },
        1,
    )
    .await;
    assert!(
        !text.contains("Wave state changes since your last turn"),
        "first turn must not include a diff block: {text}"
    );
    complete_latest_turn(boot).await;
    wait_for_any_last_seen_head(boot).await
}

async fn wait_for_turn_count(daemon: &SharedCodexAppServer, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if daemon.turn_start_count_for_test() as usize >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for turn count {count}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_state(
    harness: &SpecHarness,
    pred: impl Fn(&HarnessState) -> bool,
) -> HarnessState {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = harness.state_for_test().await;
        if pred(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for harness state; last={state:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_any_last_seen_head(boot: &Boot) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(actual) = runtime_snapshot(boot).await.last_seen_head {
            return actual;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for last_seen_head"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_runtime_snapshot(
    boot: &Boot,
    pred: impl Fn(&HarnessSnapshot) -> bool,
) -> HarnessSnapshot {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = runtime_snapshot(boot).await;
        if pred(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for runtime snapshot; last={snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_pending_len(harness: &SpecHarness, len: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let actual = harness.pending_len_for_test().await;
        if actual == len {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for pending len {len}; last={actual}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn set_last_seen_head_raw(boot: &Boot, head: &str) {
    boot.harness
        .set_last_seen_head_for_test(Some(head.to_string()))
        .await;
    let mut snapshot = runtime_snapshot(boot).await;
    snapshot.last_seen_head = Some(head.to_string());
    persist_runtime_snapshot(boot, &snapshot).await;
}

async fn runtime_snapshot(boot: &Boot) -> HarnessSnapshot {
    let state_text: Option<String> = sqlx::query_scalar(
        r#"SELECT handle_state_json
             FROM runtimes
             WHERE card_id = ?1
               AND status IN ('starting', 'running', 'idle', 'turn_pending')
             ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
             LIMIT 1"#,
    )
    .bind(boot.spec_card_id.as_str())
    .fetch_one(boot.repo.pool())
    .await
    .expect("active spec runtime handle state");
    let state_text = state_text.expect("spec runtime has handle state");
    let value = serde_json::from_str(&state_text).expect("handle state json");
    HarnessSnapshot::from_value_strict(value)
}

async fn persist_runtime_snapshot(boot: &Boot, snapshot: &HarnessSnapshot) {
    let state_text = serde_json::to_string(snapshot).expect("snapshot json");
    sqlx::query(
        r#"UPDATE runtimes
              SET handle_state_json = ?1
            WHERE card_id = ?2
              AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(state_text)
    .bind(boot.spec_card_id.as_str())
    .execute(boot.repo.pool())
    .await
    .expect("persist runtime snapshot");
}

async fn update_card_payload_with_event(boot: &Boot, card: &Card, payload: Value) -> Card {
    let card_id = card.id.clone();
    let lookup_card_id = card_id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        wave: card.wave_id.clone(),
        cove: boot.cove_id.clone(),
    };
    boot.repo
        .write_with_event(
            ActorId::Kernel,
            scope,
            None,
            &boot.events,
            &boot.write,
            Box::new(move |tx| {
                let card_id = card_id.clone();
                let payload = payload.clone();
                Box::pin(async move {
                    let card = calm_server::db::sqlite::card_update_tx(
                        tx,
                        card_id.as_str(),
                        CardPatch {
                            kind: None,
                            sort: None,
                            payload: Some(payload),
                            deletable: None,
                        },
                    )
                    .await?;
                    Ok(Event::CardUpdated(card))
                })
            }),
        )
        .await
        .expect("card payload updated event");
    boot.repo
        .card_get(lookup_card_id.as_str())
        .await
        .unwrap()
        .unwrap()
}

async fn spec_card(boot: &Boot) -> Card {
    boot.repo
        .card_get(boot.spec_card_id.as_str())
        .await
        .unwrap()
        .unwrap()
}

fn turn_text(daemon: &SharedCodexAppServer, idx: usize) -> String {
    let turns = daemon.started_turns_for_test();
    let items = &turns[idx].1;
    assert_eq!(items.len(), 1);
    match &items[0] {
        InputItem::Text { text } => text.clone(),
    }
}

fn short(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

#[tokio::test]
async fn first_turn_with_last_seen_head_none_has_no_diff_block() {
    let boot = boot().await;
    let text = issue_observation(
        &boot,
        Observation::WaveGoal {
            text: "read the goal".into(),
        },
        1,
    )
    .await;

    assert_eq!(text, "read the goal");
    assert!(runtime_snapshot(&boot).await.last_seen_head.is_none());
    assert!(
        spec_card(&boot)
            .await
            .payload
            .get("last_seen_head")
            .is_none()
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovered_issued_turn_head_stamps_last_seen_head_on_completion() {
    let boot = boot().await;
    boot.harness.shutdown().await.unwrap();
    let issued_head = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    let turn_id = "turn-issued-before-restart";
    let mut snapshot = runtime_snapshot(&boot).await;
    snapshot.phase = HarnessPhaseTag::IssuingTurn;
    snapshot.last_thread_id = Some(boot.thread_id.clone());
    snapshot.last_turn_id = Some(turn_id.into());
    snapshot.last_seen_head = None;
    snapshot.issued_turn_head = Some(issued_head.clone());
    persist_runtime_snapshot(&boot, &snapshot).await;

    let repo_dyn: Arc<dyn Repo> = boot.repo.clone();
    let recovered = SpecHarness::run(SpecHarnessParams {
        runtime_id: boot.runtime_id.clone(),
        wave_id: boot.wave_id.clone(),
        card_id: boot.spec_card_id.clone(),
        thread_id: Some(boot.thread_id.clone()),
        repo: repo_dyn,
        events: boot.events.clone(),
        card_role_cache: boot.roles.clone(),
        wave_cove_cache: boot.wave_cove_cache.clone(),
        daemon: boot.daemon.clone(),
        config: HarnessConfig::default(),
        snapshot,
    });
    wait_for_state(&recovered, |s| matches!(s, HarnessState::Resumed { .. })).await;

    boot.daemon
        .emit_notification_for_test(Notification::TurnStarted {
            thread_id: boot.thread_id.clone(),
            turn: json!({ "id": turn_id }),
        });
    wait_for_state(
        &recovered,
        |s| matches!(s, HarnessState::TurnRunning { turn_id: active, .. } if active == turn_id),
    )
    .await;
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: boot.thread_id.clone(),
            turn: json!({ "id": turn_id, "status": "completed" }),
        });

    let stored = wait_for_runtime_snapshot(&boot, |snapshot| {
        snapshot.last_seen_head.as_deref() == Some(issued_head.as_str())
            && snapshot.issued_turn_head.is_none()
    })
    .await;
    assert_eq!(stored.last_seen_head.as_deref(), Some(issued_head.as_str()));
    assert!(stored.issued_turn_head.is_none());
    recovered.shutdown().await.unwrap();
}

#[tokio::test]
async fn unchanged_head_after_completed_turn_has_no_diff_block() {
    let boot = boot().await;
    complete_first_turn_and_stamp(&boot).await;
    let current = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    set_last_seen_head_raw(&boot, &current).await;

    let text = issue_observation(
        &boot,
        Observation::TaskFailed {
            idempotency_key: "same-head".into(),
            error: "noop".into(),
        },
        2,
    )
    .await;

    assert!(
        !text.contains("Wave state changes since your last turn"),
        "unchanged head must not include diff block: {text}"
    );
    assert!(text.contains("idempotency_key=same-head"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn diff_failure_from_bogus_stored_head_does_not_wedge_harness() {
    let boot = boot().await;
    let current = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    set_last_seen_head_raw(&boot, "missing-wave-vcs-commit").await;

    let text = issue_observation(
        &boot,
        Observation::TaskFailed {
            idempotency_key: "bogus-head".into(),
            error: "keep going".into(),
        },
        1,
    )
    .await;

    assert!(
        !text.contains("Wave state changes since your last turn"),
        "bad baseline must degrade to no diff block: {text}"
    );
    assert!(text.contains("idempotency_key=bogus-head"));
    complete_latest_turn(&boot).await;
    assert_eq!(wait_for_any_last_seen_head(&boot).await, current);
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn next_turn_prepends_diff_since_completed_turn_head() {
    let boot = boot().await;
    let before = complete_first_turn_and_stamp(&boot).await;
    add_report_card_event(&boot).await;
    let after = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();

    let text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "report-write".into(),
            result: json!({"ok": true}),
        },
        2,
    )
    .await;

    assert!(text.starts_with("## Wave state changes since your last turn"));
    assert!(text.contains(&format!("HEAD {} -> {}", short(&before), short(&after))));
    assert!(text.contains("report.md new"));
    assert!(text.contains("report.md new (unified patch follows)"));
    assert!(text.contains("```diff\n--- a/report.md"));
    assert!(text.contains("+++ b/report.md"));
    assert!(text.contains("@@"));
    assert!(text.contains("\n+# Goal"));
    assert!(text.contains("\n\n---\n\n"));
    assert!(text.contains("idempotency_key=report-write"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn mid_turn_changes_remain_visible_on_next_turn() {
    let boot = boot().await;
    complete_first_turn_and_stamp(&boot).await;
    let before = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    set_last_seen_head_raw(&boot, &before).await;

    let second_text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "turn-two".into(),
            result: json!({"ok": true}),
        },
        2,
    )
    .await;
    assert!(
        !second_text.contains("Wave state changes since your last turn"),
        "unchanged baseline should not produce a diff block: {second_text}"
    );

    add_report_card_event(&boot).await;
    let mid_turn_head = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    complete_latest_turn(&boot).await;
    assert_eq!(wait_for_any_last_seen_head(&boot).await, before);
    let after_completion_head = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(mid_turn_head, before);

    let third_text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "turn-three".into(),
            result: json!({"ok": true}),
        },
        3,
    )
    .await;

    assert!(third_text.starts_with("## Wave state changes since your last turn"));
    assert!(third_text.contains(&format!(
        "HEAD {} -> {}",
        short(&before),
        short(&after_completion_head)
    )));
    assert!(third_text.contains("report.md new"));
    assert!(third_text.contains("idempotency_key=turn-three"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn spec_card_own_payload_path_is_suppressed_from_turn_observation() {
    let boot = boot().await;
    complete_first_turn_and_stamp(&boot).await;
    let before = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    set_last_seen_head_raw(&boot, &before).await;

    let spec = spec_card(&boot).await;
    update_card_payload_with_event(
        &boot,
        &spec,
        json!({"schemaVersion": 1, "spec_harness": true, "private": "noise"}),
    )
    .await;

    let text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "spec-payload-only".into(),
            result: json!({}),
        },
        2,
    )
    .await;

    assert!(text.starts_with("## Wave state changes since your last turn"));
    assert!(
        !text.contains(&format!("cards/{}/payload.json", boot.spec_card_id)),
        "spec card payload path should be internal observation noise: {text}"
    );
    assert!(text.contains(&format!("cards/{}/meta.json edited", boot.spec_card_id)));
    assert!(text.contains("idempotency_key=spec-payload-only"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn batched_observations_get_one_diff_block_covering_all_changes() {
    let boot = boot().await;
    let before = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    let text = issue_observation(
        &boot,
        Observation::WaveGoal {
            text: "first turn".into(),
        },
        1,
    )
    .await;
    assert!(
        !text.contains("Wave state changes since your last turn"),
        "first turn must not include a diff block: {text}"
    );
    let first = add_worker_card_event(&boot, "one").await;
    let second = add_worker_card_event(&boot, "two").await;
    let third = add_worker_card_event(&boot, "three").await;

    boot.harness
        .observe(Observation::TaskCompleted {
            idempotency_key: "one".into(),
            result: json!({}),
        })
        .unwrap();
    boot.harness
        .observe(Observation::TaskCompleted {
            idempotency_key: "two".into(),
            result: json!({}),
        })
        .unwrap();
    boot.harness
        .observe(Observation::TaskCompleted {
            idempotency_key: "three".into(),
            result: json!({}),
        })
        .unwrap();
    wait_for_pending_len(&boot.harness, 3).await;
    let first_turn_id = boot
        .daemon
        .active_turn_for_test(&boot.thread_id)
        .expect("active first turn");
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: boot.thread_id.clone(),
            turn: json!({ "id": first_turn_id, "status": "completed" }),
        });
    wait_for_turn_count(&boot.daemon, 2).await;
    let text = turn_text(&boot.daemon, 1);

    assert_eq!(
        text.matches("Wave state changes since your last turn")
            .count(),
        1,
        "batched turn must have one diff block: {text}"
    );
    assert!(
        text.contains(&format!("HEAD {} ->", short(&before))),
        "batched diff must start from the issued first-turn head: {text}"
    );
    assert!(text.contains(first.id.as_str()));
    assert!(text.contains(&format!("cards/{}/payload.json new", first.id)));
    assert!(text.contains(second.id.as_str()));
    assert!(text.contains(third.id.as_str()));
    assert_eq!(text.matches("A dispatched task completed").count(), 3);
    boot.harness.shutdown().await.unwrap();
}
