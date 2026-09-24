//! The per-edit-session debounce. The run loop reads `std::time::Instant`, which `tokio::time::pause` cannot move,
//! so the clock is driven with `PlannerHarness::rewind_debounce_for_test`; nothing here sleeps for the debounce itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{EditAuthor, EventBus};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams,
};
use calm_server::ids::{CardId, TrackId};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::{
    SharedCodexAppServer, SharedThreadStartParams, ThreadConfig,
};
use serde_json::json;

/// Long enough for several 50 ms ticks to run; every "did NOT issue" assertion waits this long.
const TICKS: Duration = Duration::from_millis(400);

async fn idle_harness(
    config: HarnessConfig,
) -> (PlannerHarness, Arc<SharedCodexAppServer>, TrackId) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let area = repo
        .area_create(NewArea {
            name: "debounce".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "goal".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let thread_id = daemon
        .thread_start_for_card(
            card.id.as_str(),
            CardRole::Planner,
            Some(card.track_id.as_str()),
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    let worker_session_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: worker_session_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id,
        track_id: track.id.clone(),
        card_id: card.id,
        thread_id: Some(thread_id),
        repo,
        events: EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
        backend: daemon.clone().into(),
        config,
        snapshot,
    });
    (harness, daemon, track.id)
}

fn report_edit(track_id: &TrackId, version: u32) -> Observation {
    Observation::ReportEdited {
        track_id: track_id.clone(),
        body_sha256: format!("sha-{version}"),
        body: format!("# R\n\nv{version}\n"),
        author: Some(EditAuthor::User),
        body_before: Some(format!("# R\n\nv{}\n", version - 1)),
        doc_rev_after: None,
        blocks_after: None,
    }
}

async fn wait_for_turn_start(daemon: &SharedCodexAppServer, why: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while daemon.turn_start_count_for_test() == 0 {
        assert!(Instant::now() < deadline, "no turn issued: {why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until the queue holds exactly `entries` entries and the debounce window is armed, so a rewind moves timestamps every observation has already stamped.
async fn wait_queued(harness: &PlannerHarness, entries: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let armed = harness.debounce_timestamps_set_for_test().await == (true, true);
        let queued = harness.snapshot().await.pending_len();
        if armed && queued == entries {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "queue never reached {entries} armed entries (armed={armed}, queued={queued})"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn report_edits_alone_wait_for_the_edit_session_to_go_quiet() {
    // The production pairs, on purpose: the test drives the clock, not the thresholds.
    let (harness, daemon, track_id) = idle_harness(HarnessConfig::default()).await;
    harness.observe(report_edit(&track_id, 1)).unwrap();
    harness.observe(report_edit(&track_id, 2)).unwrap();
    // Two saves, one folded entry.
    wait_queued(&harness, 1).await;

    harness
        .rewind_debounce_for_test(Duration::from_secs(1))
        .await;
    tokio::time::sleep(TICKS).await;
    assert_eq!(
        daemon.turn_start_count_for_test(),
        0,
        "1 s of idle is past the ordinary 250 ms pair but not the 20 s report-edit pair"
    );

    harness
        .rewind_debounce_for_test(Duration::from_secs(20))
        .await;
    wait_for_turn_start(&daemon, "21 s of idle is past report_edit_min_idle").await;
    assert_eq!(daemon.turn_start_count_for_test(), 1);
    harness.shutdown().await.unwrap();
}

/// Wait until the newest observation has stamped the debounce window: after a rewind `last_pending_at` reads as seconds old, and the next `observe` resets it to now.
async fn wait_last_pending_refreshed(harness: &PlannerHarness) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while harness.debounce_last_pending_elapsed_ms_for_test().await >= 5_000 {
        assert!(
            Instant::now() < deadline,
            "the observation never refreshed last_pending_at"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A fold refreshes `last_pending_at` but never moves `first_pending_at`, so the idle rule is never met and `report_edit_max_wait` decides.
#[tokio::test]
async fn an_edit_session_that_never_goes_quiet_issues_at_max_wait() {
    let (harness, daemon, track_id) = idle_harness(HarnessConfig::default()).await;
    harness.observe(report_edit(&track_id, 1)).unwrap();
    wait_queued(&harness, 1).await;
    // Eleven more saves, 10 s apart: 110 s since the first, never 20 s idle.
    for version in 2..=12 {
        harness
            .rewind_debounce_for_test(Duration::from_secs(10))
            .await;
        harness.observe(report_edit(&track_id, version)).unwrap();
        wait_last_pending_refreshed(&harness).await;
        assert_eq!(
            harness.snapshot().await.pending_len(),
            1,
            "every contiguous save folds into the held entry"
        );
    }
    tokio::time::sleep(TICKS).await;
    assert_eq!(
        daemon.turn_start_count_for_test(),
        0,
        "110 s since the first save and freshly saved: neither rule is met"
    );
    assert!(harness.debounce_first_pending_elapsed_ms_for_test().await >= 110_000);

    // 11 s more: 121 s since the first save, 11 s idle (still under 20 s).
    harness
        .rewind_debounce_for_test(Duration::from_secs(11))
        .await;
    wait_for_turn_start(
        &daemon,
        "121 s since the first save is past report_edit_max_wait",
    )
    .await;
    assert_eq!(daemon.turn_start_count_for_test(), 1);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_soft_non_report_entry_restores_the_ordinary_pair() {
    let (harness, daemon, track_id) = idle_harness(HarnessConfig::default()).await;
    harness.observe(report_edit(&track_id, 1)).unwrap();
    harness
        .observe(Observation::WorkspaceLeased {
            track_id: track_id.clone(),
            card_id: CardId::from("worker-card"),
            lease_id: "lease-1".into(),
            path: "/tmp/ws".into(),
        })
        .unwrap();
    wait_queued(&harness, 2).await;
    assert!(
        !harness.debounce_hard_fire_for_test().await,
        "a workspace lease is soft; the test must not be passing on hard-fire"
    );

    harness
        .rewind_debounce_for_test(Duration::from_secs(1))
        .await;
    wait_for_turn_start(&daemon, "a mixed soft queue uses the 250 ms pair").await;
    harness.shutdown().await.unwrap();
}

/// "At once" is stated by configuration: BOTH soft pairs are set to a minute, so the only way a turn can issue inside this test's budget is the hard-fire short-circuit.
#[tokio::test]
async fn a_user_message_during_the_wait_issues_at_once() {
    let (harness, daemon, track_id) = idle_harness(HarnessConfig {
        debounce_min_idle: Duration::from_secs(60),
        debounce_max_wait: Duration::from_secs(60),
        report_edit_min_idle: Duration::from_secs(60),
        report_edit_max_wait: Duration::from_secs(60),
        ..HarnessConfig::default()
    })
    .await;
    harness.observe(report_edit(&track_id, 1)).unwrap();
    wait_queued(&harness, 1).await;
    harness
        .rewind_debounce_for_test(Duration::from_secs(1))
        .await;
    tokio::time::sleep(TICKS).await;
    assert_eq!(
        daemon.turn_start_count_for_test(),
        0,
        "still inside the edit window"
    );

    harness
        .observe_user_message_durable("are you there?".into(), Vec::new())
        .await
        .unwrap();
    wait_for_turn_start(&daemon, "a user message is hard-fire and must not wait").await;
    assert_eq!(daemon.turn_start_count_for_test(), 1);
    assert_eq!(
        harness.snapshot().await.pending_len(),
        0,
        "the queued report edit rode along with the user message"
    );
    harness.shutdown().await.unwrap();
}
