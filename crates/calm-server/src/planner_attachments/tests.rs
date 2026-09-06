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
fn used_bytes_sums_the_regular_files_in_both_directories() {
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
}

/// #1515 review F2. An agent has write access to this workspace by design, so
/// it can put a dangling symlink in `staging/`. When that made the measurement
/// `Err`, the card's upload channel was disabled permanently: every later POST
/// answered 400, and the sweep that would have cleared the entry returned zero
/// deletions on the same entry. The planted link must be classified as "not one
/// of ours" — not measured, not deleted, not fatal.
#[test]
fn a_planted_symlink_does_not_latch_the_budget_off() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    std::fs::write(staging.path().join("real.png"), vec![0u8; 10]).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("nonexistent"),
        staging.path().join("planted.png"),
    )
    .unwrap();
    // A symlink that resolves is equally not ours, and equally uncounted.
    std::fs::write(root.path().join("elsewhere"), vec![0u8; 4096]).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("elsewhere"),
        staging.path().join("planted2.png"),
    )
    .unwrap();

    for attempt in 0..3 {
        assert_eq!(
            used_bytes(root.path(), &card).unwrap(),
            10,
            "attempt {attempt}: only the regular file this store wrote is counted"
        );
    }
}

/// The other half of fail-closed, kept: a filesystem that will not answer still
/// refuses the write. Here `staging` is a regular file, so `read_dir` is
/// `ENOTDIR` — a broken subtree, not a foreign entry.
#[test]
fn a_directory_that_cannot_be_enumerated_still_refuses() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path().parent().unwrap()).unwrap();
    std::fs::write(staging.path(), b"not a directory").unwrap();

    let error =
        used_bytes(root.path(), &card).expect_err("an unenumerable directory must refuse a write");
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

/// #1515 review F2, sweep side. A dangling symlink used to abort the whole
/// enumeration, so the expired file beside it was never reclaimed — and since
/// the same entry also latched `used_bytes`, one planted link disabled uploads
/// and reclamation together. The link is stepped over; the expired file goes;
/// the link itself is left alone.
#[test]
fn a_planted_symlink_is_stepped_over_and_never_deleted() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();

    let expired = staging.path().join("expired.png");
    std::fs::write(&expired, b"x").unwrap();
    set_age(&expired, Duration::from_secs(25 * 60 * 60));
    let planted = staging.path().join("planted.png");
    std::os::unix::fs::symlink(root.path().join("gone"), &planted).unwrap();

    let removed = sweep_staging_at(&staging, SystemTime::now(), ORPHAN_TTL);
    assert_eq!(
        removed,
        vec!["expired.png".to_string()],
        "the planted entry must not stop the sweep"
    );
    assert!(!expired.exists(), "the expired file is reclaimed");
    assert!(
        std::fs::symlink_metadata(&planted).is_ok(),
        "the sweep must not have deleted the planted link either — it is not ours to age out"
    );
}

/// Fail-closed is kept where it belongs: an entry the filesystem refuses to
/// describe (here: `staging/` readable but not searchable, so `lstat` on its
/// children is `EACCES`) means the ages are unknown and nothing is deleted.
#[test]
fn a_sweep_that_cannot_stat_an_entry_deletes_nothing_at_all() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    let expired = staging.path().join("expired.png");
    std::fs::write(&expired, b"x").unwrap();
    set_age(&expired, Duration::from_secs(25 * 60 * 60));

    // r but not x: `read_dir` lists the names, `lstat` on each one is refused.
    std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let refused = std::fs::symlink_metadata(&expired).is_err();
    // Root ignores the mode bits, so the construction would be vacuous there.
    // Say so loudly rather than reporting a green that proved nothing.
    assert!(
        refused,
        "precondition: the entry stat must actually be refused — these tests must not run as root"
    );

    let removed = sweep_staging_at(&staging, SystemTime::now(), ORPHAN_TTL);
    std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(removed.is_empty(), "a failed enumeration removes nothing");
    assert!(
        expired.exists(),
        "the expired file must survive: one unreadable entry means the ages here are unknown, \
         and the sweep must not delete on an unknown age"
    );
}

/// #1515 review F3. `is_file()` follows symlinks, so a link planted under a
/// valid id served the target's bytes through the read-back endpoint. `resolve`
/// stats with `symlink_metadata` and answers only for a regular file.
#[test]
fn a_symlink_under_a_valid_id_does_not_resolve() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    let bound = bound_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    std::fs::create_dir_all(bound.path()).unwrap();
    let secret = root.path().join("id_rsa");
    std::fs::write(&secret, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();

    let staged = id("05", AttachmentFormat::Png);
    let planted_bound = id("06", AttachmentFormat::Png);
    std::os::unix::fs::symlink(&secret, staging.path().join(staged.as_str())).unwrap();
    std::os::unix::fs::symlink(&secret, bound.path().join(planted_bound.as_str())).unwrap();

    for planted in [&staged, &planted_bound] {
        let error = resolve(root.path(), &card, planted)
            .expect_err("a symlink is not an attachment this subtree serves");
        assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
    }
}

/// The adjudication behind F3, pinned as executable facts rather than as an
/// argument in a comment: neither of this module's two mutating primitives
/// escapes the subtree through a planted link.
#[test]
fn neither_unlink_nor_rename_follows_a_planted_symlink() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    let outside = root.path().join("outside.txt");

    // (a) `remove_file` unlinks the link, never the target.
    std::fs::write(&outside, b"still here").unwrap();
    let link = staging.path().join("unlink-me.png");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    super::gc::remove_staged_file(&staging, "unlink-me.png").unwrap();
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the link is gone"
    );
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"still here",
        "the target outside the subtree survives an unlink of the link"
    );

    // (b) `rename` onto a symlink replaces the link, and does not write
    //     through it.
    let target_name = staging.path().join("rename-onto.png");
    std::os::unix::fs::symlink(&outside, &target_name).unwrap();
    let part = staging.path().join("rename-onto.png.part");
    std::fs::write(&part, b"fresh bytes").unwrap();
    std::fs::rename(&part, &target_name).unwrap();
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"still here",
        "the target outside the subtree is untouched by a rename onto the link"
    );
    assert!(
        std::fs::symlink_metadata(&target_name)
            .unwrap()
            .file_type()
            .is_file(),
        "the link was replaced by the renamed regular file"
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
