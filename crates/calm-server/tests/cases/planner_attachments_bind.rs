//! Binding an uploaded attachment to a message, and carrying it into `turn/start`.

use std::path::Path;
use std::time::{Duration, SystemTime};

use axum::http::StatusCode;
use calm_server::codex_appserver::InputItem;
use calm_server::harness::HarnessSnapshot;
use calm_server::planner_attachments::{bound_file_path, dir, gc};
use calm_types::planner_attachment::AttachmentId;
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{
    Boot, Issuance, boot_with, boot_with_issuance, get, idle_snapshot, post_input,
    post_input_with_attachments, send_json, upload_png,
};

/// Upload one png and hand back its id.
async fn staged(boot: &Boot, payload: &[u8]) -> String {
    let (status, body) = upload_png(boot.app.clone(), boot.planner_card.id.as_str(), payload).await;
    assert_eq!(status, StatusCode::CREATED, "upload failed: {body}");
    body["attachmentId"]
        .as_str()
        .expect("the upload answers with an id")
        .to_string()
}

/// Run the staging sweep with a TTL of zero, reclaiming everything the sweep is entitled to reclaim right now.
async fn sweep_everything_reclaimable(boot: &Boot) -> Vec<String> {
    let dirs = dir::open_card_dirs(&boot.attachment_root(), &boot.planner_card.id)
        .await
        .expect("the card's directories open");
    gc::sweep_staging_at(dirs.staging(), SystemTime::now(), Duration::from_secs(0))
        .into_iter()
        .map(|name| name.as_str().to_string())
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

async fn read_back(boot: &Boot, attachment_id: &str) -> StatusCode {
    let (status, _) = get(
        boot.app.clone(),
        format!(
            "/api/cards/{}/planner/attachments/{attachment_id}",
            boot.planner_card.id.as_str()
        ),
    )
    .await;
    status
}

#[tokio::test]
async fn a_named_attachment_is_moved_out_of_reach_of_the_orphan_sweep() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"bound bytes").await;

    assert_eq!(
        names(&boot.staging_dir()),
        vec![id.clone()],
        "before the send the bytes are staged"
    );

    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "look at this",
        std::slice::from_ref(&id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        names(&boot.staging_dir()).is_empty(),
        "binding leaves nothing behind in staging"
    );
    assert_eq!(names(&boot.bound_dir()), vec![id.clone()]);

    let reclaimed = sweep_everything_reclaimable(&boot).await;
    assert!(
        reclaimed.is_empty(),
        "the sweep found nothing to reclaim, but reported {reclaimed:?}"
    );
    assert_eq!(
        read_back(&boot, &id).await,
        StatusCode::OK,
        "the url from the upload response still answers after a full sweep"
    );
}

#[tokio::test]
async fn an_attachment_nobody_sent_is_still_reclaimed() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let id = staged(&boot, b"never sent").await;

    let reclaimed = sweep_everything_reclaimable(&boot).await;
    assert_eq!(reclaimed, vec![id.clone()]);
    assert_eq!(read_back(&boot, &id).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_issued_turn_carries_a_local_image_item_for_the_attachment() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"issued bytes").await;

    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "what is this?",
        std::slice::from_ref(&id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // Wait on the clock, not the scheduler: issuance is driven by the run loop's 50ms tick, so a fixed number
    // of cooperative yields can all land inside one tick on a loaded machine.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut issued = Vec::new();
    while std::time::Instant::now() < deadline {
        issued = boot.daemon.started_turns_for_test();
        if !issued.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (_thread, items) = issued.first().expect("the live harness issued a turn");
    assert_eq!(items.len(), 2, "text then image: {items:?}");
    assert!(
        matches!(&items[0], InputItem::Text { text } if text.contains("what is this?")),
        "the first item is the text: {items:?}"
    );
    let InputItem::LocalImage { path } = &items[1] else {
        panic!("the second item must be a localImage: {items:?}");
    };

    let parsed = AttachmentId::parse(&id).unwrap();
    let expected = bound_file_path(&boot.attachment_root(), &boot.planner_card.id, &parsed);
    assert_eq!(
        Path::new(path),
        expected,
        "the path handed to codex is the bound path, not the staged one"
    );
    assert!(
        Path::new(path).is_file(),
        "the path handed to codex names a file that exists"
    );
}

#[tokio::test]
async fn an_image_with_no_text_is_a_message_but_an_empty_message_is_not() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"speaks for itself").await;

    let (status, body) =
        post_input_with_attachments(boot.app.clone(), &card_id, "", std::slice::from_ref(&id))
            .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an image alone is a message: {body}"
    );

    let (status, body) = post_input(boot.app.clone(), &card_id, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("must not be empty"),
        "body={body}"
    );
}

#[tokio::test]
async fn an_id_from_another_card_is_refused_and_queues_nothing() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    // A well-formed id that this card never minted.
    let foreign = "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png";

    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "borrowed",
        std::slice::from_ref(&foreign.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");

    let (_, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(
        run["pending"].as_array().map(Vec::len),
        Some(0),
        "a refused bind must not leave a message in the queue: {run}"
    );
}

#[tokio::test]
async fn a_list_that_is_too_long_or_repeats_itself_is_refused() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();

    let mut ids = Vec::new();
    for index in 0..9u8 {
        ids.push(staged(&boot, &[index; 16]).await);
    }

    let (status, body) =
        post_input_with_attachments(boot.app.clone(), &card_id, "nine", &ids).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("at most"),
        "body={body}"
    );

    let (status, body) =
        post_input_with_attachments(boot.app.clone(), &card_id, "eight", &ids[..8]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "eight is the cap, not the refusal: {body}"
    );

    let twice = vec![ids[8].clone(), ids[8].clone()];
    let (status, body) =
        post_input_with_attachments(boot.app.clone(), &card_id, "twice", &twice).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("twice"),
        "body={body}"
    );
}

#[tokio::test]
async fn planner_run_lists_a_queued_messages_attachments_without_a_host_path() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"listed").await;
    post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "see attached",
        std::slice::from_ref(&id),
    )
    .await;

    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // This fixture's track has a managed workspace, the only shape that supports attachments.
    assert_eq!(run["attachments_supported"], json!(true));
    let entry = &run["pending"][0];
    assert_eq!(entry["attachments"][0]["id"], json!(id));
    assert_eq!(entry["attachments"][0]["contentType"], json!("image/png"));
    assert_eq!(
        entry["attachments"][0]["url"],
        json!(format!("/api/cards/{card_id}/planner/attachments/{id}")),
        "the read-back url is server-built, so no client has to compose one"
    );
    assert!(entry["attachments"][0]["size"].as_u64().unwrap_or(0) > 0);

    let serialized = run.to_string();
    assert!(
        !serialized.contains(".neige"),
        "the queue read must not carry the workspace path: {serialized}"
    );
}

/// `GET /harness/items` returns each stored `params` blob verbatim. The row is inserted directly rather than
/// waited for: an empty transcript would satisfy a grep without exercising anything.
#[tokio::test]
async fn the_transcript_route_does_not_carry_the_local_image_host_path() {
    use calm_server::db::prelude::*;

    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let host_path = boot
        .bound_dir()
        .join("0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png")
        .to_string_lossy()
        .into_owned();

    boot.repo
        .harness_item_insert(
            &boot.worker_session_id,
            &card_id,
            boot.planner_card.track_id.as_str(),
            "thread-redaction",
            Some("turn-redaction"),
            Some("user-redaction"),
            Some("userMessage"),
            "item/completed",
            &json!({
                "completedAtMs": 7,
                "item": {
                    "id": "user-redaction",
                    "type": "userMessage",
                    "content": [
                        {"type": "text", "text": "what is this?"},
                        {"type": "localImage", "path": host_path}
                    ]
                }
            })
            .to_string(),
            None,
        )
        .await
        .unwrap();

    let (status, items) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/harness/items?after_id=0&limit=300&direction=asc"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={items}");
    let serialized = items.to_string();
    // The row really is in the response — otherwise the grep below proves nothing.
    assert!(serialized.contains("what is this?"), "{serialized}");
    assert!(serialized.contains("localImage"), "{serialized}");
    assert!(
        !serialized.contains(".neige"),
        "the transcript read must not carry the workspace path: {serialized}"
    );
    assert!(
        !serialized.contains(&host_path),
        "the transcript read must not carry the workspace path: {serialized}"
    );
}

#[tokio::test]
async fn a_queued_attachment_survives_the_snapshot_round_trip() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"restarted").await;
    post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "outlive me",
        std::slice::from_ref(&id),
    )
    .await;

    let persisted = serde_json::to_value(boot.harness.snapshot().await).unwrap();
    let restored = HarnessSnapshot::from_value_strict(persisted);
    let entries = restored.pending_entries();
    assert_eq!(entries.len(), 1);
    let attachments = entries[0].attachments();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].id.as_str(), id);
    assert!(
        attachments[0].path.ends_with(&id),
        "the bound path is persisted with the entry: {attachments:?}"
    );
}

/// A pre-attachments snapshot has no `attachments` key; it must read back as an addressable entry, not as a `LegacyUser`.
#[tokio::test]
async fn a_snapshot_written_before_this_slice_keeps_its_entry_addressable() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    post_input(boot.app.clone(), &card_id, "queued before the upgrade").await;

    let mut persisted = serde_json::to_value(boot.harness.snapshot().await).unwrap();
    let metas = persisted["pending_entry_meta"]
        .as_array_mut()
        .expect("meta array");
    for meta in metas.iter_mut() {
        assert!(
            meta.as_object_mut()
                .expect("a meta slot is an object")
                .remove("attachments")
                .is_some(),
            "the key this test removes must have been there"
        );
    }

    let restored = HarnessSnapshot::from_value_strict(persisted);
    let entries = restored.pending_entries();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].user_view().is_some(),
        "an old row stays addressable: {entries:?}"
    );
    assert!(entries[0].attachments().is_empty());
}

#[tokio::test]
async fn binding_does_not_change_what_the_card_has_spent() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, &[7u8; 4096]).await;

    let root = boot.attachment_root();
    let measure = || async {
        let dirs = dir::open_card_dirs(&root, &boot.planner_card.id)
            .await
            .expect("the card's directories open");
        calm_server::planner_attachments::used_bytes(&dirs).unwrap()
    };
    let before = measure().await;
    assert!(before > 4096);

    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "spend",
        std::slice::from_ref(&id),
    )
    .await;
    // The bind must be shown to have happened before the equality means anything.
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(names(&boot.bound_dir()), vec![id], "the bind ran");
    assert!(names(&boot.staging_dir()).is_empty());

    let after = measure().await;
    assert_eq!(after, before, "a bind is a move, not a second copy");
}

#[tokio::test]
async fn a_bound_directory_symlinked_to_another_card_refuses_before_writing() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"do not follow me").await;

    let victim = boot.attachment_root().join("card-victim").join("bound");
    std::fs::create_dir_all(&victim).unwrap();
    // RELATIVE: an absolute target is refused by `RESOLVE_BENEATH` on its own; `../card-victim/bound` stays beneath
    // the root so only `RESOLVE_NO_SYMLINKS` refuses it. The upload created `bound/` already, so the link replaces it.
    std::fs::remove_dir(boot.bound_dir()).unwrap();
    std::os::unix::fs::symlink("../card-victim/bound", boot.bound_dir()).unwrap();

    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "planted",
        std::slice::from_ref(&id),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    assert!(
        names(&victim).is_empty(),
        "not one byte may be written through the link: {:?}",
        names(&victim)
    );
    assert_eq!(
        names(&boot.staging_dir()),
        vec![id],
        "the staged original is untouched, so nothing was lost"
    );
    let (_, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(
        run["pending"].as_array().map(Vec::len),
        Some(0),
        "run={run}"
    );
}

/// `RESOLVE_NO_SYMLINKS` catches the sibling-card spelling; `RESOLVE_BENEATH` catches this one even without it.
#[tokio::test]
async fn a_bound_directory_symlinked_outside_the_root_refuses_before_writing() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"do not escape").await;

    let outside = boot.workspace.join("escaped");
    std::fs::create_dir_all(&outside).unwrap();
    // Relative here too, so the case is "the target leaves the root", which `RESOLVE_BENEATH` answers.
    std::fs::remove_dir(boot.bound_dir()).unwrap();
    std::os::unix::fs::symlink("../../../escaped", boot.bound_dir()).unwrap();

    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "escaping",
        std::slice::from_ref(&id),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body={body}");
    assert!(
        names(&outside).is_empty(),
        "nothing may be written outside the attachment root: {:?}",
        names(&outside)
    );
    assert_eq!(names(&boot.staging_dir()), vec![id]);
    // The refusal must not hand the client a host path.
    let sentence = body["error"].as_str().unwrap_or_default();
    assert!(!sentence.contains(".neige"), "body={body}");
    assert!(!sentence.contains("escaped"), "body={body}");
}

/// The refusal here is delivered by the READ, not the bind: `open_attachment` resolves `<card>/staging/<id>`
/// under the same rule, so a symlinked `staging` is `ELOOP` there first.
#[tokio::test]
async fn a_staging_directory_symlinked_elsewhere_refuses_before_writing() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"temporary somewhere else").await;

    // Move the real staging aside and put a link in its place, so the attachment still exists and only the directory lies.
    let real = boot.staging_dir();
    let elsewhere = real.parent().unwrap().join("elsewhere");
    std::fs::rename(&real, &elsewhere).unwrap();
    std::os::unix::fs::symlink("elsewhere", &real).unwrap();

    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "planted staging",
        std::slice::from_ref(&id),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "body={body}");
    assert!(
        !names(&elsewhere).iter().any(|name| name.ends_with(".part")),
        "no temporary may be written through the link: {:?}",
        names(&elsewhere)
    );
}

#[test]
fn the_redactor_rewrites_every_local_image_path_and_nothing_else() {
    use calm_server::planner_attachments::{REDACTED_LOCAL_IMAGE_PATH, redact_local_image_paths};

    let params = json!({
        "completedAtMs": 1,
        "item": {"content": [
            {"type": "text", "text": "look"},
            {"type": "localImage", "path": "/srv/w/.neige/attachments/c/bound/a.png"},
            {"nested": {"type": "localImage", "path": "/srv/w/.neige/attachments/c/bound/b.png"}}
        ]}
    })
    .to_string();
    let redacted = redact_local_image_paths(&params);
    assert!(!redacted.contains(".neige"), "{redacted}");
    assert_eq!(
        redacted.matches(REDACTED_LOCAL_IMAGE_PATH).count(),
        2,
        "{redacted}"
    );
    let value: Value = serde_json::from_str(&redacted).unwrap();
    assert_eq!(value["completedAtMs"], json!(1));
    assert_eq!(value["item"]["content"][0]["text"], json!("look"));
    assert_eq!(value["item"]["content"][1]["type"], json!("localImage"));

    let untouched = r#"{"item":{"text":"plain"}}"#;
    assert_eq!(redact_local_image_paths(untouched), untouched);
    // Not JSON at all: stored opaque, returned opaque, never a panic.
    assert_eq!(redact_local_image_paths("{broken"), "{broken");
}

#[tokio::test]
async fn naming_an_already_bound_attachment_again_is_accepted() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, b"twice over two messages").await;

    let (first, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "once",
        std::slice::from_ref(&id),
    )
    .await;
    assert_eq!(first, StatusCode::OK, "body={body}");
    let (second, body) = post_input_with_attachments(
        boot.app.clone(),
        &card_id,
        "again",
        std::slice::from_ref(&id),
    )
    .await;
    assert_eq!(second, StatusCode::OK, "body={body}");
    assert_eq!(names(&boot.bound_dir()), vec![id]);
}

#[tokio::test]
async fn a_traversal_shaped_id_is_rejected_by_the_body_schema() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let (status, body): (StatusCode, Value) = send_json(
        boot.app.clone(),
        "POST",
        format!("/api/cards/{card_id}/planner/input"),
        "user",
        json!({"text": "escape", "attachments": ["../../etc/passwd.png"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body={body}");
}
