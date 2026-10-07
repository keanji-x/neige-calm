use super::*;
use crate::operation::OperationCompletionBus;
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;
use calm_session::control::{AttachRequest, ControlErrorKind, ControlMsg, ControlReply};
use calm_session::{read_frame, write_frame};
use calm_truth::db::RepoRead;
use tokio::net::UnixStream;

enum Source {
    Explicit,
    Managed,
    Worktree,
    PrimaryCheckout,
}

async fn rejects_prepared_cwd(source: Source) {
    for file in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let explicit = root.path().join("invalid");
        let mut harness =
            terminal_worker_harness_with_workspace(root.path().to_str().unwrap()).await;
        let area_id = harness
            .repo
            .track_get(&harness.track_id)
            .await
            .unwrap()
            .unwrap()
            .area_id;
        use crate::db::sqlite::TrackWorkspacePlan;
        let plan = match source {
            Source::Managed => TrackWorkspacePlan::ManagedUnder(root.path().into()),
            Source::Worktree => TrackWorkspacePlan::AttachedWithTrackWorktree(root.path().into()),
            Source::Explicit | Source::PrimaryCheckout => TrackWorkspacePlan::AttachedFromCwd,
        };
        let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
        let track = crate::db::sqlite::track_create_tx(
            &mut tx,
            crate::model::NewTrack {
                template_input: None,
                area_id,
                title: "pty cwd source".into(),
                sort: None,
                cwd: if matches!(source, Source::PrimaryCheckout) {
                    explicit.display().to_string()
                } else {
                    root.path().display().to_string()
                },
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &plan,
            None,
            &harness.adapter.track_area_cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        harness.track_id = track.id.to_string();
        let invalid = if matches!(source, Source::Explicit) {
            explicit.display().to_string()
        } else {
            track.workspace.agent_cwd().to_string()
        };
        if file {
            std::fs::create_dir_all(std::path::Path::new(&invalid).parent().unwrap()).unwrap();
            std::fs::write(&invalid, b"not a directory").unwrap();
        }
        let invalid = invalid.as_str();
        let marker = root.path().join("executed");
        let adapter = TerminalAdapter::new(
            harness.repo.clone(),
            CardRoleCache::new(),
            TrackAreaCache::new(),
        );
        let payload = serde_json::to_value(TerminalCreateOperationPayload {
            actor: ActorId::KernelDispatcher,
            worker_session_id: None,
            planner_hooks: false,
            request: normalize_terminal_create_request(TerminalCreateRequestPayload {
                track_id: harness.track_id.clone(),
                title: None,
                sort: None,
                program: "printf executed > \"$MARKER\"".into(),
                cwd: if matches!(source, Source::Explicit) {
                    invalid.into()
                } else {
                    String::new()
                },
                env: json!({"MARKER":marker}),
                theme: RequestTheme::default_dark(),
            }),
        })
        .unwrap();
        let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
        let op_id = op_repo
            .insert_operation(
                "terminal-create",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: "pty-cwd".into(),
                },
                payload.clone(),
            )
            .await
            .unwrap();
        let op = op_repo
            .claim_drive_batch(1)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.id == op_id)
            .unwrap();
        let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
        let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(output.data["cwd"], invalid);
        let term_id = output.output_string("terminal_id", "test").unwrap();
        let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
            .await
            .unwrap();
        let mut daemon = DaemonClient::new_stub();
        daemon.proc_supervisor_sock = Some(supervisor.sock().into());
        let renderer = TerminalRendererRegistry::new_with_repo(harness.repo.clone());
        let ctx = SpawnCtx::new(
            harness.repo.clone(),
            op_repo,
            Arc::new(daemon),
            renderer.clone(),
            crate::event::EventBus::new(),
            OperationCompletionBus::new(),
        );
        let error = match adapter.spawn_side_effect(&output, &op, &ctx).await {
            Err(error) => error,
            Ok(_) => panic!("invalid prepared cwd launched a terminal"),
        };
        assert!(
            error.to_string().contains(invalid) && error.to_string().contains("not a directory"),
            "{error}"
        );
        assert!(renderer.is_empty());
        assert!(
            harness
                .repo
                .terminal_get(&term_id)
                .await
                .unwrap()
                .unwrap()
                .pid
                .is_none()
        );
        let mut stream = UnixStream::connect(supervisor.sock()).await.unwrap();
        write_frame(
            &mut stream,
            &ControlMsg::Attach(AttachRequest {
                proc_id: format!("term:{term_id}"),
                from_cursor: None,
                reader_id: "absent".into(),
            }),
        )
        .await
        .unwrap();
        let reply: ControlReply =
            tokio::time::timeout(std::time::Duration::from_secs(10), read_frame(&mut stream))
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            reply,
            ControlReply::Error {
                kind: ControlErrorKind::UnknownProc,
                ..
            }
        ));
        assert!(!marker.exists(), "rejected command executed");
    }
}

#[tokio::test]
async fn pty_cwd_explicit_prepare_failure_is_visible() {
    rejects_prepared_cwd(Source::Explicit).await;
}

#[tokio::test]
async fn pty_cwd_managed_prepare_failure_is_visible() {
    rejects_prepared_cwd(Source::Managed).await;
}

#[tokio::test]
async fn pty_cwd_track_worktree_prepare_failure_is_visible() {
    rejects_prepared_cwd(Source::Worktree).await;
}

#[tokio::test]
async fn pty_cwd_primary_checkout_prepare_failure_is_visible() {
    rejects_prepared_cwd(Source::PrimaryCheckout).await;
}
