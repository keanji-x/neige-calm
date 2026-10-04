use super::*;

#[tokio::test]
async fn a_fresh_template_can_report_repository_mismatch_without_tasks_or_ratification() {
    let boot = boot().await;
    boot.state.plugin.reconcile_builtins().await.unwrap();
    boot.state
        .plugin
        .enable("dev.neige.git-forge")
        .await
        .unwrap();
    let (status, created) = request_json(&boot.app, "POST", "/api/tracks".into(), &boot.cookie, Some(json!({
        "planner_provider": "codex",
        "area_id": boot.area_id, "title": "Check repository", "template_id": "dev",
        "template_input": {"issue_url":"https://github.com/owner/expected/issues/1","repo":"owner/expected","issue_number":1},
        "cwd": target_cwd("-mismatch"), "attach_folder": true,
        "theme": routes::theme::RequestTheme::default_dark(),
    }))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let track = created["id"].as_str().unwrap();
    assert!(boot.repo.track_get(track).await.unwrap().unwrap().is_open());
    let (ctx, registry, identity) = planner_tool_channel(&boot, track).await;
    let read = call_planner_tool(
        &ctx,
        &registry,
        "neige.report.read",
        identity.clone(),
        json!({}),
    )
    .await
    .unwrap();
    assert!(
        read["text"]
            .as_str()
            .unwrap()
            .contains("ask the user to correct or confirm")
    );
    let card = boot
        .repo
        .cards_by_track(track)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .unwrap();
    let payload: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    let block = payload
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|block| {
            block.payload["markdown"]
                .as_str()
                .is_some_and(|text| text.starts_with("# 待你定"))
        })
        .unwrap();
    let message = "# 待你定\n\nrepo_mismatch: input.repo=owner/expected, cwd.origin=owner/observed. Please confirm the repository before any changes.\n";
    let result = crate::support::report_writes::upsert_block(
        &ctx,
        &registry,
        identity,
        json!({ "id": block.id, "kind": "prose", "payload": {"markdown": message} }),
    )
    .await
    .unwrap();
    assert_eq!(result["rev"], block.rev + 1);
    let track_row = boot.repo.track_get(track).await.unwrap().unwrap();
    assert!(track_row.is_open(), "a report edit leaves the track open");
    let card = boot
        .repo
        .cards_by_track(track)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .unwrap();
    let report: TrackReportPayload = serde_json::from_value(card.payload).unwrap();
    assert!(
        report
            .body
            .contains("input.repo=owner/expected, cwd.origin=owner/observed")
    );
    assert!(
        report
            .blocks
            .unwrap()
            .iter()
            .all(|block| block.kind != "task")
    );
}
