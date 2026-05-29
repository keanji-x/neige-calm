//! Issue #318 INV-1 (b) — observability invariant for spec-push
//! abandonment on boot takeover.
//!
//! ## What we pin
//!
//! When [`calm_server::takeover_spec_appservers_on_boot`] gives up on a
//! wave's spec push channel (any [`TakeoverOutcome::Inert`] exit — the
//! `mkdir` failure, `-32600 "no rollout"`, spawn/connect failure, or
//! `thread/resume` handshake failure), it MUST emit a persisted
//! [`Event::SpecPushAbandoned`] for the abandoned wave.
//!
//! Until #318 closed the R1-B1 review nit from #315, the only signal
//! was `tracing::warn!`. That's:
//!   * not persisted — kernel restart loses it;
//!   * not subscribable — operator dashboards / a future re-run path
//!     can't react to it;
//!   * fundamentally unobservable to anything but a human reading the
//!     log stream.
//!
//! The spec card's payload is cleared on the `-32600 "no rollout"`
//! branch (drops `codex_thread_id`), so on subsequent boots the wave
//! is EXCLUDED from `spec_cards_for_boot_takeover` — i.e. boot won't
//! retry. Every event with `events.id > push_watermark` for that
//! wave (scope_wave = wave_id) is thereby stranded. The
//! `SpecPushAbandoned` payload carries `last_envelope_id` as an upper
//! bound on the stranded set (every id in `(push_watermark,
//! last_envelope_id]` is at risk), so consumers can size the loss
//! without re-running the boot SELECT.
//!
//! ## How we drive the Inert path without `codex-e2e`
//!
//! We point `CodexClient::codex_bin` at a guaranteed-nonexistent path
//! under the test's TempDir. `resume_spec_appserver`'s
//! `Command::new(codex_bin).spawn()` returns Err immediately (ENOENT),
//! the classifier sees a non-`-32600` error, and falls into the second
//! Inert sub-branch — the one that doesn't clear push state. The emit
//! site is shared between BOTH Inert sub-branches (mkdir failure +
//! resume failure), so this single configuration covers the
//! observability invariant.
//!
//! Plants the spec card via direct SQL (`UPDATE cards SET role='spec',
//! payload=…`) — same pattern as `tests/role_enforcement.rs`. The
//! takeover query selects on `c.role = 'spec' AND
//! json_extract(payload,'$.codex_thread_id') IS NOT NULL AND
//! w.lifecycle NOT IN ('done','canceled','failed')`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{ArtifactRef, BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, WaveId};
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use serde_json::json;
use tempfile::TempDir;

/// Stub path for [`DaemonClient::session_daemon_bin`]. Boot takeover
/// (`takeover_spec_appservers_on_boot`) never spawns the session
/// daemon — it only touches the `appserver_sock_path` /
/// `appserver_sock_dir` helpers on `DaemonClient`, which derive paths
/// from `data_dir` only. The bin field just needs to be a path-shaped
/// value to construct `DaemonClient`.
fn stub_session_daemon_bin() -> PathBuf {
    PathBuf::from("/nonexistent/session-daemon-stub-never-spawned")
}

/// Drain the broadcast bus for up to `budget`, collecting every
/// `SpecPushAbandoned` envelope.
async fn collect_spec_push_abandoned(
    rx: &mut tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
    budget: Duration,
) -> Vec<BroadcastEnvelope> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return out;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => {
                if matches!(env.event, Event::SpecPushAbandoned { .. }) {
                    out.push(env);
                }
            }
            Ok(Err(_)) | Err(_) => return out,
        }
    }
}

/// INV-1 (b) — boot takeover MUST emit a persisted, broadcast
/// `Event::SpecPushAbandoned` for a wave it leaves inert. Without this
/// signal every event with `events.id > push_watermark` for that wave
/// sits stranded after the spec card's `codex_thread_id` is cleared
/// (R1-B1 review nit from #315).
#[tokio::test]
async fn inv1_stranded_envelope_must_be_observable() {
    // Hold the concrete `Arc<SqlxRepo>` so we can use `.pool()` for the
    // raw-SQL planting step below. Reusing it as `Arc<dyn Repo>` for
    // `AppState::from_parts` is a cheap clone.
    let tmp = TempDir::new().expect("tempdir");
    let typed: Arc<SqlxRepo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = typed.clone();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: stub_session_daemon_bin(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();

    // `codex_bin` MUST point at a path that doesn't exist so
    // `Command::spawn()` ENOENTs. Anything under the TempDir works —
    // the dir exists but no file inside it does.
    let mut codex = CodexClient::new_stub();
    codex.codex_bin = tmp
        .path()
        .join("definitely-not-codex")
        .to_string_lossy()
        .to_string();

    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-inv-01-watermark-monotonic"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(codex),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    // -------- Plant the scenario --------
    let cove = repo
        .cove_create(NewCove {
            name: "inv-01-cove".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "inv-01-wave".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    // Promote to spec role + plant `codex_thread_id` so the takeover
    // SELECT picks the row up (matches the
    // `spec_cards_for_boot_takeover` filter — see `db/sqlite.rs`).
    sqlx::query("UPDATE cards SET role = 'spec', payload = json(?1) WHERE id = ?2")
        .bind(json!({ "codex_thread_id": "thr-inv-01" }).to_string())
        .bind(card.id.as_str())
        .execute(typed.pool())
        .await
        .expect("plant spec role + codex_thread_id");

    // Re-seed caches now that the row is shaped.
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    // -------- Plant a wave-scoped event so `last_envelope_id > 0` --------
    //
    // Emit a `task.completed` scoped to the wave; its `events.id`
    // becomes the upper-bound the `SpecPushAbandoned.last_envelope_id`
    // payload should carry.
    let scope = EventScope::Wave {
        wave: WaveId::from(wave.id.to_string()),
        cove: cove.id.clone(),
    };
    let envelope_id_pre = repo
        .log_pure_event(
            ActorId::User,
            scope,
            None,
            &state.events,
            &state.card_role_cache,
            &state.wave_cove_cache,
            Event::TaskCompleted {
                idempotency_key: "inv-01-precursor".into(),
                result: json!({ "note": "wave-scoped precursor" }),
                artifacts: Vec::<ArtifactRef>::new(),
            },
        )
        .await
        .expect("plant precursor task.completed");
    assert!(envelope_id_pre > 0, "precursor event MUST land with id > 0");

    // -------- Subscribe BEFORE the takeover sweep --------
    let mut rx = state.events.subscribe();

    // -------- Run takeover --------
    calm_server::takeover_spec_appservers_on_boot(&state).await;

    // -------- Drain the bus, expect exactly one SpecPushAbandoned --------
    let abandoned = collect_spec_push_abandoned(&mut rx, Duration::from_secs(2)).await;
    assert_eq!(
        abandoned.len(),
        1,
        "takeover MUST emit exactly one SpecPushAbandoned for the inert wave \
         (got {} envelopes)",
        abandoned.len()
    );
    let env = &abandoned[0];
    match &env.event {
        Event::SpecPushAbandoned {
            wave_id,
            cove_id,
            last_envelope_id,
        } => {
            assert_eq!(wave_id.as_str(), wave.id.as_str());
            assert_eq!(cove_id.as_str(), cove.id.as_str());
            assert!(
                *last_envelope_id >= envelope_id_pre,
                "last_envelope_id ({}) MUST be >= the precursor's id ({})",
                last_envelope_id,
                envelope_id_pre
            );
        }
        other => panic!("expected SpecPushAbandoned, got {other:?}"),
    }
    // Persisted: BroadcastEnvelope from `log_pure_event` carries the
    // real `events.id` (NOT 0), which is what makes the signal
    // survivable across kernel restart.
    assert!(
        env.id > 0,
        "SpecPushAbandoned envelope id MUST be > 0 (persisted), got {}",
        env.id
    );

    // -------- Confirm the wave is in the inert end-state --------
    //
    // The Inert path we hit here is the non-`-32600` branch, which
    // does NOT clear `codex_thread_id` (so the next boot retries).
    // We assert it's still present, which is the contract: the
    // observability signal is independent of whether the wave is
    // retried.
    let row: (Option<String>,) = sqlx::query_as(
        "SELECT json_extract(payload, '$.codex_thread_id') FROM cards WHERE id = ?1",
    )
    .bind(card.id.as_str())
    .fetch_one(typed.pool())
    .await
    .unwrap();
    assert_eq!(
        row.0.as_deref(),
        Some("thr-inv-01"),
        "non-'-32600' Inert branch must leave codex_thread_id intact (next boot retries); \
         abandonment signal is orthogonal"
    );
}

/// INV-1 (b) — **historical pre-#329 sketch**, preserved as an
/// alternate-angle record.
///
/// **Status**: the underlying observability gap this skeleton documented
/// was closed by **PR #329** (R1-B1 fix), which added
/// `Event::SpecPushAbandoned { wave_id, cove_id, last_envelope_id }` and
/// the inert-classifier emit site. The live behavioral test for this
/// invariant is [`inv1_stranded_envelope_must_be_observable`] above —
/// that's the one that exercises the real production path end-to-end.
///
/// This function is **deliberately kept** as a documented pre-fix
/// sketch: it captures what the test could look like *before* the
/// production seam existed, when the only honest encoding was an
/// `#[ignore]`'d `panic!` with a `blocked-by:` reason narrating the
/// gap. The multi-angle perspective (live behavioral + frozen
/// historical) is intentional — the contrast makes the design move
/// visible to future readers.
///
/// It stays `#[ignore]`'d (and asserts nothing) so it does not
/// duplicate the behavioral coverage above.
///
/// Original sketch (verbatim from the v3 attempt):
///
/// ```ignore
/// // 1. Plant a wave + spec card; do NOT install a SpecPushHandle.
/// // 2. Persist a wave-scoped event(id=N) via the events log.
/// // 3. Drive the takeover path to its inert classifier (simulate
/// //    `-32600 no rollout` from thread/resume) so the push state
/// //    is cleared and the card is excluded from boot takeover.
/// // 4. Assert one of the new surfaces fires:
/// //      let stranded = repo.stranded_envelopes(&wave.id).await?;
/// //      assert!(stranded.contains(&N),
/// //          "INV-1 (b) violated: event N is in the log, the spec card
/// //           is no longer takeover-eligible, but no stranding signal
/// //           exists for operator / replay recovery.");
/// //    OR (alternative seam):
/// //      assert!(events_for_wave(&wave.id).await
/// //          .iter().any(|e| matches!(e, Event::SpecPushAbandoned { last_envelope_id, .. }
/// //                                  if *last_envelope_id >= N)));
/// ```
///
/// PR #329 picked the second seam (the `Event::SpecPushAbandoned`
/// variant) — see the live behavioral test above for the exercised
/// shape.
#[tokio::test]
#[ignore = "historical pre-fix sketch: the stranded-envelope observability gap this \
            skeleton documented was addressed by PR #329, which added \
            Event::SpecPushAbandoned and the inert-classifier emit site. The live \
            behavioral test for INV-1 (b) is `inv1_stranded_envelope_must_be_observable` \
            in this same file (it exercises the production seam end-to-end). This \
            function is preserved as a documented alternate-angle / historical record \
            of what the test looked like before the seam existed; it intentionally \
            asserts nothing and stays `#[ignore]`'d so it does not duplicate the \
            behavioral coverage above. See: dispatcher.rs::Inner::push_to_spec \
            (no-handle arm), lib.rs::try_takeover_one_wave (inert classifier), \
            db/sqlite.rs::spec_cards_for_boot_takeover (filter that excludes cleared \
            rows). INV-1 (b) in #318; addressed by #329."]
async fn inv1_stranded_envelope_skeleton_pre_329() {
    // Body intentionally empty: this is a frozen pre-#329 sketch, not
    // a live behavioral test. The behavioral coverage lives in
    // `inv1_stranded_envelope_must_be_observable` above (which exercises
    // the `Event::SpecPushAbandoned` seam added by #329). Running this
    // with `--ignored` should be a no-op — there is nothing left to
    // assert that the live test doesn't already cover.
}
