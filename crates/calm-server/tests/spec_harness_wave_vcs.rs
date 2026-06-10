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
    write: WriteContext,
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
        runtime_id,
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
        write,
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
    let deadline = Instant::now() + Duration::from_secs(2);
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
    let deadline = Instant::now() + Duration::from_secs(2);
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
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let card = spec_card(boot).await;
        if let Some(actual) = last_seen_head(&card) {
            return actual.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for last_seen_head"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn set_last_seen_head_raw(boot: &Boot, head: &str) {
    let mut card = spec_card(boot).await;
    card.payload["last_seen_head"] = Value::String(head.to_string());
    boot.repo
        .card_update(
            boot.spec_card_id.as_str(),
            CardPatch {
                kind: None,
                sort: None,
                payload: Some(card.payload),
                deletable: None,
            },
        )
        .await
        .unwrap();
}

async fn spec_card(boot: &Boot) -> Card {
    boot.repo
        .card_get(boot.spec_card_id.as_str())
        .await
        .unwrap()
        .unwrap()
}

fn last_seen_head(card: &Card) -> Option<&str> {
    card.payload.get("last_seen_head").and_then(Value::as_str)
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
    assert!(
        boot.repo
            .card_get(boot.spec_card_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .payload
            .get("last_seen_head")
            .is_none()
    );
    boot.harness.shutdown().await.unwrap();
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
    assert!(text.contains("\n\n---\n\n"));
    assert!(text.contains("idempotency_key=report-write"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn batched_observations_get_one_diff_block_covering_all_changes() {
    let boot = boot().await;
    let before = complete_first_turn_and_stamp(&boot).await;
    let first = add_worker_card_event(&boot, "one").await;
    let second = add_worker_card_event(&boot, "two").await;
    let third = add_worker_card_event(&boot, "three").await;
    let after = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();

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
    wait_for_turn_count(&boot.daemon, 2).await;
    let text = turn_text(&boot.daemon, 1);

    assert_eq!(
        text.matches("Wave state changes since your last turn")
            .count(),
        1,
        "batched turn must have one diff block: {text}"
    );
    assert!(text.contains(&format!("HEAD {} -> {}", short(&before), short(&after))));
    assert!(text.contains(first.id.as_str()));
    assert!(text.contains(second.id.as_str()));
    assert!(text.contains(third.id.as_str()));
    assert_eq!(text.matches("A dispatched task completed").count(), 3);
    boot.harness.shutdown().await.unwrap();
}
