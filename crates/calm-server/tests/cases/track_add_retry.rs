//! Canonical receipt replay is independent of current native availability.
use super::*;

impl Boot {
    pub(super) async fn activate_caller(&self, caller: &Caller) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/cards/{}/planner/input",
                    caller.identity.card_id
                ))
                .header("content-type", "application/json")
                .header("Idempotency-Key", "creator-turn")
                .body(Body::from(
                    json!({"text":"Create delegated work from this real Human turn"}).to_string(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let ready: bool = sqlx::query_scalar("SELECT COALESCE(json_array_length(handle_state_json,'$.canonical_inputs.attempts'),0)>0 \
            FROM worker_sessions WHERE id=?1")
                    .bind(&caller.identity.session_id).fetch_one(self.repo.pool()).await.unwrap();
                if ready {break;}
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.expect("real accepted input must cross before the creator uses tools");
    }
}

#[tokio::test]
async fn u9_canonical_creator_receipt_does_not_wait_for_native_start() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    boot.codex.fail_next_thread_start_for_test();
    let operations = boot.count("SELECT count(*) FROM operations").await;
    let added = boot.add(&planner, boot.args("deferred")).await.unwrap();
    assert_eq!(
        boot.add(&planner, boot.args("deferred")).await.unwrap(),
        added
    );
    assert_eq!(
        boot.count("SELECT count(*) FROM operations").await,
        operations
    );
    assert_eq!(
        boot.deliveries(added["track_id"].as_str().unwrap()).await,
        1
    );
}

/// Replays select the durable receipt before applying the creator's current
/// readiness. The derived provider does not participate in the request fingerprint.
#[tokio::test]
async fn track_add_admin_replay_wins_over_current_provider_unavailability() {
    let boot = boot(16).await;
    let (creator, planner) = boot.user_track("portfolio").await;
    let created = boot
        .add(&planner, boot.args("readiness-replay"))
        .await
        .unwrap();
    let before = boot.track_count().await;
    use calm_server::mcp_server::tools::track_add::{TrackAddArgs, TrackAddRequest};
    let mut request = TrackAddRequest {
        creator_track_id: creator,
        area_id: boot.area_id.clone(),
        create_actor: planner.identity.to_actor_id(),
        start_actor: calm_server::ids::ActorId::AiPlanner(planner.identity.card_id.clone().into()),
        planner_provider: AgentProvider::Claude,
        args: serde_json::from_value::<TrackAddArgs>(boot.args("readiness-replay")).unwrap(),
    };
    // Keep the calling session's real Codex binding intact while the mint's
    // inherited-provider readiness changes at the route boundary.
    let creator = boot.ctx.track_creator.get().unwrap().upgrade().unwrap();
    let replay = creator.add(request.clone()).await.unwrap();
    assert_eq!(replay.id.as_str(), created["track_id"].as_str().unwrap());
    assert_eq!(boot.track_count().await, before);
    request.args = serde_json::from_value(boot.args("readiness-mint")).unwrap();
    assert!(matches!(
        creator.add(request).await,
        Err(calm_server::mcp_server::tools::track_add::TrackAddRefusal::ProviderUnavailable(_))
    ));
    assert_eq!(boot.track_count().await, before);
}
