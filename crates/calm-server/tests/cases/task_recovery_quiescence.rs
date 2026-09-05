//! A leader exit is not proof that its descendants cannot keep writing.
use std::sync::Arc;
use std::time::Duration;

use crate::in_process_renderer_e2e::{
    process_is_alive, seed_terminal_row, spawn_proc_supervisor, wait_for_pid_file,
};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, begin_immediate_tx, card_update_tx, task_claim_pending_tx, task_mark_running_tx,
};
use calm_server::event::EventBus;
use calm_server::ids::ActorId;
use calm_server::model::{CardPatch, NewCard, TaskStatus, TrackLifecycle, TrackPatch};
use calm_server::state::WriteContext;
use calm_server::task_context::TaskContextMonitor;
use calm_server::task_recovery::{RecoveryContext, recover_failed_task, task_recovery_view};
use calm_server::terminal_renderer::{RendererConfig, TerminalRendererRegistry};
use calm_server::track_report::{TrackReportPayload, persist_report, resolve_report_for_track};
use calm_types::task_recovery::TaskRecoveryRequest;
use serde_json::json;

struct StopOwnedWriter(std::path::PathBuf);
impl Drop for StopOwnedWriter {
    fn drop(&mut self) {
        let _ = std::fs::write(self.0.join("stop"), "stop");
    }
}

#[tokio::test]
async fn task_recovery_refuses_live_descendant_after_observed_terminal_exit() {
    let mut outcomes = Vec::new();
    for signalled in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let _stop = StopOwnedWriter(temp.path().to_path_buf());
        // The child only touches this fixture directory. Directory removal or
        // the stop marker ends it; a finite loop also bounds unexpected aborts.
        std::fs::write(
            temp.path().join("descendant.sh"),
            r#"
echo $$ > "$1/descendant.pid"
i=0
while [ -d "$1" ] && [ ! -e "$1/stop" ] && [ "$i" -lt 1500 ]; do
    if [ -e "$1/probe" ]; then printf alive > "$1/wrote-after-exit"; fi
    i=$((i+1))
    sleep 0.02
done
"#,
        )
        .unwrap();
        let ending = if signalled { "sleep 30" } else { "exit 1" };
        let command = format!(
            "task_root=$(pwd -P); trap '' TERM; setsid /bin/sh ./descendant.sh \"$task_root\" & while [ ! -s ./descendant.pid ]; do sleep 0.02; done; {ending}"
        );
        let control_sock = temp.path().join("supervisor.sock");
        let mut supervisor = spawn_proc_supervisor(&control_sock).await;
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let terminal = seed_terminal_row(&repo).await;
        let card = repo
            .card_get(terminal.card_id.as_str())
            .await
            .unwrap()
            .unwrap();
        repo.track_update(
            card.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Working),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let events = EventBus::new();
        let roles = calm_server::card_role_cache::CardRoleCache::new();
        let areas = calm_server::track_area_cache::TrackAreaCache::new();
        let write = WriteContext::new(roles.clone(), areas.clone());
        let declaration = json!({
            "key":"predecessor", "kind":"terminal", "command":command,
            "cwd":temp.path(), "ready":true, "declared_by":"user"
        });
        repo.card_create(NewCard {
            track_id: card.track_id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
        let pool = repo.sqlite_pool().unwrap();
        repo.seed_card_role_cache(&roles).await.unwrap();
        repo.seed_track_area_cache(&areas).await.unwrap();
        let (track, report_card, previous_report) =
            resolve_report_for_track(repo.as_ref(), card.track_id.as_str())
                .await
                .unwrap();
        let next = TrackReportPayload::new(
            "",
            calm_types::report_blocks::render_fence("task", &declaration),
        );
        let revision = previous_report.doc_rev;
        persist_report(
            repo.as_ref(),
            &events,
            &write,
            ActorId::User,
            calm_server::event::EditAuthor::User,
            track,
            report_card,
            previous_report,
            next,
            revision,
            None,
            None,
            false,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE terminals SET program=?1,cwd=?2 WHERE id=?3")
            .bind(&command)
            .bind(temp.path().display().to_string())
            .bind(&terminal.id)
            .execute(&pool)
            .await
            .unwrap();
        let task = repo
            .task_current_get(card.track_id.as_str(), "predecessor")
            .await
            .unwrap()
            .unwrap();
        let monitor = TaskContextMonitor::new(repo.clone(), events.clone(), write.clone());
        let closure = monitor
            .resolve_task_closure(card.track_id.as_str(), &task.key)
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        task_claim_pending_tx(&mut tx, &task.id, 10, &closure.refs, false)
            .await
            .unwrap();
        task_mark_running_tx(&mut tx, &task.id, Some(card.id.as_str()), 11, i64::MAX)
            .await
            .unwrap();
        card_update_tx(
            &mut tx,
            card.id.as_str(),
            CardPatch {
                payload: Some(json!({"idempotency_key":task.id})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
        let registry = TerminalRendererRegistry::new_with_repo(route_repo);
        registry.set_task_hook(calm_server::scheduler::TerminalTaskHook::new(
            repo.clone(),
            events.clone(),
            write.clone(),
        ));
        let _entry = registry
            .ensure(RendererConfig {
                terminal_id: terminal.id.clone(),
                cols: 80,
                rows: 24,
                buffer_bytes: 1 << 20,
                terminal_fg: (255, 255, 255),
                terminal_bg: (0, 0, 0),
                program: "/bin/sh".into(),
                args: vec!["-c".into(), command],
                envs: vec![("PATH".into(), "/usr/bin:/bin".into())],
                cwd: temp.path().display().to_string(),
                supervisor_sock: control_sock,
            })
            .await
            .unwrap();
        let child_pid = tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_pid_file(&temp.path().join("descendant.pid")),
        )
        .await
        .unwrap();
        if signalled {
            registry.drop_entry(&terminal.id).await;
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if repo.task_get(&task.id).await.unwrap().unwrap().status == TaskStatus::Failed {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("real terminal hook must persist failure");
        let exited = repo.terminal_get(&terminal.id).await.unwrap().unwrap();
        assert_eq!(exited.signal_killed, signalled);
        if !signalled {
            assert_eq!(exited.exit_code, Some(1));
        }
        assert!(process_is_alive(child_pid));
        std::fs::write(temp.path().join("probe"), "write now").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !temp.path().join("wrote-after-exit").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("owned descendant writes AFTER real exit persistence");
        let result = recover_failed_task(
            RecoveryContext {
                repo: repo.as_ref(),
                events: &events,
                write: &write,
            },
            card.track_id.as_str(),
            &task.key,
            TaskRecoveryRequest {
                expected_attempt_id: task.id.clone(),
                idempotency_key: "recover-after-leader-exit".into(),
                reason: "Recover task".into(),
            },
            ActorId::User,
        )
        .await;
        let capability = task_recovery_view(
            repo.as_ref(),
            card.track_id.as_str(),
            &task.key,
            ActorId::User,
        )
        .await
        .unwrap()
        .recovery;
        outcomes.push((signalled, result, capability));
        std::fs::write(temp.path().join("stop"), "stop").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while process_is_alive(child_pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("owned descendant stopped");
        registry.drop_entry(&terminal.id).await;
        let _ = supervisor.kill().await;
        let _ = supervisor.wait().await;
    }
    for (signalled, result, capability) in outcomes {
        let error = result.expect_err(
            "leader exit must not authorize recovery while detached descendants can write",
        );
        assert!(
            matches!(error, calm_server::error::CalmError::Conflict(ref message) if message.contains("descendant write fence")),
            "signalled={signalled}: {error}"
        );
        assert!(!capability.allowed);
        assert_eq!(capability.code, "predecessor_not_quiescent");
    }
}
