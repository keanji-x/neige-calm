//! A pristine directory does not make an accepted execution replaceable.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_truth::db::RepoRead;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn files(root: &Path) -> Option<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(root: &Path, directory: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    root.exists().then(|| {
        let mut result = BTreeMap::new();
        visit(root, root, &mut result);
        result
    })
}

fn assert_files_unchanged(
    label: &str,
    before: &Option<BTreeMap<PathBuf, Vec<u8>>>,
    after: &Option<BTreeMap<PathBuf, Vec<u8>>>,
) {
    if let (Some(before), Some(after)) = (before, after) {
        for path in before
            .keys()
            .chain(after.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            assert_eq!(
                before.get(path),
                after.get(path),
                "{label} workspace file {} changed",
                path.display()
            );
        }
    } else {
        assert_eq!(before, after, "{label} workspace existence changed");
    }
}

async fn response(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    uri: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

pub(super) async fn finish_turn(
    fixture: &Fixture,
    state: &AppState,
    peer: &mut Peer,
    track: &str,
    call: Value,
    turn: &str,
) {
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/completed",
        json!({"id":turn,"status":"completed"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
}

async fn original_binding_survives_repoint(native_started: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Keep original workspace","color":"#123456"}),
    )
    .await;
    let mut native = if native_started {
        Some(connect(&state.shared_codex_appserver).await)
    } else {
        None
    };
    let creation = json!({
        "area_id":area["id"],"title":"","planner_provider":"codex",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
    });
    let created = post(&app, &cookie, "/api/tracks", "first", creation.clone()).await;
    let track = created["id"].as_str().unwrap();
    if let Some((_, peer)) = native.as_mut() {
        let call = super::native_attempt_recovery_tests::initial_request(peer).await;
        finish_turn(&fixture, &state, peer, track, call, TURN).await;
    }
    let (card, worker, original_frame) = fixture.carrier(track).await;
    let original_track = fixture.repo.track_get(track).await.unwrap().unwrap();
    let original_worker = fixture
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(worker.clone()))
        .await
        .unwrap()
        .unwrap();
    let original_receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
    let counts = fixture.counts(&card).await;
    let plans: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,render_plan_json,render_plan_sha256 FROM conversation_input_submissions WHERE card_id=?1 ORDER BY id"
    ).bind(&card).fetch_all(fixture.repo.pool()).await.unwrap();
    if !native_started {
        assert!(original_worker.thread_id.is_none());
        assert!(original_worker.spawn_op_id.is_none());
        assert!(original_frame.canonical_inputs.attempts.is_empty());
        assert_eq!(original_frame.canonical_inputs.pending.len(), 1);
        // Prepare the allocated directory through its actual resource owner.
        // This is filesystem fixture setup, never NativeReady or evidence of use.
        workspace_runtime::selection::materialize_workspace(
            &original_track.workspace,
            state.workspace_root(),
            track,
        )
        .unwrap();
    }
    let old_path = Path::new(&original_track.workspace.path);
    assert!(crate::workspace_repoint::workspace_pristine(old_path).is_pristine());
    assert!(
        original_track.workspace.frozen_at.is_none(),
        "the disk-only repoint rule currently permits this binding"
    );
    let target = fixture.root.path().join("existing-user-repository");
    std::fs::create_dir(&target).unwrap();
    for args in [
        vec!["init", "--initial-branch=main"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.com",
            // Git 2.55 detaches automatic maintenance by default. Wait for its
            // process exit too, so removing maintenance.lock cannot race the
            // snapshots in either the completed-turn or pre-native case.
            "-c",
            "maintenance.autoDetach=false",
            "commit",
            "--allow-empty",
            "-m",
            "existing repository",
        ],
    ] {
        let output = workspace::command::isolated_git_command()
            .arg("-C")
            .arg(&target)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let original_files = files(old_path);
    let target_files = files(&target);
    let (status, body) = response(
        &app,
        &cookie,
        "PATCH",
        &format!("/api/tracks/{track}"),
        "repoint",
        json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}}),
    )
    .await;
    let after_track = fixture.repo.track_get(track).await.unwrap().unwrap();
    let after_worker = fixture
        .repo
        .session_get(&original_worker.id)
        .await
        .unwrap()
        .unwrap();
    let after_receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
    let after_plans: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,render_plan_json,render_plan_sha256 FROM conversation_input_submissions WHERE card_id=?1 ORDER BY id"
    ).bind(&card).fetch_all(fixture.repo.pool()).await.unwrap();
    let old_files_after = files(old_path);
    let target_files_after = files(&target);
    let counts_after = fixture.counts(&card).await;
    let (replay_status, replay_body) =
        response(&app, &cookie, "POST", "/api/tracks", "first", creation).await;
    let (send_status, send_body) = response(
        &app,
        &cookie,
        "POST",
        &format!("/api/cards/{card}/planner/input"),
        "after-repoint",
        json!({"text":"Continue on the original workspace"}),
    )
    .await;
    if send_status.is_success()
        && let Some((_, peer)) = native.as_mut()
    {
        let call = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let call = request(peer).await;
                if call["method"] != "config/read" {
                    break call;
                }
                send(peer, json!({"id":call["id"],"result":{"config":{}}})).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            call["method"], "turn/start",
            "continuation cannot create a replacement thread"
        );
        assert_eq!(call["params"]["threadId"], THREAD);
        finish_turn(
            &fixture,
            &state,
            peer,
            track,
            call,
            "same-workspace-next-turn",
        )
        .await;
    }
    let continued = if send_status.is_success() {
        Some(fixture.carrier(track).await)
    } else {
        None
    };
    let final_counts = fixture.counts(&card).await;
    if let Some((_, mut peer)) = native {
        peer.close(None).await.unwrap();
    }
    if let Some(handle) = state.harness.remove(&worker) {
        handle.shutdown().await.unwrap();
    }
    fixture.shutdown_process(&state).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "repoint={body}; original worker {:?} -> {:?}; next send={send_status} {send_body}",
        original_worker.state,
        after_worker.state
    );
    assert!(
        body["error"].as_str().unwrap().contains("saved work"),
        "{body}"
    );
    assert_eq!(after_track.workspace, original_track.workspace);
    assert_eq!(after_track.cwd_wire_alias, original_track.cwd_wire_alias);
    assert_eq!(after_worker.state, original_worker.state);
    assert_eq!(after_worker.thread_id, original_worker.thread_id);
    assert_eq!(after_worker.active_turn_id, original_worker.active_turn_id);
    assert_eq!(after_worker.spawn_op_id, original_worker.spawn_op_id);
    assert_eq!(
        after_worker.handle_state_json,
        original_worker.handle_state_json
    );
    assert_eq!(after_receipt, original_receipt);
    assert_eq!(after_plans, plans);
    assert_eq!(counts_after, counts);
    assert_files_unchanged("original", &original_files, &old_files_after);
    assert_files_unchanged("target", &target_files, &target_files_after);
    assert!(replay_status.is_success(), "{replay_body}");
    assert_eq!(replay_body["id"], created["id"]);
    assert!(send_status.is_success(), "{send_body}");
    assert_eq!(send_body["worker_session_id"], worker);
    assert_eq!(
        final_counts.2, 1,
        "old-key replay and continuation cannot mint a replacement worker"
    );
    let (after_card, after_worker_id, continued) = continued.unwrap();
    assert_eq!((after_card, after_worker_id), (card, worker));
    if !native_started {
        assert_eq!(continued.canonical_inputs.pending.len(), 2);
        assert!(continued.canonical_inputs.attempts.is_empty());
    }
}

#[tokio::test]
async fn completed_saved_work_refuses_repoint_before_retiring_the_original_execution() {
    original_binding_survives_repoint(true).await;
}

#[tokio::test]
async fn saved_work_before_native_start_refuses_repoint_without_abandoning_its_original_input() {
    original_binding_survives_repoint(false).await;
}
