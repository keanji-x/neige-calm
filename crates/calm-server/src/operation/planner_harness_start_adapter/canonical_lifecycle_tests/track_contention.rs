//! Retained activation owns Track from preparation until harness installation.
use super::*;
use crate::test_seams::{PausePoint, install_pause_for_test};
use std::time::Duration;

async fn handoff(fx: &Fixture) -> crate::provider_execution::TrackFenceHandoff {
    crate::provider_execution::TrackFenceHandoff::new(
        crate::harness::run_loop::issuance::track_fence_in_for_test(
            &fx.payload.track_id,
            fx.state.operation_runtime.track_delete_locks(),
        )
        .await,
    )
}

async fn entered(pause: &PausePoint) {
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
}

async fn counts(fx: &Fixture, expected: i64) {
    let phases: Vec<String> = sqlx::query_scalar("SELECT phase FROM operations")
        .fetch_all(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(phases, vec!["succeeded"; expected as usize]);
    tokio::time::timeout(Duration::from_secs(5), async {
        while fx.state.shared_codex_appserver.turn_start_count_for_test() < expected as u64 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fx.state
            .shared_codex_appserver
            .started_thread_params_for_test()
            .len(),
        expected as usize
    );
    assert_eq!(
        fx.state.shared_codex_appserver.turn_start_count_for_test(),
        expected as u64
    );
}

async fn holds_track(point: &str) {
    let fx = Fixture::with_native(true).await;
    let (_auth, _token, source) = source().await;
    let pause = PausePoint::default();
    install_pause_for_test(point, fx.card.as_str(), pause.clone());
    let activation = wake(&fx, source);
    let competitor = async {
        entered(&pause).await;
        assert!(
            crate::per_card_lock::try_lock_key(
                fx.state.operation_runtime.track_delete_locks(),
                &fx.payload.track_id,
            )
            .is_none(),
            "activation must retain Track between stages"
        );
        let waiting = PausePoint::default();
        install_pause_for_test("key-lock-waiting", &fx.payload.track_id, waiting.clone());
        let acquiring = handoff(&fx);
        tokio::pin!(acquiring);
        tokio::select! {
            _ = entered(&waiting) => {},
            _ = &mut acquiring => panic!("handoff bypassed activation"),
        }
        waiting.release.notify_one();
        pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), acquiring)
            .await
            .unwrap()
            .release();
    };
    let (result, ()) = tokio::join!(activation, competitor);
    result.unwrap();
    counts(&fx, 1).await;
    fx.state
        .harness
        .get(&fx.worker)
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn retained_activation_holds_track_through_interact() {
    holds_track("conversation-start-prepared").await;
}
#[tokio::test]
async fn retained_activation_holds_track_before_install() {
    holds_track("retained-claim-committed").await;
}

async fn waits_before_insert(teardown: bool) {
    let fx = Fixture::with_native(true).await;
    let (_auth, _token, source) = source().await;
    let owner = handoff(&fx).await;
    let waiting = PausePoint::default();
    install_pause_for_test("key-lock-waiting", &fx.payload.track_id, waiting.clone());
    let activation = wake(&fx, source);
    let lifecycle = async {
        entered(&waiting).await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM operations")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
        drop(
            tokio::time::timeout(
                Duration::from_secs(1),
                fx.state.operation_runtime.lock_for_track_delete(),
            )
            .await
            .unwrap(),
        );
        let registration = fx
            .state
            .operation_runtime
            .location_admission()
            .unwrap()
            .teardown_targets
            .registration();
        if teardown {
            let track = fx
                .repo
                .track_get(&fx.payload.track_id)
                .await
                .unwrap()
                .unwrap();
            registration
                .register_location(&fx.payload.track_id, track.workspace.agent_cwd().into());
        }
        waiting.release.notify_one();
        owner.release();
        registration
    };
    let (result, registration) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(activation, lifecycle)
    })
    .await
    .unwrap();
    if teardown {
        assert!(
            matches!(result, Err(CalmError::WorkspaceBusy(_))),
            "{result:?}"
        );
        counts(&fx, 0).await;
        drop(
            tokio::time::timeout(
                Duration::from_secs(1),
                fx.state.operation_runtime.lock_for_track_delete(),
            )
            .await
            .unwrap(),
        );
    } else {
        result.unwrap();
        counts(&fx, 1).await;
        fx.state
            .harness
            .get(&fx.worker)
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
    drop(registration);
}
#[tokio::test]
async fn retained_activation_waits_for_planner_handoff_before_insert() {
    waits_before_insert(false).await;
}
#[tokio::test]
async fn retained_activation_refuses_teardown_admitted_during_wait() {
    waits_before_insert(true).await;
}

#[tokio::test]
async fn retained_activation_released_track_refuses_workspace_repair() {
    let fx = Fixture::new().await;
    let locks = fx.state.operation_runtime.track_delete_locks();
    let track = fx
        .repo
        .track_get(&fx.payload.track_id)
        .await
        .unwrap()
        .unwrap();
    let held = crate::workspace_service::HeldTrack::new(
        crate::per_card_lock::lock_key(locks, &fx.payload.track_id).await,
    );
    held.release();
    let _owner = crate::per_card_lock::lock_key(locks, &fx.payload.track_id).await;
    let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = effects.clone();
    let result = crate::workspace_service::retained_worktree(
        fx.repo.clone(),
        locks,
        &track,
        crate::workspace_service::RetainedAdmission::Held(held),
        crate::workspace_service::RetainedPreparation::Locator,
        move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok(()) }
        },
    )
    .await;
    assert!(matches!(result, Err(CalmError::Conflict(_))), "{result:?}");
    assert_eq!(
        effects.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "released admission must stop before every repair effect"
    );
}

#[tokio::test]
async fn retained_activation_handed_off_missing_handle_refuses_reinstallation() {
    let fx = Fixture::with_native(true).await;
    let (_auth, _token, source) = source().await;
    wake(&fx, source.clone()).await.unwrap();
    counts(&fx, 1).await;
    let handle = fx.state.harness.remove(&fx.worker).unwrap();
    handle.shutdown().await.unwrap();
    let route = RouteState::from_ref(&fx.state);
    let mut adapter = route.planner_start_adapter.as_ref().clone();
    adapter.invocation = retained::StartInvocation::Retained {
        source: source.into(),
        workspace_root: route.workspace_root,
    };
    let held = crate::workspace_service::HeldTrack::new(
        crate::per_card_lock::lock_key(
            fx.state.operation_runtime.track_delete_locks(),
            &fx.payload.track_id,
        )
        .await,
    );
    held.release();
    let _owner = crate::per_card_lock::lock_key(
        fx.state.operation_runtime.track_delete_locks(),
        &fx.payload.track_id,
    )
    .await;
    let result = adapter
        .retained_spawn(
            &retained::RetainedPayload {
                canonical_activation_version: 2,
                card_id: fx.card.to_string(),
                worker_session_id: fx.worker.clone(),
            },
            &route.events,
            fx.state.operation_runtime.track_delete_locks(),
            held,
        )
        .await;
    assert!(matches!(result, Err(CalmError::Conflict(_))));
    assert!(fx.state.harness.get(&fx.worker).is_none());
    counts(&fx, 1).await;
}
