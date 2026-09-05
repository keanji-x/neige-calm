//! Path derivation, the budget measurement, and the staging sweep.

use std::time::{Duration, SystemTime};

use calm_types::planner_attachment::{AttachmentFormat, AttachmentId};

use super::gc::{ORPHAN_TTL, sweep_staging_at};
use super::*;
use crate::model::{TrackWorkspace, TrackWorkspaceKind};

fn managed(path: &std::path::Path) -> TrackWorkspace {
    TrackWorkspace {
        kind: TrackWorkspaceKind::Managed,
        path: path.to_string_lossy().to_string(),
        frozen_at: None,
    }
}

fn attached(path: &std::path::Path) -> TrackWorkspace {
    TrackWorkspace {
        kind: TrackWorkspaceKind::Attached,
        path: path.to_string_lossy().to_string(),
        frozen_at: None,
    }
}

fn id(hex_tail: &str, format: AttachmentFormat) -> AttachmentId {
    AttachmentId::parse(&format!(
        "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e{hex_tail}.{}",
        format.ext()
    ))
    .expect("fixture id must match the grammar")
}

fn set_age(path: &std::path::Path, age: Duration) {
    let when = SystemTime::now() - age;
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(when).unwrap();
}

#[test]
fn attached_workspaces_get_no_attachment_root_at_all() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("area").join("track");
    let error = attachment_root(&attached(&workspace), root.path())
        .expect_err("an attached workspace must be refused");
    assert!(
        matches!(&error, CalmError::BadRequest(message) if message.contains("managed workspace")),
        "expected a BadRequest naming the managed requirement, got {error:?}"
    );
}

#[test]
fn managed_workspaces_root_under_dot_neige() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("area").join("track");
    assert_eq!(
        attachment_root(&managed(&workspace), root.path()).unwrap(),
        workspace.join(".neige").join("attachments")
    );
}

#[test]
fn a_managed_path_outside_the_workspace_root_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let error = attachment_root(&managed(elsewhere.path()), root.path())
        .expect_err("a managed path outside the root must not be served");
    assert!(matches!(error, CalmError::Internal(_)), "{error:?}");
}

#[test]
fn resolve_prefers_bound_then_staging_and_refuses_anything_else() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let other_card = CardId::from("card-b");
    let staged = id("01", AttachmentFormat::Png);
    let bound = id("02", AttachmentFormat::Webp);

    std::fs::create_dir_all(staging_dir(root.path(), &card).path()).unwrap();
    std::fs::create_dir_all(bound_dir(root.path(), &card).path()).unwrap();
    std::fs::write(
        staging_dir(root.path(), &card).path().join(staged.as_str()),
        b"s",
    )
    .unwrap();
    std::fs::write(
        bound_dir(root.path(), &card).path().join(bound.as_str()),
        b"b",
    )
    .unwrap();

    let (path, format) = resolve(root.path(), &card, &bound).unwrap();
    assert_eq!(
        path,
        bound_dir(root.path(), &card).path().join(bound.as_str())
    );
    assert_eq!(format, AttachmentFormat::Webp);

    let (path, format) = resolve(root.path(), &card, &staged).unwrap();
    assert_eq!(
        path,
        staging_dir(root.path(), &card).path().join(staged.as_str())
    );
    assert_eq!(format, AttachmentFormat::Png);

    // Cross-card forgery is answered by the directory, not by a comparison:
    // card B's directory simply does not contain card A's id.
    let error =
        resolve(root.path(), &other_card, &bound).expect_err("another card's id must not resolve");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");

    let error = resolve(root.path(), &card, &id("03", AttachmentFormat::Gif))
        .expect_err("an id that stats nowhere must not resolve");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
}

#[test]
fn used_bytes_sums_both_directories_and_refuses_what_it_cannot_measure() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    assert_eq!(
        used_bytes(root.path(), &card).unwrap(),
        0,
        "a card with no directories yet has spent nothing"
    );

    let staging = staging_dir(root.path(), &card);
    let bound = bound_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    std::fs::create_dir_all(bound.path()).unwrap();
    std::fs::write(staging.path().join("a.png"), vec![0u8; 10]).unwrap();
    std::fs::write(bound.path().join("b.png"), vec![0u8; 32]).unwrap();
    assert_eq!(used_bytes(root.path(), &card).unwrap(), 42);

    // Fail-closed: an entry whose size cannot be established makes the whole
    // measurement an error, because this number gates a write.
    std::os::unix::fs::symlink(
        root.path().join("gone"),
        staging.path().join("dangling.png"),
    )
    .unwrap();
    let error = used_bytes(root.path(), &card)
        .expect_err("an unmeasurable entry must refuse, not be skipped");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
}

#[test]
fn sweep_removes_expired_staged_files_and_never_touches_bound() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    let bound = bound_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    std::fs::create_dir_all(bound.path()).unwrap();

    let old_staged = staging.path().join("old.png");
    let fresh_staged = staging.path().join("fresh.png");
    let old_bound = bound.path().join("old.png");
    for path in [&old_staged, &fresh_staged, &old_bound] {
        std::fs::write(path, b"x").unwrap();
    }
    set_age(&old_staged, Duration::from_secs(25 * 60 * 60));
    set_age(&fresh_staged, Duration::from_secs(60 * 60));
    set_age(&old_bound, Duration::from_secs(25 * 60 * 60));

    let removed = sweep_staging_at(&staging, SystemTime::now(), ORPHAN_TTL);
    assert_eq!(removed, vec!["old.png".to_string()]);
    assert!(!old_staged.exists(), "(a) an expired staged file goes");
    assert!(fresh_staged.exists(), "(b) a fresh staged file stays");
    assert!(
        old_bound.exists(),
        "(c) an equally old bound file stays — the sweep cannot even name that directory"
    );
}

#[test]
fn a_sweep_that_cannot_stat_one_entry_deletes_nothing_at_all() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();

    let expired = staging.path().join("expired.png");
    std::fs::write(&expired, b"x").unwrap();
    set_age(&expired, Duration::from_secs(25 * 60 * 60));
    std::os::unix::fs::symlink(
        root.path().join("gone"),
        staging.path().join("dangling.png"),
    )
    .unwrap();

    let removed = sweep_staging_at(&staging, SystemTime::now(), ORPHAN_TTL);
    assert!(removed.is_empty(), "a failed enumeration removes nothing");
    assert!(
        expired.exists(),
        "the expired file must survive: one unreadable entry means the ages here are unknown, \
         and the sweep must not delete on an unknown age"
    );
}

#[test]
fn the_read_back_url_is_built_by_the_server() {
    let card = CardId::from("card-a");
    let attachment = id("04", AttachmentFormat::Jpeg);
    assert_eq!(
        attachment_url(&card, &attachment),
        format!("/api/cards/card-a/planner/attachments/{attachment}")
    );
}
