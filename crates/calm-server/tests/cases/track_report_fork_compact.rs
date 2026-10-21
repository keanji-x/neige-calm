use super::*;

#[tokio::test]
async fn fork_elides_compact_tasks_and_refuses_user_activation() {
    let boot = boot().await;
    let (track, card, current) = calm_server::track_report::resolve_report_for_track(
        boot.repo.as_ref(),
        &boot.source_track_id,
    )
    .await
    .unwrap();
    let compact = json!({"key":"compact", "kind":"compact", "ready":false, "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR});
    let next = TrackReportPayload::new(
        "compact source",
        format!("{}{}", current.body, render_fence("task", &compact)),
    );
    persist_report(
        boot.repo.as_ref(),
        &boot.state.events,
        boot.state.write(),
        ActorId::KernelDispatcher,
        EditAuthor::Planner,
        track,
        card,
        current.clone(),
        next,
        current.doc_rev,
        None,
    )
    .await
    .unwrap();
    let (_, source) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{}/report", boot.source_track_id),
        &boot.cookie,
        None,
    )
    .await;
    let source_block = task_block_by_key(&source, "compact");
    let (status, target) = request_json(&boot.app, "POST", "/api/tracks".into(), &boot.cookie,
        Some(json!({"planner_provider":"codex", "area_id":boot.area_id, "title":"compact fork",
            "cwd":target_cwd("-compact"), "attach_folder":true,
            "theme":routes::theme::RequestTheme::default_dark(), "fork_report_from":boot.source_track_id}))).await;
    assert_eq!(status, StatusCode::CREATED, "{target}");
    let target_id = target["id"].as_str().unwrap();
    let (_, report) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    // Exercise the exploit even on the unfixed tree, where the copied id is still present.
    let copied = report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == source_block["id"]);
    let mut activation = compact.clone();
    activation["ready"] = json!(true);
    activation["released_by_user"] = json!(true);
    let (method, path, input) = if let Some(block) = copied {
        (
            "PATCH",
            format!(
                "/api/tracks/{target_id}/report/blocks/{}",
                block["id"].as_str().unwrap()
            ),
            json!({"kind":"task", "payload":activation, "ifBlockRev":block["rev"]}),
        )
    } else {
        (
            "POST",
            format!("/api/tracks/{target_id}/report/blocks"),
            json!({"kind":"task", "payload":activation, "ifDocRev":report["docRev"]}),
        )
    };
    let (status, response) = request_json(&boot.app, method, path, &boot.cookie, Some(input)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "user activation: {response}");
    assert!(
        report["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["kind"] != "task" || b["payload"]["kind"] != "compact")
    );
    assert_eq!(
        task_block_by_key(&report, "build")["payload"]["ready"],
        false
    );
    let (_, after) = request_json(
        &boot.app,
        "GET",
        format!("/api/tracks/{target_id}/report"),
        &boot.cookie,
        None,
    )
    .await;
    assert_eq!(
        after, report,
        "refused activation must not change the report"
    );
}
