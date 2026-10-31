//! Real detector episodes, boot issuer, receipt replay and native turns.
use super::*;
use crate::worker_quiet::{ObservedQuietEpisode, QuietWorkerInbox, WorkerQuietDetector};
use crate::worker_watch::{WorkerWatcher, watcher_card_id};
use std::sync::Mutex;

#[derive(Default)]
struct Capture(Mutex<Vec<ObservedQuietEpisode>>);
impl QuietWorkerInbox for Capture {
    fn deliver<'a>(
        &'a self,
        episode: ObservedQuietEpisode,
    ) -> futures::future::BoxFuture<'a, crate::error::Result<()>> {
        self.0.lock().unwrap().push(episode);
        Box::pin(async { Ok(()) })
    }
}
pub(super) struct ObservedWorker {
    pub(super) task: String,
    pub(super) key: String,
    terminal: String,
    entry: Arc<crate::terminal_renderer::RendererEntry>,
    stamp: i64,
}
pub(super) async fn worker(fixture: &Fixture, state: &AppState, track: &str) -> ObservedWorker {
    use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let key = crate::model::new_id();
    let task = format!("{track}:{key}");
    let card = crate::model::new_id();
    let session = crate::model::new_id();
    let op = SqlxOperationRepo::new(fixture.repo.db().clone())
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: crate::model::new_id(),
                idempotency_key: Some(task.clone()),
                payload_hash: key.clone(),
            },
            json!({"track_id":track,"cmd":"true","idempotency_key":task}),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET phase='succeeded' WHERE id=?1")
        .bind(&op)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    let (_, terminal, _) = crate::db::sqlite::card_with_codex_create_tx(
        &mut tx,
        card.clone(),
        &session,
        Some(&op),
        track.into(),
        None,
        None,
        fixture.root.path().display().to_string(),
        json!({}),
        None,
        None,
        None,
        crate::model::CardRole::Worker,
        true,
        &state.card_role_cache,
        crate::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    fixture
        .repo
        .session_projection_set_status_for_card(
            &card,
            crate::session_projection_repo::WorkerSessionState::Running,
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,declared_by,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,'codex','test','[]','running',?4,'user',?5,?5)")
        .bind(&task).bind(track).bind(&key).bind(card).bind(crate::model::now_ms()).execute(fixture.repo.pool()).await.unwrap();
    let entry =
        state
            .terminal_renderer
            .insert_test_entry(crate::terminal_renderer::RendererConfig {
                terminal_id: terminal.id.clone(),
                cols: 80,
                rows: 24,
                buffer_bytes: 8192,
                terminal_fg: (220, 220, 220),
                terminal_bg: (15, 20, 24),
                program: "/bin/sh".into(),
                args: vec![],
                envs: vec![],
                cwd: fixture.root.path().display().to_string(),
                supervisor_sock: Default::default(),
            });
    let stamp = crate::model::now_ms() - 10_000;
    entry
        .last_output_ms
        .store(stamp, std::sync::atomic::Ordering::SeqCst);
    ObservedWorker {
        task,
        key,
        terminal: terminal.id,
        entry,
        stamp,
    }
}
fn detector(state: &AppState, inbox: Arc<dyn QuietWorkerInbox>) -> WorkerQuietDetector {
    WorkerQuietDetector::new(
        state.repo.clone(),
        state.terminal_renderer.clone(),
        inbox,
        Duration::from_secs(5),
    )
}
async fn episode(state: &AppState, observed: &ObservedWorker) -> ObservedQuietEpisode {
    let capture = Arc::new(Capture::default());
    assert_eq!(
        detector(state, capture.clone())
            .tick(crate::model::now_ms())
            .await,
        vec![format!("{}:{}", observed.task, observed.stamp)]
    );
    capture.0.lock().unwrap().pop().unwrap()
}

#[tokio::test]
async fn detector_runs_native_bootstrap_body_and_later_episode_on_original_worker() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let observed = worker(&fixture, &state, &track).await;
    let watcher = Arc::new(WorkerWatcher::new(&state));
    let mut detector = detector(&state, watcher.clone());
    assert_eq!(detector.tick(crate::model::now_ms()).await.len(), 1);
    let card = watcher_card_id(&track);
    let original = recovery::receipts(&fixture, &card).await;
    assert_eq!(original.len(), 2);
    assert_eq!(
        original[0].acceptance.quiet_observation(),
        original[1].acceptance.quiet_observation()
    );
    let (original_worker, _) = frame(&fixture, &card).await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card: &card,
    };
    let call = next(&mut peer, true).await;
    native
        .complete(
            &mut peer,
            call,
            crate::worker_watch::watcher::BOOTSTRAP_TEXT.trim_end(),
            "watch-bootstrap",
        )
        .await;
    let text = crate::worker_watch::watch_text(&observed.key, &observed.task, 5).unwrap();
    let call = next(&mut peer, false).await;
    native.complete(&mut peer, call, &text, "watch-body").await;
    phase(&fixture, &card, HarnessPhaseTag::TurnCompleted).await;
    let replay = episode(&state, &observed).await;
    let (a, b) = tokio::join!(watcher.deliver(replay.clone()), watcher.deliver(replay));
    a.unwrap();
    b.unwrap();
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    let later_stamp = observed.stamp + 1;
    observed
        .entry
        .last_output_ms
        .store(later_stamp, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(detector.tick(crate::model::now_ms()).await.len(), 1);
    let call = next(&mut peer, false).await;
    native.complete(&mut peer, call, &text, "watch-later").await;
    let final_frame = phase(&fixture, &card, HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(frame(&fixture, &card).await.0, original_worker);
    assert_eq!(final_frame.canonical_inputs.attempts.len(), 3);
    assert_eq!(recovery::receipts(&fixture, &card).await.len(), 3);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn observed_episode_refuses_renderer_or_sql_changes_before_acceptance() {
    for change in [
        "output",
        "exit",
        "unreadable",
        "remove",
        "replace",
        "task",
        "role",
        "worker",
        "closed",
        "area",
        "retired",
    ] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let track = activity(&state, &app, &cookie).await;
        let observed = worker(&fixture, &state, &track).await;
        let sealed = episode(&state, &observed).await;
        match change {
            "output" => observed
                .entry
                .last_output_ms
                .store(observed.stamp + 1, std::sync::atomic::Ordering::SeqCst),
            "exit" => {
                *observed.entry.exit.lock().unwrap() =
                    Some(crate::terminal_renderer::TerminalExitInfo {
                        code: Some(0),
                        pty_seq: 0,
                        render_rev: 0,
                        exited_at: std::time::SystemTime::now(),
                    })
            }
            "unreadable" => observed
                .entry
                .handle
                .model_view
                .lock()
                .unwrap()
                .invalidate("lost screen"),
            "remove" => state
                .terminal_renderer
                .forget_entry_for_test(&observed.terminal),
            "replace" => {
                state
                    .terminal_renderer
                    .insert_test_entry(crate::terminal_renderer::RendererConfig {
                        terminal_id: observed.terminal.clone(),
                        cols: 80,
                        rows: 24,
                        buffer_bytes: 8192,
                        terminal_fg: (220, 220, 220),
                        terminal_bg: (15, 20, 24),
                        program: "/bin/sh".into(),
                        args: vec![],
                        envs: vec![],
                        cwd: fixture.root.path().display().to_string(),
                        supervisor_sock: Default::default(),
                    })
                    .last_output_ms
                    .store(observed.stamp, std::sync::atomic::Ordering::SeqCst);
            }
            "task" => {
                sqlx::query("UPDATE tasks SET status='verifying' WHERE id=?1")
                    .bind(&observed.task)
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            }
            "role" => {
                sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
                    .bind(&sealed.fact().worker_card_id)
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            }
            "worker" => {
                sqlx::query("UPDATE tasks SET worker_card_id=NULL WHERE id=?1")
                    .bind(&observed.task)
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            }
            "closed" => {
                sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
                    .bind(&track)
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            }
            "area" => {
                let area = post(
                    &app,
                    &cookie,
                    "/api/areas",
                    "other-area",
                    json!({"name":"Other area","color":"#123456"}),
                )
                .await;
                sqlx::query("UPDATE tracks SET area_id=?2 WHERE id=?1")
                    .bind(&track)
                    .bind(area["id"].as_str().unwrap())
                    .execute(fixture.repo.pool())
                    .await
                    .unwrap();
            }
            "retired" => state.dispatcher.retire(),
            _ => unreachable!(),
        }
        assert!(
            WorkerWatcher::new(&state).deliver(sealed).await.is_err(),
            "{change}"
        );
        assert!(
            state
                .repo
                .card_get(&watcher_card_id(&track))
                .await
                .unwrap()
                .is_none(),
            "{change}"
        );
        authority::assert_empty(&fixture, &watcher_card_id(&track)).await;
        shutdown(&fixture, &state).await;
    }
}

#[tokio::test]
async fn generic_text_issuer_cannot_accept_watcher_source() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let issuer = state.worker_watcher_issuer.as_ref().unwrap();
    let route = crate::state::RouteState::from_ref(&state);
    assert!(matches!(
        issuer
            .submit(
                &route,
                track.clone().into(),
                "bootstrap".into(),
                "ordinary-key".into(),
                "ordinary-text".into()
            )
            .await,
        Err(crate::error::CalmError::Forbidden(_))
    ));
    let principal = crate::auth::Principal {
        user_id: calm_types::organization::LEGACY_OWNER_USER_ID.into(),
        display_name: "Owner".into(),
        role: "owner".into(),
        session_id: "fixture".into(),
    };
    assert!(matches!(
        issuer
            .submit_prepared(
                &route,
                principal,
                track.clone().into(),
                "arbitrary".into(),
                |_| Box::pin(async { panic!("Watcher text resolve must not run") }),
                |_| Box::pin(async { panic!("Watcher text preparation must not run") }),
            )
            .await,
        Err(crate::error::CalmError::Forbidden(_))
    ));
    assert!(
        state
            .repo
            .card_get(&watcher_card_id(&track))
            .await
            .unwrap()
            .is_none()
    );
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn sql_source_owner_refuses_generic_or_mismatched_observation_facts() {
    use crate::db::sqlite::{self as db, conversation};
    use calm_types::system_assistant::SystemAssistantSource;
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let observed = worker(&fixture, &state, &track).await;
    let sealed = episode(&state, &observed).await;
    WorkerWatcher::new(&state)
        .deliver(sealed.clone())
        .await
        .unwrap();
    let card = watcher_card_id(&track);
    let original = recovery::receipts(&fixture, &card).await;
    for change in [
        "generic",
        "database",
        "track",
        "area",
        "task",
        "attempt",
        "worker",
        "session",
        "terminal",
        "stamp",
        "threshold",
        "source",
        "actor",
        "target",
    ] {
        let mut fact = sealed.fact().clone();
        // Fresh event identity lets this exercise source qualification, not duplicate receipt rejection.
        fact.last_output_ms -= 1;
        match change {
            "database" => fact.database_id = "foreign".into(),
            "track" => fact.track_id = "foreign".into(),
            "area" => fact.area_id = "foreign".into(),
            "task" => fact.task_key = "foreign".into(),
            "attempt" => fact.attempt_id = "foreign".into(),
            "worker" => fact.worker_card_id = "foreign".into(),
            "session" => fact.worker_session_id = "foreign".into(),
            "terminal" => fact.terminal_id = "foreign".into(),
            "stamp" => fact.last_output_ms = 0,
            "threshold" => fact.quiet_after_ms = 0,
            _ => {}
        }
        let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
            .await
            .unwrap();
        let event = crate::event::Event::AssistantInputRequested {
            card_id: if change == "target" {
                "foreign".into()
            } else {
                card.clone().into()
            },
            track_id: track.clone().into(),
            original_worker: original[0].original_worker.clone(),
            source: if change == "source" {
                SystemAssistantSource::TodaySummary
            } else {
                SystemAssistantSource::WorkerWatcher
            },
            subject: fact.subject(),
            version: 1,
            text: "untrusted text".into(),
        };
        let actor = if change == "actor" {
            crate::ids::ActorId::User
        } else {
            crate::ids::ActorId::Kernel
        };
        let appended = db::append_decision_event_in_tx(
            &mut tx,
            &actor,
            &crate::event::EventScope::Card {
                card: card.clone().into(),
                track: track.clone().into(),
                area: sealed.fact().area_id.clone().into(),
            },
            None,
            &event,
        )
        .await;
        if matches!(change, "actor" | "target") {
            assert!(
                matches!(appended, Err(calm_truth::error::TruthError::Forbidden(_))),
                "{change}"
            );
            tx.rollback().await.unwrap();
            assert_eq!(recovery::receipts(&fixture, &card).await, original);
            continue;
        }
        let id = appended.unwrap();
        let result = if change == "generic" {
            conversation::system_assistant_accept_tx(&mut tx, &card, id).await
        } else {
            conversation::worker_watcher_accept_tx(&mut tx, &card, id, &fact).await
        };
        assert!(
            matches!(
                result,
                Err(calm_truth::error::TruthError::Core(
                    calm_types::error::CoreError::Conflict(_)
                ))
            ),
            "{change}"
        );
        tx.rollback().await.unwrap();
        assert_eq!(recovery::receipts(&fixture, &card).await, original);
    }
    shutdown(&fixture, &state).await;
}

pub(super) async fn accept(fixture: &Fixture, state: &AppState, track: &str) -> (Value, String) {
    let observed = worker(fixture, state, track).await;
    let text = crate::worker_watch::watch_text(&observed.key, &observed.task, 5).unwrap();
    let sealed = episode(state, &observed).await;
    WorkerWatcher::new(state).deliver(sealed).await.unwrap();
    // Completing the observed task must not revoke its already permanent input.
    sqlx::query("UPDATE tasks SET status='done',finished_at_ms=?2 WHERE id=?1")
        .bind(&observed.task)
        .bind(crate::model::now_ms())
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    (
        json!({"track_id":track,"card_id":watcher_card_id(track)}),
        text,
    )
}

struct LoseAck {
    watcher: WorkerWatcher,
    lost: std::sync::atomic::AtomicBool,
}
impl QuietWorkerInbox for LoseAck {
    fn deliver<'a>(
        &'a self,
        episode: ObservedQuietEpisode,
    ) -> futures::future::BoxFuture<'a, crate::error::Result<()>> {
        Box::pin(async move {
            self.watcher.deliver(episode).await?;
            if !self.lost.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::error::CalmError::ServiceUnavailable(
                    "committed ACK lost".into(),
                ));
            }
            Ok(())
        })
    }
}
#[tokio::test]
async fn lost_committed_ack_and_cache_loss_replay_permanent_original_without_accepting_twice() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let _observed = worker(&fixture, &state, &track).await;
    let inbox = Arc::new(LoseAck {
        watcher: WorkerWatcher::new(&state),
        lost: Default::default(),
    });
    let mut first = detector(&state, inbox.clone());
    assert!(first.tick(crate::model::now_ms()).await.is_empty());
    let card = watcher_card_id(&track);
    let original = recovery::receipts(&fixture, &card).await;
    assert_eq!(original.len(), 2);
    assert_eq!(first.tick(crate::model::now_ms()).await.len(), 1);
    assert_eq!(
        detector(&state, inbox)
            .tick(crate::model::now_ms())
            .await
            .len(),
        1
    );
    assert_eq!(recovery::receipts(&fixture, &card).await, original);
    assert_eq!(
        frame(&fixture, &card)
            .await
            .1
            .canonical_inputs
            .pending
            .len(),
        2
    );
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn foreign_boot_scope_and_historical_namespace_refuse_live_episode() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let observed = worker(&fixture, &state, &track).await;
    let sealed = episode(&state, &observed).await;
    fixture.data_dir = Some(fixture.root.path().join("foreign-boot"));
    let (other, _other_app, _other_cookie) = fixture.application().await;
    let foreign_route = crate::state::RouteState::from_ref(&other);
    assert!(matches!(
        other
            .worker_watcher_issuer
            .as_ref()
            .unwrap()
            .submit_quiet(
                &foreign_route,
                state.terminal_renderer.clone(),
                sealed.clone(),
            )
            .await,
        Err(crate::error::CalmError::Forbidden(_))
    ));
    let card = watcher_card_id(&track);
    assert!(recovery::receipts(&fixture, &card).await.is_empty());
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    crate::db::sqlite::card_create_with_id_tx(
        &mut tx,
        card.clone(),
        crate::model::NewCard {
            track_id: track.into(),
            kind: "codex".into(),
            title: None,
            sort: None,
            payload: json!({"harness_profile":"assistant","assistant_provider":"codex"}),
        },
        crate::model::CardRole::Assistant,
        false,
        &state.card_role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        WorkerWatcher::new(&state).deliver(sealed).await,
        Err(crate::error::CalmError::IdempotencyKeyExhausted(_))
    ));
    assert!(recovery::receipts(&fixture, &card).await.is_empty());
    shutdown(&fixture, &state).await;
    shutdown(&fixture, &other).await;
}

#[tokio::test]
async fn output_after_observation_point_preserves_original_and_starts_next_episode() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let observed = worker(&fixture, &state, &track).await;
    let sealed = episode(&state, &observed).await;
    let card = watcher_card_id(&track);
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "system-assistant-before-commit",
        &card,
        pause.clone(),
    );
    let watcher = Arc::new(WorkerWatcher::new(&state));
    let task = tokio::spawn({
        let watcher = watcher.clone();
        async move { watcher.deliver(sealed).await }
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    observed
        .entry
        .last_output_ms
        .store(observed.stamp + 1, std::sync::atomic::Ordering::SeqCst);
    pause.release.notify_one();
    task.await.unwrap().unwrap();
    assert_eq!(recovery::receipts(&fixture, &card).await.len(), 2);
    assert_eq!(
        detector(&state, watcher)
            .tick(crate::model::now_ms())
            .await
            .len(),
        1
    );
    assert_eq!(recovery::receipts(&fixture, &card).await.len(), 3);
    shutdown(&fixture, &state).await;
}
