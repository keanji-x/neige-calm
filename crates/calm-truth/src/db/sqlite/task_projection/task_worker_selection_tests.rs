use super::*;
use calm_types::report_blocks::tasks::project_task_declarations;
use calm_types::track_report::ReportBlock;
use serde_json::json;

#[tokio::test]
async fn task_worker_selection_projects_updates_reads_and_freezes() {
    let (repo, track) = super::tests::setup().await;
    let block = ReportBlock {
        id: "b_0001".into(),
        kind: "task".into(),
        rev: 0,
        payload: json!({"key":"worker","kind":"agent","provider":"opencode","model":"first","reasoning_effort":"high","goal":"Work","ready":true,"declared_by":PLANNER_DECLARATION_AUTHOR,"no_gate_reason":"unit test"}),
    };
    let (mut declarations, diagnostics) = project_task_declarations(&[block]);
    assert!(diagnostics.iter().all(Vec::is_empty));
    let declaration = declarations.remove(0);
    let write_db = repo.db().clone();
    write_db
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                project_tasks_tx(
                    &mut tx,
                    &track,
                    std::slice::from_ref(&declaration),
                    &[vec![]],
                )
                .await
                .unwrap();
                let id = format!("{track}:worker");
                let mut row = super::super::task_get_tx(&mut tx, &id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(row.kind, crate::model::TaskKind::Agent);
                assert_eq!(row.provider.as_deref(), Some("opencode"));
                assert_eq!(row.model.as_deref(), Some("first"));
                assert_eq!(row.reasoning_effort.as_deref(), Some("high"));
                row.model = Some("repository-update".into());
                super::super::task_update_pending_tx(&mut tx, &row)
                    .await
                    .unwrap();
                assert_eq!(
                    super::super::task_get_tx(&mut tx, &id)
                        .await
                        .unwrap()
                        .unwrap()
                        .model,
                    row.model
                );
                let mut changed = declaration.clone();
                changed.provider = Some("grok".into());
                changed.model = Some("second".into());
                changed.reasoning_effort = Some("low".into());
                let outcome =
                    project_tasks_tx(&mut tx, &track, std::slice::from_ref(&changed), &[vec![]])
                        .await
                        .unwrap();
                assert_eq!(outcome.changed_keys, ["worker"]);
                let row = super::super::task_get_tx(&mut tx, &id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(row.provider, changed.provider);
                assert_eq!(row.model, changed.model);
                assert_eq!(row.reasoning_effort, changed.reasoning_effort);
                sqlx::query("UPDATE tasks SET status='running' WHERE id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                let verdicts =
                    evaluate_schedulability(&mut tx, &track, &[declaration], &[vec![]], false)
                        .await
                        .unwrap();
                assert!(
                    verdicts[0]
                        .diagnostics
                        .iter()
                        .any(|d| d.code == "declaration_changed_in_flight")
                );
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}
