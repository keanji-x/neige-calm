//! #1505 S6 PR1 — the upload and read-back endpoints, driven through the real
//! router with a real managed workspace on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, TrackWorkspacePlan, card_create_with_id_tx, track_create_tx,
};
use calm_server::event::EventBus;
use calm_server::model::{Card, CardRole, NewArea, NewCard, NewTrack, TrackWorkspaceKind, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

struct Boot {
    app: axum::Router,
    planner_card: Card,
    worker_card: Card,
    workspace: PathBuf,
    _tmp: TempDir,
}

/// One managed workspace with a real git repository in it, one planner card and
/// one worker card. `kind` picks whether the track's workspace is `Managed`
/// (the supported shape) or `Attached`.
async fn boot_with_kind(kind: TrackWorkspaceKind) -> Boot {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path().join("workspaces");
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "attachments".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    // `ManagedUnder` is the plan `POST /api/tracks` takes when no `cwd` is
    // named, and it is the only shape attachments support. The workspace is
    // frozen by the writer, so it has to be decided at creation rather than
    // patched afterwards.
    let plan = match kind {
        TrackWorkspaceKind::Managed => TrackWorkspacePlan::ManagedUnder(workspace_root.clone()),
        TrackWorkspaceKind::Attached => TrackWorkspacePlan::AttachedFromCwd,
    };
    let track_area_cache = TrackAreaCache::new();
    let mut tx = repo.pool().begin().await.unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "attachments".into(),
            sort: None,
            cwd: workspace_root
                .join(area.id.as_str())
                .join("attached")
                .to_string_lossy()
                .into_owned(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        },
        None,
        &plan,
        None,
        &track_area_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let workspace = track.workspace.clone();
    let workspace_path = PathBuf::from(&workspace.path);
    // The production materialization path, so the `.git/info/exclude` entry
    // under test is the one it writes. A no-op for an attached workspace, which
    // neige never creates.
    calm_server::workspace_materialize::materialize_workspace(
        &workspace,
        &workspace_root,
        track.id.as_str(),
    )
    .unwrap();

    let role_cache = CardRoleCache::new();
    let mut tx = repo.pool().begin().await.unwrap();
    let planner_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    let worker_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Worker,
        true,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(track_area_cache),
    )
    .with_workspace_root(workspace_root);
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        planner_card,
        worker_card,
        workspace: workspace_path,
        _tmp: tmp,
    }
}

async fn boot() -> Boot {
    boot_with_kind(TrackWorkspaceKind::Managed).await
}

impl Boot {
    fn attachments_dir(&self) -> PathBuf {
        self.workspace.join(".neige").join("attachments")
    }

    fn staging(&self) -> PathBuf {
        self.attachments_dir()
            .join(self.planner_card.id.as_str())
            .join("staging")
    }

    fn bound(&self) -> PathBuf {
        self.attachments_dir()
            .join(self.planner_card.id.as_str())
            .join("bound")
    }
}

async fn upload(
    app: &axum::Router,
    card_id: &str,
    actor: Option<&str>,
    content_type: &str,
    bytes: Vec<u8>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/cards/{card_id}/planner/attachments"))
        .header(header::CONTENT_TYPE, content_type);
    if let Some(actor) = actor {
        builder = builder.header("X-Calm-Actor", actor);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

async fn read_back(
    app: &axum::Router,
    card_id: &str,
    attachment_id: &str,
) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/cards/{card_id}/planner/attachments/{attachment_id}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body.to_vec())
}

fn header_of(headers: &[(String, String)], name: &str) -> String {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("missing header {name} in {headers:?}"))
}

fn png(payload: &[u8]) -> Vec<u8> {
    let mut bytes = PNG_MAGIC.to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

fn file_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

fn git_status(repo: &Path) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(output.status.success(), "git status failed in {repo:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The declared `Content-Type` is not consulted at all: the extension the file
/// lands under, and the type the read-back sends, both come from the magic
/// number.
#[tokio::test]
async fn the_magic_number_decides_the_extension_not_the_declared_content_type() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (status, body) = upload(
        &b.app,
        &card,
        Some("user"),
        "image/jpeg",
        png(b"declared as jpeg"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["attachmentId"].as_str().unwrap().to_string();
    assert!(
        id.ends_with(".png"),
        "id must carry the sniffed extension: {id}"
    );
    assert_eq!(body["contentType"], "image/png", "{body}");
    assert_eq!(
        body["url"],
        format!("/api/cards/{card}/planner/attachments/{id}"),
        "the server builds the read-back path, the client never composes one"
    );
    assert_eq!(body["size"], 8 + 16, "{body}");

    let (status, headers, bytes) = read_back(&b.app, &card, &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_of(&headers, "content-type"), "image/png");
    assert_eq!(bytes, png(b"declared as jpeg"));
}

/// SVG is an executable document; it is refused on this write path whatever it
/// claims to be. `readfile-raw`'s own extension table still serves
/// `image/svg+xml` — the narrowing here is not a change to that reader.
#[tokio::test]
async fn svg_is_refused_by_the_upload_endpoint() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (status, body) = upload(
        &b.app,
        &card,
        Some("user"),
        "image/svg+xml",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script/></svg>".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("SVG is refused"),
        "{body}"
    );
    assert!(file_names(&b.staging()).is_empty(), "no bytes may land");
}

/// SVG markup behind a PNG magic number is stored and served as `image/png`,
/// with `nosniff` and a sandbox CSP, so no browser is invited to run it as
/// markup. codex will fail to decode it and silently substitute placeholder
/// text; that is the accepted, declared gap.
#[tokio::test]
async fn svg_disguised_with_a_png_magic_number_is_served_as_inert_png() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (status, body) = upload(
        &b.app,
        &card,
        Some("user"),
        "image/png",
        png(b"<svg onload=\"alert(1)\"/>"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["attachmentId"].as_str().unwrap().to_string();
    assert!(id.ends_with(".png"), "{id}");

    let (status, headers, _) = read_back(&b.app, &card, &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_of(&headers, "content-type"), "image/png");
    assert_eq!(header_of(&headers, "x-content-type-options"), "nosniff");
    assert_eq!(header_of(&headers, "content-security-policy"), "sandbox");
    assert_eq!(header_of(&headers, "cache-control"), "no-store");
}

/// One size gate. 7 MiB is stored, 9 MiB is refused with 413 — there is no
/// second, lower threshold that would make this status unreachable.
#[tokio::test]
async fn a_single_size_gate_stores_seven_mib_and_refuses_nine() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    let (status, body) = upload(
        &b.app,
        &card,
        Some("user"),
        "image/png",
        png(&vec![0u8; 7 * 1024 * 1024]),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "7 MiB must be stored: {body}");

    let (status, body) = upload(
        &b.app,
        &card,
        Some("user"),
        "image/png",
        png(&vec![0u8; 9 * 1024 * 1024]),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["code"], "payload_too_large", "{body}");
    assert_eq!(
        file_names(&b.staging()).len(),
        1,
        "the refused body must leave nothing behind: {:?}",
        file_names(&b.staging())
    );
}

/// The per-card budget, in all of its shapes: refused before the body is read,
/// refused mid-stream, refused when the directory cannot be enumerated at all —
/// and *not* refused because something planted an entry this store did not
/// write.
#[tokio::test]
async fn the_per_card_budget_refuses_rather_than_reclaiming() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    // Within budget.
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"small")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 63 MiB already bound. A sparse file: `metadata().len()` is the measure,
    // and writing 63 MiB of real bytes would only slow the test down.
    std::fs::create_dir_all(b.bound()).unwrap();
    let big = std::fs::File::create(b.bound().join("bulk.png")).unwrap();
    big.set_len(63 * 1024 * 1024).unwrap();
    drop(big);

    // 2 MiB still starts (63 < 64) and is cut off mid-stream.
    let before = file_names(&b.staging());
    let (status, body) = upload(
        &b.app,
        &card,
        Some("user"),
        "image/png",
        png(&vec![0u8; 2 * 1024 * 1024]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("attachment budget exhausted"),
        "{body}"
    );
    assert_eq!(
        file_names(&b.staging()),
        before,
        "an aborted upload must leave neither the file nor its `.part`"
    );

    // Over budget outright: refused before a byte is read.
    let over = std::fs::File::create(b.bound().join("bulk2.png")).unwrap();
    over.set_len(2 * 1024 * 1024).unwrap();
    drop(over);
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"tiny")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("attachment budget exhausted"),
        "{body}"
    );

    // #1515 review F2. A dangling symlink is something an agent can plant in
    // this subtree, and it is not one of ours: uncounted, and — the part that
    // used to be a latch — not a permanent refusal of every later upload.
    std::fs::remove_file(b.bound().join("bulk.png")).unwrap();
    std::fs::remove_file(b.bound().join("bulk2.png")).unwrap();
    std::os::unix::fs::symlink(b.bound().join("gone"), b.bound().join("dangling.png")).unwrap();
    for attempt in 0..2 {
        let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"tiny")).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "attempt {attempt}: one planted entry must not disable this card's uploads: {body}"
        );
    }

    // Fail-closed is kept where it belongs: a subtree the filesystem will not
    // enumerate at all refuses the upload rather than admitting it. `bound/`
    // replaced by a regular file makes `read_dir` `ENOTDIR`.
    std::fs::remove_dir_all(b.bound()).unwrap();
    std::fs::write(b.bound(), b"not a directory").unwrap();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"tiny")).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "an unmeasurable budget must refuse, not admit: {body}"
    );
}

/// Attached workspaces belong to the user. The refusal is what keeps this slice
/// free of any code that would have to delete a directory under someone's home.
#[tokio::test]
async fn an_attached_workspace_is_refused_and_nothing_is_created() {
    let b = boot_with_kind(TrackWorkspaceKind::Attached).await;
    let card = b.planner_card.id.to_string();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("managed workspace"),
        "{body}"
    );
    assert!(
        !b.workspace.join(".neige").exists(),
        "the refusal must not have created a bypass directory: {:?}",
        b.workspace
    );
}

/// The exclude entry is a precondition for writing, not a courtesy afterwards:
/// a file the agent's `git add -A` could pick up must never exist, so a failure
/// to exclude means no bytes.
#[tokio::test]
async fn a_failed_git_exclude_refuses_the_upload_before_any_byte_lands() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let exclude = b.workspace.join(".git").join("info").join("exclude");
    std::fs::remove_file(&exclude).unwrap();
    // A directory where the exclude file belongs: readable as a path, and
    // impossible to read or append as a file.
    std::fs::create_dir(&exclude).unwrap();

    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        file_names(&b.staging()).is_empty(),
        "no byte may land when the exclusion could not be established: {:?}",
        file_names(&b.staging())
    );
}

/// `.neige/` is hidden through `.git/info/exclude`; writing a `.gitignore`
/// would be a tracked file in a repository a worker may commit. The entry is
/// appended at most once however many uploads run.
#[tokio::test]
async fn neige_is_excluded_through_git_info_exclude_and_only_once() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    for _ in 0..3 {
        let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let status = git_status(&b.workspace);
    assert!(
        !status.contains(".neige"),
        "the attachment subtree must be invisible to git: {status:?}"
    );
    assert!(
        !b.workspace.join(".gitignore").exists(),
        "a .gitignore on a managed workspace is illegal"
    );
    let exclude =
        std::fs::read_to_string(b.workspace.join(".git").join("info").join("exclude")).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".neige/")
            .count(),
        1,
        "the entry must be idempotent across uploads and materialization: {exclude:?}"
    );
}

/// Uploading is the human's channel: it puts new content into a directory an
/// agent reads. The refusal names this endpoint, which is what proves the
/// judgement was parameterized rather than the track-report text reused.
#[tokio::test]
async fn an_agent_actor_cannot_upload() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (status, body) = upload(&b.app, &card, Some("ai:codex"), "image/png", png(b"x")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("planner attachment upload"), "{message}");
    assert!(message.contains("ai:codex"), "{message}");
    assert!(
        message.contains("has no upload channel"),
        "the refusal must say why this endpoint in particular is closed: {message}"
    );
    assert!(
        !message.contains("calm.report"),
        "the track-report redirect is the wrong advice here: {message}"
    );
    assert!(file_names(&b.staging()).is_empty());
}

/// Reading is admitted for any actor, exactly like `GET /harness/items`: the
/// agent's working directory is this workspace, so a guard here would only
/// mislead.
#[tokio::test]
async fn reading_back_is_not_restricted_to_the_human_actor() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (_, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    let id = body["attachmentId"].as_str().unwrap().to_string();

    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/cards/{card}/planner/attachments/{id}"))
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// The read-back's card admission is the same predicate the transcript endpoint
/// uses; a worker card is refused by both.
#[tokio::test]
async fn a_non_planner_card_has_no_attachment_surface() {
    let b = boot().await;
    let worker = b.worker_card.id.to_string();
    let (status, body) = upload(&b.app, &worker, Some("user"), "image/png", png(b"x")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _, _) =
        read_back(&b.app, &worker, "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A malformed id never reaches a path join, and a well-formed id belonging to
/// no file on this card resolves to nothing. Both are 400.
#[tokio::test]
async fn only_an_id_this_card_actually_owns_reads_back() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (_, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    let id = body["attachmentId"].as_str().unwrap().to_string();
    let (status, _, _) = read_back(&b.app, &card, &id).await;
    assert_eq!(status, StatusCode::OK);

    for forged in [
        "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png",
        "not-a-uuid.png",
        "..%2F..%2Fetc%2Fpasswd",
    ] {
        let (status, _, _) = read_back(&b.app, &card, forged).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "forged id {forged} must not read"
        );
    }
}

/// A successful upload leaves the file under its final name and no `.part`
/// beside it: the id is returned only after the rename, so a client can never
/// reference a name that is not yet durable.
#[tokio::test]
async fn a_stored_attachment_leaves_no_part_file() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (_, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    let id = body["attachmentId"].as_str().unwrap().to_string();
    assert_eq!(file_names(&b.staging()), vec![id]);
}

// ---------------------------------------------------------------------------
// #1515 review — multi-frame bodies.
//
// Every test above sends `Body::from(Vec<u8>)`, which is exactly one data
// frame, so the outcome is always decided before `open` becomes `Some`: the
// mid-stream failure arms, the in-flight `.part`, and the GC race against it
// are all unreachable from a single frame. These drive a channel-backed body
// instead, so the upload can be held open at a chosen point.
// ---------------------------------------------------------------------------

/// One POST whose body is fed frame by frame from the test.
struct StreamingUpload {
    frames: futures::channel::mpsc::UnboundedSender<Result<axum::body::Bytes, std::io::Error>>,
    response: tokio::task::JoinHandle<(StatusCode, Value)>,
}

impl StreamingUpload {
    fn start(app: &axum::Router, card_id: &str) -> Self {
        let (frames, body) = futures::channel::mpsc::unbounded();
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/cards/{card_id}/planner/attachments"))
            .header(header::CONTENT_TYPE, "image/png")
            .header("X-Calm-Actor", "user")
            .body(Body::from_stream(body))
            .unwrap();
        let app = app.clone();
        let response = tokio::spawn(async move {
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        });
        StreamingUpload { frames, response }
    }

    fn send(&self, bytes: Vec<u8>) {
        self.frames
            .unbounded_send(Ok(axum::body::Bytes::from(bytes)))
            .expect("the upload task must still be reading its body");
    }

    async fn finish(self) -> (StatusCode, Value) {
        drop(self.frames);
        self.response.await.unwrap()
    }
}

/// Poll until `staging/` holds a `.part`, i.e. the streaming upload has passed
/// the budget measurement and created its temporary file.
async fn wait_for_part(staging: &Path) -> String {
    for _ in 0..600 {
        if let Some(name) = file_names(staging)
            .into_iter()
            .find(|name| name.ends_with(".part"))
        {
            return name;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!(
        "no `.part` appeared in {staging:?} within 6s: {:?}",
        file_names(staging)
    );
}

fn sparse_file(path: &Path, len: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    file.set_len(len).unwrap();
}

fn bytes_on_disk(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .map(|entry| std::fs::symlink_metadata(entry.unwrap().path()).unwrap())
        .filter(|meta| meta.file_type().is_file())
        .map(|meta| meta.len())
        .sum()
}

const MIB: u64 = 1024 * 1024;

/// #1515 review F1. The budget was a read-then-write: `used_bytes` is taken
/// before the body streams and the same stale number gates every frame, so two
/// uploads that overlap both measure the same "before" and both fit. Here the
/// card has exactly 1 MiB left; A takes it while B is still queued, and B must
/// be refused rather than spending it a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_overlapping_uploads_cannot_both_spend_the_last_megabyte() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    // 63 MiB bound, so exactly 1 MiB of the 64 MiB budget is left.
    sparse_file(&b.bound().join("bulk.png"), 63 * MIB);

    let a = StreamingUpload::start(&b.app, &card);
    a.send(png(&[0u8; 4])); // 12 bytes: enough to sniff and open the `.part`
    wait_for_part(&b.staging()).await;

    // B is a complete 1 MiB upload, started while A is still streaming. It
    // must not observe A's pre-upload budget.
    let app = b.app.clone();
    let card_for_b = card.clone();
    let mut second = tokio::spawn(async move {
        upload(
            &app,
            &card_for_b,
            Some("user"),
            "image/png",
            png(&vec![0u8; MIB as usize - 8]),
        )
        .await
    });
    let still_queued =
        tokio::time::timeout(std::time::Duration::from_millis(400), &mut second).await;
    assert!(
        still_queued.is_err(),
        "B must wait for A's turn on this card; it answered {still_queued:?}"
    );

    // A completes at exactly 1 MiB, filling the budget to the brim.
    a.send(vec![0u8; MIB as usize - 12]);
    let (status, body) = a.finish().await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "A takes the last megabyte: {body}"
    );

    let (status, body) = second.await.unwrap();
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "B must be refused: A already spent the last megabyte. {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("attachment budget exhausted"),
        "{body}"
    );
    assert!(
        bytes_on_disk(&b.staging()) + bytes_on_disk(&b.bound()) <= 64 * MIB,
        "the budget must actually bound the subtree: staging {} + bound {}",
        bytes_on_disk(&b.staging()),
        bytes_on_disk(&b.bound())
    );
}

/// #1515 review F5. A stalled upload's `.part` stops advancing its mtime, so it
/// ages; the sweep another upload runs at the end of its own turn would then
/// unlink a file still held open, and the first upload's rename would fail on
/// a name that no longer exists — surfacing as a 500. The card's turn is what
/// keeps the two apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_uploads_part_is_not_reaped_by_the_next_upload() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    let a = StreamingUpload::start(&b.app, &card);
    a.send(png(&[0u8; 4]));
    let part = wait_for_part(&b.staging()).await;
    // A stalls here. Its `.part` is now a day old by mtime — exactly what a
    // browser tab left open overnight produces.
    let aged = std::fs::File::options()
        .write(true)
        .open(b.staging().join(&part))
        .unwrap();
    aged.set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60))
        .unwrap();
    drop(aged);

    let app = b.app.clone();
    let card_for_b = card.clone();
    let second = tokio::spawn(async move {
        upload(&app, &card_for_b, Some("user"), "image/png", png(b"second")).await
    });
    // Give an unserialized second upload every chance to run its sweep.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let (status, body) = a.finish().await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the in-flight upload must not be reaped out from under itself: {body}"
    );
    assert!(
        !body.to_string().contains(".neige"),
        "no internal path may reach the client: {body}"
    );

    let (status, body) = second.await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// #1515 review F9. A failure that happens *after* the `.part` exists is only
/// reachable from a body with more than one frame. Frame 1 opens the file,
/// frame 2 crosses the budget; the single cleanup arm in `write_body` must
/// remove what frame 1 created.
#[tokio::test]
async fn a_failure_after_the_part_exists_removes_it() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    // 64 KiB of headroom: the first frame fits, the second cannot.
    sparse_file(&b.bound().join("bulk.png"), 64 * MIB - 64 * 1024);

    let a = StreamingUpload::start(&b.app, &card);
    a.send(png(&[0u8; 4]));
    let part = wait_for_part(&b.staging()).await;
    a.send(vec![0u8; 128 * 1024]);
    let (status, body) = a.finish().await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("attachment budget exhausted"),
        "{body}"
    );
    assert!(
        file_names(&b.staging()).is_empty(),
        "the `.part` {part} opened by the first frame must be gone: {:?}",
        file_names(&b.staging())
    );
}

/// #1515 review F9. The sweep runs on the real upload path — no test drove it
/// there before, only `sweep_staging_at` directly. An unbound upload older than
/// the orphan TTL is reclaimed by the next upload on the same card, and its URL
/// then answers 400. That is also the S6-PR1 boundary the response type
/// declares: nothing binds yet, so every upload eventually expires.
#[tokio::test]
async fn the_next_upload_reclaims_an_expired_unbound_one() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"stale")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let stale = body["attachmentId"].as_str().unwrap().to_string();
    let file = std::fs::File::options()
        .write(true)
        .open(b.staging().join(&stale))
        .unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60))
        .unwrap();
    drop(file);

    let (status, _, _) = read_back(&b.app, &card, &stale).await;
    assert_eq!(status, StatusCode::OK, "still readable before the sweep");

    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"fresh")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let fresh = body["attachmentId"].as_str().unwrap().to_string();

    assert_eq!(
        file_names(&b.staging()),
        vec![fresh],
        "the expired upload must have been swept by the fresh one's turn"
    );
    let (status, _, _) = read_back(&b.app, &card, &stale).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an expired attachment's URL stops resolving; binding is what takes one out of the \
         sweep's reach, and this one was never bound"
    );
}

/// BLOCKER 1 of the S6 delta review, and it was introduced by the round that
/// moved the sweep to the front of every upload.
///
/// `sweep_staging` walked `<root>/<card>/staging` with `std::fs::read_dir` and
/// unlinked through `std::fs::remove_file` — neither under any `RESOLVE_*`
/// flag. A **relative** link (`ln -s ../card-b/bound <root>/card-a/staging`;
/// `RESOLVE_BENEATH` would not have caught it either, since the target is
/// beneath the root) made every POST to card A walk into card B's `bound/` and
/// unlink everything older than the orphan TTL — taking card B's
/// already-issued `localImage` paths with it, which codex answers with
/// placeholder text and no error.
///
/// The reachability is the second half of why this mattered: the sweep used to
/// run only after a SUCCESSFUL upload, so reaching it meant getting past the
/// magic-number sniff, the size gate and the budget. Moved to the front, every
/// request reached it — including the ones about to be refused.
#[tokio::test]
async fn an_uploads_sweep_cannot_be_pointed_at_another_cards_bound_directory() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    // Card B's bound directory, with an attachment old enough to sweep.
    let victim = b.attachments_dir().join("card-victim").join("bound");
    std::fs::create_dir_all(&victim).unwrap();
    let precious = victim.join("0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png");
    std::fs::write(&precious, b"card B's bound bytes").unwrap();
    let aged = std::fs::File::options()
        .write(true)
        .open(&precious)
        .unwrap();
    aged.set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60))
        .unwrap();
    drop(aged);

    // Card A's staging, replaced by a RELATIVE link into it.
    let staging = b.staging();
    std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&staging);
    std::os::unix::fs::symlink("../card-victim/bound", &staging).unwrap();

    // Any upload at all — it does not even have to be accepted.
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"trigger")).await;
    assert_ne!(status, StatusCode::CREATED, "body={body}");

    assert!(
        precious.exists(),
        "card B's bound attachment must survive an upload on card A"
    );
    assert_eq!(
        std::fs::read(&precious).unwrap(),
        b"card B's bound bytes",
        "and must be untouched"
    );
}

/// BLOCKER 2 of the same review: the UPLOAD write path never got the
/// descriptor treatment, so `create_dir_all` + `File::create` + `rename` on
/// `staging.path().join(..)` wrote wherever a planted `staging` link pointed —
/// including outside the attachment root entirely.
#[tokio::test]
async fn an_upload_cannot_be_redirected_outside_the_attachment_root() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    // Somewhere outside `.neige/attachments` altogether, reached relatively so
    // the case is "the target leaves the root" rather than "it is spelled
    // absolutely".
    //
    // The arithmetic is the test. The link sits at
    // `<workspace>/.neige/attachments/<card>/staging`, so its `..` is `<card>`
    // and `../../../` is `<workspace>` — which is where `outside` must be. The
    // first version of this case put `outside` under `.neige` and so pointed
    // the link at a directory the assertion never looked in: it passed with
    // every resolve guard removed, which is the shape of vacuous test this
    // round exists to stop shipping.
    let outside = b.workspace.join("escaped");
    std::fs::create_dir_all(&outside).unwrap();
    let staging = b.staging();
    std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&staging);
    std::os::unix::fs::symlink("../../../escaped", &staging).unwrap();

    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"escaping")).await;
    assert_ne!(status, StatusCode::CREATED, "body={body}");
    assert!(
        file_names(&outside).is_empty(),
        "no byte, and no `.part`, may be written outside the attachment root: {:?}",
        file_names(&outside)
    );
    // The refusal must not hand the client a host path, the same rule the read
    // path follows.
    let sentence = body["error"].as_str().unwrap_or_default();
    assert!(!sentence.contains(".neige"), "body={body}");
    assert!(!sentence.contains("escaped"), "body={body}");
}

/// #1505 S6 review — a card that is over its ceiling must be able to get back
/// under it.
///
/// The budget was a one-way door. The sweep ran only AFTER a successful
/// upload, and a card at or over its ceiling is refused before it, so the one
/// thing that could free space never ran: every later upload answered "budget
/// exhausted" forever, orphan TTL or not.
///
/// The over-budget state is reachable without any abuse. `bind` publishes into
/// `bound/` and then retires the staged original; a process that dies between
/// those two leaves the same bytes in both counted directories.
#[tokio::test]
async fn a_card_over_its_budget_recovers_once_its_staged_bytes_expire() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    // One real staged upload, so there is a `staging/` to age.
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"stale")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let stale = body["attachmentId"].as_str().unwrap().to_string();

    // 65 MiB of it: the card is over the 64 MiB ceiling on staged bytes alone.
    let big = std::fs::File::options()
        .write(true)
        .open(b.staging().join(&stale))
        .unwrap();
    big.set_len(65 * 1024 * 1024).unwrap();
    drop(big);

    // Still fresh, so nothing may be reclaimed and the refusal is correct.
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"blocked")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("attachment budget exhausted"),
        "{body}"
    );

    // Now expired. The very next upload must sweep BEFORE it measures.
    let aged = std::fs::File::options()
        .write(true)
        .open(b.staging().join(&stale))
        .unwrap();
    aged.set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60))
        .unwrap();
    drop(aged);

    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"recovered")).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an expired staged file is reclaimed before the ceiling is read: {body}"
    );
    let recovered = body["attachmentId"].as_str().unwrap().to_string();
    assert_eq!(file_names(&b.staging()), vec![recovered]);
}

/// #1515 review F3. `is_file()` follows symlinks, so a link planted under a
/// well-formed id — an agent has write access to this workspace by design —
/// made the read-back endpoint serve the link's target as `image/png`.
#[tokio::test]
async fn a_symlink_planted_under_a_valid_id_is_not_served() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let secret = b.workspace.join("id_rsa");
    std::fs::write(&secret, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();

    let planted = "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png";
    for dir in [b.staging(), b.bound()] {
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&secret, dir.join(planted)).unwrap();
    }

    let (status, _, bytes) = read_back(&b.app, &card, planted).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a planted symlink is not an attachment"
    );
    assert!(
        !bytes.starts_with(b"-----BEGIN"),
        "the link's target must never be served"
    );
}

// ---------------------------------------------------------------------------
// #1515 review round 2.
// ---------------------------------------------------------------------------

const SECRET: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot yours\n";

/// #1515 review round 2, BLOCKER. `resolve` lstats a NAME; the read-back then
/// opened the same name a moment later, and `open(2)` follows. Anything with
/// write access to the workspace — an agent, by design — can `rename` a symlink
/// onto that name in between, and on that interleaving the endpoint served the
/// link's target as `image/png`.
///
/// The two round-1 symlink tests plant the link statically, so `resolve`
/// answers 400 and the open is never reached: they structurally cannot see
/// this. This one holds the race open instead — a thread flips the name between
/// a real PNG and a symlink to a secret with `rename` (atomic, so the name is
/// always one or the other) while the endpoint is hammered. The assertion is
/// not "the last read was fine"; it is that across every read the secret's
/// bytes were never served, whatever the interleaving.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_symlink_swapped_in_after_the_check_is_never_served() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let secret = b.workspace.join("id_rsa");
    std::fs::write(&secret, SECRET).unwrap();

    let real = png(b"the real attachment");
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", real.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["attachmentId"].as_str().unwrap().to_string();

    let target = b.staging().join(&id);
    let staging = b.staging();
    let stop = Arc::new(AtomicBool::new(false));
    let flipper_stop = stop.clone();
    let flipper_secret = secret.clone();
    let flipper_real = real.clone();
    let swaps = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let flipper_swaps = swaps.clone();
    let flipper = std::thread::spawn(move || {
        let link_tmp = staging.join("swap-link.tmp");
        let file_tmp = staging.join("swap-file.tmp");
        while !flipper_stop.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&link_tmp);
            if std::os::unix::fs::symlink(&flipper_secret, &link_tmp).is_err() {
                continue;
            }
            let _ = std::fs::rename(&link_tmp, &target);
            let _ = std::fs::write(&file_tmp, &flipper_real);
            let _ = std::fs::rename(&file_tmp, &target);
            flipper_swaps.fetch_add(1, Ordering::Relaxed);
        }
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut served_real = 0u32;
    let mut refused = 0u32;
    let mut leaks = 0u32;
    let mut reads = 0u32;
    while std::time::Instant::now() < deadline {
        let (status, _, bytes) = read_back(&b.app, &card, &id).await;
        reads += 1;
        if status == StatusCode::OK {
            if bytes.starts_with(b"-----BEGIN") {
                leaks += 1;
            } else {
                served_real += 1;
            }
        } else {
            refused += 1;
        }
        if leaks > 0 {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    flipper.join().unwrap();

    assert_eq!(
        leaks, 0,
        "the link's target was served {leaks} times out of {reads} reads — the check must be on \
         the handle, not on the name"
    );
    // Neither half of the loop may be vacuous: the endpoint really answered,
    // and the flipper really swapped the name under it.
    assert!(
        served_real > 0,
        "no read ever saw the real attachment ({reads} reads, {refused} refusals) — the loop \
         proves nothing"
    );
    assert!(
        refused > 0,
        "no read ever landed on the symlink ({reads} reads, {} swaps) — the race was never open",
        swaps.load(Ordering::Relaxed)
    );
}

/// #1515 review round 2. `finish` — flush, `sync_all`, `rename` — runs after
/// the body is complete, and each of its steps can fail on a full or dying
/// disk. Round 1 took the part out of `open` *before* calling it, so those
/// failures returned through a `?` with no cleanup and left a `.part` spending
/// the card's budget until a sweep reclaimed it a day later.
///
/// The failure is forced by putting a directory where the rename's destination
/// goes: `rename(file -> dir)` is `EISDIR`, and the staging directory stays
/// writable so the cleanup that must happen still can.
#[tokio::test]
async fn a_failure_publishing_the_final_name_still_removes_the_part() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    let a = StreamingUpload::start(&b.app, &card);
    a.send(png(&[0u8; 4]));
    let part = wait_for_part(&b.staging()).await;
    let final_name = part.trim_end_matches(".part").to_string();
    std::fs::create_dir(b.staging().join(&final_name)).unwrap();

    let (status, body) = a.finish().await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a rename onto a directory must fail: {body}"
    );
    assert_eq!(
        file_names(&b.staging()),
        vec![final_name],
        "only the planted directory may remain — the `.part` must have been removed"
    );
}

/// #1515 review round 2. Round 1 took the host path out of exactly one message,
/// the rename's. Every refusal this endpoint produces is rendered into an HTTP
/// body, so the whole class has to be swept — and a test that asserts only the
/// status cannot see a leak.
#[tokio::test]
async fn no_refusal_body_carries_the_host_workspace_path() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let workspace = b.workspace.display().to_string();

    let check = |what: &str, body: &Value| {
        let rendered = body.to_string();
        assert!(
            !rendered.contains(&workspace),
            "{what}: the error body names the host workspace path: {rendered}"
        );
        // `.neige/` on its own is allowed: it is the workspace-relative name
        // the user is told to look under, and it appears in the budget advice
        // too. What must never appear is an absolute host path — the
        // workspace, or the layout above it.
        assert!(
            !rendered.contains("/workspaces/"),
            "{what}: the error body names the server's on-disk layout: {rendered}"
        );
    };

    // (a) a card directory that is not a directory: `bound/` is a regular
    //     file. The refusal now comes from the guarded OPEN rather than from
    //     the measurement — every filesystem step resolves through descriptors
    //     opened up front — and it is a 500 rather than a 400 for the reason
    //     that distinction exists: the client did not do this and cannot fix
    //     it. What this case is about is unchanged: whatever refuses, the body
    //     must not carry a host path, and the opener's own errors do.
    std::fs::create_dir_all(b.bound().parent().unwrap()).unwrap();
    std::fs::write(b.bound(), b"not a directory").unwrap();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    check("unusable card directory", &body);

    // (b) the read-back cannot open what the id names.
    std::fs::remove_file(b.bound()).unwrap();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["attachmentId"].as_str().unwrap().to_string();
    std::os::unix::fs::symlink(b.workspace.join("gone"), b.staging().join("shadow.png")).unwrap();
    std::fs::remove_file(b.staging().join(&id)).unwrap();
    std::os::unix::fs::symlink(b.workspace.join("id_rsa"), b.staging().join(&id)).unwrap();
    let (status, _, bytes) = read_back(&b.app, &card, &id).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    check("unresolvable attachment", &body);

    // (c) the git exclude entry cannot be established. Round 2's commit claimed
    //     "the same rule holds for store_upload's failures"; it did not — this
    //     arm interpolated the repository path into the 500 body.
    let exclude = b.workspace.join(".git").join("info").join("exclude");
    std::fs::remove_file(&exclude).unwrap();
    std::fs::create_dir(&exclude).unwrap();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"x")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    check("git exclude failure", &body);
}

/// #1515 review round 3. The common ending for an upload is not a `return` — it
/// is the handler future being dropped, because the client went away. Round 2
/// put cleanup in an arm that only ran on `return`, so a reset connection left
/// a `<uuid>.png.part` in `staging/` spending the card's budget until the 24h
/// sweep. Cleanup now hangs off `OpenPart`'s destructor, which a drop does run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_disappears_mid_body_leaves_no_part() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();

    let a = StreamingUpload::start(&b.app, &card);
    a.send(png(&[0u8; 4]));
    let part = wait_for_part(&b.staging()).await;

    // Aborting the task drops the handler future exactly where axum drops it
    // when a connection resets: inside `stream_into`, awaiting the next frame,
    // with the `.part` open.
    a.response.abort();
    let _ = a.response.await;
    drop(a.frames);

    for _ in 0..200 {
        if file_names(&b.staging()).is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!(
        "the dropped handler left {part} behind: {:?}",
        file_names(&b.staging())
    );
}
