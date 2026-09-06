//! #1505 S6-PR2 — binding an uploaded attachment to a message, and carrying it
//! into `turn/start`.
//!
//! S6-PR1 shipped the disk half with nothing that wrote `bound/`, so every
//! upload expired after the orphan TTL. What this file pins is the half that
//! makes an attachment permanent, and it pins it at the two places the claim
//! could be false: after a sweep that would have removed a staged file, and in
//! the payload actually handed to the daemon.

use std::path::Path;
use std::time::{Duration, SystemTime};

use axum::http::StatusCode;
use calm_server::codex_appserver::InputItem;
use calm_server::harness::HarnessSnapshot;
use calm_server::planner_attachments::{bound_file_path, gc, staging_dir};
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

/// Run the staging sweep with a TTL of zero — i.e. reclaim everything the
/// sweep is entitled to reclaim, right now.
///
/// This is the decisive instrument of this file. "The attachment is permanent"
/// is not a claim about the passage of time; it is a claim about which
/// directory the bytes are in, and the sweep is the thing that reads that
/// directory. Asserting the file still exists without running the sweep would
/// pass identically before and after the bind path.
fn sweep_everything_reclaimable(boot: &Boot) -> Vec<String> {
    let root = boot.attachment_root();
    let staging = staging_dir(&root, &boot.planner_card.id);
    gc::sweep_staging_at(&staging, SystemTime::now(), Duration::from_secs(0))
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

/// The acceptance case: a sent message's attachment is out of the sweep's
/// reach, and its url still answers.
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

    let reclaimed = sweep_everything_reclaimable(&boot);
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

/// The negative half of the same statement, and the reason the sweep above is
/// the right instrument: an upload nobody sent IS reclaimed, and its url then
/// stops working. Without this, the test above could pass with a sweep that
/// does nothing at all.
#[tokio::test]
async fn an_attachment_nobody_sent_is_still_reclaimed() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let id = staged(&boot, b"never sent").await;

    let reclaimed = sweep_everything_reclaimable(&boot);
    assert_eq!(reclaimed, vec![id.clone()]);
    assert_eq!(read_back(&boot, &id).await, StatusCode::BAD_REQUEST);
}

/// What actually reaches codex: a `localImage` item, after the text, naming a
/// path that exists on disk at the moment the turn is issued.
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

    let mut issued = Vec::new();
    for _ in 0..4_000 {
        issued = boot.daemon.started_turns_for_test();
        if !issued.is_empty() {
            break;
        }
        tokio::task::yield_now().await;
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

/// Pasting a screenshot and pressing enter. An empty text beside an attachment
/// is a message; an empty text beside nothing is still a refusal.
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

/// Cross-card forgery. The path is derived from the card in the URL, so
/// another card's id simply is not there — and the refusal must happen before
/// anything is written to this card's queue.
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

/// The two shapes of a malformed list. Both are refusals rather than quiet
/// repairs, because both mean the client lost track of its own state.
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

/// The queue read shows the attachment, by id, with no host path anywhere in
/// the response.
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
    let entry = &run["pending"][0];
    assert_eq!(entry["attachments"][0]["id"], json!(id));
    assert_eq!(entry["attachments"][0]["contentType"], json!("image/png"));
    assert!(entry["attachments"][0]["size"].as_u64().unwrap_or(0) > 0);

    let serialized = run.to_string();
    assert!(
        !serialized.contains(".neige"),
        "the queue read must not carry the workspace path: {serialized}"
    );
}

/// A queued attachment is part of the entry, so it survives the snapshot the
/// harness persists and a boot recovery replays.
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

/// A pre-S6 snapshot has no `attachments` key. It must read back as an
/// addressable entry with no attachments — NOT as a `LegacyUser`, which is
/// what a required field would have made it.
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

/// Binding moves bytes between the two directories the budget counts, so the
/// number does not change. What changes is that the bound half can never come
/// back — which is exactly what the budget's doc comment now says.
#[tokio::test]
async fn binding_does_not_change_what_the_card_has_spent() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let id = staged(&boot, &[7u8; 4096]).await;

    let root = boot.attachment_root();
    let before =
        calm_server::planner_attachments::used_bytes(&root, &boot.planner_card.id).unwrap();
    assert!(before > 4096);

    post_input_with_attachments(boot.app.clone(), &card_id, "spend", &[id]).await;

    let after = calm_server::planner_attachments::used_bytes(&root, &boot.planner_card.id).unwrap();
    assert_eq!(after, before, "a bind is a move, not a second copy");
}

/// Naming the same attachment on a second message is legal and is a no-op on
/// disk: the first send already bound it.
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

/// An unparsable id never reaches the store: `AttachmentId`'s grammar is the
/// gate, and it runs during deserialization of the request body.
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
