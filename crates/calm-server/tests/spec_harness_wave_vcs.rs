use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::{InputItem, Notification};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, session_set_status_tx, session_start_runtime_tx,
};
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
use calm_server::wave_fs_view::WaveFsView;
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
    session_start_runtime_tx(
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
            spawn_op_id: None,
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

async fn log_worker_codex_hook(boot: &Boot, card: &Card, key: &str, prompt: &str) {
    boot.repo
        .log_pure_event(
            ActorId::Kernel,
            EventScope::Card {
                card: card.id.clone(),
                wave: boot.wave_id.clone(),
                cove: boot.cove_id.clone(),
            },
            None,
            &boot.events,
            &boot.roles,
            &boot.wave_cove_cache,
            Event::CodexHook {
                card_id: card.id.clone(),
                kind: "hook.codex.user_prompt_submit".into(),
                hook_idempotency_key: key.into(),
                payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt}),
            },
        )
        .await
        .expect("worker codex hook event");
}

async fn refresh_transcripts(boot: &Boot) -> wave_vcs::CommitHash {
    let mut tx = boot
        .repo
        .pool()
        .begin()
        .await
        .expect("begin transcript refresh");
    let commit = wave_vcs::snapshot_transcripts_for_cards_in_wave(
        &mut tx,
        &boot.wave_id,
        None,
        wave_vcs::MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("snapshot transcripts");
    tx.commit().await.expect("commit transcript refresh");
    commit
}

async fn start_worker_runtime_with_event(boot: &Boot, card: &Card, status: RunStatus) -> String {
    let runtime_id = new_id();
    let returned_runtime_id = runtime_id.clone();
    let card_id = card.id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        wave: boot.wave_id.clone(),
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
                let runtime_id = runtime_id.clone();
                let card_id = card_id.clone();
                let status = status.clone();
                Box::pin(async move {
                    let runtime = session_start_runtime_tx(
                        tx,
                        RuntimeInit {
                            id: runtime_id,
                            card_id: card_id.to_string(),
                            kind: RuntimeKind::CodexCard,
                            agent_provider: Some(AgentProvider::Codex),
                            status,
                            terminal_run_id: None,
                            thread_id: Some("worker-thread-current-schema".into()),
                            session_id: None,
                            active_turn_id: None,
                            handle_state_json: None,
                            lease_owner: None,
                            lease_until_ms: None,
                            spawn_op_id: None,
                            now_ms: now_ms(),
                        },
                    )
                    .await?;
                    Ok(Event::RuntimeStarted {
                        runtime_id: runtime.id,
                        card_id: runtime.card_id,
                        kind: runtime.kind,
                        agent_provider: runtime.agent_provider,
                        status: runtime.status,
                    })
                })
            }),
        )
        .await
        .expect("worker runtime started event");
    returned_runtime_id
}

async fn set_worker_runtime_status_with_event(
    boot: &Boot,
    card: &Card,
    runtime_id: &str,
    old_status: RunStatus,
    new_status: RunStatus,
) {
    let runtime_id = runtime_id.to_string();
    let card_id = card.id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        wave: boot.wave_id.clone(),
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
                let runtime_id = runtime_id.clone();
                let card_id = card_id.clone();
                let old_status = old_status.clone();
                let new_status = new_status.clone();
                Box::pin(async move {
                    session_set_status_tx(tx, &runtime_id, new_status.clone()).await?;
                    Ok(Event::RuntimeStatusChanged {
                        runtime_id,
                        card_id: card_id.to_string(),
                        old_status,
                        new_status,
                    })
                })
            }),
        )
        .await
        .expect("worker runtime status changed event");
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
    wait_for_in_mem_last_seen_head(boot).await
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

// Waits on the harness's IN-MEMORY `last_seen_head` (not the persisted snapshot).
// The turn-completion path persists the snapshot one await-point BEFORE it stamps
// the in-memory value (run_loop.rs ~1534 then ~1536), so polling the snapshot can
// return before the in-memory stamp lands — and a later in-memory stamp would then
// clobber a test's `set_last_seen_head_raw` override. Waiting on the in-memory
// value guarantees that stamp has landed, so the override is authoritative for the
// next turn's diff baseline. See issue #687.
async fn wait_for_in_mem_last_seen_head(boot: &Boot) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(actual) = boot.harness.last_seen_head_for_test().await {
            return actual;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for in-memory last_seen_head"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_last_seen_head_eq(boot: &Boot, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let actual = boot.harness.last_seen_head_for_test().await;
        if actual.as_deref() == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for in-memory last_seen_head == {expected}; last={actual:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_head_path(boot: &Boot, path: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_head = "none".to_string();
    loop {
        if let Some(head) = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
            .await
            .expect("head query")
        {
            match wave_vcs::tree_at(boot.repo.pool(), &head)
                .await
                .expect("tree query")
            {
                Some(tree) if tree.entries.contains_key(path) => return head,
                Some(tree) => {
                    last_head = format!("{head} ({} paths)", tree.entries.len());
                }
                None => {
                    last_head = format!("{head} (tree missing)");
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for wave-vcs head containing path {path}; last_head={last_head}"
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

async fn corrupt_card_payload(boot: &Boot, card_id: &CardId) {
    let res = sqlx::query("UPDATE cards SET payload = ?1 WHERE id = ?2")
        .bind("{not-json")
        .bind(card_id.as_str())
        .execute(boot.repo.pool())
        .await
        .expect("corrupt card payload");
    assert_eq!(res.rows_affected(), 1, "card payload should be corrupted");
}

async fn runtime_snapshot(boot: &Boot) -> HarnessSnapshot {
    let state_text: Option<String> = sqlx::query_scalar(
        r#"SELECT handle_state_json
             FROM worker_sessions
             WHERE card_id = ?1
               AND state IN ('starting', 'running', 'idle', 'turn_pending')
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
        r#"UPDATE worker_sessions
              SET handle_state_json = ?1
            WHERE card_id = ?2
              AND state IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(&state_text)
    .bind(boot.spec_card_id.as_str())
    .execute(boot.repo.pool())
    .await
    .expect("persist worker session snapshot");
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
    let issued_head = wait_for_runtime_snapshot(&boot, |s| s.issued_turn_head.is_some())
        .await
        .issued_turn_head
        .expect("issued turn head");
    complete_latest_turn(&boot).await;
    wait_for_last_seen_head_eq(&boot, &issued_head).await;
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn transcript_refresh_failure_from_corrupt_card_payload_does_not_wedge_harness() {
    let boot = boot().await;
    let before = complete_first_turn_and_stamp(&boot).await;
    let worker = add_worker_card_event(&boot, "corrupt-refresh-payload").await;
    let current = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(current, before);
    set_last_seen_head_raw(&boot, &current).await;
    corrupt_card_payload(&boot, &worker.id).await;

    let text = issue_observation(
        &boot,
        Observation::TaskFailed {
            idempotency_key: "corrupt-refresh-payload".into(),
            error: "keep issuing".into(),
        },
        2,
    )
    .await;

    assert!(
        !text.contains("Wave state changes since your last turn"),
        "corrupt refresh source must degrade to no diff block: {text}"
    );
    assert!(text.contains("idempotency_key=corrupt-refresh-payload"));
    let issued_head = wait_for_runtime_snapshot(&boot, |s| s.issued_turn_head.is_some())
        .await
        .issued_turn_head
        .expect("issued turn head");
    assert_eq!(
        issued_head, current,
        "refresh failure must preserve live-HEAD fallback behavior"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn next_turn_prepends_diff_since_completed_turn_head() {
    let boot = boot().await;
    let before = complete_first_turn_and_stamp(&boot).await;
    add_report_card_event(&boot).await;

    let text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "report-write".into(),
            result: json!({"ok": true}),
        },
        2,
    )
    .await;
    let issued_head = wait_for_runtime_snapshot(&boot, |s| s.issued_turn_head.is_some())
        .await
        .issued_turn_head
        .expect("issued turn head");

    assert!(text.starts_with("## Wave state changes since your last turn"));
    assert!(text.contains(&format!(
        "HEAD {} -> {}",
        short(&before),
        short(&issued_head)
    )));
    assert!(text.contains("report.md new (by kernel)"));
    assert!(text.contains("report.md new (by kernel) (unified patch follows)"));
    assert!(text.contains("```diff\n--- a/report.md"));
    assert!(text.contains("+++ b/report.md"));
    assert!(text.contains("@@"));
    assert!(text.contains("\n+# 概要"));
    assert!(text.contains("\n\n---\n\n"));
    assert!(text.contains("idempotency_key=report-write"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn turn_issuance_refreshes_hook_transcripts_before_diff() {
    let boot = boot().await;
    let worker = add_worker_card_event(&boot, "hook-transcript-refresh").await;
    let before = complete_first_turn_and_stamp(&boot).await;
    set_last_seen_head_raw(&boot, &before).await;

    for seq in 0..3 {
        log_worker_codex_hook(
            &boot,
            &worker,
            &format!("issuance-refresh-hook-{seq}"),
            &format!("issuance progress {seq}"),
        )
        .await;
    }

    let text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "hook-transcript-refresh".into(),
            result: json!({"ok": true}),
        },
        2,
    )
    .await;
    let issued_head = wait_for_runtime_snapshot(&boot, |s| s.issued_turn_head.is_some())
        .await
        .issued_turn_head
        .expect("issued turn head");
    let events_path = format!("cards/{}/events.json", worker.id.as_str());
    let conversation_path = format!("cards/{}/conversation.md", worker.id.as_str());

    assert!(text.starts_with("## Wave state changes since your last turn"));
    assert!(text.contains(&format!(
        "HEAD {} -> {}",
        short(&before),
        short(&issued_head)
    )));
    assert_eq!(text.matches(&format!("{events_path} edited")).count(), 1);
    assert_eq!(
        text.matches(&format!("{conversation_path} edited")).count(),
        1
    );
    assert!(text.contains("idempotency_key=hook-transcript-refresh"));

    let issued_record = wave_vcs::commit_record(boot.repo.pool(), &issued_head)
        .await
        .expect("issued head commit record")
        .expect("issued head commit");
    assert_eq!(
        issued_record.message.as_deref(),
        Some("transcript refresh"),
        "successful issuance should stamp the pre-diff refresh commit"
    );

    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .expect("wave");
    let view = WaveFsView::new(boot.repo.as_ref(), &boot.write);
    let vcs_conversation = wave_vcs::cat_at(boot.repo.pool(), &issued_head, &conversation_path)
        .await
        .expect("issued head conversation")
        .content;
    let live_conversation = view
        .cat(&wave, &conversation_path)
        .await
        .expect("live conversation")
        .content;
    assert_eq!(vcs_conversation, live_conversation);
    assert!(
        vcs_conversation.contains("issuance progress 2"),
        "conversation.md = {vcs_conversation}"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn since_last_turn_override_fences_post_refresh_hook_commit() {
    let boot = boot().await;
    let worker = add_worker_card_event(&boot, "post-refresh-fence").await;
    let before = complete_first_turn_and_stamp(&boot).await;
    let events_path = format!("cards/{}/events.json", worker.id.as_str());
    let conversation_path = format!("cards/{}/conversation.md", worker.id.as_str());

    log_worker_codex_hook(&boot, &worker, "pre-refresh-hook", "pre refresh prompt").await;
    let refresh_head = refresh_transcripts(&boot).await;
    let refresh_conversation =
        wave_vcs::cat_at(boot.repo.pool(), &refresh_head, &conversation_path)
            .await
            .expect("refresh conversation")
            .content;
    assert!(refresh_conversation.contains("pre refresh prompt"));

    log_worker_codex_hook(&boot, &worker, "post-refresh-hook", "post refresh prompt").await;
    let hook_head = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .expect("head query")
        .expect("post-refresh hook head");
    assert_ne!(
        hook_head, refresh_head,
        "post-refresh hook should advance live HEAD"
    );
    let hook_head_conversation = wave_vcs::cat_at(boot.repo.pool(), &hook_head, &conversation_path)
        .await
        .expect("hook head conversation")
        .content;
    assert!(
        !hook_head_conversation.contains("post refresh prompt"),
        "hook-only commit should not contain its transcript until the next refresh"
    );

    let fenced = wave_vcs::since_last_turn_block(
        boot.repo.pool(),
        &boot.wave_id,
        Some(&before),
        Some(&refresh_head),
        Some(&boot.spec_card_id),
    )
    .await
    .expect("fenced diff");
    assert_eq!(fenced.current_head.as_deref(), Some(refresh_head.as_str()));
    let fenced_block = fenced.block.expect("fenced diff block");
    assert_eq!(
        fenced_block
            .matches(&format!("- {events_path} edited"))
            .count(),
        1
    );
    assert_eq!(
        fenced_block
            .matches(&format!("- {conversation_path} edited"))
            .count(),
        1
    );

    let unfenced = wave_vcs::since_last_turn_block(
        boot.repo.pool(),
        &boot.wave_id,
        Some(&before),
        None,
        Some(&boot.spec_card_id),
    )
    .await
    .expect("unfenced diff");
    assert_eq!(unfenced.current_head.as_deref(), Some(hook_head.as_str()));

    let next_refresh_head = refresh_transcripts(&boot).await;
    let next_conversation =
        wave_vcs::cat_at(boot.repo.pool(), &next_refresh_head, &conversation_path)
            .await
            .expect("next refresh conversation")
            .content;
    assert!(next_conversation.contains("post refresh prompt"));

    let next = wave_vcs::since_last_turn_block(
        boot.repo.pool(),
        &boot.wave_id,
        Some(&refresh_head),
        Some(&next_refresh_head),
        Some(&boot.spec_card_id),
    )
    .await
    .expect("next fenced diff");
    assert_eq!(
        next.current_head.as_deref(),
        Some(next_refresh_head.as_str())
    );
    let next_block = next.block.expect("next diff block");
    assert_eq!(
        next_block
            .matches(&format!("- {events_path} edited"))
            .count(),
        1
    );
    assert_eq!(
        next_block
            .matches(&format!("- {conversation_path} edited"))
            .count(),
        1
    );

    let after_next = wave_vcs::since_last_turn_block(
        boot.repo.pool(),
        &boot.wave_id,
        Some(&next_refresh_head),
        None,
        Some(&boot.spec_card_id),
    )
    .await
    .expect("after next refresh diff");
    assert!(
        after_next.block.is_none(),
        "post-refresh transcript paths should appear in one next-turn diff only"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn ai_actor_attribution_flows_into_diff_block() {
    let boot = boot().await;
    complete_first_turn_and_stamp(&boot).await;
    let worker = add_card_with_event(
        &boot.repo,
        &boot.events,
        &boot.roles,
        &boot.write,
        &boot.wave_id,
        &boot.cove_id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "ai-attribution"}),
    )
    .await;
    let before = wait_for_head_path(&boot, "runs/ai-attribution.json").await;
    set_last_seen_head_raw(&boot, &before).await;

    let worker_id = worker.id.clone();
    boot.repo
        .write_with_event(
            ActorId::AiCodex(worker_id.clone()),
            EventScope::Card {
                card: worker_id.clone(),
                wave: boot.wave_id.clone(),
                cove: boot.cove_id.clone(),
            },
            None,
            &boot.events,
            &boot.write,
            Box::new(move |_tx| {
                Box::pin(async move {
                    Ok(Event::TaskCompleted {
                        idempotency_key: "ai-attribution".into(),
                        result: json!({"ok": true}),
                        artifacts: vec![],
                        agent_message: None,
                    })
                })
            }),
        )
        .await
        .expect("ai task completed event");

    let text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "ai-attribution-observed".into(),
            result: json!({"ok": true}),
        },
        2,
    )
    .await;
    let label = format!(
        "runs/ai-attribution.json edited (by ai:codex:{})",
        worker.id
    );
    assert!(
        text.contains(&label),
        "AI actor label should flow into diff attribution: {text}"
    );
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
    let turn_two_issued_head = wait_for_runtime_snapshot(&boot, |s| s.issued_turn_head.is_some())
        .await
        .issued_turn_head
        .expect("turn two issued head");
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
    wait_for_runtime_snapshot(&boot, |s| {
        s.last_seen_head.as_deref() == Some(turn_two_issued_head.as_str())
            && s.issued_turn_head.is_none()
    })
    .await;
    assert_ne!(mid_turn_head, turn_two_issued_head);

    let third_text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "turn-three".into(),
            result: json!({"ok": true}),
        },
        3,
    )
    .await;
    let turn_three_issued_head = wait_for_runtime_snapshot(&boot, |s| s.issued_turn_head.is_some())
        .await
        .issued_turn_head
        .expect("turn three issued head");

    assert!(third_text.starts_with("## Wave state changes since your last turn"));
    assert!(third_text.contains(&format!(
        "HEAD {} -> {}",
        short(&turn_two_issued_head),
        short(&turn_three_issued_head)
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
        !text.contains(&format!("cards/{}/.payload.json", boot.spec_card_id)),
        "spec card payload path should be internal observation noise: {text}"
    );
    assert!(text.contains(&format!("cards/{}/.meta.json edited", boot.spec_card_id)));
    assert!(text.contains("idempotency_key=spec-payload-only"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_status_flip_current_schema_has_no_payload_diff_entry() {
    let boot = boot().await;
    complete_first_turn_and_stamp(&boot).await;
    let worker = add_worker_card_event(&boot, "runtime-status-current-schema").await;
    let runtime_id = start_worker_runtime_with_event(&boot, &worker, RunStatus::Starting).await;
    let before = wave_vcs::head(boot.repo.pool(), &boot.wave_id)
        .await
        .unwrap()
        .unwrap();
    set_last_seen_head_raw(&boot, &before).await;

    add_report_card_event(&boot).await;
    set_worker_runtime_status_with_event(
        &boot,
        &worker,
        &runtime_id,
        RunStatus::Starting,
        RunStatus::Running,
    )
    .await;

    let text = issue_observation(
        &boot,
        Observation::TaskCompleted {
            idempotency_key: "runtime-status-current-schema".into(),
            result: json!({"ok": true}),
        },
        2,
    )
    .await;

    assert!(text.starts_with("## Wave state changes since your last turn"));
    assert!(text.contains("report.md new"));
    assert!(
        !text.contains(&format!("cards/{}/.payload.json", worker.id.as_str())),
        "current-schema runtime re-render must not create payload diff noise: {text}"
    );
    assert!(text.contains("idempotency_key=runtime-status-current-schema"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn batched_observations_get_one_diff_block_covering_all_changes() {
    let boot = boot().await;
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
    let first_issued_head = wait_for_runtime_snapshot(&boot, |s| s.issued_turn_head.is_some())
        .await
        .issued_turn_head
        .expect("first issued turn head");
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
        text.contains(&format!("HEAD {} ->", short(&first_issued_head))),
        "batched diff must start from the issued first-turn head: {text}"
    );
    assert!(text.contains(first.id.as_str()));
    assert!(text.contains(&format!("cards/{}/.payload.json new", first.id)));
    assert!(text.contains(second.id.as_str()));
    assert!(text.contains(third.id.as_str()));
    assert_eq!(text.matches("A dispatched task completed").count(), 3);
    boot.harness.shutdown().await.unwrap();
}
