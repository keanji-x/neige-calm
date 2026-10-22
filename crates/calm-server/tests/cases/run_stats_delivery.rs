//! Durable statistics exercised through actual worker reporting and Git settlement.
use super::git_delivery::fixture;
use calm_server::track_planner_context::TrackPlannerContextProjector;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_stats_delivered_commits_survive_event_retention_and_ignore_no_change() {
    let fx = fixture().await;
    let worker = fx.claude_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("stats-delivery", "claude", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("stats.txt"), "delivery\n").unwrap();
    fx.complete(&worker, &task.id, Some("test: deliver statistics fixture"))
        .await;
    fx.wait_settled(&task.id).await;
    let candidate = fx.candidate_row(&task.id).await.unwrap();
    assert_ne!(candidate.commit_sha, candidate.base_sha);
    let projector = TrackPlannerContextProjector::new(
        fx.boot.repo.clone(),
        fx.boot.ctx.events.clone(),
        fx.boot.ctx.write.clone(),
    )
    .unwrap();
    projector.recompute_track(fx.track()).await.unwrap();
    async fn count(fx: &super::git_delivery::Fx) -> f64 {
        let stats = fx
            .boot
            .repo
            .overlays_for("track", fx.track())
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "run.stats")
            .unwrap();
        stats.payload["cell"]["table"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["dimension"] == "commits")
            .unwrap()["count"]
            .as_f64()
            .unwrap()
    }
    assert_eq!(count(&fx).await, 1.0);
    // Read another track through the same projector/pool: the candidate must stay local.
    let other = fx
        .boot
        .repo
        .track_create(calm_server::model::NewTrack {
            template_input: None,
            area_id: fx.boot.area_id.clone(),
            title: "other".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    projector.recompute_track(other.id.as_str()).await.unwrap();
    let stats = fx
        .boot
        .repo
        .overlays_for("track", other.id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.kind == "run.stats")
        .unwrap();
    assert_eq!(stats.payload["cell"]["table"]["rows"][0]["count"], 0.0);
    // No-op delivery uses a fresh lease at the delivered tip.
    fx.release_lease_by_hand(&worker.card_id).await;
    let next = fx
        .new_worker(
            "stats-no-change",
            calm_server::session_projection_repo::AgentProvider::Claude,
        )
        .await;
    let _lease = fx.kernel_lease(&next.card_id).await;
    let task = fx
        .running_task("stats-no-change", "claude", &next.card_id, json!({}))
        .await;
    fx.complete(&next, &task.id, Some("test: unchanged statistics fixture"))
        .await;
    fx.wait_settled(&task.id).await;
    let candidate = fx.candidate_row(&task.id).await.unwrap();
    assert_eq!(candidate.commit_sha, candidate.base_sha);
    projector.recompute_track(fx.track()).await.unwrap();
    assert_eq!(count(&fx).await, 1.0);
    sqlx::query("DELETE FROM events")
        .execute(&fx.pool())
        .await
        .unwrap();
    assert!(!projector.recompute_track(fx.track()).await.unwrap());
    assert_eq!(count(&fx).await, 1.0);
}
