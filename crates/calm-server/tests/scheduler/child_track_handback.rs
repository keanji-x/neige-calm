use super::*;

#[tokio::test]
async fn child_track_met_result_uses_kernel_report_and_latest_candidate() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    let (_task, child) = seed_child_parent(&boot, "met-result", true, None).await;
    let report = boot
        .repo
        .cards_by_track(&child)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind == "track-report")
        .unwrap();
    let payload = calm_server::track_report::TrackReportPayload::initial();
    let mut doc = calm_server::track_report_doc::ReportDoc::from_payload(&payload);
    doc.increment_doc_rev().unwrap();
    doc.increment_doc_rev().unwrap();
    sqlx::query("UPDATE cards SET body_crdt=?1 WHERE id=?2")
        .bind(doc.to_bytes())
        .bind(report.id.as_str())
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    let expected = crate::support::child_track::seed_candidates(
        boot.repo.as_ref(),
        &child,
        report.id.as_str(),
    )
    .await;
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    let result = &event_rows(&boot, "task.completed").await[0].1["result"];
    assert_eq!(
        result,
        &json!({"source":"child-track","outcome":"met","child_track_id":child,
        "report":{"card_id":report.id,"rev":2},"candidate":expected})
    );
}
