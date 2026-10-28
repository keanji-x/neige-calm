//! Resource preparation through authenticated upload, without conversation activation.
use super::*;
use crate::per_card_lock::{lock_key, try_lock_key};
use crate::test_seams::{PausePoint, install_pause_for_test};
use calm_truth::db::RepoRead;

const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31, 0,
    5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

async fn blank(f: &Fixture, app: &axum::Router, cookie: &str) -> (crate::model::Track, String) {
    let a = u8_canonical::area(app, cookie, "upload resource").await;
    let mut request = u8_canonical::body(&a, "codex");
    request["admin"] = json!(true);
    request["title"] = json!("");
    let created = post(app, cookie, "/api/tracks", "blank", request).await;
    let id = created["id"].as_str().unwrap();
    let track = f.repo.track_get(id).await.unwrap().unwrap();
    let (card, _, _) = f.carrier(id).await;
    assert!(!std::path::Path::new(&track.workspace.path).exists());
    (track, card)
}

fn start(
    app: &axum::Router,
    cookie: &str,
    card: &str,
    body: Body,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let app = app.clone();
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/cards/{card}/planner/attachments"))
        .header(header::COOKIE, cookie)
        .header("x-calm-actor", "user")
        .header(header::CONTENT_TYPE, "image/png")
        .body(body)
        .unwrap();
    tokio::spawn(async move {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    })
}

struct Pause(PausePoint);
impl Pause {
    fn arm(point: &str, key: &str) -> Self {
        let pause = PausePoint::default();
        install_pause_for_test(point, key, pause.clone());
        Self(pause)
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.0.entered.notified())
            .await
            .unwrap();
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}

async fn unchanged(
    f: &Fixture,
    state: &AppState,
    track: &str,
    before: &(String, String, CanonicalHarnessSnapshot),
) {
    assert_eq!(&f.carrier(track).await, before);
    assert_eq!(f.counts(&before.0).await, (0, 0, 1, 0, 0));
    assert!(!state.shared_codex_appserver.is_running());
    assert!(state.harness.get(&before.1).is_none());
    let activations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations WHERE kind='planner-harness-start' AND target_id=?",
    )
    .bind(&before.0)
    .fetch_one(f.repo.pool())
    .await
    .unwrap();
    assert_eq!(activations, 0);
}

#[tokio::test]
async fn p1_workspace_upload_managed_resource_keeps_original_empty_carrier() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let before = f.carrier(track.id.as_str()).await;
    let result = start(&app, &cookie, &card, Body::from(PNG)).await.unwrap();
    assert_eq!(result.0, StatusCode::CREATED, "{result:?}");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(result.1["url"].as_str().unwrap())
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        PNG
    );
    unchanged(&f, &state, track.id.as_str(), &before).await;
    f.shutdown_process(&state).await;
}

async fn revoked_queued(membership: bool) {
    let f = Fixture::new().await;
    let (state, app, cookie, auth) = f.application_with_auth(false).await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let before = f.carrier(track.id.as_str()).await;
    let selected = Pause::arm("workspace-upload-selected", &card);
    let upload = start(&app, &cookie, &card, Body::from(PNG));
    selected.entered().await;
    if membership {
        sqlx::query("DELETE FROM area_members WHERE area_id=?")
            .bind(track.area_id.as_str())
            .execute(f.repo.pool())
            .await
            .unwrap();
    } else {
        auth.sessions
            .remove(cookie.split_once('=').unwrap().1)
            .unwrap();
    }
    drop(selected);
    let result = upload.await.unwrap();
    assert_eq!(
        result.0,
        if membership {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        },
        "{result:?}"
    );
    assert!(!std::path::Path::new(&track.workspace.path).exists());
    unchanged(&f, &state, track.id.as_str(), &before).await;
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_rechecks_queued_session() {
    revoked_queued(false).await;
}
#[tokio::test]
async fn p1_workspace_upload_rechecks_queued_membership() {
    revoked_queued(true).await;
}

#[tokio::test]
async fn p1_workspace_upload_deleted_selection_never_materializes() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let selected = Pause::arm("workspace-upload-selected", &card);
    let upload = start(&app, &cookie, &card, Body::from(PNG));
    selected.entered().await;
    let (status, _) = u8b_canonical::http(
        &app,
        &cookie,
        "DELETE",
        &format!("/api/tracks/{}", track.id),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    drop(selected);
    assert_eq!(upload.await.unwrap().0, StatusCode::NOT_FOUND);
    assert!(!std::path::Path::new(&track.workspace.path).exists());
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_changed_selection_never_materializes_old_locator() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let area = u8_canonical::area(&app, &cookie, "changed selection area").await;
    let other = workspace::materialize::managed_workspace_path(
        &f.root.path().join("workspaces"),
        area["id"].as_str().unwrap(),
        track.id.as_str(),
    );
    sqlx::query("DELETE FROM area_members WHERE area_id=?")
        .bind(area["id"].as_str().unwrap())
        .execute(f.repo.pool())
        .await
        .unwrap();
    let held = lock_key(
        state.operation_runtime.track_delete_locks(),
        track.id.as_str(),
    )
    .await;
    let selected = Pause::arm("workspace-upload-selected", &card);
    let upload = start(&app, &cookie, &card, Body::from(PNG));
    selected.entered().await;
    sqlx::query("UPDATE tracks SET area_id=?,workspace_path=? WHERE id=?")
        .bind(area["id"].as_str().unwrap())
        .bind(other.to_str().unwrap())
        .bind(track.id.as_str())
        .execute(f.repo.pool())
        .await
        .unwrap();
    drop(selected);
    drop(held);
    let result = upload.await.unwrap();
    assert!(!std::path::Path::new(&track.workspace.path).exists());
    assert!(
        !other.exists(),
        "changed selection must not materialize an unauthorized Area"
    );
    assert_eq!(result.0, StatusCode::CONFLICT, "{result:?}");
    f.shutdown_process(&state).await;
}

fn stream() -> (
    tokio::sync::mpsc::UnboundedSender<
        std::result::Result<hyper::body::Frame<axum::body::Bytes>, std::io::Error>,
    >,
    Body,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|frame| (frame, rx))
    });
    (tx, Body::new(http_body_util::StreamBody::new(stream)))
}

async fn part(track: &crate::model::Track, card: &str) -> std::path::PathBuf {
    let staging = std::path::Path::new(&track.workspace.path)
        .join(".neige/attachments")
        .join(card)
        .join("staging");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(entries) = std::fs::read_dir(&staging)
                && let Some(entry) = entries
                    .flatten()
                    .find(|e| e.file_name().to_str().unwrap().ends_with(".part"))
            {
                break entry.path();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn p1_workspace_upload_rechecks_authority_before_publish() {
    let f = Fixture::new().await;
    let (state, app, cookie, auth) = f.application_with_auth(false).await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let (frames, body) = stream();
    let upload = start(&app, &cookie, &card, body);
    frames
        .send(Ok(hyper::body::Frame::data(PNG.into())))
        .unwrap();
    let temporary = part(&track, &card).await;
    auth.sessions
        .remove(cookie.split_once('=').unwrap().1)
        .unwrap();
    drop(frames);
    assert_eq!(upload.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert!(
        std::fs::read_dir(temporary.parent().unwrap())
            .unwrap()
            .next()
            .is_none()
    );
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_cancelled_publish_keeps_track_until_last_effect() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let (frames, body) = stream();
    let upload = start(&app, &cookie, &card, body);
    frames
        .send(Ok(hyper::body::Frame::data(PNG.into())))
        .unwrap();
    let temporary = part(&track, &card).await;
    let id = temporary
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .trim_end_matches(".part");
    let publishing = Pause::arm("workspace-upload-publish", id);
    drop(frames);
    publishing.entered().await;
    upload.abort();
    assert!(upload.await.unwrap_err().is_cancelled());
    assert!(
        try_lock_key(
            state.operation_runtime.track_delete_locks(),
            track.id.as_str()
        )
        .is_none()
    );
    drop(publishing);
    let _settled = tokio::time::timeout(
        Duration::from_secs(10),
        lock_key(
            state.operation_runtime.track_delete_locks(),
            track.id.as_str(),
        ),
    )
    .await
    .unwrap();
    assert!(!temporary.exists());
    assert_eq!(std::fs::read(temporary.with_file_name(id)).unwrap(), PNG);
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_symlink_never_initializes_external_directory() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let path = std::path::Path::new(&track.workspace.path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let external = f.root.path().join("external");
    std::fs::create_dir(&external).unwrap();
    std::os::unix::fs::symlink(&external, path).unwrap();
    let result = start(&app, &cookie, &card, Body::from(PNG)).await.unwrap();
    assert_eq!(result.0, StatusCode::CONFLICT, "{result:?}");
    assert!(std::fs::read_dir(&external).unwrap().next().is_none());
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_foreign_owner_is_not_repaired_or_written() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let path = std::path::Path::new(&track.workspace.path);
    workspace::materialize::materialize_managed_workspace(
        &f.root.path().join("workspaces"),
        path,
        "another-track",
    )
    .unwrap();
    let marker = std::fs::read(path.join(".git/neige-workspace")).unwrap();
    let result = start(&app, &cookie, &card, Body::from(PNG)).await.unwrap();
    assert_eq!(result.0, StatusCode::INTERNAL_SERVER_ERROR, "{result:?}");
    assert_eq!(
        std::fs::read(path.join(".git/neige-workspace")).unwrap(),
        marker
    );
    assert!(!path.join(".neige").exists());
    assert!(!result.1.to_string().contains(path.to_str().unwrap()));
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_cancelled_stream_cleans_before_owner_releases() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let (frames, body) = stream();
    let upload = start(&app, &cookie, &card, body);
    frames
        .send(Ok(hyper::body::Frame::data(PNG.into())))
        .unwrap();
    let temporary = part(&track, &card).await;
    upload.abort();
    assert!(upload.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(10), async {
        while temporary.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let _settled = tokio::time::timeout(
        Duration::from_secs(10),
        lock_key(
            state.operation_runtime.track_delete_locks(),
            track.id.as_str(),
        ),
    )
    .await
    .unwrap();
    assert!(
        std::fs::read_dir(temporary.parent().unwrap())
            .unwrap()
            .next()
            .is_none()
    );
    drop(frames);
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_rechecks_selection_before_publish() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let area = u8_canonical::area(&app, &cookie, "changed selection area").await;
    let other = workspace::materialize::managed_workspace_path(
        &f.root.path().join("workspaces"),
        area["id"].as_str().unwrap(),
        track.id.as_str(),
    );
    sqlx::query("DELETE FROM area_members WHERE area_id=?")
        .bind(area["id"].as_str().unwrap())
        .execute(f.repo.pool())
        .await
        .unwrap();
    let (frames, body) = stream();
    let upload = start(&app, &cookie, &card, body);
    frames
        .send(Ok(hyper::body::Frame::data(PNG.into())))
        .unwrap();
    let temporary = part(&track, &card).await;
    sqlx::query("UPDATE tracks SET area_id=?,workspace_path=? WHERE id=?")
        .bind(area["id"].as_str().unwrap())
        .bind(other.to_str().unwrap())
        .bind(track.id.as_str())
        .execute(f.repo.pool())
        .await
        .unwrap();
    drop(frames);
    let result = upload.await.unwrap();
    assert!(
        std::fs::read_dir(temporary.parent().unwrap())
            .unwrap()
            .next()
            .is_none()
    );
    assert!(!other.exists());
    assert_eq!(result.0, StatusCode::CONFLICT, "{result:?}");
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_cancelled_preparation_keeps_git_under_track_owner() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let preparing = Pause::arm("workspace-managed-materializing", track.id.as_str());
    let upload = start(&app, &cookie, &card, Body::from(PNG));
    preparing.entered().await;
    upload.abort();
    assert!(upload.await.unwrap_err().is_cancelled());
    assert!(
        try_lock_key(
            state.operation_runtime.track_delete_locks(),
            track.id.as_str()
        )
        .is_none()
    );
    drop(preparing);
    let _settled = tokio::time::timeout(
        Duration::from_secs(10),
        lock_key(
            state.operation_runtime.track_delete_locks(),
            track.id.as_str(),
        ),
    )
    .await
    .unwrap();
    assert!(
        workspace::materialize::is_materialized(
            std::path::Path::new(&track.workspace.path),
            track.id.as_str()
        )
        .unwrap()
    );
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_existing_owned_repo_does_not_mint_a_commit() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let area = u8_canonical::area(&app, &cookie, "existing upload resource").await;
    // This production historical entry already materializes the owned repository.
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "existing",
        u8_canonical::body(&area, "codex"),
    )
    .await;
    let track = f
        .repo
        .track_get(created["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(track.id.as_str())
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&track.workspace.path)
            .args(args)
            .output()
            .unwrap()
    };
    let path = std::path::Path::new(&track.workspace.path);
    std::fs::write(path.join("pending.txt"), b"historical staged bytes").unwrap();
    assert!(git(&["add", "pending.txt"]).status.success());
    assert!(git(&["update-ref", "-d", "HEAD"]).status.success());
    let index = std::fs::read(path.join(".git/index")).unwrap();
    assert!(!git(&["rev-parse", "--verify", "HEAD"]).status.success());
    let result = start(&app, &cookie, &card, Body::from(PNG)).await.unwrap();
    assert_eq!(result.0, StatusCode::CREATED, "{result:?}");
    assert!(!git(&["rev-parse", "--verify", "HEAD"]).status.success());
    assert_eq!(std::fs::read(path.join(".git/index")).unwrap(), index);
    assert_eq!(
        git(&["diff", "--cached", "--name-only"]).stdout,
        b"pending.txt\n"
    );
    assert!(!state.shared_codex_appserver.is_running());
    f.shutdown_process(&state).await;
}

#[tokio::test]
async fn p1_workspace_upload_canonical_owned_repo_preserves_unborn_head_and_index() {
    let f = Fixture::new().await;
    let (state, app, cookie) = f.application().await;
    let (track, card) = blank(&f, &app, &cookie).await;
    let before = f.carrier(track.id.as_str()).await;
    let first = start(&app, &cookie, &card, Body::from(PNG)).await.unwrap();
    assert_eq!(first.0, StatusCode::CREATED, "{first:?}");
    let path = std::path::Path::new(&track.workspace.path);
    let git = |args: &[&str]| {
        workspace::command::neige_git_command()
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(git(&["rev-parse", "--verify", "HEAD"]).status.success());
    std::fs::write(path.join("pending.txt"), b"user staged bytes").unwrap();
    assert!(git(&["add", "pending.txt"]).status.success());
    assert!(git(&["update-ref", "-d", "HEAD"]).status.success());
    assert!(!git(&["rev-parse", "--verify", "HEAD"]).status.success());
    let index = std::fs::read(path.join(".git/index")).unwrap();
    let staged = git(&["diff", "--cached", "--name-only"]);
    assert!(staged.status.success());
    assert_eq!(staged.stdout, b"pending.txt\n");
    let second = start(&app, &cookie, &card, Body::from(PNG)).await.unwrap();
    assert_eq!(second.0, StatusCode::CREATED, "{second:?}");
    // Check the public resource before checking Git: successful storage must not
    // require accepting an unrelated source commit as a side effect.
    for result in [&first, &second] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(result.1["url"].as_str().unwrap())
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            PNG
        );
    }
    assert!(!git(&["rev-parse", "--verify", "HEAD"]).status.success());
    assert_eq!(std::fs::read(path.join(".git/index")).unwrap(), index);
    assert_eq!(
        git(&["diff", "--cached", "--name-only"]).stdout,
        staged.stdout
    );
    assert_eq!(
        git(&["ls-files", "--others", "--exclude-standard"]).stdout,
        b""
    );
    unchanged(&f, &state, track.id.as_str(), &before).await;
    f.shutdown_process(&state).await;
}

struct UnblockWrite(Option<std::sync::mpsc::Sender<()>>);
impl Drop for UnblockWrite {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[test]
fn p1_workspace_upload_cancelled_write_keeps_fence_until_io_completes() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            let f = Fixture::new().await;
            let (state, app, cookie) = f.application().await;
            let (track, card) = blank(&f, &app, &cookie).await;
            let (frames, body) = stream();
            let upload = start(&app, &cookie, &card, body);
            frames
                .send(Ok(hyper::body::Frame::data(PNG.into())))
                .unwrap();
            let temporary = part(&track, &card).await;
            tokio::time::timeout(Duration::from_secs(10), async {
                while std::fs::metadata(&temporary).unwrap().len() != PNG.len() as u64 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let id = temporary
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .trim_end_matches(".part");
            let queued = Pause::arm("workspace-upload-write-queued", id);
            // Occupy the sole blocking worker only after the first real write completed.
            // Tokio's next actual file write must queue behind this barrier.
            let (entered, entered_rx) = tokio::sync::oneshot::channel();
            let (release, released) = std::sync::mpsc::channel();
            let unblock = UnblockWrite(Some(release));
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                released.recv().unwrap();
            });
            entered_rx.await.unwrap();
            frames
                .send(Ok(hyper::body::Frame::data(
                    axum::body::Bytes::from_static(b"next frame"),
                )))
                .unwrap();
            queued.entered().await;
            upload.abort();
            assert!(upload.await.unwrap_err().is_cancelled());
            drop(queued);
            let probe = tokio::time::timeout(
                Duration::from_millis(500),
                lock_key(
                    state.operation_runtime.track_delete_locks(),
                    track.id.as_str(),
                ),
            )
            .await;
            assert!(
                probe.is_err(),
                "Track released while its actual file write was still queued"
            );
            drop(probe);
            drop(unblock);
            blocker.await.unwrap();
            let _settled = tokio::time::timeout(
                Duration::from_secs(10),
                lock_key(
                    state.operation_runtime.track_delete_locks(),
                    track.id.as_str(),
                ),
            )
            .await
            .unwrap();
            assert!(
                std::fs::read_dir(temporary.parent().unwrap())
                    .unwrap()
                    .next()
                    .is_none()
            );
            drop(frames);
            f.shutdown_process(&state).await;
        });
}
