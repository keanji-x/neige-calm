//! Runtime layer-B recovery for a fully wedged spec app-server.
//!
//! These are process-level tests against the deterministic fake codex
//! fixture, so they run in normal CI. Real-codex-binary tests remain gated
//! behind `codex-e2e`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_update_tx, card_with_codex_create_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{CardPatch, CardRole, NewCove, NewWave, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::settings::Settings;
use calm_server::spec_appserver::{
    TurnWatchdogConfig, spawn_spec_appserver_with_watchdog_config_and_recovery,
};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::{Value, json};
use tempfile::TempDir;

const WATCHDOG: TurnWatchdogConfig = TurnWatchdogConfig {
    max_turn_duration: Duration::from_millis(50),
    interrupt_completion_budget: Duration::from_millis(150),
};
const RECOVERY_DEADLINE: Duration = Duration::from_secs(20);

fn fake_codex_bin() -> String {
    env!("CARGO_BIN_EXE_osc-probe-child").to_string()
}
struct RuntimeHarness {
    _tmp: TempDir,
    repo: Arc<dyn Repo>,
    state: AppState,
    wave_id: WaveId,
    cove_id: CoveId,
    spec_card_id: String,
    interrupt_marker: PathBuf,
    initial_pgid: i32,
}

impl RuntimeHarness {
    async fn new(wedge_process_count: u32) -> Self {
        let tmp_root = PathBuf::from("/tmp/csr");
        std::fs::create_dir_all(&tmp_root).expect("mkdir test temp root");
        let tmp = tempfile::Builder::new()
            .prefix("case-")
            .tempdir_in(tmp_root)
            .expect("tempdir");
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let events = EventBus::new();
        let role_cache = CardRoleCache::new();
        let cove_cache = WaveCoveCache::new();
        let mut codex = CodexClient::new_stub();
        codex.codex_bin = fake_codex_bin();
        let state = AppState::from_parts(
            repo.clone(),
            events.clone(),
            Arc::new(DaemonClient {
                data_dir: tmp.path().join("data").join("terminals"),
                proc_supervisor_sock: None,
            }),
            Arc::new(PluginHost::new_full(
                Arc::new(PluginRegistry::empty()),
                repo.clone(),
                PathBuf::new(),
                tmp.path().join("plugins-data"),
                Vec::new(),
                events.clone(),
                role_cache.clone(),
                cove_cache.clone(),
            )),
            Arc::new(codex),
            Some(role_cache.clone()),
            Some(cove_cache.clone()),
        );

        let cove = repo
            .cove_create(NewCove {
                name: "runtime-recovery".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "runtime recovery wave".into(),
                sort: None,
                cwd: tmp.path().display().to_string(),
                attach_folder: false,
                theme: routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        cove_cache.insert(wave.id.clone(), cove.id.clone());

        let spec_card_id = new_id();
        let card_id_for_tx = spec_card_id.clone();
        let wave_id_for_tx = wave.id.clone();
        let cwd_for_tx = wave.cwd.clone();
        let role_cache_for_tx = role_cache.clone();
        let (spec_card, _) = write_with_event_typed(
            repo.as_ref(),
            ActorId::Kernel,
            EventScope::Wave {
                wave: wave.id.clone(),
                cove: cove.id.clone(),
            },
            None,
            &events,
            &role_cache,
            &cove_cache,
            move |tx| {
                Box::pin(async move {
                    let (card, _term, _token) = card_with_codex_create_tx(
                        tx,
                        card_id_for_tx,
                        wave_id_for_tx,
                        None,
                        cwd_for_tx,
                        json!({}),
                        Some("runtime recovery".into()),
                        None,
                        None,
                        CardRole::Spec,
                        false,
                        &role_cache_for_tx,
                        routes::theme::RequestTheme::default_dark(),
                    )
                    .await?;
                    Ok((card.clone(), Event::CardAdded(card)))
                })
            },
        )
        .await
        .unwrap();

        let sock = state.daemon.appserver_sock_path(&spec_card_id);
        let interrupt_marker = tmp.path().join("interrupt.marker");
        let env = json!({
            "FAKE_CODEX_INTERRUPT_MARKER": interrupt_marker.display().to_string(),
            "FAKE_CODEX_WEDGE_PROCESS_COUNT": wedge_process_count.to_string(),
        });
        let signal = calm_server::wire_spec_push_recovery_supervisor_with_watchdog_for_test(
            &state,
            &Settings::default(),
            &spec_card_id,
            wave.id.clone(),
            WATCHDOG,
        );
        let handle = spawn_spec_appserver_with_watchdog_config_and_recovery(
            &state.codex.codex_bin,
            &env,
            "goal",
            &sock,
            None,
            WATCHDOG,
            Some(signal),
        )
        .await
        .expect("fake app-server should boot");
        let thread_id = handle.thread_id.clone();
        let initial_pgid = handle.pgid;
        let sock_str = handle.sock.to_string_lossy().to_string();
        let start_time = handle.start_time;
        let boot_id = handle.boot_id.clone();

        let update_card_id = spec_card.id.to_string();
        let update_thread_id = thread_id.clone();
        write_with_event_typed(
            repo.as_ref(),
            ActorId::Kernel,
            EventScope::Card {
                card: spec_card.id.clone(),
                wave: wave.id.clone(),
                cove: cove.id.clone(),
            },
            None,
            &events,
            &role_cache,
            &cove_cache,
            move |tx| {
                Box::pin(async move {
                    let mut payload = spec_card.payload.clone();
                    let map = payload.as_object_mut().expect("spec payload object");
                    map.insert("codex_thread_id".into(), Value::String(update_thread_id));
                    map.insert("appserver_sock".into(), Value::String(sock_str));
                    map.insert("appserver_pgid".into(), Value::Number(initial_pgid.into()));
                    map.insert(
                        "appserver_start_time".into(),
                        start_time
                            .map(|v| Value::Number(serde_json::Number::from(v)))
                            .unwrap_or(Value::Null),
                    );
                    map.insert(
                        "appserver_boot_id".into(),
                        boot_id.map(Value::String).unwrap_or(Value::Null),
                    );
                    map.insert("push_watermark".into(), Value::Number(0.into()));
                    let card = card_update_tx(
                        tx,
                        &update_card_id,
                        CardPatch {
                            kind: None,
                            sort: None,
                            payload: Some(payload),
                            deletable: None,
                        },
                    )
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            },
        )
        .await
        .unwrap();

        let card_key: CardId = spec_card_id.clone().into();
        handle
            .install_watermark_sink(state.dispatcher.watermark_sink_for(card_key.clone()))
            .await;
        handle
            .install_queue_persist(state.dispatcher.queue_persist_for(card_key))
            .await;
        state
            .spec_push
            .park(wave.id.clone(), handle, state.aspects.as_ref())
            .await;

        Self {
            _tmp: tmp,
            repo,
            state,
            wave_id: wave.id,
            cove_id: cove.id,
            spec_card_id,
            interrupt_marker,
            initial_pgid,
        }
    }

    async fn push_watermark(&self) -> i64 {
        self.repo
            .spec_cards_for_boot_takeover()
            .await
            .unwrap()
            .into_iter()
            .find_map(|(c, w, _t, _p, _s, _st, _b, watermark)| {
                (c == self.spec_card_id && w == self.wave_id.as_str()).then_some(watermark)
            })
            .expect("spec card takeover row")
    }

    async fn current_pgid(&self) -> Option<i32> {
        self.repo
            .spec_cards_for_boot_takeover()
            .await
            .unwrap()
            .into_iter()
            .find_map(|(c, _w, _t, pgid, _s, _st, _b, _wm)| {
                (c == self.spec_card_id).then_some(pgid)
            })
            .flatten()
    }

    async fn emit_user_edit(&self, after: &str) -> i64 {
        self.repo
            .log_pure_event(
                ActorId::User,
                EventScope::Card {
                    card: self.spec_card_id.clone().into(),
                    wave: self.wave_id.clone(),
                    cove: self.cove_id.clone(),
                },
                None,
                &self.state.events,
                &self.state.card_role_cache,
                &self.state.wave_cove_cache,
                Event::WaveReportEdited {
                    wave_id: self.wave_id.clone(),
                    card_id: self.spec_card_id.clone().into(),
                    author: EditAuthor::User,
                    edit_id: new_id(),
                    summary_before: String::new(),
                    summary_after: after.into(),
                    body_before: String::new(),
                    body_after: after.into(),
                },
            )
            .await
            .expect("emit user edit")
    }

    async fn wait_for_interrupt_marker(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.interrupt_marker.exists() {
            assert!(
                Instant::now() < deadline,
                "fake app-server never saw turn/interrupt"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_for_new_pgid(&self, seen: &mut Vec<i32>) -> i32 {
        let deadline = Instant::now() + RECOVERY_DEADLINE;
        loop {
            if let Some(pgid) = self.current_pgid().await
                && !seen.contains(&pgid)
            {
                seen.push(pgid);
                return pgid;
            }
            assert!(
                Instant::now() < deadline,
                "runtime recovery did not persist a fresh pgid; seen={seen:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_watermark_at_least(&self, event_id: i64, label: &str) {
        let deadline = Instant::now() + RECOVERY_DEADLINE;
        loop {
            if self.push_watermark().await >= event_id {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{label} did not reach the spec push watermark"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_abandoned(&self) {
        let deadline = Instant::now() + RECOVERY_DEADLINE;
        loop {
            let rows = self.repo.events_since(0, None).await.unwrap();
            if rows.into_iter().any(|(_id, _ver, _scope, ev)| {
                matches!(ev, Event::SpecPushAbandoned { wave_id, .. } if wave_id == self.wave_id)
            }) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "sustained wedging did not emit SpecPushAbandoned"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn teardown(&self) {
        calm_server::terminal_sweeper::reap_spec_push(&self.state, &self.wave_id).await;
    }
}

#[tokio::test]
async fn spec_push_runtime_recovery_recovers_and_delivers_catchup_plus_live() {
    let h = RuntimeHarness::new(1).await;
    let mut pgids = vec![h.initial_pgid];

    let replay_event_id = h.emit_user_edit("after wedge").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.push_watermark().await,
        0,
        "running/wedging handle must not advance durable watermark before queued delivery"
    );

    h.wait_for_interrupt_marker().await;
    let post_pgid = h.wait_for_new_pgid(&mut pgids).await;
    assert!(h.state.spec_push.contains(&h.wave_id));
    assert_ne!(
        h.initial_pgid, post_pgid,
        "recovery must respawn a new process group"
    );

    h.wait_for_watermark_at_least(replay_event_id, "catch-up replay")
        .await;

    let live_event_id = h.emit_user_edit("after recovery").await;
    h.wait_for_watermark_at_least(live_event_id, "post-recovery live event")
        .await;

    h.teardown().await;
}

#[tokio::test]
async fn spec_push_runtime_recovery_second_wedge_after_recovery_rearms_and_recovers_again() {
    let h = RuntimeHarness::new(2).await;
    let mut pgids = vec![h.initial_pgid];

    let first = h.emit_user_edit("first wedge").await;
    h.wait_for_new_pgid(&mut pgids).await;

    let second = h.emit_user_edit("second wedge").await;
    h.wait_for_new_pgid(&mut pgids).await;
    assert_eq!(
        pgids.len(),
        3,
        "expected initial process plus two recovered process groups"
    );

    let final_live = h.emit_user_edit("healthy after second recovery").await;
    h.wait_for_watermark_at_least(
        first.max(second).max(final_live),
        "post-second-recovery live event",
    )
    .await;

    h.teardown().await;
}

#[tokio::test]
async fn spec_push_runtime_recovery_sustained_wedging_abandons_after_budget() {
    let h = RuntimeHarness::new(4).await;
    let mut pgids = vec![h.initial_pgid];

    let mut emitted = 0;
    let deadline = Instant::now() + RECOVERY_DEADLINE;
    while Instant::now() < deadline {
        emitted += 1;
        h.emit_user_edit(&format!("sustained wedge {emitted}"))
            .await;
        let wait = tokio::time::timeout(Duration::from_secs(3), h.wait_for_abandoned()).await;
        if wait.is_ok() {
            assert!(
                pgids.len() <= 4,
                "restart budget should allow at most three respawns; pgids={pgids:?}"
            );
            h.teardown().await;
            return;
        }
        if let Some(pgid) = h.current_pgid().await
            && !pgids.contains(&pgid)
        {
            pgids.push(pgid);
        }
    }

    panic!("sustained wedging did not exhaust the restart budget after {emitted} events");
}
