//! Path derivation, the budget measurement, and the staging sweep.

use std::os::unix::fs::FileTypeExt;
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

async fn read_all(mut opened: OpenAttachment) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    opened.file.read_to_end(&mut bytes).await.unwrap();
    bytes
}

#[tokio::test]
async fn open_attachment_prefers_bound_then_staging_and_refuses_anything_else() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let other_card = CardId::from("card-b");
    let staged = id("01", AttachmentFormat::Png);
    let bound = id("02", AttachmentFormat::Webp);

    std::fs::create_dir_all(staging_dir(root.path(), &card).path()).unwrap();
    std::fs::create_dir_all(bound_dir(root.path(), &card).path()).unwrap();
    std::fs::write(
        staging_dir(root.path(), &card).path().join(staged.as_str()),
        b"staged bytes",
    )
    .unwrap();
    std::fs::write(
        bound_dir(root.path(), &card).path().join(bound.as_str()),
        b"bound bytes",
    )
    .unwrap();

    let opened = open_attachment(root.path(), &card, &bound).await.unwrap();
    assert_eq!(opened.format, AttachmentFormat::Webp);
    assert_eq!(opened.size, 11);
    assert_eq!(read_all(opened).await, b"bound bytes");

    let opened = open_attachment(root.path(), &card, &staged).await.unwrap();
    assert_eq!(opened.format, AttachmentFormat::Png);
    assert_eq!(read_all(opened).await, b"staged bytes");

    // Cross-card forgery is answered by the root the resolution is pinned
    // beneath, not by a comparison: card B's directory does not contain card
    // A's id, and nothing under card B's subtree can reach out of it.
    let error = open_attachment(root.path(), &other_card, &bound)
        .await
        .expect_err("another card's id must not open");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");

    let error = open_attachment(root.path(), &card, &id("03", AttachmentFormat::Gif))
        .await
        .expect_err("an id that names nothing must not open");
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

/// #1515 review F3, corrected in round 4. A link planted under a valid id must
/// not be followed.
///
/// The round-3 version of this test wrote an **absolute** target, which is the
/// one spelling `RESOLVE_BENEATH` already rejects on its own — so it passed
/// without measuring anything the code did. Both spellings are driven here, and
/// the relative one is the case that matters: it is what an agent in the
/// workspace would write, and it is what `RESOLVE_NO_SYMLINKS` exists for.
#[tokio::test]
async fn a_symlink_under_a_valid_id_does_not_open() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    let bound = bound_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    std::fs::create_dir_all(bound.path()).unwrap();

    // The target lives OUTSIDE the attachment root, so "it was refused" cannot
    // be confused with "it was outside the root anyway".
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa");
    std::fs::write(&secret, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();
    // ... and one inside it, reachable relatively, which `RESOLVE_BENEATH`
    // permits and only `RESOLVE_NO_SYMLINKS` refuses.
    let inside = root.path().join("inside.png");
    std::fs::write(&inside, b"in-root bytes").unwrap();

    let absolute_staged = id("05", AttachmentFormat::Png);
    let absolute_bound = id("06", AttachmentFormat::Png);
    let relative_staged = id("09", AttachmentFormat::Png);
    let relative_bound = id("0a", AttachmentFormat::Png);
    std::os::unix::fs::symlink(&secret, staging.path().join(absolute_staged.as_str())).unwrap();
    std::os::unix::fs::symlink(&secret, bound.path().join(absolute_bound.as_str())).unwrap();
    // `staging/` is `<root>/<card>/staging`, so `../../inside.png` is the root.
    std::os::unix::fs::symlink(
        "../../inside.png",
        staging.path().join(relative_staged.as_str()),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../../inside.png",
        bound.path().join(relative_bound.as_str()),
    )
    .unwrap();
    assert!(
        std::fs::read(staging.path().join(relative_staged.as_str())).is_ok(),
        "precondition: the relative link really does resolve to a readable file"
    );

    for planted in [
        &absolute_staged,
        &absolute_bound,
        &relative_staged,
        &relative_bound,
    ] {
        let error = open_attachment(root.path(), &card, planted)
            .await
            .expect_err("a symlink is not an attachment this subtree serves");
        assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
    }
}

/// #1515 review round 4, BLOCKER. `RESOLVE_BENEATH` pins resolution beneath the
/// root, and the root is `attachments/` — so every *other card* is beneath it
/// too. A relative link is therefore not an escape at all in `openat2`'s terms,
/// and both of these returned card B's bytes on the delegated path as shipped
/// in round 3.
///
/// (b) is the regression: the `is_regular_file` check round 3 deleted used
/// `symlink_metadata` and refused every symlink, absolute or relative.
#[tokio::test]
async fn no_relative_symlink_reaches_another_cards_subtree() {
    let root = tempfile::tempdir().unwrap();
    let card_a = CardId::from("card-a");
    let card_b = CardId::from("card-b");
    let wanted = id("08", AttachmentFormat::Png);

    let b_bound = bound_dir(root.path(), &card_b);
    std::fs::create_dir_all(b_bound.path()).unwrap();
    std::fs::write(
        b_bound.path().join(wanted.as_str()),
        b"card B's private image",
    )
    .unwrap();

    // Control: card B can read its own file. Without this the test could pass
    // because nothing opens at all.
    let opened = open_attachment(root.path(), &card_b, &wanted)
        .await
        .expect("card B's own attachment must still open");
    assert_eq!(read_all(opened).await, b"card B's private image");

    // (a) intermediate: card A's `staging` IS a link into card B's subtree.
    let a_staging = staging_dir(root.path(), &card_a);
    std::fs::create_dir_all(a_staging.path().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("../card-b/bound", a_staging.path()).unwrap();
    assert!(
        a_staging.path().join(wanted.as_str()).is_file(),
        "precondition (a): a following resolver really would find card B's file"
    );
    let error = open_attachment(root.path(), &card_a, &wanted)
        .await
        .expect_err("(a) an intermediate link must not reach another card");
    assert!(matches!(error, CalmError::BadRequest(_)), "(a) {error:?}");

    // (b) leaf: card A's `staging` is a real directory holding a link to card
    // B's file. This is the spelling round 3 regressed on.
    std::fs::remove_file(a_staging.path()).unwrap();
    std::fs::create_dir_all(a_staging.path()).unwrap();
    std::os::unix::fs::symlink(
        "../../card-b/bound/08.png".replace("08.png", wanted.as_str()),
        a_staging.path().join(wanted.as_str()),
    )
    .unwrap();
    assert!(
        a_staging.path().join(wanted.as_str()).is_file(),
        "precondition (b): a following resolver really would find card B's file"
    );
    let error = open_attachment(root.path(), &card_a, &wanted)
        .await
        .expect_err("(b) a leaf link must not reach another card");
    assert!(matches!(error, CalmError::BadRequest(_)), "(b) {error:?}");
}

/// #1515 review round 3, BLOCKER. A FIFO on the final component blocks
/// `open(2)` until a writer appears unless `O_NONBLOCK` is set — and because
/// every `tokio::fs` open is a `spawn_blocking`, one such request parks a
/// blocking thread that a client disconnect does not reclaim. 512 of them and
/// every `tokio::fs` call in the process queues forever.
///
/// The hand-rolled `O_NOFOLLOW` open this replaced had no `O_NONBLOCK`, and the
/// `fstat` that was supposed to reject the FIFO was never reached. The vetted
/// opener sets it, so the refusal is `ENXIO` at the syscall.
///
/// The assertion is the wall clock: a regression does not fail this test, it
/// hangs it, so the call is given a deadline of its own.
#[tokio::test]
async fn a_fifo_under_a_valid_id_neither_blocks_nor_serves() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    let planted = id("07", AttachmentFormat::Png);
    nix::unistd::mkfifo(
        &staging.path().join(planted.as_str()),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .expect("the fixture needs a real FIFO");
    assert!(
        std::fs::symlink_metadata(staging.path().join(planted.as_str()))
            .unwrap()
            .file_type()
            .is_fifo(),
        "precondition: the planted entry must actually be a FIFO"
    );

    let answered = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        open_attachment(root.path(), &card, &planted),
    )
    .await
    .expect("the open must return; a FIFO must not park the blocking thread");
    let error = answered.expect_err("a FIFO is not an attachment");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
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

// ---------------------------------------------------------------------------
// #1515 review round 2.
// ---------------------------------------------------------------------------

/// The fail-closed arm `used_bytes` KEPT — an entry the filesystem refuses to
/// describe — had no test at all: replacing it with `continue` left the whole
/// suite green. This is the same construction `gc.rs`'s sweep already had
/// (`staging/` readable but not searchable, so `lstat` on its children is
/// `EACCES`), which is exactly the one that was missing here.
#[test]
fn a_budget_entry_that_cannot_be_stat_d_refuses() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path()).unwrap();
    let entry = staging.path().join("real.png");
    std::fs::write(&entry, vec![0u8; 10]).unwrap();

    std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let refused = std::fs::symlink_metadata(&entry).is_err();
    // Root ignores the mode bits, so the construction would be vacuous there.
    // Say so loudly rather than reporting a green that proved nothing.
    assert!(
        refused,
        "precondition: the entry stat must actually be refused — these tests must not run as root"
    );

    let measured = used_bytes(root.path(), &card);
    std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

    let error = measured.expect_err("an entry that cannot be stat'd must refuse the write");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
}

/// No refusal this module builds may carry a host path: every one of them is
/// rendered into an HTTP error body.
#[test]
fn no_budget_refusal_names_a_host_path() {
    let root = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let staging = staging_dir(root.path(), &card);
    std::fs::create_dir_all(staging.path().parent().unwrap()).unwrap();
    std::fs::write(staging.path(), b"not a directory").unwrap();

    let error = used_bytes(root.path(), &card).expect_err("ENOTDIR must refuse");
    let message = format!("{error}");
    assert!(
        !message.contains(&root.path().display().to_string()),
        "the refusal must not carry the host path: {message}"
    );
    assert!(
        !message.contains("staging"),
        "nor the subtree's layout: {message}"
    );
}

fn staged_names(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// A managed workspace with a real git repository, the one precondition
/// `store_upload` checks before it writes anything.
fn git_workspace(tmp: &std::path::Path) -> std::path::PathBuf {
    let repo = tmp.join("workspace");
    std::fs::create_dir_all(&repo).unwrap();
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .status()
        .expect("git must be on PATH for this test");
    assert!(status.success(), "git init failed in {repo:?}");
    repo
}

fn png_prefix() -> axum::body::Bytes {
    let mut bytes = vec![0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0u8; 4]);
    axum::body::Bytes::from(bytes)
}

/// #1515 review round 2. The per-card turn that makes the budget honest is a
/// lane, and a lane one client can sit in forever is a denial of service this
/// server had no other defence against — nothing in calm-server bounds a
/// request body's duration. The upload therefore carries its own deadline, and
/// the two things that must be true when it fires are that the lane is free
/// again and that no `.part` is left behind.
#[tokio::test]
async fn an_upload_that_stops_sending_gives_up_the_cards_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = git_workspace(tmp.path());
    let root = repo.join(".neige").join("attachments");
    let card = CardId::from("card-a");
    let locks = crate::per_card_lock::new_per_card_locks();

    // A body that sends a sniffable prefix and then simply stops. The sender
    // stays alive for the whole test, so the stream never ends on its own.
    let (frames, body) = futures::channel::mpsc::unbounded::<std::io::Result<axum::body::Bytes>>();
    frames.unbounded_send(Ok(png_prefix())).unwrap();

    let stalled = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store::store_upload(
            &root,
            &repo,
            &card,
            &locks,
            std::time::Duration::from_millis(150),
            axum::body::Body::from_stream(body),
        ),
    )
    .await
    .expect("the upload must give up on its own, not hang until the test times out");

    let error = stalled.expect_err("a body that stops arriving must be refused");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");
    assert!(
        format!("{error}").contains("stopped arriving"),
        "the refusal must say what happened: {error}"
    );
    assert_eq!(
        staged_names(staging_dir(&root, &card).path()),
        Vec::<String>::new(),
        "the abandoned `.part` must not survive the deadline"
    );

    // The lane is free: a second upload on the SAME card runs immediately,
    // while the stalled sender is still open.
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store::store_upload(
            &root,
            &repo,
            &card,
            &locks,
            std::time::Duration::from_secs(5),
            axum::body::Body::from(png_prefix().to_vec()),
        ),
    )
    .await
    .expect("the card's turn must have been released")
    .expect("a well-behaved upload after a timed-out one must succeed");
    assert_eq!(second.size, 12);
    drop(frames);
}

/// #1515 review round 3. The deadline no longer wraps `finish`, so a refused
/// upload cannot leave an attachment published under its final name. This pins
/// the invariant the change exists for: after a timeout, `staging/` holds
/// nothing at all — neither the `.part` nor a published name.
#[tokio::test]
async fn a_timed_out_upload_publishes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = git_workspace(tmp.path());
    let root = repo.join(".neige").join("attachments");
    let card = CardId::from("card-a");
    let locks = crate::per_card_lock::new_per_card_locks();

    let (frames, body) = futures::channel::mpsc::unbounded::<std::io::Result<axum::body::Bytes>>();
    frames.unbounded_send(Ok(png_prefix())).unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store::store_upload(
            &root,
            &repo,
            &card,
            &locks,
            std::time::Duration::from_millis(120),
            axum::body::Body::from_stream(body),
        ),
    )
    .await
    .expect("the upload must give up on its own")
    .expect_err("a body that stops arriving must be refused");
    assert!(matches!(error, CalmError::BadRequest(_)), "{error:?}");

    assert_eq!(
        staged_names(staging_dir(&root, &card).path()),
        Vec::<String>::new(),
        "a refused upload must leave neither a `.part` nor a published attachment"
    );
    drop(frames);
}

/// #1515 review round 3. No error this module family can put in front of a
/// client may carry a host path. Round 1 fixed one message, round 2 fixed two
/// more and claimed the class; this drives the constructors that were still
/// leaking.
///
/// The list is the `CalmError::` sites in `mod.rs` and `store.rs` that a
/// request can reach, taken by grep rather than from memory: `attachment_root`
/// (two arms), `directory_bytes`/`unmeasurable`, the `ensure_git_exclude_entry`
/// arm, `staging_dir_or_refuse`, `open_attachment`, and `OpenPart`'s create /
/// write / flush / fsync / rename arms. The ones this test cannot construct
/// from outside (fsync failures) carry no path by inspection and are named in
/// `store.rs`.
#[test]
fn attachment_root_faults_name_no_host_path() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    let relative = TrackWorkspace {
        kind: TrackWorkspaceKind::Managed,
        path: "relative/workspace".into(),
        frozen_at: None,
    };
    for (what, workspace) in [
        ("a relative managed path", relative),
        ("a managed path outside the root", managed(elsewhere.path())),
    ] {
        let error = attachment_root(&workspace, root.path()).expect_err(what);
        let message = format!("{error}");
        assert!(matches!(error, CalmError::Internal(_)), "{what}: {error:?}");
        for leaked in [
            root.path().display().to_string(),
            elsewhere.path().display().to_string(),
            "relative/workspace".to_string(),
        ] {
            assert!(
                !message.contains(&leaked),
                "{what}: the fault names a host path: {message}"
            );
        }
    }
}
