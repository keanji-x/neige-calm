//! #1505 S6 PR1 — the upload and read-back endpoints, driven through the real
//! router with a real managed workspace on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// The per-card budget, in all three of its shapes: refused before the body is
/// read, refused mid-stream, and refused when the directory cannot be measured.
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

    // Fail-closed: a directory whose bytes cannot be counted refuses the
    // upload rather than admitting it.
    std::fs::remove_file(b.bound().join("bulk.png")).unwrap();
    std::fs::remove_file(b.bound().join("bulk2.png")).unwrap();
    std::os::unix::fs::symlink(b.bound().join("gone"), b.bound().join("dangling.png")).unwrap();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"tiny")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
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
