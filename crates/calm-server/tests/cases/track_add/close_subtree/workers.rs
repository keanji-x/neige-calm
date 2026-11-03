use super::*;

#[tokio::test]
async fn close_cancels_running_serializes_worker_completion_and_rejects_late_reports() {
    for order in ["complete-first", "close-first", "concurrent"] {
        let boot = boot(16).await;
        let (track, _) = boot.user_track(order).await;
        let worker = running_worker(&boot, &track).await;
        let done = || {
            boot.registry.lookup("neige_task_done").unwrap()(
                boot.ctx.clone(),
                worker.identity.clone(),
                json!({"attempt_id":format!("{track}:work"),"result":{"completed":true},"commit_message":"fix(test): Finish worker"}),
            )
        };
        let completed = match order {
            "complete-first" => {
                let result = done().await;
                assert!(
                    result.is_ok(),
                    "completion must win before close: {result:?}"
                );
                assert_eq!(rest_close(&boot, &track).await, StatusCode::OK);
                result
            }
            "close-first" => {
                assert_eq!(rest_close(&boot, &track).await, StatusCode::OK);
                done().await
            }
            "concurrent" => {
                let (closed, result) = tokio::join!(rest_close(&boot, &track), done());
                assert_eq!(closed, StatusCode::OK);
                result
            }
            _ => unreachable!(),
        };
        let status = boot
            .repo
            .task_get(&format!("{track}:work"))
            .await
            .unwrap()
            .unwrap()
            .status;
        if completed.is_ok() {
            assert_eq!(status, calm_server::model::TaskStatus::Done, "{order}");
            assert_eq!(
                boot.count("SELECT count(*) FROM events WHERE kind='task.completed'")
                    .await,
                1
            );
        } else {
            assert_eq!(
                status,
                calm_server::model::TaskStatus::Canceled,
                "{order}: {completed:?}"
            );
            assert_eq!(
                boot.count("SELECT count(*) FROM events WHERE kind='task.completed'")
                    .await,
                0
            );
            for name in ["neige_task_done", "neige_task_fail"] {
                let args = if name == "neige_task_done" {
                    json!({"attempt_id":format!("{track}:work"),"commit_message":"fix(test): Late report"})
                } else {
                    json!({"attempt_id":format!("{track}:work"),"reason":"late"})
                };
                let error = boot.registry.lookup(name).unwrap()(
                    boot.ctx.clone(),
                    worker.identity.clone(),
                    args,
                )
                .await
                .unwrap_err();
                assert!(matches!(error.code, -32403 | -32409), "{error:?}");
            }
            assert_eq!(
                boot.repo
                    .task_get(&format!("{track}:work"))
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                calm_server::model::TaskStatus::Canceled
            );
        }
    }
}

#[tokio::test]
async fn close_refuses_uncancellable_terminal_and_unattributed_live_worker() {
    for kind in ["terminal", "orphan", "missing-thread", "missing-terminal"] {
        let boot = boot(16).await;
        let (track, _) = boot.user_track(kind).await;
        if kind == "terminal" {
            task(
                &boot,
                &track,
                "terminal",
                "running",
                TASK_IN_TRACK_ROUTE,
                None,
            )
            .await;
            boot.repo
                .db()
                .writer()
                .execute(
                    sqlx::query("UPDATE tasks SET kind='terminal' WHERE track_id=?1").bind(&track),
                )
                .await
                .unwrap();
        } else if kind == "orphan" {
            let worker = boot.worker_on(&track).await;
            boot.repo
                .db()
                .writer()
                .execute(
                    sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                        .bind(&worker.identity.session_id)
                        .bind(&worker.identity.card_id),
                )
                .await
                .unwrap();
        } else {
            let worker = running_worker(&boot, &track).await;
            boot.repo
                .db()
                .writer()
                .execute(
                    sqlx::query(
                        "UPDATE worker_sessions SET thread_id=NULL,provider=?2 WHERE id=?1",
                    )
                    .bind(&worker.identity.session_id)
                    .bind(if kind == "missing-terminal" {
                        "claude"
                    } else {
                        "codex"
                    }),
                )
                .await
                .unwrap();
            if kind == "missing-terminal" {
                boot.repo
                    .db()
                    .writer()
                    .execute(
                        sqlx::query("UPDATE tasks SET kind='claude' WHERE id=?1")
                            .bind(format!("{track}:work")),
                    )
                    .await
                    .unwrap();
            }
        }
        assert_eq!(
            rest_close(&boot, &track).await,
            StatusCode::CONFLICT,
            "{kind}"
        );
        assert!(open(&boot, &track).await);
        assert_eq!(
            boot.count("SELECT count(*) FROM track_close_cleanups")
                .await,
            0
        );
    }
}
